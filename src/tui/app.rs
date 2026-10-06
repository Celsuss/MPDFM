//! The state machine and the loop that drives it.
//!
//! # The loop
//!
//! ```text
//! draw if dirty  →  block on recv()  →  update()  →  drain try_recv()  →  repeat
//! ```
//!
//! Two properties of that shape are acceptance criteria rather than style:
//!
//! - **It draws only when something changed.** [`App::update`] returns whether the
//!   screen would look different, and a message that changes nothing visible — a
//!   tick with no message to retire, an MPD poll that said the same thing as last
//!   time — costs no frame. Nothing here polls, so an idle `mpdfm` is three
//!   blocked threads and one wakeup a second.
//! - **It drains before it draws.** A held-down `j`, a resize the terminal reports
//!   as three events, a burst of scan progress: all of them are folded into one
//!   frame, so the UI cannot fall behind its own input queue.
//!
//! # The view stack
//!
//! `views` is never empty. `views[0]` is the base view and everything above it is
//! an overlay. Opening one pushes, `esc` pops, and the state underneath is
//! untouched because it was never anywhere else — the browser's cursor lives in
//! the browser's own state, not in a "current screen" the overlay replaced. That is
//! the whole trick, and the reason the task asks for a stack rather than a
//! `current_view` field.
//!
//! # What is deliberately thin here
//!
//! The browser is a list with a cursor, and the status bar is one line of text.
//! Tasks 22–26 own what they become; this task owns the shell they live in, so
//! each of them is built just far enough to prove the shell works — a cursor to
//! show that an overlay does not lose it, a status bar to show that the tick
//! reaches it.
//!
//! # Keys, and what answers them
//!
//! Nothing here matches on a `KeyCode`. A keypress goes through
//! [`super::keys::Keys`] — normalize, resolve the mode's table, maybe wait for
//! the second half of a sequence — and comes out as an [`Action`], which
//! [`App::dispatch`] answers. Three consequences:
//!
//! - the bindings are data, so `keys.toml` can change them (task 21);
//! - `:q` and `q` are the same code path, because command mode produces the same
//!   `Action`;
//! - `dispatch`'s `match` is exhaustive, so an action cannot be added to the
//!   vocabulary without this file deciding what it does — and the verbs tasks 22–25
//!   own answer with the task that owns them rather than with silence.
//!
//! Two keys are deliberately *not* in the keymap: `ctrl-c`, and the `y`/`n` of a
//! confirmation prompt. Both are escape hatches, and an escape hatch a user can
//! remap away is not one.
//!
//! # No `println!`
//!
//! Nothing in this module writes to stdout or stderr. Diagnostics go to
//! [`Log`], which is a file or nothing. See `log.rs` for why.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library, ScanProgress};
use mpdfm_core::ops::Plan;
use mpdfm_core::playlist::PlaylistIndex;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::action::Action;
use super::command::{self, Command, CommandLine};
use super::event::Events;
use super::keys::{KeyChord, KeyMap, KeyWarning, Keys, Mode, Resolution};
use super::log::Log;
use super::msg::{MpdSnapshot, Msg, ScanOutcome, TaskOutcome};
use super::terminal::{MIN_SIZE, fits};
use super::{PANIC_AT, work};

/// How long an informational toast stays up once it is the one on screen.
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// How many messages may be waiting for the bottom line.
///
/// Generous: the point of the cap is that nothing grows without bound, not that
/// anything is ever expected to reach it. Task 26's `:messages` is what makes a
/// backlog readable rather than only survivable.
const TOAST_QUEUE: usize = 16;

/// Which pane has the keyboard.
///
/// The browser is two panes (task 22): the directory tree and the files in the
/// selected directory. `tab` moves between them. It is here and not in the browser
/// view because the status bar reads it, and because task 23's tag editor is a
/// third thing `tab` will have to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The directory tree.
    Tree,
    /// The files in the selected directory.
    Files,
}

impl Focus {
    /// The other one.
    fn toggled(self) -> Self {
        match self {
            Self::Tree => Self::Files,
            Self::Files => Self::Tree,
        }
    }

    /// The word the status bar shows.
    fn label(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Files => "files",
        }
    }
}

/// One entry on the view stack.
///
/// The base is always [`View::Browser`]. The overlays carry their own state, so
/// popping one throws away exactly that state and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// The library browser. Task 22.
    Browser,
    /// The key help, generated from the live keymap for the mode it was opened
    /// from. Task 26 adds the other modes' sections and the grouping.
    Help {
        /// The mode whose bindings are listed.
        mode: Mode,
        /// How far down the list has been scrolled. There are more bindings than
        /// rows on an 80×24 terminal, so this is not optional.
        scroll: u16,
    },
    /// The `:` line. Drawn on the bottom line rather than over the body, which is
    /// where a command line belongs and why it is on the stack anyway: it is the
    /// thing `esc` closes, and it decides the mode.
    Command(CommandLine),
    /// A question the app will not go past. The keys that answer it are not in the
    /// keymap; see [`App::on_confirm_key`].
    Confirm(Confirm),
    /// Something the user should read once, which is not an error — what was wrong
    /// with `keys.toml`, for instance.
    Notice {
        /// The panel's title, including its surrounding spaces.
        title: String,
        /// The whole text. Wrapped, never truncated.
        body: String,
    },
    /// Something went wrong, and it is not going away on a timer. Task 26 turns
    /// this into the full panel with the path and the suggested next step.
    Error(String),
}

impl View {
    /// Whether this view is drawn over the one below it rather than instead of it.
    fn is_overlay(&self) -> bool {
        !matches!(self, Self::Browser)
    }

    /// Whether this view has the keyboard to itself.
    ///
    /// A panel is something to read and dismiss, so letting `j` move a cursor
    /// behind it would be a surprise. The command line is not modal in this sense —
    /// it handles the editing actions and passes the rest on, so `ctrl-r` bound
    /// under `[command]` still rescans.
    fn is_modal(&self) -> bool {
        matches!(
            self,
            Self::Help { .. } | Self::Confirm(_) | Self::Notice { .. } | Self::Error(_)
        )
    }

    /// The name the status bar shows and the log records.
    fn name(&self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Help { .. } => "help",
            Self::Command(_) => "command",
            Self::Confirm(_) => "confirm",
            Self::Notice { .. } => "notice",
            Self::Error(_) => "error",
        }
    }
}

/// A yes/no question, and what a yes means.
///
/// The only one so far is `q` with staged operations, which is an acceptance
/// criterion: losing a plan to a keystroke is exactly the kind of quiet damage this
/// program exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    /// What the user is being asked.
    question: String,
    /// The action a `y` dispatches.
    on_yes: Action,
}

/// How serious a message is, and therefore whether it expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something worked.
    Info,
    /// Something is worth a look.
    Warn,
}

/// A transient message on the bottom line.
///
/// Errors are *not* toasts: an error that scrolls away unread is an error that
/// was swallowed, which the task forbids. They become [`View::Error`] instead.
#[derive(Debug, Clone)]
pub struct Toast {
    text: String,
    level: Level,
    /// How long it gets once it is the one on screen.
    lifetime: Duration,
    /// When it will have had its turn. `None` until it reaches the front of the
    /// queue, because a message that waited behind two others has not been read
    /// yet and its clock should not have been running.
    expires: Option<Instant>,
}

/// What the status line says about a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScanState {
    /// No scan has been asked for.
    Idle,
    /// A worker is walking the library. The last progress it reported, if any has
    /// arrived yet.
    Running(Option<ScanProgress>),
    /// A scan finished and the counts are the library's.
    Done { files: usize, dirs: usize },
}

/// Everything the TUI knows.
pub struct App {
    /// The resolved configuration. Read by the workers, never by a widget for a
    /// path it should be taking from [`Library`].
    config: Config,
    /// The library, once a scan has produced one. `None` before the first scan
    /// lands, which is the state the very first frame is drawn in.
    library: Option<Library>,
    /// The playlist index, loaded with the library.
    index: Option<PlaylistIndex>,
    /// The staged operations. Empty until task 24 can add to it; held here from
    /// the start because the status bar counts it and because `q` will have to ask
    /// about it.
    plan: Plan,
    /// Which pane the keyboard is in.
    focus: Focus,
    /// The bindings, and the half-finished sequence waiting for its second key.
    keys: Keys,
    /// The view stack. Never empty; `views[0]` is the base.
    views: Vec<View>,
    /// Messages waiting for the bottom line, oldest first.
    ///
    /// A queue and not a slot: two things worth saying in the same second — a
    /// scan's counts and a warning about what it found — would otherwise mean the
    /// second silently overwriting the first, and a message nobody saw is the same
    /// as one that was never posted. Each gets its own turn and its own clock.
    toasts: VecDeque<Toast>,
    /// What the status line says about scanning.
    scan: ScanState,
    /// The browser's cursor, as an index into the current directory's rows.
    cursor: usize,
    /// What MPD last said. `None` until the first poll answers.
    mpd: Option<MpdSnapshot>,
    /// Whether an MPD poll is already out, so the tick does not stack them up.
    mpd_in_flight: bool,
    /// Where workers send their answers.
    tx: Sender<Msg>,
    /// `--log`, or nothing.
    log: Arc<Log>,
    /// Set by `q` and by [`Msg::Shutdown`]; the loop checks it.
    quit: bool,
    /// The last size the terminal reported, for the log and for the too-small
    /// message.
    size: (u16, u16),
}

impl App {
    /// A new app, with nothing scanned yet.
    pub fn new(config: Config, keys: KeyMap, tx: Sender<Msg>, log: Arc<Log>) -> Self {
        Self {
            config,
            library: None,
            index: None,
            plan: Plan::new(),
            focus: Focus::Tree,
            keys: Keys::new(keys),
            views: vec![View::Browser],
            toasts: VecDeque::new(),
            scan: ScanState::Idle,
            cursor: 0,
            mpd: None,
            mpd_in_flight: false,
            tx,
            log,
            quit: false,
            size: (0, 0),
        }
    }

