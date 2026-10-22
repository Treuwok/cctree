//! Test-only data for the renderer's snapshots (and Task 20's tests): one
//! machine close to the spec 4.6 mockup.
//!
//! - two Claude Code sessions, `~/prj/cctree` (with three MCPs and some other
//!   children) and one whose working directory has a very long Chinese name;
//! - memory split into "in RAM" and "pushed out" on every process, with an
//!   "other" part (the memory sentence of the mockup) on `chrome-devtools-mcp`
//!   of `chrome-live` and on the second session;
//! - the user MCPs `chrome-cdp` and `chrome-live`, and the plugin MCP
//!   `playwright`;
//! - one suspected orphan, `ChatGPT Meetings`, and one other Node.js process;
//! - cctree itself, so the header has its own memory to show.
//!
//! No Codex session runs, so that category reads "not running".

use std::collections::HashSet;

use crate::classify::{Classification, classify};
use crate::config::{McpConfigs, McpScope, McpServerConfig};
use crate::kill::{KillPlan, KillScope, plan_kill};
use crate::model::{MemInfo, Platform, ProcKey, RawProc, Snapshot, SystemInfo, Tool};
use crate::testkit::{p, snap};
use crate::tree::{Node, NodeId, ViewMode, build_tree};

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

/// A process using `mem_mb` MB: 70% of it in RAM, 30% pushed out, and a
/// shared part of a tenth, so the two sides of the memory sentence are equal.
fn proc_(pid: u32, ppid: u32, start: u64, argv: &[&str], mem_mb: u64, cpu: f32) -> RawProc {
    let mut r = p(pid, ppid, start, argv);
    r.cpu = Some(cpu);
    r.mem = Some(MemInfo {
        private: Some(mem_mb * MB),
        in_ram: Some(mem_mb * MB * 7 / 10),
        swapped_out: Some(mem_mb * MB - mem_mb * MB * 7 / 10),
        shared: Some(mem_mb * MB / 10),
        os_tool_value: Some(mem_mb * MB),
        ..Default::default()
    });
    r
}

/// `r` with its memory split as given (MB). What is left of the main figure
/// is the "other" part of the memory sentence.
fn split(mut r: RawProc, in_ram: u64, swapped: u64, shared: u64) -> RawProc {
    if let Some(m) = r.mem.as_mut() {
        m.in_ram = Some(in_ram * MB);
        m.swapped_out = Some(swapped * MB);
        m.shared = Some(shared * MB);
    }
    r
}

fn server(name: &str, args: &[&str], scope: McpScope) -> McpServerConfig {
    McpServerConfig {
        tool: Tool::ClaudeCode,
        name: name.into(),
        command: "npx".into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        scope,
    }
}

