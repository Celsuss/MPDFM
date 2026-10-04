//! Driving the real `mpdfm` binary against a [`Fixture`].
//!
//! The tests in this directory run the built executable rather than calling
//! `cli::run` in process, because that is the path the user takes: argument
//! parsing, configuration discovery, the prompt, the exit code and the two
//! output streams are all part of what is being tested, and three of those five
//! do not exist for an in-process call.
//!
//! # Hermetic by construction
//!
//! Two things keep every run inside the fixture's temp directory, and they are
//! independent of each other on purpose:
//!
//! 1. **the environment is cleared**, and `HOME` and the three `XDG_*`
//!    variables are pointed at the fixture. Every default path MPDFM can derive
//!    — `~/.config/mpd/playlists`, `~/.local/share/mpdfm`, the `mpd.conf`
//!    search — therefore lands inside the fixture even if nothing else is set;
//! 2. **a `config.toml` in the fixture names all four roots explicitly**, and
//!    is passed with `--config`, so nothing is being inferred.
//!
//! [`World::assert_hermetic`] then asks the binary itself where it is looking
//! (`mpdfm config show --json`) and checks every answer against
//! [`real_library_roots`] — the same guard `mpdfm_core::testing` uses. That is
//! the acceptance criterion "a guard proves no test touched `~/Music` or
//! `~/.config/mpd`", answered with the binary's own report rather than with an
//! assurance.
//!
//! The guard cannot prove a *read* did not happen; what it proves is that none
//! of the paths this process would read or write is one of the real ones. For
//! the tiers where that is not enough — anything that writes — the rule is the
//! task's: tests only ever run against a `Fixture`.

#![allow(dead_code, reason = "each test file uses a different part of this")]

use assert_cmd::Command;
use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::testing::{Fixture, Snapshot, real_library_roots};

/// A fixture library, plus the configuration file that points the binary at it.
pub struct World {
    pub fx: Fixture,
    config_file: Utf8PathBuf,
}

impl World {
    /// Wrap a fixture and write the `config.toml` that names its four roots.
    ///
    /// `mpd_enabled` is false: these tests are about the CLI, and a developer
    /// with MPD running on the default port must not get different results from
    /// one without. The commands that talk to MPD have their own tests in
    /// `crates/core/tests/mpd.rs`.
    pub fn new(fx: Fixture) -> Self {
        let config_file = fx.root().join("config.toml");
        std::fs::write(
            &config_file,
            format!(
                "music_dir = {music:?}\n\
                 playlist_dir = {playlists:?}\n\
                 data_dir = {data:?}\n\
                 state_file = {state:?}\n\
                 mpd_enabled = false\n\
                 trigger_update_after_commit = false\n",
                music = fx.music_dir().as_str(),
                playlists = fx.playlist_dir().as_str(),
                data = fx.data_dir().as_str(),
                state = fx.state_file().as_str(),
            ),
        )
        .expect("the fixture root is writable");

        Self { fx, config_file }
    }

    /// The full library of `docs/PLAN.md` §3, in about 30 files.
    pub fn realistic() -> Self {
        Self::new(Fixture::realistic())
    }

