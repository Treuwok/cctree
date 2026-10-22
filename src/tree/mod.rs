//! The display tree, built from a [`Snapshot`] and its [`Classification`].
//!
//! [`build_tree`] gives the rows of one of two views as a forest of [`Node`]s
//! with their totals already worked out:
//!
//! - [`ViewMode::BySession`]: the four categories, always present and in the
//!   fixed [`Category`] order. Under Claude Code and Codex come the sessions
//!   (a session nested in another one sits under its outer session). A
//!   session's children are, in this order and each only when it has members:
//!   its [`NodeId::SelfRow`], the MCP group, the other children, the detached
//!   processes, then the nested sessions.
//! - [`ViewMode::McpOverview`]: one node per MCP name over all sessions; below
//!   it one node per instance (the MCP of one session), below that the
//!   member processes.
//!
//! # Rules (spec 4.6)
//!
//! - **Totals**: every node with children is the sum of the children that
//!   count (`summed`), so the numbers on screen always add up. A process with
//!   descendants gets a `SelfRow` first, carrying its own numbers, which is
//!   how its own share is part of that sum. `mem` is the sum of the main
//!   memory figure (`mem.private`); `approx` is set as soon as a member is
//!   tracked and its figure is missing; `cpu` sums the values that exist;
//!   `count` is the number of distinct processes.
//! - **MCP members are flat**: an MCP node lists its processes one level
//!   deep, in start order (pre-order of the lineage), with no `SelfRow`. Its
//!   `pid` is the server's (shown dim), or the head's when there is no server.
//!   An MCP is identified by its head process, not by its name: two launches of
//!   the same server in one session are two nodes. Other Node.js groups are
//!   flat too.
//! - **Subtrees**: other children, detached processes and orphans keep their
//!   process tree, cut at the members of another group (an MCP node, a nested
//!   session), which sit where they belong.
//! - **`NestedNote`**: a session nested in a session of the other tool counts
//!   in the outer session's category, so its own category gets a note
//!   (`summed = false`, no numbers) instead of the totals.
//! - Unrelated processes never appear. `exited` is for the app layer to set
//!   when it keeps a vanished process for one more refresh: here it is always
//!   `false`.
//!
//! # Choices the spec leaves open
//!
//! - `Label::Category::n` is the number of rows below the category that count:
//!   top-level sessions, orphan roots, Node.js groups. A nested session is
//!   counted below its outer session, not here.
//! - Orphan roots carry their [`Certainty`] in `Label::Process::orphan`; the
//!   reason is left in the [`Classification`] (look the root's key up there).
//! - `cwd` is the session's working directory as recorded, without the `~`
//!   abbreviation. An overview instance shows its session's.
//! - Children are in pre-order of the lineage (start order, then snapshot
//!   order); the overview lists names alphabetically. Sorting by the user's
//!   choice is the renderer's job.
//!
//! One pass over the processes plus hash lookups, no recursion: O(n), whatever
//! the depth of the process tree.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::classify::argv::display_name;
use crate::classify::lineage::Lineage;
use crate::classify::{Category, Certainty, Classification, McpPart, Role};
use crate::model::{Pid, ProcKey, ProcStatus, RawProc, Snapshot, Tool};

/// What a row is. Stable across refreshes, so the cursor can follow a row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeId {
    Category(Category),
    /// A process row; a session is one too, keyed by its root.
    Proc(ProcKey),
    /// The "(self)" row of a process that has children.
    SelfRow(ProcKey),
    /// The MCP group of the session rooted at the key.
    McpGroup(ProcKey),
    /// One MCP node, keyed by its head process.
    Mcp(ProcKey),
    /// The other children of the session rooted at the key.
    OtherChildren(ProcKey),
    /// The detached processes of the session rooted at the key.
    Detached(ProcKey),
    OtherGroup(String),
    NestedNote(Tool),
    /// An MCP name in the overview.
    OverviewMcp(String),
    /// One MCP of one session in the overview: its name and head process.
    OverviewInstance(String, ProcKey),
}

/// What a row says; the renderer turns it into text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    Category {
        cat: Category,
        n: usize,
    },
    Session {
        tool: Tool,
        cwd: Option<String>,
        launcher: Option<String>,
        app_server: bool,
        /// Start time of the root process, Unix seconds.
        started: u64,
    },
    McpGroup {
        n: usize,
    },
    Mcp {
        name: String,
        plugin: bool,
    },
    OtherChildren,
    Detached,
    SelfRow,
    OtherGroup(String),
    Process {
        name: String,
        /// Set for the members of an MCP node.
        part: Option<McpPart>,
        zombie: bool,
        /// Set on the root of an orphan group.
        orphan: Option<Certainty>,
    },
    NestedNote {
        /// The tool of the nested sessions.
        tool: Tool,
        n: usize,
        /// The tool of the sessions they run in (and are counted with).
        inside: Tool,
    },
    OverviewMcp {
        name: String,
        instances: usize,
    },
    OverviewInstance {
        cwd: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub label: Label,
    /// The process behind the row; for an MCP node, its server's.
    pub pid: Option<Pid>,
    /// `pid` is not the row's own (shown dim).
    pub pid_dim: bool,
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
    /// At least one member is tracked but its memory could not be read.
    pub approx: bool,
    /// Number of processes.
    pub count: usize,
    /// Whether a parent adds this row's numbers to its own.
    pub summed: bool,
    pub exited: bool,
    pub children: Vec<Node>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    BySession,
    McpOverview,
}

