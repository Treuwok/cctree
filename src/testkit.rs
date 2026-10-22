//! Test-only builders for synthetic processes and snapshots.
//!
//! Compiled only under `cfg(test)` (see `lib.rs`), shared by every module's
//! unit tests. `RawProc` lives in this crate, so the chainable builders are
//! plain inherent methods that only exist in test builds: no extension trait
//! to import, just `use crate::testkit::{p, snap};`.
//!
//! ```ignore
//! let s = snap(Platform::MacOs, vec![
//!     p(10, 1, 5, &["/usr/bin/node", "x.js"]).cwd("/Users/u/prj").env("CLAUDECODE", "1"),
//!     p(11, 10, 6, &["sh"]).zombie(),
//! ]);
//! ```
//!
//! Builder defaults (see [`p`]): own user, not root, `Running`, `cpu = Some(0.0)`,
//! `mem.private = 1 MiB`, no cwd, empty env, `name`/`exe` taken from `argv[0]`.

use std::collections::BTreeMap;

use crate::classify::classify;
use crate::config::{McpConfigs, McpScope, McpServerConfig};
use crate::model::{
    MemInfo, MemUnavailable, Pid, Platform, ProcKey, ProcStatus, RawProc, Snapshot, SystemInfo,
    Tool,
};
use crate::tree::{Node, ViewMode, build_tree};

/// Builds a snapshot around `procs`. cctree itself is the (absent) process
/// `99999` started at `1`, and the home directory is `/Users/u`.
pub fn snap(platform: Platform, procs: Vec<RawProc>) -> Snapshot {
    Snapshot {
        platform,
        taken_at: 0,
        self_key: ProcKey {
            pid: 99999,
            start_time: 1,
        },
        home: Some("/Users/u".into()),
        in_wsl: false,
        procs,
        system: SystemInfo::default(),
        unreadable_count: 0,
    }
}

/// Builds a process that belongs to the current user and is running.
pub fn p(pid: Pid, ppid: Pid, start: u64, argv: &[&str]) -> RawProc {
    let first = argv.first().copied().unwrap_or_default();
    RawProc {
        key: ProcKey {
            pid,
            start_time: start,
        },
        ppid: Some(ppid),
        name: file_name(first).to_string(),
        exe: argv.first().map(|a| a.to_string()),
        argv: argv.iter().map(|a| a.to_string()).collect(),
        cwd: None,
        env: BTreeMap::new(),
        is_own_user: true,
        is_root_user: false,
        status: ProcStatus::Running,
        cpu: Some(0.0),
        mem: Some(MemInfo {
            private: Some(1 << 20),
            ..Default::default()
        }),
    }
}

/// One user-level Claude Code MCP config: `chrome-cdp`, started as
/// `npx -y chrome-devtools-mcp@latest`. Pairs with [`base`].
pub(crate) fn cfgs() -> McpConfigs {
    McpConfigs {
        servers: vec![McpServerConfig {
            tool: Tool::ClaudeCode,
            name: "chrome-cdp".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "chrome-devtools-mcp@latest".into()],
            scope: McpScope::User,
        }],
        problems: vec![],
    }
}

/// A macOS process tree with one Claude Code session (pid 200, cwd
/// `/Users/u/prj/a`) started from a shell. Its direct children:
///
/// - 210 `npm exec chrome-devtools-mcp@latest` (matches `chrome-cdp` in
///   [`cfgs`]) -> 211 the server -> 212 its telemetry watchdog;
/// - 220 `npm exec @playwright/mcp@latest` (no config) -> 221 the server,
///   which carries `CLAUDE_CODE_SESSION_ID=S1`;
/// - 230 a Bash-tool shell `/bin/zsh -c ls`.
pub(crate) fn base() -> Vec<RawProc> {
    vec![
        p(1, 0, 1, &["/sbin/launchd"]),
        p(100, 1, 10, &["/bin/zsh"]),
        p(200, 100, 20, &["claude"]).cwd("/Users/u/prj/a"),
        p(210, 200, 21, &["npm", "exec", "chrome-devtools-mcp@latest"]),
        p(211, 210, 22, &["chrome-devtools-mcp"]),
        p(
            212,
            211,
            23,
            &["node", "/x/telemetry/watchdog/main.js", "--parent-pid=211"],
        ),
        p(220, 200, 21, &["npm", "exec", "@playwright/mcp@latest"]),
        p(221, 220, 22, &["node", "/x/.bin/playwright-mcp"]).env("CLAUDE_CODE_SESSION_ID", "S1"),
        p(230, 200, 30, &["/bin/zsh", "-c", "ls"]),
    ]
}

