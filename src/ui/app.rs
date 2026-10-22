//! The interactive program: the terminal, the event loop, and what the keys
//! and the mouse do with the worker's frames (spec 4.6 to 4.8).
//!
//! The worker thread ([`crate::worker`]) collects; this thread only handles
//! input and draws, and draws only when a frame arrived, input came in, a
//! kill made progress or a message ran out.
//!
//! # Messages
//!
//! The message line shows one message, and while it is up the detail panel
//! shows all of it instead of the selected row. So a message stays
//! [`MESSAGE_FOR`] at most, and the next key that does something clears it.
//!
//! - A message caused by the user (a kill's result, why a kill was refused)
//!   replaces whatever is up.
//! - "The selected process ended" is shown only when no message is up: after
//!   ending the selected row it would push the kill's result away.
//! - A notice about the machine (a config file that cannot be read, processes
//!   that cannot be read, an OS too old for some memory figures) is shown once
//!   when it appears, and not over a message of the user's that is still up
//!   (it waits for that one to go). It is shown again only after it went away
//!   and came back. With several at once, only the first in that order shows.
//! - The WSL notices are a banner above the table instead, for as long as
//!   they hold: in WSL, and on Windows when no session is found but WSL runs
//!   (`vmmem` / `vmmemWSL`).

use std::io;
use std::mem::discriminant;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, KeyEventKind, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;

use crate::format::shorten_home;
use crate::i18n::{Lang, Msg, Text, tr};
use crate::kill::{KillError, KillRefusal, KillReport, KillScope, OsControl, execute, plan_kill};
use crate::model::{MemUnavailable, Platform};
use crate::tree::{Node, build_tree};
use crate::worker::{self, Frame, ToWorker, Worker};

use super::keys::{WHEEL_ROWS, map_key, map_mouse};
use super::{
    Action, Dialog, Layout, Row, UiState, View, apply, draw, flatten, merge_exited, note_seen,
    reselect,
};

/// How often input is looked for.
const POLL: Duration = Duration::from_millis(50);
/// How long a message stays up.
const MESSAGE_FOR: Duration = Duration::from_secs(10);
/// How long ending a process waits after asking, and how often it checks.
const KILL_GRACE: Duration = Duration::from_secs(3);
const KILL_POLL: Duration = Duration::from_millis(100);

/// Runs the interactive view until the user quits. The caller checks first
/// that stdout is a terminal.
pub fn run(lang: Lang) -> io::Result<()> {
    let color = std::env::var_os("NO_COLOR").is_none();
    install_panic_hook(lang);
    let result = enter().and_then(|term| event_loop(term, lang, color));
    leave();
    result
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn enter() -> io::Result<Term> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(io::stdout()))
}

/// Puts the terminal back as it was. Errors are ignored: there is nothing
/// better to do with them on the way out.
fn leave() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

/// On a panic, in any thread: restore the terminal, say how to report it,
/// print the panic, and exit (the UI must not go on drawing on a terminal
/// that is no longer set up for it).
fn install_panic_hook(lang: Lang) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        eprintln!("{}", tr(lang, Text::PanicReport));
        previous(info);
        std::process::exit(101);
    }));
}

fn event_loop(mut term: Term, lang: Lang, color: bool) -> io::Result<()> {
    let (frames_tx, frames) = mpsc::channel();
    let (to_worker, cmds) = mpsc::channel();
    let mut app = App::new(lang, color, to_worker);
    let home = std::env::home_dir().unwrap_or_default();
    let collecting = worker::spawn(Worker::new(home), app.st.interval_secs, frames_tx, cmds);
    let mut dirty = true;
    loop {
        if dirty {
            let mut layout = app.layout;
            term.draw(|f| layout = draw(f, &app.view()))?;
            app.drawn(layout);
            dirty = false;
        }
        if event::poll(POLL)? {
            let now = Instant::now();
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.key(k, now) {
                        break;
                    }
                    dirty = true;
                }
                Event::Mouse(m) => dirty |= app.mouse(m, now),
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        let now = Instant::now();
        loop {
            match frames.try_recv() {
                Ok(f) => {
                    app.frame(f, now);
                    dirty = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("the collection thread stopped"));
                }
            }
        }
        dirty |= app.kill_progress(now);
        dirty |= app.expire_message(now);
    }
    let _ = app.to_worker.send(ToWorker::Quit);
    let _ = collecting.join();
    Ok(())
}

