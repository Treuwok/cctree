//! Core data types shared by the collectors, the tree builder and the UI.
//!
//! Everything here is plain data: a [`Snapshot`] is what one collection pass
//! produces, and it round-trips through JSON so it can be dumped and replayed.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub type Pid = u32;

/// Environment variables kept when reading another process's environment.
/// Every other name is dropped at collection time and never stored.
pub const ENV_ALLOWLIST: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PROJECT_DIR",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_ENTRYPOINT",
    "CODEX_HOME",
];

/// Identity of a process that survives PID reuse: the PID plus its start time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcKey {
    pub pid: Pid,
    /// Process start time in Unix seconds.
    pub start_time: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tool {
    ClaudeCode,
    Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcStatus {
    Running,
    Sleeping,
    Zombie,
    Other,
}

/// Why a memory figure could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemUnavailable {
    PermissionDenied,
    OsTooOld,
    Gone,
    Unsupported,
}

/// Per-process memory, in bytes. A `None` field means "not available".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemInfo {
    pub private: Option<u64>,
    pub in_ram: Option<u64>,
    pub swapped_out: Option<u64>,
    pub shared: Option<u64>,
    /// The figure the OS's own tool reports (Activity Monitor, Task Manager, ...).
    pub os_tool_value: Option<u64>,
    pub swapped_is_estimate: bool,
    pub unavailable: Option<MemUnavailable>,
}

impl MemInfo {
    pub fn unavailable(r: MemUnavailable) -> MemInfo {
        MemInfo {
            unavailable: Some(r),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawProc {
    pub key: ProcKey,
    pub ppid: Option<Pid>,
    pub name: String,
    pub exe: Option<String>,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    /// Only names in [`ENV_ALLOWLIST`] are ever stored here.
    pub env: BTreeMap<String, String>,
    pub is_own_user: bool,
    pub is_root_user: bool,
    pub status: ProcStatus,
    pub cpu: Option<f32>,
    pub mem: Option<MemInfo>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub ram_used: u64,
    pub ram_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub cpu_pct: Option<f32>,
    pub logical_cpus: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub platform: Platform,
    /// Collection time in Unix seconds.
    pub taken_at: u64,
    pub self_key: ProcKey,
    pub home: Option<String>,
    pub in_wsl: bool,
    pub procs: Vec<RawProc>,
    pub system: SystemInfo,
    /// Processes whose details could not be read (permission denied, gone, ...).
    pub unreadable_count: usize,
}

impl Snapshot {
    pub fn get(&self, k: ProcKey) -> Option<&RawProc> {
        self.procs.iter().find(|p| p.key == k)
    }

    pub fn index(&self) -> HashMap<Pid, &RawProc> {
        self.procs.iter().map(|p| (p.key.pid, p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn snapshot_roundtrips_through_json() {
        let p = RawProc {
            key: ProcKey {
                pid: 7,
                start_time: 100,
            },
            ppid: Some(1),
            name: "node".into(),
            exe: Some("/usr/bin/node".into()),
            argv: vec!["node".into(), "x.js".into()],
            cwd: None,
            env: BTreeMap::from([("CLAUDECODE".into(), "1".into())]),
            is_own_user: true,
            is_root_user: false,
            status: ProcStatus::Running,
            cpu: Some(0.5),
            mem: Some(MemInfo {
                private: Some(10),
                ..Default::default()
            }),
        };
        let s = Snapshot {
            platform: Platform::Linux,
            taken_at: 1,
            self_key: p.key,
            home: Some("~".into()),
            in_wsl: false,
            procs: vec![p.clone()],
            system: SystemInfo::default(),
            unreadable_count: 0,
        };
        let back: Snapshot = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.get(p.key), Some(&p));
        assert_eq!(
            back.index().get(&7).map(|r| &r.name),
            Some(&"node".to_string())
        );
    }

    #[test]
    fn platform_current_matches_cfg() {
        #[cfg(target_os = "macos")]
        assert_eq!(Platform::current(), Platform::MacOs);
        #[cfg(windows)]
        assert_eq!(Platform::current(), Platform::Windows);
        #[cfg(target_os = "linux")]
        assert_eq!(Platform::current(), Platform::Linux);
    }
}
