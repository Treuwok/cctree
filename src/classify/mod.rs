//! Turns a raw [`Snapshot`] into the process roles the
//! tree is built from: which processes are Claude Code / Codex sessions, which
//! are their MCP servers (and what each member of an MCP node does), and which
//! are other children of a session. A second pass over what is left
//! (`orphan`) finds the detached, orphaned and other Node.js processes; every
//! process left without a role is [`Role::Unrelated`].
//!
//! [`classify`] walks the validated process forest ([`Lineage`]) once, top
//! down in pre-order, carrying where the walk is: outside every session, at a
//! direct child of a session, inside an MCP node, or inside another child's
//! subtree. Each process is looked at once, so a pass is linear in the number
//! of processes (plus config matching for the direct children of sessions).
//!
//! The rules (spec §4.3), with the choices the spec leaves open:
//!
//! - **Session**: a process [`detect_tool`] recognises, outside an MCP node,
//!   is the root of a session; `outer` is the root of the nearest enclosing
//!   session. Exceptions, for a direct child of a session:
//!   - it matches one of the session tool's MCP configs: it is an MCP node
//!     (`codex mcp-server` configured as a Claude Code MCP);
//!   - its parent is the npm `node` shell of the same tool: it is that shell's
//!     native binary, the same session. It gets [`Role::OtherChild`], but its
//!     children count as direct children of the session, so the MCP servers of
//!     an npm-installed Codex are still found.
//! - **MCP node**: a direct child of a session that matches one of the session
//!   tool's MCP configs, or that [`looks_like_mcp`], is the head of an MCP
//!   node, which takes in every descendant of the head, tool processes
//!   included (`npx @openai/codex mcp-server` runs `codex` inside the node; it
//!   is that MCP, not a nested session). Each member's part: a wrapper
//!   ([`is_wrapper`]) is [`McpPart::Wrapper`], the first other member in
//!   pre-order (the head itself when it is no wrapper) is [`McpPart::Server`],
//!   the rest are [`McpPart::Helper`].
//! - **Config name**: when several configs match, the most specific for this
//!   session wins: a project config of this session (its cwd, or the
//!   `CLAUDE_PROJECT_DIR` the head was given; a worktree session runs in the
//!   worktree but gets the main checkout there), then a plugin, then the
//!   user's, then another project's; among equals the first in config order.
//!   Without a config the node is named by [`mcp_fallback_name`].
//! - **Other child**: any other direct child of a session and its subtree,
//!   down to (not including) the root of a nested session.
//! - **Claude Code session id**: the `CLAUDE_CODE_SESSION_ID` of the first
//!   process, in pre-order, that belongs to the session (its root excluded:
//!   the root only inherits the value of whoever started it). Nested
//!   sessions' processes belong to the nested session. A Codex session has
//!   none: its processes only carry the value inherited from an enclosing
//!   Claude Code session.

pub mod argv;
pub mod lineage;
mod orphan;
pub mod rules;

use std::collections::{HashMap, HashSet};

use crate::config::{McpConfigs, McpScope, McpServerConfig};
use crate::model::{Pid, ProcKey, RawProc, Snapshot, Tool};

use argv::{config_matches, is_wrapper, looks_like_mcp, mcp_fallback_name};
use lineage::Lineage;
use rules::{ToolHit, detect_tool};

/// Set by Claude Code on the processes it starts.
const SESSION_ID_ENV: &str = "CLAUDE_CODE_SESSION_ID";
/// The project directory Claude Code gives the processes it starts.
const PROJECT_DIR_ENV: &str = "CLAUDE_PROJECT_DIR";

/// The top-level groups of the tree. The declaration order is the fixed order
/// on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    ClaudeCode,
    Codex,
    Orphaned,
    OtherNode,
}

/// What a process does inside an MCP node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum McpPart {
    /// Only starts another program (`npm exec`, `npx`, `cmd /c`, `conhost`).
    Wrapper,
    /// The MCP server itself.
    Server,
    /// Anything else the server started (a telemetry watchdog, ...).
    Helper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Certainty {
    Confirmed,
    Suspected,
}

