//! Two-tier process collector (spec 4.9).
//!
//! * [`Collector::snapshot`] is the cheap tier. It lists every process with
//!   its cheap fields and CPU, and reads the static fields (argv, exe, cwd,
//!   allowlisted environment) once for each process the first time it shows up.
//! * [`Collector::fill_dynamic`] is the expensive tier. It reads memory only
//!   for the processes the UI will show, plus the system-wide numbers.
//!
//! Two rules from the design review are load-bearing:
//!
//! 1. **CPU is computed in the all-process refresh** (`with_cpu()` in
//!    [`Collector::snapshot`]), never in a second, tracked-only refresh. On
//!    Linux sysinfo's `set_time` swaps "previous" for "current" CPU time on
//!    every refresh whatever the refresh kind says, so a second refresh would
//!    compare against a baseline taken milliseconds earlier and read ~0
//!    (spec 4.9, pinned by `busy_child_reports_real_cpu`).
//!
//!    The refresh's job is to keep each process's `accumulated_cpu_time` fresh;
//!    the percentage is then taken from the change in that figure between two
//!    snapshots ([`cpu_percent`]), not from sysinfo's own `cpu_usage()`. The
//!    latter is wrong on its first reading on some platforms: on Windows the
//!    first value is measured against a zero baseline (about 0 %), on Linux and
//!    macOS it is skipped when the baseline is exactly 0 (a very young process).
//! 2. **Environment variables are read only through a throwaway `System`** that
//!    is dropped right after [`env::extract_env`]. The long-lived `sys` never
//!    holds environ: sysinfo caches the whole environment, which for MCP child
//!    processes includes secrets such as `CLAUDE_CODE_MESSAGING_TOKEN`
//!    (spec 4.3, pinned by `main_system_never_holds_environ`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{
    Pid as SysPid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, Uid,
    UpdateKind,
};

use crate::model::{
    MemInfo, MemUnavailable, Platform, ProcKey, ProcStatus, RawProc, Snapshot, SystemInfo,
};

pub mod env;
pub mod mem;

/// Process names (without `.exe`) counted by `unreadable_count` when their
/// command line cannot be read (spec 4.8).
const TOOL_NAMES: &[&str] = &["node", "npm", "npx", "bun", "deno", "claude", "codex"];

/// Converts a per-core CPU percentage (100 = one core fully busy) to cctree's
/// unit: Windows is a share of the whole machine (like Task Manager), macOS and
/// Linux stay a share of one core (like `top`).
pub fn normalize_cpu(raw: f32, platform: Platform, logical_cpus: usize) -> f32 {
    match platform {
        Platform::Windows => raw / logical_cpus.max(1) as f32,
        Platform::MacOs | Platform::Linux => raw,
    }
}

/// Per-core CPU percentage between two readings of a process's accumulated CPU
/// time (milliseconds) taken `elapsed` apart: 100 means one core fully busy.
fn cpu_percent(prev_ms: u64, now_ms: u64, elapsed: Duration) -> f32 {
    let wall_ms = elapsed.as_secs_f32() * 1000.0;
    if wall_ms <= 0.0 {
        return 0.0;
    }
    now_ms.saturating_sub(prev_ms) as f32 / wall_ms * 100.0
}

/// Undoes a rewritten process title.
///
/// A Node process that sets `process.title` (`npm exec pkg --flag`) overwrites
/// its argument area. sysinfo then reports the whole title as `argv[0]`, and the
/// "arguments" after it are whatever follows in memory: the environment
/// strings (`HOME=...`, `CLAUDE_CODE_MESSAGING_TOKEN=...`) on macOS, empty
/// strings on Linux. Keeping those would store environment values that
/// [`ENV_ALLOWLIST`](crate::model::ENV_ALLOWLIST) was meant to drop (spec 4.3).
///
/// If `raw[0]` contains whitespace and every later element is empty or looks
/// like `NAME=...`, the result is the whitespace-separated words of `raw[0]`.
/// Anything else is returned unchanged.
pub fn normalize_argv(raw: Vec<String>) -> Vec<String> {
    let Some((first, rest)) = raw.split_first() else {
        return raw;
    };
    if !first.contains(|c: char| c.is_ascii_whitespace())
        || !rest.iter().all(|a| a.is_empty() || is_env_like(a))
    {
        return raw;
    }
    let words: Vec<String> = first.split_ascii_whitespace().map(str::to_string).collect();
    if words.is_empty() { raw } else { words }
}

