//! Turning terminal input into [`Action`]s (the key table of spec 4.6).
//!
//! What a key means depends on what is open, checked in this order:
//!
//! 1. `Ctrl+C` always quits.
//! 2. While processes are being ended, every other key is ignored; the kill
//!    dialog takes only `Enter` (confirm) and `Esc` (cancel).
//! 3. The help page: the moving keys scroll it, `Esc`, `q` and `?` close it,
//!    every other key is ignored.
//! 4. The search box: characters go into the search, `Backspace` deletes,
//!    `Enter` is done, `Esc` clears; `↑` and `↓` still move.
//! 5. Otherwise the table of spec 4.6. `Esc` clears a search that is set.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::Row;
use super::render::Layout;
use super::state::{Action, Dialog, UiState};

/// Rows a wheel notch moves: the app applies the [`map_mouse`] action this
/// many times.
pub const WHEEL_ROWS: usize = 3;

pub fn map_key(ev: KeyEvent, st: &UiState) -> Action {
    use Action as A;
    use KeyCode::*;
    if ev.code == Char('c') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        return A::Quit;
    }
    match &st.dialog {
        Some(Dialog::Killing { .. }) => return A::None,
        Some(Dialog::ConfirmKill(_)) => {
            return match ev.code {
                Enter => A::Confirm,
                Esc => A::Cancel,
                _ => A::None,
            };
        }
        None => {}
    }
    let moving = match ev.code {
        Up | Char('k') if !st.search_editing => A::Up,
        Down | Char('j') if !st.search_editing => A::Down,
        Up => A::Up,
        Down => A::Down,
        PageUp => A::PageUp,
        PageDown => A::PageDown,
        Home => A::Home,
        End => A::End,
        _ => A::None,
    };
    if st.help_open {
        return match ev.code {
            Esc | Char('q') => A::Cancel,
            Char('?') => A::Help,
            _ => moving,
        };
    }
    if st.search_editing {
        return match ev.code {
            Char(c) => A::SearchChar(c),
            Backspace => A::SearchBackspace,
            Enter => A::SearchDone,
            Esc => A::Cancel,
            Up | Down => moving,
            _ => A::None,
        };
    }
    match ev.code {
        Left | Char('h') => A::Collapse,
        Right | Char('l') => A::Expand,
        Enter => A::Toggle,
        Char('x') => A::KillOne,
        Char('X') => A::KillTree,
        Char('m') => A::ToggleView,
        Char('s') => A::CycleSort,
        Char('/') => A::StartSearch,
        Char(' ') => A::TogglePause,
        Char('+') => A::Faster,
        Char('-') => A::Slower,
        Char('?') => A::Help,
        Char('q') => A::Quit,
        Esc => A::Cancel,
        _ => moving,
    }
}