/// What the ending thread reports.
enum KillEvent {
    Progress(usize, usize),
    Done(KillReport),
}

struct App {
    st: UiState,
    color: bool,
    /// The latest frame; `None` until the first one.
    data: Option<Frame>,
    /// The tree of the current view, with the processes that just ended.
    roots: Vec<Node>,
    rows: Vec<Row>,
    /// Where the last draw put things.
    layout: Layout,
    banner: Option<Msg>,
    /// When the message up now goes away.
    message_until: Option<Instant>,
    /// The machine notice that holds now (see the module docs).
    notice: Option<Msg>,
    to_worker: Sender<ToWorker>,
    killing: Option<Receiver<KillEvent>>,
}

impl App {
    fn new(lang: Lang, color: bool, to_worker: Sender<ToWorker>) -> App {
        App {
            st: UiState::new(lang),
            color,
            data: None,
            roots: Vec::new(),
            rows: Vec::new(),
            layout: Layout {
                table: Rect::default(),
                first_row: 0,
                help_scroll: 0,
            },
            banner: None,
            message_until: None,
            notice: None,
            to_worker,
            killing: None,
        }
    }

    fn view(&self) -> View<'_> {
        View {
            st: &self.st,
            rows: &self.rows,
            snap: self.data.as_ref().map(|f| &f.snap),
            cls: self.data.as_ref().map(|f| &f.cls),
            color: self.color,
            banner: self.banner.clone(),
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    /// Keeps what the renderer worked out: the scroll of the table and of the
    /// help page.
    fn drawn(&mut self, layout: Layout) {
        self.st.scroll = layout.first_row;
        self.st.help_scroll = u16::try_from(layout.help_scroll).unwrap_or(u16::MAX);
        self.layout = layout;
    }

    /// The rows again, after the tree or the state changed. `true` when the
    /// selected process is gone.
    fn reflow(&mut self, now: Instant) -> bool {
        self.rows = flatten(&self.roots, &self.st, now);
        reselect(&mut self.st, &self.rows, &self.roots)
    }

    fn frame(&mut self, f: Frame, now: Instant) {
        let next = build_tree(&f.snap, &f.cls, self.st.view);
        self.roots = merge_exited(&self.roots, next);
        note_seen(&mut self.st, &self.roots, now);
        if self.reflow(now) && !self.message_up(now) {
            self.say(Msg::SelectionGone, now);
        }
        self.banner = banner(&f);
        self.note(notice(&f), now);
        self.data = Some(f);
    }

    fn say(&mut self, m: Msg, now: Instant) {
        self.st.message = Some(m);
        self.message_until = Some(now + MESSAGE_FOR);
    }

    fn message_up(&self, now: Instant) -> bool {
        self.message_until.is_some_and(|t| now < t)
    }

    /// Shows the machine notice `n` when it is new (see the module docs).
    fn note(&mut self, n: Option<Msg>, now: Instant) {
        let kind = |m: &Option<Msg>| m.as_ref().map(discriminant);
        if kind(&n) == kind(&self.notice) {
            return;
        }
        match n {
            None => self.notice = None,
            Some(m) if !self.message_up(now) => {
                self.say(m.clone(), now);
                self.notice = Some(m);
            }
            Some(_) => {}
        }
    }

    /// `true` when a message went away.
    fn expire_message(&mut self, now: Instant) -> bool {
        if self.message_until.is_some_and(|t| now >= t) {
            self.st.message = None;
            self.message_until = None;
            return true;
        }
        false
    }

    /// Handles a key; `true` to quit.
    fn key(&mut self, k: KeyEvent, now: Instant) -> bool {
        let a = map_key(k, &self.st);
        self.act(a, now)
    }

    /// Handles the mouse; `true` when something changed.
    fn mouse(&mut self, m: MouseEvent, now: Instant) -> bool {
        let a = map_mouse(m, &self.st, &self.rows, &self.layout);
        if a == Action::None {
            return false;
        }
        let times = match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => WHEEL_ROWS,
            _ => 1,
        };
        for _ in 0..times {
            self.act(a.clone(), now);
        }
        true
    }

