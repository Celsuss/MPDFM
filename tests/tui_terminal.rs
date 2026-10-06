//! The promise task 20 exists for: **the terminal is always restored.**
//!
//! Everything else about the shell is tested in process, against a
//! `TestBackend`, in `src/tui/`. This file tests the part that has no in-process
//! equivalent: raw mode and the alternate screen are properties of a real
//! terminal, and a `Vec<u8>` cannot be left in raw mode.
//!
//! # How a test gets a terminal
//!
//! Through `script(1)`, which allocates a pseudo-terminal, runs a command on it
//! and records everything written to it. Inside that pty the test can do the thing
//! the task's criteria actually ask for:
//!
//! ```text
//! stty -g > before   →   mpdfm …   →   stty -g > after
//! ```
//!
//! `stty -g` is the terminal's complete mode set in one line, and comparing the
//! two files is exactly "`stty -a` unchanged before and after" — the criterion,
//! mechanised. It is a stronger check than looking for escape sequences in the
//! output, because it would catch a restore that wrote all the right sequences and
//! forgot `tcsetattr`. It did catch one, during this task.
//!
//! # Hermetic, and read-only
//!
//! Each test runs against a [`World`] fixture with the environment cleared, and
//! the TUI's only interaction with the library is the startup scan, which reads.
//! There is no code path in these tests that opens a library file for writing —
//! and `assert_hermetic` asks the binary itself where it is looking, as every
//! other test in this directory does.
//!
//! # If `script` is missing
//!
//! It is part of util-linux and present on the development machine and on every
//! reasonable CI image, but it is not Rust and not vendored. A run without it
//! reports a skip rather than a failure, and `docs/tasks/20-tui-shell.md` records
//! the same checks as a `just verify-tui` recipe for doing them by hand.

#![cfg(unix)]

mod harness;

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use harness::World;

/// Everything one run through a pty produced.
struct PtyRun {
    /// What the binary exited with.
    code: i32,
    /// Everything written to the terminal, escapes and all.
    output: String,
    /// The terminal's modes before the binary ran.
    before: String,
    /// And after. Equal to `before` is the whole point.
    after: String,
}

impl PtyRun {
    /// The criterion, as one assertion: the terminal is in the state it was found
    /// in.
    fn assert_terminal_restored(&self) {
        assert_eq!(
            self.before,
            self.after,
            "the terminal's modes changed across the run.\n\
             before: {}\nafter:  {}\noutput: {}",
            self.before,
            self.after,
            self.visible()
        );
    }

    /// The output with escapes spelled out, so a failure message is readable.
    fn visible(&self) -> String {
        self.output.replace('\u{1b}', "ESC")
    }

    /// Where in the output a sequence first appears, for an ordering assertion.
    fn position_of(&self, needle: &str) -> Option<usize> {
        self.output.find(needle)
    }
}

/// The pty's size, in columns and rows. Comfortably above the 60x15 floor, so
/// these tests see the real layout; the floor itself is tested in `src/tui/app.rs`,
/// where a terminal can be any size for free.
const PTY_SIZE: (u16, u16) = (100, 30);

/// `CSI ?1049h` / `CSI ?1049l` — enter and leave the alternate screen.
const ENTER_ALT: &str = "\u{1b}[?1049h";
const LEAVE_ALT: &str = "\u{1b}[?1049l";
/// `CSI ?25h` — show the cursor.
const SHOW_CURSOR: &str = "\u{1b}[?25h";