/// Why a process counts as orphaned.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OrphanReason {
    SessionEnded {
        tool: Tool,
        project_dir: Option<String>,
    },
    RecordedParentGone {
        pid: Pid,
    },
    PathOnly {
        tool: Tool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The root process of a session (see [`SessionInfo`]).
    SessionRoot,
    /// A member of the MCP node `name` of the session rooted at `session`.
    /// `head` is the session's direct child that the node starts at;
    /// `plugin` names the Claude Code plugin whose config defines the server.
    Mcp {
        session: ProcKey,
        name: String,
        head: ProcKey,
        part: McpPart,
        plugin: Option<String>,
    },
    /// Belongs to the session rooted at `session` but is no MCP server.
    OtherChild {
        session: ProcKey,
    },
    Detached {
        session: ProcKey,
    },
    Orphan {
        certainty: Certainty,
        reason: OrphanReason,
    },
    OrphanMember {
        root: ProcKey,
    },
    OtherNode {
        group: String,
    },
    Unrelated,
}

/// One Claude Code or Codex session, identified by its root process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub root: ProcKey,
    pub tool: Tool,
    /// The root process's working directory.
    pub cwd: Option<String>,
    /// The app that started the root (see [`Lineage::launcher`]); `None` when
    /// it was started from a shell.
    pub launcher: Option<String>,
    /// The root runs as `app-server` (`codex app-server`).
    pub app_server: bool,
    /// For a nested session, the root of the nearest enclosing session.
    pub outer: Option<ProcKey>,
    /// See the module documentation. Always `None` for Codex. A resumed
    /// session's members can carry more than one id; this is only the first,
    /// but the detached/orphan pass knows every id seen on a member.
    pub claude_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Classification {
    /// In the order the walk found them: a session comes after the sessions
    /// that enclose it.
    pub sessions: Vec<SessionInfo>,
    /// Processes without an entry are [`Role::Unrelated`].
    pub roles: HashMap<ProcKey, Role>,
}

static UNRELATED: Role = Role::Unrelated;

impl Classification {
    /// The role of `k`; [`Role::Unrelated`] when nothing was recorded.
    pub fn role(&self, k: ProcKey) -> &Role {
        self.roles.get(&k).unwrap_or(&UNRELATED)
    }

    /// The session whose root process is `root`.
    pub fn session(&self, root: ProcKey) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.root == root)
    }

    /// Every process whose role is not [`Role::Unrelated`].
    pub fn tracked(&self) -> HashSet<ProcKey> {
        self.roles
            .iter()
            .filter(|(_, role)| **role != Role::Unrelated)
            .map(|(k, _)| *k)
            .collect()
    }

    /// The category whose totals include `k`. A nested session and its
    /// processes count in the category of their outermost session, whatever
    /// their own tool. `None` for an unrelated process, or for a role that
    /// names a session that is not in [`Classification::sessions`].
    pub fn category_of(&self, k: ProcKey) -> Option<Category> {
        let root = match self.role(k) {
            Role::SessionRoot => k,
            Role::Mcp { session, .. }
            | Role::OtherChild { session }
            | Role::Detached { session } => *session,
            Role::Orphan { .. } | Role::OrphanMember { .. } => return Some(Category::Orphaned),
            Role::OtherNode { .. } => return Some(Category::OtherNode),
            Role::Unrelated => return None,
        };
        let mut session = self.session(root)?;
        // `outer` always names an ancestor, so the chain ends; the bound only
        // guards a hand-made, cyclic classification.
        for _ in 0..self.sessions.len() {
            match session.outer.and_then(|o| self.session(o)) {
                Some(outer) => session = outer,
                None => break,
            }
        }
        Some(match session.tool {
            Tool::ClaudeCode => Category::ClaudeCode,
            Tool::Codex => Category::Codex,
        })
    }
}

/// Where the walk is, as seen by the process being visited.
#[derive(Clone, Copy)]
enum Ctx {
    /// Outside every session.
    Free,
    /// Its parent is the root (or the native body) of `sessions[i]`.
    Direct(usize),
    /// Inside the subtree of another child of `sessions[i]`.
    Other(usize),
    /// Inside the MCP node `mcps[i]`.
    Mcp(usize),
}

/// An MCP node found so far.
struct McpNode {
    /// Index into the sessions.
    session: usize,
    name: String,
    head: ProcKey,
    plugin: Option<String>,
    /// The node's [`McpPart::Server`] has been assigned.
    has_server: bool,
}

/// What visiting one process decided.
struct Visit {
    role: Option<Role>,
    /// The session (index) the process is a member of, root excluded.
    member_of: Option<usize>,
    /// Passed on to every child.
    ctx: Ctx,
    /// The tool signature of a session root or native body, passed on to
    /// every child.
    hit: Option<ToolHit>,
}

