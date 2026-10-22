//! Drawing (spec 4.6, 4.7): one [`View`] becomes one frame. Nothing here
//! changes state or reads the terminal; [`draw`] says where the table is and
//! where it scrolled to, and the app does the rest.
//!
//! # Layout, top to bottom
//!
//! 1. the header, 1 line;
//! 2. the banner, only with [`View::banner`]: the message wrapped, up to 3
//!    lines (a banner can be long, and its last part says what to do);
//! 3. the column titles, 1 line, then the table;
//! 4. a rule and the detail panel, 4 lines;
//! 5. the message line, 1 line, and the key bar, 1 line.
//!
//! The terminal must be at least 80×24 ([`MIN_WIDTH`], [`MIN_HEIGHT`]); below
//! that only [`Msg::TooSmall`] is drawn. The help page and the kill dialog
//! are drawn over the main screen. The help page is longer than 24 lines, so
//! it scrolls ([`UiState::help_scroll`]); a hint at its foot says when there
//! is more below.
//!
//! # Choices the spec leaves open
//!
//! - **Detail panel**: spec 4.6 says 3 to 4 lines; this is a rule plus 4 lines
//!   of text. Spec 4.4 asks for the memory sentence and "what the system's
//!   tool shows" besides the ids and the command, and at 80 columns the
//!   memory sentence alone can take 2 lines. The lines, in order: name and
//!   ids; why it is orphaned (orphans only); the memory sentence; what the
//!   system's tool shows (only when there is room); the command line, with how
//!   long it has run at its end. The command line always stays.
//! - **Messages**: the message line shows the message cut to one line. While a
//!   message is up the detail panel shows all of it, wrapped (the "what to do"
//!   part is the end of a message and must not be lost), instead of the
//!   selected row.
//! - **Too wide for 80 columns**: the header and the key bar drop their items
//!   by priority until they fit, and neither wraps. The header drops the sort,
//!   the view, `? help`, ...; the key bar drops `/`, `s`, `m`, pause, `?`, in
//!   that order, so the moving, folding and ending keys and `q` stay.
//! - **Scrolling**: [`UiState::scroll`] is read, not written, because the view
//!   is immutable; [`Layout::first_row`] is where the table starts after
//!   keeping the cursor in view, and the app stores it back.
//! - **No colour** ([`View::color`] false): no foreground or background colour
//!   anywhere. The cursor is reversed, "new" and warnings are bold, grey
//!   things are dim.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::classify::{Category, Certainty, Classification, McpPart, OrphanReason, Role};
use crate::format::{NA, fit_width, fmt_age, fmt_bytes, fmt_cpu, fmt_mem, shorten_home};
use crate::i18n::{
    self, Lang, MIN_HEIGHT, MIN_WIDTH, Msg, Resume, Text, app_server_title, category_title,
    detail_group, detail_ids, detail_mem_line, detail_os_tool, header_interval, header_ram,
    header_self, header_sort, header_swap, header_zombies, help_keys, help_memory_terms,
    kill_interrupts, kill_more, kill_progress, kill_total, launched_by, mcp_group_title,
    mcp_instances, msg, nested_note, orphan_reason_parent_gone, orphan_reason_path_only,
    orphan_reason_session_ended, tr,
};
use crate::kill::{KillPlan, KillTarget, KillWarning, ResumeHint};
use crate::mask::mask_argv;
use crate::model::{MemUnavailable, Platform, ProcKey, ProcStatus, Snapshot, Tool};
use crate::tree::{Label, NodeId, ViewMode};

use super::flatten::parent_index;
use super::state::{Dialog, SortKey};
use super::{Row, UiState};

/// Column widths of the table, each including the gap before it, so the
/// numbers are right-aligned and a 7-digit PID still has a space in front.
const PID_W: usize = 8;
const CPU_W: usize = 8;
const MEM_W: usize = 12;
const COUNT_W: usize = 7;
const NUMBERS_W: usize = PID_W + CPU_W + MEM_W + COUNT_W;

/// Lines of text in the detail panel (below its rule).
const DETAIL_H: usize = 4;
/// The most lines a banner takes.
const BANNER_MAX: usize = 3;
/// The widest the kill dialog gets, borders included.
const DIALOG_W: u16 = 72;
/// The most processes the kill dialog lists.
const MAX_LISTED: usize = 10;

/// What [`draw`] needs. Nothing in it is changed.
pub struct View<'a> {
    pub st: &'a UiState,
    /// The rows of the table, from [`flatten`](super::flatten).
    pub rows: &'a [Row],
    /// `None` until the first collection is done.
    pub snap: Option<&'a Snapshot>,
    pub cls: Option<&'a Classification>,
    /// `false` when `NO_COLOR` is set.
    pub color: bool,
    /// A line above the table (the WSL notice).
    pub banner: Option<Msg>,
    /// Unix seconds, for "running for".
    pub now: u64,
}

/// Where [`draw`] put the table, for turning a mouse position into a row.
///
/// The row at screen line `y` is `first_row + (y - table.y)` when `y` is in
/// `table.y..table.y + table.height` and that index is below `rows.len()`. A
/// row's fold mark (▶ or ▼, a blank when it has no children) is at column
/// `table.x + depth * 2`, 1 column wide, so a click on `x == table.x + depth *
/// 2` folds or unfolds a row that has children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The rows only, without the column titles.
    pub table: Rect,
    /// The index of the first row drawn; store it in [`UiState::scroll`].
    pub first_row: usize,
    /// How far the help page is scrolled once clamped to its length; store it
    /// in [`UiState::help_scroll`]. Without it, `End` followed by `Up` would
    /// start from a number far past the last line.
    pub help_scroll: usize,
}

