//! Command-line shapes of MCP servers: launcher normalization, matching a
//! process against the user's MCP config, fallback names, wrapper detection and
//! display names.
//!
//! An MCP server is usually started through a launcher (`npx -y pkg`,
//! `npm exec pkg`, `uvx pkg`, `cmd /c npx ...` on Windows, `docker run image`).
//! [`normalize_launch`] strips the launcher so that a config entry and the
//! process it produced compare equal whichever launcher spelling each uses.
//!
//! Every file name is taken by splitting on both `/` and `\` and dropping a
//! trailing `.exe`/`.cmd` (case-insensitive), whatever OS cctree runs on.
//!
//! Config arguments can hold secrets (`--api-key <key>`). They are only ever
//! compared here; nothing in this module formats, logs or returns them as text.

use crate::config::McpServerConfig;
use crate::model::RawProc;

use super::lineage::{file_name, strip_executable_extension};

/// What a command line starts, with the launcher taken off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// The package, image or program that actually runs, as written (a version
    /// such as `@latest` is kept). Empty if the command line names none.
    pub program: String,
    /// Everything after [`Launch::program`].
    pub args: Vec<String>,
}

/// Flags of `docker run` that take their value as the next word, so that word
/// is not mistaken for the image name. Flags written `--flag=value` and boolean
/// flags need no entry.
const DOCKER_VALUE_FLAGS: &[&str] = &[
    "-a",
    "--add-host",
    "--annotation",
    "--attach",
    "--cap-add",
    "--cap-drop",
    "--cidfile",
    "--cpus",
    "--device",
    "--dns",
    "-e",
    "--entrypoint",
    "--env",
    "--env-file",
    "--expose",
    "--gpus",
    "--group-add",
    "-h",
    "--hostname",
    "--ip",
    "-l",
    "--label",
    "--label-file",
    "--link",
    "--log-driver",
    "--log-opt",
    "-m",
    "--memory",
    "--mount",
    "--name",
    "--net",
    "--network",
    "-p",
    "--platform",
    "--publish",
    "--pull",
    "--restart",
    "--runtime",
    "--security-opt",
    "--shm-size",
    "--tmpfs",
    "-u",
    "--user",
    "--userns",
    "-v",
    "--volume",
    "--volumes-from",
    "-w",
    "--workdir",
];

/// Executables that run a script file given as an argument.
const SCRIPT_RUNNERS: &[&str] = &["node", "nodejs", "bun", "deno"];

/// Script names (without extension) that say nothing about what the script is,
/// so the directory holding it names it instead.
const GENERIC_SCRIPT_NAMES: &[&str] = &["main", "index", "cli"];

/// The parts of a name that carry no meaning in `mcp-server-fetch`,
/// `server-sequential-thinking`, `chrome-devtools-mcp`, `foo-mcp-server`.
const NAME_PREFIXES: &[&str] = &["mcp-server-", "server-"];
const NAME_SUFFIXES: &[&str] = &["-mcp-server", "-mcp"];

/// Strips the launcher from `argv`: a leading `cmd /c`, then `npx`, `npm exec`,
/// `pnpm dlx`, `bunx`, `uvx`, `uv tool run`, `uv run`, `deno run` or
/// `docker run` together with their flags. The first word that is not a flag
/// is the [`Launch::program`]. For `docker run` that is the image: the first
/// word that is neither a flag nor a flag's value.
///
/// Without a launcher the program is the file name of `argv[0]` (so
/// `C:\x\node.exe` is `node`) and the arguments are the rest.
pub fn normalize_launch(argv: &[String]) -> Launch {
    parse(argv).launch
}

/// Whether the process `argv` was started from the config entry `cfg`.
///
/// Both command lines are normalized with [`normalize_launch`]. They match when
/// the programs are equal (ignoring ASCII case, versions included, so
/// `pkg@latest` and `pkg@1.6.0` differ) and every config argument appears in
/// the process arguments, in the same order, other arguments allowed between.
/// A config argument holding a `${...}` placeholder is compared only by the text
/// after the last `}` (`${CLAUDE_PLUGIN_ROOT}/server.js` needs an argument that
/// ends in `/server.js`); with nothing after it the argument is not compared.
///
/// This judges one config on its own. Server names are not unique across
/// plugins and scopes, so callers must not assume at most one config matches.
pub fn config_matches(cfg: &McpServerConfig, argv: &[String]) -> bool {
    let want = {
        let mut full = Vec::with_capacity(1 + cfg.args.len());
        full.push(cfg.command.clone());
        full.extend(cfg.args.iter().cloned());
        normalize_launch(&full)
    };
    let have = normalize_launch(argv);
    if want.program.is_empty() || !want.program.eq_ignore_ascii_case(&have.program) {
        return false;
    }
    let mut remaining = have.args.iter();
    want.args.iter().all(|w| match placeholder_suffix(w) {
        Some("") => true,
        Some(suffix) => {
            let suffix = forward_slashes(suffix);
            remaining.any(|a| forward_slashes(a).ends_with(&suffix))
        }
        None => remaining.any(|a| a == w),
    })
}

