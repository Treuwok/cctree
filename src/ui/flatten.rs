//! The tree as rows: what is open, in what order, which rows match the
//! search, and how rows keep their identity from one refresh to the next.
//!
//! # Rules (spec 4.6)
//!
//! - **Open or shut**: a row's children are listed when its id is in
//!   [`UiState::expanded`].
//! - **Order**: only among siblings, by [`UiState::sort`] (memory and CPU
//!   largest first, PID and name smallest first; a row without the value goes
//!   last). The categories keep their order, a `SelfRow` is always first and a
//!   `NestedNote` always last. Ties go by PID, then by id, so a refresh never
//!   shuffles equal rows. The members of an MCP row (and of an instance in
//!   the overview) are not sorted: they stay in the tree's order, the start
//!   order (wrapper, server, helpers).
//! - **Search**: a row matches when its name or PID contains the text, ignoring
//!   case. Only matches and their ancestors are listed, and those ancestors are
//!   open whatever `expanded` says. The name is the row's label text: the name
//!   of a process, an MCP, a Node.js group; a session's working directory and
//!   launcher. An empty search matches everything.
//!   Searching the command line is not done here: it is not in a [`Node`]
//!   (the snapshot has it, masked), so a UI that wants it needs the snapshot
//!   and would filter on its own.
//! - **New**: a process first noticed (see [`note_seen`](super::note_seen))
//!   less than 5 seconds ago is `is_new`. Flattening changes no state.
//! - **Exited**: [`merge_exited`] keeps a process that has just vanished for
//!   one more refresh, flagged `exited`, under the row it was under.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::tree::{self, Label, Node, NodeId};

use super::state::{SortKey, UiState, refresh_path};

/// How long a process stays "new".
const NEW_FOR: Duration = Duration::from_secs(5);

/// One line of the tree table. `node` is the row's node without its children.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: NodeId,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub is_new: bool,
    pub node: Node,
}

struct Cx<'a> {
    st: &'a UiState,
    now: Instant,
    /// The search text, lower case; `None` when there is nothing to search.
    query: Option<String>,
}

/// The rows of `roots`, in the order they are drawn.
pub fn flatten(roots: &[Node], st: &UiState, now: Instant) -> Vec<Row> {
    let query = st
        .search
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let cx = Cx { st, now, query };
    let mut out = Vec::new();
    for n in sorted(roots, st.sort) {
        visit(n, 0, &cx, &mut out);
    }
    out
}

fn visit(n: &Node, depth: usize, cx: &Cx, out: &mut Vec<Row>) {
    let at = out.len();
    out.push(Row {
        id: n.id.clone(),
        depth,
        has_children: !n.children.is_empty(),
        expanded: false,
        is_new: is_new(cx, &n.id),
        node: bare(n),
    });
    // Under a search every row is visited: a match may be anywhere below.
    let open = cx.query.is_some() || cx.st.expanded.contains(&n.id);
    if open {
        for c in ordered(n, cx.st.sort) {
            visit(c, depth + 1, cx, out);
        }
    }
    let listed = out.len() > at + 1;
    match &cx.query {
        None => out[at].expanded = open && listed,
        Some(q) if listed || matches(n, q) => {
            out[at].has_children = listed;
            out[at].expanded = listed;
        }
        Some(_) => out.truncate(at),
    }
}

/// `n` without its children.
fn bare(n: &Node) -> Node {
    Node {
        id: n.id.clone(),
        label: n.label.clone(),
        pid: n.pid,
        pid_dim: n.pid_dim,
        cpu: n.cpu,
        mem: n.mem,
        approx: n.approx,
        count: n.count,
        summed: n.summed,
        exited: n.exited,
        children: Vec::new(),
    }
}

fn is_new(cx: &Cx, id: &NodeId) -> bool {
    let (NodeId::Proc(k) | NodeId::SelfRow(k)) = id else {
        return false;
    };
    let Some(Some(t)) = cx.st.first_seen.get(k) else {
        return false;
    };
    cx.now.saturating_duration_since(*t) < NEW_FOR
}

/// The text a row shows as its name, and a second text it also answers to.
fn texts(l: &Label) -> [Option<&str>; 2] {
    match l {
        Label::Session { cwd, launcher, .. } => [cwd.as_deref(), launcher.as_deref()],
        Label::Mcp { name, .. }
        | Label::Process { name, .. }
        | Label::OtherGroup(name)
        | Label::OverviewMcp { name, .. } => [Some(name), None],
        Label::OverviewInstance { cwd } => [cwd.as_deref(), None],
        Label::Category { .. }
        | Label::McpGroup { .. }
        | Label::OtherChildren
        | Label::Detached
        | Label::SelfRow
        | Label::NestedNote { .. } => [None, None],
    }
}

/// Whether `n` matches `query` (lower case) by name or PID.
fn matches(n: &Node, query: &str) -> bool {
    n.pid.is_some_and(|p| p.to_string().contains(query))
        || texts(&n.label)
            .into_iter()
            .flatten()
            .any(|t| t.to_lowercase().contains(query))
}

/// The children of `n` in the order they are drawn: the members of an MCP
/// keep the tree's order (start order), any other siblings are sorted.
fn ordered(n: &Node, key: SortKey) -> Vec<&Node> {
    if matches!(n.id, NodeId::Mcp(_) | NodeId::OverviewInstance(..)) {
        n.children.iter().collect()
    } else {
        sorted(&n.children, key)
    }
}

