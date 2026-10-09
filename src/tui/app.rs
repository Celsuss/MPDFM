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
//! # What is here and what is in a view
//!
//! This file owns the shell: the loop, the stack, the chrome, and the dispatch
//! that turns an [`Action`] into a call on whatever has the keyboard. It owns no
//! cursor and no listing — [`Browser`] does (task 22), and tasks 23–25 add the
//! views beside it. The rule that keeps the two apart is that nothing here
//! reaches into a view's state to answer a question a method could answer, and
//! nothing in a view knows what a frame is.
//!
//! The status bar is the one deliberate exception: it reads a little from
//! everything, because that is what a status bar is. Task 26 owns what goes on
//! it and in which order it elides.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library, ScanProgress};
use mpdfm_core::ops::commit::Progress;
use mpdfm_core::ops::{Effects, Live, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget as _, Wrap};

use super::action::Action;
use super::command::{self, Command, CommandLine};
use super::event::Events;
use super::keys::{KeyChord, KeyMap, KeyWarning, Keys, Mode, Resolution};
use super::log::Log;
use super::msg::{MpdSnapshot, Msg, NotCommitted, Reads, ScanOutcome, TaskOutcome};
use super::terminal::{MIN_SIZE, fits};
use super::views::browser::{Browser, Enter, Pane, Sort, TreeRow};
use super::views::pending::{self, Pending, Report};
use super::views::tagedit::{Begin, FileAction, Hints, Preview, Started, TagEdit};
use super::widgets::details::DetailsPane;
use super::widgets::filelist::FileList;
use super::widgets::input::Input;
use super::widgets::{fit, pad};
use super::{PANIC_AT, work};

/// How long an informational toast stays up once it is the one on screen.
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// Rows of chrome around the body: the header, the status bar, the message line.
///
/// Named because three places need the same number — the layout, the half-page
/// step, and the estimate of how many rows of tags to read ahead — and a layout
/// that disagreed with the read-ahead by a constant would be a bug nobody saw.
const CHROME_ROWS: u16 = 3;

/// How wide the terminal has to be before the details pane is worth its space.
///
/// Below this the two panes that can be navigated get the whole width, which is
/// the right trade: a 14-cell details pane is unreadable and the listing is what
/// the keys act on. The task calls it "a third, narrow column on wide terminals".
const DETAILS_FROM: u16 = 90;

/// Cells given to the details pane when it is shown.
const DETAILS_W: u16 = 26;

/// The actions that still mean something while a tag field is open for typing.
///
/// A key that types a character types it instead, whatever it is bound to —
/// see [`App::on_field_key`], which is where this is used and why it is a list
/// rather than a `match`.
const IN_FIELD: &[Action] = &[
    Action::Left,
    Action::Right,
    Action::Up,
    Action::Down,
    Action::DeleteChar,
    Action::ClearLine,
    Action::Submit,
    Action::Cancel,
];

/// How much of the body a per-file action's preview box takes, in percent.
///
/// Named because two things need the same number: the box the frame draws, and
/// the estimate of how far one keypress scrolls it.
const PREVIEW_PERCENT: usize = 80;

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

    /// The browser's name for the same pane.
    ///
    /// Two enums rather than one so the view does not depend on the shell; see
    /// [`Pane`]. The mapping is total and this is the only place it is written.
    fn pane(self) -> Pane {
        match self {
            Self::Tree => Pane::Tree,
            Self::Files => Pane::Files,
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
    /// The tag editor, on the marks or on the file under the cursor. Task 23.
    ///
    /// Boxed because it holds every selected file's tags — two hundred `TagSet`s
    /// is not a thing to move through a `match` every time a key is pressed.
    TagEdit(Box<TagEdit>),
    /// What is staged, what it would do, and the `c` that says yes. Task 24.
    ///
    /// Boxed for the same reason as the editor: it holds an `Effects`, which for
    /// a two-thousand-file organize is every filesystem step and every playlist
    /// line in the plan.
    ///
    /// It holds the *preview* and not the plan. The plan is `App::plan`, which is
    /// what makes staged operations survive `esc` and a resize — popping this
    /// view throws away a cursor and some folds, and nothing else.
    Pending(Box<Pending>),
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
            Self::Help { .. }
                | Self::Confirm(_)
                | Self::Notice { .. }
                | Self::Error(_)
                | Self::TagEdit(_)
                | Self::Pending(_)
        )
    }

    /// The name the status bar shows and the log records.
    fn name(&self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::TagEdit(_) => "tagedit",
            Self::Pending(_) => "pending",
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
    /// What a `y` means.
    on_yes: Answer,
}

/// What saying yes to a [`Confirm`] does.
///
/// Most answers are an action, which is what makes `q` → "2 staged operations
/// would be lost" → `y` work without a second code path. One is not: throwing
/// away a form's unsaved fields is not a verb in the vocabulary and should not
/// become one, because nothing outside the tag editor could mean it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// Dispatch this action.
    Act(Action),
    /// Close the tag editor, throwing away what was typed into it.
    DiscardEdits,
    /// Throw the staged plan away.
    ///
    /// Not an action for the same reason as `DiscardEdits`: `x` is the verb, and
    /// it is the thing that *asks*. A second action that discarded without
    /// asking would be a key a user could bind and lose a plan to.
    DiscardPlan,
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

/// A transaction on a worker, and what it has said so far.
///
/// One of these at a time: a commit and an undo are the same thing from the
/// library's point of view — a transaction in progress — and the second of two
/// would be previewed against a library the first is in the middle of changing.
#[derive(Debug)]
struct Running {
    /// What it is, for the message line: `committing`, `undoing`.
    what: &'static str,
    /// How many operations it is about, for the same.
    ops: usize,
    /// The last thing the worker said. `None` until the first word of it.
    progress: Option<Progress>,
    /// Set from this thread to call a commit off.
    ///
    /// Shared with the worker rather than sent to it: by the time a message had
    /// been received the commit would be past the one boundary where it can
    /// still be abandoned with nothing to put back (`commit::Options::cancel`).
    cancel: Arc<AtomicBool>,
    /// Whether the user has asked for that and the worker has not answered yet.
    cancelling: bool,
}

impl Running {
    /// The line the message bar shows while this is going on.
    fn line(&self) -> String {
        if self.cancelling {
            return format!("{}: stopping before anything is changed…", self.what);
        }
        let plural = if self.ops == 1 { "" } else { "s" };
        let detail = match self.progress {
            None => "starting…".to_owned(),
            Some(Progress::Validating) => "re-checking the library…".to_owned(),
            Some(Progress::BackingUp { steps }) => {
                format!("backing up, {steps} step(s) to run…")
            }
            Some(Progress::Steps { done, steps }) => {
                let percent = done.saturating_mul(100) / steps.max(1);
                format!("{done}/{steps} files ({percent}%)")
            }
            Some(Progress::Playlists { playlists }) => {
                format!("rewriting {playlists} playlist(s)…")
            }
            Some(Progress::Finishing) => "finishing…".to_owned(),
        };
        format!("{} {} operation{plural} · {detail}", self.what, self.ops)
    }

    /// Whether this can still be called off.
    ///
    /// Only before the first mutation, which is while the plan is being
    /// re-validated. After that the honest answer is that the transaction has to
    /// finish or be recovered, and a key that pretended otherwise would be
    /// worse than no key.
    fn is_cancellable(&self) -> bool {
        !self.cancelling && matches!(self.progress, None | Some(Progress::Validating))
    }
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
    /// The browser: where it is in the tree, what is marked, what is cached.
    browser: Browser,
    /// Whether a tag read is already out, so a held `j` does not start a thread
    /// per row. Cleared when the answer lands, which is also when the next
    /// window is asked for.
    tags_in_flight: bool,
    /// How many things the last scan could not model — non-UTF-8 names, an
    /// unreadable directory, a playlist that would not parse.
    ///
    /// A badge on the status bar and not a silent omission: a library browser
    /// that quietly shows 2 799 of 2 800 files is lying about the library.
    warnings: usize,
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
    /// What MPD last said. `None` until the first poll answers.
    mpd: Option<MpdSnapshot>,
    /// Whether an MPD poll is already out, so the tick does not stack them up.
    mpd_in_flight: bool,
    /// The commit or undo on a worker, when there is one, and how far it has got.
    running: Option<Running>,
    /// MPD's live queue, as the last poll that asked for it found it.
    ///
    /// Only polled for while something is staged, and held here because the
    /// preview and the commit must be given the *same* answer: whether the
    /// daemon is holding a queue decides whether a move rewrites MPD's saved
    /// queue or warns that it will need a requeue, and disagreeing about that
    /// between the two is drift and a refused commit.
    queue: Option<Vec<RelPath>>,
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
            browser: Browser::new(),
            tags_in_flight: false,
            warnings: 0,
            keys: Keys::new(keys),
            views: vec![View::Browser],
            toasts: VecDeque::new(),
            scan: ScanState::Idle,
            mpd: None,
            mpd_in_flight: false,
            running: None,
            queue: None,
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
                self.remember_scroll();
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
        let dirty = match msg {
            Msg::Input(event) => self.on_event(event),
            Msg::Tick => self.on_tick(),
            Msg::Progress(progress) => {
                self.scan = ScanState::Running(Some(progress));
                true
            }
            Msg::ScanDone(outcome) => self.on_scan_done(*outcome),
            Msg::MpdStatus(snapshot) => self.on_mpd(*snapshot),
            Msg::TaskDone(outcome) => self.on_task_done(*outcome),
            Msg::Committing(progress) => self.on_committing(progress),
            Msg::Shutdown => {
                self.log.line("shutdown: asked to stop");
                self.quit = true;
                false
            }
        };

        // Whatever just happened may have brought a row into view, so this is
        // the one place that asks — after the state has settled and before the
        // frame is drawn. It never reads a file itself; see `request_tags`.
        self.request_tags();
        dirty
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