/// Draws `v` on `f`.
pub fn draw(f: &mut Frame, v: &View) -> Layout {
    let area = f.area();
    let ui = Ui {
        v,
        lang: v.st.lang,
        look: Look { color: v.color },
        home: v.snap.and_then(|s| s.home.as_deref()),
    };
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        ui.too_small(f, area);
        return Layout {
            table: Rect::default(),
            first_row: 0,
            help_scroll: v.st.help_scroll.into(),
        };
    }
    let mut layout = ui.main(f, area);
    if v.st.help_open {
        layout.help_scroll = ui.help(f, area);
    } else if let Some(Dialog::ConfirmKill(plan)) = &v.st.dialog {
        ui.kill_dialog(f, area, plan);
    }
    layout
}

/// The styles. With no colour only modifiers are used.
#[derive(Clone, Copy)]
struct Look {
    color: bool,
}

impl Look {
    fn paint(self, c: Color, plain: Modifier) -> Style {
        if self.color {
            Style::default().fg(c)
        } else {
            Style::default().add_modifier(plain)
        }
    }

    fn dim(self) -> Style {
        self.paint(Color::DarkGray, Modifier::DIM)
    }

    /// A process that appeared a moment ago.
    fn fresh(self) -> Style {
        self.paint(Color::Green, Modifier::BOLD)
    }

    fn warn(self) -> Style {
        self.paint(Color::Yellow, Modifier::BOLD)
    }

    fn danger(self) -> Style {
        self.paint(Color::Red, Modifier::BOLD)
            .add_modifier(Modifier::BOLD)
    }

    fn bold(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn cursor(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }
}

type Styled = (String, Style);

struct Ui<'a> {
    v: &'a View<'a>,
    lang: Lang,
    look: Look,
    home: Option<&'a str>,
}

