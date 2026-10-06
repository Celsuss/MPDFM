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
//! Keys are hardcoded here and belong to task 21, which replaces this `match` with
//! a keymap. The ones that exist are the ones the criteria need: `q`, `?`, `esc`,
//! `R`, and the cursor.
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

use super::event::Events;
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
    /// The key help. Task 26 generates it from the keymap; this is the stack's
    /// first customer and exists to prove the stack.
    Help,
    /// Something went wrong, and it is not going away on a timer. Task 26 turns
    /// this into the full panel with the path and the suggested next step.
    Error(String),
}

impl View {
    /// Whether this view is drawn over the one below it rather than instead of it.
    fn is_overlay(&self) -> bool {
        !matches!(self, Self::Browser)
    }

    /// The name the status bar shows and the log records.
    fn name(&self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Help => "help",
            Self::Error(_) => "error",
        }
    }
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
    pub fn new(config: Config, tx: Sender<Msg>, log: Arc<Log>) -> Self {
        Self {
            config,
            library: None,
            index: None,
            plan: Plan::new(),
            focus: Focus::Tree,
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

    /// A keypress. Task 21 replaces this with a keymap; the bindings here are the
    /// ones task 20's criteria need, and `docs/tasks/21-keymap.md` is the list they
    /// will become.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        // In raw mode the terminal does not turn `ctrl-c` into a signal, so it
        // arrives as a key. Honouring it is kindness: a user who wants out reaches
        // for it before they read the help.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return false;
        }

        match key.code {
            KeyCode::Char('q') => {
                // Task 21 asks here when the plan is not empty. Nothing can stage
                // an operation yet, so there is nothing to ask about.
                self.quit = true;
                false
            }
            KeyCode::Char('?') => self.push(View::Help),
            KeyCode::Esc => self.pop(),
            KeyCode::Char('R') => {
                self.rescan();
                true
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') => self.set_cursor(0),
            KeyCode::Char('G') => self.set_cursor(self.row_count().saturating_sub(1)),
            KeyCode::Tab => {
                self.focus = self.focus.toggled();
                true
            }
            _ => false,
        }
    }

    /// The slow tick: retire the message that has had its turn, and ask MPD what
    /// it is doing.
    ///
    /// Returns true only when something on screen actually changed, which is what
    /// keeps an idle `mpdfm` off the CPU. The poll it starts is a thread; the answer
    /// arrives later as [`Msg::MpdStatus`] and redraws then if it differs.
    fn on_tick(&mut self) -> bool {
        let retired = self.retire_toast();

        if !self.mpd_in_flight {
            self.mpd_in_flight = true;
            work::poll_mpd(self.tx.clone(), self.config.clone(), Arc::clone(&self.log));
        }

        retired
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

        let [header, body, status, message] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        frame.render_widget(self.header(), header);
        // The base view, always — an overlay is drawn over it, not instead of it,
        // which is what makes "popping does not lose state" visible as well as
        // true.
        self.render_browser(body, frame);
        frame.render_widget(self.status_bar(), status);
        frame.render_widget(self.message_line(), message);

        for view in self.views.iter().filter(|view| view.is_overlay()) {
            self.render_overlay(view, body, frame);
        }
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

    /// An overlay, centred over the body.
    fn render_overlay(&self, view: &View, body: Rect, frame: &mut ratatui::Frame) {
        let (title, text, color) = match view {
            View::Browser => return,
            View::Help => (" help ", HELP.to_owned(), Color::Cyan),
            View::Error(message) => (
                " error ",
                format!("{message}\n\nesc to dismiss"),
                Color::Red,
            ),
        };

        // Three quarters of the body, centred: wide enough for a path, and it
        // leaves the browser visible around the edges so that it is obvious the
        // overlay is on top of something rather than instead of it.
        let [area] = Layout::horizontal([Constraint::Percentage(75)])
            .flex(Flex::Center)
            .areas(body);
        let [area] = Layout::vertical([Constraint::Percentage(75)])
            .flex(Flex::Center)
            .areas(area);

        // Without this the browser's rows show through the gaps in the text.
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(color))
                    .title(title),
            ),
            area,
        );
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

    /// The bottom line: the toast, or the scan's progress, or nothing.
    fn message_line(&self) -> Paragraph<'_> {
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
            ScanState::Idle | ScanState::Done { .. } => "? help · q quit · R rescan".to_owned(),
        };
        Paragraph::new(Line::from(text).style(Style::new().fg(Color::DarkGray)))
    }
}

/// The help text. Task 26 generates this from task 21's keymap, at which point it
/// cannot document a binding that does not exist; until then it documents exactly
/// the keys `on_key` handles.
const HELP: &str = "\
j / k / ↓ / ↑   move
g / G          top / bottom
tab            switch pane
R              rescan the library
?              this help
esc            close an overlay
q / ctrl-c     quit";

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
        (App::new(fx.config(), tx, Arc::new(Log::off())), rx)
    }

    /// A key press, as the input thread would deliver it.
    fn key(code: KeyCode) -> Msg {
        Msg::Input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// A char key press.
    fn press(c: char) -> Msg {
        key(KeyCode::Char(c))
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
        assert_eq!(app.views.last(), Some(&View::Help));
        terminal
            .draw(|frame| app.render(frame.area(), frame))
            .expect("drawing should work");
        let with_help = text(&terminal);
        assert!(with_help.contains("rescan the library"), "{with_help}");
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
        assert!(!after.contains("rescan the library"), "{after}");
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
        let mut app = App::new(fx.config(), tx, Arc::new(Log::off()));
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

    // -- the log -----------------------------------------------------------

    #[test]
    fn the_log_records_the_session_and_nothing_reaches_the_terminal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("session.log"))
            .expect("temp path is UTF-8");

        let fx = Fixture::realistic();
        let (tx, _rx) = mpsc::channel();
        let log = Arc::new(Log::to_file(&path).expect("a log in a temp dir opens"));
        let mut app = App::new(fx.config(), tx, log);
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
