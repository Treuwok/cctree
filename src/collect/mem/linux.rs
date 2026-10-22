//! Linux memory reader: `/proc/<pid>/status`.

use std::fs;
use std::io::ErrorKind;

use super::from_linux_status_bytes;
use crate::model::{MemInfo, MemUnavailable, Pid};

pub fn read(pid: Pid) -> MemInfo {
    match fs::read(format!("/proc/{pid}/status")) {
        Ok(bytes) => from_linux_status_bytes(&bytes),
        Err(e) => MemInfo::unavailable(match e.kind() {
            ErrorKind::NotFound => MemUnavailable::Gone,
            ErrorKind::PermissionDenied => MemUnavailable::PermissionDenied,
            // The process can exit between open and read, which gives ESRCH.
            _ if e.raw_os_error() == Some(libc::ESRCH) => MemUnavailable::Gone,
            _ => MemUnavailable::Unsupported,
        }),
    }
}
