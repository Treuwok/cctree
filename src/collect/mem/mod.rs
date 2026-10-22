//! Plain-language memory model (spec 4.4): maps each OS's raw counters onto
//! the same few fields of [`MemInfo`].

use crate::model::{MemInfo, MemUnavailable, Pid};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// macOS: `phys_footprint` is the main figure (Activity Monitor's "Memory");
/// `internal` is what is in RAM and `compressed` is what the system squeezed.
pub fn from_macos(footprint: u64, resident: u64, internal: u64, compressed: u64) -> MemInfo {
    MemInfo {
        private: Some(footprint),
        in_ram: Some(internal),
        swapped_out: Some(compressed),
        shared: Some(resident.saturating_sub(internal)),
        os_tool_value: Some(footprint),
        swapped_is_estimate: false,
        unavailable: None,
    }
}

/// Windows: Private Bytes is the main figure. `private_ws` (private working
/// set) needs Windows 10 1709+; without it only the main figure is known.
pub fn from_windows(private_usage: u64, working_set: u64, private_ws: Option<u64>) -> MemInfo {
    match private_ws {
        Some(pws) => MemInfo {
            private: Some(private_usage),
            in_ram: Some(pws),
            // Windows has no per-process "compressed" counter, so this is the
            // pagefile + compressed + reserved-but-unused total: an estimate.
            swapped_out: Some(private_usage.saturating_sub(pws)),
            shared: Some(working_set.saturating_sub(pws)),
            os_tool_value: Some(pws),
            swapped_is_estimate: true,
            unavailable: None,
        },
        None => MemInfo {
            private: Some(private_usage),
            unavailable: Some(MemUnavailable::OsTooOld),
            ..Default::default()
        },
    }
}

/// Linux: parse the text of `/proc/<pid>/status`. Kernel threads and zombies
/// have no `VmRSS`, so they report `Unsupported`.
pub fn from_linux_status(text: &str) -> MemInfo {
    let (mut rss, mut anon, mut file, mut shmem, mut swap) = (None, None, None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "VmRSS" => &mut rss,
            "RssAnon" => &mut anon,
            "RssFile" => &mut file,
            "RssShmem" => &mut shmem,
            "VmSwap" => &mut swap,
            _ => continue,
        };
        *slot = parse_kb(value);
    }
    let Some(rss) = rss else {
        return MemInfo::unavailable(MemUnavailable::Unsupported);
    };
    let Some(anon) = anon else {
        // VmRSS without RssAnon: kernel older than 4.5 cannot split RSS.
        return MemInfo {
            os_tool_value: Some(rss),
            unavailable: Some(MemUnavailable::OsTooOld),
            ..Default::default()
        };
    };
    // Kernels built without swap omit VmSwap; that means nothing is swapped.
    let swap = swap.unwrap_or(0);
    MemInfo {
        private: Some(anon.saturating_add(swap)),
        in_ram: Some(anon),
        swapped_out: Some(swap),
        shared: Some(file.unwrap_or(0).saturating_add(shmem.unwrap_or(0))),
        os_tool_value: Some(rss),
        swapped_is_estimate: false,
        unavailable: None,
    }
}

/// Same as [`from_linux_status`] for the raw bytes of `/proc/<pid>/status`.
/// The `Name:` line is the raw `comm`, which need not be valid UTF-8; the
/// memory lines are plain ASCII, so a lossy conversion loses nothing we use.
pub fn from_linux_status_bytes(bytes: &[u8]) -> MemInfo {
    from_linux_status(&String::from_utf8_lossy(bytes))
}

/// Parses a `/proc/<pid>/status` value such as `"   12000 kB"` into bytes.
fn parse_kb(value: &str) -> Option<u64> {
    let kb: u64 = value.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kb.saturating_mul(1024))
}

