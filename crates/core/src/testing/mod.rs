//! A throwaway music library in a temp directory, and the guard that keeps tests
//! away from the real one.
//!
//! Everything in M1 is a change to files that MPD and the user both care about,
//! so none of it can be verified by reasoning — it has to be run against a tree
//! that has the same ugly properties as the real library: non-ASCII names,
//! spaces, brackets, `&`, `+`, apostrophes, a multi-disc album, scene clutter
//! next to the audio, a playlist that is a symlink into a dotfiles repo, a CUE
//! virtual track, a radio-URL playlist, and one reference that was already broken
//! before MPDFM ever ran. [`Fixture::realistic`] builds exactly that, from the
//! facts recorded in `docs/PLAN.md` §3.
//!
//! ```
//! # #[cfg(unix)] {
//! use mpdfm_core::testing::{Fixture, names};
//!
//! let fx = Fixture::realistic();
//! let before = fx.snapshot();
//!
//! // ... an operation under test, which must stay inside the fixture ...
//! fx.assert_inside(&fx.music_dir().join(names::MF_DOOM_TRACK));
//!
//! before.assert_same(&fx.snapshot()); // nothing changed yet
//! # }
//! ```
//!
//! # Conventions
//!
//! **Errors panic.** A fixture that cannot be built is a broken test, not a case
//! to handle, so every method panics with the path it failed on instead of
//! returning a `Result` that every call site would have to unwrap.
//!
//! **Unix only.** The builder makes symlinks and the snapshot records permission
//! bits, both of which MPDFM's playlist handling depends on. This is a Linux tool
//! (`docs/PLAN.md` §3); the `testing` feature does not pretend otherwise.
//!
//! **Nothing is shared between fixtures.** Each one owns a [`tempfile::TempDir`]
//! that is removed when it drops, so tests can run in parallel and leave nothing
//! in `/tmp`.

mod audio;
mod snapshot;

pub use audio::AudioTemplate;
pub use snapshot::{Entry as SnapshotEntry, EntryKind as SnapshotEntryKind, Snapshot, digest};

use camino::{Utf8Path, Utf8PathBuf};

use crate::paths::{self, RelPath};

/// Where the fixture puts each of the four roots MPDFM knows about, relative to
/// the fixture root. They mirror the real layout's *relationship* — the playlist
/// directory is not inside the music directory, the data directory is somewhere
/// else again — without copying the real absolute paths.
const MUSIC_DIR: &str = "music";
const PLAYLIST_DIR: &str = "playlists";
const STATE_FILE: &str = "mpd/state";
const DATA_DIR: &str = "data";

/// Default target directory for [`FixtureBuilder::symlinked_playlist`]: inside
/// the fixture (so the containment guard is satisfied) but outside the playlist
/// directory (so a rewrite has to follow the link to find it), which is exactly
/// how `Radios.m3u` sits in the real setup.
pub const DOTFILES_PLAYLISTS: &str = "dotfiles/mpd/playlists";

/// Tracks generated for a disc of [`FixtureBuilder::multi_disc`].
const DISC_TRACKS: &[&str] = &["01 Wrecked.mp3", "02 Sirens + Symphony.mp3"];

/// Tracks generated for [`FixtureBuilder::non_ascii_album`].
const NON_ASCII_TRACKS: &[&str] = &["01 So Hï.mp3", "02 Tänd Ljusen.mp3", "03 ノスタルジア.mp3"];

/// Tracks generated for [`FixtureBuilder::flac_album`].
const FLAC_TRACKS: &[&str] = &[
    "01 So What.flac",
    "02 Freddie Freeloader + alt take.flac",
    "03 Blue in Green.flac",
];

/// The names [`Fixture::realistic`] builds, so a test can refer to a track
/// without retyping an 80-character scene release directory and getting one
/// character wrong.
pub mod names {
    /// Album directory with `.`s inside a component, brackets and spaces.
    pub const MF_DOOM_ALBUM: &str = "hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag";
    /// A track referenced by two different playlists.
    pub const MF_DOOM_TRACK: &str =
        "hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3";

    /// Album directory with `&`, `+` and parentheses.
    pub const SNOOP_ALBUM: &str = "hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube";
    /// Track with an apostrophe in its name.
    pub const SNOOP_TRACK: &str = "hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3";

