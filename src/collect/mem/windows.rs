//! Windows memory reader (stub for the RED step).

use crate::model::{MemInfo, MemUnavailable, Pid};

#[allow(dead_code)]
#[repr(C)]
struct ProcessMemoryCountersEx2 {
    cb: u32,
}

pub(crate) fn read(_pid: Pid) -> MemInfo {
    MemInfo::unavailable(MemUnavailable::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::mem::read_mem;

    #[test]
    fn ex2_layout_is_96_bytes_on_x64() {
        assert_eq!(std::mem::size_of::<ProcessMemoryCountersEx2>(), 96);
    }

    #[test]
    fn reads_own_process() {
        let m = read_mem(std::process::id());
        assert!(m.private.unwrap() > 0);
        assert!(m.in_ram.is_some() || m.unavailable == Some(MemUnavailable::OsTooOld));
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