        // A form field being typed into is the other place a key means something
        // it does not mean anywhere else: a letter.
        if self.tagedit().is_some_and(TagEdit::is_editing) {
            return self.on_field_key(key);
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
    /// Tasks 24 and 25 add the views that reach the other two modes. A panel is
    /// something to dismiss rather than a mode, which is why one resolves as
    /// [`Mode::Browser`] and then has its keys filtered in [`App::dispatch`].
    fn mode(&self) -> Mode {
        match self.views.last() {
            Some(View::Command(_)) => Mode::Command,
            Some(View::TagEdit(_)) => Mode::TagEdit,
            Some(View::Pending(_)) => Mode::Pending,
            _ => Mode::Browser,
        }
    }

    /// `y` or `n`, and nothing else.
    fn on_confirm_key(&mut self, confirm: &Confirm, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                self.views.pop();
                match &confirm.on_yes {
                    Answer::Act(action) => {
                        self.dispatch(*action);
                    }
                    // The editor is what the question was in front of, so it is
                    // what is on top now.
                    Answer::DiscardEdits => {
                        if matches!(self.views.last(), Some(View::TagEdit(_))) {
                            self.log.line("tagedit: changes discarded");
                            self.views.pop();
                        }
                    }
                    Answer::DiscardPlan => {
                        self.discard_plan();
                    }
                }
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

    /// A keypress while one of the tag editor's fields is open for typing.
    ///
    /// The rule, in one sentence: **a key that types a character types it, and
    /// every other key keeps its binding** — filtered to the verbs a field can
    /// use ([`IN_FIELD`]). So `j` and `G`, which move between fields everywhere
    /// else in this mode, type a `j` and a `G`; `ctrl-u` still clears the line,
    /// `backspace` still deletes, `tab` and the arrows still move, and all four
    /// still follow a remap.
    ///
    /// That is what lets one `[tagedit]` section in `keys.toml` serve both halves
    /// of the form. The alternative was a second mode, which would mean a user
    /// configuring the editor had to know which of two sections each key lands
    /// in, and a section that must be left unbound for the letters to stay
    /// letters.
    ///
    /// The keymap is not consulted for a character at all, which also settles
    /// what a half-finished sequence means in here: the first `g` of a `gg` is a
    /// letter in a text field, and never a key that is waiting for its second.
    fn on_field_key(&mut self, key: KeyEvent) -> bool {
        if let Some(c) = KeyChord::from_event(key).typed() {
            return self
                .tagedit_mut()
                .and_then(TagEdit::input_mut)
                .is_some_and(|input| input.insert(c));
        }
        match self.keys.press(Mode::TagEdit, key, Instant::now()) {
            Resolution::Act(action) if IN_FIELD.contains(&action) => self.dispatch(action),
            // A sequence that begins with a named key — `ctrl-x s` — can still be
            // half-finished in here, and the corner shows it the way it does
            // anywhere else.
            Resolution::Partial => true,
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

        // The tag editor has the keyboard and answers most of the vocabulary
        // itself. What it does not claim — the help, quitting — falls through to
        // the panel rules below, which is how `?` opens the help over a form.
        if let Some(dirty) = self.tagedit_action(action) {
            return dirty;
        }

        // And so does the pending view, for the same reason and in the same
        // place: `d` unstages in there and stages a delete everywhere else.
        if let Some(dirty) = self.pending_action(action) {
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
            // -- moving around, which is the browser's -----------------------
            Action::Down => {
                self.browse(|browser, pane, library| browser.move_cursor(pane, 1, library))
            }
            Action::Up => {
                self.browse(|browser, pane, library| browser.move_cursor(pane, -1, library))
            }
            Action::Top => {
                self.browse(|browser, pane, library| browser.set_cursor(pane, 0, library))
            }
            Action::Bottom => self.browse(|browser, pane, library| browser.go_last(pane, library)),
            Action::HalfPageDown => {
                let step = self.page_step();
                self.browse(|browser, pane, library| browser.move_cursor(pane, step, library))
            }
            Action::HalfPageUp => {
                let step = -self.page_step();
                self.browse(|browser, pane, library| browser.move_cursor(pane, step, library))
            }
            Action::SwitchPane => {
                self.focus = self.focus.toggled();
                true
            }
            Action::Left => {
                let pane = self.focus.pane();
                self.browse(move |browser, _, library| browser.leave(pane, library))
            }
            Action::Parent => self.browse(|browser, _, library| browser.go_up(library)),
            Action::Right | Action::Open => self.open_action(),

            // -- marking ------------------------------------------------------
            Action::ToggleMark => self.browse(|browser, _, library| browser.toggle_mark(library)),
            Action::VisualSelect => self.browse(|browser, _, library| browser.visual(library)),
            Action::MarkAll => self.browse(|browser, _, library| browser.mark_all(library)),
            Action::UnmarkAll => self.browse(|browser, _, _| browser.unmark_all()),

            // -- chrome -------------------------------------------------------
            // `esc` closes an open visual range before it dismisses a message:
            // an abandoned selection is the more recent of the two, and the one
            // the user is looking at.
            Action::Cancel => self.browser.cancel_visual() || self.pop(),
            Action::CommandMode => self.push(View::Command(CommandLine::new())),
            Action::Help => self.toggle_help(),
            Action::Rescan => {
                self.rescan();
                true
            }
            Action::Quit | Action::ForceQuit => self.quit_action(action),

            // -- changing things ----------------------------------------------
            Action::EditTags => self.open_tag_editor(),
            Action::Undo => self.undo_last(None),

            // -- staging, and the view that shows what was staged -------------
            Action::StageMove => self.stage_move(),
            Action::Rename => self.rename(),
            Action::StageDelete => self.stage_delete(),
            Action::ShowPending => self.show_pending(),
            Action::Commit => self.commit_pending(),
            Action::DiscardPending => self.discard_pending(),
            // The one verb that needs a staged operation to point at, so the
            // browser has nothing to do with it.
            Action::Unstage => {
                let key = self.keys.map().key_for(Mode::Pending, Action::ShowPending);
                let how = key.map_or_else(
                    || "open the pending view first".to_owned(),
                    |key| format!("press {key} for the pending view"),
                );
                self.notify(Level::Warn, format!("nothing to unstage here — {how}"));
                true
            }

            // -- the views that are not built yet ---------------------------
            Action::Search | Action::SearchNext | Action::SearchPrev | Action::Filter => {
                self.not_yet(action.help(), Some("25-search-and-filter.md"))
            }
            Action::Organize => self.not_yet(action.help(), Some("28-organize-command.md")),

            // -- only meaningful inside the tag editor ----------------------
            Action::EditField
            | Action::ClearField
            | Action::TitleFromFilename
            | Action::RenumberTracks
            | Action::StageTags
            | Action::StageAndCommit => {
                self.notify(
                    Level::Warn,
                    format!("{}: only in the tag editor", action.help()),
                );
                true
            }

            // -- only meaningful where there is a line of text --------------
            Action::Submit | Action::DeleteChar | Action::ClearLine => false,
        }
    }

    /// Do something to the browser, if there is a library for it to be about.
    ///
    /// The one place the two fields are borrowed together, and the reason every
    /// browser verb above is one line: a library that has not landed yet means
    /// the keys move nothing, which is right — there is nothing on screen to
    /// move through.
    fn browse<F>(&mut self, act: F) -> bool
    where
        F: FnOnce(&mut Browser, Pane, &Library) -> bool,
    {
        let Some(library) = &self.library else {
            return false;
        };
        let was = self.browser.dir().clone();
        let dirty = act(&mut self.browser, self.focus.pane(), library);
        if self.browser.dir() != &was {
            self.log
                .line(format!("browser: {}", self.browser.dir_label()));
        }
        dirty
    }

    /// `l` / `enter`: into a directory, across to the listing, or into a file —
    /// which is the tag editor's job and so is answered by naming it.
    fn open_action(&mut self) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        match self.browser.enter(self.focus.pane(), library) {
            Enter::Opened => {
                self.log
                    .line(format!("browser: {}", self.browser.dir_label()));
                true
            }
            Enter::ToFiles => {
                let moved = self.focus != Focus::Files;
                self.focus = Focus::Files;
                moved
            }
            Enter::File(_) => self.dispatch(Action::EditTags),
            Enter::Nothing => false,
        }
    }

    // -- the tag editor ----------------------------------------------------

    /// The editor on the stack, if it is there.
    fn tagedit(&self) -> Option<&TagEdit> {
        match self.views.last() {
            Some(View::TagEdit(form)) => Some(form),
            _ => None,
        }
    }

    /// The editor on the stack, to change.
    fn tagedit_mut(&mut self) -> Option<&mut TagEdit> {
        match self.views.last_mut() {
            Some(View::TagEdit(form)) => Some(form),
            _ => None,
        }
    }

    /// `e`: open the tag editor on the marks, or on the row under the cursor.
    ///
    /// The tags are **not** read here. Two hundred marked files is two hundred
    /// opens, so the form goes on screen saying what it is waiting for and a
    /// worker answers with [`TaskOutcome::Selection`].
    fn open_tag_editor(&mut self) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let (files, skipped) = tag_targets(&self.browser, library);
        if files.is_empty() {
            self.notify(
                Level::Warn,
                "nothing to edit: mark some audio files, or put the cursor on one",
            );
            return true;
        }

        let root = library.root().to_path_buf();
        self.log.line(format!(
            "tagedit: opening on {} file(s), {skipped} skipped",
            files.len()
        ));
        if skipped > 0 {
            // Not silent: a user who marked an album and its `.nfo` should be
            // told the `.nfo` was left out rather than left to wonder about the
            // count in the title.
            let plural = if skipped == 1 { "" } else { "s" };
            self.notify(
                Level::Warn,
                format!("{skipped} marked path{plural} hold no audio and were left out"),
            );
        }
        work::read_selection(self.tx.clone(), files.clone(), root, Arc::clone(&self.log));
        self.push(View::TagEdit(Box::new(TagEdit::opening(files))))
    }

    /// The tag editor's share of the actions, or `None` if it wants none of them.
    ///
    /// Three states, and they claim different verbs: a preview waiting to be
    /// answered, a field open for typing, and the form itself.
    fn tagedit_action(&mut self, action: Action) -> Option<bool> {
        let form = self.tagedit()?;
        if form.preview().is_some() {
            return Some(self.preview_action(action));
        }
        if form.is_editing() {
            return Some(self.field_action(action));
        }
        self.form_action(action)
    }

    /// A key while a per-file action's preview is on screen.
    ///
    /// Nothing else happens until it is answered: it is a question about every
    /// file in the selection, and a list that could be navigated away from
    /// without answering would be a preview nobody had to look at.
    fn preview_action(&mut self, action: Action) -> bool {
        let rows = self.preview_rows();
        let step = self.page_step();
        let mut applied = None;

        let dirty = {
            let Some(form) = self.tagedit_mut() else {
                return false;
            };
            match action {
                Action::Submit => {
                    let what = form.preview().map(|preview| preview.action);
                    let dirty = form.accept_preview();
                    applied = what;
                    dirty
                }
                Action::Cancel => form.cancel_preview(),
                Action::Down => form.scroll_preview(1, rows),
                Action::Up => form.scroll_preview(-1, rows),
                Action::HalfPageDown => form.scroll_preview(step, rows),
                Action::HalfPageUp => form.scroll_preview(-step, rows),
                Action::Top => form.scroll_preview(isize::MIN, rows),
                Action::Bottom => form.scroll_preview(isize::MAX, rows),
                _ => false,
            }
        };

        if let Some(what) = applied {
            self.notify(
                Level::Info,
                format!("{what}: in the form, not yet written · stage it to write it"),
            );
        }
        dirty
    }

    /// A key while a field is open for typing, already filtered down to the verbs
    /// that are about text ([`IN_FIELD`]).
    fn field_action(&mut self, action: Action) -> bool {
        let Some(form) = self.tagedit_mut() else {
            return false;
        };
        match action {
            // Both leave the field, keeping what was typed. They can, because
            // leaving a field writes nothing anywhere — the task is explicit
            // about that — so there is no asymmetry for two keys to express.
            Action::Submit | Action::Cancel => form.end(),
            // Accept the field and move on, which is what a form does.
            Action::Down => {
                form.end();
                form.move_cursor(1);
                true
            }
            Action::Up => {
                form.end();
                form.move_cursor(-1);
                true
            }
            Action::Left => form.input_mut().is_some_and(Input::left),
            Action::Right => form.input_mut().is_some_and(Input::right),
            Action::DeleteChar => form.input_mut().is_some_and(Input::backspace),
            Action::ClearLine => form.input_mut().is_some_and(Input::clear),
            _ => false,
        }
    }

    /// A key on the form itself, between fields.
    fn form_action(&mut self, action: Action) -> Option<bool> {
        let step = self.page_step();
        Some(match action {
            Action::Down => self.tagedit_mut()?.move_cursor(1),
            Action::Up => self.tagedit_mut()?.move_cursor(-1),
            Action::HalfPageDown => self.tagedit_mut()?.move_cursor(step),
            Action::HalfPageUp => self.tagedit_mut()?.move_cursor(-step),
            Action::Top => self.tagedit_mut()?.set_cursor(0),
            Action::Bottom => self.tagedit_mut()?.set_cursor(usize::MAX),
            Action::EditField | Action::Submit | Action::Open | Action::Right => self.begin_field(),
            Action::ClearField => self.tagedit_mut()?.clear_field(),
            Action::TitleFromFilename => self.file_action(FileAction::TitleFromFilename),
            Action::RenumberTracks => self.file_action(FileAction::RenumberTracks),
            Action::StageTags => self.stage_tags(false),
            Action::StageAndCommit => self.stage_tags(true),
            Action::Cancel => self.close_tag_editor(),
            // Everything else — the help, quitting, a browser verb somebody bound
            // in here — is the panel rules' business.
            _ => return None,
        })
    }

    /// `i`: start typing into the field under the cursor, or say why not.
    fn begin_field(&mut self) -> bool {
        let Some(form) = self.tagedit_mut() else {
            return false;
        };
        let field = form.field();
        match form.begin() {
            Begin::Opened => true,
            Begin::PerFile(action) => {
                // Named rather than merely refused: "every file wants its own
                // title" is half an answer without the key that does it.
                let key = self.keys.map().key_for(Mode::TagEdit, action_for(action));
                let how = match key {
                    Some(key) => format!("press {key} for `{action}`"),
                    None => format!("use `{action}`"),
                };
                self.notify(
                    Level::Warn,
                    format!("every file wants its own {field} — {how}"),
                );
                true
            }
            Begin::NotReady => false,
        }
    }

    /// `T` / `N`: work out what a per-file action would do, and show it.
    fn file_action(&mut self, action: FileAction) -> bool {
        let Some(form) = self.tagedit_mut() else {
            return false;
        };
        match form.start_action(action) {
            Started::Shown => true,
            Started::Nothing => {
                self.notify(
                    Level::Info,
                    format!("{action}: nothing to do · every file already says that"),
                );
                true
            }
            Started::NotReady => false,
        }
    }

    /// `esc` on the form: leave, asking first if there is anything to lose.
    fn close_tag_editor(&mut self) -> bool {
        let Some(form) = self.tagedit() else {
            return false;
        };
        if !form.is_modified() {
            return self.pop();
        }
        let fields: Vec<String> = form.modified().iter().map(ToString::to_string).collect();
        let files = form.deltas().len();
        self.push(View::Confirm(Confirm {
            question: format!(
                "{} not staged, across {files} file(s).\nThrow the changes away?",
                fields.join(", ")
            ),
            on_yes: Answer::DiscardEdits,
        }))
    }

    /// `w` / `W`: turn the form into operations and stage them.
    ///
    /// Four refusals, in this order, and **nothing is staged unless all four
    /// pass**: a value that will not parse, a per-file field typed across a
    /// selection, a form that would change nothing, and a plan that cannot be
    /// committed — which is where a file MPDFM may not write is reported, by
    /// name, before anything is staged.
    fn stage_tags(&mut self, and_commit: bool) -> bool {
        // Everything the form has to say, taken out of it in one borrow so that
        // the refusals below can put messages on screen.
        let Some(form) = self.tagedit() else {
            return false;
        };
        if form.is_loading() {
            return false;
        }
        let files = form.len();
        let errors: Vec<String> = form
            .errors()
            .iter()
            .map(|(field, why)| format!("{field}: {why}"))
            .collect();
        let per_file: Vec<String> = form
            .per_file_refused()
            .iter()
            .map(ToString::to_string)
            .collect();
        let fields: Vec<String> = form.modified().iter().map(ToString::to_string).collect();
        let deltas = form.deltas();

        if !errors.is_empty() {
            self.notify(
                Level::Warn,
                format!("nothing staged — {}", errors.join("; ")),
            );
            return true;
        }
        if !per_file.is_empty() {
            self.notify(
                Level::Warn,
                format!(
                    "nothing staged — {} cannot be one value across {files} files",
                    per_file.join(" and ")
                ),
            );
            return true;
        }
        if deltas.is_empty() {
            self.notify(
                Level::Info,
                "nothing to change: every selected file already says that",
            );
            return true;
        }

        let count = deltas.len();
        let mut planned = self.plan.clone();
        for (target, changes) in deltas {
            planned.push(Operation::WriteTags { target, changes });
        }

        // The preflight. A tag edit against a file MPDFM cannot write is a
        // `Conflict::NotTaggable`, which is how "a non-writable file in the
        // selection is reported before staging, naming it" is answered — by the
        // same validation the CLI and the pending view use, rather than by a
        // check this view invented for itself.
        let Some(effects) = self.preview_plan(&planned) else {
            return true;
        };
        if !effects.is_committable() {
            let reasons: Vec<String> = effects.conflicts.iter().map(ToString::to_string).collect();
            self.fail(format!(
                "nothing was staged — this edit cannot be committed:\n\n{}",
                reasons.join("\n")
            ));
            return true;
        }

        self.plan = planned;
        self.log
            .line(format!("tagedit: staged {count} tag edit(s)"));
        // The form's work is done: it has become operations, and the plan is
        // where those live now.
        self.pop();

        if and_commit {
            return self.commit_plan(effects);
        }
        let plural = if count == 1 { "" } else { "s" };
        self.notify(
            Level::Info,
            format!(
                "staged {}: {count} file{plural} · {} pending",
                fields.join(", "),
                self.plan.len()
            ),
        );
        true
    }

    /// What a plan would do, or the reason it cannot be worked out. Touches
    /// nothing.
    ///
    /// The library and the playlist index arrive together from a scan, so one
    /// without the other means no scan has landed and there is nothing to
    /// validate against yet.
    fn preview_plan(&mut self, plan: &Plan) -> Option<Effects> {
        let (Some(library), Some(index)) = (&self.library, &self.index) else {
            self.notify(Level::Warn, "no library yet · rescan first");
            return None;
        };
        // `validate_live` and not `validate`: whether MPD is holding a queue
        // decides whether its saved queue is rewritten or merely warned about,
        // and the commit is given the same answer (`App::queue`).
        Some(plan.validate_live(library, index, &self.config, &self.live()))
    }

    /// Commit everything staged, on a worker.
    ///
    /// `effects` is what the user was shown and agreed to. Commit re-validates
    /// and refuses as drift if the answer has changed since, which is what makes
    /// handing a worker a clone of the model safe.
    fn commit_plan(&mut self, effects: Effects) -> bool {
        if self.running.is_some() {
            self.notify(Level::Warn, "a transaction is already running");
            return true;
        }
        let Some(library) = self.library.clone() else {
            return false;
        };
        let ops = self.plan.len();
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(Running {
            what: "committing",
            ops,
            progress: None,
            cancel: Arc::clone(&cancel),
            cancelling: false,
        });
        self.log
            .line(format!("commit: starting, {ops} operation(s)"));
        work::commit(
            self.tx.clone(),
            work::CommitJob {
                plan: self.plan.clone(),
                library,
                effects,
                config: self.config.clone(),
                // The same answer the preview was given, or commit refuses as
                // drift. See `App::queue`.
                queue: self.queue.clone(),
                cancel,
                log: Arc::clone(&self.log),
            },
        );
        true
    }

    /// `u`: reverse a committed transaction — the one the pending view has just
    /// made, or the most recent undoable one.
    ///
    /// `txid` is what makes the offer after a commit honest: it names the
    /// transaction the user is looking at, not whichever happens to be newest by
    /// the time they press the key.
    fn undo_last(&mut self, txid: Option<String>) -> bool {
        if self.running.is_some() {
            self.notify(Level::Warn, "a transaction is already running");
            return true;
        }
        let what = txid.clone().map_or_else(
            || "the last transaction".to_owned(),
            |txid| format!("transaction {txid}"),
        );
        self.running = Some(Running {
            what: "undoing",
            ops: 0,
            progress: None,
            cancel: Arc::new(AtomicBool::new(false)),
            cancelling: false,
        });
        self.log.line(format!("undo: starting on {what}"));
        work::undo(
            self.tx.clone(),
            self.config.clone(),
            txid,
            Arc::clone(&self.log),
        );
        self.notify(Level::Info, format!("undoing {what}…"));
        true
    }

    // -- staging, and the view that shows what is staged -------------------

    /// The pending view on the stack, if it is there.
    fn pending(&self) -> Option<&Pending> {
        match self.views.last() {
            Some(View::Pending(view)) => Some(view),
            _ => None,
        }
    }

    /// The pending view on the stack, to change.
    fn pending_mut(&mut self) -> Option<&mut Pending> {
        match self.views.last_mut() {
            Some(View::Pending(view)) => Some(view),
            _ => None,
        }
    }

    /// The pending view's share of the actions, or `None` if it wants none of
    /// them.
    ///
    /// `d` is the reason this exists and sits beside the tag editor's: in here it
    /// takes an operation off the plan, and everywhere else it stages a delete.
    /// One mode, one meaning, and no `match` on a `KeyCode` anywhere.
    fn pending_action(&mut self, action: Action) -> Option<bool> {
        self.pending()?;
        let rows = self.pending_rows();
        let step = self.page_step();
        Some(match action {
            Action::Down => self.pending_mut()?.move_cursor(1, rows),
            Action::Up => self.pending_mut()?.move_cursor(-1, rows),
            Action::HalfPageDown => self.pending_mut()?.move_cursor(step, rows),
            Action::HalfPageUp => self.pending_mut()?.move_cursor(-step, rows),
            Action::Top => self.pending_mut()?.set_cursor(0, rows),
            Action::Bottom => self.pending_mut()?.set_cursor(usize::MAX, rows),
            // Unfolding: `enter` either way, and the two directions for a user
            // who thinks of it as a tree.
            Action::Open | Action::Submit => self.pending_mut()?.toggle(),
            Action::Right => self.pending_mut()?.expand(),
            Action::Left => self.pending_mut()?.collapse(),

            Action::Unstage => self.unstage_selected(),
            Action::Commit => self.commit_pending(),
            Action::DiscardPending => self.discard_pending(),
            // The transaction this view has just made, by name.
            Action::Undo => {
                let txid = match self.pending()?.report() {
                    Some(Report::Done { txid, .. }) => Some(txid.clone()),
                    _ => None,
                };
                self.undo_last(txid)
            }
            // `p` is the key that opened it, so it is the key that closes it.
            Action::ShowPending => self.pop(),
            Action::Cancel => self.leave_pending(),
            // The help, quitting, and anything a user has bound in here that
            // this view has no opinion about: the panel rules answer those.
            _ => return None,
        })
    }

    /// `esc` in the pending view: call off a commit, dismiss a report, or leave.
    ///
    /// In that order, because that is the order of what the user is looking at.
    fn leave_pending(&mut self) -> bool {
        if self.cancel_commit() {
            return true;
        }
        if let Some(view) = self.pending_mut()
            && view.report().is_some()
        {
            view.dismiss();
            // A report about a transaction that emptied the plan has nothing
            // left behind it, so dismissing it leaves the view as well.
            return if self.plan.is_empty() {
                self.pop()
            } else {
                true
            };
        }
        self.pop()
    }

    /// Ask a running commit to stop, if it still can.
    ///
    /// Returns whether there was anything to ask. A transaction that is past
    /// re-validation has already taken backups and written a record, and the way
    /// out of that one is `mpdfm recover` — so this says so rather than setting a
    /// flag nothing will read.
    fn cancel_commit(&mut self) -> bool {
        let Some(running) = &mut self.running else {
            return false;
        };
        if !running.is_cancellable() {
            self.notify(
                Level::Warn,
                "too late to stop: the transaction is past the point where nothing had changed",
            );
            return true;
        }
        running.cancel.store(true, Ordering::Relaxed);
        running.cancelling = true;
        self.log.line("commit: cancellation asked for");
        self.notify(Level::Info, "stopping the commit…");
        true
    }

    /// `p`: show what is staged, re-validated as of now.
    fn show_pending(&mut self) -> bool {
        if matches!(self.views.last(), Some(View::Pending(_))) {
            return self.pop();
        }
        if self.plan.is_empty() {
            self.notify(Level::Info, "nothing staged");
            return true;
        }
        let plan = self.plan.clone();
        let Some(effects) = self.preview_plan(&plan) else {
            return true;
        };
        self.push(View::Pending(Box::new(Pending::new(effects))))
    }

    /// Stage these operations and show what they would do.
    ///
    /// Staged first and previewed second, which is the opposite of the tag
    /// editor's order and deliberately so: a tag edit that cannot be committed is
    /// refused before it is staged because the form is still open and the user
    /// can fix it there, while a move that conflicts has nowhere else to be
    /// fixed. The pending view *is* where it is fixed — the conflict is on the
    /// row, and `dd` takes it off.
    fn stage(&mut self, ops: Vec<Operation>, what: &str) -> bool {
        let count = ops.len();
        let mut planned = self.plan.clone();
        for op in ops {
            planned.push(op);
        }
        let Some(effects) = self.preview_plan(&planned) else {
            return true;
        };

        self.plan = planned;
        self.log.line(format!(
            "plan: staged {count} {what} op(s), {} total",
            self.plan.len()
        ));
        let conflicts = effects.conflicts.len();
        match self.pending_mut() {
            Some(view) => {
                view.dismiss();
                view.revalidated(effects);
            }
            None => {
                self.push(View::Pending(Box::new(Pending::new(effects))));
            }
        }
        if conflicts > 0 {
            self.notify(
                Level::Warn,
                format!(
                    "staged {count} {what} op(s) — {conflicts} conflict(s): nothing will commit until they are gone"
                ),
            );
        } else {
            self.notify(
                Level::Info,
                format!("staged {count} {what} op(s) · {} pending", self.plan.len()),
            );
        }
        true
    }

    /// What a staged move or delete is about: the marks, or the row the cursor is
    /// on.
    ///
    /// The same rule as the tag editor's, and the same reason: a user who has
    /// marked nothing means the thing they are looking at.
    fn targets(&self) -> Vec<RelPath> {
        let Some(library) = &self.library else {
            return Vec::new();
        };
        let mut marks = self.browser.marks();
        if marks.is_empty() {
            marks.extend(self.browser.focused_path(library));
        }
        marks
    }

    /// `m`: move the marks into the directory the browser is showing.
    ///
    /// Mark, walk to where they belong, press the key — which is the gesture a
    /// file manager has and a `move` command does not. `:move <dst>` is the other
    /// door, and the one that can rename.
    fn stage_move(&mut self) -> bool {
        let dir = self.browser.dir().clone();
        let targets = self.targets();
        if targets.is_empty() {
            self.notify(
                Level::Warn,
                "nothing to move: mark something, or put the cursor on it",
            );
            return true;
        }
        match self.move_ops_into(&dir, &targets) {
            Ok(ops) => self.stage_moves(ops),
            Err(message) => {
                self.notify(Level::Warn, message);
                true
            }
        }
    }

    /// `r`: rename the row under the cursor, by opening `:move` on its own path.
    ///
    /// The command line and not a prompt of its own: the destination of a rename
    /// is a path, editing a path is what that line does, and `:move` is already
    /// the thing that stages one. A user who changes their mind presses `esc`.
    fn rename(&mut self) -> bool {
        let Some(library) = &self.library else {
            return false;
        };
        let Some(target) = self.browser.focused_path(library) else {
            self.notify(Level::Warn, "nothing under the cursor to rename");
            return true;
        };
        self.push(View::Command(CommandLine::of(format!("move {target}"))))
    }

    /// `:move <dst>` — one source to that exact path, several into that
    /// directory.
    ///
    /// The distinction is what makes the command a rename as well: with one
    /// thing selected, the destination the user typed is the destination, and
    /// with several there is nothing else `dst` could sensibly be.
    fn stage_move_to(&mut self, dst: &str) -> bool {
        let targets = self.targets();
        if targets.is_empty() {
            self.notify(
                Level::Warn,
                "nothing to move: mark something, or put the cursor on it",
            );
            return true;
        }

        let ops = match targets.as_slice() {
            [only] => {
                let Some(library) = &self.library else {
                    return false;
                };
                RelPath::parse(dst)
                    .map(|to| vec![move_op(library, only, to)])
                    .map_err(|err| format!("{dst}: {err}"))
            }
            many => DirPath::parse(dst)
                .map_err(|err| format!("{dst}: {err}"))
                .and_then(|dir| self.move_ops_into(&dir, many)),
        };
        match ops {
            Ok(ops) => self.stage_moves(ops),
            Err(message) => {
                self.notify(Level::Warn, message);
                true
            }
        }
    }

    /// Each target moved into `dir`, keeping its own name.
    fn move_ops_into(&self, dir: &DirPath, targets: &[RelPath]) -> Result<Vec<Operation>, String> {
        let Some(library) = &self.library else {
            return Ok(Vec::new());
        };
        targets
            .iter()
            .map(|target| {
                dir.join(target.file_name())
                    .map(|to| move_op(library, target, to))
                    .map_err(|err| format!("{}: {err}", target.file_name()))
            })
            .collect()
    }

    /// Stage these moves, leaving out the ones that would not move anything.
    ///
    /// A move of something onto itself is not a conflict to be shown, it is a
    /// keypress that meant nothing — most often `m` in the directory the marks
    /// are already in.
    fn stage_moves(&mut self, mut ops: Vec<Operation>) -> bool {
        ops.retain(|op| op.destination().is_none_or(|to| to != op.source()));
        if ops.is_empty() {
            self.notify(Level::Info, "already there: nothing to move");
            return true;
        }
        self.stage(ops, "move")
    }

    /// `d`: stage a delete of the marks.
    ///
    /// Files only. A delete is per file in the journal, because the unit of
    /// reversal is the file, and a marked *directory* is left out rather than
    /// quietly expanded into everything under it — deleting a tree is not a
    /// thing to infer from one keystroke. `delete_enabled` is what decides
    /// whether any of it can be committed, and the pending view is where that
    /// shows.
    fn stage_delete(&mut self) -> bool {
        let targets = self.targets();
        if targets.is_empty() {
            self.notify(
                Level::Warn,
                "nothing to delete: mark something, or put the cursor on it",
            );
            return true;
        }
        let Some(library) = &self.library else {
            return false;
        };

        let mut ops = Vec::new();
        let mut skipped = 0;
        for target in targets {
            if library.get(&target).is_some() {
                ops.push(Operation::Delete { target });
            } else {
                skipped += 1;
            }
        }
        if ops.is_empty() {
            self.notify(
                Level::Warn,
                "nothing staged: a delete is per file, and no file is marked",
            );
            return true;
        }
        if skipped > 0 {
            let plural = if skipped == 1 { "y was" } else { "ies were" };
            self.notify(
                Level::Warn,
                format!("{skipped} marked director{plural} left out: a delete is per file"),
            );
        }
        self.stage(ops, "delete")
    }

    /// `dd`: take the operation under the cursor off the plan.
    ///
    /// Re-validates afterwards, which is not housekeeping: dropping one
    /// operation can make a conflict disappear — the two that wanted the same
    /// destination — and can equally make one appear, when the operation that
    /// was going to vacate a directory is the one that went.
    fn unstage_selected(&mut self) -> bool {
        let Some(view) = self.pending() else {
            return false;
        };
        if view.report().is_some() {
            self.notify(Level::Info, "that transaction is already committed");
            return true;
        }
        let ops = view.selected_ops();
        if ops.is_empty() {
            self.notify(Level::Warn, "no operation under the cursor");
            return true;
        }

        // Descending, so that removing one does not move the next.
        let mut dropped = Vec::new();
        for &at in ops.iter().rev() {
            if let Some(op) = self.plan.remove(at) {
                dropped.push(op);
            }
        }
        self.log
            .line(format!("plan: dropped {} operation(s)", dropped.len()));

        if self.plan.is_empty() {
            self.notify(Level::Info, "nothing staged");
            self.pop();
            return true;
        }
        self.revalidate();
        let count = dropped.len();
        let plural = if count == 1 { "" } else { "s" };
        self.notify(
            Level::Info,
            format!(
                "dropped {count} operation{plural} · {} pending",
                self.plan.len()
            ),
        );
        true
    }

    /// `x`: throw the whole plan away, once the user has said so.
    fn discard_pending(&mut self) -> bool {
        if self.plan.is_empty() {
            self.notify(Level::Info, "nothing staged");
            return true;
        }
        let count = self.plan.len();
        let plural = if count == 1 { "" } else { "s" };
        self.push(View::Confirm(Confirm {
            question: format!("{count} staged operation{plural} will be thrown away."),
            on_yes: Answer::DiscardPlan,
        }))
    }

    /// Throw the plan away, which is what saying yes to that question means.
    fn discard_plan(&mut self) -> bool {
        let count = self.plan.len();
        self.plan = Plan::new();
        self.log
            .line(format!("plan: discarded {count} operation(s)"));
        if matches!(self.views.last(), Some(View::Pending(_))) {
            self.views.pop();
        }
        let plural = if count == 1 { "" } else { "s" };
        self.notify(
            Level::Info,
            format!("discarded {count} staged operation{plural}"),
        );
        true
    }

    /// `c`: commit what is staged, once it is clear that it can be.
    fn commit_pending(&mut self) -> bool {
        if self.running.is_some() {
            self.notify(Level::Warn, "a transaction is already running");
            return true;
        }
        if self.plan.is_empty() {
            self.notify(Level::Info, "nothing staged");
            return true;
        }
        // What the user is looking at, when they are looking at something:
        // `c` commits the preview on screen and not a fresh one that may have
        // moved under it. Commit re-validates anyway and refuses as drift if the
        // disk no longer matches, which is the honest outcome — and better than
        // silently committing a plan nobody read.
        let plan = self.plan.clone();
        let effects = match self.pending() {
            Some(view) if view.report().is_none() => view.effects().clone(),
            _ => {
                let Some(effects) = self.preview_plan(&plan) else {
                    return true;
                };
                effects
            }
        };

        if !effects.is_committable() {
            // Shown, not described: the view puts the cursor on the first
            // refused operation with the reason unfolded under it.
            let conflicts = effects.conflicts.len();
            match self.pending_mut() {
                Some(view) => view.revalidated(effects),
                None => {
                    self.push(View::Pending(Box::new(Pending::new(effects))));
                }
            }
            self.notify(
                Level::Warn,
                if conflicts == 0 {
                    "there is nothing to do".to_owned()
                } else {
                    format!("not committed: {conflicts} conflict(s) to deal with first")
                },
            );
            return true;
        }

        // Shown before it runs, so that a commit the user started from the
        // browser has the preview and the progress in front of them.
        if self.pending().is_none() {
            self.push(View::Pending(Box::new(Pending::new(effects.clone()))));
        } else if let Some(view) = self.pending_mut() {
            view.dismiss();
        }
        self.commit_plan(effects)
    }

    /// Re-validate the staged plan and hand the answer to the open view.
    ///
    /// The whole of how this view stays honest: nothing patches [`Effects`], and
    /// every change to the plan or to the library comes back through here.
    fn revalidate(&mut self) -> bool {
        let plan = self.plan.clone();
        let (Some(library), Some(index)) = (&self.library, &self.index) else {
            return false;
        };
        let effects = plan.validate_live(library, index, &self.config, &self.live());
        match self.pending_mut() {
            Some(view) => {
                view.revalidated(effects);
                true
            }
            None => false,
        }
    }

    /// What MPD is holding, as the preview and the commit must both be told.
    fn live(&self) -> Live<'_> {
        Live {
            queue: self.queue.as_deref(),
        }
    }