    /// The multi-disc set's root — an album directory with no audio directly in it.
    pub const MERCURY_ALBUM: &str =
        "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]";
    /// First disc directory of the set.
    pub const MERCURY_CD1: &str = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 1 - Mercury - Acts 1";
    /// Second disc directory of the set.
    pub const MERCURY_CD2: &str = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 2 - Mercury - Acts 2";
    /// A track on the first disc.
    pub const MERCURY_TRACK: &str = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 1 - Mercury - Acts 1/01 Wrecked.mp3";
    /// The `.cue` sheet next to the first disc's FLAC.
    pub const MERCURY_CUE: &str = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 1 - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue";
    /// The CUE virtual-track reference as a playlist spells it: the `.cue` file
    /// plus a `trackNNNN` component that is not a file at all.
    pub const MERCURY_CUE_TRACK: &str = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 1 - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue/track0017";

    /// Album directory whose name is not ASCII.
    pub const KREAM_ALBUM: &str = "electronic/KREAM - So Hï [c0D2h71bFFI]";
    /// Track whose name is not ASCII.
    pub const KREAM_TRACK: &str = "electronic/KREAM - So Hï [c0D2h71bFFI]/01 So Hï.mp3";

    /// The FLAC album's directory.
    pub const KIND_OF_BLUE_ALBUM: &str = "jazz/Miles Davis - Kind of Blue (1959) [FLAC]";
    /// A FLAC track.
    pub const KIND_OF_BLUE_TRACK: &str =
        "jazz/Miles Davis - Kind of Blue (1959) [FLAC]/01 So What.flac";

    /// A track in a directory with no album structure at all.
    pub const SWITCHANGEL_TRACK: &str = "coding-music/SwitchAngel/Coding_Trance.mp3";
    /// The one m4a — 5 of the real library's ~2 800 files are m4a.
    pub const SWITCHANGEL_M4A: &str = "coding-music/SwitchAngel/Coding_Trance_Reprise.m4a";

    /// The reference that does not resolve, and never did.
    pub const BROKEN_REFERENCE: &str = "pop/gone/missing.mp3";

    /// Playlist of plain relative paths.
    pub const HIP_HOP_PLAYLIST: &str = "Hip hop.m3u";
    /// Playlist that references [`MF_DOOM_TRACK`] as well, so a move has to
    /// rewrite two files.
    pub const MF_DOOM_PLAYLIST: &str = "MF Doom.m3u";
    /// Playlist holding the CUE virtual track and the broken reference.
    pub const POP_PLAYLIST: &str = "Pop.m3u";
    /// Playlist that is a symlink, and holds only URLs and `#EXT` lines.
    pub const RADIOS_PLAYLIST: &str = "Radios.m3u";
}

/// A throwaway music library on disk, deleted when this value drops.
///
/// Build one with [`Fixture::realistic`] for the full library, or with
/// [`Fixture::builder`] to assemble only the parts a test needs.
#[derive(Debug)]
pub struct Fixture {
    /// Owns the directory: dropping it removes the tree. Nothing ever reads it —
    /// living exactly as long as the `Fixture` is its whole job.
    #[expect(dead_code, reason = "held for its Drop impl, which deletes the tree")]
    temp: tempfile::TempDir,
    root: Utf8PathBuf,
    music_dir: Utf8PathBuf,
    playlist_dir: Utf8PathBuf,
    state_file: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    tracks: Vec<RelPath>,
    aux_files: Vec<RelPath>,
    playlists: Vec<String>,
    cue_references: Vec<String>,
    broken_references: Vec<String>,
}

impl Fixture {
    /// Start an empty fixture. Every builder method adds to it immediately.
    #[must_use]
    pub fn builder() -> FixtureBuilder {
        FixtureBuilder::new()
    }

