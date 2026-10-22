//! Ending processes (spec 4.7), the planning half: [`plan_kill`] works out
//! exactly which processes `x` or `X` on a row would end, in which order, or
//! why it must not. Nothing is signalled here.
//!
//! # Rules
//!
//! - **`x`** ([`KillScope::One`]): on a process row, that process. A `SelfRow`
//!   is its process. Any group row is [`KillRefusal::GroupNeedsX`].
//! - **`X`** ([`KillScope::Tree`]):
//!   - on the Claude Code, Codex and Other Node.js categories:
//!     [`KillRefusal::CategoryTooBroad`], whatever is below them;
//!   - on a process row: the process and all its descendants in the validated
//!     lineage ([`Lineage`]);
//!   - on a group row (MCP group, one MCP, other children, detached, a
//!     Node.js group, the overview's rows): every process of the row
//!     ([`member_keys`]) and all their descendants;
//!   - on the orphan category: only the [`Certainty::Confirmed`] orphan roots
//!     and their descendants; `excluded_suspected` counts the suspected roots.
//!
//!   Two rules bound what the walk down the descendants takes in:
//!   - **Detached processes** ([`Role::Detached`]) and their descendants
//!     come along only for `X` on a session's own row: those of that session
//!     and of every session drawn under it (nested in it through `outer`), as
//!     the row shows them. `X` on any other row (a group, a plain process)
//!     never takes a detached process the row does not show, even when it
//!     ends a nested session as a descendant.
//!   - **Sessions not drawn under the row stop the walk**: a session root
//!     reached by the walk is ended (with its subtree) only if it belongs
//!     under the row's own sessions (the anchors: a session row's root; the
//!     session a member, other child or detached process belongs to; none for
//!     orphans and other Node.js), i.e. it is an anchor or nested in one.
//!     Otherwise it and its whole subtree are left out: a top-level session
//!     the user started in a tmux server that is detached from the session
//!     being ended, or one a Node.js tool started.
//! - **Left out**: zombies within an `X` (they have already ended), and keys
//!   that are no longer in the snapshot.
//! - **Order**: deepest first. A target's depth is the number of targets
//!   among its ancestors, where the root of a detached group counts as a child
//!   of its session's root. Ties go by key (PID, then start time), so the
//!   subject's root comes last.
//! - **Refusals**, checked in this order:
//!   1. any target is cctree itself or one of its ancestors (the shell, the
//!      terminal, a session cctree runs in): [`KillRefusal::SelfOrAncestor`].
//!      cctree is [`Snapshot::self_key`] and also whatever process has its
//!      PID (should the start time differ); its ancestors are those of the
//!      lineage and those of the recorded `ppid` chain, where the lineage
//!      dropped a link;
//!   2. `x` on a zombie: [`KillRefusal::Zombie`] (also `X` on a zombie whose
//!      tree has nothing else to end, which says more than "nothing to
//!      kill"). It comes before the next two: a zombie needs no ending, so
//!      suggesting `sudo` would be wrong;
//!   3. any target is PID 0 or 1, or runs as root / SYSTEM:
//!      [`KillRefusal::System`] (the first one in kill order);
//!   4. any target belongs to another user, unless cctree itself runs as
//!      root (`sudo cctree`; unknown counts as not root):
//!      [`KillRefusal::OtherUser`];
//!   5. `X` on the orphan category when every orphan is only suspected:
//!      [`KillRefusal::OnlySuspected`];
//!   6. no target at all (an unknown or stale row, a note row, an empty
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
//!   consulted. If no process in the snapshot has its PID, its ancestors are
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
//! targets (plus an `outer` walk per session reached); no recursion, so any
//! depth of process tree is fine.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::classify::argv::display_name;
use crate::classify::lineage::Lineage;
use crate::classify::{Category, Certainty, Classification, Role, SessionInfo};
use crate::model::{Pid, Platform, ProcKey, ProcStatus, RawProc, Snapshot, Tool};
use crate::tree::{Node, NodeId, find, member_keys};

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
    /// Suspected orphans that `X` on the orphan category leaves alone.
    pub excluded_suspected: usize,
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
    /// `X` on the orphan category, which holds only suspected orphans (`n`).
    OnlySuspected {
        n: usize,
    },
    /// `X` on a whole category other than the orphans.
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
    if scope == KillScope::Tree
        && matches!(
            id,
            NodeId::Category(Category::ClaudeCode | Category::Codex | Category::OtherNode)
        )
    {
        return Err(KillRefusal::CategoryTooBroad);
    }
    let node = find(roots, id).ok_or(KillRefusal::NothingToKill)?;
    let ctx = Ctx::new(snap, cls);
    match (id, scope) {
        (NodeId::Proc(k), KillScope::One) | (NodeId::SelfRow(k), _) => ctx.one(id, *k),
        (NodeId::Proc(k), KillScope::Tree) => ctx.tree(id, &[*k], 0),
        (NodeId::NestedNote(_), _) => Err(KillRefusal::NothingToKill),
        (_, KillScope::One) => Err(KillRefusal::GroupNeedsX),
        (NodeId::Category(Category::Orphaned), KillScope::Tree) => {
            let mut seeds = Vec::new();
            let mut suspected = 0;
            for c in &node.children {
                let NodeId::Proc(k) = c.id else { continue };
                match cls.role(k) {
                    Role::Orphan {
                        certainty: Certainty::Confirmed,
                        ..
                    } => seeds.push(k),
                    Role::Orphan {
                        certainty: Certainty::Suspected,
                        ..
                    } => suspected += 1,
                    _ => {}
                }
            }
            if seeds.is_empty() && suspected > 0 {
                return Err(KillRefusal::OnlySuspected { n: suspected });
            }
            ctx.tree(id, &seeds, suspected)
        }
        (_, KillScope::Tree) => ctx.tree(id, &member_keys(node), 0),
    }
}

