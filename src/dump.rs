//! `--dump-snapshot`: writes an anonymised snapshot for fixtures and bug reports.
//!
//! Anonymising masks secrets in every command line ([`mask_argv`]), replaces the
//! home directory with `~`, and renames real session ids to `sid-1`, `sid-2`, ...
//! Project directory names under the home directory are left as they are, so a
//! dump must be reviewed before it is published (spec 4.12).

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::collect::Collector;
use crate::mask::mask_argv;
use crate::model::{Platform, ProcKey, Snapshot};

const SESSION_ID_VAR: &str = "CLAUDE_CODE_SESSION_ID";

/// Returns a copy of `s` that is safe to share: see the module docs.
pub fn anonymize(s: &Snapshot) -> Snapshot {
    let home = s
        .home
        .as_deref()
        .map(|h| h.trim_end_matches(['/', '\\']))
        .filter(|h| !h.is_empty());
    let windows = s.platform == Platform::Windows;
    let tilde = |text: &str| match home {
        Some(h) => replace_home(text, h, windows),
        None => text.to_string(),
    };

    // Real session ids in order of first appearance. The same id always gets
    // the same label, so the processes of one session still group together.
    let mut sessions: Vec<&str> = Vec::new();
    let mut procs = Vec::with_capacity(s.procs.len());
    for p in &s.procs {
        let mut q = p.clone();
        q.exe = p.exe.as_deref().map(tilde);
        q.argv = mask_argv(&p.argv).iter().map(|a| tilde(a)).collect();
        q.cwd = p.cwd.as_deref().map(tilde);
        q.env = p
            .env
            .iter()
            .map(|(k, v)| {
                let v = if k == SESSION_ID_VAR {
                    let n = match sessions.iter().position(|seen| seen == v) {
                        Some(i) => i + 1,
                        None => {
                            sessions.push(v);
                            sessions.len()
                        }
                    };
                    format!("sid-{n}")
                } else {
                    tilde(v)
                };
                (k.clone(), v)
            })
            .collect();
        procs.push(q);
    }
    Snapshot {
        home: Some("~".to_string()),
        procs,
        ..s.clone()
    }
}

/// Replaces `home` with `~` wherever it appears as a whole path prefix: not
/// after a name character (`/mnt/Users/u`) and not before one (`/Users/u2`).
/// On Windows the match ignores case and also covers the `C:/Users/u` spelling.
fn replace_home(text: &str, home: &str, windows: bool) -> String {
    let mut out = text.to_string();
    out = replace_home_once(&out, home, windows);
    if windows && home.contains('\\') {
        out = replace_home_once(&out, &home.replace('\\', "/"), true);
    }
    out
}

