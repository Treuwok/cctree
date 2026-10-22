//! Environment-variable and WSL helpers for the collector.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::model::ENV_ALLOWLIST;

/// Keeps only the `NAME=value` entries whose name is in [`ENV_ALLOWLIST`].
/// Everything else (API keys, tokens, ...) is dropped here and never stored.
/// Entries without an `=` are ignored.
pub fn extract_env(environ: &[OsString]) -> BTreeMap<String, String> {
    environ
        .iter()
        .filter_map(|entry| {
            let entry = entry.to_string_lossy();
            let (name, value) = entry.split_once('=')?;
            ENV_ALLOWLIST
                .contains(&name)
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

/// WSL if `WSL_DISTRO_NAME` is set, or the kernel release mentions Microsoft
/// (case-insensitive), e.g. `5.15.153.1-microsoft-standard-WSL2`.
pub fn detect_wsl(wsl_distro_env: Option<&str>, osrelease: Option<&str>) -> bool {
    wsl_distro_env.is_some_and(|v| !v.is_empty())
        || osrelease.is_some_and(|r| r.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_env_keeps_only_allowlist() {
        let e = extract_env(&[
            "CLAUDECODE=1".into(),
            "OPENAI_API_KEY=sk-1".into(),
            "CLAUDE_CODE_SESSION_ID=abc".into(),
            "BROKEN".into(),
        ]);
        assert_eq!(
            e,
            BTreeMap::from([
                ("CLAUDECODE".into(), "1".into()),
                ("CLAUDE_CODE_SESSION_ID".into(), "abc".into())
            ])
        );
    }

    #[test]
    fn wsl_detection() {
        assert!(detect_wsl(Some("Ubuntu"), None));
        assert!(detect_wsl(None, Some("5.15.153.1-microsoft-standard-WSL2")));
        assert!(!detect_wsl(None, Some("6.8.0-45-generic")));
    }

    #[test]
    fn extract_env_keeps_values_containing_equals_signs() {
        let e = extract_env(&["CLAUDE_CONFIG_DIR=/a=b".into()]);
        assert_eq!(e["CLAUDE_CONFIG_DIR"], "/a=b");
    }

    #[test]
    fn empty_wsl_distro_name_is_not_wsl() {
        assert!(!detect_wsl(Some(""), None));
        assert!(!detect_wsl(None, None));
    }
}