    /// How many rows the pending view's body has, from the last size the
    /// terminal reported.
    ///
    /// An estimate, like [`App::list_rows`]: it decides how far a half-page jump
    /// goes, and the frame itself measures the real pane.
    fn pending_rows(&self) -> usize {
        usize::from(self.body().height.saturating_sub(2)).max(1)
    }

    /// The keys the pending view's own text names, read off the live keymap.
    fn pending_hints(&self) -> pending::Hints {
        let key = |action| self.keys.map().key_for(Mode::Pending, action);
        pending::Hints {
            commit: key(Action::Commit),
            drop: key(Action::Unstage),
            discard: key(Action::DiscardPending),
            undo: key(Action::Undo),
            expand: key(Action::Open),
            back: key(Action::Cancel),
        }
    }

    /// How many rows a preview box has room for, from the last size the terminal
    /// reported.
    ///
    /// An estimate, like [`App::list_rows`], and allowed to be: it decides how
    /// far one keypress scrolls, and the frame itself measures the real box.
    fn preview_rows(&self) -> usize {
        usize::from(self.body().height)
            .saturating_mul(PREVIEW_PERCENT)
            .saturating_div(100)
            .saturating_sub(2)
            .max(1)
    }

    /// The keys the tag editor's own text names, read off the live keymap.
    fn tagedit_hints(&self) -> Hints {
        let key = |action| self.keys.map().key_for(Mode::TagEdit, action);
        Hints {
            titles: key(Action::TitleFromFilename),
            renumber: key(Action::RenumberTracks),
            clear: key(Action::ClearField),
            stage: key(Action::StageTags),
            commit: key(Action::StageAndCommit),
            accept: key(Action::Submit),
            cancel: key(Action::Cancel),
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
            Command::Move { dst } => self.stage_move_to(&dst),
            Command::Organize { template } => self.not_yet(
                format!("organize by {template}"),
                Some("28-organize-command.md"),
            ),
            Command::Undo { txid } => self.undo_last(txid),
            Command::Doctor => self.not_yet("doctor", Some("29-doctor.md")),
            Command::Set { key, value } => self.set_setting(&key, &value),
        }
    }

    /// Apply `:set <key>=<value>`, or say why it could not be.
    ///
    /// One key so far. `sort` is here and not in `config.toml` on purpose: it is
    /// a property of this session's browsing, the task asks for it to be
    /// remembered *per session*, and a setting written to a file would outlive
    /// the reason somebody chose it. Everything else `:set` takes still parses
    /// and still has nowhere to go, which is what it says.
    fn set_setting(&mut self, key: &str, value: &str) -> bool {
        if key != "sort" {
            return self.not_yet(
                format!("set {key}={value}: nothing applies that setting"),
                None,
            );
        }
        let Some(sort) = Sort::parse(value) else {
            self.notify(
                Level::Warn,
                format!("no sort called `{value}`; try {}", Sort::names()),
            );
            return true;
        };
        self.browser.set_sort(sort, self.library.as_ref());
        self.notify(Level::Info, format!("sorting by {sort}"));
        true
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
                on_yes: Answer::Act(Action::ForceQuit),
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
            // The queue is only worth a round trip when something is staged:
            // it is the one part of a poll whose cost is the length of the
            // user's queue, and nothing but a preview reads it.
            work::poll_mpd(
                self.tx.clone(),
                self.config.clone(),
                !self.plan.is_empty(),
                Arc::clone(&self.log),
            );
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
                self.warnings = warnings;
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
                // Where the browser was may not exist any more, and whatever it
                // had cached is about a library that has just been replaced.
                if let Some(library) = &self.library {
                    self.browser.library_changed(library, self.index.as_ref());
                }
                self.tags_in_flight = false;
                // The preview was worked out against the library that has just
                // been replaced, which includes the one a commit's own rescan
                // replaces. Nothing is patched; it is made again.
                if self.pending().is_some() {
                    self.revalidate();
                }

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

        // The live queue decides whether a staged move rewrites MPD's saved
        // queue or warns that the daemon will need a requeue, so a queue that
        // has changed under an open preview is a preview that has to be made
        // again. Rare — the user has to have touched MPD while reading it —
        // which is why this is the one thing a poll can cost a re-validation.
        let requeued = self.queue != snapshot.queue;
        self.queue = snapshot.queue.clone();
        self.mpd = Some(snapshot);
        if requeued && self.pending().is_some() && self.revalidate() {
            return true;
        }
        changed
    }