/// Whether `argv` looks like an MCP server. It does if any of these holds:
/// * it was started through a launcher (see [`normalize_launch`]);
/// * the program's file name contains `mcp` (`/opt/x/my-mcp`);
/// * the program is an interpreter (`node`, `nodejs`, `bun`, `deno`, `python`,
///   `python3`, `python3.12`, ...) and the file name of its script, or the
///   module after `-m`, contains `mcp` (`node /x/.bin/playwright-mcp`,
///   `python -m mcp_server_time`).
///
/// All comparisons ignore case. Other arguments are never inspected, so
/// `claude --mcp-config x.json` or `zsh -c "rg mcpServers src/"` do not count,
/// and neither does any argument that starts with `-` or holds whitespace.
pub fn looks_like_mcp(argv: &[String]) -> bool {
    let parsed = parse(argv);
    parsed.via_launcher
        || mentions_mcp(&parsed.launch.program)
        || interpreter_target(&parsed.launch.program, &parsed.launch.args)
            .is_some_and(|t| mentions_mcp(t.text()))
}

/// A name for an MCP server that is not in the user's config: the program of
/// [`normalize_launch`] without its version, reduced to its last path
/// component, without an `mcp-server-` / `server-` prefix or a `-mcp-server` /
/// `-mcp` suffix. If nothing meaningful is left (`@playwright/mcp`), the npm
/// scope without its `@` is used (`playwright`). Never empty.
///
/// Without a launcher, an interpreter (see [`looks_like_mcp`]) is not a name:
/// the script's file name without extension, or the `-m` module with `_`
/// written as `-`, takes the program's place (`node /x/.bin/playwright-mcp`
/// is `playwright`, `python -m mcp_server_time` is `time`). If there is no
/// script or module, the interpreter's own name remains.
pub fn mcp_fallback_name(argv: &[String]) -> String {
    let parsed = parse(argv);
    let launch = &parsed.launch;
    let target = if parsed.via_launcher {
        None
    } else {
        interpreter_target(&launch.program, &launch.args)
    };
    let (dir, last): (&str, String) = match target {
        Some(Target::Script(script)) => ("", script_stem(script).to_string()),
        Some(Target::Module(module)) => ("", module.replace('_', "-")),
        None => {
            let program = strip_version(&launch.program);
            let (dir, last) = match program.rsplit_once(['/', '\\']) {
                Some((dir, last)) => (dir, last),
                None => ("", program),
            };
            // `image:tag` names the image without its tag.
            (dir, last.split(':').next().unwrap_or(last).to_string())
        }
    };
    let scope = dir
        .rsplit(['/', '\\'])
        .next()
        .and_then(|d| d.strip_prefix('@'))
        .filter(|s| !s.is_empty());

    let mut name = last.as_str();
    if let Some(rest) = NAME_PREFIXES.iter().find_map(|p| name.strip_prefix(p)) {
        name = rest;
    }
    if let Some(rest) = NAME_SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) {
        name = rest;
    }
    if matches!(name, "" | "mcp" | "server" | "mcp-server") {
        if let Some(scope) = scope {
            return scope.to_string();
        }
        if name.is_empty() {
            return "mcp".to_string();
        }
    }
    name.to_string()
}

/// Whether `p` only exists to start another program, so the tree should look
/// through it: `npm exec ...`, `npx`, `cmd(.exe) /c ...`, and the console hosts
/// `conhost.exe` and `OpenConsole.exe`.
pub fn is_wrapper(p: &RawProc) -> bool {
    let name = base_name(&p.name);
    let program = p
        .argv
        .first()
        .map(|a| base_name(a))
        .filter(|n| !n.is_empty())
        .unwrap_or(name);
    let is = |n: &str, want: &str| n.eq_ignore_ascii_case(want);

    if [program, name]
        .iter()
        .any(|n| is(n, "conhost") || is(n, "openconsole"))
    {
        return true;
    }
    if is(program, "npx") {
        return true;
    }
    if is(program, "npm") {
        return p
            .argv
            .get(1)
            .is_some_and(|a| a.eq_ignore_ascii_case("exec"));
    }
    is(program, "cmd") && cmd_command(&p.argv).is_some()
}

