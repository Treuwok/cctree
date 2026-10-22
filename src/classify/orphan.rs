//! The second classification pass: roles for the processes outside every
//! session tree, which the session pass ([`super::classify`]) leaves without
//! one. Such a process is
//!
//! - **detached** ([`Role::Detached`]): it names a live session that started
//!   it, though it is no longer in that session's tree (a dev server the Bash
//!   tool started with `nohup`). It still belongs to that session;
//! - **orphaned** ([`Role::Orphan`]): its owner has ended and it is still
//!   here, confirmed or only suspected;
//! - **other Node.js** ([`Role::OtherNode`]): neither, but a `node`, `npm`,
//!   `npx`, `bun` or `deno` process (process name or `argv[0]` file name,
//!   ignoring case and an executable extension), or one that
//!   [`looks_like_mcp`];
//! - **unrelated**: anything else (no role is recorded).
//!
//! Evidence of the owner, strongest first (spec §4.3); the first kind a
//! process has decides:
//!
//! 1. **Strong**: an environment variable that names the session
//!    ([`SESSION_MARKERS`]: `CLAUDE_CODE_SESSION_ID` for Claude Code; none is
//!    known for Codex yet). Every id carried by a member of a live session
//!    (an MCP member or another child, inside an MCP node too; not the root,
//!    which only inherits its starter's id) names that session, so a resumed
//!    session that carries two ids is known by both. An id that names a live
//!    session makes the process detached from it. Any other id makes it an
//!    orphan, confirmed only when it is [sure](#when-an-orphan-is-confirmed).
//!    An id that no live member carries does not prove its session ended: a
//!    session's id changes while it runs (`/clear`, `--resume`), its MCP
//!    servers keep the old one, and the new one is only on children started
//!    since, which may all have exited.
//! 2. **Medium**: `--parent-pid=N` or `--parent-pid N`. When no process has
//!    PID N, or the one that has it started after this process (the PID was
//!    reused), the recorded owner is gone: an orphan, confirmed only when it
//!    is [sure](#when-an-orphan-is-confirmed). A live owner is no evidence.
//! 3. **Weak**: [`path_evidence`] names a tool, nobody meaningful started the
//!    process ([`Lineage::is_adopted`]) and no session of that tool is
//!    running: a suspected orphan.
//!
//! # When an orphan is confirmed
//!
//! A confirmed orphan group may be ended as a whole, so strong and medium
//! evidence give a confirmed orphan only when all of these hold, and a
//! suspected one otherwise (same reason):
//!
//! - nobody meaningful is its parent ([`Lineage::is_adopted`]: no valid
//!   parent, PID 1 or a subreaper), so no live process (a terminal
//!   multiplexer, a session cctree does not recognise) may still own it;
//! - it is no app the user runs: its exe is not in a `.app` bundle, under
//!   `/Applications/`, or (Windows) in `Program Files`, `Program Files (x86)`
//!   or `WindowsApps`;
//! - for strong evidence, no live session of the same tool **could own** it:
//!   one that started no later than the process and, when the process was
//!   given a `CLAUDE_PROJECT_DIR`, works in a directory related to it (equal,
//!   or one inside the other at a path boundary). Without
//!   `CLAUDE_PROJECT_DIR` (Bash-tool children never carry it, and the Bash
//!   tool can `cd` anywhere) any such session could own it, whatever the cwd.
//!   A session works in its root's cwd and in every project its members were
//!   given (a worktree session gives them the main checkout); a session with
//!   neither known could own any process started after it, and every session
//!   could own a process whose own start time is unknown. Paths compare with
//!   `/` separators and no trailing one, ignoring ASCII case on macOS and
//!   Windows snapshots and in Windows-style paths (a `\` or a drive letter).
//!
//! # Choices the spec leaves open
//!
//! - **Groups**: the descendants of a detached process are detached from the
//!   same session, and the descendants of an orphan are its
//!   [`Role::OrphanMember`]s, whatever evidence they carry themselves. A
//!   process that already has a role keeps it, and its subtree is judged
//!   afresh.
//! - **Still in use**: an orphan group can be ended as a whole, so it must
//!   never take in a process that is still in use. A process is never an
//!   orphan if it is, or is an ancestor of, the root of a live session,
//!   cctree itself, or a process whose id names a live session. Ancestors
//!   here follow the recorded `ppid` too, where the lineage dropped it.
//! - **An id seen in several live sessions** names a session of the id's own
//!   tool first (the members of a Codex session started from Claude Code
//!   inherit that Claude Code session's id), then the session found first.
//! - **Empty values** (an empty id or project directory) count as missing.
//!   PID 0 is no recorded owner, and when a process's own start time is
//!   unknown (0) only a missing PID proves its owner gone.
//! - **Other Node.js group**: [`Lineage::top_app`], or the process's own
//!   [`app_name`] when there is none.
//!
//! Every step is one pass over the processes with hash-map lookups (one per
//! path component for the "could own" check), so the whole pass is linear in
//! the number of processes.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use crate::model::{Pid, Platform, ProcKey, RawProc, Snapshot, Tool};

use super::argv::{base_name, looks_like_mcp};
use super::lineage::{Lineage, app_name, is_shell, is_system_root};
use super::rules::path_evidence;
use super::{Certainty, OrphanReason, PROJECT_DIR_ENV, Role, SESSION_ID_ENV, SessionInfo};

/// Environment variables through which a tool tells the processes it starts
/// which session started them.
struct SessionMarker {
    tool: Tool,
    /// Holds the session's id.
    id: &'static str,
    /// Holds the session's project directory.
    project_dir: &'static str,
}

/// Strong evidence, one entry per tool. Codex's equivalent is not known yet
/// (spec §6).
const SESSION_MARKERS: &[SessionMarker] = &[SessionMarker {
    tool: Tool::ClaudeCode,
    id: SESSION_ID_ENV,
    project_dir: PROJECT_DIR_ENV,
}];

/// Program names (file name without an executable extension, any case) that
/// count as other Node.js.
const NODE_PROGRAMS: &[&str] = &["node", "npm", "npx", "bun", "deno"];

/// The option a process records its owner's PID in (chrome-devtools-mcp's
/// telemetry watchdog).
const PARENT_PID_FLAG: &str = "--parent-pid";

/// What a process's ancestors pass on to it.
#[derive(Clone, Copy)]
enum Inherited {
    Nothing,
    /// Detached from the session rooted here.
    Detached(ProcKey),
    /// A member of the orphan group rooted here.
    Orphan(ProcKey),
}

/// What the evidence on one process says.
enum Verdict {
    Detached(ProcKey),
    Orphan(Certainty, OrphanReason),
}