/// Finds the sessions, their MCP nodes and their other children. Total: any
/// snapshot gives a result, with no panic. See the module documentation for
/// the rules.
pub fn classify(snap: &Snapshot, configs: &McpConfigs) -> Classification {
    let l = Lineage::build(snap);
    let mut walk = Walk {
        l: &l,
        configs,
        sessions: Vec::new(),
        mcps: Vec::new(),
    };
    let mut roles = HashMap::new();
    let mut visited: HashSet<ProcKey> = HashSet::with_capacity(snap.procs.len());

    // Pre-order over the forest, tops and children in snapshot order. Each
    // entry carries the context and tool signature its parent left.
    let mut stack: Vec<(ProcKey, Ctx, Option<ToolHit>)> = snap
        .procs
        .iter()
        .rev()
        .filter(|p| l.parent(p.key).is_none())
        .map(|p| (p.key, Ctx::Free, None))
        .collect();
    while let Some((k, ctx, parent_hit)) = stack.pop() {
        // A PID listed twice under the same key would be reached twice.
        if !visited.insert(k) {
            continue;
        }
        let p = l.proc_(k);
        let visit = walk.visit(p, ctx, parent_hit);
        if let Some(s) = visit.member_of {
            walk.note_session_id(s, p);
        }
        if let Some(role) = visit.role {
            roles.insert(k, role);
        }
        stack.extend(
            l.children(k)
                .iter()
                .rev()
                .map(|&child| (child, visit.ctx, visit.hit)),
        );
    }

    orphan::assign_rest(snap, &l, &walk.sessions, &mut roles);
    Classification {
        sessions: walk.sessions,
        roles,
    }
}

/// The state of one [`classify`] walk.
struct Walk<'a> {
    l: &'a Lineage<'a>,
    configs: &'a McpConfigs,
    sessions: Vec<SessionInfo>,
    mcps: Vec<McpNode>,
}

impl<'a> Walk<'a> {
    fn visit(&mut self, p: &RawProc, ctx: Ctx, parent_hit: Option<ToolHit>) -> Visit {
        match ctx {
            Ctx::Mcp(m) => self.mcp_member(p, m),
            Ctx::Free => match detect_tool(p) {
                Some(hit) => self.new_session(p, hit, None),
                None => Visit {
                    role: None,
                    member_of: None,
                    ctx: Ctx::Free,
                    hit: None,
                },
            },
            Ctx::Other(s) => match detect_tool(p) {
                Some(hit) => self.new_session(p, hit, Some(s)),
                None => self.other_child(s, Ctx::Other(s)),
            },
            Ctx::Direct(s) => self.direct_child(p, s, parent_hit),
        }
    }

    /// A direct child of `sessions[s]` (or of its native body).
    fn direct_child(&mut self, p: &RawProc, s: usize, parent_hit: Option<ToolHit>) -> Visit {
        let hit = detect_tool(p);
        let config = self.best_config(p, s);
        match hit {
            Some(_) if config.is_some() => self.new_mcp(p, s, config),
            Some(hit) => {
                let body = parent_hit.is_some_and(|h| h.node_wrapper && h.tool == hit.tool);
                if body {
                    Visit {
                        hit: Some(hit),
                        ..self.other_child(s, Ctx::Direct(s))
                    }
                } else {
                    self.new_session(p, hit, Some(s))
                }
            }
            None if config.is_some() || looks_like_mcp(&p.argv) => self.new_mcp(p, s, config),
            None => self.other_child(s, Ctx::Other(s)),
        }
    }

    fn new_session(&mut self, p: &RawProc, hit: ToolHit, outer: Option<usize>) -> Visit {
        let i = self.sessions.len();
        self.sessions.push(SessionInfo {
            root: p.key,
            tool: hit.tool,
            cwd: p.cwd.clone(),
            launcher: self.l.launcher(p.key),
            app_server: p.argv.iter().any(|a| a == "app-server"),
            outer: outer.map(|s| self.sessions[s].root),
            claude_session_id: None,
        });
        Visit {
            role: Some(Role::SessionRoot),
            member_of: None,
            ctx: Ctx::Direct(i),
            hit: Some(hit),
        }
    }

    fn other_child(&self, s: usize, ctx: Ctx) -> Visit {
        Visit {
            role: Some(Role::OtherChild {
                session: self.sessions[s].root,
            }),
            member_of: Some(s),
            ctx,
            hit: None,
        }
    }

