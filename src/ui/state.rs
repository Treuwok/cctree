//! The state the user changes with the keys, and what each key does to it.
//!
//! [`apply`] covers the cursor, folding, sorting, the view, the search, pause
//! and the refresh interval. Ending processes, the dialogs' `Confirm` and
//! `Quit` belong to the app, which needs the snapshot and the worker for them.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::classify::Category;
use crate::i18n::{Lang, Msg};
use crate::kill::KillPlan;
use crate::model::ProcKey;
use crate::tree::{Node, NodeId, ViewMode};

use super::flatten::{Row, ancestors, parent_index};

/// The refresh interval, in seconds, is kept between these two.
const MIN_INTERVAL: u64 = 1;
const MAX_INTERVAL: u64 = 10;

/// What siblings are sorted by. `Mem` and `Cpu` put the largest first, `Pid`
/// and `Name` the smallest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Mem,
    Cpu,
    Pid,
    Name,
}

impl SortKey {
    /// The key `s` switches to: memory, CPU, PID, name, and around.
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Mem => SortKey::Cpu,
            SortKey::Cpu => SortKey::Pid,
            SortKey::Pid => SortKey::Name,
            SortKey::Name => SortKey::Mem,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    ConfirmKill(KillPlan),
    Killing { done: usize, total: usize },
}

#[derive(Debug)]
pub struct UiState {
    pub lang: Lang,
    /// The rows that are open. Under an active search the ancestors of the
    /// matches are open whatever this says.
    pub expanded: HashSet<NodeId>,
    /// The cursor, by identity, so it stays on its row when rows move.
    pub selected: Option<NodeId>,
    /// The rows above `selected`, outermost first, as of the last [`apply`] or
    /// [`reselect`](super::reselect): where the cursor falls back to when its
    /// row is gone.
    pub selected_path: Vec<NodeId>,
    pub sort: SortKey,
    pub view: ViewMode,
    pub paused: bool,
    pub interval_secs: u64,
    pub search: Option<String>,
    pub search_editing: bool,
    pub dialog: Option<Dialog>,
    pub message: Option<Msg>,
    pub help_open: bool,
    /// How many lines the help page is scrolled. It may run past the end:
    /// the renderer clamps it, and hands the clamped value back in
    /// [`Layout::help_scroll`](super::Layout::help_scroll) to be stored here.
    pub help_scroll: u16,
    /// For the renderer to keep the cursor in view; nothing here moves it.
    pub scroll: usize,
    /// When each process was first noticed; `None` is "there when cctree
    /// started". See [`note_seen`].
    pub first_seen: HashMap<ProcKey, Option<Instant>>,
    /// [`note_seen`] has run once. A flag of its own, because the first tree
    /// can be empty (no session running yet) and a later process is then new.
    seeded: bool,
}

impl UiState {
    pub fn new(lang: Lang) -> UiState {
        UiState {
            lang,
            expanded: [Category::ClaudeCode, Category::Codex, Category::Orphaned]
                .into_iter()
                .map(NodeId::Category)
                .collect(),
            selected: None,
            selected_path: Vec::new(),
            sort: SortKey::Mem,
            view: ViewMode::BySession,
            paused: false,
            interval_secs: 2,
            search: None,
            search_editing: false,
            dialog: None,
            message: None,
            help_open: false,
            help_scroll: 0,
            scroll: 0,
            first_seen: HashMap::new(),
            seeded: false,
        }
    }
}