/// The snapshot, its classification and the by-session tree, taken 15 minutes
/// after the sessions started (`taken_at` is the time to give the renderer).
pub fn mock_fixture() -> (Snapshot, Classification, Vec<Node>) {
    let cdp = "--browserUrl=http://127.0.0.1:9222";
    let watchdog = "/x/chrome-devtools-mcp/build/telemetry/watchdog/main.js";
    let procs = vec![
        proc_(1, 0, 1, &["/sbin/launchd"], 12, 0.0),
        proc_(100, 1, 10, &["/bin/zsh"], 8, 0.0),
        // Session 1: ~/prj/cctree.
        proc_(49049, 100, 20, &["claude"], 550, 0.4).cwd("/Users/u/prj/cctree"),
        // chrome-cdp.
        proc_(
            49721,
            49049,
            21,
            &["npm", "exec", "chrome-devtools-mcp@latest", cdp],
            80,
            0.0,
        ),
        proc_(49722, 49721, 22, &["chrome-devtools-mcp", cdp], 150, 0.0),
        proc_(
            49723,
            49722,
            23,
            &["node", watchdog, "--parent-pid=49722"],
            100,
            0.0,
        ),
        // chrome-live: the server is the one the memory sentence is checked on.
        proc_(
            49815,
            49049,
            24,
            &["npm", "exec", "chrome-devtools-mcp@latest", "--autoConnect"],
            97,
            0.0,
        ),
        split(
            proc_(
                50094,
                49815,
                25,
                &["chrome-devtools-mcp", "--autoConnect"],
                162,
                0.0,
            ),
            112,
            42,
            15,
        ),
        proc_(
            50485,
            50094,
            26,
            &["node", watchdog, "--parent-pid=50094"],
            154,
            0.0,
        ),
        // playwright, a plugin MCP.
        proc_(
            49769,
            49049,
            27,
            &["npm", "exec", "@playwright/mcp@latest"],
            80,
            0.0,
        ),
        proc_(
            49770,
            49769,
            28,
            &["node", "/x/.bin/playwright-mcp"],
            135,
            0.0,
        ),
        // Other children: a shell and the language server it started.
        proc_(51010, 49049, 30, &["/bin/zsh", "-c", "ls"], 6, 0.1),
        proc_(
            51011,
            51010,
            31,
            &["typescript-language-server", "--stdio"],
            6,
            0.0,
        ),
        // Session 2, whose working directory is long.
        split(
            proc_(52091, 100, 40, &["claude"], 545, 0.0)
                .cwd("/Users/u/prj/非常非常長的中文專案目錄名稱/子資料夾"),
            380,
            150,
            40,
        ),
        proc_(52092, 52091, 41, &["/bin/zsh", "-c", "pwd"], 6, 0.0),
        // Not started by any session: a suspected orphan and a dev server.
        proc_(
            80871,
            1,
            5,
            &["/private/var/folders/x/T/codex-home-Z/r/ChatGPT Meetings"],
            102,
            0.2,
        ),
        proc_(60001, 1, 60, &["node", "/Users/u/prj/web/vite.js"], 88, 0.0),
        // cctree itself.
        proc_(90001, 100, 900, &["cctree"], 12, 0.5),
    ];
    let self_key = procs.last().expect("cctree is the last process").key;
    let mut s = snap(Platform::MacOs, procs);
    s.self_key = self_key;
    s.taken_at = 925;
    s.system = SystemInfo {
        ram_used: 27 * GB,
        ram_total: 36 * GB,
        swap_used: 2355 * MB,
        swap_total: 8 * GB,
        cpu_pct: Some(12.0),
        logical_cpus: 10,
    };
    let cfgs = McpConfigs {
        servers: vec![
            server(
                "chrome-cdp",
                &["-y", "chrome-devtools-mcp@latest", cdp],
                McpScope::User,
            ),
            server(
                "chrome-live",
                &["-y", "chrome-devtools-mcp@latest", "--autoConnect"],
                McpScope::User,
            ),
            server(
                "playwright",
                &["@playwright/mcp@latest"],
                McpScope::Plugin("playwright".into()),
            ),
        ],
        problems: vec![],
    };
    let c = classify(&s, &cfgs);
    let t = build_tree(&s, &c, ViewMode::BySession);
    (s, c, t)
}

/// `X` on the `chrome-live` MCP of the first session: three processes, and
/// the warning that Claude Code will show the MCP as disconnected.
pub fn mock_mcp_plan() -> KillPlan {
    let (s, c, t) = mock_fixture();
    plan_kill(&s, &c, &t, &NodeId::Mcp(key(&s, 49815)), KillScope::Tree)
        .expect("the chrome-live MCP can be ended")
}

/// The key of the process `pid` of the fixture.
pub fn key(s: &Snapshot, pid: u32) -> ProcKey {
    s.index()[&pid].key
}

/// Every id in `ns` and below, for expanding the whole tree.
pub fn all_ids(ns: &[Node], out: &mut HashSet<NodeId>) {
    for n in ns {
        out.insert(n.id.clone());
        all_ids(&n.children, out);
    }
}