    /// Starts an MCP node at `head`, a direct child of `sessions[s]`.
    fn new_mcp(&mut self, head: &RawProc, s: usize, config: Option<&McpServerConfig>) -> Visit {
        let (name, plugin) = match config {
            Some(cfg) => {
                let plugin = match &cfg.scope {
                    McpScope::Plugin(plugin) => Some(plugin.clone()),
                    McpScope::User | McpScope::Project(_) => None,
                };
                (cfg.name.clone(), plugin)
            }
            None => (mcp_fallback_name(&head.argv), None),
        };
        let m = self.mcps.len();
        self.mcps.push(McpNode {
            session: s,
            name,
            head: head.key,
            plugin,
            has_server: false,
        });
        self.mcp_member(head, m)
    }

    fn mcp_member(&mut self, p: &RawProc, m: usize) -> Visit {
        let node = &mut self.mcps[m];
        let part = if is_wrapper(p) {
            McpPart::Wrapper
        } else if node.has_server {
            McpPart::Helper
        } else {
            node.has_server = true;
            McpPart::Server
        };
        Visit {
            role: Some(Role::Mcp {
                session: self.sessions[node.session].root,
                name: node.name.clone(),
                head: node.head,
                part,
                plugin: node.plugin.clone(),
            }),
            member_of: Some(node.session),
            ctx: Ctx::Mcp(m),
            hit: None,
        }
    }

    /// The MCP config of `sessions[s]`'s tool that `p` was started from, the
    /// most specific for this session if several match (see the module
    /// documentation).
    fn best_config(&self, p: &RawProc, s: usize) -> Option<&'a McpServerConfig> {
        let session = &self.sessions[s];
        let own_dirs = [
            session.cwd.as_deref(),
            p.env.get(PROJECT_DIR_ENV).map(String::as_str),
        ];
        self.configs
            .servers
            .iter()
            .filter(|cfg| cfg.tool == session.tool && config_matches(cfg, &p.argv))
            .min_by_key(|cfg| scope_rank(&cfg.scope, own_dirs))
    }

    /// Records the Claude Code session id carried by `p`, a member of
    /// `sessions[s]`, if that session has none yet. Called in pre-order.
    fn note_session_id(&mut self, s: usize, p: &RawProc) {
        let session = &mut self.sessions[s];
        if session.tool == Tool::ClaudeCode
            && session.claude_session_id.is_none()
            && let Some(id) = p.env.get(SESSION_ID_ENV)
        {
            session.claude_session_id = Some(id.clone());
        }
    }
}

/// Lower is more specific for a session working in `own_dirs`.
fn scope_rank(scope: &McpScope, own_dirs: [Option<&str>; 2]) -> u8 {
    match scope {
        McpScope::Project(dir) if own_dirs.into_iter().flatten().any(|d| same_dir(d, dir)) => 0,
        McpScope::Plugin(_) => 1,
        McpScope::User => 2,
        McpScope::Project(_) => 3,
    }
}

/// Whether two directory paths name the same directory, ignoring the
/// separator style, a trailing separator and ASCII case.
fn same_dir(a: &str, b: &str) -> bool {
    !a.is_empty() && dir_key(a) == dir_key(b)
}