/// Gives a role to every process that has none in `roles` yet: detached,
/// orphaned, other Node.js, or (no entry) unrelated. See the module
/// documentation for the rules. Roles already in `roles` never change. Total:
/// any snapshot gives a result, with no panic.
pub(crate) fn assign_rest(
    snap: &Snapshot,
    l: &Lineage,
    sessions: &[SessionInfo],
    roles: &mut HashMap<ProcKey, Role>,
) {
    let live = Live::build(snap, sessions, roles);
    let in_use = in_use(snap, l, sessions, &live, roles);
    let home = snap.home.as_deref();

    let mut visited: HashSet<ProcKey> = HashSet::with_capacity(snap.procs.len());
    // Pre-order over the forest, tops and children in snapshot order. Each
    // entry carries what its ancestors pass on and its farthest ancestor that
    // is an app (what `Lineage::top_app` would walk up to find).
    let mut stack: Vec<(ProcKey, Inherited, Option<ProcKey>)> = snap
        .procs
        .iter()
        .rev()
        .filter(|p| l.parent(p.key).is_none())
        .map(|p| (p.key, Inherited::Nothing, None))
        .collect();
    while let Some((k, inherited, top)) = stack.pop() {
        // A PID listed twice under the same key would be reached twice.
        if !visited.insert(k) {
            continue;
        }
        let p = l.proc_(k);
        let pass_on = match roles.entry(k) {
            Entry::Occupied(_) => Inherited::Nothing,
            Entry::Vacant(slot) => {
                let (role, pass_on) = match inherited {
                    Inherited::Detached(session) => (Some(Role::Detached { session }), inherited),
                    Inherited::Orphan(root) => (Some(Role::OrphanMember { root }), inherited),
                    Inherited::Nothing => match live.judge(p, l, home) {
                        Some(Verdict::Detached(session)) => (
                            Some(Role::Detached { session }),
                            Inherited::Detached(session),
                        ),
                        Some(Verdict::Orphan(certainty, reason)) if !in_use.contains(&k) => (
                            Some(Role::Orphan { certainty, reason }),
                            Inherited::Orphan(k),
                        ),
                        _ => (
                            other_node(p, top.map(|t| l.proc_(t)), home),
                            Inherited::Nothing,
                        ),
                    },
                };
                if let Some(role) = role {
                    slot.insert(role);
                }
                pass_on
            }
        };
        let top = top.or_else(|| (!is_shell(p) && !is_system_root(p)).then_some(k));
        stack.extend(
            l.children(k)
                .iter()
                .rev()
                .map(|&child| (child, pass_on, top)),
        );
    }
}

/// What the live sessions are known by.
struct Live<'a> {
    /// The live session (root) each session id names, per tool.
    ids: HashMap<(Tool, &'a str), ProcKey>,
    /// When the live sessions started, by where they work.
    starts: Starts,
}

/// The earliest start time of the live sessions of a tool, overall and by
/// the directories they work in (as [`path_key`]s), to answer "could a live
/// session own this process" (see the module documentation) in a few
/// lookups per path component.
#[derive(Default)]
struct Starts {
    /// Paths compare ignoring ASCII case (macOS and Windows snapshots).
    fold_case: bool,
    /// Any live session.
    any: HashMap<Tool, u64>,
    /// A live session that works in no known directory.
    nowhere: HashMap<Tool, u64>,
    /// A live session that works in exactly this directory.
    at: HashMap<(Tool, String), u64>,
    /// A live session that works in this directory or below it.
    within: HashMap<(Tool, String), u64>,
}

impl Starts {
    /// Records a live session of `tool` started at `start` that works in
    /// `dirs` ([`path_key`]s).
    fn add(&mut self, tool: Tool, start: u64, dirs: &HashSet<String>) {
        fn earliest<K: std::hash::Hash + Eq>(map: &mut HashMap<K, u64>, key: K, start: u64) {
            map.entry(key)
                .and_modify(|t| *t = (*t).min(start))
                .or_insert(start);
        }
        earliest(&mut self.any, tool, start);
        if dirs.is_empty() {
            earliest(&mut self.nowhere, tool, start);
        }
        for dir in dirs {
            earliest(&mut self.at, (tool, dir.clone()), start);
            earliest(&mut self.within, (tool, dir.clone()), start);
            for above in parents(dir) {
                earliest(&mut self.within, (tool, above.to_string()), start);
            }
        }
    }

    /// Whether a live session of `tool` could own a process started at
    /// `start` (`u64::MAX` when unknown) that was given the project directory
    /// `project_dir`.
    fn could_own(&self, tool: Tool, project_dir: Option<&str>, start: u64) -> bool {
        let by = |t: Option<&u64>| t.is_some_and(|&t| t <= start);
        let Some(project) = project_dir else {
            return by(self.any.get(&tool));
        };
        let project = path_key(project, self.fold_case);
        // The session works in the project, below it, or above it.
        by(self.nowhere.get(&tool))
            || by(self.within.get(&(tool, project.clone())))
            || parents(&project).any(|above| by(self.at.get(&(tool, above.to_string()))))
    }
}

impl<'a> Live<'a> {
    /// One pass over the sessions and one over the processes.
    fn build(
        snap: &'a Snapshot,
        sessions: &[SessionInfo],
        roles: &HashMap<ProcKey, Role>,
    ) -> Live<'a> {
        let fold_case = snap.platform != Platform::Linux;
        let mut index = HashMap::with_capacity(sessions.len());
        // The directories each session works in.
        let mut dirs: Vec<HashSet<String>> = Vec::with_capacity(sessions.len());
        for (i, s) in sessions.iter().enumerate() {
            index.insert(s.root, i);
            dirs.push(
                non_empty(s.cwd.as_deref())
                    .map(|cwd| path_key(cwd, fold_case))
                    .into_iter()
                    .collect(),
            );
        }
        // Each id names the index of a session. When several sessions show
        // it, the lowest rank wins: a session of the id's own tool, then the
        // one found first.
        let mut ids: HashMap<(Tool, &str), usize> = HashMap::new();
        for p in &snap.procs {
            let Some(Role::Mcp { session, .. } | Role::OtherChild { session }) = roles.get(&p.key)
            else {
                continue;
            };
            let Some(&i) = index.get(session) else {
                continue;
            };
            for m in SESSION_MARKERS {
                let rank = |i: usize| (sessions[i].tool != m.tool, i);
                if let Some(id) = env(p, m.id) {
                    ids.entry((m.tool, id))
                        .and_modify(|named| {
                            if rank(i) < rank(*named) {
                                *named = i;
                            }
                        })
                        .or_insert(i);
                }
                if let Some(dir) = env(p, m.project_dir) {
                    dirs[i].insert(path_key(dir, fold_case));
                }
            }
        }
        let mut starts = Starts {
            fold_case,
            ..Starts::default()
        };
        for (s, dirs) in sessions.iter().zip(&dirs) {
            starts.add(s.tool, s.root.start_time, dirs);
        }
        Live {
            ids: ids
                .into_iter()
                .map(|(id, i)| (id, sessions[i].root))
                .collect(),
            starts,
        }
    }

    /// The live session that the id `p` carries names, if any.
    fn owner(&self, p: &RawProc) -> Option<ProcKey> {
        let (m, id) = session_id(p)?;
        self.ids.get(&(m.tool, id)).copied()
    }

    /// The strongest evidence `p` carries, judged.
    fn judge(&self, p: &RawProc, l: &Lineage, home: Option<&str>) -> Option<Verdict> {
        // Strong or medium evidence is sure only when nothing alive could
        // still own `p` and `p` is no app the user runs.
        let certainty = |could_be_owned: bool| {
            if could_be_owned || !l.is_adopted(p.key) || is_gui_app(p) {
                Certainty::Suspected
            } else {
                Certainty::Confirmed
            }
        };
        if let Some((m, id)) = session_id(p) {
            if let Some(&session) = self.ids.get(&(m.tool, id)) {
                return Some(Verdict::Detached(session));
            }
            let project_dir = env(p, m.project_dir);
            let start = match p.key.start_time {
                0 => u64::MAX,
                start => start,
            };
            let owned = self.starts.could_own(m.tool, project_dir, start);
            let reason = OrphanReason::SessionEnded {
                tool: m.tool,
                project_dir: project_dir.map(str::to_string),
            };
            return Some(Verdict::Orphan(certainty(owned), reason));
        }
        if let Some(pid) = recorded_parent(&p.argv)
            && owner_gone(l, p, pid)
        {
            let reason = OrphanReason::RecordedParentGone { pid };
            return Some(Verdict::Orphan(certainty(false), reason));
        }
        let tool = path_evidence(p, home)?;
        let tool_running = self.starts.any.contains_key(&tool);
        (l.is_adopted(p.key) && !tool_running).then_some(Verdict::Orphan(
            Certainty::Suspected,
            OrphanReason::PathOnly { tool },
        ))
    }
}