    /// A commit said how far it has got.
    ///
    /// No state a key can act on changes here: it is a line on screen, and the
    /// one thing it decides is whether `esc` can still call the commit off.
    fn on_committing(&mut self, progress: Progress) -> bool {
        let Some(running) = &mut self.running else {
            // The answer arrived before the last of the progress, which is
            // possible: two sends, one channel, and the loop drains it.
            return false;
        };
        let changed = running.progress != Some(progress);
        running.progress = Some(progress);
        changed
    }

    /// A worker that was not a scan came back.
    fn on_task_done(&mut self, outcome: TaskOutcome) -> bool {
        match outcome {
            TaskOutcome::Tags(reads) => {
                self.tags_in_flight = false;
                // A failure here is per-file and already in the row's own slot:
                // it shows as `?` in the listing and as the reason in the
                // details pane. A panel for one unreadable track in a directory
                // of fourteen would be a modal dialogue nobody asked for.
                self.browser.tags_arrived(reads)
            }
            TaskOutcome::Selection(reads) => self.on_selection(reads),
            TaskOutcome::Committed(result) => self.on_committed(result),
            TaskOutcome::Undone(result) => self.on_undone(result),
            TaskOutcome::Failed { what, message } => {
                self.tags_in_flight = false;
                self.fail(format!("{what}: {message}"));
                true
            }
        }
    }

    /// The tag editor's files have been read.
    ///
    /// A file that would not read refuses the whole form, which is the rule
    /// `mpdfm tag set` follows and for the same reason: a bulk view of nine of
    /// ten files answers a question nobody asked, and `<multiple>` computed over
    /// a selection that is missing a file is a lie about that selection.
    fn on_selection(&mut self, reads: Reads) -> bool {
        // The user may have left while the read was out, in which case the answer
        // is of no interest — the form it was for is gone.
        let Some(form) = self.tagedit_mut() else {
            return false;
        };
        match form.arrived(reads) {
            Ok(()) => true,
            Err(unreadable) => {
                self.views.pop();
                let list: Vec<String> = unreadable
                    .iter()
                    .map(|message| format!("  - {message}"))
                    .collect();
                self.fail(format!(
                    "these file(s) could not be read, so the editor was not opened:\n\n{}",
                    list.join("\n")
                ));
                true
            }
        }
    }

    /// A commit finished.
    ///
    /// The plan is emptied only on success: a refused commit wrote nothing and
    /// the operations are still what the user staged, which is what they need in
    /// order to fix whatever was wrong.
    fn on_committed(
        &mut self,
        result: Result<Box<mpdfm_core::ops::commit::Committed>, NotCommitted>,
    ) -> bool {
        self.running = None;
        match result {
            Ok(committed) => {
                self.plan = Plan::new();
                let warnings: Vec<String> =
                    committed.warnings.iter().map(ToString::to_string).collect();
                let report = Report::Done {
                    txid: committed.txid.to_string(),
                    headline: committed.headline(),
                    mpd: pending::Mpd::of(&committed.record),
                    warnings: warnings.clone(),
                };

                // The view that was watching keeps the txid, the warnings and
                // the offer to undo, because every one of those is something to
                // act on rather than to notice. Without one — `W` from the tag
                // editor — they go on the message line instead.
                match self.pending_mut() {
                    Some(view) => view.finished(report),
                    None => {
                        for warning in warnings {
                            self.notify(Level::Warn, warning);
                        }
                        let undo = self
                            .keys
                            .map()
                            .key_for(Mode::Browser, Action::Undo)
                            .map_or_else(String::new, |key| format!(" \u{b7} {key} to undo"));
                        self.notify(Level::Info, format!("{}{undo}", committed.headline()));
                    }
                }
                // The library on screen is a version behind: the files moved, or
                // their tags changed, and the browser drops its tag cache on a
                // rescan. This is what makes the change visible immediately
                // rather than on the next keypress that happens to re-read
                // something.
                self.rescan();
                true
            }
            // Nothing was written, and the plan is still staged — so the view
            // says so and stays on what the user was looking at.
            Err(NotCommitted::Cancelled) => {
                match self.pending_mut() {
                    Some(view) => view.finished(Report::Cancelled),
                    None => self.notify(Level::Info, "the commit was called off"),
                }
                true
            }
            // Core's message, whole: which step stopped it, and the
            // `mpdfm recover` that puts it back.
            Err(NotCommitted::Failed(message)) => {
                match self.pending_mut() {
                    Some(view) => view.finished(Report::Failed { message }),
                    None => self.fail(message),
                }
                // Whatever did happen, happened: the browser is a version behind
                // either way.
                self.rescan();
                true
            }
        }
    }

    /// An undo finished.
    fn on_undone(&mut self, result: Result<Box<mpdfm_core::journal::Reversed>, String>) -> bool {
        self.running = None;
        match result {
            Ok(reversed) => {
                for warning in &reversed.warnings {
                    self.notify(Level::Warn, warning.to_string());
                }
                self.notify(Level::Info, reversed.headline());
                // Back to the browser, which is where the reversal is visible:
                // a report about a transaction that no longer stands is not
                // something to leave on screen.
                if matches!(self.views.last(), Some(View::Pending(_))) {
                    self.views.pop();
                }
                self.rescan();
                true
            }
            // Including "there is nothing to undo", which is a thing to read
            // rather than a thing to notice: it means the journal is not what the
            // user thought it was.
            Err(message) => {
                self.fail(message);
                true
            }
        }
    }

    /// Start reading the tags of any visible row that has none yet.
    ///
    /// Nothing here opens a file: [`Browser::wanted`] names the paths and
    /// `work::read_tags` is the thread that reads them, which is the same
    /// division the scan uses. One batch at a time — a held-down `j` would
    /// otherwise start a thread per row — and the answer is what asks for the
    /// next one, so scrolling fast coalesces into a few large reads instead of
    /// many small ones.
    fn request_tags(&mut self) {
        if self.tags_in_flight {
            return;
        }
        let Some(library) = &self.library else {
            return;
        };
        let rows = self.list_rows();
        let wanted = self.browser.wanted(library, rows);
        if wanted.is_empty() {
            return;
        }
        self.tags_in_flight = true;
        work::read_tags(
            self.tx.clone(),
            wanted,
            library.root().to_path_buf(),
            Arc::clone(&self.log),
        );
    }

    /// How many rows the listing has room for, from the last size the terminal
    /// reported.
    ///
    /// An estimate, and allowed to be: it decides how many tags are read ahead,
    /// and the frame itself measures the real area. The two differ by one row
    /// when a command error is on screen, which costs one extra file read and
    /// nothing else.
    fn list_rows(&self) -> usize {
        usize::from(self.size.1.saturating_sub(CHROME_ROWS + 2))
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

    /// The browser: the tree, the listing, and — when there is room — the
    /// details of whatever the listing's cursor is on.
    ///
    /// Everything a widget needs is derived here, into owned values, from a
    /// library that is borrowed for the length of this call and no longer.
    /// Nothing is mutated: the scroll offsets the next frame starts from are
    /// recorded by [`App::remember_scroll`], which the loop calls after the draw.
    fn render_browser(&self, area: Rect, frame: &mut ratatui::Frame) {
        let Some(library) = &self.library else {
            let text = match &self.scan {
                ScanState::Running(_) => "scanning…",
                _ => "no library yet",
            };
            frame.render_widget(
                Paragraph::new(text).block(Block::new().borders(Borders::ALL).title(" / ")),
                area,
            );
            return;
        };

        let [tree_area, files_area, details_area] = panes(area);
        // The listing is the only pane whose height decides how much work is
        // done, which is what makes the whole view virtualized: `rows` returns
        // this many rows and the widget cannot look past them.
        let height = usize::from(files_area.height.saturating_sub(2));

        self.render_tree(tree_area, library, frame);
        self.render_files(files_area, library, height, frame);
        if details_area.width > 0 {
            frame.render_widget(
                DetailsPane::new(&self.browser.details(library, self.index.as_ref()))
                    .block(Block::new().borders(Borders::ALL).title(" details ")),
                details_area,
            );
        }
    }

    /// The directory tree, indented, with the current directory on the cursor.
    fn render_tree(&self, area: Rect, library: &Library, frame: &mut ratatui::Frame) {
        let height = usize::from(area.height.saturating_sub(2));
        let shown = self.browser.tree(library, height);
        let block = Block::new()
            .borders(Borders::ALL)
            .title(format!(" {} ", fit(root_name(library), inner_width(area))));

        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 {
            return;
        }

        let cells = usize::from(inner.width);
        let lines: Vec<Line<'static>> = shown
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let mut line = Line::from(vec![
                    Span::styled(
                        pad(if row.referenced { "⚠" } else { "" }, 2),
                        Style::new().fg(Color::Magenta),
                    ),
                    Span::raw(pad(&tree_label(row), cells.saturating_sub(2))),
                ]);
                if shown.cursor == Some(index) {
                    line = line.style(if self.focus == Focus::Tree {
                        Style::new().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::new().add_modifier(Modifier::BOLD)
                    });
                }
                line
            })
            .collect();
        Paragraph::new(lines).render(inner, frame.buffer_mut());
    }

    /// The listing, or the reason there is nothing in it.
    fn render_files(
        &self,
        area: Rect,
        library: &Library,
        height: usize,
        frame: &mut ratatui::Frame,
    ) {
        let shown = self.browser.rows(library, height);
        let title = format!(" {} ", fit(&self.browser.dir_label(), inner_width(area)));
        let footer = if shown.total == 0 {
            String::new()
        } else {
            format!(" {}/{} ", self.browser.cursor() + 1, shown.total)
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(footer);

        if shown.total == 0 {
            // Not "nothing here": an empty directory and one the scan could not
            // read look identical in the model, and which of the two it is, is
            // the thing a user needs to know.
            let text = match &self.scan {
                ScanState::Running(_) => "scanning…".to_owned(),
                _ => self.browser.empty_reason(library),
            };
            frame.render_widget(Paragraph::new(text).block(block), area);
            return;
        }

        // The visual range is in listing coordinates; the widget's are window
        // coordinates, so it is shifted and clipped to what is on screen.
        let range = self.browser.visual_range().and_then(|(from, to)| {
            let start = shown.range.start;
            (to >= start).then(|| {
                (
                    from.saturating_sub(start),
                    (to - start).min(shown.rows.len().saturating_sub(1)),
                )
            })
        });

        frame.render_widget(
            FileList::new(&shown.rows)
                .cursor(shown.cursor)
                .range(range)
                .focused(self.focus == Focus::Files)
                .block(block),
            area,
        );
    }

    /// Record where each pane ended up scrolled to, after a frame.
    ///
    /// Separate from the draw because the draw takes `&self`: a render that
    /// mutated the state it renders from is a render whose output depends on how
    /// many times it has been called.
    fn remember_scroll(&mut self) {
        let Some(library) = &self.library else {
            return;
        };
        let [tree_area, files_area, _] = panes(self.body());
        let tree = self
            .browser
            .tree(library, usize::from(tree_area.height.saturating_sub(2)))
            .range
            .start;
        let files = self
            .browser
            .rows(library, usize::from(files_area.height.saturating_sub(2)))
            .range
            .start;
        self.browser.scrolled(tree, files);
    }

    /// The area the body occupies, from the last size the terminal reported.
    fn body(&self) -> Rect {
        Rect::new(0, 1, self.size.0, self.size.1.saturating_sub(CHROME_ROWS))
    }

    /// An overlay over the body.
    fn render_overlay(&self, view: &View, body: Rect, frame: &mut ratatui::Frame) {
        match view {
            // Drawn on the bottom line, by `render_message`: a command line that
            // covered the listing would hide what the command is about.
            View::Browser | View::Command(_) => {}
            View::Help { mode, scroll } => self.render_help(*mode, *scroll, body, frame),
            View::TagEdit(form) => self.render_tagedit(form, body, frame),
            View::Pending(view) => self.render_pending(view, body, frame),
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

    /// The tag editor: the form, and the preview of a per-file action over it.
    ///
    /// The whole body and not a centred box. It is not a dialogue — the task calls
    /// it "the form the user will spend most of their time in" — and ten fields
    /// plus the actions and the modified line do not fit in three quarters of a
    /// 24-row terminal.
    fn render_tagedit(&self, form: &TagEdit, body: Rect, frame: &mut ratatui::Frame) {
        let hints = self.tagedit_hints();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Cyan))
            .title(fit(&form.title(), inner_width(body)))
            .title_bottom(fit(&form.footer(&hints), inner_width(body)));

        let inner = block.inner(body);
        frame.render_widget(Clear, body);
        frame.render_widget(block, body);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let cells = usize::from(inner.width);
        Paragraph::new(form.lines(cells, &hints)).render(inner, frame.buffer_mut());

        // The terminal's own cursor, where the next character goes — only when
        // this form is the top view, so a help overlay pushed over it does not
        // get a cursor sitting on it.
        if matches!(self.views.last(), Some(View::TagEdit(_)))
            && let Some((column, row)) = form.caret(cells, &hints)
            && row < inner.height
        {
            frame.set_cursor_position((
                inner
                    .x
                    .saturating_add(column.min(inner.width.saturating_sub(1))),
                inner.y.saturating_add(row),
            ));
        }

        if let Some(preview) = form.preview() {
            self.render_preview(preview, body, frame, &hints);
        }
    }

    /// The pending view: what is staged, and what a running commit is doing
    /// about it.
    ///
    /// The whole body, like the tag editor and for the same reason: this is not a
    /// dialogue to dismiss but the screen where the user reads a plan, and a plan
    /// does not fit in three quarters of a 24-row terminal.
    ///
    /// The footer is the shell's: while a transaction is running it says so, and
    /// says whether it can still be stopped — which is a fact about the worker
    /// and not about the view.
    fn render_pending(&self, view: &Pending, body: Rect, frame: &mut ratatui::Frame) {
        let footer = match &self.running {
            Some(running) => {
                let stop = if running.is_cancellable() {
                    self.keys
                        .map()
                        .key_for(Mode::Pending, Action::Cancel)
                        .map_or_else(String::new, |key| format!(" \u{b7} {key} to stop"))
                } else {
                    String::new()
                };
                format!(" {}{stop} ", running.line())
            }
            None => view.footer(&self.pending_hints()),
        };
        // The border is the first thing read, so it says the one thing that
        // matters most: whether anything is going to happen. A plan that will
        // not commit is in the same red as the conflicts that refuse it.
        let border = match (&self.running, view.report()) {
            (Some(_), _) => Color::Cyan,
            (None, Some(Report::Done { .. })) => Color::Green,
            (None, Some(Report::Failed { .. })) => Color::Red,
            (None, Some(Report::Cancelled)) => Color::Yellow,
            (None, None) if view.is_committable() => Color::Green,
            (None, None) => Color::Red,
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(border))
            .title(fit(&view.title(), inner_width(body)))
            .title_bottom(fit(&footer, inner_width(body)));
        let inner = block.inner(body);
        frame.render_widget(Clear, body);
        frame.render_widget(block, body);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        Paragraph::new(view.lines(usize::from(inner.width), usize::from(inner.height)))
            .render(inner, frame.buffer_mut());
    }

    /// A per-file action's preview, over the form.
    ///
    /// Scrolled and never truncated: two hundred renumbered tracks is two hundred
    /// rows a user can read through, because a hidden change is the thing this
    /// program exists to prevent.
    fn render_preview(
        &self,
        preview: &Preview,
        body: Rect,
        frame: &mut ratatui::Frame,
        hints: &Hints,
    ) {
        let percent = u16::try_from(PREVIEW_PERCENT).unwrap_or(80);
        let [area] = Layout::horizontal([Constraint::Percentage(percent)])
            .flex(Flex::Center)
            .areas(body);
        let [area] = Layout::vertical([Constraint::Percentage(percent)])
            .flex(Flex::Center)
            .areas(area);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Yellow))
            .title(fit(&preview.title(), inner_width(area)))
            .title_bottom(fit(&preview.footer(hints), inner_width(area)));
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        if inner.width == 0 {
            return;
        }
        Paragraph::new(preview.lines(usize::from(inner.width), usize::from(inner.height)))
            .render(inner, frame.buffer_mut());
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
    /// this is the subset the shell and the browser can answer for.
    fn status_bar(&self) -> Paragraph<'_> {
        // No path here: the listing's own title carries it, and a 46-character
        // scene-release directory would push everything that changes off the
        // right-hand end of an 80-column bar. Task 26 owns the elision order;
        // not repeating a thing that is already on screen is free.
        let mut parts = vec![
            format!("{} marked", self.browser.marked()),
            format!("{} pending", self.plan.len()),
            format!("sort {}", self.browser.sort()),
            format!("focus {}", self.focus.label()),
        ];
        if self.browser.in_visual() {
            parts.push("VISUAL".to_owned());
        }
        if self.running.is_some() {
            // Shown here as well as on the message line, because the message
            // line is also where a toast goes and this one must not be possible
            // to miss.
            parts.push("WRITING".to_owned());
        }
        if self.warnings > 0 {
            // A badge, because a scan that skipped a file and said nothing is a
            // browser that is lying about the library. The count is the whole
            // message; `:messages` (task 26) is where the list will live.
            let plural = if self.warnings == 1 { "" } else { "s" };
            parts.push(format!("⚠ {} warning{plural}", self.warnings));
        }
        parts.push(
            self.mpd
                .as_ref()
                .map_or_else(|| "○ mpd ?".to_owned(), mpd_summary),
        );
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
        // A transaction in flight outranks a toast: it is the only thing on this
        // line that is still happening, and the one the user is waiting for.
        if let Some(running) = &self.running {
            let hint = match (
                running.is_cancellable(),
                self.keys.map().key_for(Mode::Pending, Action::Cancel),
            ) {
                (true, Some(key)) => format!(" \u{b7} {key} to stop"),
                _ => String::new(),
            };
            return Paragraph::new(
                Line::from(format!("{}{hint}", running.line())).style(Style::new().fg(Color::Cyan)),
            );
        }
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