/// The wheel moves the cursor (or scrolls the help page) by [`WHEEL_ROWS`];
/// a left click on a row selects it, and on its ▶ or ▼ (see [`Layout`])
/// folds or unfolds it. Clicks are ignored while the help page is open, and
/// the mouse is ignored while a kill dialog is.
pub fn map_mouse(ev: MouseEvent, st: &UiState, rows: &[Row], layout: &Layout) -> Action {
    if st.dialog.is_some() {
        return Action::None;
    }
    match ev.kind {
        MouseEventKind::ScrollUp => Action::Up,
        MouseEventKind::ScrollDown => Action::Down,
        MouseEventKind::Down(MouseButton::Left) if !st.help_open => {
            let t = layout.table;
            let inside = (t.y..t.y.saturating_add(t.height)).contains(&ev.row)
                && (t.x..t.x.saturating_add(t.width)).contains(&ev.column);
            let i = layout.first_row + usize::from(ev.row.saturating_sub(t.y));
            match rows.get(i) {
                Some(r) if inside => {
                    let glyph = usize::from(t.x) + r.depth * 2;
                    if r.has_children && usize::from(ev.column) == glyph {
                        Action::ToggleAt(i)
                    } else {
                        Action::Select(i)
                    }
                }
                _ => Action::None,
            }
        }
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::testkit::{base, tree};
    use crate::tree::ViewMode;
    use crate::ui::flatten;
    use crate::ui::mock::mock_mcp_plan;
    use ratatui::layout::Rect;
    use std::time::Instant;

    #[test]
    fn key_map_normal_mode() {
        let s = UiState::new(Lang::En);
        for (k, a) in [
            (KeyCode::Char('x'), Action::KillOne),
            (KeyCode::Char('X'), Action::KillTree),
            (KeyCode::Char('m'), Action::ToggleView),
            (KeyCode::Char('s'), Action::CycleSort),
            (KeyCode::Char(' '), Action::TogglePause),
            (KeyCode::Char('+'), Action::Faster),
            (KeyCode::Char('-'), Action::Slower),
            (KeyCode::Char('?'), Action::Help),
            (KeyCode::Char('q'), Action::Quit),
            (KeyCode::Char('j'), Action::Down),
            (KeyCode::Left, Action::Collapse),
            (KeyCode::Char('l'), Action::Expand),
            (KeyCode::Enter, Action::Toggle),
            (KeyCode::Char('/'), Action::StartSearch),
        ] {
            assert_eq!(map_key(KeyEvent::from(k), &s), a, "{k:?}");
        }
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), &s),
            Action::Quit
        );
    }

    #[test]
    fn key_map_dialog_and_search_modes() {
        let mut s = UiState::new(Lang::En);
        s.dialog = Some(Dialog::Killing { done: 0, total: 1 });
        assert_eq!(
            map_key(KeyEvent::from(KeyCode::Char('x')), &s),
            Action::None
        );
        s.dialog = Some(Dialog::ConfirmKill(mock_mcp_plan()));
        assert_eq!(map_key(KeyEvent::from(KeyCode::Enter), &s), Action::Confirm);
        assert_eq!(map_key(KeyEvent::from(KeyCode::Esc), &s), Action::Cancel);
        let mut s = UiState::new(Lang::En);
        s.search_editing = true;
        assert_eq!(
            map_key(KeyEvent::from(KeyCode::Char('x')), &s),
            Action::SearchChar('x')
        );
    }

    #[test]
    fn help_page_takes_scroll_and_close_keys_only() {
        let mut s = UiState::new(Lang::En);
        s.help_open = true;
        for (k, a) in [
            (KeyCode::Down, Action::Down),
            (KeyCode::Char('k'), Action::Up),
            (KeyCode::PageDown, Action::PageDown),
            (KeyCode::End, Action::End),
            (KeyCode::Esc, Action::Cancel),
            (KeyCode::Char('q'), Action::Cancel),
            (KeyCode::Char('?'), Action::Help),
            (KeyCode::Char('x'), Action::None),
            (KeyCode::Enter, Action::None),
        ] {
            assert_eq!(map_key(KeyEvent::from(k), &s), a, "{k:?}");
        }
    }

    #[test]
    fn mouse_wheel_moves_click_selects_glyph_folds() {
        let s = UiState::new(Lang::En);
        let t = tree(base(), ViewMode::BySession);
        let rows = flatten(&t, &s, Instant::now());
        // Row 1 is a session (depth 1, has children); the table starts at
        // line 3 and is scrolled by one row.
        assert!(rows[1].has_children && rows[1].depth == 1);
        let layout = Layout {
            table: Rect::new(0, 3, 80, 10),
            first_row: 1,
            help_scroll: 0,
        };
        let ev = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let click = |column, row| ev(MouseEventKind::Down(MouseButton::Left), column, row);
        let map = |e| map_mouse(e, &s, &rows, &layout);
        assert_eq!(map(ev(MouseEventKind::ScrollDown, 5, 5)), Action::Down);
        assert_eq!(map(ev(MouseEventKind::ScrollUp, 5, 5)), Action::Up);
        assert_eq!(map(click(10, 3)), Action::Select(1));
        assert_eq!(map(click(2, 3)), Action::ToggleAt(1));
        assert_eq!(map(click(10, 2)), Action::None);
        assert_eq!(map(click(10, 12)), Action::None);
        let mut open = UiState::new(Lang::En);
        open.help_open = true;
        assert_eq!(map_mouse(click(10, 3), &open, &rows, &layout), Action::None);
    }
}
