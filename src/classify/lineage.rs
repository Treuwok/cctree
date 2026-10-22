//! Validated parent/child relations between the processes of one snapshot.
//!
//! A raw `ppid` is only a claim: PIDs are reused, and on Windows the recorded
//! parent may be an unrelated, newer process (or the ppid chain may even loop).
//! [`Lineage`] keeps a parent link only when the parent is in the snapshot and
//! did not start after the child, then drops every link that sits on a cycle.
//! What is left is a forest, so every walk below terminates.
//!
//! A process whose parent link is missing, or points at PID 1 or a subreaper,
//! is "adopted": whoever started it is gone or no longer meaningful.
//!
//! Queries about a key that is not in the snapshot are answered as "nothing
//! known" (no parent, no children, ...) instead of panicking; only
//! [`Lineage::proc_`] has to panic, because it returns a reference.

use std::collections::HashMap;

use crate::model::{Pid, ProcKey, RawProc, Snapshot};

/// Process names (case-insensitive, without `.exe`) that are interactive shells
/// or shell front doors. A process started from one was started by the user.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "nu",
    "login",
    "cmd",
    "powershell",
    "pwsh",
];

/// Names (case-insensitive) of system processes that sit above every app, so
/// they never name one. PIDs 0 and 1 are excluded as well.
const SYSTEM_ROOTS: &[&str] = &["launchd", "systemd", "init", "explorer.exe", "services.exe"];

/// File extensions that mark an executable; only these are dropped from a
/// program name (`python3.12` and `com.apple.Foo` keep their dots).
const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "com", "bat", "cmd"];

/// The validated parent/child graph of one [`Snapshot`], built by
/// [`Lineage::build`]. It borrows the snapshot and is cheap to rebuild.
pub struct Lineage<'a> {
    snap: &'a Snapshot,
    /// Index into `snap.procs` by PID. If a PID is listed twice, the later
    /// entry wins, like [`Snapshot::index`].
    by_pid: HashMap<Pid, usize>,
    /// Validated parent of every process, as an index into `snap.procs`.
    parent: Vec<Option<usize>>,
    /// Children of process `i` are `child_keys[child_start[i]..child_start[i + 1]]`.
    child_start: Vec<usize>,
    child_keys: Vec<ProcKey>,
}

impl<'a> Lineage<'a> {
    /// A parent link is kept only if the parent is in the snapshot and its
    /// start time is not later than the child's. Every link on a ppid cycle is
    /// then dropped, so those processes become tops of their own trees.
    pub fn build(snap: &'a Snapshot) -> Lineage<'a> {
        let procs = &snap.procs;
        let by_pid: HashMap<Pid, usize> = procs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.key.pid, i))
            .collect();

