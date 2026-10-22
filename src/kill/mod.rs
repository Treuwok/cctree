//! Ending processes (spec 4.7). [`plan_kill`] works out exactly which
//! processes `x` or `X` on a row would end, in which order, or why it must
//! not; it signals nothing. [`execute`] then carries a plan out through a
//! [`ProcessControl`] ([`OsControl`] is the real one); see its documentation.
//!
//! # Rules
//!
//! - **`x`** ([`KillScope::One`]): on a process row, that process. A `SelfRow`
//!   is its process. Any group row is [`KillRefusal::GroupNeedsX`].
//! - **`X`** ([`KillScope::Tree`]) ends what the row shows:
//!   - on any category row: [`KillRefusal::CategoryTooBroad`], whatever is
//!     below it. Orphans are ended by selecting an orphan's own row;
//!   - on any other row (a process, a session, an MCP group, one MCP, other
//!     children, detached, a Node.js group, the overview's rows): every
//!     process drawn under the row ([`member_keys`]: a session's row holds its
//!     MCPs, other children, detached processes and nested sessions), plus
//!     their descendants in the validated lineage ([`Lineage`]) that the
//!     classification does not track ([`Role::Unrelated`]). A tracked
//!     descendant that is not drawn under the row belongs to another row (a
//!     nested session below "other children", a session started in a
//!     detached tmux server): it is skipped, and so is its subtree.
//! - **Left out**: zombies within an `X` (they have already ended), and keys
//!   that are no longer in the snapshot.
//! - **Order**: deepest first. A target's depth is the number of targets
//!   among its ancestors, where the root of a detached group counts as a child
//!   of its session's root. Ties go by key (PID, then start time), so the
//!   subject's root comes last.
//! - **Refusals**, checked in this order:
//!   1. any target is cctree itself ([`Snapshot::self_key`]) or one of its
//!      lineage ancestors (the shell, the terminal, a session cctree runs in):
//!      [`KillRefusal::SelfOrAncestor`];
//!   2. `x` on a zombie: [`KillRefusal::Zombie`] (also `X` on a zombie whose
//!      tree has nothing else to end, which says more than "nothing to
//!      kill"). It comes before the next two: a zombie needs no ending, so
//!      suggesting `sudo` would be wrong;
//!   3. any target is PID 0 or 1, or runs as root / SYSTEM:
//!      [`KillRefusal::System`] (the first one in kill order);
//!   4. any target belongs to another user, unless cctree itself runs as
//!      root (`sudo cctree`; unknown counts as not root):
//!      [`KillRefusal::OtherUser`];
//!   5. no target at all (an unknown or stale row, a note row, an empty
//!      group): [`KillRefusal::NothingToKill`].
//!
//!   One refused target refuses the whole plan: the user is told why and can
//!   end the rest row by row.
//! - **Warnings**: one [`KillWarning::InterruptsSession`] per session root
//!   ended (outer sessions first; identical warnings once), and one
//!   [`KillWarning::McpDisconnect`] per tool whose session loses an MCP
//!   member while it keeps running. When the plan ends that session too, the
//!   interruption warning already says it all.
//! - `freed` adds up the targets' `mem.private`; `freed_approx` is set when
//!   one is unknown. `force_only` is set on Windows.
//!
//! # Choices the spec leaves open
//!
//! - cctree knows itself only through [`Snapshot::self_key`], which the
//!   collector sets (tests set it too); `std::process::id()` is not
//!   consulted. If cctree is missing from the snapshot, its ancestors are
//!   unknown and only the key itself is protected.
//! - "Runs as root" is the `is_root_user` of cctree's own process. An
//!   elevated Windows administrator is not SYSTEM, so there another user's
//!   processes stay refused (the OS would deny most of them anyway).
//! - A zombie's `parent` is the PID that has to reap it. On macOS and Linux
//!   that is the recorded `ppid` (the kernel's answer, also when the lineage
//!   dropped the link because the parent, a subreaper, started later), as
//!   long as that process is in the snapshot; on Windows only a validated
//!   parent, because a recorded PPID there can be a reused PID. `None` means
//!   the parent is gone.
//! - The detached rule and the lineage could, in a hand-made classification,
//!   make a loop (a detached process above its own session). The depth walk
//!   cuts such a loop instead of going round it.
//!
//! Building a plan is linear in the number of processes plus a sort of the
//! targets; no recursion, so any depth of process tree is fine.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::{Duration, Instant};

use crate::classify::argv::display_name;
use crate::classify::lineage::Lineage;
use crate::classify::{Classification, Role, SessionInfo};
use crate::model::{Pid, Platform, ProcKey, ProcStatus, RawProc, Snapshot, Tool};
use crate::tree::{Node, NodeId, find, member_keys};

mod os;
pub use os::OsControl;

/// `x` ends the process of the row; `X` the whole tree or group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KillScope {
    One,
    Tree,
}

/// One process a plan would end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillTarget {
    pub key: ProcKey,
    pub name: String,
    /// Detached from its session (listed in a section of its own).
    pub detached: bool,
    /// The main memory figure (`mem.private`), when known.
    pub mem: Option<u64>,
}