    /// The whole library described in `docs/PLAN.md` §3, in about 30 files.
    ///
    /// This is what nearly every test wants. It contains, on purpose:
    /// non-ASCII names, spaces, brackets, `&`, `+`, an apostrophe, a `..` inside
    /// a component, a multi-disc album, aux files, a FLAC album, an m4a-free
    /// mp3-only album, a symlinked playlist, a CUE virtual-track reference, a
    /// radio-URL playlist, a track referenced from two playlists, a saved queue,
    /// and exactly one broken reference.
    #[must_use]
    pub fn realistic() -> Self {
        use names as n;

        Self::builder()
            .album(
                n::MF_DOOM_ALBUM,
                &[
                    "01 Beef Rap.mp3",
                    "02 Hoe Cakes.mp3",
                    "03 Potholderz (feat. Count Bass D).mp3",
                ],
            )
            // `.m3u` and `.log` inside an album directory: clutter that has to
            // travel with the album, and a `.m3u` the scanner must not mistake
            // for one of MPD's own playlists.
            .aux(
                n::MF_DOOM_ALBUM,
                &[
                    "folder.jpg",
                    "info.nfo",
                    "mm..food.sfv",
                    "eac.log",
                    "Mm..Food.m3u",
                ],
            )
            .album(
                n::SNOOP_ALBUM,
                &["01.Smokin' On.mp3", "02.Young, Wild & Free.mp3"],
            )
            .multi_disc(
                n::MERCURY_ALBUM,
                &["CD 1 - Mercury - Acts 1", "CD 2 - Mercury - Acts 2"],
            )
            .cue_reference(
                n::MERCURY_CD1,
                "Imagine Dragons - Mercury - Acts 1.flac.cue/track0017",
            )
            .non_ascii_album(n::KREAM_ALBUM)
            .flac_album(n::KIND_OF_BLUE_ALBUM)
            .album(
                "coding-music/SwitchAngel",
                &["Coding_Trance.mp3", "Coding_Trance_Reprise.m4a"],
            )
            .playlist(
                n::HIP_HOP_PLAYLIST,
                &[n::MF_DOOM_TRACK, "", "# and the soundtrack", n::SNOOP_TRACK],
            )
            .playlist(n::MF_DOOM_PLAYLIST, &[n::MF_DOOM_TRACK])
            .playlist(
                n::POP_PLAYLIST,
                &["#EXTM3U", n::MERCURY_CUE_TRACK, n::MERCURY_TRACK],
            )
            .broken_reference(n::POP_PLAYLIST, n::BROKEN_REFERENCE)
            .playlist_with_urls(n::RADIOS_PLAYLIST, &[])
            .symlinked_playlist(n::RADIOS_PLAYLIST, DOTFILES_PLAYLISTS)
            .state_file_queue(&[
                n::KREAM_TRACK,
                n::MF_DOOM_TRACK,
                n::KIND_OF_BLUE_TRACK,
                n::SWITCHANGEL_TRACK,
            ])
            .build()
    }

    /// The fixture root. Everything an operation touches must be under it.
    #[must_use]
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Stands in for MPD's `music_directory`.
    #[must_use]
    pub fn music_dir(&self) -> &Utf8Path {
        &self.music_dir
    }

    /// Stands in for MPD's `playlist_directory`.
    #[must_use]
    pub fn playlist_dir(&self) -> &Utf8Path {
        &self.playlist_dir
    }

    /// Stands in for `~/.config/mpd/state`. Always exists; its saved queue is
    /// empty unless [`FixtureBuilder::state_file_queue`] was called.
    #[must_use]
    pub fn state_file(&self) -> &Utf8Path {
        &self.state_file
    }

    /// Stands in for `~/.local/share/mpdfm` — journal and backups.
    #[must_use]
    pub fn data_dir(&self) -> &Utf8Path {
        &self.data_dir
    }

    /// The absolute path of something inside the music directory.
    ///
    /// Goes through [`RelPath`] rather than joining blindly, so a test that
    /// mistypes a path is told which path, here, instead of failing on a missing
    /// file three steps later.
    ///
    /// # Panics
    ///
    /// If `rel` is not a valid [`RelPath`].
    #[must_use]
    pub fn abs(&self, rel: &str) -> Utf8PathBuf {
        self.rel(rel).to_abs(self.music_dir())
    }

    /// Parse a relative path, panicking if it is not a valid [`RelPath`] — which
    /// in a test means a typo in the test itself.
    ///
    /// # Panics
    ///
    /// If `rel` is not a valid [`RelPath`].
    #[must_use]
    pub fn rel(&self, rel: &str) -> RelPath {
        RelPath::parse(rel).unwrap_or_else(|err| panic!("{rel:?} is not a RelPath: {err}"))
    }

