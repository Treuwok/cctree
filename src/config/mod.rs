//! Read-only view of the MCP servers the user configured for Claude Code and
//! Codex, so MCP processes can be labelled with the user's own names.

pub mod claude;
pub mod codex;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::Tool;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum McpScope {
    User,
    Project(String),
    Plugin(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct McpServerConfig {
    pub tool: Tool,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub scope: McpScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigProblem {
    pub path: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpConfigs {
    pub servers: Vec<McpServerConfig>,
    pub problems: Vec<ConfigProblem>,
}

/// What a config file is, which decides how it is parsed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Kind {
    /// `.claude.json`: user servers plus `projects[*]`.
    ClaudeJson,
    /// `<claude dir>/plugins/installed_plugins.json`: yields more sources.
    InstalledPlugins,
    /// A `.mcp.json` (session cwd or plugin install), tagged with its scope.
    Mcp(McpScope),
    /// `<installPath>/.claude-plugin/plugin.json`, tagged with the plugin name.
    PluginManifest(String),
    /// Codex `config.toml`.
    CodexToml,
}

/// One file to read; also the cache key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Source {
    path: PathBuf,
    kind: Kind,
}

enum Parsed {
    Servers(Vec<McpServerConfig>),
    /// `(plugin name, installPath)` pairs.
    Plugins(Vec<(String, String)>),
}

struct Entry {
    /// `(mtime, len)` the file had when parsed; `None` when the platform
    /// gave no modification time, which disables reuse.
    stamp: Option<(SystemTime, u64)>,
    outcome: Result<Parsed, String>,
}

/// Loads MCP server definitions from Claude Code and Codex config files.
///
/// Every file is cached by modification time, so a call that finds nothing
/// changed costs one `stat` per file and no parsing.
pub struct ConfigLoader {
    claude_dir: PathBuf,
    claude_json: PathBuf,
    codex_home: PathBuf,
    cache: HashMap<Source, Entry>,
    parse_count: usize,
    result: McpConfigs,
}

impl ConfigLoader {
    /// `home` is the user's home directory; `get_env` looks up the
    /// `CLAUDE_CONFIG_DIR` and `CODEX_HOME` overrides of this process.
    pub fn new(home: &Path, get_env: &dyn Fn(&str) -> Option<String>) -> Self {
        let env = |name: &str| get_env(name).filter(|v| !v.is_empty());
        let (claude_dir, claude_json) = match env("CLAUDE_CONFIG_DIR") {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                let json = dir.join(".claude.json");
                (dir, json)
            }
            None => (home.join(".claude"), home.join(".claude.json")),
        };
        let codex_home = env("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        ConfigLoader {
            claude_dir,
            claude_json,
            codex_home,
            cache: HashMap::new(),
            parse_count: 0,
            result: McpConfigs::default(),
        }
    }

    /// Number of times a file was actually read and parsed so far.
    pub fn parse_count(&self) -> usize {
        self.parse_count
    }

    /// Reads the default config locations plus the extra Claude config
    /// directories, Codex homes and session working directories (`.mcp.json`).
    /// Identical servers found in several places are reported once. A missing
    /// file is not a problem; an unreadable or malformed one adds a
    /// [`ConfigProblem`].
    pub fn load(
        &mut self,
        extra_claude_dirs: &[String],
        extra_codex_homes: &[String],
        session_cwds: &[String],
    ) -> &McpConfigs {
        let mut queue: VecDeque<Source> = VecDeque::new();
        queue.push_back(Source {
            path: self.claude_json.clone(),
            kind: Kind::ClaudeJson,
        });
        queue.push_back(installed_plugins_source(&self.claude_dir));
        queue.push_back(codex_source(&self.codex_home));
        for dir in extra_claude_dirs.iter().filter(|d| !d.is_empty()) {
            let dir = Path::new(dir);
            queue.push_back(Source {
                path: dir.join(".claude.json"),
                kind: Kind::ClaudeJson,
            });
            queue.push_back(installed_plugins_source(dir));
        }
        for home in extra_codex_homes.iter().filter(|h| !h.is_empty()) {
            queue.push_back(codex_source(Path::new(home)));
        }
        for cwd in session_cwds.iter().filter(|c| !c.is_empty()) {
            queue.push_back(Source {
                path: Path::new(cwd).join(".mcp.json"),
                kind: Kind::Mcp(McpScope::Project(cwd.clone())),
            });
        }

        let mut visited: HashSet<Source> = HashSet::new();
        let mut seen: HashSet<McpServerConfig> = HashSet::new();
        let mut servers = Vec::new();
        let mut problems = Vec::new();
        while let Some(source) = queue.pop_front() {
            if !visited.insert(source.clone()) || !self.refresh(&source) {
                continue;
            }
            let Some(entry) = self.cache.get(&source) else {
                continue;
            };
            match &entry.outcome {
                Err(detail) => problems.push(ConfigProblem {
                    path: source.path.to_string_lossy().into_owned(),
                    detail: detail.clone(),
                }),
                Ok(Parsed::Servers(list)) => {
                    for server in list {
                        if seen.insert(server.clone()) {
                            servers.push(server.clone());
                        }
                    }
                }
                Ok(Parsed::Plugins(plugins)) => {
                    for (name, install_path) in plugins {
                        let root = Path::new(install_path);
                        queue.push_back(Source {
                            path: root.join(".mcp.json"),
                            kind: Kind::Mcp(McpScope::Plugin(name.clone())),
                        });
                        queue.push_back(Source {
                            path: root.join(".claude-plugin").join("plugin.json"),
                            kind: Kind::PluginManifest(name.clone()),
                        });
                    }
                }
            }
        }
        // Sources no longer asked for (a finished session, a vanished file)
        // must not linger in the cache.
        self.cache.retain(|source, _| visited.contains(source));
        self.result = McpConfigs { servers, problems };
        &self.result
    }

    /// Makes the cache entry for `source` current. Returns `false` when the
    /// file does not exist (any cached entry is dropped), `true` when
    /// `self.cache` now holds an entry for it.
    fn refresh(&mut self, source: &Source) -> bool {
        let meta = match fs::metadata(&source.path) {
            Ok(meta) => meta,
            Err(e) if is_missing(&e) => {
                self.cache.remove(source);
                return false;
            }
            Err(e) => return self.cache_failure(source, format!("cannot read file: {e}")),
        };
        if !meta.is_file() {
            // Also keeps a FIFO or device from blocking the read below.
            return self.cache_failure(source, "cannot read file: not a regular file".to_string());
        }
        let stamp = meta.modified().ok().map(|mtime| (mtime, meta.len()));
        let unchanged = stamp.is_some()
            && self
                .cache
                .get(source)
                .is_some_and(|entry| entry.stamp == stamp);
        if unchanged {
            return true;
        }
        // A failed read is not cached against the mtime: it may be transient
        // (a writer holding the file), so it is retried on the next load.
        let text = match fs::read_to_string(&source.path) {
            Ok(text) => text,
            Err(e) => return self.cache_failure(source, format!("cannot read file: {e}")),
        };
        self.parse_count += 1;
        let outcome = parse(&source.kind, &text);
        self.cache.insert(source.clone(), Entry { stamp, outcome });
        true
    }

    fn cache_failure(&mut self, source: &Source, detail: String) -> bool {
        self.cache.insert(
            source.clone(),
            Entry {
                stamp: None,
                outcome: Err(detail),
            },
        );
        true
    }
}

fn installed_plugins_source(claude_dir: &Path) -> Source {
    Source {
        path: claude_dir.join("plugins").join("installed_plugins.json"),
        kind: Kind::InstalledPlugins,
    }
}

fn codex_source(codex_home: &Path) -> Source {
    Source {
        path: codex_home.join("config.toml"),
        kind: Kind::CodexToml,
    }
}

fn parse(kind: &Kind, text: &str) -> Result<Parsed, String> {
    match kind {
        Kind::ClaudeJson => claude::parse_claude_json(text).map(Parsed::Servers),
        Kind::InstalledPlugins => claude::parse_installed_plugins(text).map(Parsed::Plugins),
        Kind::Mcp(scope) => claude::parse_mcp_json(text, scope.clone()).map(Parsed::Servers),
        Kind::PluginManifest(name) => {
            claude::parse_plugin_json(text, McpScope::Plugin(name.clone())).map(Parsed::Servers)
        }
        Kind::CodexToml => codex::parse_codex_toml(text).map(Parsed::Servers),
    }
}

fn is_missing(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Windows editors like to prepend a byte order mark; neither parser accepts it.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Describes a JSON error by category and position only: the library's own
/// message can quote the offending value.
fn json_error(err: &serde_json::Error) -> String {
    use serde_json::error::Category;
    let what = match err.classify() {
        Category::Io => "read error",
        Category::Syntax => "syntax error",
        Category::Data => "unexpected structure",
        Category::Eof => "unexpected end of file",
    };
    format!(
        "invalid JSON ({what}) at line {} column {}",
        err.line(),
        err.column()
    )
}

#[cfg(test)]
mod tests {
    use super::claude::*;
    use super::codex::*;
    use super::*;

    const CLAUDE_JSON: &str = r#"{"mcpServers":{
  "chrome-cdp":{"type":"stdio","command":"npx","args":["-y","chrome-devtools-mcp@latest","--browserUrl","http://127.0.0.1:9222"]},
  "figma":{"type":"http","url":"https://x"}},
  "projects":{"/p/shop":{"mcpServers":{"shopify-dev-mcp":{"command":"npx","args":["-y","@shopify/dev-mcp@latest"]}}}}}"#;

