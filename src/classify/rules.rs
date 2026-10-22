//! Tool signatures and path evidence.
//!
//! [`detect_tool`] decides whether one process is the root of a Claude Code or
//! Codex session. [`path_evidence`] is the weak counterpart: where a process
//! lives (or runs) can hint at the tool that started it even when its own
//! command line says nothing, which is what orphan detection falls back on.
//!
//! Paths are compared after turning `\` into `/` and lowercasing ASCII, so the
//! same rule matches a Windows path on a macOS host and the other way round.

use crate::model::{RawProc, Tool};

use super::argv::base_name;

/// A process recognised as the root of a session of `tool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolHit {
    pub tool: Tool,
    /// The process is the `node` shell that runs the tool's npm package
    /// (`node .../@openai/codex/bin/codex.js`), not the tool's own binary.
    pub node_wrapper: bool,
}

/// Main executables of desktop apps that share a name with a tool but are not
/// the CLI, so they never count as a session. Entries are lowercase with `/`
/// separators and are matched against the normalized exe path or `argv[0]`:
///
/// - An entry ending in `/` is a directory: any path inside it is excluded.
/// - Any other entry is a path suffix that must start at a path component, so
///   `claude.app/contents/macos/claude` does not match `myclaude.app/...`.
///
/// The Codex CLI that ships inside `ChatGPT.app` lives in a nested
/// `CodexCLI.app` and runs as `codex`, so none of these entries match it.
pub const DESKTOP_APP_EXCLUDES: &[&str] = &[
    "claude.app/contents/macos/claude",
    "codex.app/contents/macos/codex",
    "chatgpt.app/contents/macos/chatgpt",
    // Windows install directories: verify with recon.
    "/appdata/local/anthropicclaude/",
    "/appdata/local/programs/claude/",
];

/// Programs that run a JavaScript file given as an argument.
const NODE_PROGRAMS: &[&str] = &["node", "nodejs"];

/// The npm package scripts that `node` runs for each tool.
const CLAUDE_NPM_SCRIPT: &str = "@anthropic-ai/claude-code/cli.js";
const CODEX_NPM_SCRIPT: &str = "@openai/codex/bin/codex.js";

/// Where the native Claude Code installer keeps its versioned binaries.
const CLAUDE_VERSIONS_DIR: &str = "/claude/versions/";

/// Temp directories that Codex plugins use as `CODEX_HOME`.
const CODEX_HOME_MARK: &str = "/codex-home-";

/// Whether `p` is the root of a Claude Code or Codex session, and which.
///
/// Claude Code is a process whose `argv[0]` is `claude`, the native binary
/// started by its own path under `claude/versions/` (the binary is multi-call:
/// its helpers run as `ugrep` or `rg` from the same file and are not
/// sessions), or `node` running the npm package's `cli.js`. Codex is a process
/// whose exe or `argv[0]` is `codex`, or `node` running `bin/codex.js`. Main
/// executables of the desktop apps never count.
pub fn detect_tool(p: &RawProc) -> Option<ToolHit> {
    let exe = p.exe.as_deref();
    let argv0 = p.argv.first().map(String::as_str);

    if [exe, argv0].into_iter().flatten().any(is_desktop_app) {
        return None;
    }

    let native = |tool| {
        Some(ToolHit {
            tool,
            node_wrapper: false,
        })
    };
    if argv0.is_some_and(|a| is_program(a, "claude")) || runs_own_versioned_binary(exe, argv0) {
        return native(Tool::ClaudeCode);
    }
    if [exe, argv0]
        .into_iter()
        .flatten()
        .any(|path| is_program(path, "codex"))
    {
        return native(Tool::Codex);
    }

    if is_node(p) {
        let wrapper = |tool| {
            Some(ToolHit {
                tool,
                node_wrapper: true,
            })
        };
        if runs_script(p, CLAUDE_NPM_SCRIPT) {
            return wrapper(Tool::ClaudeCode);
        }
        if runs_script(p, CODEX_NPM_SCRIPT) {
            return wrapper(Tool::Codex);
        }
    }
    None
}