    /// Draw, wait, update, repeat, until something says stop.
    ///
    /// The first scan is started here rather than in [`App::new`], so that the
    /// first frame — "scanning…" — is on screen before the walk begins. Starting it
    /// first and drawing afterwards would show a blank terminal for the length of a
    /// cold scan, which is the frozen screen this whole design is avoiding.
    ///
    /// # Errors
    ///
    /// Only if the terminal cannot be drawn on. Everything else is a message on
    /// screen: a failed scan, an unreachable MPD and a worker that will not start
    /// are all states of the app, not ends of it.
    pub fn run<B>(&mut self, terminal: &mut Terminal<B>, events: &Events) -> Result<()>
    where
        B: Backend,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        // Before the first frame, so that a half-page jump has a page to measure
        // itself against even if no resize ever arrives.
        if let Ok(size) = terminal.size() {
            self.size = (size.width, size.height);
        }
        self.rescan();

        let mut dirty = true;
        while !self.quit {
            if dirty {
                terminal.draw(|frame| self.render(frame.area(), frame))?;
                dirty = false;
            }

            let Some(msg) = events.recv() else {
                // Every sender is gone. In production that cannot happen while the
                // tick thread lives; in a test it is the end of the script, and
                // leaving is the only honest thing to do with an app that was not
                // told to quit.
                break;
            };
            dirty |= self.update(msg);

            // Fold everything else that is already waiting into the same frame.
            while !self.quit {
                let Some(msg) = events.try_recv() else { break };
                dirty |= self.update(msg);
            }
        }
        self.log.line("loop: leaving");
        Ok(())
    }

    /// Handle one message. Returns whether the screen would now look different.
    pub fn update(&mut self, msg: Msg) -> bool {
        if self.log.is_on() {
            self.log.line(format!("msg: {}", msg.name()));
        }
        if PANIC_AT.is("event") {
            panic!(
                "{}=event: panicking while handling a {} on purpose",
                PANIC_AT.name(),
                msg.name()
            );
        }
        match msg {
            Msg::Input(event) => self.on_event(event),
            Msg::Tick => self.on_tick(),
            Msg::Progress(progress) => {
                self.scan = ScanState::Running(Some(progress));
                true
            }
            Msg::ScanDone(outcome) => self.on_scan_done(*outcome),
            Msg::MpdStatus(snapshot) => self.on_mpd(*snapshot),
            Msg::TaskDone(outcome) => self.on_task_done(*outcome),
            Msg::Shutdown => {
                self.log.line("shutdown: asked to stop");
                self.quit = true;
                false
            }
        }
    }

    // -- messages ----------------------------------------------------------

    /// A terminal event.
    fn on_event(&mut self, event: Event) -> bool {
        match event {
            // A key *press*. Releases and repeats are reported by terminals that
            // support the Kitty protocol, and acting on both halves of a keypress
            // would make every binding fire twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Resize(width, height) => {
                let was = self.size;
                self.size = (width, height);
                self.log.line(format!(
                    "resize: {width}x{height} (was {}x{})",
                    was.0, was.1
                ));
                true
            }
            // Focus changes, mouse reports from a terminal that sends them
            // unasked, pastes: nothing here wants them, and redrawing for them
            // would be a frame for nothing.
            _ => false,
        }
    }

    /// A keypress, resolved through the keymap.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        // `ctrl-c` is not in the keymap and cannot be remapped away. In raw mode the
        // terminal does not turn it into a signal, so it arrives as a key, and a
        // user who wants out reaches for it before they read the help. It means the
        // same thing `q` does — so it asks about staged operations — and a second
        // one, from the prompt it raises, leaves regardless. Two presses always
        // get out, and neither of them throws a plan away silently.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            let action = if matches!(self.views.last(), Some(View::Confirm(_))) {
                Action::ForceQuit
            } else {
                Action::Quit
            };
            return self.dispatch(action);
        }

        // A prompt reads its own keys, for the same reason: a question nobody can
        // answer is worse than one that was never asked.
        if let Some(View::Confirm(confirm)) = self.views.last() {
            let confirm = confirm.clone();
            return self.on_confirm_key(&confirm, key);
        }

        let mode = self.mode();
        match self.keys.press(mode, key, Instant::now()) {
            Resolution::Act(action) => self.dispatch(action),
            // Worth a frame: the bottom line shows the half-finished sequence, so
            // a `g` that is waiting for something looks like it is waiting.
            Resolution::Partial => true,
            Resolution::Nothing => self.on_unbound(key),
        }
    }

    /// Which set of bindings is in force.
    ///
    /// Tasks 23–25 add the views that reach the other three modes. Until then an
    /// overlay is something to dismiss rather than a mode, which is why a panel
    /// resolves as [`Mode::Browser`] and then has its keys filtered in
    /// [`App::dispatch`].
    fn mode(&self) -> Mode {
        match self.views.last() {
            Some(View::Command(_)) => Mode::Command,
            _ => Mode::Browser,
        }
    }

    /// `y` or `n`, and nothing else.
    fn on_confirm_key(&mut self, confirm: &Confirm, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                self.views.pop();
                self.dispatch(confirm.on_yes);
                true
            }
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => {
                self.log.line("confirm: declined");
                self.views.pop();
                true
            }
            // Anything else leaves the question on screen, which is what makes it
            // a question rather than a notification.
            _ => false,
        }
    }

    /// A key nothing is bound to. Only a view that takes text wants one.
    fn on_unbound(&mut self, key: KeyEvent) -> bool {
        let Some(c) = KeyChord::from_event(key).typed() else {
            return false;
        };
        match self.views.last_mut() {
            Some(View::Command(line)) => line.insert(c),
            _ => false,
        }
    }

    // -- actions -----------------------------------------------------------

    /// Do what the user asked for, however they asked for it.
    ///
    /// Returns whether the screen would now look different.
    ///
    /// The `match` at the bottom is exhaustive on purpose: adding an action to the
    /// vocabulary does not compile until this decides what it does. The verbs tasks
    /// 22–25 own say which task owns them, because a binding that silently did
    /// nothing would be indistinguishable from one that is broken.
    pub fn dispatch(&mut self, action: Action) -> bool {
        self.log.line(format!("action: {action}"));

        // In command mode the editing verbs are about the line rather than the
        // library. Anything the line does not claim falls through, so a `ctrl-r`
        // bound under `[command]` still rescans.
        if let Some(dirty) = self.command_action(action) {
            return dirty;
        }

        // A panel has the keyboard: only the things that get rid of it work — and,
        // for the help, the ones that move around inside it.
        if self.views.last().is_some_and(View::is_modal) {
            if let Some(dirty) = self.help_action(action) {
                return dirty;
            }
            return match action {
                Action::Cancel => self.pop(),
                Action::Help => self.toggle_help(),
                Action::Quit | Action::ForceQuit => self.quit_action(action),
                _ => false,
            };
        }

        match action {
            // -- the shell answers these ------------------------------------
            Action::Down => self.move_cursor(1),
            Action::Up => self.move_cursor(-1),
            Action::Top => self.set_cursor(0),
            Action::Bottom => self.set_cursor(self.row_count().saturating_sub(1)),
            Action::HalfPageDown => self.move_cursor(self.page_step()),
            Action::HalfPageUp => self.move_cursor(-self.page_step()),
            Action::SwitchPane => {
                self.focus = self.focus.toggled();
                true
            }
            Action::Cancel => self.pop(),
            Action::CommandMode => self.push(View::Command(CommandLine::new())),
            Action::Help => self.toggle_help(),
            Action::Rescan => {
                self.rescan();
                true
            }
            Action::Quit | Action::ForceQuit => self.quit_action(action),

            // -- the views that are not built yet ---------------------------
            Action::Left
            | Action::Right
            | Action::Open
            | Action::Parent
            | Action::ToggleMark
            | Action::VisualSelect
            | Action::MarkAll
            | Action::UnmarkAll
            | Action::StageMove
            | Action::Rename
            | Action::StageDelete => self.not_yet(action.help(), Some("22-browser-view.md")),
            Action::EditTags => self.not_yet(action.help(), Some("23-tagedit-view.md")),
            Action::ShowPending
            | Action::Unstage
            | Action::Commit
            | Action::DiscardPending
            | Action::Undo => self.not_yet(action.help(), Some("24-pending-view.md")),
            Action::Search | Action::SearchNext | Action::SearchPrev | Action::Filter => {
                self.not_yet(action.help(), Some("25-search-and-filter.md"))
            }
            Action::Organize => self.not_yet(action.help(), Some("28-organize-command.md")),

            // -- only meaningful where there is a line of text --------------
            Action::Submit | Action::DeleteChar | Action::ClearLine => false,
        }
    }

    /// The command line's share of the actions, or `None` if it wants none of them.
    fn command_action(&mut self, action: Action) -> Option<bool> {
        if !matches!(self.views.last(), Some(View::Command(_))) {
            return None;
        }
        // These two change the stack, so they are handled before anything borrows
        // the line out of it.
        match action {
            Action::Cancel => {
                self.pop();
                return Some(true);
            }
            Action::Submit => return Some(self.submit_command()),
            _ => {}
        }

        let Some(View::Command(line)) = self.views.last_mut() else {
            return None;
        };
        Some(match action {
            Action::Left => line.left(),
            Action::Right => line.right(),
            Action::DeleteChar => line.backspace(),
            Action::ClearLine => line.clear(),
            _ => return None,
        })
    }

    /// Run what was typed at `:`, or say why it cannot be run.
    fn submit_command(&mut self) -> bool {
        let Some(View::Command(line)) = self.views.last() else {
            return false;
        };
        match line.parse() {
            Ok(command) => {
                self.log.line(format!("command: :{}", line.text()));
                self.views.pop();
                self.keys.clear();
                self.run_command(command)
            }
            // `:` and then `enter` is a change of mind, not a mistake.
            Err(command::CommandError::Empty) => self.pop(),
            Err(err) => {
                // The line stays open with the reason under it, so the user edits
                // what they typed instead of typing it again.
                if let Some(View::Command(line)) = self.views.last_mut() {
                    line.fail(err);
                }
                true
            }
        }
    }

    /// Carry out a parsed command.
    ///
    /// Only the two that this task owns do anything; see
    /// [`super::command`] for why the rest parse now and act later.
    pub fn run_command(&mut self, command: Command) -> bool {
        match command {
            Command::Quit { force } => self.quit_action(if force {
                Action::ForceQuit
            } else {
                Action::Quit
            }),
            Command::Move { dst } => {
                self.not_yet(format!("move to {dst}"), Some("22-browser-view.md"))
            }
            Command::Organize { template } => self.not_yet(
                format!("organize by {template}"),
                Some("28-organize-command.md"),
            ),
            Command::Undo { txid } => {
                let what = txid.map_or_else(
                    || "undo the last transaction".to_owned(),
                    |txid| format!("undo {txid}"),
                );
                self.not_yet(what, Some("24-pending-view.md"))
            }
            Command::Doctor => self.not_yet("doctor", Some("29-doctor.md")),
            // No task owns live settings, and inventing one here would be a
            // promise this plan has not made. What `:set` has is a settled grammar
            // and a test; what it does not have is anywhere to put the value.
            Command::Set { key, value } => self.not_yet(
                format!("set {key}={value}: nothing applies a setting"),
                None,
            ),
        }
    }

    /// Leave, asking first when there is something staged to lose.
    fn quit_action(&mut self, action: Action) -> bool {
        if action == Action::Quit && !self.plan.is_empty() {
            let count = self.plan.len();
            let plural = if count == 1 { "" } else { "s" };
            self.log.line("quit: asking about the pending plan");
            return self.push(View::Confirm(Confirm {
                question: format!(
                    "{count} staged operation{plural} would be lost.\nQuit without committing?"
                ),
                on_yes: Action::ForceQuit,
            }));
        }
        self.quit = true;
        false
    }

    /// Open the help on the mode that is in force, or close it if it is open.
    fn toggle_help(&mut self) -> bool {
        if matches!(self.views.last(), Some(View::Help { .. })) {
            return self.pop();
        }
        let mode = self.mode();
        self.push(View::Help { mode, scroll: 0 })
    }

    /// Say that something is understood but not built yet, and which task builds it.
    ///
    /// A warning rather than information: the key worked, and nothing happened, and
    /// the user should know which of those two is the surprise.
    fn not_yet(&mut self, what: impl std::fmt::Display, task: Option<&str>) -> bool {
        let text = match task {
            Some(task) => format!("{what} — not yet; docs/tasks/{task} owns it"),
            None => format!("{what} — not yet"),
        };
        self.notify(Level::Warn, text);
        true
    }

    /// How far a half-page jump goes: half the rows the listing has.
    fn page_step(&self) -> isize {
        // The chrome is four rows (header, status, message) plus the listing's own
        // border, and a half page of nothing is still one row.
        let rows = self.size.1.saturating_sub(5).max(2);
        isize::try_from(rows / 2).unwrap_or(1)
    }

    /// The slow tick: retire the message that has had its turn, and ask MPD what
    /// it is doing.
    ///
    /// Returns true only when something on screen actually changed, which is what
    /// keeps an idle `mpdfm` off the CPU. The poll it starts is a thread; the answer
    /// arrives later as [`Msg::MpdStatus`] and redraws then if it differs.
    fn on_tick(&mut self) -> bool {
        let retired = self.retire_toast();
        // A `g` nobody finished stops being pending, and stops saying so on the
        // bottom line. `Keys::press` enforces the same deadline, and has to: the
        // next keypress may well arrive before the next tick.
        let expired = self.keys.expire(Instant::now());

        if !self.mpd_in_flight {
            self.mpd_in_flight = true;
            work::poll_mpd(self.tx.clone(), self.config.clone(), Arc::clone(&self.log));
        }

        retired || expired
    }

    /// A scan came back.
    fn on_scan_done(&mut self, outcome: ScanOutcome) -> bool {
        let ScanOutcome {
            library,
            index,
            playlist_warnings,
            elapsed,
        } = outcome;

        match library {
            Ok(library) => {
                let warnings = library.warnings().len() + playlist_warnings.len();
                self.scan = ScanState::Done {
                    files: library.len(),
                    dirs: library.dir_count(),
                };
                let summary = format!(
                    "scanned {} files in {} directories in {} ms",
                    library.len(),
                    library.dir_count(),
                    elapsed.as_millis()
                );
                self.library = Some(library);
                self.index = index;
                // The cursor may have been past the end of a smaller library.
                self.clamp_cursor();

                if warnings == 0 {
                    self.notify(Level::Info, summary);
                } else {
                    self.notify(Level::Warn, format!("{summary} · {warnings} warning(s)"));
                }
            }
            Err(message) => {
                self.scan = ScanState::Idle;
                // Not a toast. A library that did not scan is the whole of what
                // the user came for, and a message that vanishes after four
                // seconds is a message that was swallowed.
                self.fail(message);
            }
        }
        true
    }

    /// MPD answered. Redraws only when the answer is different from the last one,
    /// so a daemon sitting still costs one frame and not one a second.
    ///
    /// "Different" means what the status bar would show, not what the daemon
    /// said: an elapsed-time field that ticks on every poll is not a reason to
    /// redraw, and a song that changed is.
    fn on_mpd(&mut self, snapshot: MpdSnapshot) -> bool {
        self.mpd_in_flight = false;
        let was = self.mpd.as_ref().map(mpd_summary);
        let now = mpd_summary(&snapshot);
        let changed = was.as_deref() != Some(now.as_str());

        // The reason there is no state is worth a line in the log and nothing on
        // screen: an MPD that is not running is a normal state of the world, and
        // the indicator already says so in one character.
        if changed && self.log.is_on() {
            match (&snapshot.state, &snapshot.problem) {
                (None, Some(problem)) => self.log.line(format!("mpd: {now} ({problem})")),
                _ => self.log.line(format!("mpd: {now}")),
            }
        }

        self.mpd = Some(snapshot);
        changed
    }

    /// A worker that was not a scan came back.
    fn on_task_done(&mut self, outcome: TaskOutcome) -> bool {
        match outcome {
            TaskOutcome::Failed { what, message } => {
                self.fail(format!("{what}: {message}"));
                true
            }
        }
    }

    // -- state -------------------------------------------------------------

    /// Start a scan on a worker. Does nothing if one is already running — a held
    /// `R` would otherwise start a walk per keypress.
    fn rescan(&mut self) {
        if matches!(self.scan, ScanState::Running(_)) {
            return;
        }
        self.scan = ScanState::Running(None);
        work::scan(
            self.tx.clone(),
            self.config.music_dir.clone(),
            self.config.playlist_dir.clone(),
            Arc::clone(&self.log),
        );
    }

    /// Push an overlay. Returns true, since the screen now has it on it.
    fn push(&mut self, view: View) -> bool {
        self.log.line(format!("view: push {}", view.name()));
        self.views.push(view);
        true
    }

    /// Pop the top overlay, if there is one. The base view never pops: `esc` on
    /// the browser is not a way out of the program.
    fn pop(&mut self) -> bool {
        if self.views.len() <= 1 {
            // Still worth something: `esc` also dismisses the message on the
            // bottom line, which is the closest thing to "clear it" the shell has.
            // One press, one message, so a queue is read rather than skipped.
            let dismissed = self.toasts.pop_front().is_some();
            self.start_toast_clock();
            return dismissed;
        }
        let popped = self.views.pop();
        if let Some(view) = &popped {
            self.log.line(format!("view: pop {}", view.name()));
        }
        true
    }

    /// Queue a transient message for the bottom line.
    fn notify(&mut self, level: Level, text: impl Into<String>) {
        self.notify_for(level, text, TOAST_LIFETIME);
    }

    /// [`App::notify`] with an explicit lifetime, so a test can expire one without
    /// waiting four seconds.
    fn notify_for(&mut self, level: Level, text: impl Into<String>, lifetime: Duration) {
        let text = text.into();
        self.log.line(format!("toast: {text}"));
        if self.toasts.len() >= TOAST_QUEUE {
            // A flood is a bug somewhere, and dropping the oldest keeps the most
            // recent news — which is the half a user wants — without growing a
            // queue forever.
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast {
            text,
            level,
            lifetime,
            expires: None,
        });
        self.start_toast_clock();
    }

    /// Give whatever is at the front of the queue its clock, if it has not got one.
    ///
    /// Called when a message is queued and when one is retired, so the lifetime a
    /// toast gets is time spent *on screen* and not time spent waiting behind
    /// another one.
    fn start_toast_clock(&mut self) {
        if let Some(front) = self.toasts.front_mut()
            && front.expires.is_none()
        {
            front.expires = Some(Instant::now() + front.lifetime);
        }
    }

    /// Drop the front message if it has had its turn. Returns whether the bottom
    /// line now says something different.
    fn retire_toast(&mut self) -> bool {
        let done = self
            .toasts
            .front()
            .and_then(|front| front.expires)
            .is_some_and(|expires| Instant::now() >= expires);
        if !done {
            return false;
        }
        self.toasts.pop_front();
        self.start_toast_clock();
        true
    }

    /// Report something that must be read rather than noticed.
    ///
    /// Opens [`View::Error`] on the stack: it has to be dismissed, it shows the
    /// whole message, and whatever was underneath is still there afterwards. Task
    /// 26 gives it the path and the suggested next step.
    fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.log.line(format!("error: {message}"));
        // One panel at a time: a second failure replaces the top one rather than
        // burying it, so `esc` means "I have read it" and not "one of several".
        if matches!(self.views.last(), Some(View::Error(_))) {
            self.views.pop();
        }
        self.views.push(View::Error(message));
    }

    /// The directory the browser is showing. The root until task 22 can navigate.
    fn current_dir(&self) -> DirPath {
        DirPath::root()
    }

    /// The current directory as something to put on screen.
    ///
    /// [`DirPath::root`] displays as the empty string, which is right for joining
    /// a path and wrong for a title or a status bar: it leaves a pane called `  `
    /// and a bar that starts with a stray separator.
    fn dir_label(&self) -> String {
        let dir = self.current_dir();
        if dir.is_root() {
            "/".to_owned()
        } else {
            dir.to_string()
        }
    }

    /// How many rows the browser has — subdirectories, then files.
    fn row_count(&self) -> usize {
        self.rows().len()
    }

    /// The rows of the current directory: subdirectories first, then files.
    ///
    /// Built per draw from the model's own index, which is what the task's second
    /// pitfall asks for: the widget gets a small owned `Vec<String>` and the
    /// library is not borrowed across the draw by anything that could be mutated.
    fn rows(&self) -> Vec<String> {
        let Some(library) = &self.library else {
            return Vec::new();
        };
        let dir = self.current_dir();
        let Some(entry) = library.dir(&dir) else {
            return Vec::new();
        };

        let mut rows: Vec<String> = entry
            .subdirs()
            .iter()
            .filter_map(|sub| sub.file_name().map(|name| format!("{name}/")))
            .collect();
        rows.extend(
            entry
                .files()
                .iter()
                .filter_map(|index| library.entry(*index))
                .map(|entry| entry.file_name().to_owned()),
        );
        rows
    }

    /// Move the cursor by `delta`, stopping at either end rather than wrapping.
    fn move_cursor(&mut self, delta: isize) -> bool {
        let count = self.row_count();
        if count == 0 {
            return false;
        }
        let last = count - 1;
        let target = self.cursor.saturating_add_signed(delta).min(last);
        self.set_cursor(target)
    }

    /// Put the cursor on `row`. Returns whether it moved.
    fn set_cursor(&mut self, row: usize) -> bool {
        let moved = self.cursor != row;
        self.cursor = row;
        moved
    }

    /// Bring the cursor back inside the library after a rescan shrank it.
    fn clamp_cursor(&mut self) {
        let count = self.row_count();
        self.cursor = self.cursor.min(count.saturating_sub(1));
    }

    // -- drawing -----------------------------------------------------------

    /// Draw the whole frame into `area`.
    ///
    /// Everything a widget needs is derived here, from the model, into owned
    /// values. No lock is held and no borrow of the library outlives this call,
    /// which is the task's second pitfall.
    fn render(&self, area: Rect, frame: &mut ratatui::Frame) {
        if PANIC_AT.is("draw") {
            panic!(
                "{}=draw: panicking inside the draw callback on purpose",
                PANIC_AT.name()
            );
        }

        if !fits(area.width, area.height) {
            frame.render_widget(too_small(area), area);
            return;
        }

        // A command that would not parse gets a second row, so the reason and the
        // text it is about are both readable. Nothing else ever needs one.
        let message_rows = if self
            .command_line()
            .is_some_and(|line| line.error().is_some())
        {
            2
        } else {
            1
        };
        let [header, body, status, message] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(message_rows),
        ])
        .areas(area);

        frame.render_widget(self.header(), header);
        // The base view, always — an overlay is drawn over it, not instead of it,
        // which is what makes "popping does not lose state" visible as well as
        // true.
        self.render_browser(body, frame);
        frame.render_widget(self.status_bar(), status);
        self.render_message(message, frame);

        for view in self.views.iter().filter(|view| view.is_overlay()) {
            self.render_overlay(view, body, frame);
        }
    }

    /// The command line on the stack, if there is one.
    fn command_line(&self) -> Option<&CommandLine> {
        self.views.iter().rev().find_map(|view| match view {
            View::Command(line) => Some(line),
            _ => None,
        })
    }

    /// The top line: what MPDFM is pointed at.
    fn header(&self) -> Paragraph<'_> {
        let root = self
            .library
            .as_ref()
            .map_or(self.config.music_dir.as_str(), |library| {
                library.root().as_str()
            });
        Paragraph::new(Line::from(vec![
            Span::styled("MPDFM", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(root, Style::new().fg(Color::Cyan)),
        ]))
    }

    /// The browser: the current directory's rows, with the cursor on one.
    fn render_browser(&self, area: Rect, frame: &mut ratatui::Frame) {
        let rows = self.rows();
        let title = format!(" {} ", self.dir_label());

        if rows.is_empty() {
            let text = match &self.scan {
                ScanState::Running(_) => "scanning…",
                _ => "nothing here",
            };
            frame.render_widget(
                Paragraph::new(text).block(Block::new().borders(Borders::ALL).title(title)),
                area,
            );
            return;
        }

        let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
        let list = List::new(items)
            .block(Block::new().borders(Borders::ALL).title(title))
            .highlight_symbol("> ")
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));

        let mut state = ListState::default();
        state.select(Some(self.cursor));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// An overlay over the body.
    fn render_overlay(&self, view: &View, body: Rect, frame: &mut ratatui::Frame) {
        match view {
            // Drawn on the bottom line, by `render_message`: a command line that
            // covered the listing would hide what the command is about.
            View::Browser | View::Command(_) => {}
            View::Help { mode, scroll } => self.render_help(*mode, *scroll, body, frame),
            View::Confirm(confirm) => panel(
                " confirm ",
                &format!("{}\n\ny to quit · n or esc to stay", confirm.question),
                Color::Yellow,
                body,
                frame,
            ),
            View::Notice { title, body: text } => panel(
                title,
                &format!("{text}\n\nesc to dismiss"),
                Color::Yellow,
                body,
                frame,
            ),
            View::Error(message) => panel(
                " error ",
                &format!("{message}\n\nesc to dismiss"),
                Color::Red,
                body,
                frame,
            ),
        }
    }

    /// The help overlay, generated from the live keymap.
    ///
    /// Generated and never written down, which is the acceptance criterion: a
    /// binding the user has remapped away cannot be documented here, because this
    /// reads the same table the keypress did.
    ///
    /// It takes the whole body rather than a centred box, because there is more to
    /// say than a box holds — and it scrolls, because there is more to say than the
    /// body holds too. Task 26 owns the grouping and the sections for the modes
    /// other than the one in force.
    fn render_help(&self, mode: Mode, scroll: u16, body: Rect, frame: &mut ratatui::Frame) {
        let rows = self.help_rows(mode);
        let shown = usize::from(body.height.saturating_sub(2));
        let hidden = rows.len().saturating_sub(shown + usize::from(scroll));
        let footer = if hidden > 0 {
            format!(" {hidden} more — j / k to scroll · esc to close ")
        } else {
            " esc to close ".to_owned()
        };

        frame.render_widget(Clear, body);
        frame.render_widget(
            Paragraph::new(rows.join("\n")).scroll((scroll, 0)).block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(Color::Cyan))
                    .title(format!(" help · {mode} "))
                    .title_bottom(footer),
            ),
            body,
        );
    }

    /// One line per binding, then the commands `:` takes.
    fn help_rows(&self, mode: Mode) -> Vec<String> {
        let mut rows: Vec<String> = self
            .keys
            .map()
            .help()
            .into_iter()
            .filter(|section| section.mode == mode)
            .flat_map(|section| section.rows)
            .map(|row| format!("{:<13} {}", row.keys, row.action.help()))
            .collect();

        rows.push(String::new());
        rows.push("commands".to_owned());
        rows.extend(
            command::USAGE
                .iter()
                .map(|(usage, help)| format!("{:<13} {help}", format!(":{usage}"))),
        );
        // Not generated, because it is not in the table: see `App::on_key`.
        rows.push(String::new());
        rows.push(format!(
            "{:<13} {}",
            "ctrl-c", "quit (asks once, then leaves)"
        ));
        rows
    }

    /// Scrolling, while the help overlay has the keyboard.
    ///
    /// Returns `None` for an action the help does not use, so that `q` and `esc`
    /// still mean what they mean.
    fn help_action(&mut self, action: Action) -> Option<bool> {
        let &View::Help { mode, scroll } = self.views.last()? else {
            return None;
        };
        let step = self.page_step();
        let delta = match action {
            Action::Down => 1,
            Action::Up => -1,
            Action::HalfPageDown => step,
            Action::HalfPageUp => -step,
            Action::Top => -isize::MAX,
            Action::Bottom => isize::MAX,
            _ => return None,
        };

        // Not past the end: scrolling into blank space looks like a broken overlay.
        // The body is the terminal less the chrome and the overlay's own border.
        let shown = usize::from(self.size.1.saturating_sub(5));
        let last = self.help_rows(mode).len().saturating_sub(shown);
        let target = u16::try_from(usize::from(scroll).saturating_add_signed(delta).min(last))
            .unwrap_or(u16::MAX);

        if let Some(View::Help { scroll, .. }) = self.views.last_mut() {
            let moved = *scroll != target;
            *scroll = target;
            return Some(moved);
        }
        None
    }

    /// The status bar. Task 26 owns what goes on it and in which order it elides;
    /// this is the subset the shell can answer for.
    fn status_bar(&self) -> Paragraph<'_> {
        let marks = 0; // Task 22 marks files.
        let parts = [
            self.dir_label(),
            format!("{} marked", marks),
            format!("{} pending", self.plan.len()),
            format!("focus {}", self.focus.label()),
            self.mpd
                .as_ref()
                .map_or_else(|| "○ mpd ?".to_owned(), mpd_summary),
        ];
        Paragraph::new(Line::from(parts.join(" · ")).style(Style::new().fg(Color::DarkGray)))
    }

    /// The bottom line, which is also where the command line lives.
    fn render_message(&self, area: Rect, frame: &mut ratatui::Frame) {
        if let Some(line) = self.command_line() {
            self.render_command_line(line, area, frame);
            return;
        }

        // The half-finished sequence goes in the corner, the way vim shows a
        // pending `g`. Without it a prefix key looks like a key that did nothing.
        let partial = self.keys.partial().unwrap_or_default();
        let [text_area, partial_area] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(u16::try_from(partial.chars().count()).unwrap_or(0)),
        ])
        .areas(area);

        frame.render_widget(self.message_text(), text_area);
        if !partial.is_empty() {
            frame.render_widget(
                Paragraph::new(partial).style(Style::new().add_modifier(Modifier::BOLD)),
                partial_area,
            );
        }
    }

    /// The toast, or the scan's progress, or what the keys are.
    fn message_text(&self) -> Paragraph<'_> {
        if let Some(toast) = self.toasts.front() {
            let color = match toast.level {
                Level::Info => Color::Green,
                Level::Warn => Color::Yellow,
            };
            return Paragraph::new(Line::from(toast.text.clone()).style(Style::new().fg(color)));
        }

        let text = match &self.scan {
            ScanState::Running(None) => "scanning…".to_owned(),
            ScanState::Running(Some(progress)) => format!(
                "scanning… {} files, {} dirs — {}",
                progress.files, progress.dirs, progress.dir
            ),
            ScanState::Idle | ScanState::Done { .. } => self.hints(),
        };
        Paragraph::new(Line::from(text).style(Style::new().fg(Color::DarkGray)))
    }

    /// The idle hint, read off the keymap rather than written down.
    ///
    /// A user who has remapped `?` is told the key they chose; a user who has
    /// unbound it is not told about a key that does nothing.
    fn hints(&self) -> String {
        [
            (Action::Help, "help"),
            (Action::CommandMode, "command"),
            (Action::Rescan, "rescan"),
            (Action::Quit, "quit"),
        ]
        .into_iter()
        .filter_map(|(action, label)| {
            let key = self.keys.map().key_for(Mode::Browser, action)?;
            Some(format!("{key} {label}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// `:` and what has been typed after it, with the reason above when there is
    /// one, and the terminal's own cursor where the next character goes.
    fn render_command_line(&self, line: &CommandLine, area: Rect, frame: &mut ratatui::Frame) {
        let input = match line.error() {
            Some(error) => {
                let [above, input] =
                    Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
                frame.render_widget(
                    Paragraph::new(error.to_owned()).style(Style::new().fg(Color::Red)),
                    above,
                );
                input
            }
            None => area,
        };

        frame.render_widget(Paragraph::new(format!(":{}", line.text())), input);
        // Where the next character goes, measured in display columns rather than
        // bytes so that a path with an `ï` in it does not put the cursor adrift.
        let before = Line::raw(&line.text()[..line.cursor()]).width();
        let column = u16::try_from(before + 1).unwrap_or(u16::MAX);
        frame.set_cursor_position((input.x.saturating_add(column), input.y));
    }

    /// Put what was wrong with `keys.toml` in front of the user.
    ///
    /// A panel and not a toast, and the one place this task departs from task 20's
    /// "warnings are transient": the warning for an unknown action lists every
    /// action there is, which is the only thing a user wants at that moment, and a
    /// one-line queue that moves on after four seconds cannot carry it.
    pub fn report_key_warnings(&mut self, warnings: &[KeyWarning]) {
        if warnings.is_empty() {
            return;
        }
        for warning in warnings {
            self.log.line(format!("keys: {warning}"));
        }
        self.views.push(View::Notice {
            title: " keys.toml ".to_owned(),
            body: warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n\n"),
        });
    }
}

/// A bordered box in the middle of the body, with `text` wrapped inside it.
///
/// Three quarters of the body, centred: wide enough for a path, and it leaves the
/// browser visible around the edges so that it is obvious the overlay is on top of
/// something rather than instead of it.
fn panel(title: &str, text: &str, color: Color, body: Rect, frame: &mut ratatui::Frame) {
    let [area] = Layout::horizontal([Constraint::Percentage(75)])
        .flex(Flex::Center)
        .areas(body);
    let [area] = Layout::vertical([Constraint::Percentage(75)])
        .flex(Flex::Center)
        .areas(area);

    // Without this the browser's rows show through the gaps in the text.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text.to_owned())
            .wrap(Wrap { trim: false })
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(color))
                    .title(title.to_owned()),
            ),
        area,
    );
}

/// The screen that replaces the layout when the terminal is too small.
///
/// Wrapped, because at 20 columns even this message does not fit on one line, and
/// a truncated "terminal too sm" is worse than two short lines.
fn too_small(area: Rect) -> Paragraph<'static> {
    Paragraph::new(format!(
        "terminal too small\nneed {}x{}, have {}x{}",
        MIN_SIZE.0, MIN_SIZE.1, area.width, area.height
    ))
    .wrap(Wrap { trim: true })
    .style(Style::new().fg(Color::Yellow))
}