/// Classifies `v` on macOS with [`cfgs`] and builds the `view` tree.
pub(crate) fn tree(v: Vec<RawProc>, view: ViewMode) -> Vec<Node> {
    let s = snap(Platform::MacOs, v);
    let c = classify(&s, &cfgs());
    build_tree(&s, &c, view)
}

/// The last path component, splitting on both `/` and `\`.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl RawProc {
    pub fn name(mut self, name: &str) -> RawProc {
        self.name = name.to_string();
        self
    }

    pub fn exe(mut self, exe: &str) -> RawProc {
        self.exe = Some(exe.to_string());
        self
    }

    pub fn cwd(mut self, cwd: &str) -> RawProc {
        self.cwd = Some(cwd.to_string());
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> RawProc {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    /// Marks the process as a zombie. Only the status changes.
    pub fn zombie(mut self) -> RawProc {
        self.status = ProcStatus::Zombie;
        self
    }

    /// A system (root / SYSTEM) process: not ours, and root.
    pub fn root_user(mut self) -> RawProc {
        self.is_own_user = false;
        self.is_root_user = true;
        self
    }

    /// A process owned by another, non-root user.
    pub fn other_user(mut self) -> RawProc {
        self.is_own_user = false;
        self
    }

    /// Sets the main memory figure. `None` models a process whose memory
    /// could not be read (the reason is `PermissionDenied`).
    pub fn mem_private(mut self, private: Option<u64>) -> RawProc {
        self.mem = Some(match private {
            Some(bytes) => MemInfo {
                private: Some(bytes),
                ..Default::default()
            },
            None => MemInfo::unavailable(MemUnavailable::PermissionDenied),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_builders() {
        let r = p(10, 1, 5, &["/usr/bin/node", "x.js"]);
        assert_eq!(r.name, "node");
        assert_eq!(r.exe.as_deref(), Some("/usr/bin/node"));
        assert_eq!(r.ppid, Some(1));
        assert!(r.is_own_user && !r.is_root_user && r.status == ProcStatus::Running);
        assert_eq!(r.cpu, Some(0.0));
        assert_eq!(r.mem.unwrap().private, Some(1 << 20));

        let r = p(1, 0, 1, &[r"C:\Program Files\nodejs\node.exe"])
            .name("n")
            .exe("/e")
            .cwd("/c")
            .env("K", "v")
            .mem_private(Some(7));
        assert_eq!(
            (r.name.as_str(), r.exe.as_deref(), r.cwd.as_deref()),
            ("n", Some("/e"), Some("/c"))
        );
        assert_eq!(r.env["K"], "v");
        assert_eq!(r.mem.unwrap().private, Some(7));
        assert_eq!(p(1, 0, 1, &[r"C:\x\node.exe"]).name, "node.exe");

        assert_eq!(p(1, 0, 1, &["x"]).zombie().status, ProcStatus::Zombie);
        let root = p(1, 0, 1, &["x"]).root_user();
        assert!(root.is_root_user && !root.is_own_user);
        let other = p(1, 0, 1, &["x"]).other_user();
        assert!(!other.is_root_user && !other.is_own_user);
        let none = p(1, 0, 1, &["x"]).mem_private(None).mem.unwrap();
        assert_eq!(
            (none.private, none.unavailable),
            (None, Some(MemUnavailable::PermissionDenied))
        );
    }

    #[test]
    fn snap_has_fixed_self_and_home() {
        let s = snap(Platform::Linux, vec![p(1, 0, 1, &["init"])]);
        assert_eq!(
            s.self_key,
            ProcKey {
                pid: 99999,
                start_time: 1
            }
        );
        assert_eq!(s.home.as_deref(), Some("/Users/u"));
        assert_eq!(s.procs.len(), 1);
    }
}