/// The three panes of the browser: tree, listing, details.
///
/// The details pane is zero-wide below [`DETAILS_FROM`], which is how
/// `render_browser` decides not to draw it — a width and not a flag, so there is
/// one source of truth for the layout instead of a `bool` that could disagree
/// with it.
fn panes(body: Rect) -> [Rect; 3] {
    let details = if body.width >= DETAILS_FROM {
        DETAILS_W
    } else {
        0
    };
    // A quarter of the width for the tree, between the narrowest path worth
    // reading and the point where it starts stealing from the listing.
    let tree = (body.width / 4).clamp(14, 30).min(body.width / 2);
    Layout::horizontal([
        Constraint::Length(tree),
        Constraint::Min(10),
        Constraint::Length(details),
    ])
    .areas(body)
}

/// The audio files the tag editor would open on, and how many marked paths were
/// left out of them.
///
/// What is marked, or what the cursor is on when nothing is. A marked
/// **directory** stands for the audio files in it, one level down, which is
/// `mpdfm tag set` without `--recursive`: marking an album means that album, and
/// a multi-disc set's own root holds no audio to be ambiguous about.
///
/// Everything else that can be marked — a `.cue`, a `folder.jpg`, an `.nfo` —
/// is counted and left out rather than refused, because a user who marked a whole
/// listing with `a` meant the tracks in it and should not have to unmark the
/// clutter first.
///
/// The result is sorted by path and deduplicated. That order matters for more
/// than tidiness: it is the order `renumber tracks` numbers in, and a path that
/// was both marked and inside a marked directory must not become two operations
/// on one file, which the planner refuses as a duplicate edit.
fn tag_targets(browser: &Browser, library: &Library) -> (Vec<RelPath>, usize) {
    let mut marks = browser.marks();
    if marks.is_empty() {
        marks.extend(browser.focused_path(library));
    }

    let mut files: Vec<RelPath> = Vec::new();
    let mut skipped = 0;
    for rel in marks {
        match library.get(&rel) {
            Some(entry) if entry.is_audio() => files.push(rel),
            Some(_) => skipped += 1,
            // Not a file in the library, so it is one of the directories the
            // listing also lets you mark.
            None => {
                let Ok(dir) = DirPath::parse(rel.as_str()) else {
                    skipped += 1;
                    continue;
                };
                let audio: Vec<RelPath> = library
                    .files_in(&dir)
                    .filter(|entry| entry.is_audio())
                    .map(|entry| entry.rel.clone())
                    .collect();
                if audio.is_empty() {
                    skipped += 1;
                }
                files.extend(audio);
            }
        }
    }
    files.sort_unstable();
    files.dedup();
    (files, skipped)
}

/// The operation that moves `from` to `to`: a file move, or a directory's.
///
/// Which of the two it is, is the library's answer and not the user's: a
/// directory move takes the aux files with it and a file move does not
/// (`ops::op`), and a user who marked an album directory meant the album.
/// Anything the library does not know as a file is treated as a directory, which
/// is also the right answer for a stale model — the planner refuses it as
/// `SourceMissing` and the pending view shows the reason.
fn move_op(library: &Library, from: &RelPath, to: RelPath) -> Operation {
    if library.get(from).is_some() {
        Operation::MoveFile {
            from: from.clone(),
            to,
        }
    } else {
        Operation::MoveDir {
            from: from.clone(),
            to,
        }
    }
}

/// The action that carries out a per-file field's named alternative.
///
/// One mapping, here, so the message that refuses a typed `title` and the key
/// that does it instead cannot name two different things.
fn action_for(action: FileAction) -> Action {
    match action {
        FileAction::TitleFromFilename => Action::TitleFromFilename,
        FileAction::RenumberTracks => Action::RenumberTracks,
    }
}

/// How many cells a bordered pane has inside it.
fn inner_width(area: Rect) -> usize {
    usize::from(area.width.saturating_sub(2))
}

/// The tree pane's title: the library's own directory name.
fn root_name(library: &Library) -> &str {
    library
        .root()
        .file_name()
        .unwrap_or_else(|| library.root().as_str())
}