/// Builds the rows of `view`. See the module documentation. Total: any
/// snapshot gives a result, with no panic.
pub fn build_tree(snap: &Snapshot, cls: &Classification, view: ViewMode) -> Vec<Node> {
    let b = Builder::new(snap, cls);
    match view {
        ViewMode::BySession => b.by_session(),
        ViewMode::McpOverview => b.overview(),
    }
}

/// The first node with `id`, searching `roots` in pre-order.
pub fn find<'a>(roots: &'a [Node], id: &NodeId) -> Option<&'a Node> {
    let mut stack: Vec<&Node> = roots.iter().rev().collect();
    while let Some(n) = stack.pop() {
        if n.id == *id {
            return Some(n);
        }
        stack.extend(n.children.iter().rev());
    }
    None
}

/// The key of every process row in the subtree of `n` (`n` included), in
/// pre-order, each once. A `SelfRow` is not a process row of its own.
pub fn member_keys(n: &Node) -> Vec<ProcKey> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if let NodeId::Proc(k) = n.id
            && seen.insert(k)
        {
            out.push(k);
        }
        stack.extend(n.children.iter().rev());
    }
    out
}

/// What a node adds up to.
#[derive(Clone, Copy, Default)]
struct Totals {
    cpu: Option<f32>,
    mem: Option<u64>,
    approx: bool,
    count: usize,
}

impl Totals {
    fn of_proc(p: &RawProc) -> Totals {
        let mem = p.mem.and_then(|m| m.private);
        Totals {
            cpu: p.cpu,
            mem,
            approx: mem.is_none(),
            count: 1,
        }
    }

    /// The sum of the children that count.
    fn of_children(children: &[Node]) -> Totals {
        let mut t = Totals::default();
        for c in children.iter().filter(|c| c.summed) {
            t.cpu = add(t.cpu, c.cpu, |a, b| a + b);
            t.mem = add(t.mem, c.mem, u64::saturating_add);
            t.approx |= c.approx;
            t.count += c.count;
        }
        t
    }
}

/// The sum of the values that exist; `None` when none does.
fn add<T: Copy>(a: Option<T>, b: Option<T>, f: impl Fn(T, T) -> T) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(f(a, b)),
        (a, None) => a,
        (None, b) => b,
    }
}

impl Node {
    fn new(id: NodeId, label: Label, pid: Option<Pid>, t: Totals, children: Vec<Node>) -> Node {
        Node {
            id,
            label,
            pid,
            pid_dim: false,
            cpu: t.cpu,
            mem: t.mem,
            approx: t.approx,
            count: t.count,
            summed: true,
            exited: false,
            children,
        }
    }

    /// A row with no process of its own: the sum of its children.
    fn group(id: NodeId, label: Label, children: Vec<Node>) -> Node {
        let t = Totals::of_children(&children);
        Node::new(id, label, None, t, children)
    }

    /// A process row with no children.
    fn leaf(label: Label, p: &RawProc) -> Node {
        Node::new(
            NodeId::Proc(p.key),
            label,
            Some(p.key.pid),
            Totals::of_proc(p),
            Vec::new(),
        )
    }
}

/// The row of process `p` under `label`, with `kids` below it. With kids, the
/// first child is the `SelfRow`, so the row is the sum of its children.
fn proc_node(p: &RawProc, label: Label, kids: Vec<Node>) -> Node {
    if kids.is_empty() {
        return Node::leaf(label, p);
    }
    let own = Totals::of_proc(p);
    let mut children = Vec::with_capacity(kids.len() + 1);
    children.push(Node::new(
        NodeId::SelfRow(p.key),
        Label::SelfRow,
        Some(p.key.pid),
        own,
        Vec::new(),
    ));
    children.extend(kids);
    let t = Totals::of_children(&children);
    Node::new(NodeId::Proc(p.key), label, Some(p.key.pid), t, children)
}

fn process_label(p: &RawProc, part: Option<McpPart>, orphan: Option<Certainty>) -> Label {
    Label::Process {
        name: display_name(p),
        part,
        zombie: p.status == ProcStatus::Zombie,
        orphan,
    }
}