/// The siblings in the order they are drawn.
fn sorted(nodes: &[Node], key: SortKey) -> Vec<&Node> {
    let mut v: Vec<&Node> = nodes.iter().collect();
    if !v.iter().any(|n| matches!(n.id, NodeId::Category(_))) {
        v.sort_by(|a, b| compare(a, b, key));
    }
    v
}

fn compare(a: &Node, b: &Node, key: SortKey) -> Ordering {
    let rank = |n: &Node| match n.id {
        NodeId::SelfRow(_) => 0,
        NodeId::NestedNote(_) => 2,
        _ => 1,
    };
    let by_pid = |a: &Node, b: &Node| some_first(a.pid, b.pid, |x, y| x.cmp(&y));
    rank(a)
        .cmp(&rank(b))
        .then_with(|| match key {
            SortKey::Mem => some_first(a.mem, b.mem, |x, y| y.cmp(&x)),
            SortKey::Cpu => some_first(a.cpu, b.cpu, |x, y| y.total_cmp(&x)),
            SortKey::Pid => by_pid(a, b),
            SortKey::Name => some_first(texts(&a.label)[0], texts(&b.label)[0], cmp_ignore_case),
        })
        .then_with(|| by_pid(a, b))
        .then_with(|| a.id.cmp(&b.id))
}

/// `f` on two values; a missing value comes after any value.
fn some_first<T>(a: Option<T>, b: Option<T>, f: impl Fn(T, T) -> Ordering) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => f(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let lower = |s| str::chars(s).flat_map(char::to_lowercase);
    lower(a).cmp(lower(b))
}

/// The index of the row that `rows[i]` is a child of: the nearest row before
/// it that is less deep.
pub(super) fn parent_index(rows: &[Row], i: usize) -> Option<usize> {
    rows[..i].iter().rposition(|r| r.depth < rows[i].depth)
}

/// The ids above `rows[i]`, outermost first.
pub(super) fn ancestors(rows: &[Row], i: usize) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut at = i;
    while let Some(p) = parent_index(rows, at) {
        path.push(rows[p].id.clone());
        at = p;
    }
    path.reverse();
    path
}

/// Keeps the cursor on its row after a refresh, a new sort or a new search.
/// When its row is not listed any more it goes to the deepest row of
/// [`UiState::selected_path`] that is, or the first row. It returns `true`
/// only when the row is not in `roots` either, that is when the process is
/// gone; a row that is just hidden (an ancestor was folded, a search does not
/// match it) gives `false`. With no cursor yet it goes to the first row
/// (nothing lost, `false`).
pub fn reselect(st: &mut UiState, rows: &[Row], roots: &[Node]) -> bool {
    let at = |id: &NodeId| rows.iter().position(|r| r.id == *id);
    let (to, lost) = match st.selected.as_ref() {
        None => (0, false),
        Some(sel) => match at(sel) {
            Some(i) => (i, false),
            None => (
                st.selected_path.iter().rev().find_map(at).unwrap_or(0),
                tree::find(roots, sel).is_none(),
            ),
        },
    };
    st.selected = rows.get(to).map(|r| r.id.clone());
    if st.selected.is_none() {
        st.selected_path.clear();
    }
    refresh_path(st, rows);
    lost
}

/// `next`, plus the processes that were in `prev` and are not any more,
/// flagged `exited`, each back under the row it was under. Call it with the
/// previous result: a process that was already `exited` is dropped, so each
/// one is shown once.
///
/// - The ghost keeps its last numbers but is `summed = false`: its parent's
///   totals, built without it, do not include it.
/// - A process that vanished with its parent (a session that ended) comes
///   back with what was below it, all `exited`. Whatever of that is in `next`
///   under another row (an MCP server that outlived its session, now an
///   orphan) stays there and is not copied.
/// - A process whose parent row is not in `next` either is not shown.
pub fn merge_exited(prev: &[Node], mut next: Vec<Node>) -> Vec<Node> {
    let mut ghosts: HashMap<NodeId, Vec<Node>> = HashMap::new();
    {
        let mut present: HashSet<&NodeId> = HashSet::new();
        let mut stack: Vec<&Node> = next.iter().collect();
        while let Some(n) = stack.pop() {
            present.insert(&n.id);
            stack.extend(&n.children);
        }
        let mut stack: Vec<(Option<&NodeId>, &Node)> = prev.iter().map(|n| (None, n)).collect();
        while let Some((parent, n)) = stack.pop() {
            if n.exited {
                continue;
            }
            if matches!(n.id, NodeId::Proc(_))
                && !present.contains(&n.id)
                && let Some(p) = parent.filter(|p| present.contains(p))
            {
                let mut g = ghost(n, &present);
                g.summed = false;
                ghosts.entry(p.clone()).or_default().push(g);
            }
            stack.extend(n.children.iter().map(|c| (Some(&n.id), c)));
        }
    }
    let mut stack: Vec<&mut Node> = next.iter_mut().collect();
    while !ghosts.is_empty()
        && let Some(n) = stack.pop()
    {
        if let Some(g) = ghosts.remove(&n.id) {
            n.children.extend(g);
        }
        stack.extend(n.children.iter_mut());
    }
    next
}

/// A copy of `n` and what is below it, all `exited`, without the nodes that
/// are `present` in the new tree.
fn ghost(n: &Node, present: &HashSet<&NodeId>) -> Node {
    Node {
        exited: true,
        children: n
            .children
            .iter()
            .filter(|c| !present.contains(&c.id))
            .map(|c| ghost(c, present))
            .collect(),
        ..bare(n)
    }
}