    /// A command, with the environment cleared and pointed at the fixture.
    ///
    /// `NO_COLOR` and `TERM=dumb` so that assertions are against text and not
    /// against escape sequences; `COLUMNS` so the preview's width is the same on
    /// every machine.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("mpdfm").expect("the binary is built by `cargo test`");
        cmd.env_clear()
            .env("HOME", self.fx.root())
            .env("XDG_CONFIG_HOME", self.fx.root().join("xdg-config"))
            .env("XDG_DATA_HOME", self.fx.root().join("xdg-data"))
            .env("XDG_CACHE_HOME", self.fx.root().join("xdg-cache"))
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("COLUMNS", "100")
            .arg("--config")
            .arg(self.config_file.as_str())
            .args(args);
        cmd
    }

    /// Run a command with no stdin at all — a pipe, as a script would give it.
    pub fn run(&self, args: &[&str]) -> Run {
        Run::of(self.cmd(args).write_stdin(""))
    }

    /// Run a command with `--json` and parse what it printed.
    ///
    /// Also asserts, through [`Run::json`], that stdout holds one JSON document
    /// and nothing else — a stray `println!` on the `--json` path shows up as a
    /// parse failure here rather than as a surprise in somebody's script.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut with_json = args.to_vec();
        with_json.push("--json");
        let run = self.run(&with_json);
        run.assert_code(0);
        run.json()
    }

    /// Run a command with somebody at the keyboard, who types `reply`.
    ///
    /// [`output::ASSUME_TTY`] is how the prompt is reachable without a
    /// pseudo-terminal; see its documentation for why that seam exists.
    pub fn answer(&self, args: &[&str], reply: &str) -> Run {
        Run::of(
            self.cmd(args)
                .env("MPDFM_ASSUME_TTY", "1")
                .write_stdin(format!("{reply}\n")),
        )
    }

    /// The music and playlist directories, byte for byte.
    pub fn snapshot(&self) -> State {
        State {
            music: Snapshot::capture(self.fx.music_dir()),
            playlists: Snapshot::capture(self.fx.playlist_dir()),
        }
    }

    /// An absolute path inside the music directory.
    pub fn abs(&self, rel: &str) -> Utf8PathBuf {
        self.fx.abs(rel)
    }

    /// Ask the binary where it is looking, and refuse every answer that is not
    /// inside this fixture.
    ///
    /// Call it from any test that runs a command. It is cheap — one extra
    /// process — and it is the only check in this directory that would notice a
    /// configuration mistake pointing a *writing* test at `~/Music`.
    pub fn assert_hermetic(&self) {
        let shown = self.run(&["config", "show", "--json"]);
        shown.assert_code(0);

        let document = shown.json();
        let settings = document["settings"]
            .as_object()
            .expect("`config show --json` has a settings object");
        let guarded = real_library_roots();

        for (name, setting) in settings {
            let Some(value) = setting["value"].as_str() else {
                continue;
            };
            // Only the ones that are paths; `mpd_address` and the booleans are
            // not, and `state_file` may legitimately be absent.
            if !value.starts_with('/') {
                continue;
            }
            let path = Utf8Path::new(value);
            for root in &guarded {
                assert!(
                    !path.starts_with(root),
                    "{name} resolved to {value}, which is inside the real {root}"
                );
            }
            assert!(
                path.starts_with(self.fx.root()),
                "{name} resolved to {value}, which is outside the fixture at {}",
                self.fx.root()
            );
        }
    }
}

/// The two directories whose bytes a move must leave recoverable.
pub struct State {
    pub music: Snapshot,
    pub playlists: Snapshot,
}

impl State {
    /// Byte-for-byte identical, or a failure naming only what differs.
    pub fn assert_same(&self, other: &Self) {
        self.music.assert_same(&other.music);
        self.playlists.assert_same(&other.playlists);
    }

    /// Different in at least one way — the assertion a `--dry-run` test needs
    /// the *negation* of, and the one that proves a move test moved something.
    pub fn assert_differs(&self, other: &Self) {
        assert!(
            !self.music.diff(&other.music).is_empty()
                || !self.playlists.diff(&other.playlists).is_empty(),
            "nothing changed, but something should have"
        );
    }
}

/// What a run produced.
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    fn of(cmd: &mut Command) -> Self {
        let output = cmd.output().expect("the binary runs");
        Self {
            // `None` is a signal, which on this platform means the process was
            // killed — never an exit code, so it must not be mistaken for one.
            code: output
                .status
                .code()
                .unwrap_or_else(|| panic!("mpdfm was killed by a signal: {:?}", output.status)),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Assert the exit code, printing both streams when it is wrong — which is
    /// the difference between a one-line failure and half an hour.
    pub fn assert_code(&self, want: i32) -> &Self {
        assert_eq!(
            self.code, want,
            "expected exit {want}, got {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        );
        self
    }

    /// stdout parsed as JSON, which also asserts that it *is* JSON and nothing
    /// else — a stray `println!` on the `--json` path shows up here.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|err| {
            panic!(
                "stdout is not one JSON document ({err})\n--- stdout ---\n{}\n--- stderr ---\n{}",
                self.stdout, self.stderr
            )
        })
    }

    /// Assert stdout contains this, naming both streams if it does not.
    pub fn assert_stdout(&self, needle: &str) -> &Self {
        assert!(
            self.stdout.contains(needle),
            "stdout does not contain {needle:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    /// Assert stderr contains this.
    pub fn assert_stderr(&self, needle: &str) -> &Self {
        assert!(
            self.stderr.contains(needle),
            "stderr does not contain {needle:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}