/// Every process that is, or is an ancestor of, the root of a live session,
/// cctree itself, or a process without a role whose id names a live session.
/// Ancestors follow both the validated parent and the recorded `ppid` (which
/// the lineage drops when the parent started later or sits on a cycle), to
/// stay on the safe side. Each process is added once and never walked from
/// again, so this is linear and ends on any cycle.
fn in_use(
    snap: &Snapshot,
    l: &Lineage,
    sessions: &[SessionInfo],
    live: &Live,
    roles: &HashMap<ProcKey, Role>,
) -> HashSet<ProcKey> {
    let by_key: HashMap<ProcKey, &RawProc> = snap.procs.iter().map(|p| (p.key, p)).collect();
    let recorded_parent = |k: ProcKey| {
        let ppid = by_key.get(&k)?.ppid?;
        l.key_of(ppid)
    };
    let owned = snap
        .procs
        .iter()
        .filter(|p| !roles.contains_key(&p.key) && live.owner(p).is_some())
        .map(|p| p.key);
    let mut stack: Vec<ProcKey> = sessions
        .iter()
        .map(|s| s.root)
        .chain([snap.self_key])
        .chain(owned)
        .collect();
    let mut marked = HashSet::new();
    while let Some(k) = stack.pop() {
        if marked.insert(k) {
            stack.extend([l.parent(k), recorded_parent(k)].into_iter().flatten());
        }
    }
    marked
}

/// [`Role::OtherNode`] for a Node.js-like `p` (see the module documentation),
/// grouped by the app `top` (its farthest ancestor that is an app) or, without
/// one, by its own app name.
fn other_node(p: &RawProc, top: Option<&RawProc>, home: Option<&str>) -> Option<Role> {
    let named = |path: &str| {
        let name = base_name(path);
        NODE_PROGRAMS.iter().any(|n| name.eq_ignore_ascii_case(n))
    };
    let node_like =
        named(&p.name) || p.argv.first().is_some_and(|a| named(a)) || looks_like_mcp(&p.argv);
    node_like.then(|| Role::OtherNode {
        group: app_name(top.unwrap_or(p), home),
    })
}

/// The first session marker `p` carries, with the id.
fn session_id(p: &RawProc) -> Option<(&'static SessionMarker, &str)> {
    SESSION_MARKERS
        .iter()
        .find_map(|m| Some((m, env(p, m.id)?)))
}

/// The PID of the first `--parent-pid=N` / `--parent-pid N` after `argv[0]`.
/// `None` if there is none, or its value is not a PID (0 is not).
fn recorded_parent(argv: &[String]) -> Option<Pid> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let Some(rest) = arg.strip_prefix(PARENT_PID_FLAG) else {
            continue;
        };
        let value = if rest.is_empty() {
            args.next()?.as_str()
        } else if let Some(value) = rest.strip_prefix('=') {
            value
        } else {
            continue;
        };
        return value.parse().ok().filter(|&pid: &Pid| pid != 0);
    }
    None
}

/// Whether the owner `p` recorded as `pid` is gone: no process has that PID,
/// or the one that has it started after `p` (the PID was reused). When `p`'s
/// own start time is unknown (0), only a missing PID counts.
fn owner_gone(l: &Lineage, p: &RawProc, pid: Pid) -> bool {
    match l.key_of(pid) {
        None => true,
        Some(owner) => p.key.start_time != 0 && owner.start_time > p.key.start_time,
    }
}

/// `path` in the form paths are compared in: `/` separators and no trailing
/// one, ASCII-lowercased when `fold_case` (macOS and Windows snapshots) or
/// when it is a Windows-style path.
fn path_key(path: &str, fold_case: bool) -> String {
    let key = path.replace('\\', "/");
    let key = key.trim_end_matches('/');
    if fold_case || is_windows_path(path) {
        key.to_ascii_lowercase()
    } else {
        key.to_string()
    }
}