/// A category row; `n` counts the rows below it that count.
fn category(cat: Category, children: Vec<Node>) -> Node {
    let n = children.iter().filter(|c| c.summed).count();
    Node::group(NodeId::Category(cat), Label::Category { cat, n }, children)
}

fn other_tool(t: Tool) -> Tool {
    match t {
        Tool::ClaudeCode => Tool::Codex,
        Tool::Codex => Tool::ClaudeCode,
    }
}

fn tool_index(t: Tool) -> usize {
    match t {
        Tool::ClaudeCode => 0,
        Tool::Codex => 1,
    }
}

/// A set of processes shown as process subtrees under one parent row.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Grp {
    /// The other children of the session rooted here.
    Other(ProcKey),
    /// The processes detached from the session rooted here.
    Detached(ProcKey),
    /// The orphan group rooted here.
    Orphan(ProcKey),
}

impl Grp {
    fn of(role: &Role, k: ProcKey) -> Option<Grp> {
        match role {
            Role::OtherChild { session } => Some(Grp::Other(*session)),
            Role::Detached { session } => Some(Grp::Detached(*session)),
            Role::Orphan { .. } => Some(Grp::Orphan(k)),
            Role::OrphanMember { root } => Some(Grp::Orphan(*root)),
            Role::SessionRoot | Role::Mcp { .. } | Role::OtherNode { .. } | Role::Unrelated => None,
        }
    }
}

/// One MCP node found in the classification.
struct McpData {
    name: String,
    plugin: bool,
    /// The root of the session it belongs to.
    session: ProcKey,
    head: ProcKey,
    /// Its processes in pre-order, the head first.
    members: Vec<(ProcKey, McpPart)>,
}

/// Everything the two views are built from.
struct Builder<'a> {
    cls: &'a Classification,
    l: Lineage<'a>,
    by_key: HashMap<ProcKey, &'a RawProc>,
    /// Pre-order over the whole forest: tops and children in snapshot order.
    order: Vec<ProcKey>,
    /// The sessions of the classification that are in the snapshot, by root:
    /// the index into `cls.sessions`.
    sessions: HashMap<ProcKey, usize>,
    /// In pre-order of their heads.
    mcps: Vec<McpData>,
}