    /// The playlist file **as found**, which for a symlinked playlist is the
    /// link and not its target.
    #[must_use]
    pub fn playlist_path(&self, name: &str) -> Utf8PathBuf {
        self.playlist_dir.join(name)
    }

    /// Every audio file the builder created, in the order it created them.
    #[must_use]
    pub fn tracks(&self) -> &[RelPath] {
        &self.tracks
    }

    /// Every non-audio file inside the music directory — the cover art, `.nfo`,
    /// `.sfv` and `.cue` clutter that has to travel with an album.
    #[must_use]
    pub fn aux_files(&self) -> &[RelPath] {
        &self.aux_files
    }

    /// Names of the playlists created, in creation order.
    #[must_use]
    pub fn playlists(&self) -> &[String] {
        &self.playlists
    }

    /// CUE virtual-track references, as a playlist spells them
    /// (`…/album.flac.cue/track0017`).
    #[must_use]
    pub fn cue_references(&self) -> &[String] {
        &self.cue_references
    }

    /// References written into a playlist that deliberately resolve to nothing.
    #[must_use]
    pub fn broken_references(&self) -> &[String] {
        &self.broken_references
    }

    /// A recursive digest of the entire fixture — music, playlists, the symlink
    /// target outside the playlist directory, the state file and the data
    /// directory.
    ///
    /// Capture one before an operation and one after; [`Snapshot::assert_same`]
    /// is the undo invariant in a single line.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::capture(self.root())
    }

    /// Whether `path` really is inside this fixture, symlinks resolved.
    ///
    /// Delegates to [`paths::contains`], so the guard tests exercise the same
    /// code the commit path will.
    #[must_use]
    pub fn contains_path(&self, path: &Utf8Path) -> bool {
        paths::contains(self.root(), path)
    }

    /// Panic unless `path` is inside this fixture.
    ///
    /// This is the test-suite half of safety invariant 5: no operation may read
    /// or write outside the configured roots. Hand it every path an operation is
    /// about to touch. It fails closed — a path it cannot resolve is treated as
    /// outside — and says so loudly when the path turns out to be the user's real
    /// library, because that is the mistake that actually costs something.
    ///
    /// # Panics
    ///
    /// If `path` is not inside the fixture.
    pub fn assert_inside(&self, path: &Utf8Path) {
        if self.contains_path(path) {
            return;
        }
        let real = if in_real_library(path) {
            "\n    THIS IS THE USER'S REAL LIBRARY. No test may read or write it."
        } else {
            ""
        };
        panic!(
            "path escapes the fixture:\n    path:    {path}\n    fixture: {}{real}",
            self.root()
        );
    }

    /// [`Fixture::assert_inside`] for a whole batch of paths.
    ///
    /// # Panics
    ///
    /// If any path is not inside the fixture.
    pub fn assert_all_inside<'a, I>(&self, paths: I)
    where
        I: IntoIterator<Item = &'a Utf8Path>,
    {
        for path in paths {
            self.assert_inside(path);
        }
    }

    /// Flip one bit of the last byte of a file, to prove that a check notices a
    /// one-byte change.
    ///
    /// Writes through a symlink on purpose: that is how the real `Radios.m3u`
    /// gets edited.
    ///
    /// # Panics
    ///
    /// If `path` is outside the fixture, unreadable, or empty.
    pub fn flip_byte(&self, path: &Utf8Path) {
        self.assert_inside(path);
        let mut bytes = read(path);
        let last = bytes
            .last_mut()
            .unwrap_or_else(|| panic!("cannot flip a byte of the empty file {path}"));
        *last ^= 0x01;
        write(path, &bytes);
    }
}

/// Builds a [`Fixture`] piece by piece, writing each piece to disk as it goes.
///
/// Methods take paths **relative to the music directory** (or, for playlists, a
/// bare file name) and panic on anything they cannot do, since a fixture that
/// half-built would only produce a confusing failure later.
#[derive(Debug)]
pub struct FixtureBuilder {
    fixture: Fixture,
}

impl FixtureBuilder {
    /// Create the temp directory and the four roots inside it.
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("mpdfm-fixture-")
            .tempdir()
            .expect("could not create a temp directory for the fixture");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .expect("the temp directory's path is not UTF-8");