/// A path with a `\` or a drive letter.
fn is_windows_path(path: &str) -> bool {
    path.contains('\\')
        || matches!(path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
}

/// The directories above the [`path_key`] `key`, each ending at a path
/// boundary: `/a/b/c` gives the root (an empty key), `/a` and `/a/b`.
fn parents(key: &str) -> impl Iterator<Item = &str> {
    key.match_indices('/').map(move |(i, _)| &key[..i])
}

/// Install directories of Windows apps, matched case-insensitively with `\`
/// separators.
const WINDOWS_APP_DIRS: &[&str] = &[
    r"\program files\",
    r"\program files (x86)\",
    r"\windowsapps\",
];

/// Whether `p` runs an app the user starts and uses (OrbStack, an IDE) rather
/// than a command-line tool: its exe (or `argv[0]` without one) sits in a
/// `.app` bundle, under `/Applications/`, or, for a Windows-style path, in
/// `Program Files`, `Program Files (x86)` or `WindowsApps`. Case is ignored.
fn is_gui_app(p: &RawProc) -> bool {
    let Some(path) = p
        .exe
        .as_deref()
        .filter(|e| !e.is_empty())
        .or_else(|| p.argv.first().map(String::as_str))
    else {
        return false;
    };
    // A directory (never the file itself) named `<something>.app`.
    let in_bundle = path.split(['/', '\\']).rev().skip(1).any(|dir| {
        let dir = dir.as_bytes();
        dir.len() > 4 && dir[dir.len() - 4..].eq_ignore_ascii_case(b".app")
    });
    let applications = path
        .as_bytes()
        .get(..14)
        .is_some_and(|start| start.eq_ignore_ascii_case(b"/Applications/"));
    let windows_app = is_windows_path(path) && {
        let path = path.replace('/', "\\").to_ascii_lowercase();
        WINDOWS_APP_DIRS.iter().any(|dir| path.contains(dir))
    };
    in_bundle || applications || windows_app
}

/// The value of the environment variable `name` of `p`, unless it is unset or
/// empty.
fn env<'p>(p: &'p RawProc, name: &str) -> Option<&'p str> {
    non_empty(p.env.get(name).map(String::as_str))
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify;
    use crate::config::McpConfigs;
    use crate::model::Platform;
    use crate::testkit::{base, cfgs, p, snap};

    const SID: &str = "CLAUDE_CODE_SESSION_ID";
    const PROJ: &str = "CLAUDE_PROJECT_DIR";

    // ---- The brief's tests ----

    #[test]
    fn strong_evidence_live_session_is_detached() {
        let mut v = base();
        v.push(p(400, 1, 70, &["node", "dev-server.js"]).env("CLAUDE_CODE_SESSION_ID", "S1"));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert!(
            matches!(c.role(s.index()[&400].key), Role::Detached { session } if *session == s.index()[&200].key)
        );
    }
    #[test]
    fn strong_evidence_dead_session_is_confirmed_orphan() {
        let mut v = base();
        v.push(
            p(401, 1, 70, &["node", "x"])
                .env("CLAUDE_CODE_SESSION_ID", "GONE")
                .env("CLAUDE_PROJECT_DIR", "/Users/u/prj/old"),
        );
        v.push(p(402, 401, 71, &["node", "child"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert!(
            matches!(c.role(k(401)), Role::Orphan { certainty: Certainty::Confirmed,
        reason: OrphanReason::SessionEnded { tool: Tool::ClaudeCode, project_dir: Some(d) } } if d == "/Users/u/prj/old")
        );
        assert!(matches!(c.role(k(402)), Role::OrphanMember { root } if *root == k(401)));
    }
    #[test]
    fn unmapped_sid_with_live_same_project_is_suspected() {
        let mut v = base();
        v.push(
            p(403, 1, 70, &["node", "x"])
                .env("CLAUDE_CODE_SESSION_ID", "OTHER")
                .env("CLAUDE_PROJECT_DIR", "/Users/u/prj/a"),
        );
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert!(matches!(
            c.role(s.index()[&403].key),
            Role::Orphan {
                certainty: Certainty::Suspected,
                ..
            }
        ));
    }
    #[test]
    fn recorded_parent_pid_rules() {
        let mut v = base();
        v.push(p(
            404,
            1,
            70,
            &["node", "/x/watchdog/main.js", "--parent-pid=777"],
        ));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert!(matches!(
            c.role(s.index()[&404].key),
            Role::Orphan {
                certainty: Certainty::Confirmed,
                reason: OrphanReason::RecordedParentGone { pid: 777 }
            }
        ));
    }
    #[test]
    fn path_only_needs_adoption_and_no_live_session() {
        let meet = "/private/var/folders/x/T/codex-home-Z/r/ChatGPT Meetings";
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(80871, 1, 5, &[meet]),
                p(
                    500,
                    1,
                    5,
                    &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"],
                ),
                p(501, 500, 6, &["/Users/u/.codex/plugins/cache/h/host"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(
            c.role(k(80871)),
            Role::Orphan {
                certainty: Certainty::Suspected,
                reason: OrphanReason::PathOnly { tool: Tool::Codex }
            }
        ));
        assert!(matches!(c.role(k(501)), Role::Unrelated));
        let mut v = s.procs.clone();
        v.push(p(600, 1, 9, &["codex"]));
        let s2 = snap(Platform::MacOs, v);
        let c2 = classify(&s2, &McpConfigs::default());
        assert!(!matches!(
            c2.role(s2.index()[&80871].key),
            Role::Orphan { .. }
        ));
    }
    #[test]
    fn other_node_grouped_by_top_app() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(
                    1444,
                    1,
                    2,
                    &["/Users/u/.hermes/hermes-agent/venv/bin/python"],
                ),
                p(2724, 1444, 3, &["/Users/u/.hermes/node/bin/node", "w.js"]),
                p(1564, 1, 3, &["./node", "./hipkiLocalServer.js"]),
                p(50, 1, 3, &["/usr/bin/python3"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(c.role(k(2724)), Role::OtherNode { group } if group == "hermes"));
        assert!(matches!(c.role(k(1564)), Role::OtherNode { .. }));
        assert!(matches!(c.role(k(50)), Role::Unrelated));
    }

    // ---- Controller ruling: every id seen on a live session's members ----

    #[test]
    fn every_id_seen_on_a_member_maps_to_its_session() {
        let mut v = base();
        // A resumed session: the MCP server 221 carries S1, an other child S9.
        v.push(p(231, 230, 31, &["node", "x.js"]).env(SID, "S9"));
        v.push(p(410, 1, 80, &["node", "dev.js"]).env(SID, "S9"));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(c.role(k(221)), Role::Mcp { session, .. } if *session == k(200)));
        assert_eq!(c.role(k(231)), &Role::OtherChild { session: k(200) });
        // The session keeps one id; the lookup knows both.
        assert_eq!(c.sessions[0].claude_session_id.as_deref(), Some("S1"));
        assert_eq!(c.role(k(410)), &Role::Detached { session: k(200) });
    }

    #[test]
    fn ids_inside_an_mcp_node_count_too() {
        let mut v = base();
        // A `claude -p` run by an MCP server is a member of that MCP node; its
        // own children carry the new session id.
        v.push(p(213, 211, 24, &["claude", "-p", "review"]).env(SID, "S1"));
        v.push(p(214, 213, 25, &["node", "/x/hook.js"]).env(SID, "S7"));
        v.push(p(411, 1, 80, &["node", "srv.js"]).env(SID, "S7"));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(c.role(k(214)), Role::Mcp { session, .. } if *session == k(200)));
        assert_eq!(c.role(k(411)), &Role::Detached { session: k(200) });
    }

    // ---- Beyond the brief ----

    #[test]
    fn descendants_follow_their_root_whatever_they_carry() {
        let mut v = base();
        // Detached root, an env-less child, a grandchild with a dead id.
        v.push(p(420, 1, 80, &["node", "dev.js"]).env(SID, "S1"));
        v.push(p(421, 420, 81, &["/opt/esbuild", "--service"]));
        v.push(p(422, 421, 82, &["node", "w.js"]).env(SID, "GONE"));
        // Orphan root (in a project no live session works in), a child with
        // the id of no session, a recorded parent that is gone.
        v.push(
            p(430, 1, 80, &["node", "old.js"])
                .env(SID, "GONE")
                .env(PROJ, "/Users/u/prj/old"),
        );
        v.push(p(431, 430, 81, &["node", "a.js"]).env(SID, "OTHER"));
        v.push(p(432, 431, 82, &["node", "b.js", "--parent-pid=999"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        for pid in [420, 421, 422] {
            assert_eq!(c.role(k(pid)), &Role::Detached { session: k(200) }, "{pid}");
        }
        assert!(matches!(
            c.role(k(430)),
            Role::Orphan {
                certainty: Certainty::Confirmed,
                reason: OrphanReason::SessionEnded {
                    tool: Tool::ClaudeCode,
                    project_dir: Some(_)
                }
            }
        ));
        for pid in [431, 432] {
            assert_eq!(
                c.role(k(pid)),
                &Role::OrphanMember { root: k(430) },
                "{pid}"
            );
        }
        // The session pass's roles are untouched.
        let before = classify(&snap(Platform::MacOs, base()), &cfgs());
        for (key, role) in &before.roles {
            assert_eq!(c.role(*key), role);
        }
    }

    #[test]
    fn strong_evidence_wins_over_a_recorded_parent() {
        let mut v = base();
        v.push(
            p(
                440,
                1,
                80,
                &["node", "/x/watchdog/main.js", "--parent-pid=777"],
            )
            .env(SID, "S1"),
        );
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(440)), &Role::Detached { session: k(200) });
    }

    #[test]
    fn empty_values_are_no_evidence() {
        let mut v = base();
        v.push(p(445, 1, 80, &["node", "x.js"]).env(SID, ""));
        // An empty project dir is none: any earlier live session (200) could
        // own the process, whatever its cwd.
        v.push(
            p(446, 1, 80, &["node", "x.js"])
                .env(SID, "GONE")
                .env(PROJ, "")
                .cwd("/Users/u/prj/old"),
        );
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(
            c.role(k(445)),
            &Role::OtherNode {
                group: "node".into()
            }
        );
        assert!(matches!(
            c.role(k(446)),
            Role::Orphan {
                certainty: Certainty::Suspected,
                reason: OrphanReason::SessionEnded {
                    project_dir: None,
                    ..
                }
            }
        ));
    }

    #[test]
    fn suspected_when_a_live_session_works_in_that_project() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(100, 1, 10, &["/bin/zsh"]),
                // A session in a sibling worktree: its members get the main
                // checkout, which is not below its cwd.
                p(200, 100, 20, &["claude"]).cwd("/Users/u/prj/main-wt"),
                p(230, 200, 30, &["node", "x.js"]).env(PROJ, "/Users/u/prj/main"),
                // A live Codex session in another project.
                p(600, 1, 9, &["codex"]).cwd("/Users/u/prj/other"),
                p(450, 1, 80, &["node", "a.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/main/"),
                p(451, 1, 80, &["node", "b.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/main-wt"),
                p(452, 1, 80, &["node", "c.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/other"),
                p(453, 1, 80, &["node", "d.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/mai"),
                p(454, 1, 80, &["node", "e.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/U/prj/main"),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        let certainty = |pid| match c.role(k(pid)) {
            Role::Orphan {
                certainty,
                reason: OrphanReason::SessionEnded { .. },
            } => Some(*certainty),
            _ => None,
        };
        // The members' project (trailing separator ignored), the root's cwd;
        // a macOS snapshot ignores case.
        for pid in [450, 451, 454] {
            assert_eq!(certainty(pid), Some(Certainty::Suspected), "{pid}");
        }
        // Only a session of the same tool counts; a prefix must end at a
        // path boundary.
        for pid in [452, 453] {
            assert_eq!(certainty(pid), Some(Certainty::Confirmed), "{pid}");
        }
        assert!(matches!(
            c.role(k(450)),
            Role::Orphan { reason: OrphanReason::SessionEnded { project_dir: Some(d), .. }, .. }
                if d == "/Users/u/prj/main/"
        ));
    }

    // ---- Controller ruling 2: who could own an unmapped id ----

    /// The certainty of the orphan `orphan` (pid 700, with a child 701) in a
    /// macOS snapshot with home `home` and the given live session roots
    /// (children of a login shell started at 10).
    fn certainty_of(home: &str, sessions: &[RawProc], orphan: RawProc) -> Option<Certainty> {
        certainty_on(Platform::MacOs, home, sessions, orphan)
    }

    /// [`certainty_of`] on `platform`.
    fn certainty_on(
        platform: Platform,
        home: &str,
        sessions: &[RawProc],
        orphan: RawProc,
    ) -> Option<Certainty> {
        let mut v = vec![p(1, 0, 1, &["/sbin/launchd"]), p(100, 1, 10, &["/bin/zsh"])];
        v.extend(sessions.iter().cloned());
        v.push(orphan);
        v.push(p(701, 700, 81, &["node", "child.js"]));
        let mut s = snap(platform, v);
        s.home = Some(home.into());
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(701)), &Role::OrphanMember { root: k(700) });
        match c.role(k(700)) {
            Role::Orphan {
                certainty,
                reason: OrphanReason::SessionEnded { .. },
            } => Some(*certainty),
            _ => None,
        }
    }

    /// A live Claude Code session root in `cwd`, started at `start`.
    fn claude_at(cwd: &str, start: u64) -> RawProc {
        p(200, 100, start, &["claude"]).cwd(cwd)
    }

    /// An orphan tree root with an id no live session carries, in `cwd`,
    /// started at 80, without `CLAUDE_PROJECT_DIR`.
    fn gone_in(cwd: &str) -> RawProc {
        p(700, 1, 80, &["node", "dev.js"]).env(SID, "GONE").cwd(cwd)
    }

    const HOME: &str = "/Users/u";
    const SUSPECTED: Option<Certainty> = Some(Certainty::Suspected);
    const CONFIRMED: Option<Certainty> = Some(Certainty::Confirmed);

    /// [`gone_in`] `/tmp`, given the project dir `dir`.
    fn gone_for(dir: &str) -> RawProc {
        gone_in("/tmp").env(PROJ, dir)
    }

    #[test]
    fn unmapped_id_below_an_earlier_sessions_cwd_is_suspected() {
        let earlier = [claude_at("/Users/u/prj/a", 20)];
        assert_eq!(
            certainty_of(HOME, &earlier, gone_in("/Users/u/prj/a/sub")),
            SUSPECTED
        );
        assert_eq!(
            certainty_of(HOME, &earlier, gone_for("/Users/u/prj/a/sub")),
            SUSPECTED
        );
    }

    #[test]
    fn unmapped_id_in_another_project_is_confirmed() {
        let other = [claude_at("/Users/u/prj/other", 20)];
        assert_eq!(
            certainty_of(HOME, &other, gone_for("/Users/u/prj/a/sub")),
            CONFIRMED
        );
    }

    #[test]
    fn without_a_project_dir_the_cwd_does_not_matter() {
        // Bash-tool children never carry CLAUDE_PROJECT_DIR, and the Bash
        // tool can `cd` anywhere.
        let web = [claude_at("/Users/u/prj/web", 20)];
        assert_eq!(
            certainty_of(HOME, &web, gone_in("/Users/u/prj/api")),
            SUSPECTED
        );
        assert_eq!(
            certainty_of(HOME, &web, gone_for("/Users/u/prj/api")),
            CONFIRMED
        );
    }

    #[test]
    fn a_session_started_after_the_orphan_cannot_own_it() {
        let later_other = [claude_at("/Users/u/prj/other", 90)];
        assert_eq!(
            certainty_of(HOME, &later_other, gone_in("/Users/u/prj/a/sub")),
            CONFIRMED
        );
        let later_same = [claude_at("/Users/u/prj/a", 90)];
        assert_eq!(
            certainty_of(HOME, &later_same, gone_in("/Users/u/prj/a/sub")),
            CONFIRMED
        );
        // Started in the same second: it could.
        let same_second = [claude_at("/Users/u/prj/a", 80)];
        assert_eq!(
            certainty_of(HOME, &same_second, gone_in("/Users/u/prj/a/sub")),
            SUSPECTED
        );
    }

    #[test]
    fn without_a_project_any_earlier_session_could_own_it() {
        // A desktop app (OrbStack) runs in `/`.
        let app = || gone_in("/");
        assert_eq!(
            certainty_of(HOME, &[claude_at("/Users/u/prj/other", 20)], app()),
            SUSPECTED
        );
        assert_eq!(certainty_of(HOME, &[], app()), CONFIRMED);
        assert_eq!(
            certainty_of(HOME, &[claude_at("/Users/u/prj/other", 90)], app()),
            CONFIRMED
        );
        // Only a session of the same tool counts.
        let codex = [p(200, 100, 20, &["codex"]).cwd("/Users/u/prj/other")];
        assert_eq!(certainty_of(HOME, &codex, app()), CONFIRMED);
    }

    #[test]
    fn only_the_project_dir_is_matched() {
        let earlier = [claude_at("/Users/u/prj/a", 20)];
        assert_eq!(
            certainty_of(HOME, &earlier, gone_for("/Users/u/prj/a")),
            SUSPECTED
        );
        let elsewhere = gone_in("/Users/u/prj/a/sub").env(PROJ, "/Users/u/prj/old");
        assert_eq!(certainty_of(HOME, &earlier, elsewhere), CONFIRMED);
    }

    #[test]
    fn how_project_dirs_relate() {
        // A session below the orphan's project is related, too.
        let worktree = [claude_at("/Users/u/prj/a/.claude/worktrees/wt", 20)];
        assert_eq!(
            certainty_of(HOME, &worktree, gone_for("/Users/u/prj/a")),
            SUSPECTED
        );
        // Not at a path boundary.
        assert_eq!(
            certainty_of(
                HOME,
                &[claude_at("/Users/u/prj/ab", 20)],
                gone_for("/Users/u/prj/a/x")
            ),
            CONFIRMED
        );
        // Anonymized paths.
        let tilde = [claude_at("~/prj/a", 20)];
        assert_eq!(certainty_of("~", &tilde, gone_for("~/prj/a/x")), SUSPECTED);
        assert_eq!(certainty_of("~", &tilde, gone_for("~/prj/b")), CONFIRMED);
        // macOS and Windows snapshots ignore case; Linux keeps it, except in
        // Windows-style paths.
        let mixed = [claude_at("/Users/U/Prj/A", 20)];
        assert_eq!(
            certainty_on(Platform::MacOs, HOME, &mixed, gone_for("/users/u/prj/a")),
            SUSPECTED
        );
        assert_eq!(
            certainty_on(Platform::Windows, HOME, &mixed, gone_for("/users/u/prj/a")),
            SUSPECTED
        );
        assert_eq!(
            certainty_on(Platform::Linux, HOME, &mixed, gone_for("/users/u/prj/a")),
            CONFIRMED
        );
        let win = [claude_at(r"C:\Users\u\prj\a", 20)];
        for platform in [Platform::Linux, Platform::Windows] {
            assert_eq!(
                certainty_on(platform, HOME, &win, gone_for(r"c:\users\U\PRJ\A\sub\")),
                SUSPECTED
            );
            assert_eq!(
                certainty_on(platform, HOME, &win, gone_for("C:/Users/u/prj/b")),
                CONFIRMED
            );
        }
    }

    #[test]
    fn unknown_start_times_could_own_or_be_owned() {
        let earlier = [claude_at("/Users/u/prj/a", 20)];
        // The orphan's own start time is unknown: a later session may have
        // started it.
        let unknown = p(700, 1, 0, &["node", "dev.js"])
            .env(SID, "GONE")
            .env(PROJ, "/Users/u/prj/a/sub");
        let later = [claude_at("/Users/u/prj/a", 90)];
        assert_eq!(certainty_of(HOME, &later, unknown), SUSPECTED);
        // A session whose cwd is unknown could own any later process.
        let mut nowhere = earlier[0].clone();
        nowhere.cwd = None;
        assert_eq!(
            certainty_of(HOME, &[nowhere], gone_for("/Users/u/prj/b")),
            SUSPECTED
        );
    }

    // ---- Fix round 1: more ways a confirmed orphan could be wrong ----

    #[test]
    fn evidence_under_a_live_parent_is_only_suspected() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                // A terminal multiplexer, and a Claude Code that bun runs
                // (not recognised as a session).
                p(800, 1, 50, &["/opt/homebrew/bin/tmux"]),
                p(810, 1, 50, &["bun", "/n/@anthropic-ai/claude-code/cli.js"]),
                p(801, 800, 80, &["node", "dev.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/old"),
                p(802, 801, 81, &["node", "w.js"]),
                p(811, 810, 80, &["node", "x.js"]).env(SID, "GONE"),
                p(
                    820,
                    800,
                    80,
                    &["node", "/x/watchdog/main.js", "--parent-pid=777"],
                ),
                // The same, adopted.
                p(830, 1, 80, &["node", "dev.js"])
                    .env(SID, "GONE")
                    .env(PROJ, "/Users/u/prj/old"),
                p(
                    831,
                    1,
                    80,
                    &["node", "/x/watchdog/main.js", "--parent-pid=777"],
                ),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        let orphan = |pid| match c.role(k(pid)) {
            Role::Orphan { certainty, reason } => Some((*certainty, reason.clone())),
            _ => None,
        };
        let ended = |project_dir: Option<&str>| OrphanReason::SessionEnded {
            tool: Tool::ClaudeCode,
            project_dir: project_dir.map(str::to_string),
        };
        let gone = OrphanReason::RecordedParentGone { pid: 777 };
        let old = Some("/Users/u/prj/old");
        assert_eq!(orphan(801), Some((Certainty::Suspected, ended(old))));
        assert_eq!(c.role(k(802)), &Role::OrphanMember { root: k(801) });
        assert_eq!(orphan(811), Some((Certainty::Suspected, ended(None))));
        assert_eq!(orphan(820), Some((Certainty::Suspected, gone.clone())));
        assert_eq!(orphan(830), Some((Certainty::Confirmed, ended(old))));
        assert_eq!(orphan(831), Some((Certainty::Confirmed, gone)));
    }

    #[test]
    fn an_app_is_never_a_confirmed_orphan() {
        let app = "/Applications/OrbStack.app/Contents/MacOS/OrbStack";
        let orb = p(700, 1, 80, &[app]).env(SID, "GONE").cwd("/");
        assert_eq!(certainty_of(HOME, &[], orb), SUSPECTED);
        let node = p(700, 1, 80, &["/usr/local/bin/node", "x.js"])
            .env(SID, "GONE")
            .cwd("/");
        assert_eq!(certainty_of(HOME, &[], node), CONFIRMED);
        // A recorded parent that is gone does not make it sure either.
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(702, 1, 80, &[app, "--parent-pid=777"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        assert!(matches!(
            c.role(s.index()[&702].key),
            Role::Orphan {
                certainty: Certainty::Suspected,
                reason: OrphanReason::RecordedParentGone { pid: 777 }
            }
        ));
    }

    #[test]
    fn app_paths() {
        let app = |exe: &str| is_gui_app(&p(1, 0, 1, &[exe]));
        for exe in [
            "/Applications/OrbStack.app/Contents/MacOS/OrbStack",
            "/applications/Utilities/tool",
            "/Users/u/Applications/X.app/Contents/MacOS/x",
            "/opt/x/Foo.APP/Contents/MacOS/foo",
            r"C:\Program Files\Foo\foo.exe",
            "c:/program files (x86)/x/y.exe",
            r"C:\Program Files\WindowsApps\Claude_1.0\app\claude.exe",
            r"D:\PROGRAM FILES\x.exe",
        ] {
            assert!(app(exe), "{exe}");
        }
        for exe in [
            "/usr/local/bin/node",
            "/opt/Program Files/x",
            "/x/run.app",
            "/x/.app/y",
            "/Users/u/apps.application/x",
            "/Applications",
            r"C:\Users\u\AppData\Local\x.exe",
            "",
        ] {
            assert!(!app(exe), "{exe}");
        }
        // The exe wins; without one, argv[0] counts.
        let by_exe =
            p(1, 0, 1, &["OrbStack"]).exe("/Applications/OrbStack.app/Contents/MacOS/OrbStack");
        assert!(is_gui_app(&by_exe));
        let mut by_argv = p(1, 0, 1, &["/Applications/X.app/Contents/MacOS/X"]);
        by_argv.exe = None;
        assert!(is_gui_app(&by_argv));
    }

    #[test]
    fn in_use_also_follows_recorded_parents_the_lineage_dropped() {
        let s = snap(
            Platform::Windows,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                // The session's recorded parent started after it, so the
                // lineage drops the link.
                p(900, 1, 100, &["tmux.exe"]).env(SID, "GONE"),
                p(901, 900, 50, &["claude.exe"]),
                // A recorded-parent cycle through a session.
                p(910, 911, 5, &["claude.exe"]),
                p(911, 910, 5, &["node.exe", "x.js"]).env(SID, "GONE"),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(901)), &Role::SessionRoot);
        assert_eq!(c.role(k(910)), &Role::SessionRoot);
        for pid in [900, 911] {
            assert!(
                !matches!(
                    c.role(k(pid)),
                    Role::Orphan { .. } | Role::OrphanMember { .. }
                ),
                "{pid}"
            );
        }
    }

    #[test]
    fn an_id_seen_in_several_sessions_prefers_a_claude_code_session() {
        // A Codex session started (and left running) by Claude Code session
        // 200's Bash tool: its members inherited 200's id S1. It is found
        // before session 200.
        let mut v = vec![
            p(1, 0, 1, &["/sbin/launchd"]),
            p(470, 1, 15, &["codex"]).env(SID, "S1"),
            p(471, 470, 16, &["bash", "-c", "x"]).env(SID, "S1"),
            p(472, 470, 16, &["bash", "-c", "y"]).env(SID, "S5"),
        ];
        v.extend(base().into_iter().skip(1));
        v.push(p(473, 1, 80, &["node", "a.js"]).env(SID, "S1"));
        v.push(p(474, 1, 80, &["node", "b.js"]).env(SID, "S5"));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.sessions[0].root, k(470));
        assert_eq!(c.role(k(473)), &Role::Detached { session: k(200) });
        // An id only a Codex session's members carry still points there.
        assert_eq!(c.role(k(474)), &Role::Detached { session: k(470) });
    }

    #[test]
    fn recorded_parent_pid_forms() {
        let mut v = base();
        // `--parent-pid N`, gone.
        v.push(p(500, 1, 70, &["node", "w.js", "--parent-pid", "777"]));
        // Alive and older: no evidence.
        v.push(p(501, 1, 70, &["node", "w.js", "--parent-pid=100"]));
        // The PID now belongs to a newer process: gone.
        v.push(p(502, 1, 70, &["node", "w.js", "--parent-pid=503"]));
        v.push(p(503, 1, 75, &["/bin/sleep", "9"]));
        // Started in the same second as its owner: alive.
        v.push(p(504, 1, 75, &["node", "w.js", "--parent-pid=503"]));
        // Malformed, or not the flag.
        v.push(p(505, 1, 70, &["node", "w.js", "--parent-pid="]));
        v.push(p(506, 1, 70, &["node", "w.js", "--parent-pid=abc"]));
        v.push(p(507, 1, 70, &["node", "w.js", "--parent-pid=-1"]));
        v.push(p(508, 1, 70, &["node", "w.js", "--parent-pid=0"]));
        v.push(p(509, 1, 70, &["node", "w.js", "--parent-pid"]));
        v.push(p(510, 1, 70, &["node", "w.js", "--parent-pidx=777"]));
        v.push(p(511, 1, 70, &["--parent-pid=777"]).name("node"));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        let gone = |pid| match c.role(k(pid)) {
            Role::Orphan {
                certainty: Certainty::Confirmed,
                reason: OrphanReason::RecordedParentGone { pid },
            } => Some(*pid),
            _ => None,
        };
        assert_eq!(gone(500), Some(777));
        assert_eq!(gone(502), Some(503));
        for pid in [501, 504, 505, 506, 507, 508, 509, 510, 511] {
            assert_eq!(gone(pid), None, "{pid}");
            assert!(matches!(c.role(k(pid)), Role::OtherNode { .. }), "{pid}");
        }
    }

    #[test]
    fn recorded_parent_with_unknown_own_start_time_needs_a_missing_pid() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(10, 1, 50, &["/usr/bin/owner"]),
                p(11, 1, 0, &["node", "w.js", "--parent-pid=10"]),
                p(12, 1, 0, &["node", "w.js", "--parent-pid=13"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(c.role(k(11)), Role::OtherNode { .. }));
        assert!(matches!(
            c.role(k(12)),
            Role::Orphan {
                reason: OrphanReason::RecordedParentGone { pid: 13 },
                ..
            }
        ));
    }

    #[test]
    fn path_only_evidence_for_claude_code() {
        let by_exe = p(530, 1, 5, &["/Users/u/.claude/plugins/x/server"]);
        let by_cwd = p(531, 1, 5, &["/usr/bin/python3"]).cwd("/Users/u/.claude/x");
        let not_adopted = p(533, 532, 6, &["/Users/u/.claude/plugins/x/server"]);
        let procs = vec![
            p(1, 0, 1, &["/sbin/launchd"]),
            by_exe,
            by_cwd,
            p(532, 1, 5, &["/usr/bin/host"]),
            not_adopted,
        ];
        let s = snap(Platform::MacOs, procs.clone());
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        for pid in [530, 531] {
            assert!(
                matches!(
                    c.role(k(pid)),
                    Role::Orphan {
                        certainty: Certainty::Suspected,
                        reason: OrphanReason::PathOnly {
                            tool: Tool::ClaudeCode
                        }
                    }
                ),
                "{pid}"
            );
        }
        assert_eq!(c.role(k(533)), &Role::Unrelated);
        // A live Codex session does not matter; a live Claude Code one does.
        let mut with_codex = procs.clone();
        with_codex.push(p(600, 1, 9, &["codex"]));
        let s = snap(Platform::MacOs, with_codex);
        let c = classify(&s, &McpConfigs::default());
        assert!(matches!(c.role(k(530)), Role::Orphan { .. }));
        let mut with_claude = procs;
        with_claude.push(p(601, 1, 9, &["claude"]));
        let s = snap(Platform::MacOs, with_claude);
        let c = classify(&s, &McpConfigs::default());
        for pid in [530, 531] {
            assert_eq!(c.role(k(pid)), &Role::Unrelated, "{pid}");
        }
    }

    #[test]
    fn a_process_hosting_a_live_session_is_never_an_orphan() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                // A terminal multiplexer started by an ended session's Bash
                // tool; the user later started a session in one of its panes.
                p(450, 1, 50, &["/opt/homebrew/bin/tmux"]).env(SID, "GONE"),
                p(451, 450, 51, &["-zsh"]).name("zsh"),
                p(452, 451, 52, &["claude"]),
                // Another pane: a leftover dev server, an orphan on its own.
                p(453, 450, 53, &["node", "dev.js"]).env(SID, "GONE"),
                p(454, 453, 54, &["node", "w.js"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(450)), &Role::Unrelated);
        assert_eq!(c.role(k(451)), &Role::Unrelated);
        assert_eq!(c.role(k(452)), &Role::SessionRoot);
        // Its parent (tmux) is alive and no reaper: only suspected.
        assert!(matches!(
            c.role(k(453)),
            Role::Orphan {
                certainty: Certainty::Suspected,
                ..
            }
        ));
        assert_eq!(c.role(k(454)), &Role::OrphanMember { root: k(453) });
    }

    #[test]
    fn a_process_holding_cctree_or_a_live_sessions_process_is_never_an_orphan() {
        let mut v = base();
        v.push(p(460, 1, 50, &["/opt/homebrew/bin/tmux"]).env(SID, "GONE"));
        v.push(p(461, 460, 51, &["-zsh"]).name("zsh"));
        v.push(p(462, 461, 52, &["/usr/local/bin/cctree"]).env(SID, "GONE"));
        v.push(p(465, 1, 50, &["/opt/homebrew/bin/tmux"]).env(SID, "GONE"));
        v.push(p(466, 465, 51, &["node", "dev.js"]).env(SID, "S1"));
        let mut s = snap(Platform::MacOs, v);
        s.self_key = s.index()[&462].key;
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        for pid in [460, 461, 462, 465] {
            assert!(
                !matches!(
                    c.role(k(pid)),
                    Role::Orphan { .. } | Role::OrphanMember { .. }
                ),
                "{pid}"
            );
        }
        assert_eq!(c.role(k(466)), &Role::Detached { session: k(200) });
    }

    #[test]
    fn other_node_names_and_mcp_lookalikes() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(
                    10,
                    1,
                    2,
                    &["/Applications/Visual Studio Code.app/Contents/MacOS/Electron"],
                ),
                p(11, 10, 3, &["/bin/zsh"]),
                // A title-rewritten npm: the name is still node.
                p(12, 11, 4, &["npm", "run", "dev"]).name("node"),
                p(13, 12, 5, &["C:\\x\\NPX.CMD", "y"]),
                p(14, 1, 5, &["deno", "run", "x.ts"]),
                p(15, 1, 5, &["/Users/u/.bun/bin/bun", "x"]),
                p(16, 1, 5, &["uvx", "mcp-server-time"]),
                p(17, 1, 5, &["worker"]).name("Node.exe"),
                p(18, 1, 5, &["/usr/bin/nodejs"]),
                p(19, 1, 5, &["python3", "-m", "http.server"]),
                p(20, 1, 5, &["/usr/bin/grep", "mcp"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let l = Lineage::build(&s);
        let k = |pid| s.index()[&pid].key;
        let group = |pid| match c.role(k(pid)) {
            Role::OtherNode { group } => Some(group.as_str()),
            _ => None,
        };
        assert_eq!(group(12), Some("Visual Studio Code"));
        assert_eq!(group(13), Some("Visual Studio Code"));
        assert_eq!(group(14), Some("deno"));
        assert_eq!(group(15), Some("bun"));
        assert_eq!(group(16), Some("uvx"));
        assert_eq!(group(17), Some("worker"));
        for pid in [1, 10, 11, 18, 19, 20] {
            assert_eq!(c.role(k(pid)), &Role::Unrelated, "{pid}");
        }
        // The group is Lineage::top_app, or the process's own app name.
        for proc_ in &s.procs {
            if let Role::OtherNode { group } = c.role(proc_.key) {
                let want = l
                    .top_app(proc_.key)
                    .unwrap_or_else(|| app_name(proc_, s.home.as_deref()));
                assert_eq!(group, &want);
            }
        }
    }

    #[test]
    fn existing_roles_are_kept_and_stop_the_spread() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(1, 0, 1, &["/sbin/launchd"]),
                p(600, 1, 5, &["node", "a.js"]).env(SID, "GONE"),
                p(601, 600, 6, &["node", "b.js"]),
                p(602, 601, 7, &["node", "c.js"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| s.index()[&pid].key;
        let kept = Role::OtherNode {
            group: "kept".into(),
        };
        let mut roles = HashMap::from([(k(601), kept.clone())]);
        assign_rest(&s, &l, &[], &mut roles);
        assert!(matches!(roles.get(&k(600)), Some(Role::Orphan { .. })));
        assert_eq!(roles.get(&k(601)), Some(&kept));
        assert_eq!(
            roles.get(&k(602)),
            Some(&Role::OtherNode {
                group: "node".into()
            })
        );
        assert_eq!(roles.get(&k(1)), None);
    }

    #[test]
    fn hostile_snapshots_do_not_panic() {
        let mut no_argv = p(7, 1, 3, &["x"]).env(SID, "GONE");
        no_argv.argv.clear();
        no_argv.exe = None;
        no_argv.name.clear();
        let cases: Vec<Vec<RawProc>> = vec![
            vec![],
            // A ppid cycle carrying ids and recorded parents.
            vec![
                p(1, 2, 1, &["node", "--parent-pid=2"]).env(SID, "GONE"),
                p(2, 1, 1, &["node", "--parent-pid", "1"]).env(SID, "S1"),
                p(3, 2, 1, &["claude"]),
                p(4, 3, 2, &["node", "x"]).env(SID, "S1"),
            ],
            // Duplicate PIDs and keys, a self parent, empty argv.
            vec![
                p(5, 5, 1, &["node"]).env(SID, "GONE"),
                p(5, 5, 1, &["node"]).env(SID, "GONE"),
                p(5, 0, 2, &["npx", "y", "--parent-pid=5"]),
                p(6, 5, 2, &["claude"]),
                no_argv,
                p(8, 7, 9, &[""]).cwd("~/.codex").zombie(),
            ],
        ];
        for procs in cases {
            for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
                let mut s = snap(platform, procs.clone());
                s.home = None;
                let c = classify(&s, &cfgs());
                for proc_ in &s.procs {
                    let _ = (c.role(proc_.key), c.category_of(proc_.key));
                }
            }
        }
    }

    #[test]
    fn a_deep_chain_is_handled_in_one_pass() {
        let depth: u32 = 20_000;
        let mut v = vec![
            p(1, 0, 1, &["/sbin/launchd"]),
            p(2, 1, 2, &["node", "root.js"]).env(SID, "GONE"),
        ];
        v.extend((3..depth).map(|pid| p(pid, pid - 1, u64::from(pid), &["node", "x"])));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert!(matches!(c.role(k(2)), Role::Orphan { .. }));
        assert_eq!(c.role(k(depth - 1)), &Role::OrphanMember { root: k(2) });
        assert_eq!(c.tracked().len(), depth as usize - 2);
    }
}