/// Whether `script(1)` is available. Without it there is no pty to test on.
fn have_script() -> bool {
    Command::new("script")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Say why a test did nothing, loudly enough to be noticed in `cargo test` output.
fn skip(test: &str) {
    eprintln!(
        "SKIPPED {test}: `script` (util-linux) is not installed, so there is no \
         pseudo-terminal to test the restore on. See docs/tasks/20-tui-shell.md."
    );
}

/// Run the binary inside a pseudo-terminal and report what happened to the
/// terminal.
///
/// `shell` is the command line to run — `$BIN` is the binary and `$ARGS` the
/// hermetic `--config` flag, both exported — and `keys` is typed at it. The
/// surrounding script captures `stty -g` on both sides of it, which is the
/// measurement these tests exist for.
fn pty(world: &World, scratch: &Utf8Path, shell: &str, keys: &str) -> PtyRun {
    std::fs::create_dir_all(scratch).expect("the fixture root is writable");
    let runner = scratch.join("run.sh");
    let keyfile = scratch.join("keys");
    let before = scratch.join("before");
    let after = scratch.join("after");
    let status = scratch.join("status");
    let recorded = scratch.join("output");

    std::fs::write(
        &runner,
        format!(
            // `script` has nothing to inherit a window size from — both its ends
            // are files — so the pty starts at 0x0 and a TUI on it draws an empty
            // frame. Sizing it first is what makes these tests see a real one.
            // The size is not part of `stty -g`, so it does not disturb the
            // before/after comparison, but it is set before `before` is captured
            // anyway.
            "stty rows {rows} cols {cols}\n\
             stty -g > {before:?}\n\
             {shell}\n\
             echo $? > {status:?}\n\
             stty -g > {after:?}\n",
            rows = PTY_SIZE.1,
            cols = PTY_SIZE.0,
            before = before.as_str(),
            after = after.as_str(),
            status = status.as_str(),
        ),
    )
    .expect("writing the runner");
    std::fs::write(&keyfile, keys).expect("writing the keys");

    let mut command = Command::new("script");
    command
        .env_clear()
        // A real terminal type: `TERM=dumb`, which the rest of this directory
        // uses to keep assertions text-only, is not a terminal a TUI should be
        // asked to draw on.
        .env("TERM", "xterm-256color")
        .env("BIN", env!("CARGO_BIN_EXE_mpdfm"))
        .env("CONFIG", world.config_file().as_str())
        .args(["--quiet", "--return", "--command"])
        .arg(format!("sh {runner}"))
        .arg("/dev/null")
        .stdin(std::fs::File::open(&keyfile).expect("the key file is readable"))
        .stdout(std::fs::File::create(&recorded).expect("the fixture root is writable"))
        .stderr(std::process::Stdio::piped());

    for (name, value) in world.env() {
        // `TERM` is overridden above; everything else is the hermetic set.
        if name != "TERM" {
            command.env(name, value);
        }
    }

    let out = command.output().expect("`script` should run");
    assert!(
        out.status.success(),
        "`script` itself failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let read = |path: &Utf8PathBuf, what: &str| {
        std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("the runner should have written {what}: {err}"))
    };
    PtyRun {
        code: read(&status, "the exit status")
            .trim()
            .parse()
            .expect("an exit status is a number"),
        // Lossy: the recording holds whatever the terminal was sent, and a frame
        // of box-drawing characters cut off mid-run need not be valid UTF-8.
        output: String::from_utf8_lossy(&std::fs::read(&recorded).expect("read the recording"))
            .into_owned(),
        before: read(&before, "the modes before the run"),
        after: read(&after, "the modes after the run"),
    }
}

/// A scratch directory inside the fixture, named after the test using it.
fn scratch(world: &World, name: &str) -> Utf8PathBuf {
    world.fixture().root().join(format!("pty-{name}"))
}

/// The one TUI check that needs no pty, and the reason the others do: with stdout
/// redirected there is no terminal to put into raw mode, and MPDFM says so instead
/// of writing escape sequences into whatever the user redirected it to.
#[test]
fn a_run_with_no_terminal_refuses_instead_of_drawing_into_a_file() {
    let world = World::realistic();
    let run = world.run(&[]);

    run.assert_code(1);
    run.assert_stderr("raw mode");
    // What is missing, and where to go instead.
    run.assert_stderr("needs a terminal");
    run.assert_stderr("--help");
    assert!(
        run.stdout.is_empty(),
        "nothing should have been drawn: {:?}",
        run.stdout
    );
}