    /// Does `a`; `true` to quit.
    fn act(&mut self, a: Action, now: Instant) -> bool {
        if a == Action::None {
            return false;
        }
        self.st.message = None;
        self.message_until = None;
        let page = usize::from(self.layout.table.height);
        match a {
            Action::Quit => return true,
            Action::KillOne => self.plan(KillScope::One, now),
            Action::KillTree => self.plan(KillScope::Tree, now),
            Action::Confirm => self.confirm(),
            Action::Cancel if self.st.dialog.is_some() => self.st.dialog = None,
            Action::ToggleView => {
                apply(&mut self.st, &a, &self.rows, page);
                if let Some(f) = &self.data {
                    self.roots = build_tree(&f.snap, &f.cls, self.st.view);
                }
            }
            Action::TogglePause => {
                apply(&mut self.st, &a, &self.rows, page);
                let _ = self.to_worker.send(ToWorker::Pause(self.st.paused));
            }
            Action::Faster | Action::Slower => {
                apply(&mut self.st, &a, &self.rows, page);
                let _ = self
                    .to_worker
                    .send(ToWorker::SetInterval(self.st.interval_secs));
            }
            _ => apply(&mut self.st, &a, &self.rows, page),
        }
        self.reflow(now);
        false
    }

    fn plan(&mut self, scope: KillScope, now: Instant) {
        let (Some(f), Some(id)) = (&self.data, &self.st.selected) else {
            return;
        };
        match plan_kill(&f.snap, &f.cls, &self.roots, id, scope) {
            Ok(plan) => self.st.dialog = Some(Dialog::ConfirmKill(plan)),
            Err(r) => self.say(refusal_msg(r), now),
        }
    }

    /// Ends the processes of the confirmed plan on a thread of its own.
    fn confirm(&mut self) {
        let Some(Dialog::ConfirmKill(plan)) = self.st.dialog.take() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.st.dialog = Some(Dialog::Killing {
            done: 0,
            total: plan.targets.len(),
        });
        self.killing = Some(rx);
        thread::spawn(move || {
            let mut progress = |done, total| {
                let _ = tx.send(KillEvent::Progress(done, total));
            };
            let report = execute(&plan, &OsControl, KILL_GRACE, KILL_POLL, &mut progress);
            let _ = tx.send(KillEvent::Done(report));
        });
    }

    /// Takes in what the ending thread reported; `true` when something
    /// changed. When it is done, the result is shown and the worker asked
    /// for a fresh frame.
    fn kill_progress(&mut self, now: Instant) -> bool {
        let Some(rx) = self.killing.take() else {
            return false;
        };
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(KillEvent::Progress(done, total)) => {
                    self.st.dialog = Some(Dialog::Killing { done, total });
                    changed = true;
                }
                Ok(KillEvent::Done(report)) => {
                    self.st.dialog = None;
                    self.say(kill_msg(&report), now);
                    let _ = self.to_worker.send(ToWorker::Refresh);
                    return true;
                }
                Err(TryRecvError::Empty) => {
                    self.killing = Some(rx);
                    return changed;
                }
                Err(TryRecvError::Disconnected) => {
                    self.st.dialog = None;
                    return true;
                }
            }
        }
    }
}

fn refusal_msg(r: KillRefusal) -> Msg {
    match r {
        KillRefusal::GroupNeedsX => Msg::GroupNeedsX,
        KillRefusal::CategoryTooBroad => Msg::KillRefusedCategory,
        KillRefusal::SelfOrAncestor => Msg::KillRefusedSelf,
        KillRefusal::System { pid } => Msg::KillRefusedSystem { pid },
        KillRefusal::OtherUser { pid } => Msg::KillRefusedOtherUser { pid },
        KillRefusal::Zombie { pid, parent } => Msg::KillRefusedZombie { pid, parent },
        KillRefusal::NothingToKill => Msg::KillNothing,
    }
}