        let fixture = Fixture {
            music_dir: root.join(MUSIC_DIR),
            playlist_dir: root.join(PLAYLIST_DIR),
            state_file: root.join(STATE_FILE),
            data_dir: root.join(DATA_DIR),
            root,
            temp,
            tracks: Vec::new(),
            aux_files: Vec::new(),
            playlists: Vec::new(),
            cue_references: Vec::new(),
            broken_references: Vec::new(),
        };

        // If the temp directory landed inside the real library — `$TMPDIR` set to
        // `~/Music`, say — stop before writing anything into it. The empty
        // directory itself is removed when this half-built fixture drops.
        assert!(
            !in_real_library(&fixture.root),
            "the temp directory {} is inside the user's real library; check $TMPDIR",
            fixture.root
        );

        for dir in [
            fixture.music_dir(),
            fixture.playlist_dir(),
            fixture.data_dir(),
        ] {
            create_dir_all(dir);
        }
        // Always present, so task 14 has a file to rewrite even when the test
        // never asked for a queue.
        write(fixture.state_file(), state_body(&[]).as_bytes());

        Self { fixture }
    }

    /// An album directory of audio files.
    ///
    /// `dir` is relative to the music directory and is usually
    /// `genre/album`; each name in `tracks` picks its container by extension.
    ///
    /// # Panics
    ///
    /// If a track name is not an audio file MPDFM handles.
    #[must_use]
    pub fn album(mut self, dir: &str, tracks: &[&str]) -> Self {
        for name in tracks {
            let template = AudioTemplate::for_file_name(name).unwrap_or_else(|| {
                panic!("{name:?} is not an audio file; put clutter in `aux` instead")
            });
            self.add_audio(&format!("{dir}/{name}"), template);
        }
        self
    }

    /// The clutter that ships next to a scene release and has to move with it:
    /// cover art, `.nfo`, `.sfv`, `.log`.
    ///
    /// Each file gets a short unique text body, so a snapshot can tell them
    /// apart. They are therefore *not* decodable images — task 30 (cover art)
    /// will want a real one, and should add it rather than assume `folder.jpg`
    /// is a JPEG.
    #[must_use]
    pub fn aux(mut self, dir: &str, files: &[&str]) -> Self {
        for name in files {
            let rel = format!("{dir}/{name}");
            let body = format!("MPDFM fixture aux file: {rel}\n");
            self.add_file(&rel, body.as_bytes(), FileRole::Aux);
        }
        self
    }

    /// A multi-disc set: an album directory that holds no audio itself, only
    /// `CD 1 …` / `CD 2 …` subdirectories that do.
    ///
    /// Task 05 has to recognise the discs as album directories and link them to
    /// the set root; task 27 has to keep them together when organizing.
    #[must_use]
    pub fn multi_disc(mut self, dir: &str, discs: &[&str]) -> Self {
        for disc in discs {
            self = self.album(&format!("{dir}/{disc}"), DISC_TRACKS);
        }
        self
    }

    /// An album whose directory and track names are not ASCII — 1 023 files in
    /// the real library are like this.
    #[must_use]
    pub fn non_ascii_album(self, dir: &str) -> Self {
        self.album(dir, NON_ASCII_TRACKS)
    }

    /// A FLAC album. 357 of the real library's files are FLAC, and a reorg that
    /// skipped them would be a trap (`docs/PLAN.md` D5).
    #[must_use]
    pub fn flac_album(self, dir: &str) -> Self {
        self.album(dir, FLAC_TRACKS)
    }

    /// A single audio file at an arbitrary path — a stray track with no album
    /// directory around it.
    #[must_use]
    pub fn track(mut self, rel: &str) -> Self {
        let template = AudioTemplate::for_file_name(rel)
            .unwrap_or_else(|| panic!("{rel:?} is not an audio file"));
        self.add_audio(rel, template);
        self
    }

    /// A playlist of exactly these lines, LF-terminated with a trailing newline.
    ///
    /// Lines are written verbatim — `""` for a blank line, `"# …"` for a comment
    /// — because the parser's contract (task 06) is byte fidelity, and a fixture
    /// that tidied its input could not test that.
    ///
    /// Creating a playlist that already exists overwrites it; to add a line, use
    /// [`FixtureBuilder::broken_reference`] or write to the file directly.
    #[must_use]
    pub fn playlist(mut self, name: &str, lines: &[&str]) -> Self {
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        self.add_playlist(name, body.as_bytes());
        self
    }

    /// The `Radios.m3u` shape: `#EXTM3U`, `# comment` lines, `#EXTINF:-1,Name`,
    /// blank lines and `http(s)://` URLs — every kind of playlist line that is
    /// *not* a track and must survive a rewrite byte-for-byte.
    ///
    /// `extra` is appended after the canned block.
    #[must_use]
    pub fn playlist_with_urls(self, name: &str, extra: &[&str]) -> Self {
        let mut lines: Vec<&str> = vec![
            "#EXTM3U",
            "# Lofi / Downtempo",
            "#EXTINF:-1,Lofi Radio",
            "https://play.streamafrica.net/lofiradio",
            "#EXTINF:-1,SomaFM - Groove Salad",
            "http://ice1.somafm.com/groovesalad-256-mp3",
            "",
            "# Liquid Drum & Bass",
            "#EXTINF:-1,Bassdrive",
            "http://ice.bassdrive.net/stream",
        ];
        lines.extend_from_slice(extra);
        self.playlist(name, &lines)
    }

    /// A playlist written from exact bytes, for the cases a line list cannot
    /// express: CRLF endings, a missing trailing newline, a UTF-8 BOM.
    #[must_use]
    pub fn playlist_raw(mut self, name: &str, bytes: &[u8]) -> Self {
        self.add_playlist(name, bytes);
        self
    }

    /// Move an existing playlist out to `target_dir` (relative to the fixture
    /// root, e.g. [`DOTFILES_PLAYLISTS`]) and leave an absolute symlink behind in
    /// its place — the shape `Radios.m3u` has in the real setup.
    ///
    /// A rewrite must edit the target and leave the link alone (task 06), so a
    /// test that writes through this without resolving it first replaces the
    /// link with a regular file, and the snapshot says so.
    ///
    /// If the playlist does not exist yet, an `#EXTM3U`-only one is created at
    /// the target.
    ///
    /// # Panics
    ///
    /// If the playlist is already a symlink.
    #[must_use]
    pub fn symlinked_playlist(mut self, name: &str, target_dir: &str) -> Self {
        let link = self.fixture.playlist_path(name);
        let target = self.fixture.root().join(target_dir).join(name);
        create_dir_all(target.parent().expect("a playlist target has a parent"));

        let existing = std::fs::symlink_metadata(&link);
        match existing {
            Ok(meta) if meta.is_symlink() => panic!("{link} is already a symlink"),
            Ok(_) => std::fs::rename(&link, &target)
                .unwrap_or_else(|err| panic!("cannot move {link} to {target}: {err}")),
            Err(_) => {
                write(&target, b"#EXTM3U\n");
                self.fixture.playlists.push(name.to_owned());
            }
        }
        std::os::unix::fs::symlink(&target, &link)
            .unwrap_or_else(|err| panic!("cannot link {link} -> {target}: {err}"));
        self
    }

    /// A CUE sheet and the FLAC it describes, plus the virtual-track reference a
    /// playlist would use.
    ///
    /// `dir` is the album directory; `spec` is `<name>.flac.cue/trackNNNN`. MPD
    /// treats the `trackNNNN` component as a track inside the `.cue` file, so
    /// the path in a playlist names something that is not a file — the parser
    /// (task 06) has to split it and the mover (task 09) has to keep the two
    /// halves together.
    ///
    /// # Panics
    ///
    /// If `spec` is not `<something>.cue/<track id>`.
    #[must_use]
    pub fn cue_reference(mut self, dir: &str, spec: &str) -> Self {
        let (cue_name, track_id) = spec
            .rsplit_once('/')
            .unwrap_or_else(|| panic!("{spec:?} should be `<name>.flac.cue/trackNNNN`"));
        let audio_name = cue_name
            .strip_suffix(".cue")
            .unwrap_or_else(|| panic!("{cue_name:?} should end in `.cue`"));

        self.add_audio(&format!("{dir}/{audio_name}"), AudioTemplate::Flac);

        // Real sheets are CRLF, which also means the snapshot's verbatim copy of
        // this file proves a rewrite did not silently normalize line endings.
        let title = dir.rsplit('/').next().unwrap_or(dir);
        let sheet = [
            "PERFORMER \"Various Artists\"".to_owned(),
            format!("TITLE \"{title}\""),
            format!("FILE \"{audio_name}\" WAVE"),
            format!("  TRACK {} AUDIO", cue_track_number(track_id)),
            "    TITLE \"Wrecked\"".to_owned(),
            "    INDEX 01 57:12:00".to_owned(),
            String::new(),
        ]
        .join("\r\n");
        self.add_file(
            &format!("{dir}/{cue_name}"),
            sheet.as_bytes(),
            FileRole::Aux,
        );

        self.fixture
            .cue_references
            .push(format!("{dir}/{cue_name}/{track_id}"));
        self
    }

    /// Append a reference to a playlist that resolves to nothing, and make sure
    /// the file it names really is absent.
    ///
    /// The real library has exactly one of these. Tools that "clean up" broken
    /// references silently are how playlists get destroyed, so tasks 07 and 29
    /// have to report it and leave it alone.
    ///
    /// # Panics
    ///
    /// If the referenced file actually exists.
    #[must_use]
    pub fn broken_reference(mut self, playlist: &str, rel: &str) -> Self {
        let absent = self.fixture.music_dir().join(rel);
        assert!(
            !absent.exists(),
            "{rel} exists, so it is not a broken reference"
        );

        let path = self.fixture.playlist_path(playlist);
        if !path.exists() {
            self.fixture.playlists.push(playlist.to_owned());
        }
        append_line(&path, rel);
        self.fixture.broken_references.push(rel.to_owned());
        self
    }

    /// Write MPD's state file with `queue` as the saved queue.
    ///
    /// The shape is the real one: `key: value` lines, then the queue between
    /// `playlist_begin` and `playlist_end` as `N:relative/path`. 61 of these
    /// paths exist on the real machine and break just as silently as a playlist
    /// (task 14).
    #[must_use]
    pub fn state_file_queue(self, queue: &[&str]) -> Self {
        write(self.fixture.state_file(), state_body(queue).as_bytes());
        self
    }

    /// A file whose name is not valid UTF-8, which the scanner must report and
    /// skip rather than guess at (safety invariant 8).
    ///
    /// Left out of [`Fixture::realistic`] deliberately: every name in the real
    /// library is valid UTF-8, so only the tests that are about this case should
    /// pay for it.
    ///
    /// # Panics
    ///
    /// If the file cannot be created.
    #[must_use]
    pub fn non_utf8_file(self, dir: &str, name_bytes: &[u8]) -> Self {
        use std::os::unix::ffi::OsStrExt as _;

        let parent = self.fixture.music_dir().join(dir);
        create_dir_all(&parent);
        let path = parent
            .as_std_path()
            .join(std::ffi::OsStr::from_bytes(name_bytes));
        std::fs::write(&path, AudioTemplate::Mp3v24.bytes())
            .unwrap_or_else(|err| panic!("cannot write {}: {err}", path.display()));
        self
    }

    /// Finish, and hand over the fixture.
    ///
    /// # Panics
    ///
    /// If a root the fixture promises is missing — a sign a builder method wrote
    /// somewhere it should not have.
    #[must_use]
    pub fn build(self) -> Fixture {
        for path in [
            self.fixture.music_dir(),
            self.fixture.playlist_dir(),
            self.fixture.data_dir(),
        ] {
            assert!(path.is_dir(), "{path} should be a directory");
        }
        assert!(
            self.fixture.state_file().is_file(),
            "{} should be a file",
            self.fixture.state_file()
        );
        self.fixture
    }

    /// Write an audio file and record it as a track.
    fn add_audio(&mut self, rel: &str, template: AudioTemplate) {
        self.add_file(rel, template.bytes(), FileRole::Audio);
    }

    /// Write one file under the music directory, recording what it is.
    ///
    /// Validating through [`RelPath`] here is what stops a fixture from
    /// containing a path the rest of MPDFM could not name — a trailing slash, a
    /// doubled separator — which would produce a baffling failure much later.
    fn add_file(&mut self, rel: &str, bytes: &[u8], role: FileRole) {
        let rel = self.fixture.rel(rel);
        let path = rel.to_abs(self.fixture.music_dir());
        write(&path, bytes);
        match role {
            FileRole::Audio => self.fixture.tracks.push(rel),
            FileRole::Aux => self.fixture.aux_files.push(rel),
        }
    }

    /// Write a playlist, following the symlink if there already is one.
    fn add_playlist(&mut self, name: &str, bytes: &[u8]) {
        assert!(
            !name.contains('/'),
            "a playlist name is a file name, not a path: {name:?}"
        );
        let path = self.fixture.playlist_path(name);
        let known = self.fixture.playlists.iter().any(|it| it == name);
        write(&path, bytes);
        if !known {
            self.fixture.playlists.push(name.to_owned());
        }
    }
}