#[test]
fn q_quits_and_leaves_the_terminal_exactly_as_it_was() {
    if !have_script() {
        skip("q_quits_and_leaves_the_terminal_exactly_as_it_was");
        return;
    }
    let world = World::realistic();
    world.assert_hermetic();

    let run = pty(
        &world,
        &scratch(&world, "quit"),
        "\"$BIN\" --config \"$CONFIG\" --no-mpd",
        "q\n",
    );

    assert_eq!(run.code, 0, "`q` is a clean exit: {}", run.visible());
    run.assert_terminal_restored();

    // It drew: the alternate screen was entered, a frame was written in it, and it
    // was left again.
    let visible = run.visible();
    assert!(
        visible.contains("MPDFM"),
        "a frame should have been drawn: {visible}"
    );
    assert!(visible.contains("ESC[?1049h"), "{visible}");
    assert!(visible.contains("ESC[?1049l"), "{visible}");
    assert!(
        run.position_of(ENTER_ALT) < run.position_of(LEAVE_ALT),
        "entered and then left, in that order: {visible}"
    );
    // And the cursor the TUI hid is visible again.
    assert!(visible.contains("ESC[?25h"), "{visible}");
    assert!(
        run.position_of(SHOW_CURSOR) > run.position_of(ENTER_ALT),
        "the cursor is shown on the way out, not on the way in: {visible}"
    );
}

#[test]
fn an_induced_panic_restores_the_terminal_before_it_prints_the_backtrace() {
    if !have_script() {
        skip("an_induced_panic_restores_the_terminal_before_it_prints_the_backtrace");
        return;
    }
    let world = World::realistic();

    // The worst moment to panic: inside the draw callback, with the cursor hidden
    // and the alternate screen active.
    let run = pty(
        &world,
        &scratch(&world, "panic"),
        "MPDFM_TUI_PANIC=draw RUST_BACKTRACE=1 \"$BIN\" --config \"$CONFIG\" --no-mpd",
        "q\n",
    );

    assert_eq!(run.code, 101, "a panic exits 101: {}", run.visible());
    // The criterion that matters most: the shell is usable afterwards.
    run.assert_terminal_restored();

    let visible = run.visible();
    assert!(visible.contains("panicked at"), "{visible}");
    assert!(
        visible.contains("panicking inside the draw callback on purpose"),
        "{visible}"
    );
    // Readable, which means a backtrace and not one line.
    assert!(visible.contains("stack backtrace"), "{visible}");
    assert!(visible.contains("App::render"), "{visible}");

    // And the whole point of the hook: the message was printed *after* the
    // alternate screen was left, so it survives on the normal screen instead of
    // being discarded with the frame it was drawn over.
    let left = run
        .position_of(LEAVE_ALT)
        .expect("the panic hook leaves the alternate screen");
    let panicked = run
        .position_of("panicked at")
        .expect("the panic message is printed");
    assert!(
        left < panicked,
        "the terminal must be restored before anything is printed: {visible}"
    );
}

#[test]
fn sigterm_exits_cleanly_through_the_guard() {
    if !have_script() {
        skip("sigterm_exits_cleanly_through_the_guard");
        return;
    }
    let world = World::realistic();

    // Backgrounded inside the pty so the script can signal it, then waited for so
    // that `$?` is the binary's own status and not the shell's.
    let run = pty(
        &world,
        &scratch(&world, "sigterm"),
        "\"$BIN\" --config \"$CONFIG\" --no-mpd & pid=$!; sleep 1; kill -TERM $pid; wait $pid",
        "",
    );

    assert_eq!(
        run.code,
        0,
        "SIGTERM is a clean exit, not a death: {}",
        run.visible()
    );
    run.assert_terminal_restored();

    let visible = run.visible();
    // It got as far as drawing, and then left properly rather than being killed
    // mid-frame.
    assert!(visible.contains("ESC[?1049h"), "{visible}");
    assert!(visible.contains("ESC[?1049l"), "{visible}");
}