fn replace_home_once(text: &str, home: &str, ignore_case: bool) -> String {
    let is_name_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
    // ASCII lowercasing keeps byte offsets, so indices found in the lowercase
    // copies are valid in `text`.
    let (hay, needle) = if ignore_case {
        (text.to_ascii_lowercase(), home.to_ascii_lowercase())
    } else {
        (text.to_string(), home.to_string())
    };
    let mut out = String::with_capacity(text.len());
    let mut copied = 0; // `text[..copied]` is already in `out`
    let mut from = 0;
    while let Some(rel) = hay[from..].find(&needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before = &text[..start];
        // `file:///Users/u/x` (and `file:///C:/Users/u/x`): the slash before the
        // home directory belongs to the URL, not to a longer path.
        let before_ok = before.ends_with("://")
            || before.ends_with("file:///")
            || before
                .chars()
                .next_back()
                .is_none_or(|c| !is_name_char(c) && !matches!(c, '/' | '\\' | '~'));
        let after_ok = text[end..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '/' | '\\') || !is_name_char(c));
        if before_ok && after_ok {
            out.push_str(&text[copied..start]);
            out.push('~');
            copied = end;
            from = end;
        } else {
            from = start + text[start..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Collects twice (CPU needs two readings), reads memory for every process of
/// the current user, anonymises, and writes pretty-printed JSON to `path`. Prints
/// `collected <N> processes; warm tick <ms> ms` on stderr; the time covers the
/// second `snapshot()` plus `fill_dynamic`.
pub fn write_dump(path: &Path) -> std::io::Result<()> {
    let mut collector = Collector::new();
    collector.snapshot();
    std::thread::sleep(Duration::from_secs(1));

    let started = Instant::now();
    let mut snap = collector.snapshot();
    let tracked: HashSet<ProcKey> = snap
        .procs
        .iter()
        .filter(|p| p.is_own_user)
        .map(|p| p.key)
        .collect();
    collector.fill_dynamic(&mut snap, &tracked);
    let warm_tick = started.elapsed();

    let anonymized = anonymize(&snap);
    let mut json = serde_json::to_string_pretty(&anonymized)?;
    json.push('\n');
    std::fs::write(path, json)?;
    eprintln!(
        "collected {} processes; warm tick {} ms",
        anonymized.procs.len(),
        warm_tick.as_millis()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn anonymize_removes_home_secrets_and_session_ids() {
        let s = testkit::snap(
            Platform::MacOs,
            vec![
                testkit::p(
                    10,
                    1,
                    5,
                    &["/Users/u/bin/x", "--token", "t", "/Users/u/prj"],
                )
                .cwd("/Users/u/prj")
                .env("CLAUDE_CODE_SESSION_ID", "real-uuid"),
            ],
        );
        let a = anonymize(&s);
        let p = &a.procs[0];
        assert_eq!(p.argv, ["~/bin/x", "--token", "***", "~/prj"]);
        assert_eq!(p.cwd.as_deref(), Some("~/prj"));
        assert_eq!(p.env["CLAUDE_CODE_SESSION_ID"], "sid-1");
        assert_eq!(a.home.as_deref(), Some("~"));
    }

    #[test]
    fn session_ids_are_numbered_by_first_appearance_and_stay_consistent() {
        let s = testkit::snap(
            Platform::Linux,
            vec![
                testkit::p(1, 0, 1, &["a"]).env(SESSION_ID_VAR, "bbb"),
                testkit::p(2, 1, 1, &["b"]).env(SESSION_ID_VAR, "aaa"),
                testkit::p(3, 1, 1, &["c"]).env(SESSION_ID_VAR, "bbb"),
            ],
        );
        let a = anonymize(&s);
        let ids: Vec<&str> = a
            .procs
            .iter()
            .map(|p| p.env[SESSION_ID_VAR].as_str())
            .collect();
        assert_eq!(ids, ["sid-1", "sid-2", "sid-1"]);
    }

    #[test]
    fn home_is_replaced_in_exe_and_env_values_but_not_in_other_directories() {
        let s = testkit::snap(
            Platform::MacOs,
            vec![
                testkit::p(
                    1,
                    0,
                    1,
                    &[
                        "/Users/u/.local/bin/claude",
                        "--config=/Users/u/cfg",
                        "/Users/u2/x",
                    ],
                )
                .env("CLAUDE_CONFIG_DIR", "/Users/u/.claude")
                .env("CODEX_HOME", "/Volumes/Backup/Users/u/.codex"),
            ],
        );
        let p = &anonymize(&s).procs[0];
        assert_eq!(p.exe.as_deref(), Some("~/.local/bin/claude"));
        assert_eq!(
            p.argv,
            ["~/.local/bin/claude", "--config=~/cfg", "/Users/u2/x"]
        );
        assert_eq!(p.env["CLAUDE_CONFIG_DIR"], "~/.claude");
        assert_eq!(p.env["CODEX_HOME"], "/Volumes/Backup/Users/u/.codex");
    }

    #[test]
    fn home_inside_file_urls_is_replaced() {
        let s = testkit::snap(
            Platform::MacOs,
            vec![testkit::p(
                1,
                0,
                1,
                &["node", "--import", "file:///Users/u/prj/loader.mjs"],
            )],
        );
        assert_eq!(anonymize(&s).procs[0].argv[2], "file://~/prj/loader.mjs");
        let mut w = testkit::snap(
            Platform::Windows,
            vec![testkit::p(
                1,
                0,
                1,
                &["node", "file:///C:/Users/Sam/prj/x.mjs"],
            )],
        );
        w.home = Some(r"C:\Users\Sam".into());
        // The drive-letter home has no leading slash, so the URL's own `///` stays.
        assert_eq!(anonymize(&w).procs[0].argv[1], "file:///~/prj/x.mjs");
    }

    #[test]
    fn windows_home_matches_ignoring_case_and_either_slash() {
        let mut s = testkit::snap(
            Platform::Windows,
            vec![
                testkit::p(
                    1,
                    0,
                    1,
                    &[
                        r"C:\Users\Sam\bin\claude.exe",
                        "c:/users/sam/prj",
                        r"C:\Users\Samuel\x",
                    ],
                )
                .cwd(r"c:\USERS\sam\prj"),
            ],
        );
        s.home = Some(r"C:\Users\Sam".into());
        let p = &anonymize(&s).procs[0];
        assert_eq!(p.argv, [r"~\bin\claude.exe", "~/prj", r"C:\Users\Samuel\x"]);
        assert_eq!(p.cwd.as_deref(), Some(r"~\prj"));
    }

    #[test]
    fn anonymize_without_home_still_masks_and_sets_tilde() {
        let mut s = testkit::snap(
            Platform::Linux,
            vec![testkit::p(1, 0, 1, &["x", "--password=p"])],
        );
        s.home = None;
        let a = anonymize(&s);
        assert_eq!(a.procs[0].argv, ["x", "--password=***"]);
        assert_eq!(a.home.as_deref(), Some("~"));
    }

    #[test]
    fn write_dump_produces_a_parseable_anonymized_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json");
        write_dump(&path).unwrap();
        let back: Snapshot =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.home.as_deref(), Some("~"));
        assert!(back.get(back.self_key).is_some());
        assert!(
            back.procs
                .iter()
                .flat_map(|p| &p.argv)
                .all(|a| !a.starts_with("sk-"))
        );
    }
}