/// How to get an interrupted session back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResumeHint {
    /// A command to run in a terminal (`claude --resume`, `codex resume`).
    Command(String),
    /// The app that started the session, to reopen it from.
    ReopenFrom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KillWarning {
    /// The plan ends the root of a running session.
    InterruptsSession {
        tool: Tool,
        /// The session's working directory, as recorded (not abbreviated).
        cwd: Option<String>,
        hint: ResumeHint,
    },
    /// The plan ends an MCP member of a session it leaves running.
    McpDisconnect { tool: Tool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct KillPlan {
    pub subject: NodeId,
    /// Each process once, deepest first: a process comes before every target
    /// above it.
    pub targets: Vec<KillTarget>,
    pub warnings: Vec<KillWarning>,
    /// Sum of the targets' known `mem`.
    pub freed: u64,
    /// At least one target's memory is unknown, so `freed` is a lower bound.
    pub freed_approx: bool,
    /// The processes are ended by force right away (Windows).
    pub force_only: bool,
}

/// Why nothing will be ended. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KillRefusal {
    /// `x` on a group row.
    GroupNeedsX,
    /// The plan would end cctree or one of its ancestors.
    SelfOrAncestor,
    /// PID 0 or 1, or a root / SYSTEM process.
    System {
        pid: Pid,
    },
    /// Another user's process.
    OtherUser {
        pid: Pid,
    },
    /// Already ended; `parent` has to reap it (`None`: the parent is gone).
    Zombie {
        pid: Pid,
        parent: Option<Pid>,
    },
    NothingToKill,
    /// `X` on a whole category.
    CategoryTooBroad,
}

/// What `scope` on the row `id` of `roots` would end. Total: any id, row set
/// or snapshot gives a plan or a refusal, with no panic. See the module
/// documentation for the rules.
pub fn plan_kill(
    snap: &Snapshot,
    cls: &Classification,
    roots: &[Node],
    id: &NodeId,
    scope: KillScope,
) -> Result<KillPlan, KillRefusal> {
    if scope == KillScope::Tree && matches!(id, NodeId::Category(_)) {
        return Err(KillRefusal::CategoryTooBroad);
    }
    let node = find(roots, id).ok_or(KillRefusal::NothingToKill)?;
    let ctx = Ctx::new(snap, cls);
    match (id, scope) {
        (NodeId::Proc(k), KillScope::One) | (NodeId::SelfRow(k), _) => ctx.one(id, *k),
        (NodeId::NestedNote(_), _) => Err(KillRefusal::NothingToKill),
        (_, KillScope::One) => Err(KillRefusal::GroupNeedsX),
        (_, KillScope::Tree) => ctx.tree(id, &member_keys(node)),
    }
}

/// What one plan is worked out from.
struct Ctx<'a> {
    snap: &'a Snapshot,
    cls: &'a Classification,
    l: Lineage<'a>,
    by_key: HashMap<ProcKey, &'a RawProc>,
}

impl<'a> Ctx<'a> {
    fn new(snap: &'a Snapshot, cls: &'a Classification) -> Ctx<'a> {
        Ctx {
            snap,
            cls,
            l: Lineage::build(snap),
            by_key: snap.procs.iter().map(|p| (p.key, p)).collect(),
        }
    }

    /// `x` on the process `k`.
    fn one(&self, subject: &NodeId, k: ProcKey) -> Result<KillPlan, KillRefusal> {
        let p = *self.by_key.get(&k).ok_or(KillRefusal::NothingToKill)?;
        self.refuse_self(&[k])?;
        if p.status == ProcStatus::Zombie {
            return Err(self.zombie(p));
        }
        refuse_owner(&[p], self.self_is_root())?;
        Ok(self.plan(subject, vec![k]))
    }

    /// `X` on a row whose processes are `members`: see [`Ctx::closure`];
    /// zombies left out.
    fn tree(&self, subject: &NodeId, members: &[ProcKey]) -> Result<KillPlan, KillRefusal> {
        let keys: Vec<ProcKey> = self
            .closure(members)
            .into_iter()
            .filter(|k| self.by_key[k].status != ProcStatus::Zombie)
            .collect();
        if keys.is_empty() {
            // `X` on a lone zombie says why there is nothing to end.
            if let NodeId::Proc(k) = subject
                && let Some(p) = self.by_key.get(k)
                && p.status == ProcStatus::Zombie
            {
                return Err(self.zombie(p));
            }
            return Err(KillRefusal::NothingToKill);
        }
        let ordered = kill_order(&self.l, self.cls, &keys);
        self.refuse_self(&ordered)?;
        let procs: Vec<&RawProc> = ordered.iter().map(|k| self.by_key[k]).collect();
        refuse_owner(&procs, self.self_is_root())?;
        Ok(self.plan(subject, ordered))
    }

    /// The `members` of a row that are in the snapshot, each once, and their
    /// lineage descendants that the classification does not track. A tracked
    /// descendant that is not a member belongs to another row: the walk
    /// stops there, so neither it nor its subtree is taken.
    fn closure(&self, members: &[ProcKey]) -> Vec<ProcKey> {
        let mut seen: HashSet<ProcKey> = HashSet::new();
        let mut out = Vec::new();
        let mut stack: Vec<ProcKey> = members.iter().rev().copied().collect();
        while let Some(k) = stack.pop() {
            if !self.by_key.contains_key(&k) || !seen.insert(k) {
                continue;
            }
            out.push(k);
            // Members are on the stack already; any other tracked child is
            // another row's.
            let untracked = self.l.children(k).iter().rev();
            stack.extend(untracked.filter(|&&c| *self.cls.role(c) == Role::Unrelated));
        }
        out
    }

    /// cctree and its ancestors are never ended.
    fn refuse_self(&self, keys: &[ProcKey]) -> Result<(), KillRefusal> {
        let me = self.snap.self_key;
        let mut protected: HashSet<ProcKey> = self.l.ancestors(me).into_iter().collect();
        protected.insert(me);
        if keys.iter().any(|k| protected.contains(k)) {
            return Err(KillRefusal::SelfOrAncestor);
        }
        Ok(())
    }

    /// cctree runs as root (`sudo cctree`); unknown counts as not.
    fn self_is_root(&self) -> bool {
        self.by_key
            .get(&self.snap.self_key)
            .is_some_and(|p| p.is_root_user)
    }

    fn zombie(&self, p: &RawProc) -> KillRefusal {
        let parent = if self.snap.platform == Platform::Windows {
            self.l.parent(p.key).map(|k| k.pid)
        } else {
            p.ppid
                .filter(|&pp| pp != p.key.pid && self.l.key_of(pp).is_some())
        };
        KillRefusal::Zombie {
            pid: p.key.pid,
            parent,
        }
    }

    /// The plan for `keys`, already checked and in kill order.
    fn plan(&self, subject: &NodeId, keys: Vec<ProcKey>) -> KillPlan {
        let targets: Vec<KillTarget> = keys
            .iter()
            .map(|k| {
                let p = self.by_key[k];
                KillTarget {
                    key: *k,
                    name: display_name(p),
                    detached: matches!(self.cls.role(*k), Role::Detached { .. }),
                    mem: p.mem.and_then(|m| m.private),
                }
            })
            .collect();
        let freed = targets
            .iter()
            .filter_map(|t| t.mem)
            .fold(0u64, u64::saturating_add);
        let freed_approx = targets.iter().any(|t| t.mem.is_none());
        KillPlan {
            subject: subject.clone(),
            warnings: self.warnings(&keys),
            targets,
            freed,
            freed_approx,
            force_only: self.snap.platform == Platform::Windows,
        }
    }

    fn warnings(&self, keys: &[ProcKey]) -> Vec<KillWarning> {
        let set: HashSet<ProcKey> = keys.iter().copied().collect();
        let mut out: Vec<KillWarning> = Vec::new();
        let mut push = |w: KillWarning| {
            if !out.contains(&w) {
                out.push(w);
            }
        };
        for s in &self.cls.sessions {
            if set.contains(&s.root) {
                push(KillWarning::InterruptsSession {
                    tool: s.tool,
                    cwd: s.cwd.clone(),
                    hint: resume_hint(s),
                });
            }
        }
        let tools: HashMap<ProcKey, Tool> =
            self.cls.sessions.iter().map(|s| (s.root, s.tool)).collect();
        for k in keys {
            if let Role::Mcp { session, .. } = self.cls.role(*k)
                && !set.contains(session)
                && let Some(&tool) = tools.get(session)
            {
                push(KillWarning::McpDisconnect { tool });
            }
        }
        out
    }
}