#[test]
fn no_alt_screen_draws_in_place_and_still_restores_the_terminal() {
    if !have_script() {
        skip("no_alt_screen_draws_in_place_and_still_restores_the_terminal");
        return;
    }
    let world = World::realistic();

    let run = pty(
        &world,
        &scratch(&world, "no-alt"),
        "\"$BIN\" --config \"$CONFIG\" --no-mpd --no-alt-screen",
        "q\n",
    );

    assert_eq!(run.code, 0, "{}", run.visible());
    run.assert_terminal_restored();

    let visible = run.visible();
    assert!(
        !visible.contains("1049"),
        "--no-alt-screen must touch neither screen: {visible}"
    );
    // The frame is still drawn, and the cursor still comes back.
    assert!(visible.contains("MPDFM"), "{visible}");
    assert!(visible.contains("ESC[?25h"), "{visible}");
}

/// `SIGWINCH`, end to end: the terminal is shrunk below the floor under a running
/// session and then grown again.
///
/// The floor itself is tested in `src/tui/` against any size for free; what is only
/// testable here is that a real resize *arrives* — `crossterm` turns `SIGWINCH`
/// into an `Event::Resize`, and this is the one check that the signal reaches the
/// loop at all — and that the app comes back rather than staying in the
/// too-small state.
#[test]
fn a_resize_below_the_floor_shows_the_message_and_recovers_when_it_grows_back() {
    if !have_script() {
        skip("a_resize_below_the_floor_shows_the_message_and_recovers_when_it_grows_back");
        return;
    }
    let world = World::realistic();

    let run = pty(
        &world,
        &scratch(&world, "resize"),
        // Backgrounded so the script can resize the terminal under it. The
        // sleeps are a second each: nothing here is racing, the frames are
        // sub-millisecond, and a shorter wait would only make the test flaky on a
        // loaded machine.
        "\"$BIN\" --config \"$CONFIG\" --no-mpd & pid=$!; sleep 1;          stty rows 10 cols 40; sleep 1;          stty rows 30 cols 100; sleep 1;          kill -TERM $pid; wait $pid",
        "",
    );

    assert_eq!(run.code, 0, "{}", run.visible());
    run.assert_terminal_restored();

    let visible = run.visible();
    assert!(
        visible.contains("terminal too small"),
        "40x10 should have been refused: {visible}"
    );
    assert!(visible.contains("have 40x10"), "{visible}");

    // And it recovered: the header is drawn again *after* the message, which is
    // the half of the criterion a one-shot check would miss.
    let too_small = visible
        .find("terminal too small")
        .expect("the message is there");
    assert!(
        visible[too_small..].contains("MPDFM"),
        "the layout should come back when the terminal does: {visible}"
    );
}

#[test]
fn the_log_file_gets_the_session_and_the_terminal_gets_none_of_it() {
    if !have_script() {
        skip("the_log_file_gets_the_session_and_the_terminal_gets_none_of_it");
        return;
    }
    let world = World::realistic();
    let dir = scratch(&world, "log");
    std::fs::create_dir_all(&dir).expect("the fixture root is writable");
    let log = dir.join("session.log");

    // Ended by a signal after a second rather than by a keystroke: a `q` that is
    // already in the pty's buffer is handled before the scan worker reports, and
    // this test wants a whole session in the log.
    let run = pty(
        &world,
        &dir,
        &format!(
            "\"$BIN\" --config \"$CONFIG\" --no-mpd -v --log {:?} &              pid=$!; sleep 1; kill -TERM $pid; wait $pid",
            log.as_str()
        ),
        "",
    );

    assert_eq!(run.code, 0, "{}", run.visible());
    run.assert_terminal_restored();

    let recorded = std::fs::read_to_string(&log).expect("the log should have been written");
    assert!(recorded.contains("scan: walking"), "{recorded}");
    assert!(recorded.contains("msg: scan-done"), "{recorded}");
    assert!(recorded.contains("shutdown: asked to stop"), "{recorded}");
    assert!(recorded.contains("loop: leaving"), "{recorded}");

    // The other half: none of it reached the screen. `-v` is on, which on every
    // other command writes to stderr — inside the TUI it must not, because stderr
    // is the terminal the frame is on.
    let visible = run.visible();
    for leaked in [
        "msg: scan-done",
        "loop: leaving",
        "view: push",
        "scan: walking",
    ] {
        assert!(
            !visible.contains(leaked),
            "{leaked:?} reached the terminal while the TUI owned it:\n{visible}"
        );
    }
}