/// What the status bar says about MPD, and therefore also what decides whether a
/// poll's answer is worth a frame.
///
/// Task 26 owns the final shape of this, including the order the parts are elided
/// in on a narrow terminal. What is here is the indicator the task asks for plus
/// the current song's file name — the whole path is too long for a status bar and
/// the directory is usually the album that is already on screen.
fn mpd_summary(snapshot: &MpdSnapshot) -> String {
    let Some(state) = &snapshot.state else {
        // "I was told not to ask" and "it did not answer" are different things to
        // put in front of somebody who is wondering why there is no indicator.
        return if snapshot.enabled {
            "○ offline".to_owned()
        } else {
            "· mpd off".to_owned()
        };
    };
    if state.updating {
        return "◐ updating".to_owned();
    }

    let indicator = match state.play_state {
        mpdfm_core::mpd::PlayState::Play => "● playing",
        mpdfm_core::mpd::PlayState::Pause => "● paused",
        mpdfm_core::mpd::PlayState::Stop => "● connected",
    };
    match &state.song {
        Some(song) => {
            let name = song.rsplit('/').next().unwrap_or(song);
            format!("{indicator} {name}")
        }
        None => indicator.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use mpdfm_core::testing::Fixture;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::tui::msg::{MpdState, ScanOutcome};

    /// A terminal of a given size, with nothing on it yet.
    fn screen(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).expect("a test backend has a size")
    }

    /// Everything that has been drawn, as one string per row.
    ///
    /// Styles are dropped: what is asserted on is what a user reads, and a test
    /// that also pinned the colours would fail every time task 26 adjusted one.
    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Everything drawn, flattened, for a `contains` assertion.
    fn text(terminal: &Terminal<TestBackend>) -> String {
        lines(terminal).join("\n")
    }

    /// An app over a fixture, plus the receiver its workers write to.
    ///
    /// The receiver is returned rather than dropped so that a worker's `send`
    /// succeeds; nothing reads it, because the tests feed the loop a script
    /// instead of waiting on a real scan.
    fn app(fx: &Fixture) -> (App, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        (
            App::new(fx.config(), KeyMap::defaults(), tx, Arc::new(Log::off())),
            rx,
        )
    }

    /// A key press, as the input thread would deliver it.
    fn key(code: KeyCode) -> Msg {
        Msg::Input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// A char key press.
    fn press(c: char) -> Msg {
        key(KeyCode::Char(c))
    }

    /// Draw a frame, telling the app how big the terminal is first.
    ///
    /// Which is what `App::run` does once and a `Resize` does afterwards — the
    /// half-page jump and the help overlay's scroll limit both need it.
    fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        let area = *terminal.backend().buffer().area();
        app.size = (area.width, area.height);
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
    }

    /// Type a whole string, one keypress at a time, as command mode receives it.
    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(press(c));
        }
    }

    /// An app whose keymap is the defaults with `keys_toml` merged over them.
    fn app_with_keys(fx: &Fixture, keys_toml: &str) -> (App, mpsc::Receiver<Msg>) {
        let mut map = KeyMap::defaults();
        let mut warnings = Vec::new();
        crate::tui::keys::merge(
            &mut map,
            camino::Utf8Path::new("keys.toml"),
            keys_toml,
            &mut warnings,
        );
        assert!(
            warnings.is_empty(),
            "the test's own keys.toml: {warnings:?}"
        );
        let (tx, rx) = mpsc::channel();
        (App::new(fx.config(), map, tx, Arc::new(Log::off())), rx)
    }

    /// One staged operation, so that `q` has something to warn about.
    fn staged() -> mpdfm_core::ops::Operation {
        mpdfm_core::ops::Operation::Delete {
            target: mpdfm_core::paths::RelPath::parse("rock/a.mp3").expect("a relative path"),
        }
    }

    /// A scan that succeeded, as the worker would report it.
    fn scanned(fx: &Fixture) -> Msg {
        let library = Library::scan(fx.music_dir()).expect("the fixture scans");
        let (index, playlist_warnings) = PlaylistIndex::load(fx.playlist_dir());
        Msg::ScanDone(Box::new(ScanOutcome {
            library: Ok(library),
            index: Some(index),
            playlist_warnings,
            elapsed: Duration::from_millis(7),
        }))
    }

    // -- the frame ---------------------------------------------------------

    #[test]
    fn it_draws_a_frame_and_q_ends_the_loop() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        let events = Events::scripted(vec![scanned(&fx), press('q')]);
        app.run(&mut terminal, &events)
            .expect("drawing should work");

        assert!(app.quit, "`q` should end the loop");
        let drawn = text(&terminal);
        assert!(drawn.contains("MPDFM"), "{drawn}");
        assert!(
            drawn.contains(fx.music_dir().as_str()),
            "the header should name the library root:\n{drawn}"
        );
        // The browser shows what the scan found, which for the fixture is a set
        // of genre directories at the top level.
        assert!(drawn.contains('/'), "{drawn}");
        assert!(
            drawn.contains("pending"),
            "the status bar should be there:\n{drawn}"
        );
    }

    #[test]
    fn the_loop_ends_when_the_script_runs_out_even_without_a_quit_key() {
        // The production loop never sees this — the tick thread outlives it — but
        // a test whose app will not quit has to fail rather than hang, and this is
        // the assertion that the seam behaves that way.
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        app.run(&mut terminal, &Events::scripted(vec![Msg::Tick]))
            .expect("drawing should work");
        assert!(
            !app.quit,
            "nothing asked it to quit; it ran out of messages"
        );
    }

    #[test]
    fn shutdown_ends_the_loop_the_same_way_q_does() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        app.run(&mut terminal, &Events::scripted(vec![Msg::Shutdown]))
            .expect("drawing should work");
        assert!(
            app.quit,
            "SIGTERM should leave through the same door as `q`"
        );
    }

    #[test]
    fn ctrl_c_quits_because_raw_mode_will_not_turn_it_into_a_signal() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.update(Msg::Input(event));
        assert!(app.quit);
    }

    // -- the size floor ----------------------------------------------------

    #[test]
    fn a_terminal_below_the_floor_says_so_and_recovers_when_it_grows() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);

        // The task's example: 40x10 gets the message.
        let mut small = screen(40, 10);
        app.update(scanned(&fx));
        small
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&small);
        assert!(drawn.contains("too small"), "{drawn}");
        assert!(
            drawn.contains("40x10"),
            "it should say what it has: {drawn}"
        );
        assert!(drawn.contains("60x15"), "and what it needs: {drawn}");
        // Nothing of the real layout leaks through.
        assert!(!drawn.contains("MPDFM"), "{drawn}");

        // Back above the floor, the layout returns — with the library still
        // there, because the too-small screen is a drawing decision and not a
        // state the app enters.
        let mut big = screen(80, 24);
        big.draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&big);
        assert!(drawn.contains("MPDFM"), "{drawn}");
        assert!(!drawn.contains("too small"), "{drawn}");
    }

    #[test]
    fn exactly_at_the_floor_the_real_layout_is_drawn() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));

        let mut terminal = screen(MIN_SIZE.0, MIN_SIZE.1);
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("MPDFM"), "{drawn}");
        assert!(!drawn.contains("too small"), "{drawn}");
    }

    // -- the view stack ----------------------------------------------------

    #[test]
    fn an_overlay_pops_without_losing_what_was_underneath() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        app.update(scanned(&fx));
        // Put the cursor somewhere that is not the default, and remember what the
        // browser was showing there.
        app.update(press('j'));
        app.update(press('j'));
        assert_eq!(app.cursor, 2);
        let before = app.views.clone();

        assert!(app.update(press('?')), "help should open");
        assert_eq!(
            app.views.last(),
            Some(&View::Help {
                mode: Mode::Browser,
                scroll: 0
            })
        );
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let with_help = text(&terminal);
        assert!(with_help.contains("help · browser"), "{with_help}");
        // Derived from the keymap, which is task 21's criterion; `move down` is
        // `Action::Down`'s own help line next to the keys bound to it.
        assert!(with_help.contains("j / down"), "{with_help}");
        assert!(with_help.contains("move down"), "{with_help}");
        // The overlay is over the browser, not instead of it: the header and the
        // status bar are still on screen.
        assert!(with_help.contains("MPDFM"), "{with_help}");
        assert!(with_help.contains("pending"), "{with_help}");

        assert!(app.update(key(KeyCode::Esc)), "esc should close it");
        assert_eq!(app.views, before, "the stack should be back as it was");
        assert_eq!(app.cursor, 2, "the cursor must survive the overlay");

        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let after = text(&terminal);
        assert!(!after.contains("move down"), "{after}");
    }

    #[test]
    fn esc_on_the_base_view_does_not_quit_or_pop() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        app.update(key(KeyCode::Esc));
        assert_eq!(app.views, vec![View::Browser]);
        assert!(!app.quit, "esc is not a way out of the program");
    }

    #[test]
    fn a_second_error_replaces_the_first_rather_than_burying_it() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);

        app.fail("the first thing");
        app.fail("the second thing");
        assert_eq!(app.views.len(), 2, "one panel at a time: {:?}", app.views);
        assert_eq!(
            app.views.last(),
            Some(&View::Error("the second thing".to_owned()))
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.views, vec![View::Browser]);
    }

    // -- scanning ----------------------------------------------------------

    #[test]
    fn progress_reaches_the_message_line_while_the_scan_is_still_running() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        // What `App::run` does first, without the thread: the scan is in flight
        // and nothing has been found yet.
        app.scan = ScanState::Running(None);
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        assert!(text(&terminal).contains("scanning…"), "{}", text(&terminal));

        assert!(app.update(Msg::Progress(ScanProgress {
            files: 1_024,
            dirs: 42,
            dir: DirPath::root(),
        })));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("1024 files"), "{drawn}");
        assert!(drawn.contains("42 dirs"), "{drawn}");
    }

    #[test]
    fn a_finished_scan_fills_the_browser_and_posts_a_toast() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        assert!(app.library.is_none());
        app.update(scanned(&fx));

        let library = app.library.as_ref().expect("the scan should have landed");
        assert!(!library.is_empty());
        assert!(app.index.is_some(), "the playlist index travels with it");
        assert!(matches!(app.scan, ScanState::Done { .. }));

        let toast = app.toasts.front().expect("a finished scan says so");
        assert!(toast.text.contains("files"), "{}", toast.text);

        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("scanned"), "{drawn}");
    }

    #[test]
    fn a_scan_that_failed_opens_a_panel_rather_than_a_toast_that_scrolls_away() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        app.update(Msg::ScanDone(Box::new(ScanOutcome {
            library: Err("/no/such/dir: No such file or directory".to_owned()),
            index: None,
            playlist_warnings: Vec::new(),
            elapsed: Duration::ZERO,
        })));

        assert!(app.toasts.is_empty(), "an error is not a toast");
        let Some(View::Error(message)) = app.views.last() else {
            panic!("a failed scan should open the error panel: {:?}", app.views);
        };
        assert!(message.contains("/no/such/dir"), "{message}");

        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        // The whole message, not a one-line truncation of it.
        assert!(drawn.contains("No such file or directory"), "{drawn}");
        assert!(drawn.contains("esc to dismiss"), "{drawn}");
    }

    #[test]
    fn a_rescan_while_one_is_running_does_not_start_a_second_walk() {
        let fx = Fixture::realistic();
        let (mut app, rx) = app(&fx);

        app.rescan();
        app.rescan();
        // One worker, so one `ScanDone` however many times `R` was pressed. The
        // progress messages before it are the same worker's.
        let done = std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(10)).ok())
            .filter(|msg| matches!(msg, Msg::ScanDone(_)))
            .take(1)
            .count();
        assert_eq!(done, 1);
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a second walk would send a second ScanDone"
        );
    }

    #[test]
    fn a_rescan_brings_the_cursor_back_inside_a_library_that_shrank() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.update(press('G'));
        let bottom = app.cursor;
        assert!(bottom > 0, "the fixture should have more than one row");

        // A library with one directory in it, as a rescan after a big move might
        // find.
        let smaller = Fixture::builder().album("only", &["01 a.mp3"]).build();
        app.update(scanned(&smaller));
        assert!(
            app.cursor < bottom,
            "the cursor should have been clamped, not left past the end"
        );
        assert_eq!(app.cursor, app.row_count() - 1);
    }

    /// The task's worker criterion, end to end: a real scan of a library the size
    /// of the user's, on a real thread, while the UI thread keeps drawing.
    ///
    /// What is asserted is the structure rather than a stopwatch — a timing
    /// threshold on a loaded machine is a flaky test. Two properties, and between
    /// them they are what "the UI never blocks on I/O" means here:
    ///
    /// - **the first frame goes up before the walk has reported anything.** This
    ///   thread calls no scanning code at all, so there is nothing for it to wait
    ///   on; if the scan were inline there would be one frame and it would come
    ///   after the walk;
    /// - **keys are served between the worker's reports**, with the app still in
    ///   `Running`, and every one of those frames shows the progress line.
    #[test]
    fn a_three_thousand_file_scan_runs_on_a_worker_while_the_ui_keeps_drawing() {
        const GENRES: usize = 10;
        const ALBUMS: usize = 15;
        const TRACKS: usize = 18;
        const AUX: &[&str] = &["folder.jpg", "info.nfo"];
        let expected = GENRES * ALBUMS * (TRACKS + AUX.len());

        let mut builder = Fixture::builder();
        for genre in 0..GENRES {
            for album in 0..ALBUMS {
                let dir = format!("genre-{genre:02}/Artist {album:02} - Album {album:02} (2011)");
                let names: Vec<String> = (1..=TRACKS)
                    .map(|track| format!("{track:02} Track {track:02}.mp3"))
                    .collect();
                let tracks: Vec<&str> = names.iter().map(String::as_str).collect();
                builder = builder.album(&dir, &tracks).aux(&dir, AUX);
            }
        }
        let fx = builder.build();

        let (tx, rx) = mpsc::channel();
        let mut app = App::new(fx.config(), KeyMap::defaults(), tx, Arc::new(Log::off()));
        let mut terminal = screen(80, 24);

        app.rescan();
        // The first frame goes up *before* the walk has reported anything, which
        // is what a user sees instead of a blank terminal on a cold scan.
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        assert!(text(&terminal).contains("scanning…"), "{}", text(&terminal));

        let mut frames_during_scan = 0usize;
        let mut progress_reports = 0usize;
        let mut keys_served_during_scan = 0usize;
        let files;

        loop {
            let msg = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the worker should report within 30 s");
            let finished = matches!(msg, Msg::ScanDone(_));
            if matches!(msg, Msg::Progress(_)) {
                progress_reports += 1;
            }
            app.update(msg);

            if !finished {
                // A keystroke arriving mid-scan is handled now, not after the
                // walk: this is the responsiveness the criterion is about.
                app.update(press('?'));
                app.update(key(KeyCode::Esc));
                keys_served_during_scan += 1;

                terminal
                    .draw(|frame| app.render(frame.area(), frame))
                    .expect("drawing should work");
                frames_during_scan += 1;
                let drawn = text(&terminal);
                assert!(drawn.contains("scanning…"), "{drawn}");
            } else {
                files = app.library.as_ref().map_or(0, Library::len);
                break;
            }
        }

        assert_eq!(files, expected, "the worker should have found every file");
        assert!(
            progress_reports > 1,
            "a {expected}-file walk should report more than once, not {progress_reports} time(s)"
        );
        assert_eq!(frames_during_scan, progress_reports);
        assert!(
            frames_during_scan > 1,
            "the UI drew {frames_during_scan} frame(s) mid-scan"
        );
        assert!(keys_served_during_scan > 1);

        // And the counts are on screen once it is over.
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains(&expected.to_string()), "{drawn}");
    }

    // -- drawing only when something changed -------------------------------

    #[test]
    fn a_message_that_changes_nothing_visible_asks_for_no_frame() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.toasts.clear();

        // A tick with no toast to expire: the MPD poll it starts is a thread, and
        // its answer is a message of its own.
        assert!(!app.update(Msg::Tick), "an idle tick must not redraw");

        // A key nothing is bound to.
        assert!(!app.update(press('Z')));

        // The cursor already at the top, asked to go up.
        app.cursor = 0;
        assert!(!app.update(press('k')));

        // Events the app has no use for.
        assert!(!app.update(Msg::Input(Event::FocusGained)));
    }

    #[test]
    fn a_tick_that_retires_a_toast_does_ask_for_a_frame() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);

        app.notify_for(Level::Info, "gone in a moment", Duration::ZERO);
        assert_eq!(app.toasts.len(), 1);
        assert!(app.update(Msg::Tick), "the message line changed");
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn two_messages_in_one_second_are_both_shown_rather_than_one_replacing_the_other() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        app.notify_for(Level::Info, "the first thing", Duration::ZERO);
        app.notify_for(Level::Warn, "the second thing", Duration::ZERO);

        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("the first thing"), "{drawn}");
        assert!(
            !drawn.contains("the second thing"),
            "one at a time: {drawn}"
        );

        // The second one's turn comes when the first one's is over, and it has not
        // been counting down in the meantime.
        assert!(app.update(Msg::Tick));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("the second thing"), "{drawn}");

        assert!(app.update(Msg::Tick));
        assert!(app.toasts.is_empty());
        assert!(!app.update(Msg::Tick), "an empty queue is not a redraw");
    }

    #[test]
    fn a_queued_message_does_not_start_counting_down_until_it_is_on_screen() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);

        app.notify(Level::Info, "shown now");
        app.notify(Level::Info, "shown later");

        let front = app.toasts.front().expect("something is queued");
        assert!(front.expires.is_some(), "the visible one has a clock");
        let back = app.toasts.back().expect("two are queued");
        assert!(back.expires.is_none(), "the waiting one does not");
    }

    #[test]
    fn esc_dismisses_one_message_at_a_time() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);

        app.notify(Level::Info, "one");
        app.notify(Level::Info, "two");

        assert!(app.update(key(KeyCode::Esc)));
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(
            app.toasts.front().map(|toast| toast.text.as_str()),
            Some("two")
        );
        assert!(app.update(key(KeyCode::Esc)));
        assert!(app.toasts.is_empty());
        // And with nothing left to dismiss, `esc` on the base view is a no-op
        // rather than a redraw.
        assert!(!app.update(key(KeyCode::Esc)));
    }

    #[test]
    fn the_message_queue_is_bounded() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        for n in 0..TOAST_QUEUE * 3 {
            app.notify(Level::Info, format!("message {n}"));
        }
        assert_eq!(app.toasts.len(), TOAST_QUEUE);
        // The newest survive, because they are the ones a user wants.
        assert!(
            app.toasts
                .back()
                .is_some_and(|toast| toast.text.ends_with(&(TOAST_QUEUE * 3 - 1).to_string()))
        );
    }

    #[test]
    fn a_resize_asks_for_a_frame_and_is_remembered() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        assert!(app.update(Msg::Input(Event::Resize(100, 40))));
        assert_eq!(app.size, (100, 40));
    }

    #[test]
    fn an_mpd_answer_that_repeats_itself_costs_no_frame() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);

        let playing = |song: &str| {
            Msg::MpdStatus(Box::new(MpdSnapshot {
                state: Some(MpdState {
                    play_state: mpdfm_core::mpd::PlayState::Play,
                    song: Some(song.to_owned()),
                    updating: false,
                }),
                enabled: true,
                problem: None,
            }))
        };

        assert!(
            app.update(playing("rock/a.mp3")),
            "the first answer is news"
        );
        assert!(!app.update(playing("rock/a.mp3")), "the same answer is not");
        assert!(app.update(playing("rock/b.mp3")), "a new song is");

        // And the poll is not stacked up: the tick only starts one when none is out.
        assert!(!app.mpd_in_flight, "an answer clears the flag");
    }

    #[test]
    fn the_status_bar_says_what_mpd_is_doing() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));

        app.update(Msg::MpdStatus(Box::new(MpdSnapshot {
            state: None,
            enabled: true,
            problem: Some("connection refused".to_owned()),
        })));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        assert!(text(&terminal).contains("offline"), "{}", text(&terminal));

        // Switched off is not the same as not answering, and the bar says which.
        app.update(Msg::MpdStatus(Box::new(MpdSnapshot {
            state: None,
            enabled: false,
            problem: Some("MPD is switched off for this run".to_owned()),
        })));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("mpd off"), "{drawn}");
        assert!(!drawn.contains("offline"), "{drawn}");

        app.update(Msg::MpdStatus(Box::new(MpdSnapshot {
            state: Some(MpdState {
                play_state: mpdfm_core::mpd::PlayState::Play,
                song: Some("hiphop/MF DOOM/01 Doomsday.mp3".to_owned()),
                updating: false,
            }),
            enabled: true,
            problem: None,
        })));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let drawn = text(&terminal);
        assert!(drawn.contains("playing"), "{drawn}");
        // The file name, not the whole path: a status bar has 80 columns and the
        // directory is usually the album already on screen.
        assert!(drawn.contains("01 Doomsday.mp3"), "{drawn}");
        assert!(!drawn.contains("hiphop/MF DOOM/01"), "{drawn}");
    }

    // -- the cursor and the focus ------------------------------------------

    #[test]
    fn the_cursor_stops_at_both_ends_rather_than_wrapping() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        let last = app.row_count() - 1;

        app.update(press('k'));
        assert_eq!(app.cursor, 0, "up from the top stays at the top");

        app.update(press('G'));
        assert_eq!(app.cursor, last);
        app.update(press('j'));
        assert_eq!(app.cursor, last, "down from the bottom stays at the bottom");

        // `gg`, which takes two presses: one `g` is a prefix and nothing else.
        app.update(press('g'));
        assert_eq!(app.cursor, last, "a lone `g` moves nothing");
        app.update(press('g'));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn the_cursor_does_nothing_in_a_library_with_no_rows() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        assert_eq!(app.row_count(), 0);
        assert!(!app.update(press('j')));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn tab_moves_the_focus_and_the_status_bar_shows_which_pane_has_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));

        assert_eq!(app.focus, Focus::Tree);
        assert!(app.update(key(KeyCode::Tab)));
        assert_eq!(app.focus, Focus::Files);
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        assert!(
            text(&terminal).contains("focus files"),
            "{}",
            text(&terminal)
        );

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn a_key_release_is_not_a_second_keypress() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));

        let mut release = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!app.update(Msg::Input(Event::Key(release))));
        assert_eq!(app.cursor, 0, "a release must not move anything");
    }

    // -- the keymap --------------------------------------------------------

    #[test]
    fn every_action_in_the_vocabulary_is_answered_by_something() {
        // `dispatch`'s `match` is exhaustive, so this cannot find an action with no
        // arm. What it does find is one that panics, and one that claims nothing
        // changed when it opened a view.
        let fx = Fixture::realistic();
        for &action in Action::ALL {
            let (mut app, _rx) = app(&fx);
            app.update(scanned(&fx));
            let dirty = app.dispatch(action);
            if action == Action::ForceQuit {
                assert!(app.quit, "force_quit should leave");
                continue;
            }
            assert!(
                dirty
                    || matches!(
                        action,
                        Action::Quit
                            | Action::Submit
                            | Action::DeleteChar
                            | Action::ClearLine
                            | Action::Top
                            | Action::Up
                            | Action::HalfPageUp
                    ),
                "{action} did nothing and did not say why"
            );
        }
    }

    #[test]
    fn an_action_this_task_does_not_implement_names_the_task_that_does() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.toasts.clear();

        assert!(app.update(press('e')), "`e` is bound to edit_tags");
        let toast = app.toasts.front().expect("it should say something");
        assert!(toast.text.contains("edit tags"), "{}", toast.text);
        assert!(toast.text.contains("23-tagedit-view.md"), "{}", toast.text);
        assert_eq!(
            toast.level,
            Level::Warn,
            "a key that did nothing is a surprise, not news"
        );
    }

    #[test]
    fn a_remapped_key_works_and_the_key_it_replaced_does_not() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app_with_keys(
            &fx,
            "[browser]\n\"J\" = \"half_page_down\"\n\"j\" = \"none\"\n",
        );
        app.update(scanned(&fx));
        app.size = (80, 24);

        assert!(!app.update(press('j')), "`j` was unbound");
        assert_eq!(app.cursor, 0);
        assert!(app.update(press('J')), "`J` is a half page now");
        assert_eq!(app.cursor, app.row_count() - 1, "the fixture is short");
    }

    #[test]
    fn a_half_page_is_half_the_listing_and_stops_at_the_end() {
        // A library with enough rows that a half page is not the whole of it.
        let mut builder = Fixture::builder();
        for n in 0..40 {
            builder = builder.album(&format!("album-{n:02}"), &["01 a.mp3"]);
        }
        let fx = builder.build();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.size = (80, 24);
        assert_eq!(app.row_count(), 40);

        let step = usize::try_from(app.page_step()).expect("a positive step");
        assert!((2..20).contains(&step), "{step} is not half a screen");
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(app.cursor, step);
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(app.cursor, 0, "and back, stopping at the top");
    }

    #[test]
    fn the_bottom_line_names_the_keys_that_are_actually_bound() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app_with_keys(&fx, "[browser]\n\"?\" = \"none\"\n\"f1\" = \"help\"\n");
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.toasts.clear();

        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("f1 help"), "{drawn}");
        assert!(!drawn.contains("? help"), "{drawn}");
        assert!(drawn.contains("q quit"), "{drawn}");
    }

    #[test]
    fn a_pending_sequence_shows_in_the_corner_until_it_times_out() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.toasts.clear();

        assert!(app.update(press('g')), "a prefix is worth a frame");
        draw(&mut app, &mut terminal);
        let bottom = lines(&terminal).pop().expect("there is a bottom line");
        assert!(bottom.ends_with('g'), "{bottom:?}");
    }

    #[test]
    fn startup_warnings_about_keys_toml_open_a_panel_rather_than_scrolling_past() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);

        let mut map = KeyMap::defaults();
        let mut warnings = Vec::new();
        crate::tui::keys::merge(
            &mut map,
            camino::Utf8Path::new("/tmp/keys.toml"),
            "[browser]\n\"ctrl-r\" = \"rescann\"\n",
            &mut warnings,
        );
        app.report_key_warnings(&warnings);

        assert!(app.toasts.is_empty(), "a list of actions is not a toast");
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("keys.toml"), "{drawn}");
        assert!(drawn.contains("rescann"), "{drawn}");
        // The valid names, which is the whole reason this is a panel.
        assert!(drawn.contains("rescan"), "{drawn}");
        assert!(drawn.contains("esc to dismiss"), "{drawn}");

        assert!(app.update(key(KeyCode::Esc)));
        assert_eq!(app.views, vec![View::Browser]);
    }

    #[test]
    fn nothing_at_all_is_wrong_with_the_default_keys() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.report_key_warnings(&[]);
        assert_eq!(app.views, vec![View::Browser], "no panel for no warnings");
    }

    // -- the help overlay --------------------------------------------------

    #[test]
    fn the_rendered_help_follows_a_remap() {
        let fx = Fixture::realistic();
        let mut terminal = screen(80, 24);

        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.update(press('?'));
        draw(&mut app, &mut terminal);
        let before = text(&terminal);
        assert!(before.contains("j / down"), "{before}");

        // The same help, over a keymap where `j` is somewhere else. Nothing about
        // the overlay is written down, so this is the whole of what changed.
        let (mut app, _rx) =
            app_with_keys(&fx, "[browser]\n\"j\" = \"none\"\n\"ctrl-j\" = \"down\"\n");
        app.update(scanned(&fx));
        app.update(press('?'));
        draw(&mut app, &mut terminal);
        let after = text(&terminal);
        assert!(after.contains("ctrl-j"), "{after}");
        assert!(
            !after.contains("j / down"),
            "the help must not document a binding that was remapped away:\n{after}"
        );
    }

    #[test]
    fn the_help_lists_the_commands_and_scrolls_to_reach_them() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.update(press('?'));

        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("more — j / k to scroll"), "{drawn}");
        assert!(
            !drawn.contains(":organize"),
            "it is below the fold:\n{drawn}"
        );

        // Down to the bottom, where the commands are.
        assert!(app.update(press('G')));
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains(":organize <template>"), "{drawn}");
        assert!(drawn.contains(":q!"), "{drawn}");
        // `ctrl-c` is not in the keymap, and the help says so anyway.
        assert!(drawn.contains("ctrl-c"), "{drawn}");

        assert!(app.update(press('g')) && app.update(press('g')), "back up");
        assert!(matches!(
            app.views.last(),
            Some(View::Help { scroll: 0, .. })
        ));
        // And the cursor underneath never moved, because the overlay had the keys.
        assert_eq!(app.cursor, 0);

        assert!(app.update(press('?')), "`?` closes it again");
        assert_eq!(app.views, vec![View::Browser]);
    }

    // -- command mode ------------------------------------------------------

    #[test]
    fn command_mode_types_a_command_and_runs_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.toasts.clear();

        assert!(app.update(press(':')), "`:` opens the line");
        assert_eq!(app.mode(), Mode::Command);
        type_in(&mut app, "move hiphop/MF DOOM");
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains(":move hiphop/MF DOOM"), "{drawn}");
        // The letters went into the line and not into the browser: `m` is
        // `stage_move` out here, and the cursor has not moved either.
        assert_eq!(app.cursor, 0);

        assert!(app.update(key(KeyCode::Enter)), "enter runs it");
        assert_eq!(app.views, vec![View::Browser], "and closes the line");
        let toast = app.toasts.front().expect("it reported something");
        assert!(
            toast.text.contains("move to hiphop/MF DOOM"),
            "{}",
            toast.text
        );
    }

    #[test]
    fn a_command_that_does_not_parse_leaves_the_line_open_with_the_reason_under_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.toasts.clear();

        app.update(press(':'));
        type_in(&mut app, "wibble");
        assert!(app.update(key(KeyCode::Enter)));
        assert_eq!(
            app.mode(),
            Mode::Command,
            "the line stays open to be edited"
        );

        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("no command `wibble`"), "{drawn}");
        assert!(
            drawn.contains(":wibble"),
            "what caused it is still there:\n{drawn}"
        );

        // Editing clears the complaint, and the line can be fixed rather than
        // retyped.
        for _ in 0..6 {
            app.update(key(KeyCode::Backspace));
        }
        type_in(&mut app, "doctor");
        assert!(app.update(key(KeyCode::Enter)));
        assert_eq!(app.views, vec![View::Browser]);
        let toast = app.toasts.front().expect("doctor said something");
        assert!(toast.text.contains("29-doctor.md"), "{}", toast.text);
    }

    #[test]
    fn the_command_line_edits_and_esc_abandons_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.toasts.clear();

        app.update(press(':'));
        type_in(&mut app, "doctor");
        // ctrl-u is `clear_line` here and `half_page_up` in the browser, which is
        // the whole argument for modes.
        assert!(app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL
        )))));
        assert_eq!(app.command_line().map(CommandLine::text), Some(""));

        type_in(&mut app, "q");
        assert!(app.update(key(KeyCode::Esc)), "esc closes it");
        assert_eq!(app.views, vec![View::Browser]);
        assert!(!app.quit, "the `q` it held was text, not a key");

        // `:` then enter is a change of mind and not an error.
        app.update(press(':'));
        assert!(app.update(key(KeyCode::Enter)));
        assert_eq!(app.views, vec![View::Browser]);
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn colon_q_is_the_same_door_as_the_q_key() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.plan.push(staged());

        app.update(press(':'));
        type_in(&mut app, "q");
        app.update(key(KeyCode::Enter));
        assert!(!app.quit, "there is a plan, so it asks");
        assert!(matches!(app.views.last(), Some(View::Confirm(_))));
        app.update(press('n'));

        // And `:q!` does not ask, which is the only difference between them.
        app.update(press(':'));
        type_in(&mut app, "q!");
        app.update(key(KeyCode::Enter));
        assert!(app.quit);
    }

    // -- quitting with a plan in hand --------------------------------------

    #[test]
    fn q_with_staged_operations_asks_before_discarding_them() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(80, 24);
        app.update(scanned(&fx));
        app.plan.push(staged());

        assert!(app.update(press('q')));
        assert!(!app.quit, "it must ask, not leave");
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("1 staged operation"), "{drawn}");
        assert!(drawn.contains("y to quit"), "{drawn}");

        // Anything that is not an answer leaves the question up.
        assert!(!app.update(press('j')));
        assert!(matches!(app.views.last(), Some(View::Confirm(_))));

        // `n` stays, and the plan is still there.
        assert!(app.update(press('n')));
        assert!(!app.quit);
        assert_eq!(app.plan.len(), 1);
        assert_eq!(app.views, vec![View::Browser]);

        // `y` leaves.
        app.update(press('q'));
        assert!(app.update(press('y')));
        assert!(app.quit);
    }

    #[test]
    fn q_with_nothing_staged_does_not_ask() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        assert!(app.plan.is_empty());
        app.update(press('q'));
        assert!(app.quit, "there was nothing to lose");
    }

    #[test]
    fn ctrl_c_asks_once_and_then_leaves_whatever_the_answer_would_have_been() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.plan.push(staged());

        let ctrl_c = || {
            Msg::Input(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )))
        };

        app.update(ctrl_c());
        assert!(!app.quit, "the first one asks, like `q`");
        assert!(matches!(app.views.last(), Some(View::Confirm(_))));
        app.update(ctrl_c());
        assert!(
            app.quit,
            "and the second leaves: two presses always get out"
        );
    }

    #[test]
    fn a_confirmation_cannot_be_rebound_out_of_existence() {
        // `y` / `n` are not in the keymap, so a keys.toml that binds them to
        // something else cannot leave a user stuck in a prompt.
        let fx = Fixture::realistic();
        let (mut app, _rx) =
            app_with_keys(&fx, "[browser]\n\"y\" = \"rescan\"\n\"n\" = \"rescan\"\n");
        app.update(scanned(&fx));
        app.plan.push(staged());

        app.update(press('q'));
        assert!(app.update(press('y')));
        assert!(app.quit);
    }

    // -- the log -----------------------------------------------------------

    #[test]
    fn the_log_records_the_session_and_nothing_reaches_the_terminal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("session.log"))
            .expect("temp path is UTF-8");

        let fx = Fixture::realistic();
        let (tx, _rx) = mpsc::channel();
        let log = Arc::new(Log::to_file(&path).expect("a log in a temp dir opens"));
        let mut app = App::new(fx.config(), KeyMap::defaults(), tx, log);
        let mut terminal = screen(80, 24);

        let events = Events::scripted(vec![scanned(&fx), press('?'), press('q')]);
        app.run(&mut terminal, &events)
            .expect("drawing should work");

        let text = std::fs::read_to_string(&path).expect("read the log");
        assert!(text.contains("msg: scan-done"), "{text}");
        assert!(text.contains("view: push help"), "{text}");
        assert!(text.contains("loop: leaving"), "{text}");
    }
}
