//! The real [`ProcessControl`]: asks the operating system.
//!
//! - **unix**: `kill(2)` with SIGTERM or SIGKILL. `EPERM` is
//!   [`KillError::PermissionDenied`], `ESRCH` is [`KillError::NotFound`].
//! - **Windows**: `TerminateProcess`. There is no polite signal there, so
//!   plans are `force_only` and [`ProcessControl::terminate`] would do the
//!   same as [`ProcessControl::force`]. A process that has exited but is still
//!   held open by someone (its parent) answers `TerminateProcess` with
//!   "access denied"; that is [`KillError::NotFound`], not a permission error.

use sysinfo::{Pid as SysPid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

use super::{KillError, ProcState, ProcessControl};
use crate::model::{Pid, ProcKey};

/// Signals processes through the operating system.
pub struct OsControl;

impl ProcessControl for OsControl {
    /// Reads just this PID with a fresh `System`, so nothing is cached between
    /// calls. A zombie is [`ProcState::Gone`]: a process that was just ended
    /// stays a zombie until its parent collects it, and counting that as still
    /// running would wait out the whole grace period (spec 4.7, point 5).
    fn state(&self, key: ProcKey) -> ProcState {
        let pid = SysPid::from_u32(key.pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        match sys.process(pid) {
            None => ProcState::Gone,
            Some(p) if p.status() == ProcessStatus::Zombie => ProcState::Gone,
            Some(p) if p.start_time() == key.start_time => ProcState::Same,
            Some(_) => ProcState::Reused,
        }
    }

    fn terminate(&self, pid: Pid) -> Result<(), KillError> {
        sys::terminate(pid)
    }

    fn force(&self, pid: Pid) -> Result<(), KillError> {
        sys::force(pid)
    }
}

#[cfg(unix)]
mod sys {
    use super::{KillError, Pid};

    pub fn terminate(pid: Pid) -> Result<(), KillError> {
        signal(pid, libc::SIGTERM)
    }

    pub fn force(pid: Pid) -> Result<(), KillError> {
        signal(pid, libc::SIGKILL)
    }

    fn signal(pid: Pid, sig: libc::c_int) -> Result<(), KillError> {
        // `kill(2)` with 0 or a negative number signals a whole group, so
        // only a real PID may get through.
        let target = libc::pid_t::try_from(pid)
            .ok()
            .filter(|&p| p > 0)
            .ok_or_else(|| KillError::Other(format!("invalid pid {pid}")))?;
        // SAFETY: `kill` takes two plain integers and touches no memory of ours.
        if unsafe { libc::kill(target, sig) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        Err(match err.raw_os_error() {
            Some(libc::EPERM) => KillError::PermissionDenied,
            Some(libc::ESRCH) => KillError::NotFound,
            _ => KillError::Other(err.to_string()),
        })
    }
}

#[cfg(windows)]
mod sys {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };
    use windows::core::HRESULT;

    use super::{KillError, Pid};

    /// An open process handle. Dropping it closes the handle.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle this struct owns (from
            // `OpenProcess`) and `Drop` runs once, so it is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    impl Handle {
        /// True when the process has exited (it stays openable while anyone
        /// holds a handle to it). If the exit code cannot be read, assume it
        /// is still running.
        fn has_exited(&self) -> bool {
            let mut code = 0u32;
            // SAFETY: `self.0` is a live handle with query access and `code`
            // is a valid out-pointer for one `u32`.
            let read = unsafe { GetExitCodeProcess(self.0, &mut code) };
            read.is_ok() && code != STILL_ACTIVE.0 as u32
        }
    }

    /// There is no polite signal on Windows.
    pub fn terminate(pid: Pid) -> Result<(), KillError> {
        force(pid)
    }

    pub fn force(pid: Pid) -> Result<(), KillError> {
        // SAFETY: `OpenProcess` takes plain values and returns a new handle
        // (or an error); it touches no memory of ours.
        let opened = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        };
        let handle = Handle(opened.map_err(|e| kill_error(&e))?);
        // SAFETY: `handle.0` is a live handle with terminate access.
        unsafe { TerminateProcess(handle.0, 1) }.map_err(|e| {
            if handle.has_exited() {
                KillError::NotFound
            } else {
                kill_error(&e)
            }
        })
    }

    fn kill_error(e: &windows::core::Error) -> KillError {
        if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
            KillError::PermissionDenied
        } else if e.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) {
            // No process has this pid.
            KillError::NotFound
        } else {
            KillError::Other(e.message())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};

    use super::*;

    /// Kills and reaps the child when dropped, whatever the test did.
    struct Guard(Child);

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    fn start_time_of(pid: Pid) -> u64 {
        let pid = SysPid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        sys.process(pid).expect("process is running").start_time()
    }

    #[cfg(unix)]
    fn sleeper() -> Guard {
        Guard(
            Command::new("sleep")
                .arg("60")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// A child that has already ended and been waited for. It is returned, not
    /// dropped, because on Windows the process stays openable while its handle
    /// is held.
    fn ended_child() -> Guard {
        #[cfg(unix)]
        let mut c = Command::new("true");
        #[cfg(windows)]
        let mut c = {
            let mut c = Command::new("cmd");
            c.args(["/c", "exit"]);
            c
        };
        let mut child = c.spawn().unwrap();
        child.wait().unwrap();
        Guard(child)
    }

    #[test]
    fn state_tells_same_reused_and_gone() {
        let me = std::process::id();
        let key = ProcKey {
            pid: me,
            start_time: start_time_of(me),
        };
        assert_eq!(OsControl.state(key), ProcState::Same);
        let later = ProcKey {
            start_time: key.start_time + 1,
            ..key
        };
        assert_eq!(OsControl.state(later), ProcState::Reused);
        // Far above any PID the system hands out (and not a multiple of 4).
        let gone = ProcKey {
            pid: 0x3FFF_FFF3,
            ..key
        };
        assert_eq!(OsControl.state(gone), ProcState::Gone);
    }

    #[cfg(unix)]
    #[test]
    fn an_ended_child_that_is_not_collected_yet_is_gone() {
        use std::time::{Duration, Instant};

        // `kill` does not collect the child, so until `wait` it is a zombie.
        let mut child = sleeper();
        let key = ProcKey {
            pid: child.0.id(),
            start_time: start_time_of(child.0.id()),
        };
        assert_eq!(OsControl.state(key), ProcState::Same);
        child.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while OsControl.state(key) == ProcState::Same && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(OsControl.state(key), ProcState::Gone);
    }

    #[test]
    fn signalling_an_ended_process_is_not_found() {
        let ended = ended_child();
        assert_eq!(OsControl.terminate(ended.0.id()), Err(KillError::NotFound));
        assert_eq!(OsControl.force(ended.0.id()), Err(KillError::NotFound));
    }
}