/// What a file written under the music directory counts as.
enum FileRole {
    Audio,
    Aux,
}

/// The track number a `trackNNNN` component names, as a CUE sheet writes it:
/// `track0017` is `TRACK 17`. Anything that is not `track` plus digits is copied
/// through, since the fixture's job is to reproduce input, not to correct it.
fn cue_track_number(track_id: &str) -> String {
    track_id
        .strip_prefix("track")
        .and_then(|digits| digits.parse::<u32>().ok())
        .map_or_else(|| track_id.to_owned(), |number| format!("{number:02}"))
}

/// MPD's state file with `queue` as the saved queue.
///
/// The `key: value` head is trimmed from the real file, including the trailing
/// space after `lastloadedplaylist:` that MPD writes when nothing is loaded — a
/// rewrite must not "tidy" that away.
fn state_body(queue: &[&str]) -> String {
    let mut body = String::from(STATE_HEAD);
    body.push_str("playlist_begin\n");
    for (index, rel) in queue.iter().enumerate() {
        body.push_str(&format!("{index}:{rel}\n"));
    }
    body.push_str("playlist_end\n");
    body
}

/// The state file's non-queue part, trimmed from the real one.
const STATE_HEAD: &str = "\
sw_volume: 45
audio_device_state:1:PipeWire Sound Server
state: pause
current: 1
time: 12.345
random: 0
repeat: 0
single: 0
consume: 0
crossfade: 0
mixrampdb: 0
mixrampdelay: -1
lastloadedplaylist: 
";