        let mut parent: Vec<Option<usize>> = procs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let j = *by_pid.get(&p.ppid?)?;
                (j != i && procs[j].key.start_time <= p.key.start_time).then_some(j)
            })
            .collect();
        break_cycles(&mut parent);

        // Group children by parent. The sort is stable, so children keep the
        // snapshot's order.
        let mut edges: Vec<(usize, ProcKey)> = parent
            .iter()
            .enumerate()
            .filter_map(|(i, par)| Some(((*par)?, procs[i].key)))
            .collect();
        edges.sort_by_key(|&(par, _)| par);
        let mut child_start = vec![0; procs.len() + 1];
        for &(par, _) in &edges {
            child_start[par + 1] += 1;
        }
        for i in 0..procs.len() {
            child_start[i + 1] += child_start[i];
        }
        let child_keys = edges.into_iter().map(|(_, key)| key).collect();

        Lineage {
            snap,
            by_pid,
            parent,
            child_start,
            child_keys,
        }
    }

    /// The process with key `k`.
    ///
    /// # Panics
    /// If `k` is not in the snapshot this lineage was built from.
    pub fn proc_(&self, k: ProcKey) -> &'a RawProc {
        let snap: &'a Snapshot = self.snap;
        match self.index_of(k) {
            Some(i) => &snap.procs[i],
            None => panic!("{k:?} is not in the snapshot"),
        }
    }

    /// The key of the process with this PID, if there is one.
    pub fn key_of(&self, pid: Pid) -> Option<ProcKey> {
        self.by_pid.get(&pid).map(|&i| self.snap.procs[i].key)
    }

    /// The validated parent, or `None` for a process with no trustworthy one.
    pub fn parent(&self, k: ProcKey) -> Option<ProcKey> {
        let par = self.parent[self.index_of(k)?]?;
        Some(self.snap.procs[par].key)
    }

    /// Direct children, in snapshot order.
    pub fn children(&self, k: ProcKey) -> &[ProcKey] {
        match self.index_of(k) {
            Some(i) => &self.child_keys[self.child_start[i]..self.child_start[i + 1]],
            None => &[],
        }
    }

    /// Parent, grandparent, ...: nearest first.
    pub fn ancestors(&self, k: ProcKey) -> Vec<ProcKey> {
        let mut out = Vec::new();
        let mut cur = self.index_of(k);
        while let Some(par) = cur.and_then(|i| self.parent[i]) {
            out.push(self.snap.procs[par].key);
            cur = Some(par);
        }
        out
    }

    /// Every descendant in pre-order (a process before its children, children
    /// in snapshot order), not including `k` itself.
    pub fn descendants(&self, k: ProcKey) -> Vec<ProcKey> {
        let mut out = Vec::new();
        let mut stack: Vec<ProcKey> = self.children(k).iter().rev().copied().collect();
        while let Some(key) = stack.pop() {
            out.push(key);
            stack.extend(self.children(key).iter().rev().copied());
        }
        out
    }

    /// Number of ancestors: 0 for the top of a tree.
    pub fn depth(&self, k: ProcKey) -> usize {
        let mut depth = 0;
        let mut cur = self.index_of(k);
        while let Some(par) = cur.and_then(|i| self.parent[i]) {
            depth += 1;
            cur = Some(par);
        }
        depth
    }

    /// True when nobody meaningful started this process: it has no validated
    /// parent, or its parent is PID 1 or a subreaper (`systemd --user`, WSL's
    /// `/init`) that inherits orphans.
    pub fn is_adopted(&self, k: ProcKey) -> bool {
        match self.index_of(k).and_then(|i| self.parent[i]) {
            None => true,
            Some(par) => {
                let parent = &self.snap.procs[par];
                parent.key.pid == 1 || is_subreaper(parent)
            }
        }
    }

    /// The app that started `k`, or `None` when `k` was started from a shell
    /// (the user did it, so there is nothing to name) or has no parent.
    pub fn launcher(&self, k: ProcKey) -> Option<String> {
        let parent = &self.snap.procs[self.parent[self.index_of(k)?]?];
        if is_shell(parent) {
            return None;
        }
        Some(app_name(parent, self.snap.home.as_deref()))
    }

    /// The app at the top of `k`'s ancestry: the farthest ancestor that is not
    /// PID 0 or 1 or a system root (`launchd`, `systemd`, `init`, `explorer.exe`,
    /// `services.exe`).
    pub fn top_app(&self, k: ProcKey) -> Option<String> {
        let mut top = None;
        let mut cur = self.index_of(k)?;
        while let Some(par) = self.parent[cur] {
            if !is_system_root(&self.snap.procs[par]) {
                top = Some(par);
            }
            cur = par;
        }
        top.map(|i| app_name(&self.snap.procs[i], self.snap.home.as_deref()))
    }

    fn index_of(&self, k: ProcKey) -> Option<usize> {
        let procs = &self.snap.procs;
        match self.by_pid.get(&k.pid) {
            Some(&i) if procs[i].key == k => Some(i),
            // The same PID is listed twice, so the map holds only one of them.
            Some(_) => procs.iter().position(|p| p.key == k),
            None => None,
        }
    }
}

/// A short display name for the program behind `p`, the first rule that
/// applies winning:
/// 1. the `X` of the first `X.app` path component (macOS bundles);
/// 2. the first directory under `home` that starts with `.`, without the dot
///    (`~/.hermes/venv/bin/python` is "hermes");
/// 3. the executable's file name without an executable extension;
/// 4. the process name, if there is no path.
///
/// The path is `exe`, or `argv[0]` when `exe` is unknown. Both `/` and `\` split
/// it, whatever OS cctree runs on.
pub fn app_name(p: &RawProc, home: Option<&str>) -> String {
    let non_empty = |s: &&str| !s.is_empty();
    let path = p
        .exe
        .as_deref()
        .filter(non_empty)
        .or_else(|| p.argv.first().map(String::as_str).filter(non_empty));
    if let Some(path) = path {
        if let Some(app) = bundle_name(path) {
            return app.to_string();
        }
        if let Some(dir) = home.and_then(|h| dot_dir_under(path, h)) {
            return dir.to_string();
        }
        let stem = strip_executable_extension(file_name(path));
        if !stem.is_empty() {
            return stem.to_string();
        }
    }
    strip_executable_extension(&p.name).to_string()
}