/// System processes and processes whose owner could not be read are refused,
/// and so are other users' unless cctree runs as root; the first one in
/// `procs` is named.
fn refuse_owner(procs: &[&RawProc], self_is_root: bool) -> Result<(), KillRefusal> {
    if let Some(p) = procs
        .iter()
        .find(|p| p.key.pid <= 1 || p.is_root_user || p.owner_unknown)
    {
        return Err(KillRefusal::System { pid: p.key.pid });
    }
    if !self_is_root && let Some(p) = procs.iter().find(|p| !p.is_own_user) {
        return Err(KillRefusal::OtherUser { pid: p.key.pid });
    }
    Ok(())
}

/// How to get the session back: its resume command when it was started from
/// a terminal, else the app that started it.
fn resume_hint(s: &SessionInfo) -> ResumeHint {
    match &s.launcher {
        Some(app) => ResumeHint::ReopenFrom(app.clone()),
        None => ResumeHint::Command(
            match s.tool {
                Tool::ClaudeCode => "claude --resume",
                Tool::Codex => "codex resume",
            }
            .to_string(),
        ),
    }
}

/// `keys` (distinct) deepest first, ties by key. A key's depth is the number
/// of keys on its chain of plan parents ([`plan_parent`]); a loop in that
/// chain is cut where the walk meets itself.
fn kill_order(l: &Lineage, cls: &Classification, keys: &[ProcKey]) -> Vec<ProcKey> {
    let set: HashSet<ProcKey> = keys.iter().copied().collect();
    let mut sorted: Vec<ProcKey> = keys.to_vec();
    sorted.sort_unstable();
    let mut depth: HashMap<ProcKey, usize> = HashMap::with_capacity(sorted.len());
    let mut path: Vec<ProcKey> = Vec::new();
    let mut on_path: HashSet<ProcKey> = HashSet::new();
    for &k in &sorted {
        path.clear();
        on_path.clear();
        let mut cur = Some(k);
        // The depth of the last key on `path`.
        let top = loop {
            let Some(c) = cur else { break 0 };
            if let Some(&d) = depth.get(&c) {
                break d + 1;
            }
            if !on_path.insert(c) {
                break 0;
            }
            path.push(c);
            cur = plan_parent(l, cls, &set, c);
        };
        for (i, c) in path.iter().rev().enumerate() {
            depth.insert(*c, top + i);
        }
    }
    sorted.sort_by_key(|k| (Reverse(depth[k]), *k));
    sorted
}

/// The nearest target above `k`: its nearest lineage ancestor in `set`; for
/// the root of a detached group (no target above it in the lineage), the
/// root of its session when that is a target.
fn plan_parent(
    l: &Lineage,
    cls: &Classification,
    set: &HashSet<ProcKey>,
    k: ProcKey,
) -> Option<ProcKey> {
    let mut cur = l.parent(k);
    while let Some(a) = cur {
        if set.contains(&a) {
            return Some(a);
        }
        cur = l.parent(a);
    }
    match cls.role(k) {
        Role::Detached { session } if *session != k && set.contains(session) => Some(*session),
        _ => None,
    }
}

/// What a plan target is now, compared with when the plan was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcState {
    /// Running, with the same start time.
    Same,
    /// Not running any more. A zombie counts: it has ended and only waits for
    /// its parent to collect it.
    Gone,
    /// The PID now belongs to a process that started later: not the one the
    /// plan meant, so it must not be touched.
    Reused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KillError {
    PermissionDenied,
    /// No such process (any more).
    NotFound,
    Other(String),
}

/// What [`execute`] needs from the operating system.
pub trait ProcessControl: Send + Sync {
    fn state(&self, key: ProcKey) -> ProcState;
    /// Asks the process to end (SIGTERM).
    fn terminate(&self, pid: Pid) -> Result<(), KillError>;
    /// Ends the process without asking (SIGKILL, TerminateProcess).
    fn force(&self, pid: Pid) -> Result<(), KillError>;
}

/// How [`execute`] went. Every target is counted once: `graceful` + `forced`
/// + `already_gone` + `skipped_reused` + `failed` is the number of targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KillReport {
    /// Ended after being asked to.
    pub graceful: usize,
    /// Still running when the grace period ran out, ended by force (always
    /// the case for a `force_only` plan).
    pub forced: usize,
    /// Ended by themselves before or while being signalled, zombies included.
    pub already_gone: usize,
    /// The PID belongs to another process now; left alone.
    pub skipped_reused: Vec<Pid>,
    pub failed: Vec<(Pid, KillError)>,
}

impl KillReport {
    /// A signal found nothing to signal: the process ended by itself. Any
    /// other error is a failure.
    fn signal_failed(&mut self, pid: Pid, e: KillError) {
        match e {
            KillError::NotFound => self.already_gone += 1,
            e => self.failed.push((pid, e)),
        }
    }
}

