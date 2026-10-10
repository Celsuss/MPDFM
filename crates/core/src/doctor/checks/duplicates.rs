//! The same song, or the same bytes, in more than one place.
//!
//! Both checks are notes. Two copies of a song may be a mistake or may be the
//! album version and the compilation version, and which one to keep is the
//! user's call — so neither check offers a fix.
//!
//! # `--deep`, and why it is opt-in
//!
//! `identical-files` reads every byte of every audio file whose size some other
//! audio file shares. Size first because it is free (the scan has it) and
//! because two files of different sizes cannot be the same bytes, so on a real
//! library most files are never opened. What is left is hashed, and a hash
//! match is then **confirmed byte for byte**: FNV-1a is a checksum and not a
//! proof, and "these are identical" is a claim the user may delete a file over.
//!
//! Comparing whole files means two copies of one recording with different tags
//! are not found. That is the conservative side to err on; `same-song` is the
//! check for those.

use std::collections::BTreeMap;
use std::io::Read as _;

use camino::Utf8Path;

use super::super::{Check, Info, Item, Progress};
use super::tags::{Table, song_key};
use crate::library::{Entry, Library};
use crate::ops::exec_fs::hash_file;

/// Run one check of this group.
pub(crate) fn run(
    info: Info,
    library: &Library,
    tags: Option<&Table>,
    deep: bool,
    progress: &mut dyn FnMut(Progress),
) -> Check {
    match info.name {
        "same-song" => {
            let table = tags.expect("tags are read when same-song runs");
            Check::ran(info, same_song(library, table))
        }
        "identical-files" if deep => Check::ran(info, identical_files(library, progress)),
        "identical-files" => Check::skipped(
            info,
            "pass --deep to compare file contents; it reads every same-sized audio file in full",
        ),
        _ => super::unknown(info),
    }
}

/// Tracks with the same artist and title, in more than one file.
fn same_song(library: &Library, table: &Table) -> Vec<Item> {
    let mut songs: BTreeMap<(String, String), Vec<&Entry>> = BTreeMap::new();
    for (entry, tags, _) in table.tagged(library) {
        if let Some(key) = song_key(tags) {
            songs.entry(key).or_default().push(entry);
        }
    }
    let mut items: Vec<Item> = songs
        .into_values()
        .filter(|entries| entries.len() > 1)
        .map(|entries| {
            let (first, rest) = entries.split_first().expect("at least two");
            let others: Vec<&str> = rest.iter().map(|entry| entry.rel.as_str()).collect();
            Item::new(first.rel.as_str(), format!("also {}", others.join(", ")))
        })
        .collect();
    items.sort_by(|left, right| left.what.cmp(&right.what));
    items
}

/// Audio files that are byte-for-byte the same.
fn identical_files(library: &Library, progress: &mut dyn FnMut(Progress)) -> Vec<Item> {
    let mut by_size: BTreeMap<u64, Vec<&Entry>> = BTreeMap::new();
    for entry in library.entries().iter().filter(|entry| entry.is_audio()) {
        by_size.entry(entry.size).or_default().push(entry);
    }
    let candidates: Vec<&Entry> = by_size
        .into_values()
        .filter(|same| same.len() > 1)
        .flatten()
        .collect();

    let total = candidates.len();
    let mut by_hash: BTreeMap<(u64, u64), Vec<&Entry>> = BTreeMap::new();
    for (done, entry) in candidates.into_iter().enumerate() {
        progress(Progress::Hashing { done, total });
        // A file that cannot be read now is not claimed to be anything.
        if let Ok(hash) = hash_file(&entry.rel.to_abs(library.root())) {
            by_hash.entry((entry.size, hash)).or_default().push(entry);
        }
    }
    progress(Progress::Hashing { done: total, total });

    let mut items = Vec::new();
    for same in by_hash.into_values().filter(|same| same.len() > 1) {
        // Confirmed, not assumed: split the hash group into sets of truly equal
        // files, comparing each to the first member of every set so far.
        let mut sets: Vec<Vec<&Entry>> = Vec::new();
        for entry in same {
            let abs = entry.rel.to_abs(library.root());
            match sets
                .iter_mut()
                .find(|set| same_bytes(&set[0].rel.to_abs(library.root()), &abs).unwrap_or(false))
            {
                Some(set) => set.push(entry),
                None => sets.push(vec![entry]),
            }
        }
        for set in sets.into_iter().filter(|set| set.len() > 1) {
            let others: Vec<&str> = set[1..].iter().map(|entry| entry.rel.as_str()).collect();
            items.push(Item::new(
                set[0].rel.as_str(),
                format!("identical bytes to {}", others.join(", ")),
            ));
        }
    }
    items.sort_by(|left, right| left.what.cmp(&right.what));
    items
}

/// Whether two files hold exactly the same bytes.
///
/// Only ever called on two files of the same size, so reading `left` in chunks
/// and filling the same length from `right` covers both.
fn same_bytes(left: &Utf8Path, right: &Utf8Path) -> std::io::Result<bool> {
    const CHUNK: usize = 64 * 1024;
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut a = vec![0_u8; CHUNK];
    let mut b = vec![0_u8; CHUNK];
    loop {
        let read = left.read(&mut a)?;
        if read == 0 {
            return Ok(right.read(&mut b[..1])? == 0);
        }
        if right.read_exact(&mut b[..read]).is_err() || a[..read] != b[..read] {
            return Ok(false);
        }
    }
}