/// Whether `s` starts with `NAME=`, where `NAME` is `[A-Za-z_][A-Za-z0-9_]*`.
fn is_env_like(s: &str) -> bool {
    let Some((name, _)) = s.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Fields read once per process and cached by [`ProcKey`].
struct StaticInfo {
    name: String,
    exe: Option<String>,
    argv: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    is_own_user: bool,
    is_root_user: bool,
}

pub struct Collector {
    /// Long-lived. Never refreshed with `with_environ`, so it never holds a
    /// process's environment (see the module docs).
    sys: System,
    statics: HashMap<ProcKey, StaticInfo>,
    /// The previous reading of each process's accumulated CPU time (ms) and
    /// when it was taken. A key without an entry has no CPU figure yet.
    cpu_prev: HashMap<ProcKey, (u64, Instant)>,
    own_uid: Option<Uid>,
    self_pid: SysPid,
    home: Option<String>,
    in_wsl: bool,
    logical_cpus: usize,
}

impl Collector {
    pub fn new() -> Collector {
        let mut sys = System::new();
        // Fills the logical-CPU list and takes the baseline for the system-wide
        // CPU figure that `fill_dynamic` reports.
        sys.refresh_cpu_usage();
        let logical_cpus = sys.cpus().len();
        Collector {
            sys,
            statics: HashMap::new(),
            cpu_prev: HashMap::new(),
            own_uid: None,
            self_pid: sysinfo::get_current_pid()
                .unwrap_or_else(|_| SysPid::from_u32(std::process::id())),
            home: std::env::home_dir().map(|h| h.to_string_lossy().into_owned()),
            in_wsl: detect_wsl_here(),
            logical_cpus,
        }
    }

    /// Cheap tier: every process, with CPU, plus the static fields of
    /// processes seen for the first time. `mem` stays `None` until
    /// [`Collector::fill_dynamic`].
    pub fn snapshot(&mut self) -> Snapshot {
        // `without_tasks()` is required: on Linux threads would otherwise be
        // listed as processes. `with_cpu()` must stay here (module docs, rule 1).
        // Without it `accumulated_cpu_time` would not be refreshed at all.
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_cpu()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let sampled_at = Instant::now();
        if self.own_uid.is_none() {
            self.own_uid = self
                .sys
                .process(self.self_pid)
                .and_then(|p| p.user_id().cloned());
        }

        let platform = Platform::current();
        let live: Vec<(ProcKey, &Process)> = self
            .sys
            .processes()
            .values()
            .map(|p| {
                let key = ProcKey {
                    pid: p.pid().as_u32(),
                    start_time: p.start_time(),
                };
                (key, p)
            })
            .collect();

        // Static fields: only for keys not seen before. The environment and cwd
        // are read only for new, own-user, non-zombie processes.
        let own_uid = &self.own_uid;
        let new: Vec<(ProcKey, &Process)> = live
            .iter()
            .filter(|(key, _)| !self.statics.contains_key(key))
            .map(|(key, p)| (*key, *p))
            .collect();
        let wanted: Vec<(SysPid, u64)> = new
            .iter()
            .filter(|(_, p)| is_own_user(p, own_uid) && p.status() != ProcessStatus::Zombie)
            .map(|(key, p)| (p.pid(), key.start_time))
            .collect();
        let mut env_cwd = read_env_and_cwd(&wanted);
        for (key, p) in &new {
            let (env, cwd) = env_cwd.remove(&p.pid()).unwrap_or_default();
            self.statics.insert(
                *key,
                StaticInfo {
                    name: p.name().to_string_lossy().into_owned(),
                    exe: p.exe().map(|e| e.to_string_lossy().into_owned()),
                    argv: normalize_argv(
                        p.cmd()
                            .iter()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect(),
                    ),
                    cwd,
                    env,
                    is_own_user: is_own_user(p, own_uid),
                    is_root_user: p.user_id().is_some_and(is_root_uid),
                },
            );
        }

        // Dynamic cheap fields, merged with the cached static ones.
        let mut procs = Vec::with_capacity(live.len());
        for (key, p) in &live {
            let info = &self.statics[key];
            // One reading is not enough: CPU needs the change since the last one.
            let accumulated_ms = p.accumulated_cpu_time();
            let cpu = self
                .cpu_prev
                .insert(*key, (accumulated_ms, sampled_at))
                .map(|(prev_ms, prev_at)| {
                    let raw = cpu_percent(prev_ms, accumulated_ms, sampled_at - prev_at);
                    normalize_cpu(raw, platform, self.logical_cpus)
                });
            procs.push(RawProc {
                key: *key,
                ppid: p.parent().map(|pp| pp.as_u32()),
                name: info.name.clone(),
                exe: info.exe.clone(),
                argv: info.argv.clone(),
                cwd: info.cwd.clone(),
                env: info.env.clone(),
                is_own_user: info.is_own_user,
                is_root_user: info.is_root_user,
                status: map_status(p.status()),
                cpu,
                mem: None,
            });
        }
        procs.sort_by_key(|p| p.key.pid);

        // Forget processes that are gone.
        let alive: HashSet<ProcKey> = procs.iter().map(|p| p.key).collect();
        self.statics.retain(|k, _| alive.contains(k));
        self.cpu_prev.retain(|k, _| alive.contains(k));

        let self_key = ProcKey {
            pid: self.self_pid.as_u32(),
            start_time: self
                .sys
                .process(self.self_pid)
                .map_or(0, |p| p.start_time()),
        };
        Snapshot {
            platform,
            taken_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            self_key,
            home: self.home.clone(),
            in_wsl: self.in_wsl,
            unreadable_count: count_unreadable(&procs),
            procs,
            system: SystemInfo {
                logical_cpus: self.logical_cpus,
                ..Default::default()
            },
        }
    }

    /// Expensive tier: reads memory for `tracked` and for cctree itself, and
    /// fills `snap.system`. CPU was already computed by [`Collector::snapshot`].
    pub fn fill_dynamic(&mut self, snap: &mut Snapshot, tracked: &HashSet<ProcKey>) {
        let self_key = snap.self_key;
        for p in &mut snap.procs {
            if !(tracked.contains(&p.key) || p.key == self_key) {
                continue;
            }
            p.mem = Some(if p.status == ProcStatus::Zombie {
                // Reading a zombie fails (on macOS even as "permission denied"),
                // which would be misreported. Its memory is simply gone.
                MemInfo::unavailable(MemUnavailable::Gone)
            } else {
                mem::read_mem(p.key.pid)
            });
        }

        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        snap.system = SystemInfo {
            ram_used: self.sys.used_memory(),
            ram_total: self.sys.total_memory(),
            swap_used: self.sys.used_swap(),
            swap_total: self.sys.total_swap(),
            // Whole machine, 0..=100 on every platform (not per core).
            cpu_pct: (!self.sys.cpus().is_empty()).then(|| self.sys.global_cpu_usage()),
            logical_cpus: self.logical_cpus,
        };
    }
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads the allowlisted environment and the cwd of `wanted` (pid, start time)
/// through a throwaway `System` that is dropped when this function returns, so
/// no full environment outlives the call. A pid whose start time no longer
/// matches was reused by a different process and is skipped.
fn read_env_and_cwd(
    wanted: &[(SysPid, u64)],
) -> HashMap<SysPid, (BTreeMap<String, String>, Option<String>)> {
    if wanted.is_empty() {
        return HashMap::new();
    }
    let pids: Vec<SysPid> = wanted.iter().map(|(pid, _)| *pid).collect();
    let mut tmp = System::new();
    tmp.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        false,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_environ(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    wanted
        .iter()
        .filter_map(|(pid, start_time)| {
            let p = tmp.process(*pid)?;
            (p.start_time() == *start_time).then(|| {
                (
                    *pid,
                    (
                        env::extract_env(p.environ()),
                        p.cwd().map(|c| c.to_string_lossy().into_owned()),
                    ),
                )
            })
        })
        .collect()
}

fn is_own_user(p: &Process, own_uid: &Option<Uid>) -> bool {
    matches!((p.user_id(), own_uid), (Some(a), Some(b)) if a == b)
}

#[cfg(unix)]
fn is_root_uid(uid: &Uid) -> bool {
    **uid == 0
}

/// Windows: the well-known SYSTEM, LOCAL SERVICE and NETWORK SERVICE accounts.
#[cfg(windows)]
fn is_root_uid(uid: &Uid) -> bool {
    matches!(
        uid.to_string().as_str(),
        "S-1-5-18" | "S-1-5-19" | "S-1-5-20"
    )
}

fn map_status(s: ProcessStatus) -> ProcStatus {
    match s {
        ProcessStatus::Run => ProcStatus::Running,
        ProcessStatus::Sleep | ProcessStatus::Idle | ProcessStatus::UninterruptibleDiskSleep => {
            ProcStatus::Sleeping
        }
        ProcessStatus::Zombie => ProcStatus::Zombie,
        _ => ProcStatus::Other,
    }
}

/// Counts processes that look like node/npm/npx/bun/deno/claude/codex but whose
/// command line could not be read (spec 4.8). Not every unreadable process:
/// macOS always has hundreds of system processes it cannot read. Zombies are
/// skipped because their command line is empty by definition, not for lack of
/// permission.
fn count_unreadable(procs: &[RawProc]) -> usize {
    procs
        .iter()
        .filter(|p| p.argv.is_empty() && p.status != ProcStatus::Zombie && is_tool_name(&p.name))
        .count()
}

fn is_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    TOOL_NAMES.contains(&lower.strip_suffix(".exe").unwrap_or(&lower))
}

#[cfg(target_os = "linux")]
fn detect_wsl_here() -> bool {
    env::detect_wsl(
        std::env::var("WSL_DISTRO_NAME").ok().as_deref(),
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .as_deref(),
    )
}

/// WSL only exists on Linux; Windows and macOS are never "in WSL".
#[cfg(not(target_os = "linux"))]
fn detect_wsl_here() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{p, snap};
    use std::process::{Child, Command, Stdio};

    fn strings(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_argv_splits_a_rewritten_title_followed_by_environ() {
        // macOS: the "arguments" after the title are environment strings.
        let macos = strings(&[
            "npm exec chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222",
            "HOME=/Users/u",
            "LOGNAME=u",
            "PATH=/usr/bin:/bin",
            "CLAUDE_CODE_MESSAGING_TOKEN=secret",
            "_=/usr/bin/x",
            "EMPTY=",
        ]);
        assert_eq!(
            normalize_argv(macos),
            strings(&[
                "npm",
                "exec",
                "chrome-devtools-mcp@latest",
                "--browserUrl",
                "http://127.0.0.1:9222"
            ])
        );
        // Linux: the area after the title is empty strings.
        assert_eq!(
            normalize_argv(strings(&["npm exec @playwright/mcp@latest", "", ""])),
            strings(&["npm", "exec", "@playwright/mcp@latest"])
        );
        // A lone argv[0] with whitespace is split too.
        assert_eq!(normalize_argv(strings(&["a b"])), strings(&["a", "b"]));
        assert_eq!(normalize_argv(strings(&["a \t b"])), strings(&["a", "b"]));
    }

    #[test]
    fn normalize_argv_leaves_real_command_lines_alone() {
        for argv in [
            &["node", "/x/a.js", "--flag"][..],
            // argv[0] has a space, but the next argument is not environment-like.
            &[
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "--type=renderer",
            ],
            // Environment-like arguments, but argv[0] has no whitespace.
            &["env", "FOO=1", "node"],
            &["env", "FOO=1"],
            &["chrome-devtools-mcp", "HOME=/Users/u"],
            // Not NAME=: leading digit, no name, no `=`.
            &["a b", "1X=1"],
            &["a b", "=x"],
            &["a b", "plain"],
            &["a b", "-x=1"],
            &["a b", "HOME=/u", "plain"],
            &["only-one-word"],
            &[""],
            &["   "],
        ] {
            assert_eq!(normalize_argv(strings(argv)), strings(argv), "{argv:?}");
        }
        assert_eq!(normalize_argv(Vec::new()), Vec::<String>::new());
    }

    /// Kills the child when dropped, so a failing assertion cannot leave a
    /// busy loop running behind the test run.
    struct Guard(Child);

    impl Guard {
        fn id(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    fn sleeper_command() -> Command {
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("ping");
            c.args(["-n", "5", "127.0.0.1"]);
            c
        };
        cmd.stdout(Stdio::null());
        cmd
    }

    /// A child that stays alive long enough to be collected.
    fn spawn_sleeper() -> Guard {
        Guard(sleeper_command().spawn().expect("spawn sleeper"))
    }

    fn by_pid(s: &Snapshot, pid: u32) -> Option<&RawProc> {
        s.procs.iter().find(|p| p.key.pid == pid)
    }

    #[test]
    fn cpu_is_per_machine_on_windows_only() {
        assert_eq!(normalize_cpu(400.0, Platform::Windows, 8), 50.0);
        assert_eq!(normalize_cpu(400.0, Platform::MacOs, 8), 400.0);
        assert_eq!(normalize_cpu(400.0, Platform::Linux, 8), 400.0);
        // No CPU count yet must not divide by zero.
        assert_eq!(normalize_cpu(40.0, Platform::Windows, 0), 40.0);
    }

    #[test]
    fn cpu_percent_is_cpu_time_over_wall_time() {
        let s = Duration::from_secs;
        assert_eq!(cpu_percent(1000, 3000, s(2)), 100.0); // one core, fully busy
        assert_eq!(cpu_percent(0, 4000, s(2)), 200.0); // two cores
        assert_eq!(cpu_percent(500, 500, s(2)), 0.0);
        assert_eq!(cpu_percent(900, 100, s(2)), 0.0); // clock went backwards: no panic
        assert_eq!(cpu_percent(0, 10, Duration::ZERO), 0.0);
    }

    #[test]
    fn snapshot_contains_self_and_spawned_child() {
        let child = spawn_sleeper();
        let mut c = Collector::new();
        let s = c.snapshot();
        let me = s.get(s.self_key).expect("self present");
        assert!(me.is_own_user && !me.argv.is_empty());
        let kid = s
            .procs
            .iter()
            .find(|p| p.key.pid == child.id())
            .expect("child present");
        assert_eq!(kid.ppid, Some(std::process::id()));
    }

    #[test]
    fn collector_tolerates_vanished_process() {
        let mut child = spawn_sleeper();
        let mut c = Collector::new();
        let mut s = c.snapshot();
        let key = s
            .procs
            .iter()
            .find(|p| p.key.pid == child.id())
            .unwrap()
            .key;
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        c.fill_dynamic(&mut s, &HashSet::from([key])); // must not panic
        let p = s.get(key).unwrap();
        assert!(p.mem.as_ref().is_none_or(|m| m.private.is_none()));
    }

    #[test]
    fn busy_child_reports_real_cpu() {
        // Pins spec 4.9: CPU must not read ~0 because of the two-tier collection.
        #[cfg(unix)]
        let child = Guard(
            Command::new("sh")
                .args(["-c", "while :; do :; done"])
                .spawn()
                .unwrap(),
        );
        #[cfg(windows)]
        let child = Guard(
            Command::new("powershell")
                .args(["-NoProfile", "-Command", "while($true){}"])
                .spawn()
                .unwrap(),
        );
        let mut c = Collector::new();
        c.snapshot();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let s = c.snapshot();
        let busy = s.procs.iter().find(|p| p.key.pid == child.id()).unwrap();
        assert!(
            busy.cpu.unwrap()
                > 20.0
                    / if cfg!(windows) {
                        s.system.logical_cpus as f32
                    } else {
                        1.0
                    },
            "{:?}",
            busy.cpu
        );
    }

    #[test]
    fn main_system_never_holds_environ() {
        // Pins spec 4.3: full environments never stay in memory.
        let child = spawn_sleeper();
        let mut c = Collector::new();
        let s = c.snapshot();
        assert!(s.procs.iter().any(|p| p.key.pid == child.id()));
        assert!(
            c.sys
                .process(sysinfo::Pid::from_u32(child.id()))
                .unwrap()
                .environ()
                .is_empty()
        );
    }

    #[test]
    fn first_cpu_reading_is_none_then_some() {
        let child = spawn_sleeper();
        let mut c = Collector::new();
        let first = c.snapshot();
        assert_eq!(by_pid(&first, child.id()).unwrap().cpu, None);
        let second = c.snapshot();
        assert!(by_pid(&second, child.id()).unwrap().cpu.is_some());
    }

    /// Not a test of its own: `child_env_keeps_only_allowlisted_names` runs this
    /// test binary again with `CCTREE_TEST_SLEEP` set, to get a long-lived child
    /// whose environment is readable. (macOS hides the environment of its own
    /// system binaries such as `/bin/sleep`, so they cannot be used for that.)
    #[test]
    fn sleeper_helper() {
        if std::env::var_os("CCTREE_TEST_SLEEP").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }

    #[test]
    fn child_env_keeps_only_allowlisted_names() {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "collect::tests::sleeper_helper"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // The parent may itself run under Claude Code: start from a clean slate.
        for name in crate::model::ENV_ALLOWLIST {
            cmd.env_remove(name);
        }
        cmd.env("CCTREE_TEST_SLEEP", "1")
            .env("CLAUDECODE", "1")
            .env("OPENAI_API_KEY", "sk-test-secret");
        let child = Guard(cmd.spawn().expect("spawn test binary"));
        let mut c = Collector::new();
        let s = c.snapshot();
        let kid = by_pid(&s, child.id()).unwrap();
        assert_eq!(
            kid.env,
            BTreeMap::from([("CLAUDECODE".to_string(), "1".to_string())])
        );
        assert!(kid.cwd.is_some());
    }

    #[test]
    fn fill_dynamic_reads_self_memory_and_system_totals() {
        let mut c = Collector::new();
        let mut s = c.snapshot();
        c.fill_dynamic(&mut s, &HashSet::new());
        let me = s.get(s.self_key).unwrap();
        assert!(me.mem.unwrap().private.is_some_and(|b| b > 0));
        assert!(
            s.procs
                .iter()
                .filter(|p| p.key != s.self_key)
                .all(|p| p.mem.is_none())
        );
        assert!(s.system.ram_total > 0 && s.system.logical_cpus > 0);
    }

    #[test]
    fn fill_dynamic_marks_zombie_memory_gone_without_reading_it() {
        // The pid does not exist: a real read would not say `Gone` for a zombie.
        let z = p(4_000_000, 1, 5, &["x"]).zombie();
        let key = z.key;
        let mut s = snap(Platform::current(), vec![z]);
        Collector::new().fill_dynamic(&mut s, &HashSet::from([key]));
        assert_eq!(
            s.get(key).unwrap().mem,
            Some(MemInfo::unavailable(MemUnavailable::Gone))
        );
    }

    #[test]
    fn status_mapping() {
        assert_eq!(map_status(ProcessStatus::Run), ProcStatus::Running);
        for s in [
            ProcessStatus::Sleep,
            ProcessStatus::Idle,
            ProcessStatus::UninterruptibleDiskSleep,
        ] {
            assert_eq!(map_status(s), ProcStatus::Sleeping, "{s:?}");
        }
        assert_eq!(map_status(ProcessStatus::Zombie), ProcStatus::Zombie);
        assert_eq!(map_status(ProcessStatus::Stop), ProcStatus::Other);
        assert_eq!(map_status(ProcessStatus::Unknown(9)), ProcStatus::Other);
    }

    #[test]
    fn unreadable_count_only_counts_tool_like_names_without_cmd() {
        let empty = |pid, name: &str| {
            let mut r = p(pid, 1, 1, &[name]).name(name);
            r.argv.clear();
            r
        };
        let procs = vec![
            empty(1, "node"),
            empty(2, "Claude.exe"),
            empty(3, "launchd"),
            empty(4, "codex").zombie(),
            p(5, 1, 1, &["node", "x.js"]),
        ];
        assert_eq!(count_unreadable(&procs), 2);
    }
}