impl<'a> Builder<'a> {
    fn new(snap: &'a Snapshot, cls: &'a Classification) -> Builder<'a> {
        let l = Lineage::build(snap);
        let by_key: HashMap<ProcKey, &RawProc> = snap.procs.iter().map(|p| (p.key, p)).collect();
        let order = preorder(snap, &l);
        let sessions: HashMap<ProcKey, usize> = cls
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| by_key.contains_key(&s.root))
            .map(|(i, s)| (s.root, i))
            .collect();

        let mut mcps: Vec<McpData> = Vec::new();
        let mut by_head: HashMap<ProcKey, usize> = HashMap::new();
        for &k in &order {
            let Role::Mcp {
                session,
                name,
                head,
                part,
                plugin,
            } = cls.role(k)
            else {
                continue;
            };
            if !sessions.contains_key(session) {
                continue;
            }
            let i = *by_head.entry(*head).or_insert_with(|| {
                mcps.push(McpData {
                    name: name.clone(),
                    plugin: plugin.is_some(),
                    session: *session,
                    head: *head,
                    members: Vec::new(),
                });
                mcps.len() - 1
            });
            mcps[i].members.push((k, *part));
        }
        Builder {
            cls,
            l,
            by_key,
            order,
            sessions,
            mcps,
        }
    }

    /// A process of the snapshot. Only called with keys that came from it.
    fn proc_(&self, k: ProcKey) -> &'a RawProc {
        self.by_key[&k]
    }

    fn by_session(&self) -> Vec<Node> {
        let sessions = &self.cls.sessions;
        let (mut groups, orphans) = self.proc_groups();
        let mut mcps_of: HashMap<ProcKey, Vec<&McpData>> = HashMap::new();
        for m in &self.mcps {
            mcps_of.entry(m.session).or_default().push(m);
        }

        // The tool of the outermost session each session runs in: the
        // category its numbers are counted in.
        let mut outermost: Vec<Tool> = Vec::with_capacity(sessions.len());
        for (i, s) in sessions.iter().enumerate() {
            outermost.push(self.outer_of(i).map_or(s.tool, |j| outermost[j]));
        }

        // Sessions come after the sessions that enclose them, so going back
        // to front finishes a nested session before the one it is in.
        let mut nested: HashMap<ProcKey, Vec<Node>> = HashMap::new();
        let mut tops: [Vec<Node>; 2] = [Vec::new(), Vec::new()];
        let mut in_other_tool = [0usize; 2];
        for (i, s) in sessions.iter().enumerate().rev() {
            if self.sessions.get(&s.root) != Some(&i) {
                continue;
            }
            if s.tool != outermost[i] {
                in_other_tool[tool_index(s.tool)] += 1;
            }
            let root = s.root;
            let mut kids = Vec::new();
            if let Some(ms) = mcps_of.get(&root) {
                let nodes: Vec<Node> = ms.iter().map(|m| self.mcp_node(m)).collect();
                let n = nodes.len();
                kids.push(Node::group(
                    NodeId::McpGroup(root),
                    Label::McpGroup { n },
                    nodes,
                ));
            }
            if let Some(v) = groups.remove(&Grp::Other(root)) {
                kids.push(Node::group(
                    NodeId::OtherChildren(root),
                    Label::OtherChildren,
                    v,
                ));
            }
            if let Some(v) = groups.remove(&Grp::Detached(root)) {
                kids.push(Node::group(NodeId::Detached(root), Label::Detached, v));
            }
            if let Some(mut v) = nested.remove(&root) {
                v.reverse();
                kids.extend(v);
            }
            let label = Label::Session {
                tool: s.tool,
                cwd: s.cwd.clone(),
                launcher: s.launcher.clone(),
                app_server: s.app_server,
                started: root.start_time,
            };
            let node = proc_node(self.proc_(root), label, kids);
            match self.outer_of(i) {
                Some(o) => nested.entry(sessions[o].root).or_default().push(node),
                None => tops[tool_index(s.tool)].push(node),
            }
        }

        let mut out = Vec::with_capacity(4);
        for (cat, tool) in [
            (Category::ClaudeCode, Tool::ClaudeCode),
            (Category::Codex, Tool::Codex),
        ] {
            let mut kids = std::mem::take(&mut tops[tool_index(tool)]);
            kids.reverse();
            let n = in_other_tool[tool_index(tool)];
            if n > 0 {
                let label = Label::NestedNote {
                    tool,
                    n,
                    inside: other_tool(tool),
                };
                let mut note = Node::new(
                    NodeId::NestedNote(tool),
                    label,
                    None,
                    Totals::default(),
                    Vec::new(),
                );
                note.summed = false;
                kids.push(note);
            }
            out.push(category(cat, kids));
        }
        out.push(category(Category::Orphaned, orphans));
        out.push(category(Category::OtherNode, self.other_groups()));
        out
    }

    /// The index of the session `sessions[i]` is nested in, if that session is
    /// known and comes before it (as it does in any classification).
    fn outer_of(&self, i: usize) -> Option<usize> {
        let outer = self.cls.sessions[i].outer?;
        self.sessions.get(&outer).copied().filter(|&j| j < i)
    }

    /// The process subtrees of the groups that have a parent row, as the roots
    /// of each group in pre-order, and the orphan roots in pre-order.
    fn proc_groups(&self) -> (HashMap<Grp, Vec<Node>>, Vec<Node>) {
        let mut roots: HashMap<Grp, Vec<Node>> = HashMap::new();
        let mut orphans: Vec<Node> = Vec::new();
        // The finished children of a process whose parent is in its group.
        // Going back to front finishes a child before its parent; both lists
        // are therefore in reverse.
        let mut pending: HashMap<ProcKey, Vec<Node>> = HashMap::new();
        for &k in self.order.iter().rev() {
            let role = self.cls.role(k);
            let Some(g) = Grp::of(role, k) else {
                continue;
            };
            let p = self.proc_(k);
            let mut kids = pending.remove(&k).unwrap_or_default();
            kids.reverse();
            let orphan = match role {
                Role::Orphan { certainty, .. } => Some(*certainty),
                _ => None,
            };
            let node = proc_node(p, process_label(p, None, orphan), kids);
            let parent = self
                .l
                .parent(k)
                .filter(|&par| Grp::of(self.cls.role(par), par) == Some(g));
            match (parent, g) {
                (Some(par), _) => pending.entry(par).or_default().push(node),
                (None, Grp::Orphan(_)) => orphans.push(node),
                (None, _) => roots.entry(g).or_default().push(node),
            }
        }
        orphans.reverse();
        roots.values_mut().for_each(|v| v.reverse());
        (roots, orphans)
    }

    /// The Node.js groups, each with its processes flat, in the order their
    /// first process comes.
    fn other_groups(&self) -> Vec<Node> {
        let mut at: HashMap<&str, usize> = HashMap::new();
        let mut groups: Vec<(&str, Vec<Node>)> = Vec::new();
        for &k in &self.order {
            let Role::OtherNode { group } = self.cls.role(k) else {
                continue;
            };
            let i = *at.entry(group.as_str()).or_insert_with(|| {
                groups.push((group.as_str(), Vec::new()));
                groups.len() - 1
            });
            let p = self.proc_(k);
            groups[i]
                .1
                .push(Node::leaf(process_label(p, None, None), p));
        }
        groups
            .into_iter()
            .map(|(name, kids)| {
                let id = NodeId::OtherGroup(name.to_string());
                Node::group(id, Label::OtherGroup(name.to_string()), kids)
            })
            .collect()
    }

    fn overview(&self) -> Vec<Node> {
        let mut by_name: BTreeMap<&str, Vec<&McpData>> = BTreeMap::new();
        for m in &self.mcps {
            by_name.entry(m.name.as_str()).or_default().push(m);
        }
        by_name
            .into_iter()
            .map(|(name, ms)| {
                let instances: Vec<Node> = ms
                    .iter()
                    .map(|m| {
                        let cwd = self.cls.sessions[self.sessions[&m.session]].cwd.clone();
                        let id = NodeId::OverviewInstance(name.to_string(), m.head);
                        self.mcp_row(m, id, Label::OverviewInstance { cwd })
                    })
                    .collect();
                let label = Label::OverviewMcp {
                    name: name.to_string(),
                    instances: instances.len(),
                };
                Node::group(NodeId::OverviewMcp(name.to_string()), label, instances)
            })
            .collect()
    }

    fn mcp_node(&self, m: &McpData) -> Node {
        let label = Label::Mcp {
            name: m.name.clone(),
            plugin: m.plugin,
        };
        self.mcp_row(m, NodeId::Mcp(m.head), label)
    }

    /// An MCP row: its processes flat below it, its server's PID dim.
    fn mcp_row(&self, m: &McpData, id: NodeId, label: Label) -> Node {
        let kids: Vec<Node> = m
            .members
            .iter()
            .map(|&(k, part)| {
                let p = self.proc_(k);
                Node::leaf(process_label(p, Some(part), None), p)
            })
            .collect();
        let pid = m
            .members
            .iter()
            .find(|(_, part)| *part == McpPart::Server)
            .map_or(m.head.pid, |(k, _)| k.pid);
        let t = Totals::of_children(&kids);
        let mut node = Node::new(id, label, Some(pid), t, kids);
        node.pid_dim = true;
        node
    }
}