/// The result of a kill: the first failure, or the counts.
fn kill_msg(r: &KillReport) -> Msg {
    match r.failed.first() {
        Some((pid, e)) => Msg::KillFailed {
            pid: *pid,
            permission: *e == KillError::PermissionDenied,
        },
        None => Msg::KillDone {
            graceful: r.graceful,
            forced: r.forced,
            gone: r.already_gone,
            reused: r.skipped_reused.len(),
            failed: 0,
        },
    }
}

/// The machine notice for the message line, the first that holds.
fn notice(f: &Frame) -> Option<Msg> {
    let s = &f.snap;
    let too_old = s.procs.iter().any(|p| {
        p.mem
            .is_some_and(|m| m.unavailable == Some(MemUnavailable::OsTooOld))
    });
    if too_old {
        return Some(Msg::WindowsTooOld);
    }
    if let Some(p) = f.config_problems.first() {
        return Some(Msg::ConfigUnreadable {
            path: shorten_home(&p.path, s.home.as_deref()),
        });
    }
    (s.unreadable_count > 0).then_some(Msg::UnreadableProcs {
        n: s.unreadable_count,
    })
}

/// The WSL banner, while it holds.
fn banner(f: &Frame) -> Option<Msg> {
    let s = &f.snap;
    if s.in_wsl {
        return Some(Msg::WslBanner);
    }
    let wsl_running = s.procs.iter().any(|p| {
        let name = p.name.strip_suffix(".exe").unwrap_or(&p.name);
        name.eq_ignore_ascii_case("vmmem") || name.eq_ignore_ascii_case("vmmemWSL")
    });
    (s.platform == Platform::Windows && f.cls.sessions.is_empty() && wsl_running)
        .then_some(Msg::WslHint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Category;
    use crate::classify::classify;
    use crate::config::{ConfigProblem, McpConfigs};
    use crate::tree::NodeId;
    use crate::ui::mock::{key, mock_fixture};

    fn frame(problem: bool) -> Frame {
        let (snap, cls, _) = mock_fixture();
        let config_problems = if problem {
            vec![ConfigProblem {
                path: "/Users/u/.claude.json".into(),
                detail: "invalid JSON".into(),
            }]
        } else {
            Vec::new()
        };
        Frame {
            snap,
            cls,
            config_problems,
        }
    }

    /// The fixture without the process `pid`.
    fn without(pid: u32) -> Frame {
        let (mut snap, _, _) = mock_fixture();
        snap.procs.retain(|p| p.key.pid != pid);
        let cls = classify(&snap, &McpConfigs::default());
        Frame {
            snap,
            cls,
            config_problems: Vec::new(),
        }
    }

    #[test]
    fn user_messages_win_and_notices_show_once() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(Lang::En, false, tx);
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        app.frame(frame(false), t0);
        app.st.selected = Some(NodeId::Category(Category::ClaudeCode));
        app.act(Action::KillTree, at(1));
        assert_eq!(app.st.message, Some(Msg::KillRefusedCategory));

        // The config problem waits for the user's message to run out.
        app.frame(frame(true), at(2));
        assert_eq!(app.st.message, Some(Msg::KillRefusedCategory));
        assert!(app.expire_message(at(11)));
        assert_eq!(app.st.message, None);
        app.frame(frame(true), at(12));
        let config = Some(Msg::ConfigUnreadable {
            path: "~/.claude.json".into(),
        });
        assert_eq!(app.st.message, config);

        // A key that does something clears it, and it does not come back
        // while the problem lasts; it does once it went away and returned.
        app.act(Action::Down, at(13));
        assert_eq!(app.st.message, None);
        app.frame(frame(true), at(14));
        assert_eq!(app.st.message, None);
        app.frame(frame(false), at(16));
        app.frame(frame(true), at(18));
        assert_eq!(app.st.message, config);

        // Ending the selected process: its row goes, the result stays.
        let session = NodeId::Proc(key(&app.data.as_ref().unwrap().snap, 52091));
        app.act(Action::Down, at(19));
        app.st.selected = Some(session.clone());
        app.say(Msg::KillNothing, at(20));
        app.frame(without(52091), at(21));
        app.frame(without(52091), at(22));
        assert_ne!(app.st.selected, Some(session));
        assert_eq!(app.st.message, Some(Msg::KillNothing));
    }
}