/// Reads the memory of one process with the current platform's reader.
pub fn read_mem(pid: Pid) -> MemInfo {
    #[cfg(target_os = "linux")]
    {
        linux::read(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos::read(pid)
    }
    // Task 5 replaces this arm with the Windows reader.
    #[cfg(windows)]
    {
        let _ = pid;
        MemInfo::unavailable(MemUnavailable::Unsupported)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pid;
        MemInfo::unavailable(MemUnavailable::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_mapping() {
        let m = from_macos(63_600_000, 7_700_000, 100_000, 56_000_000);
        assert_eq!(
            (
                m.private,
                m.in_ram,
                m.swapped_out,
                m.shared,
                m.os_tool_value
            ),
            (
                Some(63_600_000),
                Some(100_000),
                Some(56_000_000),
                Some(7_600_000),
                Some(63_600_000)
            )
        );
        assert!(!m.swapped_is_estimate && m.unavailable.is_none());
    }

    #[test]
    fn windows_mapping() {
        let m = from_windows(500, 400, Some(300));
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
        assert!(m.swapped_is_estimate);
    }

    #[test]
    fn windows_mapping_old_os() {
        let m = from_windows(500, 400, None);
        assert_eq!(
            (m.private, m.in_ram, m.swapped_out, m.shared),
            (Some(500), None, None, None)
        );
        assert_eq!(m.unavailable, Some(MemUnavailable::OsTooOld));
    }

    #[test]
    fn linux_status_mapping() {
        let t = "Name:\tnode\nVmRSS:\t   12000 kB\nRssAnon:\t    8000 kB\nRssFile:\t    3000 kB\nRssShmem:\t    1000 kB\nVmSwap:\t     500 kB\n";
        let m = from_linux_status(t);
        assert_eq!(
            (
                m.private,
                m.in_ram,
                m.swapped_out,
                m.shared,
                m.os_tool_value
            ),
            (
                Some(8500 * 1024),
                Some(8000 * 1024),
                Some(500 * 1024),
                Some(4000 * 1024),
                Some(12000 * 1024)
            )
        );
    }

    #[test]
    fn linux_status_without_vmswap_counts_zero_swap() {
        let t = "VmRSS:\t 100 kB\nRssAnon:\t 60 kB\nRssFile:\t 40 kB\n";
        let m = from_linux_status(t);
        assert_eq!(
            (m.private, m.swapped_out, m.shared),
            (Some(60 * 1024), Some(0), Some(40 * 1024))
        );
    }

    #[test]
    fn linux_status_without_rssanon_is_old_kernel() {
        let m = from_linux_status("VmRSS:\t 100 kB\n");
        assert_eq!(
            (m.private, m.os_tool_value, m.unavailable),
            (None, Some(100 * 1024), Some(MemUnavailable::OsTooOld))
        );
    }

    #[test]
    fn linux_status_with_non_utf8_name_still_maps() {
        // `comm` is cut at 15 bytes, which can split a multibyte character.
        let mut t = b"Name:\t\xe4\xb8\n".to_vec();
        t.extend_from_slice(b"VmRSS:\t 12000 kB\nRssAnon:\t 8000 kB\nRssFile:\t 3000 kB\n");
        t.extend_from_slice(b"RssShmem:\t 1000 kB\nVmSwap:\t 500 kB\n");
        let m = from_linux_status_bytes(&t);
        assert_eq!(
            (
                m.private,
                m.in_ram,
                m.swapped_out,
                m.shared,
                m.os_tool_value
            ),
            (
                Some(8500 * 1024),
                Some(8000 * 1024),
                Some(500 * 1024),
                Some(4000 * 1024),
                Some(12000 * 1024)
            )
        );
        assert!(m.unavailable.is_none());
    }

    #[test]
    fn linux_kernel_thread_has_no_mem() {
        let m = from_linux_status("Name:\tkthreadd\n");
        assert_eq!(
            (m.private, m.unavailable),
            (None, Some(MemUnavailable::Unsupported))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_own_process_linux() {
        let m = read_mem(std::process::id());
        assert!(m.private.unwrap() > 0 && m.os_tool_value.unwrap() >= m.in_ram.unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_mem_of_exited_pid_reports_gone() {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert_eq!(read_mem(pid).unavailable, Some(MemUnavailable::Gone));
    }
}