/// What one plan is worked out from.
struct Ctx<'a> {
    snap: &'a Snapshot,
    cls: &'a Classification,
    l: Lineage<'a>,
    by_key: HashMap<ProcKey, &'a RawProc>,
    /// The `outer` of every session, by root.
    outer: HashMap<ProcKey, Option<ProcKey>>,
}

impl<'a> Ctx<'a> {
    fn new(snap: &'a Snapshot, cls: &'a Classification) -> Ctx<'a> {
        Ctx {
            snap,
            cls,
            l: Lineage::build(snap),
            by_key: snap.procs.iter().map(|p| (p.key, p)).collect(),
            outer: cls.sessions.iter().map(|s| (s.root, s.outer)).collect(),
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
        Ok(self.plan(subject, vec![k], 0))
    }

    /// `X`: see [`Ctx::closure`]; zombies left out.
    fn tree(
        &self,
        subject: &NodeId,
        seeds: &[ProcKey],
        excluded_suspected: usize,
    ) -> Result<KillPlan, KillRefusal> {
        // The sessions the row belongs to: a session below the row may be
        // ended only if it is drawn under one of them.
        let anchors: HashSet<ProcKey> = seeds.iter().filter_map(|&k| self.owner(k)).collect();
        // Only a session's own row takes detached processes along.
        let session_row = match subject {
            NodeId::Proc(k) if *self.cls.role(*k) == Role::SessionRoot => Some(*k),
            _ => None,
        };
        let keys: Vec<ProcKey> = self
            .closure(seeds, &anchors, session_row)
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
        Ok(self.plan(subject, ordered, excluded_suspected))
    }

    /// The session `k` belongs to: itself for a session root, the session
    /// named by its role for a member or a detached process.
    fn owner(&self, k: ProcKey) -> Option<ProcKey> {
        match self.cls.role(k) {
            Role::SessionRoot => Some(k),
            Role::Mcp { session, .. }
            | Role::OtherChild { session }
            | Role::Detached { session } => Some(*session),
            Role::Orphan { .. }
            | Role::OrphanMember { .. }
            | Role::OtherNode { .. }
            | Role::Unrelated => None,
        }
    }

    /// The session rooted at `s` is one of `tops`, or nested (through its
    /// `outer` chain) in one of them.
    fn drawn_under(&self, s: ProcKey, tops: &HashSet<ProcKey>) -> bool {
        let mut cur = Some(s);
        // `outer` names an ancestor, so the chain ends; the bound only guards
        // a hand-made, cyclic classification.
        for _ in 0..=self.outer.len() {
            let Some(c) = cur else { return false };
            if tops.contains(&c) {
                return true;
            }
            cur = self.outer.get(&c).copied().flatten();
        }
        false
    }

