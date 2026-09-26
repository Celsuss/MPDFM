//! The library model and the scan that builds it.
//!
//! [`Library::scan`] walks `music_directory` once and produces an in-memory model
//! of every file in it: what each one is, how big it is, when it changed, which
//! directory it is in, and which directories are albums. Everything downstream —
//! the browser, the move planner, `organize`, `doctor` — reads that model instead
//! of the disk.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::library::{DirPath, Library};
//!
//! let library = Library::scan(Utf8Path::new("/home/me/Music"))?;
//! println!("{} files, {} albums", library.len(), library.album_dirs().len());
//!
//! // What the browser draws at the top level, with no second walk.
//! for dir in library.subdirs_in(&DirPath::root()) {
//!     println!("{dir}/");
//! }
//! for warning in library.warnings() {
//!     eprintln!("mpdfm: warning: {warning}");
//! }
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! Two things the scan deliberately does not do, both of them load-bearing:
//!
//! - **No tag reading.** ~2 800 tag reads at startup would cost seconds for data
//!   most runs never look at, so tags are read lazily and by index (task 16).
//!   [`audio_reads`] is how the test suite holds that line.
//! - **No symlink following.** See `scan.rs`.
//!
//! `model.rs` holds the structure, `scan.rs` the walk; both are private, and
//! everything they define worth naming is re-exported here.

mod model;
mod scan;

pub use model::{
    AlbumDir, Counts, Dir, DirPath, Entry, Format, Kind, Library, ScanWarning, disc_number,
};

use std::sync::atomic::{AtomicU64, Ordering};

/// Every read of an audio file's *contents* that core has performed.
static AUDIO_READS: AtomicU64 = AtomicU64::new(0);

/// How many times core has opened an audio file to read its contents.
///
/// A scan must not move this number: it `stat`s, and nothing more. Task 16's
/// lazy tag reader is what will make it move, and it has one obligation —
/// [`record_audio_read`] on every file it opens — which is what keeps
/// `no_tag_io_during_scan` in `tests/library.rs` a real assertion rather than a
/// tautology once tag reading exists.
///
/// Process-wide and monotonic, so a test compares a delta across the code it is
/// watching rather than an absolute value.
#[must_use]
pub fn audio_reads() -> u64 {
    AUDIO_READS.load(Ordering::Relaxed)
}

/// Record that an audio file's contents were read. Called by every code path in
/// core that opens one; see [`audio_reads`].
pub fn record_audio_read() {
    AUDIO_READS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audio_read_counter_counts() {
        // Deltas, not absolutes: the counter is process-wide and tests run in
        // parallel.
        let before = audio_reads();
        record_audio_read();
        assert!(audio_reads() > before);
    }
}