impl Ui<'_> {
    fn too_small(&self, f: &mut Frame, area: Rect) {
        let text = msg(
            self.lang,
            &Msg::TooSmall {
                w: area.width,
                h: area.height,
            },
        );
        let lines: Vec<Line> = wrap(&text, area.width as usize)
            .into_iter()
            .map(Line::from)
            .collect();
        f.render_widget(Paragraph::new(lines), area);
    }

    fn main(&self, f: &mut Frame, area: Rect) -> Layout {
        let (x, w) = (area.x, area.width);
        let banner: Vec<String> = self
            .v
            .banner
            .as_ref()
            .map(|m| wrap(&msg(self.lang, m), (w as usize).saturating_sub(2)))
            .unwrap_or_default();
        let banner_h = banner.len().min(BANNER_MAX) as u16;
        let titles_y = area.y + 1 + banner_h;
        let rule_y = area.bottom() - (1 + DETAIL_H as u16 + 2);
        let table = Rect::new(x, titles_y + 1, w, rule_y - titles_y - 1);

        self.header(f, Rect::new(x, area.y, w, 1));
        if banner_h > 0 {
            let lines: Vec<Line> = clamp(banner, BANNER_MAX, w as usize - 2)
                .into_iter()
                .map(|l| Line::styled(format!(" {l}"), self.look.warn()))
                .collect();
            f.render_widget(Paragraph::new(lines), Rect::new(x, area.y + 1, w, banner_h));
        }
        let first_row = self.table(f, Rect::new(x, titles_y, w, 1), table);

        let rule = "─".repeat(w as usize);
        f.render_widget(
            Paragraph::new(Line::styled(rule, self.look.dim())),
            Rect::new(x, rule_y, w, 1),
        );
        let detail: Vec<Line> = self
            .detail((w as usize).saturating_sub(2))
            .into_iter()
            .map(|(t, s)| Line::styled(format!(" {t}"), s))
            .collect();
        f.render_widget(
            Paragraph::new(detail),
            Rect::new(x, rule_y + 1, w, DETAIL_H as u16),
        );
        let msg_y = rule_y + 1 + DETAIL_H as u16;
        self.message_line(f, Rect::new(x, msg_y, w, 1));
        self.key_bar(f, Rect::new(x, msg_y + 1, w, 1));
        Layout {
            table,
            first_row,
            help_scroll: self.v.st.help_scroll.into(),
        }
    }

    // ---- Header and key bar -------------------------------------------------

    fn header(&self, f: &mut Frame, r: Rect) {
        let (lang, st, look) = (self.lang, self.v.st, self.look);
        let item = |prio, text: String| Item::new(prio, text, Style::default());
        let mut items = vec![if st.paused {
            Item::new(0, tr(lang, Text::HeaderPaused).to_string(), look.warn())
        } else {
            item(0, header_interval(lang, st.interval_secs))
        }];
        if let Some(s) = self.v.snap {
            let sys = &s.system;
            items.push(item(
                1,
                header_ram(lang, &fmt_bytes(sys.ram_used), &fmt_bytes(sys.ram_total)),
            ));
            let windows = s.platform == Platform::Windows;
            items.push(item(
                4,
                header_swap(lang, windows, &fmt_bytes(sys.swap_used)),
            ));
            let zombies = s
                .procs
                .iter()
                .filter(|p| p.status == ProcStatus::Zombie)
                .count();
            if zombies > 0 {
                items.push(Item::new(2, header_zombies(lang, zombies), look.warn()));
            }
            if let Some(own) = s.get(s.self_key).and_then(|p| p.mem?.private) {
                items.push(item(3, header_self(lang, &fmt_bytes(own))));
            }
        }
        let view = match st.view {
            ViewMode::BySession => Text::ViewBySession,
            ViewMode::McpOverview => Text::ViewMcpOverview,
        };
        items.push(item(6, tr(lang, view).to_string()));
        items.push(item(7, header_sort(lang, sort_text(st.sort))));
        let mut help = Item::new(5, tr(lang, Text::KeyHelp).to_string(), look.dim());
        help.right = true;
        items.push(help);

        let width = r.width as usize;
        let brand_w = "cctree  ".width();
        let kept = keep_fitting(items, 3, width.saturating_sub(brand_w));
        let mut spans = vec![Span::styled("cctree", look.bold()), Span::raw("  ")];
        let mut used = brand_w;
        let mut help = None;
        for it in kept {
            if it.right {
                help = Some(it);
                continue;
            }
            if used > brand_w {
                spans.push(Span::styled(" · ", look.dim()));
                used += 3;
            }
            used += it.text.width();
            spans.push(Span::styled(it.text, it.style));
        }
        if let Some(it) = help {
            let pad = width.saturating_sub(used + it.text.width());
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(it.text, it.style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), r);
    }

    fn key_bar(&self, f: &mut Frame, r: Rect) {
        let (lang, st) = (self.lang, self.v.st);
        let width = r.width as usize;
        let text = if st.help_open {
            tr(lang, Text::HelpClose).to_string()
        } else if st.search_editing {
            tr(lang, Text::KeySearching).to_string()
        } else if matches!(st.dialog, Some(Dialog::ConfirmKill(_))) {
            format!(
                "{}  {}",
                tr(lang, Text::DialogConfirm),
                tr(lang, Text::DialogCancel)
            )
        } else {
            let mut keys = i18n::key_bar(st.paused).to_vec();
            // `s` and `/` are not in the standard bar: they go after `m`.
            keys.splice(5..5, [Text::KeySort, Text::KeySearch]);
            let items = keys
                .into_iter()
                .map(|k| Item::new(key_priority(k), tr(lang, k).to_string(), Style::default()))
                .collect();
            keep_fitting(items, 2, width.saturating_sub(1))
                .into_iter()
                .map(|i| i.text)
                .collect::<Vec<_>>()
                .join("  ")
        };
        let line = format!(" {}", clip(&text, width.saturating_sub(1)));
        f.render_widget(Paragraph::new(line), r);
    }

    /// The message, or the progress of ending processes, or the search box.
    fn message_line(&self, f: &mut Frame, r: Rect) {
        let (lang, st) = (self.lang, self.v.st);
        let (text, style) = if let Some(Dialog::Killing { done, total }) = &st.dialog {
            (kill_progress(lang, *done, *total), self.look.warn())
        } else if let Some(m) = &st.message {
            (msg(lang, m), self.look.warn())
        } else if let Some(q) = &st.search {
            let mut s = format!("{}{q}", tr(lang, Text::SearchPrompt));
            if st.search_editing {
                s.push('▏');
            }
            if self.v.rows.is_empty() {
                s.push_str(&format!("  {}", tr(lang, Text::SearchNoMatch)));
            }
            (s, Style::default())
        } else {
            return;
        };
        let line = format!(" {}", clip(&text, (r.width as usize).saturating_sub(2)));
        f.render_widget(Paragraph::new(Line::styled(line, style)), r);
    }

    // ---- The table -----------------------------------------------------------

    /// Draws the column titles and the rows; returns the first row drawn.
    fn table(&self, f: &mut Frame, titles: Rect, table: Rect) -> usize {
        let (lang, rows) = (self.lang, self.v.rows);
        let name_w = (table.width as usize).saturating_sub(NUMBERS_W);
        let col = |t| tr(lang, t);
        let head = Line::from(vec![
            Span::raw(fit_width(&format!("  {}", col(Text::ColName)), name_w)),
            Span::raw(right(col(Text::ColPid), PID_W)),
            Span::raw(right(col(Text::ColCpu), CPU_W)),
            Span::raw(right(col(Text::ColMem), MEM_W)),
            Span::raw(right(col(Text::ColCount), COUNT_W)),
        ])
        .style(self.look.bold());
        f.render_widget(Paragraph::new(head), titles);

        let at = self.selected();
        let first = first_row(self.v.st.scroll, at, table.height as usize, rows.len());
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(first)
            .take(table.height as usize)
            .map(|(i, r)| self.row_line(r, name_w, Some(i) == at))
            .collect();
        f.render_widget(Paragraph::new(lines), table);
        first
    }

    /// The index of the row under the cursor.
    fn selected(&self) -> Option<usize> {
        let id = self.v.st.selected.as_ref()?;
        self.v.rows.iter().position(|r| r.id == *id)
    }

    fn row_line(&self, r: &Row, name_w: usize, selected: bool) -> Line<'static> {
        let (n, look) = (&r.node, self.look);
        let base = if n.exited {
            look.dim()
        } else if r.is_new {
            look.fresh()
        } else if matches!(
            n.label,
            Label::Process { zombie: true, .. }
                | Label::Process {
                    orphan: Some(Certainty::Suspected),
                    ..
                }
                | Label::Category {
                    cat: Category::Orphaned,
                    ..
                }
        ) && n.count > 0
        {
            look.warn()
        } else {
            Style::default()
        };
        let glyph = match (r.has_children, r.expanded) {
            (false, _) => ' ',
            (true, true) => '▼',
            (true, false) => '▶',
        };
        let prefix = match &n.label {
            Label::Process {
                part: Some(McpPart::Helper),
                ..
            } => "└ ",
            Label::Process {
                orphan: Some(Certainty::Suspected),
                ..
            } => "？",
            Label::Category {
                cat: Category::Orphaned,
                ..
            } => "⚠ ",
            _ => "",
        };
        let name = format!(
            "{}{glyph} {prefix}{}",
            " ".repeat(r.depth * 2),
            self.label_text(&n.label)
        );
        // Rows with nothing to add up (a tool that is not running, the note
        // about nested sessions) show no numbers at all.
        let numbers = n.count > 0;
        let hide_count = match r.id {
            NodeId::SelfRow(_) => true,
            NodeId::Proc(_) => !r.has_children,
            _ => false,
        };
        let cell = |show: bool, s: String| if show { s } else { String::new() };
        let pid = n.pid.map(|p| p.to_string()).unwrap_or_default();
        let pid_style = if n.pid_dim { look.dim() } else { base };
        let line = Line::from(vec![
            Span::styled(fit_width(&name, name_w), base),
            Span::styled(right(&pid, PID_W), pid_style),
            Span::styled(right(&cell(numbers, fmt_cpu(n.cpu)), CPU_W), base),
            Span::styled(
                right(&cell(numbers, fmt_mem(n.mem, n.approx, self.lang)), MEM_W),
                base,
            ),
            Span::styled(
                right(&cell(numbers && !hide_count, n.count.to_string()), COUNT_W),
                base,
            ),
        ]);
        if selected {
            line.style(look.cursor())
        } else {
            line
        }
    }

    /// What a row says as its name, without the fold mark and the markers the
    /// table adds in front (`└`, `？`, `⚠`).
    fn label_text(&self, l: &Label) -> String {
        let lang = self.lang;
        match l {
            Label::Category { cat, n } => category_title(lang, category_text(*cat), *n),
            Label::Session {
                tool,
                cwd,
                launcher,
                app_server,
                ..
            } => {
                if *app_server {
                    return app_server_title(lang, launcher.as_deref());
                }
                let base = cwd
                    .as_deref()
                    .map(|c| shorten_home(c, self.home))
                    .unwrap_or_else(|| tool_name(lang, *tool).to_string());
                match launcher {
                    Some(l) => format!("{base}{}", launched_by(lang, l)),
                    None => base,
                }
            }
            Label::McpGroup { n } => mcp_group_title(lang, *n),
            Label::Mcp { name, plugin } => {
                if *plugin {
                    format!("{name}{}", mark(lang, tr(lang, Text::MarkPlugin)))
                } else {
                    name.clone()
                }
            }
            Label::OtherChildren => tr(lang, Text::GroupOtherChildren).to_string(),
            Label::Detached => tr(lang, Text::GroupDetached).to_string(),
            Label::SelfRow => tr(lang, Text::RowSelf).to_string(),
            Label::OtherGroup(name) => name.clone(),
            Label::Process {
                name,
                part,
                zombie,
                orphan,
            } => {
                let mut s = match part {
                    Some(McpPart::Wrapper) => tr(lang, Text::RowNpxShell).to_string(),
                    _ => name.clone(),
                };
                match orphan {
                    Some(Certainty::Confirmed) => {
                        s.push_str(&mark(lang, tr(lang, Text::MarkConfirmed)));
                    }
                    Some(Certainty::Suspected) => {
                        s.push_str(&mark(lang, tr(lang, Text::MarkSuspected)));
                    }
                    None => {}
                }
                if *zombie {
                    s.push_str(&mark(lang, tr(lang, Text::MarkZombie)));
                }
                s
            }
            Label::NestedNote { tool, n, inside } => {
                nested_note(lang, *n, tool_name(lang, *tool), tool_name(lang, *inside))
            }
            Label::OverviewMcp { name, instances } => {
                format!("{name}{}", mark(lang, &mcp_instances(lang, *instances)))
            }
            Label::OverviewInstance { cwd } => cwd
                .as_deref()
                .map(|c| shorten_home(c, self.home))
                .unwrap_or_else(|| NA.to_string()),
        }
    }

    // ---- The detail panel ----------------------------------------------------

    /// At most [`DETAIL_H`] lines, each at most `w` columns.
    fn detail(&self, w: usize) -> Vec<Styled> {
        let plain = |lines: Vec<String>| -> Vec<Styled> {
            clamp(lines, DETAIL_H, w)
                .into_iter()
                .map(|l| (l, Style::default()))
                .collect()
        };
        if let Some(m) = &self.v.st.message {
            return plain(wrap(&msg(self.lang, m), w));
        }
        let Some(i) = self.selected() else {
            return Vec::new();
        };
        if let NodeId::Proc(k) | NodeId::SelfRow(k) = &self.v.rows[i].id
            && let Some(lines) = self.process_detail(i, *k, w)
        {
            return plain(lines);
        }
        let r = &self.v.rows[i];
        let mut lines = vec![clip(&self.crumb(i), w)];
        if r.node.count > 0 {
            let mem = fmt_mem(r.node.mem, r.node.approx, self.lang);
            lines.push(clip(&detail_group(self.lang, r.node.count, &mem), w));
        }
        plain(lines)
    }

    /// "parent › name": where the row is, in two steps (the categories add
    /// nothing, so a session is just its name).
    fn crumb(&self, i: usize) -> String {
        let rows = self.v.rows;
        let own = self.label_text(&rows[i].node.label);
        match parent_index(rows, i).map(|p| &rows[p]) {
            Some(p) if !matches!(p.node.label, Label::Category { .. }) => {
                format!("{} › {own}", self.label_text(&p.node.label))
            }
            _ => own,
        }
    }

    /// The detail of a process row; `None` when the snapshot does not have
    /// the process (it ended since).
    fn process_detail(&self, i: usize, k: ProcKey, w: usize) -> Option<Vec<String>> {
        let lang = self.lang;
        let snap = self.v.snap?;
        let raw = snap.get(k)?;
        let ids = format!("    {}", detail_ids(lang, k.pid, raw.ppid));
        let first = format!(
            "{}{ids}",
            clip(&self.crumb(i), w.saturating_sub(ids.width()))
        );

        // Between the ids and the command, at most `DETAIL_H - 2` lines: why
        // it is orphaned, the memory sentence (cut with "…" when there is no
        // room for all of it), what the system's tool shows when there is room.
        let mut middle: Vec<String> = Vec::new();
        if let Some(cls) = self.v.cls
            && let Role::Orphan { reason, .. } = cls.role(k)
        {
            let why = format!(
                "{}{}",
                tr(lang, Text::DetailReason),
                self.reason_text(reason)
            );
            middle.push(clip(&why, w));
        }

        let m = raw.mem.unwrap_or_default();
        let total = match m.private {
            Some(b) => fmt_bytes(b),
            None => match m.unavailable {
                Some(u) => format!("{NA}{}", mark(lang, tr(lang, unavailable_text(u)))),
                None => NA.to_string(),
            },
        };
        let split = m.in_ram.zip(m.swapped_out).map(|(ram, out)| {
            (
                fmt_bytes(ram),
                fmt_mem(Some(out), m.swapped_is_estimate, lang),
            )
        });
        // What is left of the main figure when the two parts are taken off:
        // listed when it is more than 1%, so both sides of the "＝" are equal.
        let other = m
            .private
            .zip(m.in_ram)
            .zip(m.swapped_out)
            .and_then(|((p, ram), out)| p.checked_sub(ram.saturating_add(out)).map(|c| (p, c)))
            .filter(|(p, c)| c.saturating_mul(100) > *p)
            .map(|(_, c)| fmt_bytes(c));
        let shared = m.shared.filter(|s| *s > 0).map(fmt_bytes);
        let sentence = detail_mem_line(
            lang,
            &total,
            split.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
            other.as_deref(),
            shared.as_deref(),
        );
        let room = (DETAIL_H - 2).saturating_sub(middle.len());
        middle.extend(clamp(wrap(&sentence, w), room, w));
        if let Some(v) = m.os_tool_value
            && middle.len() < DETAIL_H - 2
        {
            let tool = os_tool_text(snap.platform);
            middle.push(detail_os_tool(lang, tool, &fmt_bytes(v)));
        }

        let args: Vec<String> = mask_argv(&raw.argv)
            .iter()
            .map(|a| shorten_home(a, self.home))
            .collect();
        let command = if args.is_empty() {
            NA.to_string()
        } else {
            args.join(" ")
        };
        // The age ends the line, the command gives way to it.
        let age = format!("    {}", fmt_age(k.start_time, self.v.now, lang));
        let command = clip(
            &format!("{}{command}", tr(lang, Text::DetailCommand)),
            w.saturating_sub(age.width()),
        );

        let mut lines = vec![first];
        lines.extend(middle);
        lines.push(format!("{command}{age}"));
        Some(lines)
    }

    fn reason_text(&self, reason: &OrphanReason) -> String {
        let lang = self.lang;
        match reason {
            OrphanReason::SessionEnded { tool, project_dir } => {
                let project = project_dir.as_deref().map(|d| shorten_home(d, self.home));
                orphan_reason_session_ended(lang, tool_name(lang, *tool), project.as_deref())
            }
            OrphanReason::RecordedParentGone { pid } => orphan_reason_parent_gone(lang, *pid),
            OrphanReason::PathOnly { tool } => {
                orphan_reason_path_only(lang, tool_name(lang, *tool))
            }
        }
    }

    // ---- Dialog and help -----------------------------------------------------

    fn kill_dialog(&self, f: &mut Frame, area: Rect, plan: &KillPlan) {
        let (lang, look) = (self.lang, self.look);
        let w = area.width.min(DIALOG_W);
        let text_w = (w as usize).saturating_sub(4);
        let mut lines: Vec<Styled> = Vec::new();
        let push = |lines: &mut Vec<Styled>, t: &str, s: Style| {
            lines.extend(wrap(t, text_w).into_iter().map(|l| (l, s)));
        };

        // The row the plan was made on, when it says more than the one process.
        let subject = self.v.rows.iter().find(|r| r.id == plan.subject);
        if let Some(r) = subject
            && plan.targets.len() > 1
        {
            let name = self.label_text(&r.node.label);
            lines.push((clip(&name, text_w), look.bold()));
        }

        // Parents first, as they read; the plan itself is deepest first.
        let (detached, own): (Vec<&KillTarget>, Vec<&KillTarget>) =
            plan.targets.iter().rev().partition(|t| t.detached);
        let pid_w = plan
            .targets
            .iter()
            .map(|t| t.key.pid.to_string().len())
            .max()
            .unwrap_or(1);
        let target = |t: &KillTarget| {
            let line = format!("  {:>pid_w$}  {}", t.key.pid, t.name);
            (clip(&line, text_w), Style::default())
        };
        let mut listed = 0;
        for t in own.iter().take(MAX_LISTED) {
            lines.push(target(t));
            listed += 1;
        }
        if listed < MAX_LISTED && !detached.is_empty() {
            push(&mut lines, tr(lang, Text::DialogDetachedHeader), look.dim());
            for t in detached.iter().take(MAX_LISTED - listed) {
                lines.push(target(t));
                listed += 1;
            }
        }
        if plan.targets.len() > listed {
            let more = kill_more(lang, plan.targets.len() - listed);
            lines.push((format!("  {more}"), look.dim()));
        }
        let total = kill_total(
            lang,
            plan.targets.len(),
            &fmt_bytes(plan.freed),
            plan.freed_approx,
        );
        push(&mut lines, &total, Style::default());

        let mut notes: Vec<Styled> = Vec::new();
        for warning in &plan.warnings {
            match warning {
                KillWarning::InterruptsSession { cwd, hint, .. } => {
                    let cwd = cwd.as_deref().map(|c| shorten_home(c, self.home));
                    let resume = match hint {
                        ResumeHint::Command(c) => Resume::Command(c),
                        ResumeHint::ReopenFrom(a) => Resume::ReopenFrom(a),
                    };
                    push(
                        &mut notes,
                        &kill_interrupts(lang, cwd.as_deref(), resume),
                        look.danger(),
                    );
                }
                KillWarning::McpDisconnect { tool } => {
                    let t = match tool {
                        Tool::ClaudeCode => Text::DialogMcpClaude,
                        Tool::Codex => Text::DialogMcpCodex,
                    };
                    push(&mut notes, tr(lang, t), Style::default());
                }
            }
        }
        if plan.force_only {
            push(&mut notes, tr(lang, Text::DialogForceOnly), look.danger());
        }
        if !notes.is_empty() {
            lines.push((String::new(), Style::default()));
            lines.extend(notes);
        }

        // The buttons are the last line and always stay: what does not fit
        // above them is cut.
        let room = (area.height as usize).saturating_sub(2 + 2 + 2);
        lines.truncate(room);
        let h = (lines.len() + 2 + 2) as u16;
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        let title = if plan.targets.len() == 1 {
            Text::DialogTitleOne
        } else {
            Text::DialogTitleTree
        };
        let block =
            Block::bordered().title(Line::styled(format!(" {} ", tr(lang, title)), look.bold()));
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);
        let body: Vec<Line> = lines
            .into_iter()
            .map(|(t, s)| Line::styled(format!(" {t}"), s))
            .collect();
        let body_h = body.len() as u16;
        f.render_widget(
            Paragraph::new(body),
            Rect::new(inner.x, inner.y, inner.width, body_h),
        );
        let buttons = format!(
            "{}        {}",
            tr(lang, Text::DialogConfirm),
            tr(lang, Text::DialogCancel)
        );
        let pad = (inner.width as usize).saturating_sub(buttons.width()) / 2;
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{}{buttons}", " ".repeat(pad)),
                look.bold(),
            )),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }

    /// Draws the help page over `area`; returns the scroll it used.
    fn help(&self, f: &mut Frame, area: Rect) -> usize {
        let (lang, look) = (self.lang, self.look);
        let text_w = (area.width as usize).saturating_sub(4);
        let mut lines: Vec<Styled> = Vec::new();
        let plain = Style::default();
        let heading = |lines: &mut Vec<Styled>, t: &str| {
            lines.push((String::new(), plain));
            lines.push((t.to_string(), look.bold()));
        };
        let para = |lines: &mut Vec<Styled>, t: &str, s: Style| {
            lines.extend(wrap(t, text_w).into_iter().map(|l| (format!("  {l}"), s)));
        };
        let colon = if lang == Lang::ZhTw { "：" } else { ": " };

        lines.push((tr(lang, Text::HelpTitle).to_string(), look.bold()));
        para(&mut lines, tr(lang, Text::HelpIntro), plain);

        // The memory words first: they are what the page is for, so the first
        // screen already has them.
        heading(&mut lines, tr(lang, Text::HelpMemoryTitle));
        for (name, what) in help_memory_terms(lang) {
            para(&mut lines, &format!("{name}{colon}{what}"), plain);
        }
        let platform = self.v.snap.map_or_else(Platform::current, |s| s.platform);
        let note = match platform {
            Platform::MacOs => Text::HelpPlatformMac,
            Platform::Windows => Text::HelpPlatformWindows,
            Platform::Linux => Text::HelpPlatformLinux,
        };
        para(&mut lines, tr(lang, note), plain);

        heading(&mut lines, tr(lang, Text::HelpKeysTitle));
        let keys = help_keys(lang);
        let key_w = keys.iter().map(|(k, _)| k.width()).max().unwrap_or(0) + 2;
        for (key, what) in keys {
            let pad = " ".repeat(key_w - key.width());
            lines.push((clip(&format!("  {key}{pad}{what}"), text_w + 2), plain));
        }

        heading(&mut lines, tr(lang, Text::HelpNotesTitle));
        for t in [
            Text::HelpNoteApprox,
            Text::HelpNoteCpu,
            Text::HelpNoteOrphan,
            Text::HelpNoteKill,
            Text::HelpNoteMouse,
        ] {
            para(&mut lines, tr(lang, t), plain);
        }
        lines.push((String::new(), plain));
        para(&mut lines, tr(lang, Text::HelpFooter), look.dim());

        f.render_widget(Clear, area);
        // The last line is the hint; the page scrolls above it.
        let room = (area.height as usize).saturating_sub(1);
        let scroll = (self.v.st.help_scroll as usize).min(lines.len().saturating_sub(room));
        let more = scroll + room < lines.len();
        let body: Vec<Line> = lines
            .into_iter()
            .skip(scroll)
            .take(room)
            .map(|(t, s)| Line::styled(format!(" {t}"), s))
            .collect();
        f.render_widget(Paragraph::new(body), area);
        let close = tr(lang, Text::HelpClose);
        let hint = if more {
            format!(" {}    {close}", tr(lang, Text::HelpMore))
        } else {
            format!(" {close}")
        };
        f.render_widget(
            Paragraph::new(Line::styled(hint, look.dim())),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
        scroll
    }
}