    /// Every key reachable from `seeds`, each once and only if it is in the
    /// snapshot, through lineage children. A session root that is not drawn
    /// under one of `anchors` stops the walk: it and its subtree are left
    /// out. When the row is the session `session_row`, the processes detached
    /// from it or from a session drawn under it come along too.
    fn closure(
        &self,
        seeds: &[ProcKey],
        anchors: &HashSet<ProcKey>,
        session_row: Option<ProcKey>,
    ) -> Vec<ProcKey> {
        let row: HashSet<ProcKey> = session_row.into_iter().collect();
        let mut detached_of: Option<HashMap<ProcKey, Vec<ProcKey>>> = None;
        let mut seen: HashSet<ProcKey> = HashSet::new();
        let mut out = Vec::new();
        let mut stack: Vec<ProcKey> = seeds.iter().rev().copied().collect();
        while let Some(k) = stack.pop() {
            if !self.by_key.contains_key(&k) || !seen.insert(k) {
                continue;
            }
            let session = *self.cls.role(k) == Role::SessionRoot;
            if session && !self.drawn_under(k, anchors) {
                continue;
            }
            out.push(k);
            stack.extend(self.l.children(k).iter().rev());
            if session && self.drawn_under(k, &row) {
                let of = detached_of.get_or_insert_with(|| self.detached_by_session());
                if let Some(d) = of.get(&k) {
                    stack.extend(d.iter().rev());
                }
            }
        }
        out
    }

    /// The detached processes of each session, by session root, in snapshot
    /// order.
    fn detached_by_session(&self) -> HashMap<ProcKey, Vec<ProcKey>> {
        let mut of: HashMap<ProcKey, Vec<ProcKey>> = HashMap::new();
        for p in &self.snap.procs {
            if let Role::Detached { session } = self.cls.role(p.key) {
                of.entry(*session).or_default().push(p.key);
            }
        }
        of
    }

    /// cctree and its ancestors are never ended.
    fn refuse_self(&self, keys: &[ProcKey]) -> Result<(), KillRefusal> {
        let protected = self.protected();
        if keys.iter().any(|k| protected.contains(k)) {
            return Err(KillRefusal::SelfOrAncestor);
        }
        Ok(())
    }