/// A short name to show for `p`. When it runs a script through `node`, `bun` or
/// `deno`, the script's file name without extension (`main`, `index` and `cli`
/// are replaced by the name of the directory holding the script); otherwise the
/// executable's file name without `.exe`. The executable is `argv[0]`, then
/// `exe`; if neither gives a name, the process name, then `?`. Never empty.
pub fn display_name(p: &RawProc) -> String {
    let first = p
        .argv
        .first()
        .map(String::as_str)
        .filter(|a| !a.is_empty())
        .or_else(|| p.exe.as_deref().filter(|e| !e.is_empty()));
    if let Some(first) = first {
        let exe = base_name(first);
        let runner = SCRIPT_RUNNERS.iter().find(|r| exe.eq_ignore_ascii_case(r));
        if let Some(runner) = runner
            && let Some(script) = script_arg(runner, p.argv.get(1..).unwrap_or_default())
        {
            let name = script_name(script);
            if !name.trim().is_empty() {
                return name.to_string();
            }
        }
        if !exe.trim().is_empty() {
            return exe.to_string();
        }
    }
    let name = base_name(&p.name);
    if name.trim().is_empty() {
        "?".to_string()
    } else {
        name.to_string()
    }
}

/// A parsed command line: the [`Launch`] and whether a launcher was used.
struct Parsed {
    launch: Launch,
    via_launcher: bool,
}

fn parse(argv: &[String]) -> Parsed {
    let argv = cmd_command(argv).unwrap_or(argv);
    let Some((first, rest)) = argv.split_first() else {
        return Parsed {
            launch: Launch::default(),
            via_launcher: false,
        };
    };
    let exe = base_name(first);
    match launcher_args(exe, rest) {
        Some((args, is_docker)) => Parsed {
            launch: first_non_flag(args, is_docker),
            via_launcher: true,
        },
        None => Parsed {
            launch: Launch {
                program: exe.to_string(),
                args: rest.to_vec(),
            },
            via_launcher: false,
        },
    }
}

/// The file name of `path` without `.exe` / `.cmd` (and the like).
fn base_name(path: &str) -> &str {
    strip_executable_extension(file_name(path))
}

/// The words after `cmd`'s `/c` (`cmd.exe /c npx -y pkg`, `cmd /s /c ...`), or
/// `None` if `argv` is not `cmd` running a command.
fn cmd_command(argv: &[String]) -> Option<&[String]> {
    let (first, rest) = argv.split_first()?;
    if !base_name(first).eq_ignore_ascii_case("cmd") {
        return None;
    }
    // Other two-character switches (`/s`, `/d`, `/q`) may come before `/c`.
    for (i, arg) in rest.iter().enumerate() {
        if arg.eq_ignore_ascii_case("/c") {
            return Some(&rest[i + 1..]);
        }
        if !(arg.len() == 2 && arg.starts_with('/')) {
            return None;
        }
    }
    None
}

/// If `exe` (a file name without extension) is a launcher, the words after the
/// launcher and its sub-command (`npm exec`, `uv tool run`, ...), and whether
/// the launcher is `docker run`.
fn launcher_args<'a>(exe: &str, rest: &'a [String]) -> Option<(&'a [String], bool)> {
    // The words after `rest` starts with each of `words`, in order.
    let after = |words: &[&str]| -> Option<&'a [String]> {
        let mut r = rest;
        for word in words {
            let (head, tail) = r.split_first()?;
            if !head.eq_ignore_ascii_case(word) {
                return None;
            }
            r = tail;
        }
        Some(r)
    };
    let plain = |args: Option<&'a [String]>| args.map(|a| (a, false));
    match exe.to_ascii_lowercase().as_str() {
        "npx" | "bunx" | "uvx" => Some((rest, false)),
        "npm" => plain(after(&["exec"])),
        "pnpm" => plain(after(&["dlx"])),
        "uv" => plain(after(&["tool", "run"]).or_else(|| after(&["run"]))),
        "deno" => plain(after(&["run"])),
        "docker" => after(&["run"])
            .or_else(|| after(&["container", "run"]))
            .map(|a| (a, true)),
        _ => None,
    }
}

/// Skips the leading flags of `args` (`-y`, `--yes`, `--`, ...) and splits at
/// the first word that is not one. For `docker run`, a flag listed in
/// [`DOCKER_VALUE_FLAGS`] also takes the word after it.
fn first_non_flag(args: &[String], is_docker: bool) -> Launch {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if !(arg.starts_with('-') && arg.len() > 1) {
            break;
        }
        let takes_value = is_docker && DOCKER_VALUE_FLAGS.contains(&arg.as_str());
        i += if takes_value { 2 } else { 1 };
    }
    match args.get(i) {
        Some(program) => Launch {
            program: program.clone(),
            args: args[i + 1..].to_vec(),
        },
        None => Launch::default(),
    }
}