// ---- Small helpers -------------------------------------------------------------

/// One piece of the header or the key bar. When they do not fit, the items
/// with the biggest `prio` go first.
struct Item {
    prio: u8,
    text: String,
    style: Style,
    /// Drawn at the right edge.
    right: bool,
}

impl Item {
    fn new(prio: u8, text: String, style: Style) -> Item {
        Item {
            prio,
            text,
            style,
            right: false,
        }
    }
}

/// `items` without the least important ones, so that the rest, with `sep`
/// columns between them, fits in `width`. The order stays.
fn keep_fitting(mut items: Vec<Item>, sep: usize, width: usize) -> Vec<Item> {
    let total = |v: &[Item]| {
        v.iter().map(|i| i.text.width()).sum::<usize>() + sep * v.len().saturating_sub(1)
    };
    while items.len() > 1 && total(&items) > width {
        let drop = items
            .iter()
            .enumerate()
            .max_by_key(|(_, i)| i.prio)
            .map_or(0, |(i, _)| i);
        items.remove(drop);
    }
    items
}

/// How soon a key-bar entry goes when the bar is too narrow (the bigger, the
/// sooner): `/`, `s`, `m`, pause, then `?`. The moving, folding and ending
/// keys and `q` stay.
fn key_priority(k: Text) -> u8 {
    match k {
        Text::KeySearch => 9,
        Text::KeySort => 8,
        Text::KeyMcp => 7,
        Text::KeyPause | Text::KeyResume => 6,
        Text::KeyHelp => 5,
        Text::KeyKillTree => 4,
        Text::KeyKillOne => 3,
        Text::KeyFold => 2,
        Text::KeyMove => 1,
        _ => 0,
    }
}

