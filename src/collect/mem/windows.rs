//! Windows memory reader: `K32GetProcessMemoryInfo` with
//! `PROCESS_MEMORY_COUNTERS_EX2`, which adds the private working set that
//! Task Manager shows as "Memory". It needs Windows 10 1709+ (plus a 2023
//! update); older systems fall back to `PROCESS_MEMORY_COUNTERS_EX`.

use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE,
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
            Err(e) if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) => {
                Err(MemUnavailable::PermissionDenied)
            }
            // No process has this pid.
            Err(e) if e.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
                Err(MemUnavailable::Gone)
            }
            Err(_) => Err(MemUnavailable::Unsupported),
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
    // Pre-fill that field with a marker: if a system accepts the larger size
    // but fills only the older prefix, the marker survives and the value is
    // treated as unknown instead of as a real 0.
    let mut ex2 = ProcessMemoryCountersEx2 {
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
        private_working_set_size: usize::MAX,
        shared_commit_usage: 0,
    };
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
        let private_ws = (ex2.private_working_set_size != usize::MAX)
            .then_some(ex2.private_working_set_size as u64);
        return from_windows(
            ex2.private_usage as u64,
            ex2.working_set_size as u64,
            private_ws,
        );
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
        Err(e) if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) => {
            MemInfo::unavailable(MemUnavailable::PermissionDenied)
        }
        // The process may have exited since the check above.
        Err(_) if process.has_exited() => MemInfo::unavailable(MemUnavailable::Gone),
        Err(_) => MemInfo::unavailable(MemUnavailable::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

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
