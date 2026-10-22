//! Windows memory reader: `K32GetProcessMemoryInfo` with
//! `PROCESS_MEMORY_COUNTERS_EX2`, which adds the private working set that
//! Task Manager shows as "Memory". It needs Windows 10 1709+ (plus a 2023
//! update); older systems fall back to `PROCESS_MEMORY_COUNTERS_EX`.

use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE, WIN32_ERROR,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::HRESULT;

use super::from_windows;
use crate::model::{MemInfo, MemUnavailable, Pid};

/// `PROCESS_MEMORY_COUNTERS_EX2` from `<psapi.h>`: the fields up to
/// `PrivateUsage` are the same as in `PROCESS_MEMORY_COUNTERS_EX`. Most
/// fields exist only to keep the offsets right.
#[allow(dead_code)]
#[repr(C)]
struct ProcessMemoryCountersEx2 {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
    private_working_set_size: usize,
    shared_commit_usage: u64,
}

/// What `private_working_set_size` is pre-filled with before the call. If the
/// system accepts the larger size but fills only the older prefix, this value
/// survives and means "unknown", which a real 0 must not be mistaken for.
const PRIVATE_WS_UNSET: usize = usize::MAX;

impl ProcessMemoryCountersEx2 {
    /// A buffer ready for `K32GetProcessMemoryInfo`: `cb` set, every counter
    /// zero, and the private working set marked as not yet filled in.
    fn request() -> ProcessMemoryCountersEx2 {
        ProcessMemoryCountersEx2 {
            cb: size_of::<ProcessMemoryCountersEx2>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
            private_usage: 0,
            private_working_set_size: PRIVATE_WS_UNSET,
            shared_commit_usage: 0,
        }
    }
}

/// Maps counters the system filled in. A private working set that still
/// holds the marker was not provided, so the "in RAM" figure is unknown.
fn from_ex2(counters: &ProcessMemoryCountersEx2) -> MemInfo {
    let private_ws = (counters.private_working_set_size != PRIVATE_WS_UNSET)
        .then_some(counters.private_working_set_size as u64);
    from_windows(
        counters.private_usage as u64,
        counters.working_set_size as u64,
        private_ws,
    )
}

fn is_win32_error(code: HRESULT, error: WIN32_ERROR) -> bool {
    code == HRESULT::from_win32(error.0)
}

/// Why `OpenProcess` failed, as a reason to show.
fn open_error_reason(code: HRESULT) -> MemUnavailable {
    if is_win32_error(code, ERROR_ACCESS_DENIED) {
        MemUnavailable::PermissionDenied
    } else if is_win32_error(code, ERROR_INVALID_PARAMETER) {
        // No process has this pid.
        MemUnavailable::Gone
    } else {
        MemUnavailable::Unsupported
    }
}

/// Why the counters call failed. `exited` says whether the process has
/// exited since it was opened.
fn counters_error_reason(code: HRESULT, exited: bool) -> MemUnavailable {
    if is_win32_error(code, ERROR_ACCESS_DENIED) {
        MemUnavailable::PermissionDenied
    } else if exited {
        MemUnavailable::Gone
    } else {
        MemUnavailable::Unsupported
    }
}