/// The index of the first row to draw: `scroll`, moved just enough to keep
/// the cursor (`at`) in the `height` lines, and never past the last page.
fn first_row(scroll: usize, at: Option<usize>, height: usize, n: usize) -> usize {
    let mut first = scroll.min(n.saturating_sub(height));
    if let Some(i) = at {
        if i < first {
            first = i;
        } else if height > 0 && i >= first + height {
            first = i + 1 - height;
        }
    }
    first
}

/// `s` in parentheses of the language: （疑似） or " (suspected)".
fn mark(lang: Lang, s: &str) -> String {
    match lang {
        Lang::ZhTw => format!("（{s}）"),
        Lang::En => format!(" ({s})"),
    }
}

fn tool_name(lang: Lang, t: Tool) -> &'static str {
    tr(
        lang,
        match t {
            Tool::ClaudeCode => Text::CatClaudeCode,
            Tool::Codex => Text::CatCodex,
        },
    )
}

fn category_text(c: Category) -> Text {
    match c {
        Category::ClaudeCode => Text::CatClaudeCode,
        Category::Codex => Text::CatCodex,
        Category::Orphaned => Text::CatOrphaned,
        Category::OtherNode => Text::CatOtherNode,
    }
}

fn sort_text(s: SortKey) -> Text {
    match s {
        SortKey::Mem => Text::SortMem,
        SortKey::Cpu => Text::SortCpu,
        SortKey::Pid => Text::SortPid,
        SortKey::Name => Text::SortName,
    }
}