/// One tree row, indented and with the open/closed marker a tree needs.
fn tree_label(row: &TreeRow) -> String {
    let marker = match (row.has_children, row.expanded) {
        (false, _) => " ",
        (true, true) => "▾",
        (true, false) => "▸",
    };
    let name = row.dir.file_name().unwrap_or("/");
    format!("{}{marker} {name}", "  ".repeat(row.depth))
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

    use mpdfm_core::library::DirPath;
    use mpdfm_core::tags::{Field, TagDelta, WriteOpts};
    use mpdfm_core::testing::{Fixture, names};
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
    ///
    /// The cells a wide character covers are **skipped**. A two-cell `ノ` lives
    /// in one `Cell` and the next one is never written, so it still holds
    /// whatever the previous frame put there — invisible on a real terminal,
    /// because the glyph is drawn over it, and so it must be invisible here too.
    /// Reading it would make an assertion fail on a name the user can see
    /// perfectly well.
    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        (0..area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += u16::try_from(super::super::widgets::width(symbol))
                        .unwrap_or(1)
                        .max(1);
                }
                row.trim_end().to_owned()
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

    /// Where the focused pane's cursor is. The browser owns it now, and these
    /// two helpers are what keep the shell's own tests about the shell.
    fn cursor(app: &App) -> usize {
        let library = app.library.as_ref().expect("a library has landed");
        app.browser.pane_cursor(app.focus.pane(), library)
    }

    /// How many rows the focused pane has.
    fn row_count(app: &App) -> usize {
        let library = app.library.as_ref().expect("a library has landed");
        app.browser.row_count(app.focus.pane(), library)
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
        assert_eq!(cursor(&app), 2);
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
        assert_eq!(cursor(&app), 2, "the cursor must survive the overlay");

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
        let bottom = cursor(&app);
        assert!(bottom > 0, "the fixture should have more than one row");

        // A library with one directory in it, as a rescan after a big move might
        // find.
        let smaller = Fixture::builder().album("only", &["01 a.mp3"]).build();
        app.update(scanned(&smaller));
        assert!(
            cursor(&app) < bottom,
            "the cursor should have been clamped, not left past the end"
        );
        assert!(
            cursor(&app) < row_count(&app),
            "and it should be on a row that exists"
        );
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
        assert_eq!(cursor(&app), 0);
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
                queue: None,
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
            queue: None,
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
            queue: None,
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
            queue: None,
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
        let last = row_count(&app) - 1;

        app.update(press('k'));
        assert_eq!(cursor(&app), 0, "up from the top stays at the top");

        app.update(press('G'));
        assert_eq!(cursor(&app), last);
        app.update(press('j'));
        assert_eq!(
            cursor(&app),
            last,
            "down from the bottom stays at the bottom"
        );

        // `gg`, which takes two presses: one `g` is a prefix and nothing else.
        app.update(press('g'));
        assert_eq!(cursor(&app), last, "a lone `g` moves nothing");
        app.update(press('g'));
        assert_eq!(cursor(&app), 0);
    }

    #[test]
    fn the_cursor_does_nothing_in_a_library_with_no_rows() {
        let fx = Fixture::builder().build();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));

        // The tree always has one row — the library root itself, which is where
        // a browser stands even when there is nothing under it.
        assert_eq!(row_count(&app), 1);
        assert!(!app.update(press('j')));
        assert_eq!(cursor(&app), 0);

        // The listing really has none.
        app.focus = Focus::Files;
        assert_eq!(row_count(&app), 0);
        assert!(!app.update(press('j')));
        assert_eq!(cursor(&app), 0);
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
        assert_eq!(cursor(&app), 0, "a release must not move anything");
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
                        // Nothing to accept, nothing to delete, nothing to clear.
                        Action::Quit
                            | Action::Submit
                            | Action::DeleteChar
                            | Action::ClearLine
                            // Already at the top of the tree.
                            | Action::Top
                            | Action::Up
                            | Action::HalfPageUp
                            // Already at the library root: there is nothing
                            // above `music_directory` and saying so would be
                            // noise on a key that is pressed constantly.
                            | Action::Left
                            | Action::Parent
                            // Nothing is marked, so unmarking changes nothing.
                            | Action::UnmarkAll
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

        assert!(app.update(press('/')), "`/` is bound to search");
        let toast = app.toasts.front().expect("it should say something");
        assert!(toast.text.contains("search"), "{}", toast.text);
        assert!(
            toast.text.contains("25-search-and-filter.md"),
            "{}",
            toast.text
        );
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
        assert_eq!(cursor(&app), 0);
        assert!(app.update(press('J')), "`J` is a half page now");
        assert_eq!(cursor(&app), row_count(&app) - 1, "the fixture is short");
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
        // 40 albums plus the root row they hang off.
        assert_eq!(row_count(&app), 41);

        let step = usize::try_from(app.page_step()).expect("a positive step");
        assert!((2..20).contains(&step), "{step} is not half a screen");
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(cursor(&app), step);
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(cursor(&app), 0, "and back, stopping at the top");
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
        assert_eq!(cursor(&app), 0);

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
        assert_eq!(cursor(&app), 0);

        assert!(app.update(key(KeyCode::Enter)), "enter runs it");
        // It ran: one move is staged, and the view that shows what is staged is
        // what the command line left behind.
        assert_eq!(app.plan.len(), 1, "the command staged the move");
        assert!(
            matches!(app.views.last(), Some(View::Pending(_))),
            "{:?}",
            app.views
        );
        let toast = app.toasts.front().expect("it reported something");
        assert!(toast.text.contains("staged 1 move op"), "{}", toast.text);
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

    // -- the browser (task 22) ---------------------------------------------

    /// A fixture with one directory holding `count` tracks, for the tests about
    /// scrolling and about how much is read.
    fn big_album(count: usize) -> Fixture {
        let tracks: Vec<String> = (0..count).map(|n| format!("{n:03} track.mp3")).collect();
        let refs: Vec<&str> = tracks.iter().map(String::as_str).collect();
        Fixture::builder().album("big", &refs).build()
    }

    /// Drive the app to `dir`, as the keys would, and draw a frame.
    fn go_to(app: &mut App, terminal: &mut Terminal<TestBackend>, dir: &str) {
        // A frame first, so the app knows how tall the listing is — which is
        // what decides how many rows of tags it asks for.
        draw(app, terminal);
        let library = app.library.as_ref().expect("a library has landed");
        app.browser
            .open(DirPath::parse(dir).expect("a fixture path"), library);
        app.focus = Focus::Files;
        app.request_tags();
        draw(app, terminal);
    }

    /// Wait for the tag worker's answer and hand it to the app.
    ///
    /// Generous: this is a correctness test and not a timing one, and a machine
    /// under load must not fail it.
    fn take_tags(app: &mut App, rx: &mpsc::Receiver<Msg>) -> Vec<String> {
        loop {
            let msg = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the tag worker should answer");
            if let Msg::TaskDone(outcome) = &msg
                && let TaskOutcome::Tags(reads) = outcome.as_ref()
            {
                let paths = reads
                    .iter()
                    .map(|(rel, _)| rel.to_string())
                    .collect::<Vec<_>>();
                app.update(msg);
                return paths;
            }
            app.update(msg);
        }
    }

    #[test]
    fn l_and_h_and_enter_and_backspace_navigate_the_tree() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        draw(&mut app, &mut terminal);

        // `l` on the tree hands the keyboard to the listing — there is nothing
        // to open that is not already open — and the second one goes in.
        assert!(app.update(press('l')));
        assert_eq!(app.focus, Focus::Files);
        assert!(app.update(press('l')));
        assert_eq!(app.browser.dir().as_str(), "coding-music");

        // `enter` goes one deeper.
        assert!(app.update(key(KeyCode::Enter)));
        assert_eq!(app.browser.dir().as_str(), "coding-music/SwitchAngel");

        // `backspace` and `h` come back out, one level each.
        assert!(app.update(key(KeyCode::Backspace)));
        assert_eq!(app.browser.dir().as_str(), "coding-music");
        assert!(app.update(press('h')));
        assert!(app.browser.dir().is_root());

        // And out of the root there is nowhere to go.
        assert!(!app.update(press('h')));
        assert!(app.browser.dir().is_root());
    }

    #[test]
    fn the_title_and_the_status_bar_follow_the_browser_into_a_directory() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));

        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );
        let drawn = text(&terminal);
        assert!(
            drawn.contains("Mm..Food"),
            "the pane names where it is:\n{drawn}"
        );
        assert!(drawn.contains("01 Beef Rap.mp3"), "{drawn}");
        // The tree shows the path it came down, not just the leaf.
        assert!(drawn.contains("hiphop"), "{drawn}");
    }

    #[test]
    fn marks_persist_across_directories_and_the_status_bar_counts_them() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));

        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );
        assert!(app.update(key(KeyCode::Char(' '))));
        assert!(app.update(key(KeyCode::Char(' '))));
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("2 marked"), "{}", text(&terminal));

        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::KIND_OF_BLUE_ALBUM,
        );
        assert!(app.update(key(KeyCode::Char(' '))));
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("3 marked"),
            "a mark in another directory must not have been lost:\n{}",
            text(&terminal)
        );
    }

    #[test]
    fn v_range_marks_and_a_marks_the_view_and_capital_a_clears_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );

        assert!(app.update(press('v')), "`v` opens a range");
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("VISUAL"), "{}", text(&terminal));
        app.update(press('j'));
        app.update(press('j'));
        assert!(app.update(press('v')), "`v` closes it");
        assert_eq!(app.browser.marked(), 3);

        // `a` takes the whole listing — three tracks and five aux files.
        assert!(app.update(press('a')));
        assert_eq!(app.browser.marked(), 8);

        assert!(app.update(press('A')));
        assert_eq!(app.browser.marked(), 0);
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("0 marked"), "{}", text(&terminal));
    }

    #[test]
    fn esc_abandons_a_visual_range_before_it_dismisses_a_message() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );

        app.update(press('v'));
        app.update(press('j'));
        assert!(app.update(key(KeyCode::Esc)));
        assert_eq!(app.browser.marked(), 0, "esc marks nothing");
        assert!(!app.browser.in_visual());
    }

    #[test]
    fn a_non_audio_file_is_on_screen_and_the_details_pane_names_the_playlists() {
        let fx = Fixture::realistic();
        let (mut app, rx) = app(&fx);
        // Wide enough for the third column; the task calls it a wide terminal.
        let mut terminal = screen(120, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );

        let drawn = text(&terminal);
        assert!(
            drawn.contains("folder.jpg"),
            "the cover art travels too:\n{drawn}"
        );
        assert!(drawn.contains("info.nfo"), "{drawn}");

        // The cursor is on `01 Beef Rap.mp3`, which two playlists point at.
        take_tags(&mut app, &rx);
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("details"), "{drawn}");
        assert!(drawn.contains("Hip hop"), "{drawn}");
        assert!(drawn.contains("MF Doom"), "{drawn}");
        assert!(drawn.contains("2 lines in 2 playlists"), "{drawn}");
    }

    #[test]
    fn a_narrow_terminal_drops_the_details_pane_rather_than_the_listing() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(70, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );

        let drawn = text(&terminal);
        assert!(!drawn.contains("details"), "{drawn}");
        assert!(drawn.contains("01 Beef Rap.mp3"), "{drawn}");
    }

    #[test]
    fn only_the_visible_rows_have_their_tags_read() {
        let fx = big_album(60);
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        // Nothing is read before there is a screen to read for.
        assert!(rx.try_recv().is_err(), "no screen, no reads");

        let before = mpdfm_core::library::audio_reads();
        go_to(&mut app, &mut terminal, "big");
        let read = take_tags(&mut app, &rx);

        let visible = app.list_rows();
        assert!((10..30).contains(&visible), "{visible} is not a screenful");
        assert_eq!(
            read.len(),
            visible,
            "a 60-file directory on a {visible}-row pane read {} files",
            read.len()
        );
        // And they are the rows at the top of the listing, in order.
        assert_eq!(read[0], "big/000 track.mp3");
        assert_eq!(
            read[visible - 1],
            format!("big/{:03} track.mp3", visible - 1)
        );
        // The counter core keeps is a lower bound here, because the test suite
        // runs in parallel and it is process-wide.
        assert!(
            mpdfm_core::library::audio_reads() >= before + visible as u64,
            "the files really were opened"
        );

        // Jumping to the end reads the other end of the listing, and nothing in
        // the middle that was never on screen.
        app.update(press('G'));
        let read = take_tags(&mut app, &rx);
        assert_eq!(read.len(), visible);
        assert_eq!(read[visible - 1], "big/059 track.mp3");
        assert_eq!(
            app.browser.cached(),
            visible * 2,
            "only the two screenfuls that were looked at"
        );

        // And going back over rows that are already cached reads nothing.
        app.update(press('g'));
        app.update(press('g'));
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a cached row must not be read a second time"
        );
    }

    #[test]
    fn a_four_hundred_entry_directory_draws_a_frame_in_well_under_a_millisecond() {
        // The task's criterion, measured rather than asserted by eye. The bound
        // is deliberately loose — this runs alongside every other test — and the
        // number that matters is the one printed, which is recorded in
        // `docs/tasks/22-browser-view.md`.
        let fx = big_album(400);
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(120, 40);
        app.update(scanned(&fx));
        go_to(&mut app, &mut terminal, "big");
        assert_eq!(
            app.browser
                .row_count(Pane::Files, app.library.as_ref().unwrap()),
            400
        );

        // Scroll a row per frame, so every frame lays out a different window and
        // none of it can be cached between them.
        let frames = 400;
        let started = Instant::now();
        for _ in 0..frames {
            app.dispatch(Action::Down);
            terminal
                .draw(|frame| app.render(frame.area(), frame))
                .expect("drawing should work");
        }
        let each = started.elapsed() / frames;
        eprintln!(
            "400-row browser: {} µs per frame ({} build)",
            each.as_micros(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        // 130 µs released and 1.4 ms unoptimized on the author's machine, both
        // recorded in the task. The bound is what "smooth" means at 60 Hz with
        // room to spare, and is loose enough to survive a loaded test runner.
        assert!(
            each < Duration::from_millis(5),
            "{} µs per frame is not smooth scrolling",
            each.as_micros()
        );
    }

    #[test]
    fn a_cjk_name_renders_without_breaking_the_columns() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(120, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::KREAM_ALBUM,
        );

        let drawn = text(&terminal);
        assert!(drawn.contains("01 So Hï.mp3"), "{drawn}");
        assert!(drawn.contains("03 ノスタルジア.mp3"), "{drawn}");

        // Every pane border is in the same column on every row of the body.
        // That is what a width bug destroys: one cell too many on the CJK row
        // and the listing's right-hand border is pushed into the details pane.
        //
        // At 120 cells the panes are 30 + 64 + 26, so the verticals are here:
        let buffer = terminal.backend().buffer();
        let [tree, files, details] = panes(app.body());
        for y in 2..21 {
            for x in [
                tree.x,
                tree.right() - 1,
                files.x,
                files.right() - 1,
                details.x,
                details.right() - 1,
            ] {
                assert_eq!(
                    buffer[(x, y)].symbol(),
                    "│",
                    "row {y} has no pane border at column {x}"
                );
            }
        }
    }

    #[test]
    fn an_empty_directory_and_an_unreadable_one_both_say_which_they_are() {
        let fx = Fixture::builder().album("pop/fine", &["a.mp3"]).build();
        std::fs::create_dir(fx.music_dir().join("pop/nothing")).expect("mkdir");
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));

        go_to(&mut app, &mut terminal, "pop/nothing");
        let drawn = text(&terminal);
        assert!(drawn.contains("empty directory"), "{drawn}");
        // The chrome is still there: an empty directory is not an error.
        assert!(drawn.contains("MPDFM"), "{drawn}");
        assert!(drawn.contains("0 marked"), "{drawn}");
    }

    #[test]
    fn a_scan_warning_shows_as_a_badge_rather_than_being_swallowed() {
        // A name that is not UTF-8: scanned, skipped, warned about.
        let fx = Fixture::builder()
            .album("pop/fine", &["a.mp3"])
            .non_utf8_file("pop/fine", b"bad\xff.mp3")
            .build();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        draw(&mut app, &mut terminal);

        assert!(app.warnings > 0, "the fixture should have produced one");
        let drawn = text(&terminal);
        assert!(drawn.contains("warning"), "{drawn}");
    }

    #[test]
    fn set_sort_changes_the_order_and_is_remembered() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 24);
        app.update(scanned(&fx));
        assert_eq!(app.browser.sort(), Sort::Name);

        app.update(press(':'));
        type_in(&mut app, "set sort=size");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.browser.sort(), Sort::Size);
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("sort size"), "{}", text(&terminal));

        // It outlives a change of directory, which is what "per session" means.
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );
        assert_eq!(app.browser.sort(), Sort::Size);
    }

    #[test]
    fn a_sort_nobody_has_lists_the_ones_there_are() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.toasts.clear();

        app.update(press(':'));
        type_in(&mut app, "set sort=alphabetical");
        app.update(key(KeyCode::Enter));

        let toast = app.toasts.front().expect("it should say something");
        assert!(toast.text.contains("alphabetical"), "{}", toast.text);
        assert!(toast.text.contains("mtime"), "{}", toast.text);
        assert_eq!(app.browser.sort(), Sort::Name, "and nothing changed");
    }

    #[test]
    fn a_setting_nothing_applies_still_says_so() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        app.toasts.clear();

        app.update(press(':'));
        type_in(&mut app, "set backup_keep=20");
        app.update(key(KeyCode::Enter));
        let toast = app.toasts.front().expect("it should say something");
        assert!(toast.text.contains("backup_keep"), "{}", toast.text);
        assert_eq!(toast.level, Level::Warn);
    }

    #[test]
    fn a_tag_read_that_fails_marks_the_row_and_does_not_open_a_panel() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(120, 24);
        app.update(scanned(&fx));
        go_to(
            &mut app,
            &mut terminal,
            mpdfm_core::testing::names::MF_DOOM_ALBUM,
        );

        let rel = mpdfm_core::paths::RelPath::parse(mpdfm_core::testing::names::MF_DOOM_TRACK)
            .expect("a fixture path");
        app.update(Msg::TaskDone(Box::new(TaskOutcome::Tags(vec![(
            rel,
            Err("not a container MPDFM edits".to_owned()),
        )]))));

        assert_eq!(app.views, vec![View::Browser], "no panel for one bad file");
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("not a container"),
            "the details pane should say why:\n{}",
            text(&terminal)
        );
    }

    // -- the tag editor (task 23) ------------------------------------------

    /// An album of three mp3s and one piece of clutter.
    ///
    /// Every mp3 the fixture stamps out is byte-identical, so a selection of
    /// them agrees about every field and there is no `<multiple>` until
    /// [`retag`] makes one.
    fn album_fixture() -> Fixture {
        Fixture::builder()
            .album(
                "hiphop/Mm..Food",
                &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"],
            )
            .aux("hiphop/Mm..Food", &["folder.jpg"])
            .build()
    }

    /// Give one file its own value for a field, so a selection disagrees.
    fn retag(fx: &Fixture, rel: &str, field: Field, value: &str) {
        mpdfm_core::tags::write(
            &fx.abs(rel),
            &TagDelta::new().set(field, value),
            &WriteOpts::new(),
        )
        .expect("the fixture is ours to write");
    }

    /// What one file says about a field, read off the disk.
    fn tag_of(fx: &Fixture, rel: &str, field: Field) -> String {
        mpdfm_core::tags::read_tags(&fx.abs(rel))
            .expect("the file reads")
            .get(field)
            .joined()
    }

    /// Feed the app every worker answer that arrives, until it is in the state
    /// the test is waiting for.
    ///
    /// The workers are real threads here, which is the point: this exercises the
    /// path a keypress actually takes, channel included. What it waits for is a
    /// thread start — the fixture's files are a few kilobytes each.
    fn settle_until(app: &mut App, rx: &mpsc::Receiver<Msg>, done: impl Fn(&App) -> bool) {
        for _ in 0..100 {
            if done(app) {
                return;
            }
            let Ok(msg) = rx.recv_timeout(Duration::from_secs(10)) else {
                break;
            };
            app.update(msg);
        }
        assert!(
            done(app),
            "no worker answer put the app in the state the test was waiting for (writing={}, scan={:?}, cached={})",
            app.running.is_some(),
            app.scan,
            app.browser.cached()
        );
    }

    /// Put the browser in a directory with the listing focused, as walking there
    /// would have.
    fn in_dir(app: &mut App, dir: &str) {
        let library = app
            .library
            .clone()
            .expect("a library has landed before navigating");
        app.browser
            .open(DirPath::parse(dir).expect("a directory"), &library);
        app.focus = Focus::Files;
    }

    /// Mark the first `n` rows of the listing, the way `n` presses of `space`
    /// would: the mark key steps down after itself.
    fn mark_first(app: &mut App, n: usize) {
        app.dispatch(Action::Top);
        for _ in 0..n {
            app.dispatch(Action::ToggleMark);
        }
    }

    /// Open the tag editor and wait for the selection's tags.
    fn open_editor(app: &mut App, rx: &mpsc::Receiver<Msg>) {
        assert!(app.dispatch(Action::EditTags), "the editor should open");
        settle_until(app, rx, |app| {
            app.tagedit().is_some_and(|form| !form.is_loading())
        });
    }

    /// Replace a field's value as a user would: `i`, `ctrl-u`, the characters,
    /// `esc`.
    ///
    /// The characters go through [`App::update`] rather than a method, because
    /// "a letter reaches the field instead of the verb it is bound to" is half of
    /// what this task's key handling has to get right.
    fn type_field(app: &mut App, field: Field, text: &str) {
        go_to_field(app, field);
        assert!(app.dispatch(Action::EditField), "{field} would not open");
        app.dispatch(Action::ClearLine);
        for c in text.chars() {
            app.update(press(c));
        }
        app.dispatch(Action::Cancel);
    }

    /// Walk the form's cursor onto a field.
    fn go_to_field(app: &mut App, field: Field) {
        let row = mpdfm_core::tags::FIELDS
            .iter()
            .position(|other| *other == field)
            .expect("a field MPDFM models");
        app.dispatch(Action::Top);
        for _ in 0..row {
            app.dispatch(Action::Down);
        }
        assert_eq!(
            app.tagedit().map(TagEdit::field),
            Some(field),
            "the cursor is not on {field}"
        );
    }

    /// The tag edits a plan holds, as `(path, field, value)`.
    fn staged_edits(app: &App) -> Vec<(String, String, String)> {
        app.plan
            .ops()
            .iter()
            .flat_map(|op| match op {
                mpdfm_core::ops::Operation::WriteTags { target, changes } => changes
                    .edits()
                    .iter()
                    .map(|(field, edit)| {
                        (
                            target.to_string(),
                            field.to_string(),
                            edit.values()
                                .map(mpdfm_core::tags::Values::joined)
                                .unwrap_or_else(|| "<cleared>".to_owned()),
                        )
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn e_opens_the_editor_on_the_marks_and_reads_them_on_a_worker() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);

        // The key, not the action: `e` is what a user presses.
        assert!(app.update(press('e')));
        assert!(matches!(app.views.last(), Some(View::TagEdit(_))));
        // The form is on screen before the tags are, saying what it waits for.
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("reading 3 file(s)"),
            "{}",
            text(&terminal)
        );

        settle_until(&mut app, &rx, |app| {
            app.tagedit().is_some_and(|form| !form.is_loading())
        });
        let form = app.tagedit().expect("the editor is open");
        assert_eq!(form.len(), 3);
        assert_eq!(
            form.files()
                .iter()
                .map(|rel| rel.file_name().to_owned())
                .collect::<Vec<_>>(),
            vec!["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"],
            "the clutter is not in the selection, and the order is the listing's"
        );

        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("Edit tags — 3 files selected"), "{shown}");
        assert!(
            shown.contains("Album artist"),
            "every field has a row:\n{shown}"
        );
        assert!(shown.contains("modified: nothing"), "{shown}");
    }

    #[test]
    fn a_marked_directory_stands_for_the_audio_files_in_it() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        // At the root of `hiphop/`, the only row is the album directory.
        in_dir(&mut app, "hiphop");
        mark_first(&mut app, 1);

        open_editor(&mut app, &rx);
        assert_eq!(
            app.tagedit().map(TagEdit::len),
            Some(3),
            "a marked album means its tracks"
        );
    }

    #[test]
    fn nothing_to_edit_says_so_rather_than_opening_an_empty_form() {
        let fx = album_fixture();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        // The cursor is on the clutter, and nothing is marked.
        app.dispatch(Action::Bottom);
        app.toasts.clear();

        assert!(app.dispatch(Action::EditTags));
        assert!(matches!(app.views.last(), Some(View::Browser)));
        let toast = app.toasts.front().expect("it should say something");
        assert!(toast.text.contains("nothing to edit"), "{}", toast.text);
    }

    #[test]
    fn editing_one_files_genre_and_staging_produces_one_write_tags_op() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        type_field(&mut app, Field::Genre, "Nu Jazz");
        assert!(app.dispatch(Action::StageTags));

        assert_eq!(app.plan.len(), 1, "one file, one operation");
        assert_eq!(
            staged_edits(&app),
            vec![(
                "hiphop/Mm..Food/01 Beef Rap.mp3".to_owned(),
                "genre".to_owned(),
                "Nu Jazz".to_owned()
            )]
        );
        // Staging closes the form: its work has become operations.
        assert!(matches!(app.views.last(), Some(View::Browser)));
        // And nothing has been written.
        assert_ne!(
            tag_of(&fx, "hiphop/Mm..Food/01 Beef Rap.mp3", Field::Genre),
            "Nu Jazz"
        );
    }

    #[test]
    fn a_multiple_field_left_alone_stages_no_change_for_it() {
        // The critical test. Three files that disagree about the title, one
        // field edited, and not one delta may mention the title.
        let fx = album_fixture();
        retag(
            &fx,
            "hiphop/Mm..Food/02 Hoe Cakes.mp3",
            Field::Title,
            "Hoe Cakes",
        );
        retag(
            &fx,
            "hiphop/Mm..Food/03 Potholderz.mp3",
            Field::Title,
            "Potholderz",
        );

        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);

        let mut terminal = screen(100, 30);
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("<multiple>"),
            "three titles and no `<multiple>`:\n{}",
            text(&terminal)
        );

        type_field(&mut app, Field::Genre, "Nu Jazz");
        app.dispatch(Action::StageTags);

        assert_eq!(app.plan.len(), 3);
        for (path, field, value) in staged_edits(&app) {
            assert_eq!(
                field, "genre",
                "{path} had its {field} written to {value:?}"
            );
        }
    }

    #[test]
    fn typing_into_a_multiple_field_writes_to_every_file() {
        let fx = album_fixture();
        retag(
            &fx,
            "hiphop/Mm..Food/02 Hoe Cakes.mp3",
            Field::Genre,
            "Soul",
        );

        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);

        type_field(&mut app, Field::Genre, "Nu Jazz");
        app.dispatch(Action::StageTags);

        assert_eq!(
            app.plan.len(),
            3,
            "all three, including the one that differed"
        );
        assert!(
            staged_edits(&app)
                .iter()
                .all(|(_, field, value)| field == "genre" && value == "Nu Jazz")
        );
    }

    #[test]
    fn opening_a_multiple_field_and_leaving_it_stages_nothing() {
        let fx = album_fixture();
        retag(
            &fx,
            "hiphop/Mm..Food/02 Hoe Cakes.mp3",
            Field::Genre,
            "Soul",
        );

        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Genre);
        app.dispatch(Action::EditField);
        // Look at it, move the text cursor, change your mind.
        app.dispatch(Action::Left);
        app.dispatch(Action::Cancel);

        assert_eq!(app.tagedit().map(TagEdit::is_modified), Some(false));
        app.toasts.clear();
        app.dispatch(Action::StageTags);
        assert!(app.plan.is_empty(), "{:?}", app.plan.ops());
        let toast = app.toasts.front().expect("it should say why");
        assert!(toast.text.contains("nothing to change"), "{}", toast.text);
    }

    #[test]
    fn a_letter_reaches_the_field_rather_than_the_verb_it_is_bound_to() {
        // `j`, `k` and `G` move between fields on the form and must still type
        // into one. The keymap is not consulted for a character at all.
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Genre);
        app.dispatch(Action::EditField);
        app.dispatch(Action::ClearLine);
        for c in "jkGgi".chars() {
            app.update(press(c));
        }
        assert_eq!(
            app.tagedit().map(TagEdit::field),
            Some(Field::Genre),
            "a letter moved the cursor"
        );
        app.dispatch(Action::Cancel);
        type_field_assert(&app, Field::Genre, "jkGgi");
    }

    /// What the form would stage for one field.
    fn type_field_assert(app: &App, field: Field, value: &str) {
        let edits = app
            .tagedit()
            .expect("the editor is open")
            .edits()
            .get(&field)
            .cloned();
        assert_eq!(
            edits,
            Some(mpdfm_core::tags::Edit::Set(mpdfm_core::tags::Values::one(
                value
            )))
        );
    }

    #[test]
    fn an_invalid_year_shows_the_reason_and_blocks_staging() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        type_field(&mut app, Field::Year, "20x4");
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("not a year"),
            "the error is not inline:\n{}",
            text(&terminal)
        );

        app.toasts.clear();
        assert!(app.dispatch(Action::StageTags));
        assert!(app.plan.is_empty(), "a bad year was staged anyway");
        assert!(
            matches!(app.views.last(), Some(View::TagEdit(_))),
            "the form stays open so the value can be fixed"
        );
        let toast = app.toasts.front().expect("it should say why");
        assert!(toast.text.contains("nothing staged"), "{}", toast.text);
        assert!(toast.text.contains("year"), "{}", toast.text);

        // Fixed, it stages. A full date rather than a year, because the fixture
        // already says 2004 and an edit to what is there changes nothing.
        type_field(&mut app, Field::Year, "2019-03-15");
        assert!(app.dispatch(Action::StageTags));
        assert_eq!(app.plan.len(), 1);
        assert_eq!(
            staged_edits(&app),
            vec![(
                "hiphop/Mm..Food/01 Beef Rap.mp3".to_owned(),
                "year".to_owned(),
                "2019-03-15".to_owned()
            )]
        );
    }

    #[test]
    fn a_per_file_field_will_not_open_across_a_selection_and_names_the_key_that_does_it() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Title);
        app.toasts.clear();
        assert!(app.dispatch(Action::EditField));
        assert_eq!(app.tagedit().map(TagEdit::is_editing), Some(false));
        let toast = app.toasts.front().expect("it should say why");
        assert!(
            toast.text.contains("every file wants its own title"),
            "{}",
            toast.text
        );
        assert!(
            toast.text.contains('T') && toast.text.contains("titles from filenames"),
            "the refusal does not name the action that does it: {}",
            toast.text
        );
    }

    #[test]
    fn esc_without_modifications_leaves_and_with_them_asks_first() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        // Nothing typed: straight out.
        app.dispatch(Action::Cancel);
        assert!(matches!(app.views.last(), Some(View::Browser)));

        // Something typed: a question, and `n` keeps the form and the edit.
        open_editor(&mut app, &rx);
        type_field(&mut app, Field::Genre, "Nu Jazz");
        app.dispatch(Action::Cancel);
        let Some(View::Confirm(confirm)) = app.views.last() else {
            panic!("esc with changes should ask: {:?}", app.views);
        };
        assert!(confirm.question.contains("genre"), "{}", confirm.question);

        app.update(press('n'));
        assert!(matches!(app.views.last(), Some(View::TagEdit(_))));
        assert_eq!(app.tagedit().map(TagEdit::is_modified), Some(true));

        // And `y` throws the edit away.
        app.dispatch(Action::Cancel);
        app.update(press('y'));
        assert!(matches!(app.views.last(), Some(View::Browser)));
        assert!(app.plan.is_empty());
    }

    #[test]
    fn esc_inside_a_field_leaves_the_field_and_not_the_form() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Genre);
        app.dispatch(Action::EditField);
        app.update(key(KeyCode::Esc));
        assert!(matches!(app.views.last(), Some(View::TagEdit(_))));
        assert_eq!(app.tagedit().map(TagEdit::is_editing), Some(false));
    }

    #[test]
    fn an_action_previews_and_must_be_answered_before_anything_else() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);

        assert!(app.dispatch(Action::TitleFromFilename));
        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("titles from filenames"), "{shown}");
        assert!(
            shown.contains("Hoe Cakes"),
            "the new value is not shown:\n{shown}"
        );
        // Two of three: the first file's title already is what its own name
        // says, and an edit that would change nothing is not in the preview.
        assert!(shown.contains("2 files would change"), "{shown}");

        // The form's own keys do not reach it: the preview is a question.
        app.dispatch(Action::ClearField);
        assert_eq!(app.tagedit().map(TagEdit::is_modified), Some(false));

        app.dispatch(Action::Submit);
        assert!(app.tagedit().is_some_and(|form| form.preview().is_none()));
        assert_eq!(
            app.tagedit().map(TagEdit::modified),
            Some(vec![Field::Title])
        );

        app.dispatch(Action::StageTags);
        assert_eq!(
            app.plan.len(),
            2,
            "the file that was already right is left alone"
        );
        assert!(
            staged_edits(&app)
                .iter()
                .all(|(_, field, _)| field == "title")
        );
    }

    #[test]
    fn a_file_that_cannot_be_written_is_named_before_anything_is_staged() {
        use std::os::unix::fs::PermissionsExt as _;

        let fx = album_fixture();
        let rel = "hiphop/Mm..Food/02 Hoe Cakes.mp3";
        let abs = fx.abs(rel);
        let was = std::fs::metadata(abs.as_std_path())
            .expect("the fixture is there")
            .permissions();
        std::fs::set_permissions(abs.as_std_path(), std::fs::Permissions::from_mode(0o444))
            .expect("the fixture is ours");

        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        mark_first(&mut app, 3);
        open_editor(&mut app, &rx);
        type_field(&mut app, Field::Genre, "Nu Jazz");

        assert!(app.dispatch(Action::StageTags));
        assert!(
            app.plan.is_empty(),
            "something was staged: {:?}",
            app.plan.ops()
        );
        let Some(View::Error(message)) = app.views.last() else {
            panic!("a refusal has to be read, not noticed: {:?}", app.views);
        };
        assert!(message.contains("nothing was staged"), "{message}");
        assert!(
            message.contains("02 Hoe Cakes.mp3"),
            "the refusal does not name the file:\n{message}"
        );

        std::fs::set_permissions(abs.as_std_path(), was).expect("the fixture is ours");
    }

    #[test]
    fn capital_w_commits_and_the_browser_shows_the_change_afterwards() {
        let fx = album_fixture();
        let rel = "hiphop/Mm..Food/01 Beef Rap.mp3";
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        type_field(&mut app, Field::Genre, "Nu Jazz");
        assert!(app.dispatch(Action::StageAndCommit));
        assert!(app.running.is_some(), "the commit runs on a worker");

        settle_until(&mut app, &rx, |app| app.running.is_none());
        assert!(app.plan.is_empty(), "a committed plan is not still pending");
        assert_eq!(
            tag_of(&fx, rel, Field::Genre),
            "Nu Jazz",
            "the file on disk"
        );

        // The commit asks for a rescan, so the browser is looking at the new
        // library rather than the one the edit was made against.
        settle_until(&mut app, &rx, |app| {
            matches!(app.scan, ScanState::Done { .. })
        });
        // A frame first: how many rows of tags to read ahead comes from the size
        // the terminal last reported, and nothing has been drawn in this test.
        draw(&mut app, &mut terminal);
        app.update(Msg::Tick);
        settle_until(&mut app, &rx, |app| app.browser.cached() > 0);
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("Nu Jazz"),
            "the browser is still showing the old genre:\n{}",
            text(&terminal)
        );
    }

    #[test]
    fn undo_from_the_browser_reverses_a_committed_tag_edit() {
        let fx = album_fixture();
        let rel = "hiphop/Mm..Food/01 Beef Rap.mp3";
        let before = tag_of(&fx, rel, Field::Genre);
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        type_field(&mut app, Field::Genre, "Nu Jazz");
        app.dispatch(Action::StageAndCommit);
        settle_until(&mut app, &rx, |app| app.running.is_none());
        assert_eq!(tag_of(&fx, rel, Field::Genre), "Nu Jazz");

        app.toasts.clear();
        assert!(app.dispatch(Action::Undo), "`u` in the browser");
        settle_until(&mut app, &rx, |app| app.running.is_none());

        assert_eq!(
            tag_of(&fx, rel, Field::Genre),
            before,
            "undo did not put the genre back"
        );
        assert!(
            app.views.iter().all(|view| !matches!(view, View::Error(_))),
            "{:?}",
            app.views
        );
    }

    #[test]
    fn utf8_typed_into_a_field_is_written_to_the_file() {
        let fx = album_fixture();
        let rel = "hiphop/Mm..Food/01 Beef Rap.mp3";
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        // A CJK album and an accented artist, both typed one keypress at a time.
        type_field(&mut app, Field::Album, "ノスタルジア");
        type_field(&mut app, Field::Artist, "KREAM - So Hï");
        app.dispatch(Action::StageAndCommit);
        settle_until(&mut app, &rx, |app| app.running.is_none());

        assert_eq!(tag_of(&fx, rel, Field::Album), "ノスタルジア");
        assert_eq!(tag_of(&fx, rel, Field::Artist), "KREAM - So Hï");
    }

    #[test]
    fn two_hundred_files_across_two_albums_open_and_preview_correctly() {
        let names: Vec<String> = (1..=100).map(|n| format!("{n:03} Track.mp3")).collect();
        let tracks: Vec<&str> = names.iter().map(String::as_str).collect();
        let fx = Fixture::builder()
            .album("hiphop/Mm..Food", &tracks)
            .album("hiphop/Madvillainy", &tracks)
            .build();

        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        // Both album directories marked, from the listing that holds them.
        in_dir(&mut app, "hiphop");
        mark_first(&mut app, 2);
        open_editor(&mut app, &rx);

        assert_eq!(app.tagedit().map(TagEdit::len), Some(200));
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("200 files selected"),
            "{}",
            text(&terminal)
        );

        // Two albums, so the album field disagrees — and it stays that way.
        type_field(&mut app, Field::Genre, "Nu Jazz");
        assert!(app.dispatch(Action::RenumberTracks));
        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("200 files would change"), "{shown}");
        assert!(
            shown.contains("200/200") || shown.contains("1/200"),
            "{shown}"
        );
        app.dispatch(Action::Submit);

        app.dispatch(Action::StageTags);
        assert_eq!(app.plan.len(), 200);
        let edits = staged_edits(&app);
        assert_eq!(edits.len(), 400, "two fields per file and nothing else");
        assert!(
            edits
                .iter()
                .all(|(_, field, _)| field == "genre" || field == "track"),
            "the album was flattened across two albums"
        );
    }

    #[test]
    fn one_file_also_shows_what_the_audio_is_and_where_it_lives() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("hiphop/Mm..Food/01 Beef Rap.mp3"), "{shown}");
        assert!(shown.contains("kbps") && shown.contains("Hz"), "{shown}");
        assert!(shown.contains("Edit tags — 01 Beef Rap.mp3"), "{shown}");
    }

    #[test]
    fn an_emptied_field_says_cleared_and_stages_a_clear() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Comment);
        assert!(app.dispatch(Action::ClearField));
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("<cleared>"),
            "the user cannot tell an emptied field from an empty one:\n{}",
            text(&terminal)
        );
    }

    #[test]
    fn the_terminal_cursor_sits_in_the_field_being_typed_into() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        go_to_field(&mut app, Field::Genre);
        app.dispatch(Action::EditField);
        draw(&mut app, &mut terminal);

        let at = terminal
            .get_cursor_position()
            .expect("a test backend has a cursor");
        let (column, row) = (at.x, at.y);
        // The form fills the body, which starts one row below the header; the
        // genre is the eighth of the ten field rows.
        let genre_row = mpdfm_core::tags::FIELDS
            .iter()
            .position(|field| *field == Field::Genre)
            .expect("genre is a field");
        assert_eq!(
            usize::from(row),
            2 + genre_row,
            "the form's border is row 1"
        );
        assert!(column > 14, "the cursor is in the label column: {column}");
    }

    #[test]
    fn the_help_overlay_opens_over_the_form_and_lists_its_keys() {
        let fx = album_fixture();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, "hiphop/Mm..Food");
        app.dispatch(Action::Top);
        open_editor(&mut app, &rx);

        app.update(press('?'));
        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("help · tagedit"), "{shown}");
        assert!(shown.contains("stage the tag edit"), "{shown}");

        // And closing it leaves the form exactly where it was.
        app.update(key(KeyCode::Esc));
        assert!(matches!(app.views.last(), Some(View::TagEdit(_))));
    }

    // -- the pending view (task 24) ----------------------------------------

    /// Mark the first row of `hiphop`, which is the MF DOOM album directory:
    /// three tracks, five aux files, two playlists and MPD's saved queue.
    fn mark_the_album(app: &mut App) {
        in_dir(app, "hiphop");
        mark_first(app, 1);
        assert_eq!(app.browser.marked(), 1, "the album directory is marked");
    }

    /// Stage a move of that album into `electronic`, the way the keys do it:
    /// mark it, walk to where it belongs, press `m`.
    fn stage_the_album_move(app: &mut App) {
        mark_the_album(app);
        in_dir(app, "electronic");
        assert!(app.dispatch(Action::StageMove), "`m` stages the move");
        assert_eq!(app.plan.len(), 1, "one operation, however many files");
    }

    /// The pending view on the stack, for a test that wants to ask it something.
    fn pending_view(app: &App) -> &Pending {
        match app.views.last() {
            Some(View::Pending(view)) => view,
            other => panic!("the pending view should be on top, not {other:?}"),
        }
    }

    #[test]
    fn m_stages_a_move_of_the_marks_and_shows_what_it_would_do() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        // Wide enough for the fixture's scene-release names, which are what the
        // renderer shortens from the left on an 80-column terminal.
        let mut terminal = screen(140, 30);
        app.update(scanned(&fx));

        stage_the_album_move(&mut app);
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);

        // The operation, what it costs, and every playlist it reaches.
        assert!(drawn.contains("PENDING (1 op)"), "{drawn}");
        assert!(drawn.contains("MOVE"), "{drawn}");
        assert!(drawn.contains("electronic/MF DOOM"), "{drawn}");
        assert!(drawn.contains(names::HIP_HOP_PLAYLIST), "{drawn}");
        assert!(drawn.contains(names::MF_DOOM_PLAYLIST), "{drawn}");
        assert!(drawn.contains("MPD saved queue"), "{drawn}");
        assert!(drawn.contains("commit"), "the footer offers it: {drawn}");
    }

    #[test]
    fn the_preview_unfolds_into_the_playlist_lines_and_matches_the_dry_run() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        // Folded, the body is what `mpdfm move --dry-run` prints — the
        // anti-divergence criterion, checked again here with the real width the
        // frame uses.
        let effects = app.plan.clone().validate(
            app.library.as_ref().expect("a library"),
            app.index.as_ref().expect("an index"),
            &app.config,
        );
        let cells = 98;
        let folded: Vec<String> = pending_view(&app)
            .body_text(cells)
            .lines()
            .map(|line| match line.strip_prefix("▸ ") {
                Some(rest) => format!("  {rest}"),
                None => line.to_owned(),
            })
            .collect();
        assert_eq!(folded.join("\n"), effects.render(cells));

        // Unfolded, it shows the lines themselves. `j` down to the first row
        // that has something to unfold, then `enter`.
        for _ in 0..40 {
            if pending_view(&app).can_expand() {
                break;
            }
            app.update(press('j'));
        }
        assert!(app.update(key(KeyCode::Enter)), "enter unfolds it");
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("- hiphop/MF DOOM"), "{drawn}");
        assert!(drawn.contains("+ electronic/MF DOOM"), "{drawn}");
    }

    #[test]
    fn staged_operations_survive_leaving_the_view_and_a_resize() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        // Away.
        assert!(app.update(key(KeyCode::Esc)), "esc leaves the view");
        assert_eq!(app.views, vec![View::Browser]);
        assert_eq!(app.plan.len(), 1, "the plan is the shell's, not the view's");
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("1 pending"),
            "the status bar still counts it:\n{}",
            text(&terminal)
        );

        // A resize, which is the other thing that must not lose it.
        app.update(Msg::Input(Event::Resize(60, 20)));
        assert_eq!(app.plan.len(), 1);

        // And back, with the preview made again rather than remembered.
        assert!(app.update(press('p')), "`p` shows it again");
        draw(&mut app, &mut terminal);
        assert!(
            text(&terminal).contains("PENDING (1 op)"),
            "{}",
            text(&terminal)
        );
    }

    #[test]
    fn dd_drops_one_operation_and_validates_what_is_left() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));

        // Two moves, the second of which is refused: it lands on an album that
        // is not going anywhere. (Landing on the *first* album's directory
        // would be legal — the planner orders a chain — which is exactly the
        // kind of thing re-validating after a drop has to get right.)
        mark_the_album(&mut app);
        in_dir(&mut app, "electronic");
        app.dispatch(Action::StageMove);
        // Out of the view first: it is modal, so the browser's own verbs do not
        // reach the browser while it is up.
        app.update(key(KeyCode::Esc));
        app.dispatch(Action::UnmarkAll);
        in_dir(&mut app, "hiphop");
        app.dispatch(Action::Top);
        app.dispatch(Action::Down);
        app.dispatch(Action::ToggleMark);
        app.run_command(command::Command::Move {
            dst: names::KREAM_ALBUM.to_owned(),
        });
        assert_eq!(app.plan.len(), 2);
        assert!(
            !pending_view(&app).is_committable(),
            "the second move lands on something that is already there"
        );

        // `dd` on the refused one — which is where the cursor already is.
        app.toasts.clear();
        assert!(matches!(app.mode(), Mode::Pending));
        app.update(press('d'));
        assert!(app.update(press('d')), "`dd` drops it");

        assert_eq!(app.plan.len(), 1, "one operation gone");
        let toast = app.toasts.front().expect("it said so");
        assert!(toast.text.contains("dropped 1 operation"), "{}", toast.text);
        // Re-validated, not patched: the conflict went with the operation.
        assert!(
            pending_view(&app).is_committable(),
            "what is left can be committed"
        );
    }

    #[test]
    fn dd_on_something_that_is_not_an_operation_says_so() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);
        app.toasts.clear();

        // Onto a playlist row, and into its diff.
        for _ in 0..40 {
            if pending_view(&app).can_expand() {
                break;
            }
            app.update(press('j'));
        }
        app.update(key(KeyCode::Enter));
        app.update(press('j'));

        app.update(press('d'));
        app.update(press('d'));
        assert_eq!(app.plan.len(), 1, "nothing was dropped");
        let toast = app.toasts.front().expect("it said so");
        assert!(toast.text.contains("no operation"), "{}", toast.text);
    }

    #[test]
    fn x_discards_everything_once_the_question_is_answered() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        assert!(app.update(press('x')), "`x` asks first");
        assert!(
            matches!(app.views.last(), Some(View::Confirm(_))),
            "{:?}",
            app.views
        );
        assert_eq!(app.plan.len(), 1, "nothing is thrown away until it is");

        // Saying no leaves it alone.
        app.update(press('n'));
        assert_eq!(app.plan.len(), 1);
        assert!(matches!(app.views.last(), Some(View::Pending(_))));

        // Saying yes empties the plan and closes the view, because there is
        // nothing left for it to be about.
        app.update(press('x'));
        app.update(press('y'));
        assert!(app.plan.is_empty(), "discarded");
        assert_eq!(app.views, vec![View::Browser]);
    }

    #[test]
    fn a_conflicting_plan_will_not_commit_and_says_why_on_the_operation() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(120, 30);
        app.update(scanned(&fx));

        // A move onto something that is already there. MPDFM never overwrites.
        mark_the_album(&mut app);
        app.run_command(command::Command::Move {
            dst: names::SNOOP_ALBUM.to_owned(),
        });
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);

        assert!(drawn.contains("REFUSED (1 op, 1 conflict)"), "{drawn}");
        assert!(drawn.contains("already exists"), "the reason: {drawn}");
        assert!(drawn.contains("refused"), "the footer: {drawn}");

        // And `c` does not start a transaction.
        app.toasts.clear();
        assert!(app.update(press('c')), "`c` answers");
        assert!(app.running.is_none(), "nothing is being written");
        let toast = app.toasts.front().expect("it said why");
        assert!(toast.text.contains("1 conflict"), "{}", toast.text);
    }

    #[test]
    fn c_commits_on_a_worker_and_leaves_the_browser_showing_the_result() {
        let fx = Fixture::realistic();
        let (mut app, rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        assert!(app.update(press('c')), "`c` commits");
        assert!(app.running.is_some(), "on a worker, not on this thread");

        // A progress indicator while it runs — the frame is drawn from the
        // worker's own reports.
        app.update(Msg::Committing(Progress::Steps { done: 3, steps: 8 }));
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("3/8 files"), "{drawn}");
        assert!(drawn.contains("37%"), "{drawn}");

        settle_until(&mut app, &rx, |app| app.running.is_none());
        assert!(app.plan.is_empty(), "a committed plan is not still pending");

        // The files moved, and the playlists went with them.
        assert!(
            fx.music_dir()
                .join("electronic/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3")
                .exists()
        );
        let playlist = std::fs::read_to_string(fx.playlist_dir().join(names::HIP_HOP_PLAYLIST))
            .expect("the playlist is readable");
        assert!(playlist.contains("electronic/MF DOOM"), "{playlist}");

        // The report: the txid, and the offer to undo it.
        let drawn = {
            draw(&mut app, &mut terminal);
            text(&terminal)
        };
        assert!(drawn.contains("COMMITTED"), "{drawn}");
        assert!(drawn.contains("undo"), "{drawn}");

        // And the library on screen is the one on disk.
        settle_until(&mut app, &rx, |app| {
            matches!(app.scan, ScanState::Done { .. })
        });
        let library = app.library.as_ref().expect("a library");
        assert!(
            library
                .dir(
                    &DirPath::parse("electronic/MF DOOM - Mm..Food (2004) [V0] scene-tag")
                        .expect("a dir")
                )
                .is_some(),
            "the browser is still showing the old library"
        );
    }

    #[test]
    fn u_after_a_commit_undoes_it_and_the_browser_shows_the_reversal() {
        let fx = Fixture::realistic();
        let (mut app, rx) = app(&fx);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        app.update(press('c'));
        settle_until(&mut app, &rx, |app| app.running.is_none());
        settle_until(&mut app, &rx, |app| {
            matches!(app.scan, ScanState::Done { .. })
        });
        assert!(matches!(
            pending_view(&app).report(),
            Some(Report::Done { .. })
        ));

        // `u`, from the report, about the transaction it names.
        app.toasts.clear();
        assert!(app.update(press('u')), "`u` undoes it right there");
        settle_until(&mut app, &rx, |app| app.running.is_none());

        assert!(
            fx.music_dir().join(names::MF_DOOM_TRACK).exists(),
            "the album is back where it was"
        );
        let playlist = std::fs::read_to_string(fx.playlist_dir().join(names::HIP_HOP_PLAYLIST))
            .expect("the playlist is readable");
        assert!(playlist.contains(names::MF_DOOM_TRACK), "{playlist}");

        // Back on the browser, which is where the reversal is visible.
        settle_until(&mut app, &rx, |app| {
            matches!(app.scan, ScanState::Done { .. })
        });
        assert_eq!(app.views, vec![View::Browser]);
        let library = app.library.as_ref().expect("a library");
        assert!(
            library
                .get(&RelPath::parse(names::MF_DOOM_TRACK).expect("a path"))
                .is_some()
        );
    }

    #[test]
    fn a_commit_that_stopped_partway_shows_what_to_run_to_put_it_back() {
        use mpdfm_core::journal::record::TxId;
        use mpdfm_core::ops::commit::CommitError;
        use mpdfm_core::ops::exec_fs::FsError;

        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);
        app.dispatch(Action::Commit);

        // The failure core raises when a filesystem step stops a transaction
        // that has already journaled some of its work. Simulated here the way
        // `commit::Inject` simulates it there: the message is core's own, and it
        // is the one the user has to be able to act on.
        let failed = CommitError::Step {
            txid: TxId::parse("20260101T101010Z-abcd").expect("a valid id"),
            position: 3,
            step: "rename hiphop/MF DOOM/03 Potholderz.mp3".to_owned(),
            source: Box::new(FsError::Exists {
                path: "/music/electronic/MF DOOM/03 Potholderz.mp3".into(),
            }),
        };
        app.update(Msg::TaskDone(Box::new(TaskOutcome::Committed(Err(
            NotCommitted::Failed(failed.to_string()),
        )))));

        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains("COMMIT FAILED"), "{drawn}");
        assert!(
            drawn.contains("stopped at step 3"),
            "what did not happen: {drawn}"
        );
        assert!(
            drawn.contains("mpdfm recover 20260101T101010Z-abcd"),
            "the recovery command: {drawn}"
        );
        // The plan is still staged: nothing about a failure says the user has
        // changed their mind.
        assert_eq!(app.plan.len(), 1);
        assert!(app.running.is_none());
    }

    #[test]
    fn a_commit_can_be_called_off_before_anything_has_changed() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);
        app.dispatch(Action::Commit);

        // While it is still re-checking the library, `esc` stops it.
        app.update(Msg::Committing(Progress::Validating));
        app.toasts.clear();
        assert!(app.update(key(KeyCode::Esc)), "esc asks it to stop");
        let running = app.running.as_ref().expect("still on the worker");
        assert!(running.cancel.load(Ordering::Relaxed), "the worker is told");
        assert!(running.cancelling);

        // Past that point it says so rather than pretending.
        app.running.as_mut().expect("running").cancelling = false;
        app.update(Msg::Committing(Progress::Steps { done: 1, steps: 8 }));
        app.toasts.clear();
        app.update(key(KeyCode::Esc));
        let toast = app.toasts.front().expect("it said so");
        assert!(toast.text.contains("too late"), "{}", toast.text);

        // And the answer puts the plan back in front of the user, untouched.
        app.update(Msg::TaskDone(Box::new(TaskOutcome::Committed(Err(
            NotCommitted::Cancelled,
        )))));
        assert_eq!(app.plan.len(), 1, "nothing was committed");
        assert!(matches!(
            pending_view(&app).report(),
            Some(Report::Cancelled)
        ));
    }

    #[test]
    fn the_mpd_queue_warning_shows_when_the_daemon_is_holding_one_of_the_files() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(110, 30);
        app.update(scanned(&fx));

        // MPD answered, and the file being moved is in its live queue. The
        // daemon writes that queue over the state file when it stops, so there
        // is nothing MPDFM can edit — the honest answer is a warning.
        app.update(Msg::MpdStatus(Box::new(MpdSnapshot {
            state: Some(MpdState {
                play_state: mpdfm_core::mpd::PlayState::Play,
                song: Some(names::MF_DOOM_TRACK.to_owned()),
                updating: false,
            }),
            enabled: true,
            problem: None,
            queue: Some(vec![RelPath::parse(names::MF_DOOM_TRACK).expect("a path")]),
        })));
        stage_the_album_move(&mut app);

        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(
            drawn.contains("will need a requeue"),
            "the requeue warning: {drawn}"
        );
        // And the saved queue is not also being rewritten, which would be the
        // edit the daemon then overwrote.
        assert!(
            !drawn.contains("MPD saved queue"),
            "it cannot be both: {drawn}"
        );
    }

    #[test]
    fn d_stages_a_delete_of_the_marks_and_a_directory_is_left_out_of_it() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));

        in_dir(&mut app, names::MF_DOOM_ALBUM);
        mark_first(&mut app, 2);
        assert!(app.update(press('d')), "`d` stages a delete");

        assert_eq!(app.plan.len(), 2, "one operation per file");
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("DELETE"), "{}", text(&terminal));
    }

    #[test]
    fn r_opens_the_command_line_on_the_path_under_the_cursor() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        in_dir(&mut app, names::MF_DOOM_ALBUM);
        app.dispatch(Action::Top);

        assert!(
            app.update(press('r')),
            "`r` is a move with the path filled in"
        );
        assert_eq!(app.mode(), Mode::Command);
        draw(&mut app, &mut terminal);
        let drawn = text(&terminal);
        assert!(drawn.contains(":move hiphop/MF DOOM"), "{drawn}");
        assert!(drawn.contains("01 Beef Rap.mp3"), "{drawn}");
    }

    #[test]
    fn q_with_a_staged_move_still_asks_before_throwing_it_away() {
        // The same question task 20 wired up, now with something real behind it.
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        assert!(app.update(press('q')), "`q` asks");
        assert!(!app.quit, "and does not leave yet");
        assert!(
            matches!(app.views.last(), Some(View::Confirm(_))),
            "{:?}",
            app.views
        );
        app.update(press('y'));
        assert!(app.quit);
    }

    #[test]
    fn a_four_hundred_line_diff_scrolls_at_well_under_a_frame_a_millisecond() {
        // The pitfall this task names is a plan whose diff is too long to show,
        // and the answer is to scroll it rather than cut it — which is only an
        // answer if scrolling is cheap. Every frame here lays out 400 operations
        // and 800 unfolded diff rows from scratch: nothing is cached between
        // frames, because the preview is never patched.
        let count = 400;
        let tracks: Vec<String> = (0..count).map(|n| format!("{n:03} track.mp3")).collect();
        let refs: Vec<&str> = tracks.iter().map(String::as_str).collect();
        let lines: Vec<String> = tracks.iter().map(|name| format!("big/{name}")).collect();
        let in_playlist: Vec<&str> = lines.iter().map(String::as_str).collect();
        let fx = Fixture::builder()
            .album("big", &refs)
            .playlist("Everything.m3u", &in_playlist)
            .build();

        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(120, 40);
        app.update(scanned(&fx));
        in_dir(&mut app, "");
        mark_first(&mut app, 1);
        in_dir(&mut app, "elsewhere");
        app.run_command(command::Command::Move {
            dst: "elsewhere/big".to_owned(),
        });
        assert_eq!(app.plan.len(), 1);
        assert_eq!(
            pending_view(&app).effects().playlist_edits[0]
                .line_edits
                .len(),
            count,
            "every line of the playlist changes"
        );

        // Unfold it, then scroll a row per frame.
        for _ in 0..40 {
            if pending_view(&app).can_expand() {
                break;
            }
            app.update(press('j'));
        }
        app.update(key(KeyCode::Enter));

        let frames = 400;
        let started = Instant::now();
        for _ in 0..frames {
            app.update(press('j'));
            terminal
                .draw(|frame| app.render(frame.area(), frame))
                .expect("drawing should work");
        }
        let each = started.elapsed() / frames;
        eprintln!(
            "400-line diff: {} µs per frame ({} build)",
            each.as_micros(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        // 530 µs released and 3.2 ms unoptimized on the author's machine, both
        // recorded in the task. Four hundred operations is a whole-library
        // organize; the bound is loose enough to survive a loaded test runner.
        assert!(
            each < Duration::from_millis(10),
            "{} µs per frame is not smooth scrolling",
            each.as_micros()
        );
    }

    #[test]
    fn the_help_over_the_pending_view_lists_the_keys_that_view_has() {
        let fx = Fixture::realistic();
        let (mut app, _rx) = app(&fx);
        let mut terminal = screen(100, 30);
        app.update(scanned(&fx));
        stage_the_album_move(&mut app);

        app.update(press('?'));
        draw(&mut app, &mut terminal);
        let shown = text(&terminal);
        assert!(shown.contains("help · pending"), "{shown}");
        assert!(shown.contains("unstage this operation"), "{shown}");
        assert!(shown.contains("dd"), "the keys it is bound to: {shown}");

        // And closing it leaves the plan and the view exactly where they were.
        app.update(key(KeyCode::Esc));
        assert!(matches!(app.views.last(), Some(View::Pending(_))));
        assert_eq!(app.plan.len(), 1);
    }
}