/// An open process handle. Dropping it closes the handle, so no error path
/// can leak one.
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// Opens `pid` with the least access that memory counters need (works
    /// for a normal user on their own processes). The error is the reason
    /// to report.
    fn open(pid: Pid) -> Result<ProcessHandle, MemUnavailable> {
        // SAFETY: `OpenProcess` takes plain values and returns a new handle
        // (or an error); it touches no memory of ours.
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => Ok(ProcessHandle(handle)),
            Err(e) => Err(open_error_reason(e.code())),
        }
    }

    /// True when the process has exited. A process stays openable while any
    /// handle to it is open, so a successful `OpenProcess` does not prove it
    /// is alive. If the exit code cannot be read, assume it is running.
    fn has_exited(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: `self.0` is a live handle with query access, and `code`
        // is a valid out-pointer for one `u32`.
        let read = unsafe { GetExitCodeProcess(self.0, &mut code) };
        read.is_ok() && code != STILL_ACTIVE.0 as u32
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a handle this struct owns (from `OpenProcess`)
        // and `Drop` runs once, so it is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn read(pid: Pid) -> MemInfo {
    // Pid 0 is the System Idle Process: it always exists, but `OpenProcess`
    // rejects it like an unknown pid, which would be reported as "gone".
    if pid == 0 {
        return MemInfo::unavailable(MemUnavailable::Unsupported);
    }
    let process = match ProcessHandle::open(pid) {
        Ok(process) => process,
        Err(reason) => return MemInfo::unavailable(reason),
    };
    if process.has_exited() {
        return MemInfo::unavailable(MemUnavailable::Gone);
    }

    // Windows 10 1709+: the extended counters, with the private working set.
    let mut ex2 = ProcessMemoryCountersEx2::request();
    // SAFETY: `process.0` is a live handle with query access. `ex2` is a
    // writable `repr(C)` buffer of exactly `cb` bytes that starts with the
    // `PROCESS_MEMORY_COUNTERS` header, and the call writes at most `cb`.
    let ex2_ok = unsafe {
        K32GetProcessMemoryInfo(
            process.0,
            (&raw mut ex2).cast(),
            size_of::<ProcessMemoryCountersEx2>() as u32,
        )
    }
    .as_bool();
    if ex2_ok {
        return from_ex2(&ex2);
    }

    // Older Windows rejects the larger size: ask for the older layout, which
    // still has Private Bytes but no private working set.
    let mut ex = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: same as above, with `PROCESS_MEMORY_COUNTERS_EX`, a `repr(C)`
    // struct that starts with the `PROCESS_MEMORY_COUNTERS` header.
    let result = unsafe {
        K32GetProcessMemoryInfo(
            process.0,
            (&raw mut ex).cast(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .ok();
    match result {
        Ok(()) => from_windows(ex.PrivateUsage as u64, ex.WorkingSetSize as u64, None),
        // The process may have exited since the check above.
        Err(e) => MemInfo::unavailable(counters_error_reason(e.code(), process.has_exited())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::mem::{offset_of, size_of};

    use ::windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX2;
    use ::windows::Win32::System::Threading::GetCurrentProcess;

    use super::*;
    use crate::collect::mem::read_mem;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn ex2_layout_is_96_bytes_on_x64() {
        assert_eq!(std::mem::size_of::<ProcessMemoryCountersEx2>(), 96);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn ex2_field_offsets_match_psapi_h() {
        assert_eq!(offset_of!(ProcessMemoryCountersEx2, working_set_size), 16);
        assert_eq!(offset_of!(ProcessMemoryCountersEx2, private_usage), 72);
        assert_eq!(
            offset_of!(ProcessMemoryCountersEx2, private_working_set_size),
            80
        );
    }

    #[test]
    fn ex2_extends_ex_with_the_same_prefix() {
        // The fallback relies on EX being a prefix of EX2.
        assert!(size_of::<PROCESS_MEMORY_COUNTERS_EX>() < size_of::<ProcessMemoryCountersEx2>());
        assert_eq!(
            offset_of!(PROCESS_MEMORY_COUNTERS_EX, PrivateUsage),
            offset_of!(ProcessMemoryCountersEx2, private_usage)
        );
    }

    #[test]
    fn reads_own_process() {
        let m = read_mem(std::process::id());
        assert!(m.private.unwrap() > 0);
        assert!(m.in_ram.is_some() || m.unavailable == Some(MemUnavailable::OsTooOld));
    }

    #[test]
    fn ex2_request_sets_cb_and_marks_private_ws_unset() {
        let r = ProcessMemoryCountersEx2::request();
        assert_eq!(r.cb as usize, size_of::<ProcessMemoryCountersEx2>());
        assert_eq!(r.private_working_set_size, PRIVATE_WS_UNSET);
        assert_eq!((r.private_usage, r.working_set_size), (0, 0));
    }

    #[test]
    fn from_ex2_with_untouched_marker_has_no_private_ws() {
        let counters = ProcessMemoryCountersEx2 {
            private_usage: 500,
            working_set_size: 400,
            ..ProcessMemoryCountersEx2::request()
        };
        let m = from_ex2(&counters);
        assert_eq!(
            (m.private, m.in_ram, m.swapped_out, m.shared),
            (Some(500), None, None, None)
        );
        assert_eq!(m.unavailable, Some(MemUnavailable::OsTooOld));
    }

    #[test]
    fn from_ex2_with_real_private_ws_fills_in_ram() {
        let counters = ProcessMemoryCountersEx2 {
            private_usage: 500,
            working_set_size: 400,
            private_working_set_size: 300,
            ..ProcessMemoryCountersEx2::request()
        };
        let m = from_ex2(&counters);
        assert_eq!(
            (
                m.private,
                m.in_ram,
                m.swapped_out,
                m.shared,
                m.os_tool_value
            ),
            (Some(500), Some(300), Some(200), Some(100), Some(300))
        );
        assert!(m.unavailable.is_none());
    }

    #[test]
    fn from_ex2_keeps_a_real_zero_private_ws() {
        // 0 is a value the system wrote; only the marker means "unknown".
        let counters = ProcessMemoryCountersEx2 {
            private_usage: 500,
            working_set_size: 400,
            private_working_set_size: 0,
            ..ProcessMemoryCountersEx2::request()
        };
        let m = from_ex2(&counters);
        assert_eq!(
            (m.private, m.in_ram, m.swapped_out, m.shared),
            (Some(500), Some(0), Some(500), Some(400))
        );
        assert!(m.unavailable.is_none());
    }

    #[test]
    fn own_process_uses_ex2_when_the_os_supports_it() {
        // Ask the OS directly, with the windows crate's own EX2 type, whether
        // it fills in the private working set for this process.
        let mut probe = PROCESS_MEMORY_COUNTERS_EX2 {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            PrivateWorkingSetSize: PRIVATE_WS_UNSET,
            ..Default::default()
        };
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always
        // valid for this process; `probe` is a writable `repr(C)` buffer of
        // exactly `cb` bytes that starts with the counters header.
        let supported = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut probe).cast(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            )
        }
        .as_bool()
            && probe.PrivateWorkingSetSize != PRIVATE_WS_UNSET;

        let m = read_mem(std::process::id());
        // `io::stderr()` bypasses the test harness's output capture, so the
        // CI log shows which path ran even when the test passes.
        let _ = writeln!(
            std::io::stderr(),
            "windows mem: EX2 {} on this OS (probe PrivateWorkingSetSize = {}); read_mem: in_ram = {:?}, unavailable = {:?}",
            if supported {
                "supported"
            } else {
                "NOT supported"
            },
            probe.PrivateWorkingSetSize,
            m.in_ram,
            m.unavailable,
        );
        if supported {
            assert!(m.unavailable.is_none(), "EX2 works but read_mem fell back");
            assert!(m.in_ram.is_some() && m.shared.is_some());
            assert_eq!(m.os_tool_value, m.in_ram);
        } else {
            assert_eq!(m.unavailable, Some(MemUnavailable::OsTooOld));
        }
    }

    #[test]
    fn open_error_reason_maps_win32_errors() {
        let code = |e: WIN32_ERROR| HRESULT::from_win32(e.0);
        assert_eq!(
            open_error_reason(code(ERROR_ACCESS_DENIED)),
            MemUnavailable::PermissionDenied
        );
        assert_eq!(
            open_error_reason(code(ERROR_INVALID_PARAMETER)),
            MemUnavailable::Gone
        );
        // ERROR_NOT_ENOUGH_MEMORY (8) is neither: no claim about the process.
        assert_eq!(
            open_error_reason(HRESULT::from_win32(8)),
            MemUnavailable::Unsupported
        );
    }

    #[test]
    fn counters_error_reason_maps_win32_errors() {
        let denied = HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
        let other = HRESULT::from_win32(ERROR_INVALID_PARAMETER.0);
        assert_eq!(
            counters_error_reason(denied, false),
            MemUnavailable::PermissionDenied
        );
        assert_eq!(
            counters_error_reason(denied, true),
            MemUnavailable::PermissionDenied
        );
        assert_eq!(counters_error_reason(other, true), MemUnavailable::Gone);
        assert_eq!(
            counters_error_reason(other, false),
            MemUnavailable::Unsupported
        );
    }

    #[test]
    fn nonexistent_pid_reports_gone_from_the_real_open_error() {
        // Pids are multiples of 4, so neither of these can exist. Nothing
        // holds a handle here, so this goes through `OpenProcess` failing
        // with ERROR_INVALID_PARAMETER, not through the exit-code check.
        for pid in [3, 0xFFFF_FFF3] {
            assert_eq!(
                read_mem(pid).unavailable,
                Some(MemUnavailable::Gone),
                "pid {pid:#x}"
            );
        }
    }

    #[test]
    fn system_idle_process_is_unsupported() {
        assert_eq!(read_mem(0).unavailable, Some(MemUnavailable::Unsupported));
    }

    #[test]
    fn read_mem_of_exited_pid_reports_gone() {
        let mut c = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert_eq!(read_mem(pid).unavailable, Some(MemUnavailable::Gone));
    }
}
