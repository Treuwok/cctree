//! macOS memory reader: Mach `task_info(TASK_VM_INFO)` through a task *name*
//! port, which a normal user may obtain for their own processes.

use std::io;
use std::mem::size_of;

use super::from_macos;
use crate::model::{MemInfo, MemUnavailable, Pid};

/// `TASK_VM_INFO` flavor from `<mach/task_info.h>`.
const TASK_VM_INFO: u32 = 22;
const KERN_SUCCESS: i32 = 0;

// These live in libSystem, which std already links. `mach_port_t` is `u32`,
// `kern_return_t` and `pid_t` are `i32`, `task_info_t` is `integer_t *`.
unsafe extern "C" {
    static mach_task_self_: u32;
    fn task_name_for_pid(target_tport: u32, pid: i32, name: *mut u32) -> i32;
    fn task_info(target_task: u32, flavor: u32, info_out: *mut i32, count: *mut u32) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
}

/// `struct task_vm_info` from `<mach/task_info.h>`, cut after `phys_footprint`
/// (the "rev1" prefix, macOS 10.11+). The header packs it to 4 bytes, so the
/// `u64` fields sit at 4-byte offsets. Never take a reference to a field;
/// copy it out. Most fields exist only to keep the offsets right.
#[allow(dead_code)]
#[repr(C, packed(4))]
#[derive(Default)]
struct TaskVmInfoRev1 {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
}

/// A send right to a task name port. Dropping it gives the right back, so no
/// error path can leak a port.
struct NamePort(u32);

impl NamePort {
    /// Asks for the name port of `pid`. A normal user gets one only for their
    /// own processes; `None` covers "no such process" and "not allowed".
    fn for_pid(pid: i32) -> Option<NamePort> {
        let mut port = 0u32;
        // SAFETY: `mach_task_self_` is initialised by libSystem before `main`
        // and only read here; `port` is a valid out-pointer for one `u32`.
        let kr = unsafe { task_name_for_pid(mach_task_self_, pid, &mut port) };
        (kr == KERN_SUCCESS).then_some(NamePort(port))
    }
}

impl Drop for NamePort {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a right this task owns (from `task_name_for_pid`)
        // and `Drop` runs once, so it is released exactly once.
        unsafe {
            mach_port_deallocate(mach_task_self_, self.0);
        }
    }
}

/// True when signal 0 says no process has this pid (`ESRCH`). Other failures
/// (`EPERM`) and success both mean it exists.
fn pid_is_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission; nothing is
    // ever delivered, whichever process(es) `pid` selects. `pid` came from a
    // `u32`, so it is never negative (no "all processes" -1 or group < -1).
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

pub(crate) fn read(pid: Pid) -> MemInfo {
    // Real pids are far below `i32::MAX`; a larger value cannot exist.
    let Ok(raw_pid) = i32::try_from(pid) else {
        return MemInfo::unavailable(MemUnavailable::Gone);
    };
    let Some(port) = NamePort::for_pid(raw_pid) else {
        return MemInfo::unavailable(if pid_is_gone(raw_pid) {
            MemUnavailable::Gone
        } else {
            MemUnavailable::PermissionDenied
        });
    };
    let mut info = TaskVmInfoRev1::default();
    let wanted = (size_of::<TaskVmInfoRev1>() / size_of::<i32>()) as u32;
    let mut count = wanted;
    // SAFETY: `port` is a live task name port, and `info` is a writable
    // buffer of exactly `count` 32-bit words with 4-byte alignment, which is
    // what `task_info` requires of `integer_t *`.
    let kr = unsafe {
        task_info(
            port.0,
            TASK_VM_INFO,
            (&raw mut info).cast::<i32>(),
            &mut count,
        )
    };
    if kr != KERN_SUCCESS {
        // The process may have exited since we got its port.
        return MemInfo::unavailable(if pid_is_gone(raw_pid) {
            MemUnavailable::Gone
        } else {
            MemUnavailable::Unsupported
        });
    }
    if count < wanted {
        // The kernel filled fewer words: no `phys_footprint` before 10.11.
        return MemInfo::unavailable(MemUnavailable::OsTooOld);
    }
    // Copy out of the packed struct (no references to packed fields).
    let (footprint, resident, internal, compressed) = (
        info.phys_footprint,
        info.resident_size,
        info.internal,
        info.compressed,
    );
    from_macos(footprint, resident, internal, compressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::mem::read_mem;

    #[test]
    fn task_vm_info_rev1_layout() {
        assert_eq!(std::mem::size_of::<TaskVmInfoRev1>(), 152);
    }

    #[test]
    fn reads_own_process() {
        let m = read_mem(std::process::id());
        let fp = m.private.unwrap();
        assert!(
            fp > 0
                && m.os_tool_value == Some(fp)
                && m.in_ram.unwrap() > 0
                && fp >= m.in_ram.unwrap()
        );
    }

    #[test]
    fn root_process_is_permission_denied() {
        assert_eq!(
            read_mem(1).unavailable,
            Some(MemUnavailable::PermissionDenied)
        );
    }

    #[test]
    fn read_mem_of_exited_pid_reports_gone() {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert_eq!(read_mem(pid).unavailable, Some(MemUnavailable::Gone));
    }
}