/// Records the processes of `roots`: the first call marks all of them `None`
/// (there since cctree started), every later call marks the ones it has not
/// seen yet `Some(now)`. Call it with each refresh's tree. Nothing is ever
/// removed, a process costs a few bytes for as long as cctree runs.
pub fn note_seen(st: &mut UiState, roots: &[Node], now: Instant) {
    let at = if st.seeded { Some(now) } else { None };
    st.seeded = true;
    let mut stack: Vec<&Node> = roots.iter().collect();
    while let Some(n) = stack.pop() {
        if let NodeId::Proc(k) = n.id {
            st.first_seen.entry(k).or_insert(at);
        }
        stack.extend(&n.children);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Collapse,
    Expand,
    Toggle,
    KillOne,
    KillTree,
    ToggleView,
    CycleSort,
    StartSearch,
    SearchChar(char),
    SearchBackspace,
    SearchDone,
    TogglePause,
    Faster,
    Slower,
    Help,
    Quit,
    Confirm,
    Cancel,
    Select(usize),
    ToggleAt(usize),
    None,
}

/// Applies `a` to `st`, given the rows now on screen and the page size.
///
/// - `Collapse` closes the row under the cursor, or moves to its parent when
///   there is nothing to close. Under a search the matches' ancestors stay
///   open, so closing one changes nothing you can see.
/// - `ToggleView` drops the cursor (its row is not in the other view); the
///   next [`reselect`](super::reselect) puts it on the first row.
/// - While the help is open, `Up`, `Down`, `PageUp`, `PageDown`, `Home` and
///   `End` scroll it ([`UiState::help_scroll`]) and the cursor stays; opening
///   or closing it puts it back at the top.
/// - `Cancel` closes the help, or else clears the search. A dialog's `Cancel`
///   is the app's.
/// - `KillOne`, `KillTree`, `Quit`, `Confirm` and `None` do nothing here.
pub fn apply(st: &mut UiState, a: &Action, rows: &[Row], page: usize) {
    let cur = st
        .selected
        .as_ref()
        .and_then(|s| rows.iter().position(|r| r.id == *s));
    let page = page.max(1);
    let go = |st: &mut UiState, i: usize| {
        if let Some(r) = rows.get(i.min(rows.len().saturating_sub(1))) {
            st.selected = Some(r.id.clone());
        }
    };
    let flip = |st: &mut UiState, r: &Row| {
        if r.has_children && !st.expanded.remove(&r.id) {
            st.expanded.insert(r.id.clone());
        }
    };
    let by = u16::try_from(page).unwrap_or(u16::MAX);
    match a {
        Action::Up if st.help_open => st.help_scroll = st.help_scroll.saturating_sub(1),
        Action::Down if st.help_open => st.help_scroll = st.help_scroll.saturating_add(1),
        Action::PageUp if st.help_open => st.help_scroll = st.help_scroll.saturating_sub(by),
        Action::PageDown if st.help_open => st.help_scroll = st.help_scroll.saturating_add(by),
        Action::Home if st.help_open => st.help_scroll = 0,
        Action::End if st.help_open => st.help_scroll = u16::MAX,
        Action::Up => go(st, cur.map_or(0, |i| i.saturating_sub(1))),
        Action::Down => go(st, cur.map_or(0, |i| i + 1)),
        Action::PageUp => go(st, cur.unwrap_or(0).saturating_sub(page)),
        Action::PageDown => go(st, cur.unwrap_or(0) + page),
        Action::Home => go(st, 0),
        Action::End => go(st, usize::MAX),
        Action::Select(i) if *i < rows.len() => go(st, *i),
        Action::Collapse => {
            if let Some(i) = cur {
                if rows[i].has_children && rows[i].expanded {
                    st.expanded.remove(&rows[i].id);
                } else if let Some(parent) = parent_index(rows, i) {
                    go(st, parent);
                }
            }
        }
        Action::Expand => {
            if let Some(r) = cur.map(|i| &rows[i])
                && r.has_children
                && !r.expanded
            {
                st.expanded.insert(r.id.clone());
            }
        }
        Action::Toggle => {
            if let Some(r) = cur.map(|i| &rows[i]) {
                flip(st, r);
            }
        }
        Action::ToggleAt(i) => {
            if let Some(r) = rows.get(*i) {
                flip(st, r);
            }
        }
        Action::ToggleView => {
            st.view = match st.view {
                ViewMode::BySession => ViewMode::McpOverview,
                ViewMode::McpOverview => ViewMode::BySession,
            };
            st.selected = None;
            st.selected_path.clear();
        }
        Action::CycleSort => st.sort = st.sort.next(),
        Action::StartSearch => {
            st.search_editing = true;
            st.search.get_or_insert_with(String::new);
        }
        Action::SearchChar(c) if st.search_editing => {
            st.search.get_or_insert_with(String::new).push(*c);
        }
        Action::SearchBackspace if st.search_editing => {
            if let Some(s) = st.search.as_mut() {
                s.pop();
            }
        }
        Action::SearchDone => {
            st.search_editing = false;
            if st.search.as_deref() == Some("") {
                st.search = None;
            }
        }
        Action::Cancel => {
            if st.help_open {
                st.help_open = false;
                st.help_scroll = 0;
            } else {
                st.search = None;
                st.search_editing = false;
            }
        }
        Action::TogglePause => st.paused = !st.paused,
        Action::Faster => st.interval_secs = st.interval_secs.saturating_sub(1).max(MIN_INTERVAL),
        Action::Slower => st.interval_secs = (st.interval_secs + 1).min(MAX_INTERVAL),
        Action::Help => {
            st.help_open = !st.help_open;
            st.help_scroll = 0;
        }
        _ => {}
    }
    refresh_path(st, rows);
}

/// Sets `selected_path` from where `selected` is in `rows`; unchanged when it
/// is not there.
pub(super) fn refresh_path(st: &mut UiState, rows: &[Row]) {
    let at = st
        .selected
        .as_ref()
        .and_then(|s| rows.iter().position(|r| r.id == *s));
    if let Some(i) = at {
        st.selected_path = ancestors(rows, i);
    }
}