/// Whether `p` is a shell (or `login`, `cmd`, ...): its name, case-insensitive
/// and without `.exe`, is one of [`SHELLS`].
pub fn is_shell(p: &RawProc) -> bool {
    let name = strip_suffix_ignore_case(&p.name, ".exe").unwrap_or(&p.name);
    SHELLS.iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// Drops the parent link of every process on a ppid cycle (a process that is its
/// own ancestor), so the links that remain form a forest. Processes that merely
/// hang off a cycle keep their link.
fn break_cycles(parent: &mut [Option<usize>]) {
    const UNSEEN: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;

    let mut state = vec![UNSEEN; parent.len()];
    let mut path: Vec<usize> = Vec::new();
    let mut on_cycle: Vec<usize> = Vec::new();
    for start in 0..parent.len() {
        if state[start] != UNSEEN {
            continue;
        }
        // Follow parent links until the chain ends, reaches a finished
        // process, or runs into the path walked so far (a cycle).
        path.clear();
        let mut cur = Some(start);
        while let Some(i) = cur {
            match state[i] {
                UNSEEN => {
                    state[i] = ON_PATH;
                    path.push(i);
                    cur = parent[i];
                }
                ON_PATH => {
                    if let Some(at) = path.iter().position(|&x| x == i) {
                        on_cycle.extend_from_slice(&path[at..]);
                    }
                    break;
                }
                _ => break,
            }
        }
        for &i in &path {
            state[i] = DONE;
        }
    }
    for i in on_cycle {
        parent[i] = None;
    }
}

/// A process that inherits orphans in place of PID 1.
fn is_subreaper(p: &RawProc) -> bool {
    let systemd_user = p.name == "systemd" && p.argv.iter().any(|a| a == "--user");
    let wsl_init = p.argv.first().is_some_and(|a| a == "/init");
    systemd_user || wsl_init
}

fn is_system_root(p: &RawProc) -> bool {
    p.key.pid <= 1 || SYSTEM_ROOTS.iter().any(|r| p.name.eq_ignore_ascii_case(r))
}

/// The `X` of the first `X.app` component of `path`.
fn bundle_name(path: &str) -> Option<&str> {
    path.split(['/', '\\'])
        .find_map(|c| c.strip_suffix(".app").filter(|name| !name.is_empty()))
}

/// For a `path` under `home`, the name (without its dot) of the first directory
/// below `home` that starts with `.`. The last component is the file itself and
/// never counts. `home` matches ignoring ASCII case, since Windows paths do.
fn dot_dir_under<'p>(path: &'p str, home: &str) -> Option<&'p str> {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() || !path.get(..home.len())?.eq_ignore_ascii_case(home) {
        return None;
    }
    // The next character must end a component: `/Users/uu` is not under `/Users/u`.
    let below = path[home.len()..].strip_prefix(['/', '\\'])?;
    let mut parts = below.split(['/', '\\']).peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        if let Some(name) = part.strip_prefix('.')
            && !name.is_empty()
            && name != "."
        {
            return Some(name);
        }
    }
    None
}

/// The last path component, splitting on both `/` and `\`.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn strip_executable_extension(file: &str) -> &str {
    match file.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && EXECUTABLE_EXTENSIONS
                    .iter()
                    .any(|e| ext.eq_ignore_ascii_case(e)) =>
        {
            stem
        }
        _ => file,
    }
}