/// Carries out `plan`, deepest target first:
///
/// 1. each target is looked at once: [`ProcState::Gone`] is counted as
///    already gone and [`ProcState::Reused`] is skipped, so a recycled PID is
///    never signalled;
/// 2. a plan that is not `force_only` asks every remaining target to end
///    ([`ProcessControl::terminate`]), then checks every `poll` until all are
///    gone or `grace` is over;
/// 3. whatever is left (all remaining targets, for a `force_only` plan) is
///    ended by [`ProcessControl::force`].
///
/// A signal that finds no process counts as already gone, not as a failure.
/// `progress(done, total)` is called each time a target is dealt with, so the
/// last call is `(total, total)`. Blocks for up to `grace`.
pub fn execute(
    plan: &KillPlan,
    ctl: &dyn ProcessControl,
    grace: Duration,
    poll: Duration,
    progress: &mut dyn FnMut(usize, usize),
) -> KillReport {
    let total = plan.targets.len();
    let mut done = 0;
    let mut tick = || {
        done += 1;
        progress(done, total);
    };
    let mut report = KillReport::default();

    let mut live: Vec<ProcKey> = Vec::new();
    for t in &plan.targets {
        match ctl.state(t.key) {
            ProcState::Gone => {
                report.already_gone += 1;
                tick();
            }
            ProcState::Reused => {
                report.skipped_reused.push(t.key.pid);
                tick();
            }
            ProcState::Same => live.push(t.key),
        }
    }

    if !plan.force_only {
        let mut asked: Vec<ProcKey> = Vec::new();
        for k in live {
            match ctl.terminate(k.pid) {
                Ok(()) => asked.push(k),
                Err(e) => {
                    report.signal_failed(k.pid, e);
                    tick();
                }
            }
        }
        let deadline = Instant::now() + grace;
        loop {
            asked.retain(|&k| {
                let ended = ctl.state(k) != ProcState::Same;
                if ended {
                    report.graceful += 1;
                    tick();
                }
                !ended
            });
            if asked.is_empty() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(poll);
        }
        live = asked;
    }

    for k in live {
        match ctl.force(k.pid) {
            Ok(()) => report.forced += 1,
            Err(e) => report.signal_failed(k.pid, e),
        }
        tick();
    }
    report
}