/// `d` in the form [`same_dir`] compares: `/` separators, no trailing one,
/// ASCII lowercase.
fn dir_key(d: &str) -> String {
    d.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpScope, McpServerConfig};
    use crate::model::{Platform, RawProc};
    use crate::testkit::{base, cfgs, p, snap};

    #[test]
    fn session_mcp_and_other_children() {
        let s = snap(Platform::MacOs, base());
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.sessions.len(), 1);
        let ses = &c.sessions[0];
        assert_eq!(
            (
                ses.root,
                ses.tool,
                ses.cwd.as_deref(),
                ses.claude_session_id.as_deref()
            ),
            (k(200), Tool::ClaudeCode, Some("/Users/u/prj/a"), Some("S1"))
        );
        assert!(
            matches!(c.role(k(210)), Role::Mcp { name, part: McpPart::Wrapper, head, .. } if name == "chrome-cdp" && *head == k(210))
        );
        assert!(matches!(
            c.role(k(211)),
            Role::Mcp {
                part: McpPart::Server,
                ..
            }
        ));
        assert!(matches!(
            c.role(k(212)),
            Role::Mcp {
                part: McpPart::Helper,
                ..
            }
        ));
        assert!(matches!(c.role(k(221)), Role::Mcp { name, .. } if name == "playwright"));
        assert!(matches!(c.role(k(230)), Role::OtherChild { .. }));
        assert_eq!(c.category_of(k(212)), Some(Category::ClaudeCode));
    }

    #[test]
    fn cross_tool_nesting_counts_in_outermost() {
        let mut v = base();
        v.push(p(300, 230, 40, &["codex", "exec", "hi"]));
        v.push(p(301, 300, 41, &["npm", "exec", "@upstash/context7-mcp"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        let codex = c.session(k(300)).unwrap();
        assert_eq!((codex.tool, codex.outer), (Tool::Codex, Some(k(200))));
        assert!(matches!(c.role(k(301)), Role::Mcp { session, .. } if *session == k(300)));
    }

    #[test]
    fn tool_configured_as_mcp_is_mcp_not_session() {
        let mut cf = cfgs();
        cf.servers.push(McpServerConfig {
            tool: Tool::ClaudeCode,
            name: "codex".into(),
            command: "codex".into(),
            args: vec!["mcp-server".into()],
            scope: McpScope::User,
        });
        let mut v = base();
        v.push(p(310, 200, 50, &["codex", "mcp-server"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cf);
        assert!(matches!(c.role(s.index()[&310].key), Role::Mcp { name, .. } if name == "codex"));
        assert_eq!(c.sessions.len(), 1);
    }

    #[test]
    fn codex_npm_wrapper_and_native_are_one_session() {
        let s = snap(
            Platform::Linux,
            vec![
                p(1, 0, 1, &["/sbin/init"]),
                p(10, 1, 2, &["bash"]),
                p(
                    11,
                    10,
                    3,
                    &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"],
                ),
                p(
                    12,
                    11,
                    4,
                    &["/usr/lib/node_modules/@openai/codex/vendor/codex"],
                ),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        assert_eq!(c.sessions.len(), 1);
        assert!(matches!(
            c.role(s.index()[&12].key),
            Role::OtherChild { .. }
        ));
    }

    #[test]
    fn zombie_with_empty_argv_inside_session_is_other_child() {
        let mut v = base();
        let mut z = p(240, 200, 60, &["x"]).zombie();
        z.argv.clear();
        v.push(z);
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert!(matches!(
            c.role(s.index()[&240].key),
            Role::OtherChild { .. }
        ));
    }

    #[test]
    fn launcher_and_app_server_labels() {
        let s = snap(
            Platform::MacOs,
            vec![
                p(
                    5,
                    1,
                    1,
                    &["/Applications/Visual Studio Code.app/Contents/MacOS/Electron"],
                ),
                p(6, 5, 2, &["claude"]),
                p(7, 5, 2, &["codex", "app-server"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(
            c.session(k(6)).unwrap().launcher.as_deref(),
            Some("Visual Studio Code")
        );
        assert!(c.session(k(7)).unwrap().app_server);
    }

    // ---- Beyond the brief ----

    fn server(name: &str, command: &str, args: &[&str], scope: McpScope) -> McpServerConfig {
        McpServerConfig {
            tool: Tool::ClaudeCode,
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            scope,
        }
    }

    fn mcp_name(c: &Classification, k: ProcKey) -> Option<(&str, Option<&str>)> {
        match c.role(k) {
            Role::Mcp { name, plugin, .. } => Some((name, plugin.as_deref())),
            _ => None,
        }
    }

    #[test]
    fn base_roles_are_complete_and_launcher_is_none_from_a_shell() {
        let s = snap(Platform::MacOs, base());
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        let ses = &c.sessions[0];
        assert_eq!(
            (ses.launcher.as_deref(), ses.outer, ses.app_server),
            (None, None, false)
        );
        assert_eq!(c.role(k(200)), &Role::SessionRoot);
        // Every member of an MCP node shares the node's name and head.
        for (pid, head, part) in [
            (210, 210, McpPart::Wrapper),
            (211, 210, McpPart::Server),
            (212, 210, McpPart::Helper),
            (220, 220, McpPart::Wrapper),
            (221, 220, McpPart::Server),
        ] {
            let Role::Mcp {
                session,
                head: h,
                part: got,
                plugin,
                ..
            } = c.role(k(pid))
            else {
                panic!("{pid} is not an MCP member: {:?}", c.role(k(pid)));
            };
            assert_eq!((*session, *h, *got, plugin), (k(200), k(head), part, &None));
        }
        assert_eq!(c.role(k(230)), &Role::OtherChild { session: k(200) });
        // The login shell and launchd stay unrelated after both passes.
        assert_eq!(c.role(k(100)), &Role::Unrelated);
        assert_eq!(c.role(k(1)), &Role::Unrelated);
        assert_eq!(c.category_of(k(100)), None);
        let want: HashSet<ProcKey> = [200, 210, 211, 212, 220, 221, 230].map(k).into();
        assert_eq!(c.tracked(), want);
    }

    #[test]
    fn mcp_without_launcher_head_is_the_server() {
        let mut v = base();
        v.push(p(250, 200, 60, &["node", "/x/.bin/my-mcp"]));
        v.push(p(251, 250, 61, &["node", "/x/worker.js"]));
        v.push(p(260, 200, 60, &["/usr/bin/caffeinate", "-i", "-t", "300"]));
        v.push(p(
            270,
            200,
            60,
            &["node", "/x/pyright-langserver", "--stdio"],
        ));
        v.push(p(271, 270, 61, &["node", "/x/tsserver.js"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert!(
            matches!(c.role(k(250)), Role::Mcp { name, part: McpPart::Server, .. } if name == "my")
        );
        assert!(matches!(
            c.role(k(251)),
            Role::Mcp {
                part: McpPart::Helper,
                ..
            }
        ));
        for pid in [260, 270, 271] {
            assert_eq!(c.role(k(pid)), &Role::OtherChild { session: k(200) });
        }
    }

    #[test]
    fn windows_wrappers_inside_an_mcp_node() {
        let s = snap(
            Platform::Windows,
            vec![
                p(10, 1, 1, &[r"C:\Users\u\.local\bin\claude.exe"]),
                p(
                    20,
                    10,
                    2,
                    &["cmd.exe", "/c", "npx", "-y", "@playwright/mcp"],
                ),
                p(21, 20, 3, &[r"C:\Windows\System32\conhost.exe", "0x4"]),
                p(22, 20, 3, &[r"C:\nodejs\node.exe", r"C:\x\cli.js"]),
                p(23, 22, 4, &["cmd.exe", "/d", "/s", "/c", "playwright-mcp"]),
                p(24, 23, 5, &[r"C:\nodejs\node.exe", r"C:\x\other.js"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        let part = |pid| match c.role(k(pid)) {
            Role::Mcp { part, name, .. } if name == "playwright" => Some(*part),
            _ => None,
        };
        assert_eq!(
            [20, 21, 22, 23, 24].map(part),
            [
                Some(McpPart::Wrapper),
                Some(McpPart::Wrapper),
                Some(McpPart::Server),
                Some(McpPart::Wrapper),
                Some(McpPart::Helper),
            ]
        );
    }

    #[test]
    fn config_scope_tie_break_prefers_this_project_then_plugin_then_user() {
        let argv = ["npm", "exec", "srv@1"];
        let run = |servers: Vec<McpServerConfig>| {
            let mut v = base();
            v.push(p(280, 200, 60, &argv).env("CLAUDE_PROJECT_DIR", "/Users/u/prj/main"));
            let s = snap(Platform::MacOs, v);
            let c = classify(
                &s,
                &McpConfigs {
                    servers,
                    problems: vec![],
                },
            );
            mcp_name(&c, s.index()[&280].key).map(|(n, pl)| (n.to_string(), pl.map(str::to_string)))
        };
        let user = || server("u", "npx", &["-y", "srv@1"], McpScope::User);
        let plugin = || server("pl", "npx", &["srv@1"], McpScope::Plugin("plug".into()));
        let other = || {
            server(
                "o",
                "npx",
                &["srv@1"],
                McpScope::Project("/Users/u/prj/z".into()),
            )
        };
        let here = || {
            server(
                "h",
                "npx",
                &["srv@1"],
                McpScope::Project("/Users/u/prj/a/".into()),
            )
        };
        let main = || {
            server(
                "m",
                "npx",
                &["srv@1"],
                McpScope::Project("/Users/u/prj/main".into()),
            )
        };
        let named = |n: &str, pl: Option<&str>| Some((n.to_string(), pl.map(str::to_string)));

        // The session's own project (its cwd, trailing separator ignored).
        assert_eq!(
            run(vec![user(), plugin(), other(), here()]),
            named("h", None)
        );
        // ... or the project dir Claude Code passed to the server.
        assert_eq!(run(vec![other(), user(), main()]), named("m", None));
        assert_eq!(
            run(vec![other(), user(), plugin()]),
            named("pl", Some("plug"))
        );
        assert_eq!(run(vec![other(), user()]), named("u", None));
        // A project that is not this session's still beats the fallback name.
        assert_eq!(run(vec![other()]), named("o", None));
        // Equally specific: the first one wins.
        let user2 = server("u2", "npx", &["srv@1"], McpScope::User);
        assert_eq!(run(vec![user2, user()]), named("u2", None));
        // A Codex config never names a Claude Code session's server.
        let mut codex = user();
        codex.tool = Tool::Codex;
        assert_eq!(run(vec![codex]), named("srv", None));
    }

    #[test]
    fn npm_codex_body_children_belong_to_the_session() {
        let s = snap(
            Platform::Linux,
            vec![
                p(10, 1, 2, &["bash"]),
                p(11, 10, 3, &["node", "/n/@openai/codex/bin/codex.js"]).cwd("/w"),
                p(12, 11, 4, &["/n/@openai/codex/vendor/codex"]),
                p(13, 12, 5, &["npx", "-y", "@upstash/context7-mcp"]),
                p(14, 12, 5, &["bash", "-c", "make"]),
            ],
        );
        let c = classify(&s, &McpConfigs::default());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.sessions.len(), 1);
        assert_eq!(
            (c.sessions[0].root, c.sessions[0].cwd.as_deref()),
            (k(11), Some("/w"))
        );
        assert_eq!(c.role(k(12)), &Role::OtherChild { session: k(11) });
        assert!(
            matches!(c.role(k(13)), Role::Mcp { session, name, .. } if *session == k(11) && name == "context7")
        );
        assert_eq!(c.role(k(14)), &Role::OtherChild { session: k(11) });
        assert_eq!(c.category_of(k(13)), Some(Category::Codex));
    }

    #[test]
    fn nested_sessions_link_to_the_nearest_and_count_in_the_outermost() {
        let mut v = base();
        v.push(p(300, 230, 40, &["codex", "exec", "hi"]).env("CLAUDE_CODE_SESSION_ID", "S1"));
        v.push(p(301, 300, 41, &["bash", "-c", "x"]).env("CLAUDE_CODE_SESSION_ID", "S1"));
        v.push(p(302, 301, 42, &["claude", "-p", "y"]).env("CLAUDE_CODE_SESSION_ID", "S1"));
        v.push(
            p(303, 302, 43, &["npx", "-y", "@playwright/mcp"]).env("CLAUDE_CODE_SESSION_ID", "S2"),
        );
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        let codex = c.session(k(300)).unwrap();
        let inner = c.session(k(302)).unwrap();
        assert_eq!((codex.outer, inner.outer), (Some(k(200)), Some(k(300))));
        assert_eq!(c.role(k(301)), &Role::OtherChild { session: k(300) });
        assert!(matches!(c.role(k(303)), Role::Mcp { session, .. } if *session == k(302)));
        // Claude Code session ids: each session's own members only. A Codex
        // session has none; its members only inherited the outer one.
        assert_eq!(
            c.session(k(200)).unwrap().claude_session_id.as_deref(),
            Some("S1")
        );
        assert_eq!(codex.claude_session_id, None);
        assert_eq!(inner.claude_session_id.as_deref(), Some("S2"));
        for pid in [300, 301, 302, 303] {
            assert_eq!(c.category_of(k(pid)), Some(Category::ClaudeCode));
        }
        assert_eq!(c.sessions.len(), 3);
    }

    #[test]
    fn session_id_comes_from_the_first_member_in_pre_order() {
        let mut v = base();
        v[4] = v[4].clone().env("CLAUDE_CODE_SESSION_ID", "OLD");
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert_eq!(c.sessions[0].claude_session_id.as_deref(), Some("OLD"));
        // The root's own (inherited) value is not the session's.
        let mut v = base();
        v[2] = v[2].clone().env("CLAUDE_CODE_SESSION_ID", "PARENT");
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        assert_eq!(c.sessions[0].claude_session_id.as_deref(), Some("S1"));
    }

    #[test]
    fn tool_process_inside_an_mcp_node_is_a_member() {
        let mut cf = cfgs();
        cf.servers.push(server(
            "codex",
            "npx",
            &["-y", "@openai/codex", "mcp-server"],
            McpScope::User,
        ));
        let mut v = base();
        v.push(p(
            320,
            200,
            50,
            &["npm", "exec", "@openai/codex", "mcp-server"],
        ));
        v.push(p(321, 320, 51, &["node", "/n/.bin/codex", "mcp-server"]));
        v.push(p(
            322,
            321,
            52,
            &["/n/@openai/codex/vendor/codex", "mcp-server"],
        ));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cf);
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.sessions.len(), 1);
        for (pid, part) in [
            (320, McpPart::Wrapper),
            (321, McpPart::Server),
            (322, McpPart::Helper),
        ] {
            assert!(
                matches!(c.role(k(pid)), Role::Mcp { name, part: got, .. } if name == "codex" && *got == part)
            );
        }
    }

    #[test]
    fn same_tool_child_without_node_wrapper_parent_is_a_nested_session() {
        let mut v = base();
        v.push(p(330, 200, 50, &["claude", "-p", "sub"]));
        let s = snap(Platform::MacOs, v);
        let c = classify(&s, &cfgs());
        let k = |pid| s.index()[&pid].key;
        assert_eq!(c.role(k(330)), &Role::SessionRoot);
        assert_eq!(c.session(k(330)).unwrap().outer, Some(k(200)));
        assert_eq!(
            c.session(k(330)).unwrap().launcher.as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn hostile_snapshots_do_not_panic() {
        let mut empty_argv = p(5, 4, 5, &["x"]);
        empty_argv.argv.clear();
        empty_argv.exe = None;
        empty_argv.name.clear();
        let cases: Vec<Vec<RawProc>> = vec![
            vec![],
            // A ppid cycle through a session, and a missing parent.
            vec![
                p(1, 2, 1, &["claude"]),
                p(2, 1, 1, &["npx", "x"]),
                p(3, 999, 1, &["codex"]),
            ],
            // The same PID twice (even the same key), a self parent, empty argv.
            vec![
                p(4, 4, 1, &["claude"]),
                p(4, 4, 1, &["claude"]),
                p(4, 0, 2, &["npx", "y"]),
                empty_argv,
                p(6, 4, 9, &[""]),
                p(7, 6, 9, &["", ""]).zombie(),
            ],
        ];
        for procs in cases {
            let s = snap(Platform::Windows, procs);
            let c = classify(&s, &cfgs());
            for proc_ in &s.procs {
                let _ = (c.role(proc_.key), c.category_of(proc_.key));
            }
            let _ = c.tracked();
        }
    }

    #[test]
    fn accessors_on_an_empty_classification() {
        let c = Classification::default();
        let k = ProcKey {
            pid: 1,
            start_time: 1,
        };
        assert_eq!(
            (c.role(k), c.session(k), c.category_of(k)),
            (&Role::Unrelated, None, None)
        );
        assert!(c.tracked().is_empty());
        let mut c = c;
        c.roles.insert(k, Role::OtherNode { group: "g".into() });
        let o = ProcKey { pid: 2, ..k };
        c.roles.insert(
            o,
            Role::Orphan {
                certainty: Certainty::Suspected,
                reason: OrphanReason::PathOnly { tool: Tool::Codex },
            },
        );
        c.roles
            .insert(ProcKey { pid: 3, ..k }, Role::OrphanMember { root: o });
        c.roles.insert(ProcKey { pid: 4, ..k }, Role::Unrelated);
        assert_eq!(c.category_of(k), Some(Category::OtherNode));
        assert_eq!(c.category_of(o), Some(Category::Orphaned));
        assert_eq!(
            c.category_of(ProcKey { pid: 3, ..k }),
            Some(Category::Orphaned)
        );
        assert_eq!(c.tracked().len(), 3);
        // A role that names a session nobody recorded has no category.
        c.roles
            .insert(ProcKey { pid: 5, ..k }, Role::OtherChild { session: o });
        assert_eq!(c.category_of(ProcKey { pid: 5, ..k }), None);
    }

    #[test]
    fn category_order_is_the_screen_order() {
        let mut v = vec![
            Category::OtherNode,
            Category::Codex,
            Category::Orphaned,
            Category::ClaudeCode,
        ];
        v.sort();
        assert_eq!(
            v,
            [
                Category::ClaudeCode,
                Category::Codex,
                Category::Orphaned,
                Category::OtherNode
            ]
        );
    }
}