/// The directories a test must never touch, so the guard can name them.
///
/// Built from the environment rather than hard-coded, and purely lexical: none
/// of these has to exist for the guard to refuse a path under it.
#[must_use]
pub fn real_library_roots() -> Vec<Utf8PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = env_path("HOME") {
        roots.push(home.join("Music"));
        roots.push(home.join(".config/mpd"));
        roots.push(home.join(".mpdconf"));
        roots.push(home.join(".local/share/mpdfm"));
        roots.push(home.join(".cache/mpdfm"));
    }
    if let Some(config) = env_path("XDG_CONFIG_HOME") {
        roots.push(config.join("mpd"));
        roots.push(config.join("mpdfm"));
    }
    if let Some(data) = env_path("XDG_DATA_HOME") {
        roots.push(data.join("mpdfm"));
    }
    if let Some(cache) = env_path("XDG_CACHE_HOME") {
        roots.push(cache.join("mpdfm"));
    }
    roots
}

/// Whether `path` is one of [`real_library_roots`] or inside one.
///
/// Component-wise, so `~/Musicbox` is not mistaken for `~/Music`.
fn in_real_library(path: &Utf8Path) -> bool {
    real_library_roots()
        .iter()
        .any(|root| path.starts_with(root))
}

fn env_path(key: &str) -> Option<Utf8PathBuf> {
    let value = std::env::var_os(key)?;
    Utf8PathBuf::from_path_buf(value.into()).ok()
}

fn create_dir_all(path: &Utf8Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|err| panic!("cannot create {path}: {err}"));
}

fn write(path: &Utf8Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        create_dir_all(parent);
    }
    std::fs::write(path, bytes).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

fn read(path: &Utf8Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

fn append_line(path: &Utf8Path, line: &str) {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|err| panic!("cannot append to {path}: {err}"));
    writeln!(file, "{line}").unwrap_or_else(|err| panic!("cannot append to {path}: {err}"));
}