    /// cctree ([`Snapshot::self_key`], and whatever process has its PID, in
    /// case the start time differs), with the ancestors of each through the
    /// lineage and through the recorded `ppid` (where the lineage dropped a
    /// link).
    fn protected(&self) -> HashSet<ProcKey> {
        let me = self.snap.self_key;
        let mut out = HashSet::from([me]);
        for start in [Some(me), self.l.key_of(me.pid)].into_iter().flatten() {
            out.insert(start);
            out.extend(self.l.ancestors(start));
            let mut walked: HashSet<ProcKey> = HashSet::new();
            let mut cur = self.by_key.get(&start).and_then(|p| p.ppid);
            while let Some(k) = cur.and_then(|pid| self.l.key_of(pid)) {
                if !walked.insert(k) {
                    break;
                }
                out.insert(k);
                cur = self.by_key.get(&k).and_then(|p| p.ppid);
            }
        }
        out
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
    fn plan(&self, subject: &NodeId, keys: Vec<ProcKey>, excluded_suspected: usize) -> KillPlan {
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
            excluded_suspected,
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

/// System processes are refused, and so are other users' unless cctree runs
/// as root; the first one in `procs` is named.
fn refuse_owner(procs: &[&RawProc], self_is_root: bool) -> Result<(), KillRefusal> {
    if let Some(p) = procs.iter().find(|p| p.key.pid <= 1 || p.is_root_user) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::argv::display_name;
    use crate::classify::{Category, Role, classify};
    use crate::model::{Platform, RawProc};
    use crate::testkit::{base, cfgs, p, snap};
    use crate::tree::{ViewMode, build_tree};
    use std::collections::HashSet;

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

    #[test]
    fn orphan_category_kills_only_confirmed() {
        let mut v = base();
        // Amended: since Task 13 an orphan is confirmed only when no live
        // session could own it. Session 200 (cwd /Users/u/prj/a) started
        // before it, so without a project directory of its own 401 would be
        // suspected; a project unrelated to 200's keeps it confirmed.
        v.push(
            p(401, 1, 70, &["node", "x"])
                .env("CLAUDE_CODE_SESSION_ID", "GONE")
                .env(PROJ, "/Users/u/prj/old"),
        );
        v.push(p(
            80871,
            1,
            5,
            &["/private/var/folders/x/T/codex-home-Z/r/ChatGPT Meetings"],
        ));
        let (s, c, t) = setup(v, Platform::MacOs);
        let pl = plan_kill(
            &s,
            &c,
            &t,
            &NodeId::Category(Category::Orphaned),
            KillScope::Tree,
        )
        .unwrap();
        assert_eq!((pl.targets.len(), pl.excluded_suspected), (1, 1));
    }

    #[test]
    fn whole_category_kill_is_refused_except_orphans() {
        let (s, c, t) = setup(base(), Platform::MacOs);
        for cat in [Category::ClaudeCode, Category::Codex, Category::OtherNode] {
            assert!(
                matches!(
                    plan_kill(&s, &c, &t, &NodeId::Category(cat), KillScope::Tree),
                    Err(KillRefusal::CategoryTooBroad)
                ),
                "{cat:?}"
            );
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
        assert_eq!(pl.excluded_suspected, 0);
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
    fn group_rows_take_their_members_and_descendants_once() {
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

        // The other children are cut at the nested session in the tree, but
        // `X` ends the descendants too: the nested session with its MCP.
        let pl = plan(NodeId::OtherChildren(k(200)));
        assert_eq!(pids(&pl), [311, 310, 231, 300, 230]);
        assert_eq!(
            pl.warnings,
            [KillWarning::InterruptsSession {
                tool: Tool::Codex,
                cwd: Some("/Users/u/prj/c".into()),
                hint: ResumeHint::Command("codex resume".into()),
            }]
        );

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

        // A confirmed orphan root and its member: the member first.
        let pl = plan(NodeId::Category(Category::Orphaned));
        assert_eq!((pids(&pl), pl.excluded_suspected), (vec![431, 430], 0));
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
        // No orphans at all.
        assert_eq!(
            plan_kill(
                &s,
                &c,
                &t,
                &NodeId::Category(Category::Orphaned),
                KillScope::Tree
            ),
            Err(KillRefusal::NothingToKill)
        );
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
    fn only_suspected_orphans_are_refused_as_such() {
        let mut v = base();
        // Started after session 200 with no project: 200 could own it.
        v.push(p(401, 1, 70, &["node", "x"]).env(SID, "GONE"));
        v.push(p(
            80871,
            1,
            5,
            &["/private/var/folders/x/T/codex-home-Z/r/ChatGPT Meetings"],
        ));
        let (s, c, t) = setup(v, Platform::MacOs);
        assert_eq!(
            plan_kill(
                &s,
                &c,
                &t,
                &NodeId::Category(Category::Orphaned),
                KillScope::Tree
            ),
            Err(KillRefusal::OnlySuspected { n: 2 })
        );
        // Ended one at a time, it is fine.
        let k = s.index()[&401].key;
        assert!(matches!(
            c.role(k),
            Role::Orphan {
                certainty: crate::classify::Certainty::Suspected,
                ..
            }
        ));
        assert_eq!(
            pids(&plan_kill(&s, &c, &t, &NodeId::Proc(k), KillScope::One).unwrap()),
            [401]
        );
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

    // ---- Fix round 1 ----

    /// [`base`] plus a Claude Code session 300 nested in 200 (started from
    /// the Bash-tool shell 230), known by the id `N1` its child 301 carries,
    /// and a dev server 450 detached from 300.
    fn nested_with_detached() -> Vec<RawProc> {
        let mut v = base();
        v.extend([
            p(300, 230, 40, &["claude"]).cwd("/Users/u/prj/n"),
            p(301, 300, 41, &["/bin/zsh", "-c", "npm run dev"]).env(SID, "N1"),
            p(450, 1, 75, &["node", "dev.js"]).env(SID, "N1"),
        ]);
        v
    }

    #[test]
    fn only_a_session_row_takes_detached_processes_along() {
        let (s, c, t) = setup(nested_with_detached(), Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(450)), &Role::Detached { session: k(300) });
        assert_eq!(c.session(k(300)).and_then(|x| x.outer), Some(k(200)));
        let plan = |id: NodeId| pids(&plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap());
        // (a) X on the outer session's row: the nested session is drawn under
        // it, and so is the process detached from the nested session.
        let all = plan(NodeId::Proc(k(200)));
        assert!(all.contains(&300) && all.contains(&301) && all.contains(&450));
        assert_eq!(all.last(), Some(&200));
        assert!(plan(NodeId::Proc(k(300))).contains(&450));
        // (b) X on a group row or a plain process row: the nested session is a
        // descendant and goes, the process detached from it stays.
        for id in [NodeId::OtherChildren(k(200)), NodeId::Proc(k(230))] {
            let got = plan(id.clone());
            assert!(got.contains(&300) && got.contains(&301), "{id:?}");
            assert!(!got.contains(&450), "{id:?}");
        }
    }

    #[test]
    fn a_session_not_drawn_under_the_row_stops_the_walk() {
        let mut v = base();
        v.extend([
            // A tmux server started from session 200 (it carries 200's id)...
            p(600, 1, 50, &["tmux", "new", "-d"]).env(SID, "S1"),
            p(601, 600, 51, &["-zsh"]),
            // ...where the user later started a session of their own.
            p(602, 601, 52, &["claude"]).cwd("/Users/u/prj/f"),
            p(603, 602, 53, &["/bin/zsh", "-c", "ls"]),
            // A Node.js tool that started a session of its own.
            p(500, 1, 90, &["node", "tool.js"]),
            p(501, 500, 91, &["claude"]).cwd("/Users/u/prj/g"),
        ]);
        let (s, c, t) = setup(v, Platform::MacOs);
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(600)), &Role::Detached { session: k(200) });
        assert_eq!(c.session(k(602)).map(|x| x.outer), Some(None));
        assert_eq!(c.session(k(501)).map(|x| x.outer), Some(None));
        for id in [
            NodeId::Proc(k(200)),
            NodeId::Detached(k(200)),
            NodeId::Proc(k(600)),
        ] {
            let pl = plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap();
            let got = pids(&pl);
            assert!(got.contains(&600) && got.contains(&601), "{id:?}");
            assert!(!got.contains(&602) && !got.contains(&603), "{id:?}");
            assert!(
                !pl.warnings.iter().any(|w| matches!(
                    w,
                    KillWarning::InterruptsSession { cwd: Some(c), .. } if c == "/Users/u/prj/f"
                )),
                "{id:?}"
            );
        }
        // A row that belongs to no session: every session below it stops the walk.
        let group = t[3].children[0].id.clone();
        assert!(matches!(group, NodeId::OtherGroup(_)));
        for id in [group, NodeId::Proc(k(500))] {
            let pl = plan_kill(&s, &c, &t, &id, KillScope::Tree).unwrap();
            assert_eq!(pids(&pl), [500], "{id:?}");
            assert!(pl.warnings.is_empty());
        }
    }

    #[test]
    fn under_sudo_other_users_processes_may_be_ended() {
        let mut v = base();
        v.push(p(99999, 100, 90, &["cctree"]).root_user());
        v.push(p(251, 200, 61, &["node", "x"]).root_user());
        v.push(p(252, 200, 61, &["node", "y"]).other_user());
        let (mut s, c, t) = setup(v, Platform::MacOs);
        let key = |s: &Snapshot, pid| s.index()[&pid].key;
        let plan =
            |s: &Snapshot, pid, scope| plan_kill(s, &c, &t, &NodeId::Proc(key(s, pid)), scope);
        s.self_key = key(&s, 99999);
        // Root: another user's process is fine, a root process still is not.
        assert_eq!(pids(&plan(&s, 252, KillScope::One).unwrap()), [252]);
        assert_eq!(
            plan(&s, 251, KillScope::One),
            Err(KillRefusal::System { pid: 251 })
        );
        assert_eq!(
            plan(&s, 200, KillScope::Tree),
            Err(KillRefusal::System { pid: 251 })
        );
        // Not root, or not known: refused as before.
        for me in [
            key(&s, 230),
            ProcKey {
                pid: 4242,
                start_time: 1,
            },
        ] {
            s.self_key = me;
            assert_eq!(
                plan(&s, 252, KillScope::One),
                Err(KillRefusal::OtherUser { pid: 252 })
            );
        }
    }

    #[test]
    fn cctree_is_found_where_the_lineage_lost_it() {
        let refused = |s: &Snapshot, c: &Classification, t: &[Node]| {
            let k = |pid| s.index()[&pid].key;
            for (id, scope) in [
                (NodeId::Proc(k(200)), KillScope::Tree),
                (NodeId::OtherChildren(k(200)), KillScope::Tree),
                (NodeId::Proc(k(230)), KillScope::One),
                (NodeId::Proc(k(200)), KillScope::One),
            ] {
                assert_eq!(
                    plan_kill(s, c, t, &id, scope),
                    Err(KillRefusal::SelfOrAncestor),
                    "{id:?} {scope:?}"
                );
            }
        };
        // cctree's recorded parent 230 looks younger than cctree, so the
        // lineage drops the link; the recorded ppid still leads to it.
        let mut v = base();
        v.push(p(99999, 230, 25, &["cctree"]));
        let (mut s, c, t) = setup(v, Platform::MacOs);
        s.self_key = s.index()[&99999].key;
        assert_eq!(Lineage::build(&s).parent(s.self_key), None);
        refused(&s, &c, &t);
        // `self_key` with a start time that does not match: the PID decides.
        let mut v = base();
        v.push(p(99999, 230, 90, &["cctree"]));
        let (mut s, c, t) = setup(v, Platform::MacOs);
        s.self_key = ProcKey {
            pid: 99999,
            start_time: 0,
        };
        refused(&s, &c, &t);
    }
}