fn os_tool_text(p: Platform) -> Text {
    match p {
        Platform::MacOs => Text::OsToolMac,
        Platform::Windows => Text::OsToolWindows,
        Platform::Linux => Text::OsToolLinux,
    }
}

fn unavailable_text(u: MemUnavailable) -> Text {
    match u {
        MemUnavailable::PermissionDenied => Text::UnavailPermission,
        MemUnavailable::OsTooOld => Text::UnavailOsTooOld,
        MemUnavailable::Gone => Text::UnavailGone,
        MemUnavailable::Unsupported => Text::UnavailUnsupported,
    }
}

/// `s` padded on the left to `width` columns (CJK counts 2).
fn right(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(s.width())))
}

/// `s` as one line of at most `width` columns, cut with `…` when longer (and
/// without the padding of [`fit_width`]).
fn clip(s: &str, width: usize) -> String {
    fit_width(s, width).trim_end().to_string()
}

/// `lines` cut to `max` lines of at most `width` columns; the last one ends
/// with `…` when something was cut.
fn clamp(mut lines: Vec<String>, max: usize, width: usize) -> Vec<String> {
    if lines.len() > max {
        lines.truncate(max);
        if let Some(last) = lines.last_mut() {
            *last = clip(&format!("{last}…"), width);
        }
    }
    lines
}