/// For a config argument with a `${...}` placeholder, the text after the last
/// `}` (empty if there is none); `None` if it has no placeholder.
fn placeholder_suffix(arg: &str) -> Option<&str> {
    arg.contains("${")
        .then(|| arg.rsplit_once('}').map_or("", |(_, suffix)| suffix))
}

/// `path` with `\` written as `/`, so Windows and Unix spellings compare equal.
fn forward_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

/// Whether the last path component of `name` contains `mcp`, ignoring case.
fn mentions_mcp(name: &str) -> bool {
    file_name(name)
        .as_bytes()
        .windows(3)
        .any(|w| w.eq_ignore_ascii_case(b"mcp"))
}

/// `program` without a trailing `@version`: `@scope/pkg@1.0` is `@scope/pkg`.
fn strip_version(program: &str) -> &str {
    // The first `@` of a scoped package starts the scope, not a version.
    let from = usize::from(program.starts_with('@'));
    match program[from..].find('@') {
        Some(at) => &program[..from + at],
        None => program,
    }
}

/// The script a `node`, `bun` or `deno` command line runs: its first argument
/// that is not a flag, ignoring `run` for bun and deno. `None` if there is no
/// script (the REPL, `-e code`).
fn script_arg<'a>(runner: &str, args: &'a [String]) -> Option<&'a str> {
    let mut skip_run =
        !runner.eq_ignore_ascii_case("node") && !runner.eq_ignore_ascii_case("nodejs");
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-e" | "--eval" | "-p" | "--print" => return None,
            "-r" | "--require" | "--import" | "--loader" | "--experimental-loader" => {
                it.next();
            }
            "run" if skip_run => skip_run = false,
            a if a.starts_with('-') => {}
            a => return Some(a),
        }
    }
    None
}

/// What an interpreter runs: a script path or a Python module name.
#[derive(Clone, Copy)]
enum Target<'a> {
    Script(&'a str),
    Module(&'a str),
}

impl<'a> Target<'a> {
    fn text(self) -> &'a str {
        match self {
            Target::Script(s) | Target::Module(s) => s,
        }
    }
}

/// Whether `program` (a file name without extension) is `python`, `python3`,
/// `python3.12`, ...
fn is_python(program: &str) -> bool {
    program
        .to_ascii_lowercase()
        .strip_prefix("python")
        .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The script or `-m` module that the interpreter `program` runs, for `node`,
/// `nodejs`, `bun`, `deno` and Python. `None` if `program` is none of those,
/// there is nothing to run (the REPL, `-e code`, `-c code`), or the text holds
/// whitespace, so it is no file or module name.
fn interpreter_target<'a>(program: &str, args: &'a [String]) -> Option<Target<'a>> {
    let target = if is_python(program) {
        python_target(args)?
    } else if SCRIPT_RUNNERS
        .iter()
        .any(|r| program.eq_ignore_ascii_case(r))
    {
        Target::Script(script_arg(program, args)?)
    } else {
        return None;
    };
    (!target.text().contains(char::is_whitespace)).then_some(target)
}

/// The module after `-m`, or else the first argument that is not a flag.
fn python_target(args: &[String]) -> Option<Target<'_>> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-m" => return it.next().map(|m| Target::Module(m)),
            "-c" => return None,
            "-W" | "-X" => {
                it.next();
            }
            a if a.starts_with('-') => {}
            a => return Some(Target::Script(a)),
        }
    }
    None
}

/// The file name of a script path without its extension.
fn script_stem(script: &str) -> &str {
    let file = file_name(script);
    match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    }
}