/// Every process in pre-order, tops and children in snapshot order.
fn preorder(snap: &Snapshot, l: &Lineage) -> Vec<ProcKey> {
    let mut out = Vec::with_capacity(snap.procs.len());
    let mut visited: HashSet<ProcKey> = HashSet::with_capacity(snap.procs.len());
    let mut stack: Vec<ProcKey> = snap
        .procs
        .iter()
        .rev()
        .filter(|p| l.parent(p.key).is_none())
        .map(|p| p.key)
        .collect();
    while let Some(k) = stack.pop() {
        // A PID listed twice under the same key would be reached twice.
        if visited.insert(k) {
            out.push(k);
            stack.extend(l.children(k).iter().rev());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Category, Certainty, McpPart, classify};
    use crate::model::{Platform, ProcStatus, RawProc, Tool};
    use crate::testkit::{base, cfgs, p, snap, tree};
    use std::collections::HashSet;

    const SID: &str = "CLAUDE_CODE_SESSION_ID";
    const PROJ: &str = "CLAUDE_PROJECT_DIR";

    // ---- The brief's tests ----

    #[test]
    fn categories_fixed_order_always_present() {
        let t = tree(base(), ViewMode::BySession);
        let ids: Vec<_> = t.iter().map(|n| n.id.clone()).collect();
        assert_eq!(
            ids,
            [
                Category::ClaudeCode,
                Category::Codex,
                Category::Orphaned,
                Category::OtherNode
            ]
            .map(NodeId::Category)
        );
        assert!(matches!(t[1].label, Label::Category { n: 0, .. }) && t[1].children.is_empty());
    }

    #[test]
    fn session_layout_and_totals() {
        let t = tree(base(), ViewMode::BySession);
        let ses = &t[0].children[0];
        assert!(matches!(
            ses.label,
            Label::Session {
                tool: Tool::ClaudeCode,
                ..
            }
        ));
        assert!(matches!(ses.children[0].id, NodeId::SelfRow(_)));
        assert!(matches!(ses.children[1].label, Label::McpGroup { n: 2 }));
        assert!(matches!(ses.children[2].label, Label::OtherChildren));
        assert_eq!(ses.count, 7);
        assert_eq!(ses.mem, Some(7 * 1024 * 1024));
        assert_eq!(ses.children[0].mem, Some(1024 * 1024));
        let cdp = &ses.children[1]
            .children
            .iter()
            .find(|n| matches!(&n.label, Label::Mcp { name, .. } if name == "chrome-cdp"))
            .unwrap();
        assert_eq!((cdp.pid, cdp.pid_dim, cdp.count), (Some(211), true, 3));
        // MCP members are flat (spec 4.6 exception): one level, in start order,
        // and no "self" row.
        assert_eq!(
            cdp.children.iter().map(|n| n.pid).collect::<Vec<_>>(),
            [Some(210), Some(211), Some(212)]
        );
        assert!(cdp.children.iter().all(|n| n.children.is_empty()));
        assert!(matches!(
            cdp.children[2].label,
            Label::Process {
                part: Some(McpPart::Helper),
                ..
            }
        ));
    }

    #[test]
    fn approx_when_a_member_is_unreadable() {
        let mut v = base();
        v[8] = v[8].clone().mem_private(None);
        let t = tree(v, ViewMode::BySession);
        assert!(t[0].children[0].approx && t[0].approx);
    }

    #[test]
    fn nested_codex_note_is_not_summed() {
        let mut v = base();
        v.push(p(300, 230, 40, &["codex", "exec", "hi"]));
        let t = tree(v, ViewMode::BySession);
        let note = t[1]
            .children
            .iter()
            .find(|n| matches!(n.id, NodeId::NestedNote(Tool::Codex)))
            .unwrap();
        assert!(
            !note.summed
                && matches!(
                    note.label,
                    Label::NestedNote {
                        n: 1,
                        inside: Tool::ClaudeCode,
                        ..
                    }
                )
        );
        assert_eq!(t[1].count, 0);
    }

    #[test]
    fn mcp_overview_groups_across_sessions() {
        let mut v = base();
        v.extend([
            p(700, 100, 80, &["claude"]),
            p(710, 700, 81, &["npm", "exec", "chrome-devtools-mcp@latest"]),
            p(711, 710, 82, &["chrome-devtools-mcp"]),
        ]);
        let t = tree(v, ViewMode::McpOverview);
        let cdp = t
            .iter()
            .find(|n| matches!(&n.label, Label::OverviewMcp { name, .. } if name == "chrome-cdp"))
            .unwrap();
        assert!(matches!(cdp.label, Label::OverviewMcp { instances: 2, .. }));
        assert_eq!(cdp.count, 5);
    }

    // ---- Beyond the brief ----

    /// Checks `count`, `mem`, `cpu` and `approx` of every node with children
    /// against the sum of the children that count (`summed`).
    fn assert_adds_up(n: &Node) {
        if !n.children.is_empty() {
            let kids = || n.children.iter().filter(|c| c.summed);
            assert_eq!(
                n.count,
                kids().map(|c| c.count).sum::<usize>(),
                "{:?}",
                n.id
            );
            let mem: Vec<u64> = kids().filter_map(|c| c.mem).collect();
            assert_eq!(
                n.mem,
                (!mem.is_empty()).then(|| mem.iter().sum::<u64>()),
                "{:?}",
                n.id
            );
            assert_eq!(n.approx, kids().any(|c| c.approx), "{:?}", n.id);
            let cpu: Vec<f32> = kids().filter_map(|c| c.cpu).collect();
            assert_eq!(n.cpu.is_some(), !cpu.is_empty(), "{:?}", n.id);
        }
        n.children.iter().for_each(assert_adds_up);
    }

    /// A fixture with every kind of node: a Claude Code session with MCPs, an
    /// other child with a child of its own, a nested Codex session with an MCP
    /// of its own, a detached dev server, a confirmed orphan with a child, and
    /// an unrelated Node.js process.
    fn busy() -> Vec<RawProc> {
        let mut v = base();
        v.extend([
            p(231, 230, 31, &["node", "x.js"]),
            p(300, 230, 40, &["codex", "exec", "hi"]).cwd("/Users/u/prj/c"),
            p(310, 300, 41, &["npx", "-y", "time-mcp"]),
            p(311, 310, 42, &["node", "/x/.bin/time-mcp"]),
            p(400, 1, 70, &["node", "dev-server.js"]).env(SID, "S1"),
            p(401, 400, 71, &["/opt/esbuild", "--service"]),
            p(430, 1, 80, &["node", "old.js"])
                .env(SID, "GONE")
                .env(PROJ, "/Users/u/prj/old"),
            p(431, 430, 81, &["node", "a.js"]),
            p(500, 1, 90, &["node", "tool.js"]),
        ]);
        v
    }

    #[test]
    fn every_tracked_process_appears_once_and_sums_add_up() {
        let s = snap(Platform::MacOs, busy());
        let c = classify(&s, &cfgs());
        let t = build_tree(&s, &c, ViewMode::BySession);
        let all: Vec<_> = t.iter().flat_map(member_keys).collect();
        assert_eq!(all.len(), c.tracked().len());
        assert_eq!(all.iter().copied().collect::<HashSet<_>>(), c.tracked());
        t.iter().for_each(assert_adds_up);
        // Claude Code: 7 of the base + 231 + 3 nested Codex + 2 detached.
        assert_eq!(t[0].count, 13);
        assert_eq!((t[1].count, t[2].count, t[3].count), (0, 2, 1));
    }

    #[test]
    fn session_children_in_order_with_nested_session_last() {
        let t = tree(busy(), ViewMode::BySession);
        let ses = &t[0].children[0];
        let kinds: Vec<_> = ses
            .children
            .iter()
            .map(|n| match &n.id {
                NodeId::SelfRow(_) => "self",
                NodeId::McpGroup(_) => "mcp",
                NodeId::OtherChildren(_) => "other",
                NodeId::Detached(_) => "detached",
                NodeId::Proc(_) => "session",
                _ => "?",
            })
            .collect();
        assert_eq!(kinds, ["self", "mcp", "other", "detached", "session"]);
        // The nested session is not a top-level session of its own category.
        assert_eq!(t[0].children.len(), 1);
        assert!(t[1].children.iter().all(|n| !n.summed));
        let nested = ses.children.last().unwrap();
        assert!(matches!(
            nested.label,
            Label::Session {
                tool: Tool::Codex,
                ..
            }
        ));
        assert_eq!(nested.count, 3);
        // Its own MCP is under it, and not under the outer session's MCPs.
        assert!(matches!(nested.children[1].label, Label::McpGroup { n: 1 }));
        assert!(matches!(ses.children[1].label, Label::McpGroup { n: 2 }));
    }

    #[test]
    fn other_children_and_detached_are_proc_subtrees() {
        let t = tree(busy(), ViewMode::BySession);
        let ses = &t[0].children[0];
        let other = &ses.children[2];
        // 230 (a shell) with its child 231; the nested session 300 is not here.
        let shell = &other.children[0];
        assert_eq!(shell.pid, Some(230));
        assert!(matches!(shell.children[0].id, NodeId::SelfRow(k) if k.pid == 230));
        assert_eq!(shell.children[1].pid, Some(231));
        assert_eq!((shell.count, other.count), (2, 2));
        let detached = &ses.children[3];
        assert!(matches!(detached.label, Label::Detached));
        assert_eq!(detached.children[0].pid, Some(400));
        assert_eq!(detached.children[0].children[1].pid, Some(401));
        assert_eq!(detached.count, 2);
    }

    #[test]
    fn orphan_roots_carry_their_certainty() {
        let t = tree(busy(), ViewMode::BySession);
        let orphaned = &t[2];
        assert!(matches!(
            orphaned.label,
            Label::Category {
                cat: Category::Orphaned,
                n: 1
            }
        ));
        let root = &orphaned.children[0];
        assert_eq!(root.pid, Some(430));
        assert!(matches!(
            root.label,
            Label::Process {
                orphan: Some(Certainty::Confirmed),
                ..
            }
        ));
        assert!(matches!(root.children[0].id, NodeId::SelfRow(_)));
        assert!(matches!(
            root.children[1].label,
            Label::Process { orphan: None, .. }
        ));
        assert_eq!(root.children[1].pid, Some(431));
    }

    #[test]
    fn other_node_group_members_are_flat() {
        let t = tree(busy(), ViewMode::BySession);
        let other = &t[3];
        assert!(matches!(
            other.label,
            Label::Category {
                cat: Category::OtherNode,
                n: 1
            }
        ));
        let group = &other.children[0];
        assert!(matches!(group.label, Label::OtherGroup(_)));
        assert_eq!(group.pid, None);
        assert_eq!(
            group.children.iter().map(|n| n.pid).collect::<Vec<_>>(),
            [Some(500)]
        );
        assert!(group.children.iter().all(|n| n.children.is_empty()));
    }

    #[test]
    fn unrelated_processes_never_appear() {
        let t = tree(busy(), ViewMode::BySession);
        let keys: HashSet<_> = t.iter().flat_map(member_keys).map(|k| k.pid).collect();
        for gone in [1, 100] {
            assert!(!keys.contains(&gone), "{gone}");
        }
        assert!(keys.contains(&200));
    }

    #[test]
    fn same_name_mcps_in_one_session_stay_separate() {
        let mut v = base();
        v.extend([
            p(240, 200, 40, &["npm", "exec", "chrome-devtools-mcp@latest"]),
            p(241, 240, 41, &["chrome-devtools-mcp"]),
        ]);
        let t = tree(v.clone(), ViewMode::BySession);
        let group = &t[0].children[0].children[1];
        assert!(matches!(group.label, Label::McpGroup { n: 3 }));
        let cdp: Vec<_> = group
            .children
            .iter()
            .filter(|n| matches!(&n.label, Label::Mcp { name, .. } if name == "chrome-cdp"))
            .collect();
        assert_eq!(cdp.len(), 2);
        assert_ne!(cdp[0].id, cdp[1].id);
        assert_eq!((cdp[0].pid, cdp[1].pid), (Some(211), Some(241)));

        let t = tree(v, ViewMode::McpOverview);
        let cdp = t
            .iter()
            .find(|n| matches!(&n.label, Label::OverviewMcp { name, .. } if name == "chrome-cdp"))
            .unwrap();
        assert!(matches!(cdp.label, Label::OverviewMcp { instances: 2, .. }));
        assert_eq!(cdp.children.len(), 2);
        assert_ne!(cdp.children[0].id, cdp.children[1].id);
        assert!(matches!(
            &cdp.children[0].label,
            Label::OverviewInstance { cwd: Some(c) } if c == "/Users/u/prj/a"
        ));
        assert_eq!(cdp.children[1].pid, Some(241));
        assert!(cdp.children[1].pid_dim);
        assert_eq!(cdp.count, 5);
    }

    #[test]
    fn mcp_overview_has_only_mcp_names_at_the_top() {
        let t = tree(busy(), ViewMode::McpOverview);
        let names: Vec<_> = t
            .iter()
            .map(|n| match &n.label {
                Label::OverviewMcp { name, .. } => name.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        // Sorted by name; the nested Codex session's MCP is listed too.
        assert_eq!(names, ["chrome-cdp", "playwright", "time"]);
        assert!(t.iter().all(|n| n.pid.is_none()));
        t.iter().for_each(assert_adds_up);
        let play = &t[1];
        assert_eq!(play.count, 2);
        // Process leaves under the instance, flat.
        assert_eq!(
            play.children[0]
                .children
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            [Some(220), Some(221)]
        );
    }

    #[test]
    fn mcp_without_a_server_shows_its_heads_pid() {
        let mut v = base();
        v.push(p(250, 200, 50, &["npx", "-y", "lonely-mcp"]));
        let t = tree(v, ViewMode::BySession);
        let lonely = t[0].children[0].children[1]
            .children
            .iter()
            .find(|n| matches!(&n.label, Label::Mcp { name, .. } if name == "lonely"))
            .unwrap();
        assert_eq!(
            (lonely.pid, lonely.pid_dim, lonely.count),
            (Some(250), true, 1)
        );
        assert!(matches!(
            lonely.children[0].label,
            Label::Process {
                part: Some(McpPart::Wrapper),
                ..
            }
        ));
    }

    #[test]
    fn zombie_and_missing_values() {
        let mut v = base();
        v[8].status = ProcStatus::Zombie;
        let mut gone = p(232, 230, 31, &["node", "y.js"]);
        gone.mem = None;
        gone.cpu = None;
        v.push(gone);
        let t = tree(v, ViewMode::BySession);
        let other = &t[0].children[0].children[2];
        let shell = &other.children[0];
        assert!(matches!(shell.label, Label::Process { zombie: true, .. }));
        let leaf = &shell.children[1];
        assert_eq!(
            (leaf.mem, leaf.cpu, leaf.approx, leaf.count),
            (None, None, true, 1)
        );
        // The shell's own memory is known, so its total is a lower bound.
        assert_eq!((shell.mem, shell.approx), (Some(1 << 20), true));
        assert_eq!(shell.cpu, Some(0.0));
    }

    #[test]
    fn find_and_member_keys() {
        let t = tree(busy(), ViewMode::BySession);
        let ses = &t[0].children[0];
        assert_eq!(find(&t, &ses.id).map(|n| &n.id), Some(&ses.id));
        assert_eq!(
            find(&t, &NodeId::Category(Category::Orphaned)).map(|n| n.count),
            Some(2)
        );
        let missing = NodeId::Proc(ProcKey {
            pid: 5,
            start_time: 5,
        });
        assert!(find(&t, &missing).is_none());
        // Pre-order, each process once, the self row not repeated.
        let pids: Vec<_> = member_keys(ses).iter().map(|k| k.pid).collect();
        assert_eq!(
            pids,
            [
                200, 210, 211, 212, 220, 221, 230, 231, 400, 401, 300, 310, 311
            ]
        );
        // A group node holds the keys of everything below it.
        assert_eq!(member_keys(&ses.children[1]).len(), 5);
        // Not a process: no keys.
        assert!(member_keys(&t[1]).is_empty());
    }

    #[test]
    fn a_very_deep_chain_is_built_without_recursion() {
        let depth: u32 = 3_000;
        let mut v = vec![p(200, 1, 2, &["claude"])];
        v.extend((201..200 + depth).map(|pid| p(pid, pid - 1, u64::from(pid), &["sleep"])));
        let t = tree(v, ViewMode::BySession);
        let ses = &t[0].children[0];
        assert_eq!(ses.count, depth as usize);
        assert_eq!(member_keys(ses).len(), depth as usize);
        let deepest = ProcKey {
            pid: 200 + depth - 1,
            start_time: u64::from(200 + depth - 1),
        };
        assert!(find(&t, &NodeId::Proc(deepest)).is_some());
    }
}