/// A plan that ends `pid` and every one of its descendants in the validated
/// lineage, in the same order as [`plan_kill`] (deepest first). It skips the
/// refusals and the classification: it exists for tests that end real
/// processes they started themselves. An unknown `pid` gives no targets.
#[doc(hidden)]
pub fn plan_for_pid_tree(snap: &Snapshot, pid: Pid) -> KillPlan {
    let cls = Classification::default();
    let ctx = Ctx::new(snap, &cls);
    let root = ctx.l.key_of(pid);
    let keys = root.map_or_else(Vec::new, |root| {
        let mut keys = vec![root];
        keys.extend(ctx.l.descendants(root));
        kill_order(&ctx.l, &cls, &keys)
    });
    let subject = root.unwrap_or(ProcKey { pid, start_time: 0 });
    ctx.plan(&NodeId::Proc(subject), keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::argv::display_name;
    use crate::classify::{Category, Role, classify};
    use crate::model::{Platform, RawProc};
    use crate::testkit::{base, cfgs, p, snap};
    use crate::tree::{ViewMode, build_tree};
    use std::collections::HashSet;
    use std::sync::Mutex;

    const SID: &str = "CLAUDE_CODE_SESSION_ID";
    const PROJ: &str = "CLAUDE_PROJECT_DIR";
    const MIB: u64 = 1024 * 1024;

    fn setup(v: Vec<RawProc>, plat: Platform) -> (Snapshot, Classification, Vec<Node>) {
        let s = snap(plat, v);
        let c = classify(&s, &cfgs());
        let t = build_tree(&s, &c, ViewMode::BySession);
        (s, c, t)
    }

    fn pids(pl: &KillPlan) -> Vec<Pid> {
        pl.targets.iter().map(|x| x.key.pid).collect()
    }

    // ---- The brief's tests ----

    #[test]
    fn x_on_process_and_group() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let one = plan_kill(&s, &c, &t, &NodeId::Proc(k(230)), KillScope::One).unwrap();
        assert_eq!(
            one.targets.iter().map(|x| x.key.pid).collect::<Vec<_>>(),
            [230]
        );
        assert!(matches!(
            plan_kill(&s, &c, &t, &NodeId::McpGroup(k(200)), KillScope::One),
            Err(KillRefusal::GroupNeedsX)
        ));
    }

    #[test]
    fn big_x_on_mcp_is_deepest_first_with_warning() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        // Controller ruling: an MCP node is keyed by its head (210), not by
        // session and name.
        let pl = plan_kill(&s, &c, &t, &NodeId::Mcp(k(210)), KillScope::Tree).unwrap();
        assert_eq!(
            pl.targets.iter().map(|x| x.key.pid).collect::<Vec<_>>(),
            [212, 211, 210]
        );
        assert_eq!(pl.freed, 3 * 1024 * 1024);
        assert!(
            pl.warnings.contains(&KillWarning::McpDisconnect {
                tool: Tool::ClaudeCode
            }) && !pl.force_only
        );
    }

    #[test]
    fn big_x_on_session_includes_detached_and_warns() {
        let mut v = base();
        v.push(p(400, 1, 70, &["node", "dev.js"]).env("CLAUDE_CODE_SESSION_ID", "S1"));
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k(200)), KillScope::Tree).unwrap();
        assert!(pl.targets.iter().any(|x| x.key.pid == 400 && x.detached));
        assert_eq!(pl.targets.last().unwrap().key.pid, 200);
        assert!(pl.warnings.iter().any(|w| matches!(w, KillWarning::InterruptsSession { hint: ResumeHint::Command(c), .. } if c == "claude --resume")));
    }

    /// Amended (scope ruling): `X` on any category row is refused, the orphan
    /// category included; the brief's `orphan_category_kills_only_confirmed`
    /// and `whole_category_kill_is_refused_except_orphans` are replaced by
    /// this test. Orphans are ended from their own rows.
    #[test]
    fn whole_category_kill_is_refused() {
        let mut v = base();
        // A confirmed orphan (its project is unrelated to session 200's) and a
        // suspected one.
        v.push(
            p(401, 1, 70, &["node", "x"])
                .env(SID, "GONE")
                .env(PROJ, "/Users/u/prj/old"),
        );
        v.push(p(
            80871,
            1,
            5,
            &["/private/var/folders/x/T/codex-home-Z/r/ChatGPT Meetings"],
        ));
        let (s, c, t) = setup(v, Platform::MacOs);
        for cat in [
            Category::ClaudeCode,
            Category::Codex,
            Category::Orphaned,
            Category::OtherNode,
        ] {
            assert_eq!(
                plan_kill(&s, &c, &t, &NodeId::Category(cat), KillScope::Tree),
                Err(KillRefusal::CategoryTooBroad),
                "{cat:?}"
            );
        }
        let k = |pid| s.index()[&pid].key;
        for pid in [401, 80871] {
            let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k(pid)), KillScope::Tree);
            assert_eq!(pids(&pl.unwrap()), [pid]);
        }
    }

    #[test]
    fn refusals() {
        let mut v = base();
        v.push(p(250, 200, 61, &["x"]).zombie());
        v.push(p(251, 200, 61, &["node", "x"]).root_user());
        v.push(p(252, 200, 61, &["node", "y"]).other_user());
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(
            plan_kill(&s, &c, &t, &NodeId::Proc(k(250)), KillScope::One),
            Err(KillRefusal::Zombie {
                pid: 250,
                parent: Some(200)
            })
        ));
        assert!(matches!(
            plan_kill(&s, &c, &t, &NodeId::Proc(k(251)), KillScope::One),
            Err(KillRefusal::System { pid: 251 })
        ));
        assert!(matches!(
            plan_kill(&s, &c, &t, &NodeId::Proc(k(252)), KillScope::One),
            Err(KillRefusal::OtherUser { pid: 252 })
        ));
    }

    #[test]
    fn tree_kill_refused_when_it_contains_cctree_ancestor() {
        let mut v = base();
        v.push(p(99999, 230, 90, &["cctree"]));
        let (mut s, c, t) = setup(v, Platform::MacOs);
        s.self_key = s.index()[&99999].key;
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(
            plan_kill(&s, &c, &t, &NodeId::Proc(k(200)), KillScope::Tree),
            Err(KillRefusal::SelfOrAncestor)
        ));
        assert!(plan_kill(&s, &c, &t, &NodeId::Proc(k(221)), KillScope::One).is_ok());
    }

    #[test]
    fn windows_plans_are_force_only_and_tree_skips_zombies() {
        let mut v = base();
        v.push(p(260, 230, 62, &["z"]).zombie());
        let (s, c, t) = setup(v, Platform::Windows);
        let k = |pid| s.index()[&pid].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k(230)), KillScope::Tree).unwrap();
        assert!(pl.force_only && pl.targets.iter().all(|x| x.key.pid != 260));
    }

    // ---- Beyond the brief ----

    /// [`base`] plus: 231 under the Bash-tool shell; a Codex session 300
    /// started from that shell, with an MCP (310 -> 311); a detached dev
    /// server 400 with a child 401; a confirmed orphan 430 with a child 431;
    /// an unrelated Node.js process 500.
    fn busy() -> Vec<RawProc> {
        let mut v = base();
        v.extend([
            p(231, 230, 31, &["node", "x.js"]),
            p(300, 230, 40, &["codex", "exec", "hi"]).cwd("/Users/u/prj/c"),
            p(310, 300, 41, &["npx", "-y", "time-mcp"]),
            p(311, 310, 42, &["node", "/x/.bin/time-mcp"]),
            p(400, 1, 70, &["node", "dev-server.js"]).env(SID, "S1"),
            p(401, 400, 71, &["/opt/esbuild", "--service"]),
            p(430, 1, 80, &["node", "old.js"])
                .env(SID, "GONE")
                .env(PROJ, "/Users/u/prj/old"),
            p(431, 430, 81, &["node", "a.js"]),
            p(500, 1, 90, &["node", "tool.js"]),
        ]);
        v
    }

    fn claude_resume(cwd: &str) -> KillWarning {
        KillWarning::InterruptsSession {
            tool: Tool::ClaudeCode,
            cwd: Some(cwd.into()),
            hint: ResumeHint::Command("claude --resume".into()),
        }
    }

    #[test]
    fn session_tree_is_deepest_first_then_by_pid() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let id = NodeId::Proc(k(200));
        let pl = plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap();
        assert_eq!(pl.subject, id);
        assert_eq!(pids(&pl), [212, 211, 221, 210, 220, 230, 200]);
        assert_eq!(
            (pl.freed, pl.freed_approx, pl.force_only),
            (7 * MIB, false, false)
        );
        // The session ends with its MCPs, so there is nothing to reconnect to.
        assert_eq!(pl.warnings, [claude_resume("/Users/u/prj/a")]);
        for x in &pl.targets {
            assert_eq!(x.name, display_name(s.get(x.key).unwrap()));
            assert_eq!(x.mem, Some(MIB));
            assert!(!x.detached);
        }
    }

    #[test]
    fn detached_roots_count_as_children_of_their_session() {
        let (s, c, t) = setup(busy(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k(200)), KillScope::Tree).unwrap();
        // Depth 0: 200. 1: its children and the detached root 400 (whose real
        // parent is launchd). 2: 211, 221, 231, 300, 401. 3: 212, 310. 4: 311.
        assert_eq!(
            pids(&pl),
            [
                311, 212, 310, 211, 221, 231, 300, 401, 210, 220, 230, 400, 200
            ]
        );
        let detached: Vec<Pid> = pl
            .targets
            .iter()
            .filter(|x| x.detached)
            .map(|x| x.key.pid)
            .collect();
        assert_eq!(detached, [401, 400]);
        // The nested Codex session is interrupted too; outer session first.
        assert_eq!(
            pl.warnings,
            [
                claude_resume("/Users/u/prj/a"),
                KillWarning::InterruptsSession {
                    tool: Tool::Codex,
                    cwd: Some("/Users/u/prj/c".into()),
                    hint: ResumeHint::Command("codex resume".into()),
                },
            ]
        );
    }

    #[test]
    fn group_rows_take_what_they_show_once() {
        let (s, c, t) = setup(busy(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let plan = |id: NodeId| plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap();

        // Both MCPs: one warning for the two of them.
        let pl = plan(NodeId::McpGroup(k(200)));
        assert_eq!(pids(&pl), [212, 211, 221, 210, 220]);
        assert_eq!(
            pl.warnings,
            [KillWarning::McpDisconnect {
                tool: Tool::ClaudeCode
            }]
        );

        // The nested session started from the shell is a row of its own,
        // drawn under the session, not under "other children".
        let pl = plan(NodeId::OtherChildren(k(200)));
        assert_eq!(pids(&pl), [231, 230]);
        assert!(pl.warnings.is_empty());

        let pl = plan(NodeId::Detached(k(200)));
        assert_eq!(pids(&pl), [401, 400]);
        assert!(pl.targets.iter().all(|x| x.detached) && pl.warnings.is_empty());

        // A Codex MCP warns for Codex.
        let pl = plan(NodeId::Mcp(k(310)));
        assert_eq!(pids(&pl), [311, 310]);
        assert_eq!(
            pl.warnings,
            [KillWarning::McpDisconnect { tool: Tool::Codex }]
        );

        // Other Node.js group.
        let group = t[3].children[0].id.clone();
        assert_eq!(pids(&plan(group)), [500]);

        // An orphan's row: the orphan and its member, the member first.
        let pl = plan(NodeId::Proc(k(430)));
        assert_eq!(pids(&pl), [431, 430]);
        assert!(pl.warnings.is_empty());
    }

    #[test]
    fn overview_rows_plan_like_their_session_view_counterparts() {
        let mut v = base();
        v.extend([
            p(700, 100, 80, &["claude"]),
            p(710, 700, 81, &["npm", "exec", "chrome-devtools-mcp@latest"]),
            p(711, 710, 82, &["chrome-devtools-mcp"]),
        ]);
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let t = build_tree(&s, &c, ViewMode::McpOverview);
        let k = |pid| s.index()[&pid].key;
        let plan = |id: NodeId, scope| plan_kill(&s, &c, &t, &id, scope);

        let all = plan(NodeId::OverviewMcp("chrome-cdp".into()), KillScope::Tree).unwrap();
        assert_eq!(pids(&all), [212, 211, 711, 210, 710]);
        assert_eq!(
            all.warnings,
            [KillWarning::McpDisconnect {
                tool: Tool::ClaudeCode
            }]
        );
        let one = NodeId::OverviewInstance("chrome-cdp".into(), k(710));
        assert_eq!(
            pids(&plan(one.clone(), KillScope::Tree).unwrap()),
            [711, 710]
        );
        assert_eq!(plan(one, KillScope::One), Err(KillRefusal::GroupNeedsX));
        assert_eq!(
            plan(NodeId::OverviewMcp("chrome-cdp".into()), KillScope::One),
            Err(KillRefusal::GroupNeedsX)
        );
        assert_eq!(
            pids(&plan(NodeId::Proc(k(711)), KillScope::One).unwrap()),
            [711]
        );
    }

    #[test]
    fn x_on_any_group_row_needs_big_x() {
        let (s, c, t) = setup(busy(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let mut ids = vec![
            NodeId::McpGroup(k(200)),
            NodeId::Mcp(k(210)),
            NodeId::OtherChildren(k(200)),
            NodeId::Detached(k(200)),
            t[3].children[0].id.clone(),
        ];
        ids.extend(
            [
                Category::ClaudeCode,
                Category::Codex,
                Category::Orphaned,
                Category::OtherNode,
            ]
            .map(NodeId::Category),
        );
        for id in ids {
            assert_eq!(
                plan_kill(&s, &c, &t, &id, KillScope::One),
                Err(KillRefusal::GroupNeedsX),
                "{id:?}"
            );
        }
        // The nested-session note is no group of processes.
        let note = NodeId::NestedNote(Tool::Codex);
        assert!(tree_has(&t, &note));
        for scope in [KillScope::One, KillScope::Tree] {
            assert_eq!(
                plan_kill(&s, &c, &t, &note, scope),
                Err(KillRefusal::NothingToKill)
            );
        }
    }

    fn tree_has(t: &[Node], id: &NodeId) -> bool {
        crate::tree::find(t, id).is_some()
    }

    #[test]
    fn self_row_is_its_process_alone() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let id = NodeId::SelfRow(k(200));
        for scope in [KillScope::One, KillScope::Tree] {
            let pl = plan_kill(&s, &c, &t, &id, scope).unwrap();
            assert_eq!((pl.subject.clone(), pids(&pl)), (id.clone(), vec![200]));
            assert_eq!(pl.warnings, [claude_resume("/Users/u/prj/a")]);
        }
    }

    #[test]
    fn x_on_one_process_warns_for_its_session_or_mcp() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let one = |pid| plan_kill(&s, &c, &t, &NodeId::Proc(k(pid)), KillScope::One).unwrap();
        assert_eq!(one(200).warnings, [claude_resume("/Users/u/prj/a")]);
        assert_eq!(
            one(211).warnings,
            [KillWarning::McpDisconnect {
                tool: Tool::ClaudeCode
            }]
        );
        assert!(one(230).warnings.is_empty());
    }

    #[test]
    fn resume_hint_names_the_app_that_started_the_session() {
        let s = snap(Platform::MacOs, base());
        let mut c = classify(&s, &cfgs());
        c.sessions[0].launcher = Some("Visual Studio Code".into());
        let t = build_tree(&s, &c, ViewMode::BySession);
        let k = s.index()[&200].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k), KillScope::One).unwrap();
        assert_eq!(
            pl.warnings,
            [KillWarning::InterruptsSession {
                tool: Tool::ClaudeCode,
                cwd: Some("/Users/u/prj/a".into()),
                hint: ResumeHint::ReopenFrom("Visual Studio Code".into()),
            }]
        );
    }

    #[test]
    fn unknown_or_stale_ids_have_nothing_to_kill() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        let gone = ProcKey {
            pid: 4242,
            start_time: 1,
        };
        // 210 with another start time: the PID was reused.
        let reused = ProcKey {
            pid: 210,
            start_time: 999,
        };
        let ids = [
            NodeId::Proc(gone),
            NodeId::Proc(reused),
            NodeId::SelfRow(gone),
            NodeId::McpGroup(gone),
            NodeId::Mcp(reused),
            NodeId::OtherChildren(gone),
            NodeId::Detached(gone),
            NodeId::OtherGroup("nope".into()),
            NodeId::NestedNote(Tool::Codex),
            NodeId::OverviewMcp("chrome-cdp".into()),
            NodeId::OverviewInstance("chrome-cdp".into(), gone),
        ];
        for id in &ids {
            for scope in [KillScope::One, KillScope::Tree] {
                assert_eq!(
                    plan_kill(&s, &c, &t, id, scope),
                    Err(KillRefusal::NothingToKill),
                    "{id:?} {scope:?}"
                );
            }
        }
        // No rows at all.
        let k200 = s.index()[&200].key;
        assert_eq!(
            plan_kill(&s, &c, &[], &NodeId::Proc(k200), KillScope::Tree),
            Err(KillRefusal::NothingToKill)
        );
        // Rows of an older snapshot: their processes have all exited.
        let (_, _, old) = setup(busy(), Platform::MacOs);
        let s2 = snap(Platform::MacOs, vec![p(1, 0, 1, &["/sbin/launchd"])]);
        let c2 = classify(&s2, &cfgs());
        for id in [NodeId::Proc(k200), NodeId::McpGroup(k200)] {
            assert_eq!(
                plan_kill(&s2, &c2, &old, &id, KillScope::Tree),
                Err(KillRefusal::NothingToKill)
            );
        }
    }

    #[test]
    fn any_refused_process_refuses_the_whole_tree() {
        for (extra, want) in [
            (
                p(251, 230, 61, &["x"]).root_user(),
                KillRefusal::System { pid: 251 },
            ),
            (
                p(252, 230, 61, &["x"]).other_user(),
                KillRefusal::OtherUser { pid: 252 },
            ),
        ] {
            let mut v = base();
            v.push(extra);
            let (s, c, t) = setup(v, Platform::MacOs);
            let k = |pid| s.index()[&pid].key;
            for id in [
                NodeId::Proc(k(200)),
                NodeId::Proc(k(230)),
                NodeId::OtherChildren(k(200)),
            ] {
                assert_eq!(plan_kill(&s, &c, &t, &id, KillScope::Tree), Err(want));
            }
            // The rest of the session is not affected.
            assert!(plan_kill(&s, &c, &t, &NodeId::McpGroup(k(200)), KillScope::Tree).is_ok());
        }
    }

    #[test]
    fn pid_0_and_1_are_system_processes() {
        // A container whose PID 1 is Claude Code itself.
        for pid in [0, 1] {
            let v = vec![
                p(pid, 0, 1, &["claude"]).cwd("/w"),
                p(20, pid, 2, &["/bin/sh", "-c", "ls"]),
            ];
            let (s, c, t) = setup(v, Platform::Linux);
            let k = s.index()[&pid].key;
            for scope in [KillScope::One, KillScope::Tree] {
                assert_eq!(
                    plan_kill(&s, &c, &t, &NodeId::Proc(k), scope),
                    Err(KillRefusal::System { pid }),
                    "{pid} {scope:?}"
                );
            }
        }
    }

    #[test]
    fn cctree_and_its_ancestors_are_refused_one_by_one_too() {
        let mut v = base();
        v.push(p(99999, 230, 90, &["cctree"]));
        let (mut s, c, t) = setup(v, Platform::MacOs);
        s.self_key = s.index()[&99999].key;
        let k = |pid| s.index()[&pid].key;
        for pid in [99999, 230, 200] {
            assert_eq!(
                plan_kill(&s, &c, &t, &NodeId::Proc(k(pid)), KillScope::One),
                Err(KillRefusal::SelfOrAncestor),
                "{pid}"
            );
        }
        assert_eq!(
            plan_kill(&s, &c, &t, &NodeId::OtherChildren(k(200)), KillScope::Tree),
            Err(KillRefusal::SelfOrAncestor)
        );
        assert_eq!(
            plan_kill(&s, &c, &t, &NodeId::SelfRow(k(200)), KillScope::Tree),
            Err(KillRefusal::SelfOrAncestor)
        );
        // Siblings of the ancestors are fine.
        assert!(plan_kill(&s, &c, &t, &NodeId::McpGroup(k(200)), KillScope::Tree).is_ok());
    }

    #[test]
    fn zombies() {
        let mut v = base();
        // A zombie under the shell, and one whose parent is not in the snapshot.
        v.push(p(260, 230, 62, &["z"]).zombie());
        v.push(p(261, 777, 63, &["node", "z.js"]).zombie());
        // A zombie member of an MCP.
        v.push(p(213, 211, 24, &["node", "/x/zombie.js"]).zombie());
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        let plan = |pid, scope| plan_kill(&s, &c, &t, &NodeId::Proc(k(pid)), scope);
        assert_eq!(
            plan(261, KillScope::One),
            Err(KillRefusal::Zombie {
                pid: 261,
                parent: None
            })
        );
        // `X` on a zombie alone says why, rather than "nothing to kill".
        for pid in [260, 261] {
            assert!(matches!(
                plan(pid, KillScope::Tree),
                Err(KillRefusal::Zombie { .. })
            ));
        }
        // Inside a tree they are left out silently.
        assert_eq!(pids(&plan(230, KillScope::Tree).unwrap()), [230]);
        let pl = plan_kill(&s, &c, &t, &NodeId::Mcp(k(210)), KillScope::Tree).unwrap();
        assert_eq!(pids(&pl), [212, 211, 210]);
    }

    #[test]
    fn freed_is_a_lower_bound_when_a_figure_is_missing() {
        let mut v = base();
        v[5] = v[5].clone().mem_private(None); // 212
        v[4] = v[4].clone().mem_private(Some(5 * MIB)); // 211
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = s.index()[&210].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Mcp(k), KillScope::Tree).unwrap();
        assert_eq!((pl.freed, pl.freed_approx), (6 * MIB, true));
        assert_eq!(pl.targets[0].mem, None);
        let mut v = base();
        v[5].mem = None;
        let (s, c, t) = setup(v, Platform::MacOs);
        let pl = plan_kill(&s, &c, &t, &NodeId::Mcp(k), KillScope::Tree).unwrap();
        assert_eq!((pl.freed, pl.freed_approx), (2 * MIB, true));
    }

    #[test]
    fn a_detached_process_above_its_own_session_does_not_loop() {
        // Hand-made: 150 is detached from session 200, yet 200's parent.
        let v = vec![
            p(1, 0, 1, &["/sbin/launchd"]),
            p(150, 1, 5, &["node", "w.js"]),
            p(200, 150, 20, &["claude"]),
            p(210, 200, 21, &["/bin/sh", "-c", "ls"]),
        ];
        let s = snap(Platform::MacOs, v);
        let mut c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        c.roles.insert(k(150), Role::Detached { session: k(200) });
        let t = build_tree(&s, &c, ViewMode::BySession);
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k(200)), KillScope::Tree).unwrap();
        let got: HashSet<Pid> = pids(&pl).into_iter().collect();
        assert_eq!(got, HashSet::from([150, 200, 210]));
        assert_eq!(pl.targets.len(), 3);
        // Whatever order the loop is cut in, a child still comes first.
        let at = |pid| pids(&pl).iter().position(|&x| x == pid);
        assert!(at(210) < at(200));
    }

    #[test]
    fn a_very_deep_chain_is_planned_deepest_first() {
        let depth: u32 = 3_000;
        let mut v = vec![p(200, 1, 2, &["claude"])];
        v.extend((201..200 + depth).map(|pid| p(pid, pid - 1, u64::from(pid), &["sleep"])));
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = s.index()[&200].key;
        let pl = plan_kill(&s, &c, &t, &NodeId::Proc(k), KillScope::Tree).unwrap();
        let want: Vec<Pid> = (200..200 + depth).rev().collect();
        assert_eq!(pids(&pl), want);
    }

    // ---- Fix round 1 (scope ruling) ----

    #[test]
    fn x_ends_what_the_row_shows() {
        let mut v = base();
        v.extend([
            // A dev server detached from session 200 (221 carries its id S1).
            p(400, 1, 70, &["node", "dev-server.js"]).env(SID, "S1"),
            // A session nested in 200, started from the Bash-tool shell 230,
            // known by the id N1 its child carries; and a dev server detached
            // from it.
            p(300, 230, 40, &["claude"]).cwd("/Users/u/prj/n"),
            p(301, 300, 41, &["/bin/zsh", "-c", "npm run dev"]).env(SID, "N1"),
            p(450, 1, 75, &["node", "dev.js"]).env(SID, "N1"),
        ]);
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(450)), &Role::Detached { session: k(300) });
        let plan = |id: NodeId| pids(&plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap());
        // The session's row shows its own detached process, and the nested
        // session with what it detached.
        let all = plan(NodeId::Proc(k(200)));
        assert!([400, 300, 301, 450].iter().all(|x| all.contains(x)));
        // "Other children" shows the shell only: the session started from it
        // is a row of its own, and what that session detached is not here.
        assert_eq!(plan(NodeId::OtherChildren(k(200))), [230]);
    }

    #[test]
    fn under_sudo_other_users_processes_may_be_ended() {
        let mut v = base();
        v.push(p(99999, 100, 90, &["cctree"]).root_user());
        v.push(p(251, 200, 61, &["node", "x"]).root_user());
        v.push(p(252, 200, 61, &["node", "y"]).other_user());
        v.push(p(253, 200, 61, &["node", "z"]).unknown_owner());
        let (mut s, c, t) = setup(v, Platform::MacOs);
        s.self_key = s.index()[&99999].key;
        let k = |pid| s.index()[&pid].key;
        let one = |pid| plan_kill(&s, &c, &t, &NodeId::Proc(k(pid)), KillScope::One);
        // Another user's process is fine now; a root process still is not,
        // nor one whose owner could not be read.
        assert_eq!(pids(&one(252).unwrap()), [252]);
        assert_eq!(one(251), Err(KillRefusal::System { pid: 251 }));
        assert_eq!(one(253), Err(KillRefusal::System { pid: 253 }));
    }

    struct FakeProc {
        alive: bool,
        ignores_term: bool,
        reused: bool,
    }

    /// A pretend operating system: each process is alive or not, may ignore
    /// `terminate`, and may be a different process now that has the same PID.
    struct FakeControl {
        procs: Mutex<HashMap<Pid, FakeProc>>,
        signalled: Mutex<Vec<Pid>>,
        terminates: Mutex<usize>,
    }

    impl FakeControl {
        /// `(pid, alive, ignores_term, reused)` for each process.
        fn new<const N: usize>(v: [(Pid, bool, bool, bool); N]) -> FakeControl {
            let procs = v
                .into_iter()
                .map(|(pid, alive, ignores_term, reused)| {
                    (
                        pid,
                        FakeProc {
                            alive,
                            ignores_term,
                            reused,
                        },
                    )
                })
                .collect();
            FakeControl {
                procs: Mutex::new(procs),
                signalled: Mutex::new(Vec::new()),
                terminates: Mutex::new(0),
            }
        }

        fn was_signalled(&self, pid: Pid) -> bool {
            self.signalled.lock().unwrap().contains(&pid)
        }

        fn terminate_calls(&self) -> usize {
            *self.terminates.lock().unwrap()
        }

        /// Signals `pid`; `ends` says whether the process dies of it.
        fn signal(&self, pid: Pid, ends: impl Fn(&FakeProc) -> bool) -> Result<(), KillError> {
            self.signalled.lock().unwrap().push(pid);
            match self.procs.lock().unwrap().get_mut(&pid) {
                Some(p) if p.alive => {
                    p.alive = !ends(p);
                    Ok(())
                }
                _ => Err(KillError::NotFound),
            }
        }
    }

    impl ProcessControl for FakeControl {
        fn state(&self, key: ProcKey) -> ProcState {
            match self.procs.lock().unwrap().get(&key.pid) {
                Some(p) if p.alive && p.reused => ProcState::Reused,
                Some(p) if p.alive => ProcState::Same,
                _ => ProcState::Gone,
            }
        }

        fn terminate(&self, pid: Pid) -> Result<(), KillError> {
            *self.terminates.lock().unwrap() += 1;
            self.signal(pid, |p| !p.ignores_term)
        }

        fn force(&self, pid: Pid) -> Result<(), KillError> {
            self.signal(pid, |_| true)
        }
    }

    /// A plan that ends `pids` in this order, every other field at its default.
    fn plan_of(pids: &[Pid]) -> KillPlan {
        KillPlan {
            subject: NodeId::OtherGroup(String::new()),
            targets: pids
                .iter()
                .map(|&pid| KillTarget {
                    key: ProcKey { pid, start_time: 0 },
                    name: String::new(),
                    detached: false,
                    mem: None,
                })
                .collect(),
            warnings: Vec::new(),
            freed: 0,
            freed_approx: false,
            force_only: false,
        }
    }

    #[test]
    fn graceful_forced_gone_and_reused() {
        // (pid, alive, ignores_term, reused): pid 1 is gone from the start;
        // pid 2 ignores SIGTERM; pid 4's PID now belongs to another process.
        let ctl = FakeControl::new([
            (1, false, false, false),
            (2, true, true, false),
            (4, true, false, true),
        ]);
        let plan = plan_of(&[2, 4, 1]);
        let mut seen = vec![];
        let r = execute(
            &plan,
            &ctl,
            Duration::from_millis(50),
            Duration::from_millis(5),
            &mut |d, t| seen.push((d, t)),
        );
        assert_eq!(
            (
                r.graceful,
                r.forced,
                r.already_gone,
                r.skipped_reused.clone()
            ),
            (0, 1, 1, vec![4])
        );
        assert_eq!(seen.last(), Some(&(3, 3)));
        assert!(!ctl.was_signalled(4));
    }

    #[test]
    fn force_only_never_terminates() {
        let ctl = FakeControl::new([(5, true, false, false)]);
        let mut plan = plan_of(&[5]);
        plan.force_only = true;
        let r = execute(
            &plan,
            &ctl,
            Duration::from_millis(50),
            Duration::from_millis(5),
            &mut |_, _| {},
        );
        assert_eq!((r.forced, ctl.terminate_calls()), (1, 0));
    }
}