/// Weak evidence from where `p` runs: the tool that most likely started it,
/// judged by its exe, then its cwd. A path with a `codex-home-` directory or
/// under `~/.codex` points at Codex; under `~/.claude`, at Claude Code.
///
/// `home` is the snapshot's home directory. Without one (or with an empty one),
/// the literal `~` stands for it, which is how anonymized snapshots write it.
pub fn path_evidence(p: &RawProc, home: Option<&str>) -> Option<Tool> {
    let home = normalize(home.unwrap_or_default());
    let home = match home.trim_end_matches('/') {
        "" => "~",
        h => h,
    };
    [p.exe.as_deref(), p.cwd.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|path| path_tool(&normalize(path), home))
}

/// The tool a normalized `path` hints at, if any. `home` is normalized, too.
fn path_tool(path: &str, home: &str) -> Option<Tool> {
    if path.contains(CODEX_HOME_MARK) || is_under_home_dir(path, home, ".codex") {
        Some(Tool::Codex)
    } else if is_under_home_dir(path, home, ".claude") {
        Some(Tool::ClaudeCode)
    } else {
        None
    }
}

/// Whether `path` is `<home>/<dir>` or inside it.
fn is_under_home_dir(path: &str, home: &str, dir: &str) -> bool {
    path.strip_prefix(home)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|rest| rest.strip_prefix(dir))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `path` with `\` turned into `/` and ASCII lowercased.
fn normalize(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// Whether the file name of `path` is `want`, ignoring case and `.exe`.
fn is_program(path: &str, want: &str) -> bool {
    base_name(path).eq_ignore_ascii_case(want)
}

fn is_desktop_app(path: &str) -> bool {
    let path = normalize(path);
    DESKTOP_APP_EXCLUDES.iter().any(|entry| {
        if entry.ends_with('/') {
            path.contains(entry)
        } else {
            path.strip_suffix(entry)
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
        }
    })
}

/// The native Claude Code binary started by its own path: the exe sits in
/// `claude/versions/` and `argv[0]` names the same file. A helper run from the
/// same file under another name (`ugrep`, `rg`) does not.
fn runs_own_versioned_binary(exe: Option<&str>, argv0: Option<&str>) -> bool {
    let (Some(exe), Some(argv0)) = (exe, argv0) else {
        return false;
    };
    let file = base_name(exe);
    !file.is_empty() && normalize(exe).contains(CLAUDE_VERSIONS_DIR) && is_program(argv0, file)
}

fn is_node(p: &RawProc) -> bool {
    let argv0 = p.argv.first().map(String::as_str);
    [argv0, p.exe.as_deref(), Some(p.name.as_str())]
        .into_iter()
        .flatten()
        .any(|path| NODE_PROGRAMS.iter().any(|n| is_program(path, n)))
}

/// Whether an argument after `argv[0]` is a path into the npm package `script`
/// (written with `/`, matched with either separator).
fn runs_script(p: &RawProc, script: &str) -> bool {
    p.argv
        .iter()
        .skip(1)
        .any(|arg| normalize(arg).contains(script))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::p;

    fn t(argv: &[&str]) -> Option<(Tool, bool)> {
        detect_tool(&p(1, 0, 1, argv)).map(|h| (h.tool, h.node_wrapper))
    }

    #[test]
    fn claude_signatures() {
        let native = p(1, 0, 1, &["claude", "--resume", "x"])
            .exe("/Users/u/.local/share/claude/versions/2.1.306")
            .name("2.1.306");
        assert_eq!(detect_tool(&native).map(|h| h.tool), Some(Tool::ClaudeCode));
        assert_eq!(
            t(&[
                "node",
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
            ]),
            Some((Tool::ClaudeCode, true))
        );
        assert_eq!(
            t(&["C:\\Users\\u\\.local\\bin\\claude.exe"]),
            Some((Tool::ClaudeCode, false))
        );
        assert_eq!(t(&["/Applications/Claude.app/Contents/MacOS/Claude"]), None);
    }

    #[test]
    fn codex_signatures() {
        assert_eq!(t(&["/opt/homebrew/bin/codex"]), Some((Tool::Codex, false)));
        assert_eq!(t(&["codex", "app-server"]), Some((Tool::Codex, false)));
        assert_eq!(
            t(&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"]),
            Some((Tool::Codex, true))
        );
        assert_eq!(t(&["/Applications/Codex.app/Contents/MacOS/Codex"]), None);
        assert_eq!(t(&["node", "server.js"]), None);
    }

    #[test]
    fn path_evidence_rules() {
        let h = Some("/Users/u");
        let meet = p(
            1,
            1,
            1,
            &["/private/var/folders/dr/x/T/codex-home-Zn46A8/codex_reviewer/m/ChatGPT Meetings"],
        );
        assert_eq!(path_evidence(&meet, h), Some(Tool::Codex));
        assert_eq!(
            path_evidence(&p(1, 1, 1, &["/Users/u/.codex/plugins/cache/x/host"]), h),
            Some(Tool::Codex)
        );
        assert_eq!(
            path_evidence(&p(1, 1, 1, &["x"]).cwd("/Users/u/.claude/tmp"), h),
            Some(Tool::ClaudeCode)
        );
        assert_eq!(path_evidence(&p(1, 1, 1, &["/usr/bin/node"]), h), None);
    }

    // The native Claude binary is multi-call: its helpers (`ugrep`, `rg`) run
    // from the same versioned file and must not look like sessions.
    #[test]
    fn versioned_binary_counts_only_when_run_by_its_own_name() {
        let versioned = "/Users/u/.local/share/claude/versions/2.1.306";
        let helper = p(1, 0, 1, &["ugrep", "-r", "x"]).exe(versioned);
        assert_eq!(detect_tool(&helper), None);
        let rg = p(1, 0, 1, &["rg", "x"]).exe(versioned);
        assert_eq!(detect_tool(&rg), None);

        let own = p(1, 0, 1, &[versioned]).exe(versioned);
        assert_eq!(
            detect_tool(&own).map(|h| (h.tool, h.node_wrapper)),
            Some((Tool::ClaudeCode, false))
        );

        // Windows: the versioned directory is matched with `\`, too, and the
        // same file under another name is still a helper.
        let win = r"C:\Users\u\.local\share\claude\versions\2.1.306\app.exe";
        let win_own = p(1, 0, 1, &[win]).exe(win);
        assert_eq!(
            detect_tool(&win_own).map(|h| h.tool),
            Some(Tool::ClaudeCode)
        );
        let win_helper = p(1, 0, 1, &["rg.exe", "x"]).exe(win);
        assert_eq!(detect_tool(&win_helper), None);
    }

    #[test]
    fn versioned_binary_needs_argv0_to_verify() {
        let mut none = p(1, 0, 1, &["x"]).exe("/home/u/.local/share/claude/versions/2.1.306");
        none.argv.clear();
        assert_eq!(detect_tool(&none), None);
    }

    #[test]
    fn claude_on_macos_by_argv0() {
        let real = p(1, 0, 1, &["claude", "--resume", "x"]).exe("/Users/u/.local/bin/claude");
        assert_eq!(
            detect_tool(&real).map(|h| (h.tool, h.node_wrapper)),
            Some((Tool::ClaudeCode, false))
        );
    }

    #[test]
    fn names_ignore_case_and_exe_suffix() {
        assert_eq!(
            t(&["C:\\Tools\\CLAUDE.EXE", "--resume"]),
            Some((Tool::ClaudeCode, false))
        );
        assert_eq!(t(&["Codex.exe"]), Some((Tool::Codex, false)));
        assert_eq!(t(&["claude-helper"]), None);
        assert_eq!(t(&["/usr/bin/claude-code-router"]), None);
    }

    #[test]
    fn codex_by_exe_when_argv0_is_retitled() {
        let retitled = p(1, 0, 1, &["codex-tui"]).exe("/opt/homebrew/bin/codex");
        assert_eq!(
            detect_tool(&retitled).map(|h| (h.tool, h.node_wrapper)),
            Some((Tool::Codex, false))
        );
    }

    #[test]
    fn codex_cli_inside_chatgpt_app_is_a_session() {
        assert_eq!(
            t(&[
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
                "app-server"
            ]),
            Some((Tool::Codex, false))
        );
    }

    #[test]
    fn desktop_app_main_executables_are_not_sessions() {
        assert_eq!(
            t(&["/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"]),
            None
        );
        assert_eq!(t(&["/Applications/Claude.app/Contents/MacOS/Claude"]), None);
        assert_eq!(t(&["/Applications/Codex.app/Contents/MacOS/Codex"]), None);
        assert_eq!(
            t(&["/Users/u/Applications/CLAUDE.app/Contents/MacOS/claude"]),
            None
        );
        // The exe alone is enough, whatever `argv[0]` says.
        let by_exe = p(1, 0, 1, &["claude"]).exe("/Applications/Claude.app/Contents/MacOS/Claude");
        assert_eq!(detect_tool(&by_exe), None);
        // Only the known main executables are excluded, not every app bundle.
        assert_eq!(
            t(&["/Applications/Other.app/Contents/MacOS/claude"]),
            Some((Tool::ClaudeCode, false))
        );
        assert_eq!(
            t(&["/Applications/MyClaude.app/Contents/MacOS/Claude"]),
            Some((Tool::ClaudeCode, false))
        );
    }

    #[test]
    fn windows_desktop_app_directories_are_not_sessions() {
        assert_eq!(
            t(&["C:\\Users\\u\\AppData\\Local\\AnthropicClaude\\app-1.0.0\\claude.exe"]),
            None
        );
        assert_eq!(
            t(&["c:\\users\\u\\appdata\\local\\anthropicclaude\\app-1.0.0\\claude.exe"]),
            None
        );
        assert_eq!(
            t(&["C:\\Users\\u\\AppData\\Local\\Programs\\Claude\\Claude.exe"]),
            None
        );
        assert_eq!(
            t(&["C:\\Users\\u\\.local\\bin\\claude.exe"]),
            Some((Tool::ClaudeCode, false))
        );
    }

    #[test]
    fn node_shells_on_windows_and_with_flags() {
        assert_eq!(
            t(&[
                "C:\\Program Files\\nodejs\\node.exe",
                "C:\\Users\\u\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js",
                "--resume"
            ]),
            Some((Tool::ClaudeCode, true))
        );
        assert_eq!(
            t(&[
                "node",
                "--no-warnings",
                "C:\\Users\\u\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\bin\\codex.js"
            ]),
            Some((Tool::Codex, true))
        );
        // Only `node` counts as the shell, and only for the tool's own script.
        assert_eq!(
            t(&["grep", "x", "/n/node_modules/@openai/codex/bin/codex.js"]),
            None
        );
        assert_eq!(
            t(&["node", "/n/node_modules/@openai/codex/bin/other.js"]),
            None
        );
        assert_eq!(t(&["node"]), None);
        let mut bare = p(1, 0, 1, &["x"]);
        bare.argv.clear();
        bare.exe = None;
        assert_eq!(
            detect_tool(&bare),
            None,
            "a process with no argv and no exe is no session"
        );
    }

    #[test]
    fn path_evidence_in_cwd_and_windows_paths() {
        let h = Some("C:\\Users\\u");
        let by_cwd = p(1, 1, 1, &["x"]).cwd("C:\\Users\\U\\.codex\\sessions");
        assert_eq!(path_evidence(&by_cwd, h), Some(Tool::Codex));
        let claude = p(1, 1, 1, &["x"]).exe("c:\\users\\u\\.claude\\bin\\tool.exe");
        assert_eq!(path_evidence(&claude, h), Some(Tool::ClaudeCode));
        let temp = p(1, 1, 1, &["x"]).cwd("C:\\Users\\u\\AppData\\Local\\Temp\\codex-home-ab12\\w");
        assert_eq!(path_evidence(&temp, h), Some(Tool::Codex));
        let tmp = p(1, 1, 1, &["x"]).cwd("/tmp/codex-home-ab12");
        assert_eq!(path_evidence(&tmp, Some("/Users/u")), Some(Tool::Codex));
    }

    #[test]
    fn path_evidence_exe_wins_over_cwd() {
        let both = p(1, 1, 1, &["x"])
            .exe("/Users/u/.codex/bin/host")
            .cwd("/Users/u/.claude/tmp");
        assert_eq!(path_evidence(&both, Some("/Users/u")), Some(Tool::Codex));
        let cwd_only = p(1, 1, 1, &["x"])
            .exe("/usr/bin/node")
            .cwd("/Users/u/.claude/tmp");
        assert_eq!(
            path_evidence(&cwd_only, Some("/Users/u")),
            Some(Tool::ClaudeCode)
        );
    }

    #[test]
    fn path_evidence_stays_inside_the_home_directory() {
        let h = Some("/Users/u");
        // Another user's `.codex`, and names that only start like the dir.
        assert_eq!(path_evidence(&p(1, 1, 1, &["/Users/v/.codex/x"]), h), None);
        assert_eq!(path_evidence(&p(1, 1, 1, &["/Users/u/.codexx/x"]), h), None);
        assert_eq!(
            path_evidence(&p(1, 1, 1, &["x"]).cwd("/Users/u/.claude.json"), h),
            None
        );
        assert_eq!(
            path_evidence(&p(1, 1, 1, &["x"]).cwd("/Users/u/prj/.claude/tmp"), h),
            None
        );
        // The directory itself counts; a trailing slash on home is ignored.
        assert_eq!(
            path_evidence(
                &p(1, 1, 1, &["x"]).cwd("/Users/u/.claude"),
                Some("/Users/u/")
            ),
            Some(Tool::ClaudeCode)
        );
        assert_eq!(path_evidence(&p(1, 1, 1, &["x"]), h), None);
    }

    // Anonymized snapshots write the home directory as `~`.
    #[test]
    fn path_evidence_with_anonymized_home() {
        let tilde = Some("~");
        let plugin = p(
            1,
            1,
            1,
            &["~/.codex/plugins/cache/openai-bundled/chrome/latest/host/ChatGPT for Chrome"],
        );
        assert_eq!(path_evidence(&plugin, tilde), Some(Tool::Codex));
        assert_eq!(
            path_evidence(&p(1, 1, 1, &["x"]).cwd("~/.claude/tmp"), tilde),
            Some(Tool::ClaudeCode)
        );
        // No home known: `~` is still taken as the home directory.
        assert_eq!(path_evidence(&plugin, None), Some(Tool::Codex));
        assert_eq!(path_evidence(&plugin, Some("")), Some(Tool::Codex));
        // The anonymized temp-dir shape still matches.
        let meet = p(1, 1, 1, &["ChatGPT Meetings"])
            .exe("/private/var/folders/xx/tmp/T/codex-home-XXXXXX/r/ChatGPT Meetings");
        assert_eq!(path_evidence(&meet, tilde), Some(Tool::Codex));
        assert_eq!(path_evidence(&meet, Some("/Users/u")), Some(Tool::Codex));
    }
}
