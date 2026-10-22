//! UI state and the logic that turns a tree into rows (spec 4.6). No drawing,
//! no terminal.

pub mod flatten;
pub mod state;

pub use flatten::{Row, flatten, merge_exited, reselect};
pub use state::{Action, Dialog, SortKey, UiState, apply, note_seen};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Category;
    use crate::i18n::Lang;
    use crate::model::ProcKey;
    use crate::testkit::{base, p, tree};
    use crate::tree::{self, Label, Node, NodeId, ViewMode};
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    fn st() -> UiState {
        UiState::new(Lang::ZhTw)
    }

    #[test]
    fn default_expansion() {
        let t = tree(base(), ViewMode::BySession);
        let r = flatten(&t, &st(), Instant::now());
        let ids: Vec<_> = r.iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids[0], NodeId::Category(Category::ClaudeCode));
        assert!(matches!(ids[1], NodeId::Proc(_)));
        assert_eq!(ids[2], NodeId::Category(Category::Codex));
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn sort_keeps_categories_and_selfrow_fixed() {
        let mut v = base();
        v.push(p(700, 100, 80, &["claude"]).mem_private(Some(500 << 20)));
        let t = tree(v, ViewMode::BySession);
        let mut s = st();
        let r = flatten(&t, &s, Instant::now());
        assert_eq!(r[1].node.pid, Some(700));
        let i = r.iter().position(|x| x.node.pid == Some(200)).unwrap();
        s.expanded.insert(r[i].id.clone());
        let r = flatten(&t, &s, Instant::now());
        assert!(matches!(r[i + 1].id, NodeId::SelfRow(_)));
        s.sort = SortKey::Pid;
        let r = flatten(&t, &s, Instant::now());
        assert_eq!(r[0].id, NodeId::Category(Category::ClaudeCode));
        assert_eq!(SortKey::Name.next(), SortKey::Mem);
    }

    #[test]
    fn search_reveals_matches_and_ancestors() {
        let t = tree(base(), ViewMode::BySession);
        let mut s = st();
        s.search = Some("playwright".into());
        let r = flatten(&t, &s, Instant::now());
        assert!(
            r.iter()
                .any(|x| matches!(&x.node.label, Label::Mcp { name, .. } if name == "playwright"))
        );
        assert!(
            r.iter()
                .all(|x| !matches!(&x.node.label, Label::Mcp { name, .. } if name == "chrome-cdp"))
        );
    }

    #[test]
    fn selection_follows_identity_and_recovers() {
        let t = tree(base(), ViewMode::BySession);
        let mut s = st();
        let r = flatten(&t, &s, Instant::now());
        apply(&mut s, &Action::Select(1), &r, 10);
        let sel = s.selected.clone().unwrap();
        assert!(matches!(sel, NodeId::Proc(_)));
        s.sort = SortKey::Name;
        let r2 = flatten(&t, &s, Instant::now());
        assert!(!reselect(&mut s, &r2));
        assert_eq!(s.selected, Some(sel));
        let t3 = tree(
            base().into_iter().filter(|p| p.key.pid != 200).collect(),
            ViewMode::BySession,
        );
        let r3 = flatten(&t3, &s, Instant::now());
        assert!(reselect(&mut s, &r3));
        assert_eq!(s.selected, Some(NodeId::Category(Category::ClaudeCode)));
    }

    #[test]
    fn collapse_on_collapsed_moves_to_parent() {
        let t = tree(base(), ViewMode::BySession);
        let mut s = st();
        let r = flatten(&t, &s, Instant::now());
        s.selected = Some(r[1].id.clone());
        apply(&mut s, &Action::Collapse, &r, 10);
        assert_eq!(s.selected, Some(NodeId::Category(Category::ClaudeCode)));
    }

    fn all_ids(ns: &[Node], out: &mut HashSet<NodeId>) {
        for n in ns {
            out.insert(n.id.clone());
            all_ids(&n.children, out);
        }
    }

    #[test]
    fn exited_rows_linger_one_refresh() {
        let watchdog = NodeId::Proc(ProcKey {
            pid: 212,
            start_time: 23,
        });
        let a = tree(base(), ViewMode::BySession);
        let without: Vec<_> = base().into_iter().filter(|p| p.key.pid != 212).collect();
        let b = merge_exited(&a, tree(without.clone(), ViewMode::BySession));
        assert!(tree::find(&b, &watchdog).is_some_and(|n| n.exited));
        let c = merge_exited(&b, tree(without, ViewMode::BySession));
        assert!(tree::find(&c, &watchdog).is_none());
    }

    #[test]
    fn only_processes_appearing_later_are_new() {
        let mut s = st();
        let t0 = Instant::now();
        note_seen(&mut s, &tree(base(), ViewMode::BySession), t0);
        let mut v = base();
        v.push(p(231, 200, 31, &["/bin/zsh", "-c", "pwd"]));
        let t = tree(v, ViewMode::BySession);
        note_seen(&mut s, &t, t0 + Duration::from_secs(1));
        all_ids(&t, &mut s.expanded);
        let r = flatten(&t, &s, t0 + Duration::from_secs(2));
        assert!(r.iter().any(|x| x.node.pid == Some(231) && x.is_new));
        assert!(r.iter().all(|x| x.node.pid != Some(230) || !x.is_new));
        assert!(
            flatten(&t, &s, t0 + Duration::from_secs(7))
                .iter()
                .all(|x| !x.is_new)
        );
    }

    #[test]
    fn process_is_new_when_cctree_started_with_nothing_to_show() {
        let mut s = st();
        let t0 = Instant::now();
        note_seen(&mut s, &tree(Vec::new(), ViewMode::BySession), t0);
        let t = tree(base(), ViewMode::BySession);
        note_seen(&mut s, &t, t0 + Duration::from_secs(1));
        let r = flatten(&t, &s, t0 + Duration::from_secs(2));
        assert!(r.iter().any(|x| x.node.pid == Some(200) && x.is_new));
    }

    #[test]
    fn ended_session_returns_whole_but_not_its_survivors() {
        let a = tree(base(), ViewMode::BySession);
        let after: Vec<_> = base().into_iter().filter(|p| p.key.pid != 200).collect();
        let b = merge_exited(&a, tree(after, ViewMode::BySession));
        let mut s = st();
        all_ids(&b, &mut s.expanded);
        let r = flatten(&b, &s, Instant::now());
        let procs = |pid| {
            r.iter()
                .filter(|x| x.node.pid == Some(pid) && matches!(x.id, NodeId::Proc(_)))
                .map(|x| x.node.exited)
                .collect::<Vec<_>>()
        };
        // The session and its shell are back, grey; the playwright server
        // outlived the session, so it is the orphan below, once, not grey.
        assert_eq!(procs(200), [true]);
        assert_eq!(procs(230), [true]);
        assert_eq!(procs(221), [false]);
    }

    #[test]
    fn keys_drive_search_sort_view_pause_and_interval() {
        let mut s = st();
        let press = |s: &mut UiState, a: Action| apply(s, &a, &[], 10);
        press(&mut s, Action::SearchChar('x'));
        assert_eq!(s.search, None);
        press(&mut s, Action::StartSearch);
        for c in ['a', 'b'] {
            press(&mut s, Action::SearchChar(c));
        }
        press(&mut s, Action::SearchBackspace);
        assert_eq!((s.search.as_deref(), s.search_editing), (Some("a"), true));
        press(&mut s, Action::SearchDone);
        assert_eq!((s.search.as_deref(), s.search_editing), (Some("a"), false));
        press(&mut s, Action::Cancel);
        assert_eq!(s.search, None);

        press(&mut s, Action::CycleSort);
        assert_eq!(s.sort, SortKey::Cpu);
        press(&mut s, Action::TogglePause);
        assert!(s.paused);
        for _ in 0..3 {
            press(&mut s, Action::Faster);
        }
        assert_eq!(s.interval_secs, 1);
        for _ in 0..20 {
            press(&mut s, Action::Slower);
        }
        assert_eq!(s.interval_secs, 10);
        s.selected = Some(NodeId::Category(Category::Codex));
        press(&mut s, Action::ToggleView);
        assert_eq!((s.view, s.selected), (ViewMode::McpOverview, None));
    }
}