fn strip_suffix_ignore_case<'s>(s: &'s str, suffix: &str) -> Option<&'s str> {
    let at = s.len().checked_sub(suffix.len())?;
    // `get` is `None` when `at` is not on a character boundary.
    let tail = s.get(at..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &s[..at])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Platform;
    use crate::testkit::{p, snap};

    #[test]
    fn parent_requires_earlier_start() {
        let s = snap(
            Platform::Windows,
            vec![
                p(10, 1, 100, &["a"]),
                p(11, 10, 150, &["b"]),
                p(12, 10, 50, &["c"]),
            ],
        );
        let l = Lineage::build(&s);
        assert_eq!(l.parent(l.key_of(11).unwrap()), l.key_of(10));
        // PID 10 was reused: the recorded parent started after its child.
        assert_eq!(l.parent(l.key_of(12).unwrap()), None);
        assert!(l.is_adopted(l.key_of(12).unwrap()));
    }

    #[test]
    fn lineage_breaks_ppid_cycles() {
        let s = snap(
            Platform::Windows,
            vec![p(20, 21, 100, &["a"]), p(21, 20, 100, &["b"])],
        );
        let l = Lineage::build(&s);
        let a = l.key_of(20).unwrap();
        assert!(l.ancestors(a).len() <= 1);
        assert!(l.is_adopted(a) && l.is_adopted(l.key_of(21).unwrap()));
    }

    #[test]
    fn equal_start_time_is_still_a_valid_parent() {
        // Start times have one-second resolution: a parent and its child can
        // share one.
        let s = snap(
            Platform::Linux,
            vec![p(10, 1, 100, &["a"]), p(11, 10, 100, &["b"])],
        );
        let l = Lineage::build(&s);
        assert_eq!(l.parent(l.key_of(11).unwrap()), l.key_of(10));
    }

    #[test]
    fn adoption_rules() {
        let s = snap(
            Platform::Linux,
            vec![
                p(1, 0, 1, &["/sbin/init"]),
                p(500, 1, 2, &["/usr/lib/systemd/systemd", "--user"]),
                p(600, 1, 2, &["/init"]),
                p(30, 1, 10, &["x"]),
                p(31, 500, 10, &["y"]),
                p(32, 600, 10, &["z"]),
                p(40, 99, 10, &["w"]),
                p(41, 30, 11, &["v"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| l.key_of(pid).unwrap();
        assert!(
            l.is_adopted(k(30))
                && l.is_adopted(k(31))
                && l.is_adopted(k(32))
                && l.is_adopted(k(40))
        );
        assert!(!l.is_adopted(k(41)));
    }

    #[test]
    fn launcher_is_none_when_parent_is_a_shell() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(
                    5,
                    1,
                    1,
                    &[
                        "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper",
                    ],
                ),
                p(6, 5, 2, &["/bin/zsh"]),
                p(7, 6, 3, &["claude"]),
                p(8, 5, 3, &["claude"]),
            ],
        );
        let l = Lineage::build(&s);
        assert_eq!(l.launcher(l.key_of(7).unwrap()), None);
        assert_eq!(
            l.launcher(l.key_of(8).unwrap()).as_deref(),
            Some("Visual Studio Code")
        );
    }

    #[test]
    fn app_names() {
        let h = Some("/Users/u");
        assert_eq!(
            app_name(
                &p(1, 0, 1, &["/Users/u/.hermes/hermes-agent/venv/bin/python"]),
                h
            ),
            "hermes"
        );
        assert_eq!(
            app_name(
                &p(1, 0, 1, &["/Applications/Claude.app/Contents/MacOS/Claude"]),
                h
            ),
            "Claude"
        );
        assert_eq!(
            app_name(
                &p(1, 0, 1, &["C:\\Program Files\\Microsoft VS Code\\Code.exe"]),
                h
            ),
            "Code"
        );
        assert_eq!(app_name(&p(1, 0, 1, &["/usr/bin/python3"]), h), "python3");
    }

    #[test]
    fn top_app_skips_system_roots() {
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
            ],
        );
        let l = Lineage::build(&s);
        assert_eq!(
            l.top_app(l.key_of(2724).unwrap()).as_deref(),
            Some("hermes")
        );
    }

    #[test]
    fn self_parent_and_longer_cycles_terminate() {
        let mut orphan = p(2, 0, 5, &["o"]);
        orphan.ppid = None;
        let s = snap(
            Platform::Windows,
            vec![
                p(1, 1, 5, &["self"]),
                orphan,
                // 30 -> 32 -> 31 -> 30, all started in the same second.
                p(30, 32, 7, &["a"]),
                p(31, 30, 7, &["b"]),
                p(32, 31, 7, &["c"]),
                // A tail hanging off the cycle keeps its (now top-level) parent.
                p(33, 30, 9, &["tail"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| l.key_of(pid).unwrap();
        for pid in [1, 2, 30, 31, 32] {
            assert_eq!(l.parent(k(pid)), None, "pid {pid}");
            assert!(l.ancestors(k(pid)).is_empty() && l.is_adopted(k(pid)));
            assert_eq!(l.depth(k(pid)), 0);
        }
        assert_eq!(l.parent(k(33)), Some(k(30)));
        assert_eq!(l.ancestors(k(33)), vec![k(30)]);
        assert!(!l.is_adopted(k(33)));
        assert_eq!(l.children(k(30)), &[k(33)]);
        assert_eq!(l.descendants(k(30)), vec![k(33)]);
    }

    #[test]
    fn walks_follow_the_validated_tree() {
        let s = snap(
            Platform::Linux,
            vec![
                p(12, 10, 4, &["c"]),
                p(1, 0, 1, &["/sbin/init"]),
                p(13, 11, 5, &["d"]),
                p(10, 1, 2, &["a"]),
                p(11, 10, 3, &["b"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| l.key_of(pid).unwrap();
        // Nearest first.
        assert_eq!(l.ancestors(k(13)), vec![k(11), k(10), k(1)]);
        assert_eq!(l.depth(k(13)), 3);
        assert_eq!(l.depth(k(1)), 0);
        // Children keep snapshot order; descendants are pre-order, without self.
        assert_eq!(l.children(k(10)), &[k(12), k(11)]);
        assert_eq!(l.descendants(k(10)), vec![k(12), k(11), k(13)]);
        assert_eq!(l.descendants(k(1)).len(), 4);
        assert!(l.descendants(k(13)).is_empty() && l.children(k(13)).is_empty());
        assert_eq!(l.proc_(k(11)).argv, ["b"]);
        assert_eq!(l.key_of(99), None);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let s = snap(Platform::Linux, vec![p(1, 0, 1, &["init"])]);
        let l = Lineage::build(&s);
        let gone = ProcKey {
            pid: 1,
            start_time: 2,
        };
        assert_eq!(l.parent(gone), None);
        assert!(l.children(gone).is_empty() && l.ancestors(gone).is_empty());
        assert!(l.descendants(gone).is_empty());
        assert_eq!(l.depth(gone), 0);
        assert_eq!((l.launcher(gone), l.top_app(gone)), (None, None));
        assert!(l.is_adopted(gone));
    }

    #[test]
    fn duplicate_pids_do_not_panic() {
        let s = snap(
            Platform::Windows,
            vec![
                p(5, 1, 10, &["old"]),
                p(5, 1, 20, &["new"]),
                p(6, 5, 30, &["kid"]),
            ],
        );
        let l = Lineage::build(&s);
        for pr in &s.procs {
            assert_eq!(l.proc_(pr.key).argv, pr.argv);
        }
        assert_eq!(l.key_of(5), Some(s.procs[1].key));
        assert_eq!(l.parent(l.key_of(6).unwrap()), l.key_of(5));
    }

    #[test]
    fn shell_names_ignore_case_and_exe() {
        for name in [
            "sh",
            "bash",
            "zsh",
            "fish",
            "dash",
            "ksh",
            "tcsh",
            "nu",
            "login",
            "cmd.exe",
            "PowerShell.EXE",
            "pwsh.exe",
            "Zsh",
        ] {
            assert!(is_shell(&p(1, 0, 1, &["x"]).name(name)), "{name}");
        }
        for name in ["node", "shell", "bashful", "claude", "cmd.com", ""] {
            assert!(!is_shell(&p(1, 0, 1, &["x"]).name(name)), "{name}");
        }
    }

    #[test]
    fn app_name_rule_order_and_fallbacks() {
        let h = Some("/Users/u");
        let name = |argv: &[&str], home| app_name(&p(1, 0, 1, argv), home);
        // 1. the first X.app component beats the home dot-directory.
        assert_eq!(
            name(&["/Users/u/.cache/Foo.app/Contents/MacOS/foo"], h),
            "Foo"
        );
        // 2. the first dot-directory under home; the file name itself does not count.
        assert_eq!(name(&["/Users/u/.bun/bin/bun"], h), "bun");
        assert_eq!(name(&["/Users/u/.config/x/.hidden/y"], h), "config");
        assert_eq!(name(&["/Users/u/.hidden"], h), ".hidden");
        assert_eq!(name(&["/Users/u/code/app/.venv/bin/python"], h), "venv");
        assert_eq!(
            name(&["/Users/u/.hermes/bin/x"], Some("/Users/u/")),
            "hermes"
        );
        assert_eq!(name(&["~/.hermes/bin/x"], Some("~")), "hermes");
        assert_eq!(
            name(
                &["C:\\Users\\u\\.cargo\\bin\\cctree.exe"],
                Some("c:\\Users\\U")
            ),
            "cargo"
        );
        // Only real children of home count, never a sibling that shares the prefix.
        assert_eq!(name(&["/Users/uu/.hermes/bin/x"], h), "x");
        assert_eq!(name(&["/tmp/.hermes/bin/x"], None), "x");
        assert_eq!(name(&["/.hermes/bin/x"], Some("/")), "x");
        // 3. executable name without an executable extension, either separator.
        assert_eq!(name(&["C:\\bin\\Node.EXE"], h), "Node");
        assert_eq!(name(&["/usr/bin/python3.12"], h), "python3.12");
        assert_eq!(
            name(&["/usr/lib/com.apple.Foo.Bar"], h),
            "com.apple.Foo.Bar"
        );
        // The path comes from exe when it is known, argv[0] otherwise.
        let r = p(1, 0, 1, &["npm exec foo"]).exe("/opt/bin/node");
        assert_eq!(app_name(&r, h), "node");
        let mut r = p(1, 0, 1, &["/opt/bin/tool"]);
        r.exe = None;
        assert_eq!(app_name(&r, h), "tool");
        // 4. no usable path at all: the process name, extension dropped.
        let mut r = p(1, 0, 1, &["x"]).name("svchost.exe");
        (r.exe, r.argv) = (None, vec![]);
        assert_eq!(app_name(&r, h), "svchost");
        r.name = "launchd".into();
        assert_eq!(app_name(&r, h), "launchd");
    }

    #[test]
    fn top_app_is_the_farthest_non_system_ancestor() {
        let s = snap(
            Platform::Windows,
            vec![
                p(4, 0, 1, &["System"]).name("System"),
                p(100, 0, 2, &["C:\\Windows\\explorer.exe"]).name("Explorer.EXE"),
                p(200, 100, 3, &["C:\\Program Files\\Terminal\\wt.exe"]),
                p(300, 200, 4, &["C:\\Windows\\System32\\cmd.exe"]),
                p(400, 300, 5, &["node.exe"]),
                p(500, 100, 3, &["C:\\Program Files\\Tool\\tool.exe"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| l.key_of(pid).unwrap();
        assert_eq!(l.top_app(k(400)).as_deref(), Some("wt"));
        assert_eq!(l.top_app(k(500)), None); // only explorer above it
        assert_eq!(l.top_app(k(200)), None);
        assert_eq!(l.top_app(k(100)), None);
        assert_eq!(l.top_app(k(4)), None);
    }

    #[test]
    fn launcher_needs_a_non_shell_parent() {
        let s = snap(
            Platform::Windows,
            vec![
                p(1, 0, 1, &["C:\\Program Files\\Claude\\Claude.exe"]),
                p(2, 1, 2, &["C:\\Windows\\System32\\cmd.exe"]),
                p(3, 1, 2, &["claude.exe"]),
                p(4, 2, 3, &["claude.exe"]),
                p(5, 99, 3, &["claude.exe"]),
            ],
        );
        let l = Lineage::build(&s);
        let k = |pid| l.key_of(pid).unwrap();
        assert_eq!(l.launcher(k(3)).as_deref(), Some("Claude"));
        assert_eq!(l.launcher(k(4)), None); // started from cmd.exe
        assert_eq!(l.launcher(k(1)), None); // no parent
        assert_eq!(l.launcher(k(5)), None); // parent not in the snapshot
    }
}