    #[test]
    fn claude_json_user_and_project_scopes_skip_http() {
        let v = parse_claude_json(CLAUDE_JSON).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(
            (v[0].name.as_str(), &v[0].scope),
            ("chrome-cdp", &McpScope::User)
        );
        assert_eq!(v[1].scope, McpScope::Project("/p/shop".into()));
        assert_eq!(v[1].args, ["-y", "@shopify/dev-mcp@latest"]);
    }

    #[test]
    fn mcp_json_wrapped_and_unwrapped() {
        let a = parse_mcp_json(
            r#"{"mcpServers":{"playwright":{"command":"npx","args":["@playwright/mcp@latest"]}}}"#,
            McpScope::Plugin("playwright".into()),
        )
        .unwrap();
        let b = parse_mcp_json(
            r#"{"playwright":{"command":"npx","args":["@playwright/mcp@latest"]}}"#,
            McpScope::Plugin("playwright".into()),
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].name, "playwright");
    }

    #[test]
    fn installed_plugins_index() {
        let t = r#"{"version":2,"plugins":{"playwright@claude-plugins-official":[{"scope":"user","installPath":"/c/playwright/af59"}]}}"#;
        assert_eq!(
            parse_installed_plugins(t).unwrap(),
            [("playwright".to_string(), "/c/playwright/af59".to_string())]
        );
    }

    #[test]
    fn codex_toml_ignores_subtables() {
        let t = "[mcp_servers.context7]\ncommand = \"npx\"\nargs = [\"-y\", \"@upstash/context7-mcp\"]\n[mcp_servers.context7.env]\nX = \"1\"\n[mcp_servers.chrome-devtools.tools.new_page]\napproval = \"never\"\n";
        let v = parse_codex_toml(t).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].tool, v[0].name.as_str()), (Tool::Codex, "context7"));
    }

    #[test]
    fn malformed_claude_json_is_a_problem_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".claude.json"), "{oops").unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[], &[]);
        assert!(c.servers.is_empty());
        assert_eq!(c.problems.len(), 1);
        assert!(c.problems[0].path.ends_with(".claude.json"));
    }

    #[test]
    fn loader_skips_reparse_when_mtime_unchanged() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".claude.json"), CLAUDE_JSON).unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        l.load(&[], &[], &[]);
        let n = l.parse_count();
        l.load(&[], &[], &[]);
        assert_eq!(l.parse_count(), n);
    }

    #[test]
    fn extra_codex_home_from_snapshot_is_loaded() {
        let d = tempfile::tempdir().unwrap();
        let tmp_home = d.path().join("codex-home-Z");
        std::fs::create_dir(&tmp_home).unwrap();
        std::fs::write(
            tmp_home.join("config.toml"),
            "[mcp_servers.node_repl]\ncommand = \"node\"\nargs = [\"repl.js\"]\n",
        )
        .unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[tmp_home.to_string_lossy().into_owned()], &[]);
        assert!(
            c.servers
                .iter()
                .any(|s| s.tool == Tool::Codex && s.name == "node_repl")
        );
    }

    #[test]
    fn large_claude_json_parses() {
        let history: String = (0..20_000)
            .map(|i| format!(r#""/p/{i}":{{"history":["{}"]}},"#, "x".repeat(200)))
            .collect();
        let t = format!(r#"{{"projects":{{{history}"/p/last":{{}}}},"mcpServers":{{}}}}"#);
        assert!(parse_claude_json(&t).unwrap().is_empty());
    }

    // ---- parsers -------------------------------------------------------

    #[test]
    fn env_values_are_never_read() {
        let t = r#"{"mcpServers":{"gh":{"command":"npx","args":["x"],"env":{"TOKEN":"s3cret-token"}}}}"#;
        let v = parse_claude_json(t).unwrap();
        assert_eq!(v.len(), 1);
        assert!(!format!("{v:?}").contains("s3cret-token"));
        let t =
            "[mcp_servers.gh]\ncommand = \"npx\"\n[mcp_servers.gh.env]\nTOKEN = \"s3cret-token\"\n";
        let v = parse_codex_toml(t).unwrap();
        assert_eq!(v.len(), 1);
        assert!(!format!("{v:?}").contains("s3cret-token"));
    }

    #[test]
    fn parse_errors_do_not_quote_the_file() {
        let e = parse_claude_json(r#"{"projects":{"/p":"super-secret"}}"#).unwrap_err();
        assert!(!e.contains("super-secret"), "{e}");
        let e = parse_claude_json(r#"{"mcpServers":{"a":{"command":"npx","args":["tok-123""#)
            .unwrap_err();
        assert!(!e.contains("tok-123"), "{e}");
        let e = parse_codex_toml("[mcp_servers.x]\ncommand = \"npx\"\nargs = [\"tok-456\"\n")
            .unwrap_err();
        assert!(!e.contains("tok-456"), "{e}");
        assert!(e.contains("line"), "{e}");
    }

    #[test]
    fn non_object_roots_and_junk_entries_do_not_panic() {
        assert!(parse_claude_json("[]").is_err());
        assert!(parse_mcp_json("[1]", McpScope::User).is_err());
        assert!(parse_installed_plugins("null").is_err());
        let v = parse_claude_json(
            r#"{"mcpServers":{"a":null,"b":{"command":5},"c":{"command":"","args":["x"]},
                "d":{"command":"node","args":["x",7,true,null,{"k":1}]}},
               "projects":{"/p":null,"/q":{"mcpServers":[1]},"/r":{}}}"#,
        )
        .unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].args, ["x", "7", "true"]);
    }

    #[test]
    fn utf8_bom_is_tolerated() {
        let t = format!("\u{feff}{CLAUDE_JSON}");
        assert_eq!(parse_claude_json(&t).unwrap().len(), 2);
        let t = "\u{feff}[mcp_servers.a]\ncommand = \"x\"\n";
        assert_eq!(parse_codex_toml(t).unwrap().len(), 1);
    }

    #[test]
    fn plugin_json_keeps_inline_servers_and_ignores_path_form() {
        let scope = McpScope::Plugin("p".into());
        let inline = parse_plugin_json(
            r#"{"name":"p","mcpServers":{"srv":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/srv","args":["--root","${CLAUDE_PLUGIN_ROOT}"]}}}"#,
            scope.clone(),
        )
        .unwrap();
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].args, ["--root", "${CLAUDE_PLUGIN_ROOT}"]);
        assert_eq!(inline[0].command, "${CLAUDE_PLUGIN_ROOT}/bin/srv");
        let by_path = parse_plugin_json(r#"{"mcpServers":"./.mcp.json"}"#, scope.clone()).unwrap();
        assert!(by_path.is_empty());
        assert!(
            parse_plugin_json(r#"{"name":"p"}"#, scope)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn installed_plugins_handles_many_records_and_the_v1_object_form() {
        let t = r#"{"version":2,"plugins":{
            "a@m":[{"scope":"user","installPath":"/c/a/1"},{"scope":"project","installPath":"/c/a/2"},{"scope":"local","installPath":"/c/a/1"},{"scope":"x"}],
            "b@m":{"version":"1","installPath":"/c/b/1"},
            "no-marketplace":[{"installPath":"/c/n"}]}}"#;
        assert_eq!(
            parse_installed_plugins(t).unwrap(),
            [
                ("a".to_string(), "/c/a/1".to_string()),
                ("a".to_string(), "/c/a/2".to_string()),
                ("b".to_string(), "/c/b/1".to_string()),
                ("no-marketplace".to_string(), "/c/n".to_string()),
            ]
        );
        assert!(parse_installed_plugins("{}").unwrap().is_empty());
    }

    #[test]
    fn codex_toml_skips_url_servers_and_takes_command_servers() {
        let t = "[mcp_servers.remote]\nurl = \"https://x\"\n[mcp_servers.local]\ncommand = \"uvx\"\nargs = [\"a\", 1]\n";
        let v = parse_codex_toml(t).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "local");
        assert_eq!(v[0].args, ["a", "1"]);
        assert_eq!(v[0].command, "uvx");
        assert!(parse_codex_toml("model = \"x\"\n").unwrap().is_empty());
    }

    // ---- loader --------------------------------------------------------

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn set_mtime(path: &Path, secs_from_epoch: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs_from_epoch))
            .unwrap();
    }

    fn names(c: &McpConfigs) -> Vec<String> {
        let mut v: Vec<String> = c.servers.iter().map(|s| s.name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn missing_files_are_not_problems() {
        let d = tempfile::tempdir().unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[], &["/no/such/cwd".to_string()]);
        assert!(c.servers.is_empty());
        assert!(c.problems.is_empty());
        assert_eq!(l.parse_count(), 0);
    }

    #[test]
    fn reads_plugin_servers_through_installed_plugins() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let wrapped = home.join("cache/m/playwright/1.0");
        let inline = home.join("cache/m/inline/2.0");
        write(
            &home.join(".claude/plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"playwright@m":[{{"scope":"user","installPath":{}}}],"inline@m":[{{"scope":"user","installPath":{}}}],"gone@m":[{{"installPath":"/no/such/dir"}}]}}}}"#,
                serde_json::to_string(&wrapped.to_string_lossy()).unwrap(),
                serde_json::to_string(&inline.to_string_lossy()).unwrap(),
            ),
        );
        write(
            &wrapped.join(".mcp.json"),
            r#"{"mcpServers":{"playwright":{"command":"npx","args":["@playwright/mcp@latest"]}}}"#,
        );
        write(
            &wrapped.join(".claude-plugin/plugin.json"),
            r#"{"name":"playwright","mcpServers":"./.mcp.json"}"#,
        );
        write(
            &inline.join(".claude-plugin/plugin.json"),
            r#"{"name":"inline","mcpServers":{"srv":{"command":"node","args":["${CLAUDE_PLUGIN_ROOT}/srv.js"]}}}"#,
        );
        let mut l = ConfigLoader::new(home, &|_| None);
        let c = l.load(&[], &[], &[]).clone();
        assert!(c.problems.is_empty(), "{:?}", c.problems);
        assert_eq!(names(&c), ["playwright", "srv"]);
        let srv = c.servers.iter().find(|s| s.name == "srv").unwrap();
        assert_eq!(srv.scope, McpScope::Plugin("inline".into()));
        assert_eq!(srv.tool, Tool::ClaudeCode);
        assert_eq!(srv.args, ["${CLAUDE_PLUGIN_ROOT}/srv.js"]);
        let pw = c.servers.iter().find(|s| s.name == "playwright").unwrap();
        assert_eq!(pw.scope, McpScope::Plugin("playwright".into()));
    }

    #[test]
    fn claude_config_dir_env_moves_both_json_and_plugins() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let cfg = d.path().join("cfg");
        write(&home.join(".claude.json"), CLAUDE_JSON);
        write(
            &cfg.join(".claude.json"),
            r#"{"mcpServers":{"only-in-cfg":{"command":"x"}}}"#,
        );
        let cfg_s = cfg.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| cfg_s.clone());
        let mut l = ConfigLoader::new(&home, &env);
        assert_eq!(names(l.load(&[], &[], &[])), ["only-in-cfg"]);
    }

    #[test]
    fn codex_home_env_replaces_default_codex_dir() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let ch = d.path().join("ch");
        write(
            &home.join(".codex/config.toml"),
            "[mcp_servers.default]\ncommand = \"a\"\n",
        );
        write(
            &ch.join("config.toml"),
            "[mcp_servers.custom]\ncommand = \"b\"\n",
        );
        let ch_s = ch.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "CODEX_HOME").then(|| ch_s.clone());
        let mut l = ConfigLoader::new(&home, &env);
        assert_eq!(names(l.load(&[], &[], &[])), ["custom"]);
    }

    #[test]
    fn default_codex_home_is_dot_codex() {
        let d = tempfile::tempdir().unwrap();
        write(
            &d.path().join(".codex/config.toml"),
            "[mcp_servers.context7]\ncommand = \"npx\"\n",
        );
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[], &[]);
        assert_eq!(names(c), ["context7"]);
        assert_eq!(c.servers[0].tool, Tool::Codex);
    }

    #[test]
    fn extra_claude_dir_is_read_like_the_default_one() {
        let d = tempfile::tempdir().unwrap();
        let extra = d.path().join("other-claude");
        let install = d.path().join("cache/pl");
        write(
            &extra.join(".claude.json"),
            r#"{"mcpServers":{"from-extra":{"command":"x"}}}"#,
        );
        write(
            &extra.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"plugins":{{"pl@m":[{{"installPath":{}}}]}}}}"#,
                serde_json::to_string(&install.to_string_lossy()).unwrap()
            ),
        );
        write(
            &install.join(".mcp.json"),
            r#"{"from-plugin":{"command":"y"}}"#,
        );
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[extra.to_string_lossy().into_owned()], &[], &[]);
        assert_eq!(names(c), ["from-extra", "from-plugin"]);
    }

    #[test]
    fn session_cwd_mcp_json_gets_project_scope() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        write(
            &cwd.join(".mcp.json"),
            r#"{"mcpServers":{"local":{"command":"node","args":["s.js"]}}}"#,
        );
        let cwd_s = cwd.to_string_lossy().into_owned();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[], std::slice::from_ref(&cwd_s));
        assert_eq!(c.servers.len(), 1);
        assert_eq!(c.servers[0].scope, McpScope::Project(cwd_s.clone()));
        // The cwd is no longer a live session: its servers go away.
        assert!(l.load(&[], &[], &[]).servers.is_empty());
    }

    #[test]
    fn identical_servers_are_deduplicated_across_sources() {
        let d = tempfile::tempdir().unwrap();
        let toml = "[mcp_servers.node_repl]\ncommand = \"node\"\nargs = [\"repl.js\"]\n";
        write(&d.path().join(".codex/config.toml"), toml);
        let a = d.path().join("codex-home-A");
        let b = d.path().join("codex-home-B");
        write(&a.join("config.toml"), toml);
        write(&b.join("config.toml"), toml);
        let homes = [
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
            d.path().join(".codex").to_string_lossy().into_owned(),
            a.to_string_lossy().into_owned(),
        ];
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        assert_eq!(l.load(&[], &homes, &[]).servers.len(), 1);
        assert_eq!(l.load(&[], &homes, &[]).servers.len(), 1);
        // Same name, different args: a different server.
        write(
            &b.join("config.toml"),
            "[mcp_servers.node_repl]\ncommand = \"node\"\nargs = [\"other.js\"]\n",
        );
        assert_eq!(l.load(&[], &homes, &[]).servers.len(), 2);
    }

    #[test]
    fn changed_file_is_reparsed_and_disappeared_file_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".claude.json");
        write(&path, r#"{"mcpServers":{"one":{"command":"x"}}}"#);
        set_mtime(&path, 1_000);
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        assert_eq!(names(l.load(&[], &[], &[])), ["one"]);
        assert_eq!(l.parse_count(), 1);

        // Same length, newer mtime: still a change.
        write(&path, r#"{"mcpServers":{"two":{"command":"x"}}}"#);
        set_mtime(&path, 2_000);
        assert_eq!(names(l.load(&[], &[], &[])), ["two"]);
        assert_eq!(l.parse_count(), 2);

        std::fs::remove_file(&path).unwrap();
        let c = l.load(&[], &[], &[]);
        assert!(c.servers.is_empty());
        assert!(c.problems.is_empty());
        assert_eq!(l.parse_count(), 2);

        // It comes back: parsed again, not served from a stale cache.
        write(&path, r#"{"mcpServers":{"two":{"command":"x"}}}"#);
        set_mtime(&path, 2_000);
        assert_eq!(names(l.load(&[], &[], &[])), ["two"]);
        assert_eq!(l.parse_count(), 3);
    }

    #[test]
    fn a_broken_file_is_reported_every_load_but_parsed_once() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join(".codex/config.toml"), "[mcp_servers.x\n");
        write(
            &d.path().join(".claude.json"),
            r#"{"mcpServers":{"ok":{"command":"x"}}}"#,
        );
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        for _ in 0..3 {
            let c = l.load(&[], &[], &[]);
            assert_eq!(names(c), ["ok"]);
            assert_eq!(c.problems.len(), 1);
            assert!(c.problems[0].path.ends_with("config.toml"));
            assert!(c.problems[0].detail.contains("TOML"));
        }
        assert_eq!(l.parse_count(), 2);
    }

    #[test]
    fn an_unreadable_file_is_a_problem_and_is_retried_not_cached() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(".claude.json");
        std::fs::write(&path, [0xff, 0xfe, 0x41, 0x00]).unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        for _ in 0..2 {
            let c = l.load(&[], &[], &[]);
            assert!(c.servers.is_empty());
            assert_eq!(c.problems.len(), 1);
            assert!(c.problems[0].detail.starts_with("cannot read file"));
        }
        assert_eq!(l.parse_count(), 0);
        // Once the file is fixed, the same loader picks it up.
        std::fs::write(&path, r#"{"mcpServers":{"ok":{"command":"x"}}}"#).unwrap();
        let c = l.load(&[], &[], &[]);
        assert_eq!(names(c), ["ok"]);
        assert!(c.problems.is_empty());
    }

    #[test]
    fn a_directory_where_a_file_should_be_is_a_problem_not_a_hang_or_panic() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".claude.json")).unwrap();
        let mut l = ConfigLoader::new(d.path(), &|_| None);
        let c = l.load(&[], &[], &[]);
        assert!(c.servers.is_empty());
        assert_eq!(c.problems.len(), 1);
    }
}