/// `s` broken into lines of at most `width` columns, at a space or between
/// Chinese characters, never inside a word or between a number and its unit
/// (`40 MB`); a word longer than a line is cut where the line ends.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let col = |c: char| c.width().unwrap_or(0);
    let can_break = |b: usize| {
        let (prev, next) = (chars[b - 1], chars[b]);
        if prev == ' ' || next == ' ' {
            let word_end = if prev == ' ' {
                b.checked_sub(2).map(|i| chars[i])
            } else {
                Some(prev)
            };
            return !word_end.is_some_and(|c| c.is_ascii_digit());
        }
        (col(prev) > 1 || col(next) > 1) && prev != '（' && !"）」，。；：、？！".contains(next)
    };
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let (mut used, mut end) = (0, start);
        while end < chars.len() && (end == start || used + col(chars[end]) <= width) {
            used += col(chars[end]);
            end += 1;
        }
        if end < chars.len()
            && let Some(b) = (start + 1..=end).rev().find(|&b| can_break(b))
        {
            end = b;
        }
        lines.push(
            chars[start..end]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
        start = end;
        while chars.get(start) == Some(&' ') {
            start += 1;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::model::ProcKey;
    use crate::tree::NodeId;
    use crate::ui::mock::{all_ids, key, mock_fixture, mock_mcp_plan};
    use crate::ui::{Dialog, flatten};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use std::time::Instant;
    use unicode_width::UnicodeWidthStr;

    /// What the terminal shows: one quoted line per row, a wide character once
    /// (`TestBackend`'s own `Display` also lists every cell a wide character
    /// covers, which makes a Chinese screen unreadable).
    fn screen(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut out = String::new();
        for cells in buf.content.chunks(buf.area.width as usize) {
            let mut skip = 0;
            out.push('"');
            for c in cells {
                if skip == 0 {
                    out.push_str(c.symbol());
                }
                skip = skip.max(c.symbol().width()).saturating_sub(1);
            }
            out.push_str("\"\n");
        }
        out
    }

    fn proc_key(pid: u32) -> ProcKey {
        key(&mock_fixture().0, pid)
    }

    fn render(w: u16, h: u16, expand_all: bool, f: impl FnOnce(&mut UiState)) -> String {
        let (s, c, t) = mock_fixture();
        let mut st = UiState::new(Lang::ZhTw);
        if expand_all {
            all_ids(&t, &mut st.expanded);
        }
        f(&mut st);
        let rows = flatten(&t, &st, Instant::now());
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|fr| {
            draw(
                fr,
                &View {
                    st: &st,
                    rows: &rows,
                    snap: Some(&s),
                    cls: Some(&c),
                    color: true,
                    banner: None,
                    now: s.taken_at,
                },
            );
        })
        .unwrap();
        screen(&term)
    }

    #[test]
    fn render_main_100x30_zh() {
        // The state of the spec mockup: the first session and `chrome-live`
        // open, the cursor on the server of `chrome-live`.
        insta::assert_snapshot!(render(100, 30, false, |s| {
            let session = proc_key(49049);
            s.expanded.extend([
                NodeId::Proc(session),
                NodeId::McpGroup(session),
                NodeId::Mcp(proc_key(49815)),
            ]);
            s.selected = Some(NodeId::Proc(proc_key(50094)));
        }));
    }

    #[test]
    fn render_80x24_long_cjk_labels() {
        // The cursor is on the second session, below the first screen: the
        // table scrolls to it, and its long name is cut.
        insta::assert_snapshot!(render(80, 24, true, |s| s.selected =
            Some(NodeId::Proc(proc_key(52091)))));
    }

    #[test]
    fn render_too_small_60x18() {
        let out = render(60, 18, false, |_| {});
        assert!(out.contains("60×18") && out.contains("80×24"));
    }

    #[test]
    fn render_kill_dialog() {
        insta::assert_snapshot!(render(100, 30, true, |s| {
            s.selected = Some(NodeId::Mcp(proc_key(49815)));
            s.dialog = Some(Dialog::ConfirmKill(mock_mcp_plan()));
        }));
    }

    #[test]
    fn render_key_bar_at_80_keeps_navigation_and_quit() {
        let out = render(80, 24, false, |_| {});
        assert!(out.contains("↑↓ 移動  ←→ 收合/展開  x 結束這個  X 結束整棵"));
        assert!(out.contains("q 離開"));
    }

    #[test]
    fn render_help_zh_has_memory_glossary() {
        let out = render(100, 40, false, |s| s.help_open = true);
        for k in ["記憶體", "在 RAM 裡", "被擠出 RAM", "共用部分"] {
            assert!(out.contains(k), "{k}");
        }
    }

    #[test]
    fn render_help_at_80x24_scrolls_to_the_footer() {
        let top = render(80, 24, false, |s| s.help_open = true);
        assert!(top.contains("↓ 還有更多") && !top.contains("非官方工具"));
        let end = render(80, 24, false, |s| {
            s.help_open = true;
            s.help_scroll = u16::MAX;
        });
        assert!(end.contains("非官方工具") && !end.contains("↓ 還有更多"));
    }

    #[test]
    fn render_header_shows_paused() {
        assert!(render(100, 30, false, |s| s.paused = true).contains("已暫停"));
    }

    #[test]
    fn render_no_color_uses_default_colors() {
        let (s, c, t) = mock_fixture();
        let st = UiState::new(Lang::En);
        let rows = flatten(&t, &st, Instant::now());
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|fr| {
            draw(
                fr,
                &View {
                    st: &st,
                    rows: &rows,
                    snap: Some(&s),
                    cls: Some(&c),
                    color: false,
                    banner: None,
                    now: 0,
                },
            );
        })
        .unwrap();
        assert!(
            term.backend()
                .buffer()
                .content
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn render_wsl_banner() {
        let (s, c, t) = mock_fixture();
        let st = UiState::new(Lang::ZhTw);
        let rows = flatten(&t, &st, Instant::now());
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|fr| {
            draw(
                fr,
                &View {
                    st: &st,
                    rows: &rows,
                    snap: Some(&s),
                    cls: Some(&c),
                    color: true,
                    banner: Some(Msg::WslBanner),
                    now: 0,
                },
            );
        })
        .unwrap();
        assert!(screen(&term).contains("這裡只看得到 WSL 裡的 process"));
    }
}
