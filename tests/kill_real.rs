//! Ends real process trees with the real operating-system calls.

use std::process::{Child, Command};
use std::time::Duration;

/// Kills the child (and, on unix, everything in its process group) when
/// dropped, so a failing assertion cannot leave processes running behind the
/// test run.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: `killpg` takes two plain integers. The child leads its own
        // group (`process_group(0)`) and is not reaped before this point, so
        // the group id is still its PID.
        unsafe {
            libc::killpg(self.0.id() as libc::pid_t, libc::SIGKILL);
        }
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[cfg(unix)]
#[test]
fn kills_a_real_tree_gracefully() {
    use std::os::unix::process::CommandExt;

    let child = Guard(
        Command::new("sh")
            .args(["-c", "sleep 60 & sleep 60 & wait"])
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(300));
    let mut col = cctree::collect::Collector::new();
    let s = col.snapshot();
    let plan = cctree::kill::plan_for_pid_tree(&s, child.0.id());
    assert_eq!(plan.targets.len(), 3);
    let r = cctree::kill::execute(
        &plan,
        &cctree::kill::OsControl,
        Duration::from_secs(3),
        Duration::from_millis(50),
        &mut |_, _| {},
    );
    // `sh` leaves by itself once both sleeps are gone, so some processes may
    // count as already gone; a zombie that is not reaped yet has ended too.
    assert_eq!(r.graceful + r.forced + r.already_gone, 3);
    assert!(r.failed.is_empty());
    assert_eq!(
        r.forced, 0,
        "SIGTERM should end all of them, not wait for the timeout and force"
    );
}

#[cfg(windows)]
#[test]
fn kills_a_real_tree_forcefully() {
    let child = Guard(
        Command::new("cmd")
            .args(["/c", "ping -n 60 127.0.0.1 >nul"])
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    let mut col = cctree::collect::Collector::new();
    let s = col.snapshot();
    let plan = cctree::kill::plan_for_pid_tree(&s, child.0.id());
    // cmd.exe + PING.EXE (and maybe conhost.exe).
    assert!(plan.force_only && plan.targets.len() >= 2);
    let r = cctree::kill::execute(
        &plan,
        &cctree::kill::OsControl,
        Duration::from_secs(3),
        Duration::from_millis(50),
        &mut |_, _| {},
    );
    assert_eq!(r.forced, plan.targets.len());
    assert!(r.failed.is_empty());
}