/// The name of a script path: its file name without extension, or the name of
/// its directory for the generic names in [`GENERIC_SCRIPT_NAMES`].
fn script_name(script: &str) -> &str {
    let stem = script_stem(script);
    if GENERIC_SCRIPT_NAMES
        .iter()
        .any(|g| stem.eq_ignore_ascii_case(g))
        && let Some(dir) = script
            .rsplit(['/', '\\'])
            .nth(1)
            .filter(|d| !matches!(*d, "" | "." | ".."))
    {
        return dir;
    }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpScope;
    use crate::model::Tool;
    use crate::testkit::p;

    fn v(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }
    fn cfg(cmd: &str, args: &str) -> McpServerConfig {
        McpServerConfig {
            tool: Tool::ClaudeCode,
            name: "n".into(),
            command: cmd.into(),
            args: if args.is_empty() { vec![] } else { v(args) },
            scope: McpScope::User,
        }
    }

    #[test]
    fn npx_and_npm_exec_normalize_equally() {
        assert_eq!(
            normalize_launch(&v(
                "npx -y chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222"
            )),
            normalize_launch(&v(
                "npm exec chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222"
            ))
        );
        assert_eq!(
            normalize_launch(&v("cmd.exe /c npx -y @playwright/mcp@latest")).program,
            "@playwright/mcp@latest"
        );
        assert_eq!(
            normalize_launch(&v("C:\\nodejs\\npx.cmd --yes @playwright/mcp@latest")).program,
            "@playwright/mcp@latest"
        );
    }

    #[test]
    fn config_matching_distinguishes_versions() {
        let cdp = cfg(
            "npx",
            "-y chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222",
        );
        let live = cfg("npx", "chrome-devtools-mcp@1.6.0 --autoConnect");
        let p1 = v("npm exec chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222");
        let p2 = v("npm exec chrome-devtools-mcp@1.6.0 --autoConnect");
        assert!(config_matches(&cdp, &p1) && !config_matches(&cdp, &p2));
        assert!(config_matches(&live, &p2) && !config_matches(&live, &p1));
    }

    #[test]
    fn config_args_with_placeholders_are_skipped() {
        let c = cfg("node", "${CLAUDE_PLUGIN_ROOT}/server.js --stdio");
        assert!(config_matches(
            &c,
            &v("node /x/plugins/p/server.js --stdio")
        ));
    }

    #[test]
    fn fallback_names() {
        for (a, n) in [
            ("npm exec @playwright/mcp@latest", "playwright"),
            ("npm exec chrome-devtools-mcp@latest", "chrome-devtools"),
            (
                "npm exec @modelcontextprotocol/server-sequential-thinking",
                "sequential-thinking",
            ),
            ("npm exec @upstash/context7-mcp --api-key x", "context7"),
            ("uvx mcp-server-fetch", "fetch"),
        ] {
            assert_eq!(mcp_fallback_name(&v(a)), n, "{a}");
        }
    }

    #[test]
    fn mcp_detection() {
        assert!(
            looks_like_mcp(&v("npm exec foo"))
                && looks_like_mcp(&v("uvx bar"))
                && looks_like_mcp(&v("/opt/x/my-mcp --stdio"))
        );
        assert!(
            !looks_like_mcp(&v("/bin/zsh -c ls"))
                && !looks_like_mcp(&v("node /x/pyright-langserver --stdio"))
        );
    }

    #[test]
    fn wrappers_and_display_names() {
        assert!(
            is_wrapper(&p(1, 0, 1, &["npm", "exec", "x"]))
                && is_wrapper(&p(1, 0, 1, &["C:\\Windows\\System32\\conhost.exe"]))
        );
        assert!(!is_wrapper(&p(1, 0, 1, &["chrome-devtools-mcp"])));
        assert_eq!(
            display_name(&p(
                1,
                0,
                1,
                &[
                    "/n/node",
                    "/x/chrome-devtools-mcp/build/src/telemetry/watchdog/main.js",
                    "--parent-pid=5"
                ]
            )),
            "watchdog"
        );
        assert_eq!(
            display_name(&p(1, 0, 1, &["node", "/x/.bin/playwright-mcp"])),
            "playwright-mcp"
        );
        assert_eq!(
            display_name(&p(1, 0, 1, &["chrome-devtools-mcp"])),
            "chrome-devtools-mcp"
        );
    }

    fn launch(program: &str, args: &str) -> Launch {
        Launch {
            program: program.into(),
            args: if args.is_empty() { vec![] } else { v(args) },
        }
    }

    #[test]
    fn every_launcher_is_stripped() {
        for (argv, program, args) in [
            ("pnpm dlx -y @foo/mcp --x", "@foo/mcp", "--x"),
            ("bunx --bun @foo/mcp", "@foo/mcp", ""),
            (
                "uvx mcp-server-fetch --ignore-robots-txt",
                "mcp-server-fetch",
                "--ignore-robots-txt",
            ),
            ("uv tool run mcp-server-fetch", "mcp-server-fetch", ""),
            ("uv run --quiet mcp-server-fetch", "mcp-server-fetch", ""),
            ("deno run -A npm:@foo/mcp", "npm:@foo/mcp", ""),
            ("npx -y -- @foo/mcp --stdio", "@foo/mcp", "--stdio"),
            ("npm exec -- @foo/mcp", "@foo/mcp", ""),
            ("/usr/local/bin/npx @foo/mcp", "@foo/mcp", ""),
            ("NPX.CMD @foo/mcp", "@foo/mcp", ""),
        ] {
            assert_eq!(normalize_launch(&v(argv)), launch(program, args), "{argv}");
        }
    }

    #[test]
    fn cmd_prefix_variants() {
        for argv in [
            "CMD.EXE /c npx -y pkg --x",
            "cmd /s /c npx -y pkg --x",
            r"C:\Windows\System32\cmd.exe /d /s /c npx -y pkg --x",
            r"C:\Windows\System32\cmd.exe /C C:\nodejs\npx.cmd --yes pkg --x",
        ] {
            assert_eq!(normalize_launch(&v(argv)), launch("pkg", "--x"), "{argv}");
        }
        // Not running a command: `cmd` is the program.
        assert_eq!(normalize_launch(&v("cmd.exe")), launch("cmd", ""));
        assert_eq!(normalize_launch(&v("cmd /k dir")), launch("cmd", "/k dir"));
    }

    #[test]
    fn docker_run_program_is_the_image() {
        assert_eq!(
            normalize_launch(&v(
                "docker run -i --rm -e GITHUB_TOKEN -v /a:/b --name=x ghcr.io/github/github-mcp-server --stdio"
            )),
            launch("ghcr.io/github/github-mcp-server", "--stdio")
        );
        assert_eq!(
            normalize_launch(&v("docker container run -it --network host img:1 serve")),
            launch("img:1", "serve")
        );
        // `docker ps` is not a launcher.
        assert_eq!(
            normalize_launch(&v("docker ps -a")),
            launch("docker", "ps -a")
        );
    }

    #[test]
    fn plain_commands_keep_their_arguments() {
        assert_eq!(
            normalize_launch(&v(r"C:\Program\nodejs\node.EXE C:\x\server.js --stdio")),
            launch("node", r"C:\x\server.js --stdio")
        );
        assert_eq!(
            normalize_launch(&v("npm install")),
            launch("npm", "install")
        );
        assert_eq!(normalize_launch(&[]), Launch::default());
        assert_eq!(normalize_launch(&v("npx")), Launch::default());
        assert_eq!(normalize_launch(&v("npx -y")), Launch::default());
    }

    #[test]
    fn config_matching_ignores_launcher_spelling() {
        let c = cfg("cmd", "/c npx -y @playwright/mcp@latest");
        assert!(config_matches(&c, &v("npm exec @playwright/mcp@latest")));
        let c = cfg("npx", "-y @playwright/mcp@latest");
        assert!(config_matches(
            &c,
            &v(r"cmd.exe /c C:\nodejs\npx.cmd --yes @playwright/mcp@latest")
        ));
        assert!(!config_matches(&c, &v("npm exec @playwright/mcp@1.0.0")));
        // Program comparison ignores ASCII case, as Windows file names do.
        let c = cfg(r"C:\Program Files\nodejs\node.exe", "server.js");
        assert!(config_matches(
            &c,
            &v(r"C:\PROGRA~1\NODEJS\NODE.EXE server.js")
        ));
    }

    #[test]
    fn config_args_must_appear_in_order() {
        let c = cfg("npx", "-y pkg --a 1 --b");
        assert!(config_matches(
            &c,
            &v("npm exec pkg --extra --a 1 --mid --b --tail")
        ));
        assert!(!config_matches(&c, &v("npm exec pkg --b --a 1")));
        assert!(!config_matches(&c, &v("npm exec pkg --a 1")));
        // Secrets are compared as plain strings.
        let c = cfg("npx", "-y @upstash/context7-mcp --api-key k1");
        assert!(config_matches(
            &c,
            &v("npm exec @upstash/context7-mcp --api-key k1")
        ));
        assert!(!config_matches(
            &c,
            &v("npm exec @upstash/context7-mcp --api-key k2")
        ));
    }

    #[test]
    fn config_placeholders_compare_only_the_suffix() {
        let c = cfg("node", "${CLAUDE_PLUGIN_ROOT}/dist/server.js --stdio");
        assert!(config_matches(
            &c,
            &v("node /h/plugins/p/dist/server.js --stdio")
        ));
        assert!(config_matches(
            &c,
            &v(r"node C:\h\plugins\p\dist\server.js --stdio")
        ));
        assert!(!config_matches(
            &c,
            &v("node /h/plugins/p/dist/other.js --stdio")
        ));
        // Nothing after the placeholder: the argument is not compared.
        let c = cfg("node", "server.js ${X} --stdio");
        assert!(config_matches(&c, &v("node server.js --stdio")));
        assert!(!config_matches(&c, &v("node server.js")));
    }

    #[test]
    fn config_matching_is_a_per_config_predicate() {
        // Two configs may share a name; each is judged by its own command.
        let a = cfg("npx", "-y @modelcontextprotocol/server-sequential-thinking");
        let mut b = a.clone();
        b.scope = McpScope::Plugin("p".into());
        b.command = "node".into();
        b.args = v("${CLAUDE_PLUGIN_ROOT}/seq.js");
        let proc_ = v("npm exec @modelcontextprotocol/server-sequential-thinking");
        assert!(config_matches(&a, &proc_) && !config_matches(&b, &proc_));
    }

    #[test]
    fn empty_programs_never_match() {
        assert!(!config_matches(&cfg("", ""), &[]));
        assert!(!config_matches(&cfg("npx", ""), &v("npx")));
        assert!(!config_matches(&cfg("node", "a.js"), &[]));
    }

    #[test]
    fn more_fallback_names() {
        for (a, n) in [
            ("npx -y @foo/mcp-server", "foo"),
            ("npx -y @foo/mcp", "foo"),
            ("npx -y @foo/server-mcp", "foo"),
            ("npm exec mcp", "mcp"),
            ("npm exec @foo/bar@2.0.0", "bar"),
            ("npm exec some-mcp-server", "some"),
            ("uvx mcp-server-time@latest", "time"),
            (
                "docker run -i --rm ghcr.io/github/github-mcp-server:latest",
                "github",
            ),
            ("/opt/x/my-mcp --stdio", "my"),
            (r"C:\x\tools\notes-mcp.exe --stdio", "notes"),
            ("npx", "mcp"),
        ] {
            assert_eq!(mcp_fallback_name(&v(a)), n, "{a}");
        }
        assert_eq!(mcp_fallback_name(&[]), "mcp");
    }

    #[test]
    fn more_mcp_detection() {
        for yes in [
            "docker run -i --rm img",
            "cmd.exe /c npx -y pkg",
            "node /x/.bin/playwright-mcp",
            "python -m my_MCP_server",
            "python /x/MCP-Server.py",
            "deno run -A main.ts",
        ] {
            assert!(looks_like_mcp(&v(yes)), "{yes}");
        }
        for no in [
            "node /x/server.js --stdio",
            "/usr/bin/python3 -m http.server",
            "/bin/zsh -c ls",
            "cmd.exe /c dir",
            "npm install",
            "node /home/mcp/index.js",
        ] {
            assert!(!looks_like_mcp(&v(no)), "{no}");
        }
        assert!(!looks_like_mcp(&[]));
    }

    fn words(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mcp_detection_ignores_argument_values() {
        for no in [
            &["zsh", "-c", "rg mcpServers src/"][..],
            &["claude", "--mcp-config", "x.json"],
            &["claude", "--strict-mcp-config"],
            &["/bin/sh", "-c", "~/.claude/hooks/mcp-guard.sh"],
            &["rg", "mcpServers", "src/"],
            &["cat", "/x/mcp.json"],
            // An interpreter's own arguments: flags and strings with spaces.
            &["node", "--mcp-flag", "/x/server.js"],
            &["node", "/x/my mcp/server.js"],
            &["node", "-e", "mcp()"],
            &["python3", "-c", "import mcp"],
            &["python3", "/x/server.py", "--mcp"],
            &["python", "-m", "http.server", "mcp"],
            &["node"],
        ] {
            assert!(!looks_like_mcp(&words(no)), "{no:?}");
        }
        for yes in [
            &["node", "/x/.bin/playwright-mcp"][..],
            &["python3", "/v/bin/mcp-server-fetch"],
            &["python", "-m", "mcp_server_time"],
            &["/opt/x/my-mcp", "--stdio"],
            &["/usr/bin/python3.12", "-u", "/v/bin/mcp-server-git"],
            &["C:\\Python\\python.exe", "-m", "mcp_server_time"],
            &["bun", "run", "/x/mcp-server.ts"],
            &["node", "--max-old-space-size=64", "/x/MCP.js"],
            &["npx", "-y", "anything"],
        ] {
            assert!(looks_like_mcp(&words(yes)), "{yes:?}");
        }
    }

    #[test]
    fn fallback_name_of_an_interpreted_script() {
        for (argv, want) in [
            (&["node", "/x/.bin/playwright-mcp"][..], "playwright"),
            (&["python3", "/v/bin/mcp-server-fetch"], "fetch"),
            (&["python", "-m", "mcp_server_time"], "time"),
            (
                &["/usr/bin/python3.12", "-u", "/v/bin/mcp-server-git"],
                "git",
            ),
            (&["bun", "run", "/x/dist/context7-mcp.js"], "context7"),
            (&["node", "--inspect", "/x/server-memory"], "memory"),
            // Not an interpreter, or no script to name: the program itself.
            (&["/opt/x/my-mcp", "--stdio"], "my"),
            (&["node"], "node"),
            (&["python3", "-c", "import mcp"], "python3"),
            (&["node", "/x/my mcp/server.js"], "node"),
            // A launcher still names the package.
            (&["npx", "-y", "@foo/mcp"], "foo"),
        ] {
            assert_eq!(mcp_fallback_name(&words(argv)), want, "{argv:?}");
        }
    }

    #[test]
    fn rewritten_title_argv_works_end_to_end() {
        // What sysinfo reports for `npm exec ...` on macOS, after collection.
        let raw = words(&[
            "npm exec chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222",
            "HOME=/Users/u",
            "LOGNAME=u",
            "PATH=/usr/bin:/bin",
        ]);
        let argv = crate::collect::normalize_argv(raw);
        let proc_ = p(
            1,
            0,
            1,
            &argv.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let c = cfg(
            "npx",
            "-y chrome-devtools-mcp@latest --browserUrl http://127.0.0.1:9222",
        );
        assert!(config_matches(&c, &proc_.argv));
        assert!(!config_matches(
            &cfg("npx", "chrome-devtools-mcp@1.6.0 --autoConnect"),
            &proc_.argv
        ));
        assert!(is_wrapper(&proc_));
        assert_eq!(display_name(&proc_), "npm");
        assert_eq!(mcp_fallback_name(&proc_.argv), "chrome-devtools");
        assert!(looks_like_mcp(&proc_.argv));
    }

    #[test]
    fn more_wrappers() {
        for yes in [
            &["npx", "-y", "pkg"][..],
            &["/usr/bin/NPX.cmd"],
            &["cmd.exe", "/c", "npx", "pkg"],
            &["CMD", "/S", "/C", "node", "x.js"],
            &["C:\\Windows\\System32\\OpenConsole.exe", "--headless"],
            &["conhost.exe", "0x4"],
            &["npm", "EXEC", "x"],
        ] {
            assert!(is_wrapper(&p(1, 0, 1, yes)), "{yes:?}");
        }
        for no in [
            &["npm", "install"][..],
            &["npm"],
            &["cmd.exe"],
            &["cmd.exe", "/k", "dir"],
            &["node", "x.js"],
            &["/bin/zsh", "-c", "ls"],
            &["conhost-helper"],
        ] {
            assert!(!is_wrapper(&p(1, 0, 1, no)), "{no:?}");
        }
        // The argv is unreadable but the name still says it is a console host.
        assert!(is_wrapper(&p(1, 0, 1, &[]).name("conhost.exe")));
        assert!(!is_wrapper(&p(1, 0, 1, &[]).name("node")));
    }

    #[test]
    fn more_display_names() {
        for (argv, want) in [
            (&["node", "/x/server/index.js"][..], "server"),
            (&["node", "/x/app/cli.mjs", "--x"], "app"),
            (&["node", "--max-old-space-size=4096", "/x/run.js"], "run"),
            (
                &["node", "-r", "ts-node/register", "/x/mcp-server.ts"],
                "mcp-server",
            ),
            (&["node", "-e", "console.log(1)"], "node"),
            (&["node"], "node"),
            (&["bun", "run", "/x/y/serve.ts"], "serve"),
            (&["deno", "run", "-A", "/x/tool/main.ts"], "tool"),
            (&["node", "main.js"], "main"),
            (&["node", "./index.js"], "index"),
            (&["node", "/main.js"], "main"),
            (
                &[r"C:\Program Files\nodejs\node.exe", r"C:\x\app\main.js"],
                "app",
            ),
            (&["/usr/bin/python3", "/x/server.py"], "python3"),
            (&["/Users/u/.nvm/bin/NODE", "/x/a.js"], "a"),
            (&["C:\\x\\Tool.EXE", "arg"], "Tool"),
            (&["/bin/zsh", "-c", "ls"], "zsh"),
        ] {
            assert_eq!(display_name(&p(1, 0, 1, argv)), want, "{argv:?}");
        }
    }

    #[test]
    fn display_name_is_never_empty() {
        // argv unreadable: use exe, then name.
        let r = p(1, 0, 1, &[]).exe("/opt/x/tool");
        assert_eq!(display_name(&r), "tool");
        assert_eq!(display_name(&p(1, 0, 1, &[]).name("foo.exe")), "foo");
        assert_eq!(display_name(&p(1, 0, 1, &[]).name("")), "?");
        assert_eq!(display_name(&p(1, 0, 1, &[]).name("  ")), "?");
        assert_eq!(display_name(&p(1, 0, 1, &["", "x"]).name("kept")), "kept");
        assert_eq!(display_name(&p(1, 0, 1, &["/"]).name("")), "?");
    }
}
