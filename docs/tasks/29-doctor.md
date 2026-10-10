# 29 — Library health checks (`mpdfm doctor`)

- **Phase:** M5 · Extras
- **Depends on:** 07, 15, 16
- **Status:** done

## Goal

Tell the user what is wrong with the library, so tagging and organizing have a
work queue instead of a vague feeling. Builds on machinery that already exists,
which is why it is cheap.

## Details

Checks, each independently selectable (`--check <name>`, `--json`):

**Reference integrity**
- broken playlist references (1 exists today: the Imagine Dragons CUE track)
- playlist entries pointing outside `music_directory`
- broken entries in MPD's saved queue
- duplicate entries within one playlist
- audio files referenced by no playlist (informational, not a problem)

**Tag health**
- files with no tags at all
- missing `title`, `artist`, `album`, or `genre`
- album directories with inconsistent `album`/`albumartist`/`year` across tracks
- missing or duplicate track numbers within an album
- ID3v1-only files (no v2 tag)
- year values that aren't plausible dates

**Filesystem hygiene**
- non-UTF-8 filenames
- directories with no audio files
- audio files sitting directly in `music_directory` root (unfiled)
- suspected NFC/NFD duplicate directory names
- case-insensitive-colliding names
- orphan cover images and `.nfo`/`.sfv` in directories with no audio

**Duplicates**
- same `artist` + `title` in multiple places (informational)
- identical file size *and* audio hash (true duplicates) — the hash pass is
  opt-in via `--deep` because it reads every byte

Output: grouped by check, counts first, then paths, capped with `--full` to show
all. `--fix` is deliberately **not** implemented in this task; where a fix is
obvious, print the `mpdfm` command that would do it.

## Acceptance criteria

- [x] every check above is implemented and has a fixture that triggers it —
      `crates/core/tests/doctor.rs`: one test per check, and
      `every_catalogued_check_is_implemented_and_fires_on_a_library_that_earns_it`,
      which builds one library with everything wrong with it and fails if any
      catalogued check finds nothing. Every heuristic also has a **near miss**
      that must not fire (a directory of singles, a partial album, two discs in
      one directory, an album's `Scans`, a release wrapper's `My Uploads`).
- [x] `doctor` on the real library reports the one known broken reference and no
      false positives — **amended, as task 15 already did: it reports 0, which is
      correct.** The CUE track is not broken, and now that the sheet is opened
      and the track inside it checked, that is established rather than assumed.
      Every finding was reviewed by hand; see "What the hand verification found".
- [x] `--json` output is stable and parseable —
      `cli_doctor.rs::the_json_report_is_stable_and_has_a_fixed_shape` compares two
      runs byte for byte and checks every key; on the real library two runs `cmp`
      equal too.
- [x] `--check tags` runs only the tag checks —
      `check_tags_runs_only_the_tag_checks` (CLI) and
      `selecting_tags_runs_only_the_tag_checks` (core).
- [x] a full run over ~2 800 files completes in a few seconds without `--deep` —
      **0.19 s** on the real library (3 132 files, 2 809 tag reads), release
      build, warm cache.
- [x] `--deep` duplicate detection finds a deliberately duplicated fixture file —
      `deep_finds_a_deliberately_duplicated_file` (core, with a same-size file
      that differs in its last byte and must not match) and
      `deep_finds_a_duplicated_file_and_says_it_is_reading` (CLI).
- [x] inconsistent-album detection catches one track with a different `album`
      spelling in an otherwise consistent directory —
      `one_track_with_a_different_album_spelling_is_caught`, including the fix.
- [x] output is capped by default and `--full` shows everything —
      `the_text_output_is_capped_and_full_lifts_the_cap`.
- [x] suggested commands printed are valid and copy-pasteable —
      `every_suggested_fix_runs_as_printed` runs every kind of fix through `sh`
      exactly as printed, against a fixture whose names have a space, `'`, `''`
      and `$HOME` in them, and then checks each one fixed what it was offered for.

## What was built

The checks moved into core (`mpdfm_core::doctor`), where a TUI view can reach
them later; `src/cli/doctor.rs` loads the inputs, runs, and renders. 24 checks
in four groups, selected by `--check <name|group>` (repeatable, or
comma-separated):

| group | checks |
|---|---|
| `references` | `broken-references`, `unrewritable-entries`, `unreadable-playlists`, `broken-queue-entries`, `duplicate-entries`, `unreferenced-audio` |
| `tags` | `unreadable-tags`, `untagged`, `missing-tags`, `inconsistent-albums`, `track-numbers`, `id3v1-only`, `implausible-years` |
| `filesystem` | `bad-names`, `unreadable-paths`, `normalization-twins`, `case-collisions`, `unfiled-audio`, `empty-dirs`, `orphan-aux`, `no-audio-dirs`, `partial-downloads` |
| `duplicates` | `same-song`, `identical-files` |

**Three severities, not two.** Task 15 had problem and note. Tag health needs a
middle: 620 tracks missing a genre is a work queue, not 620 problems, and
calling it that would bury the one broken reference that matters. So
*problem* means broken (a reference that does not resolve, a name MPDFM cannot
handle), *warning* means worth fixing while everything still works, and *note*
means normal. Severity is fixed per check, in one catalogue
(`doctor/checks/mod.rs`). `empty-dirs` moved from problem to warning on the same
reasoning: an empty directory breaks nothing.

**Text output** is counts first — every selected check under its group, so a
clean one is visibly clean and a skipped one says `(skipped)` and why — then
the findings, ten per check unless `--full`. **`--json`** is always complete,
and now carries `warnings`, `notes`, and per check `group` and `skipped`.

**Fixes** are offered only where obvious: `rmdir` for an empty directory (now
an absolute path — a relative one only worked from inside the music
directory), `tag set DIR --album/--album-artist/--year MAJORITY` for an album
outlier, `tag set FILE --title-from-filename` for a missing title, and
`organize FILE` for an unfiled track. Every path is single-quoted
(`doctor::shell_quote`); task 15's `{:?}` quoting would have turned a combining
diaeresis into `\u{308}`, which no shell reads.

**How the heuristics stay quiet.** `inconsistent-albums` and `track-numbers`
only speak about a directory that is an album by its own tags: at least three
tracks, three quarters agreeing on `album`. A gap is reported only below the
highest number present (a partial album is normal here) and only when every
track has a number. A file with no readable tag is reported once, by `untagged`
or `id3v1-only`, and not again as an album track without a number.

**CUE references are now checked inside the sheet.** A virtual track resolves
when the sheet has that track — by position or by `TRACK` number, either one —
and the `FILE` it plays is in the library. Only when neither reading finds it
is it reported; a `FILE` name MPDFM cannot place is not claimed either way.

**`orphan-aux` vs `no-audio-dirs`.** Task 15's `no-audio-dirs` covered both a
stray `cover.jpg` and an album's `Scans`. They are now split: an audioless
directory inside a release (an album directory, a multi-disc set root, or a
*wrapper* — no audio of its own, exactly one subdirectory with audio, the way a
torrent arrives) is a `no-audio-dirs` note; anywhere else its files are
`orphan-aux`. The wrapper rule errs towards silence: an artist directory with
one album and one cover-only directory reads as a wrapper too.

**`--deep`** groups audio files by size (free, from the scan), hashes only the
groups with more than one member (FNV-1a, `exec_fs::hash_file`), and confirms
each hash match **byte for byte** before calling two files identical. Progress
goes to stderr: a live counter on a terminal, one line otherwise.

**A small seam in core:** `tags::read_tags_with_layout` returns which tag blocks
a file has. A `TagSet` reads only the primary tag, so an ID3v1-only mp3 and an
untagged one looked the same.

## What the hand verification found

Run on 2026-10-10 against the real `~/Music` with the release binary — `doctor`
and `doctor --deep`, both read-only. `~/.local/share/mpdfm` still does not
exist afterwards.

| check | count | verdict |
|---|---|---|
| `broken-references` | 0 | right: the Imagine Dragons CUE track resolves (see task 15) |
| `broken-queue-entries` | 0 | the saved queue was loaded and every entry resolves |
| `duplicate-entries` | 1 | right: `Romance.m3u` lines 1 and 2 are the same Passenger track |
| `unreferenced-audio` | 2 598 | same as task 15 |
| `unreadable-tags` | 1 | right: the ADTS AAC stream named `.mp3` that task 16 found |
| `untagged` | 28 | right: 12 Kapten Bolja FLACs, 7 Alok and 9 Wiz Khalifa mp3s — checked byte-level, no `ID3` at either end |
| `missing-tags` | 620 | true; mostly YouTube rips with no genre. (600 of them lack a genre; with the 28 untagged and 1 ID3v1-only files, which this check leaves to those checks, that is exactly the 629 task 25's `missing:genre` finds) |
| `inconsistent-albums` | 4 | all real: two S3RL tracks tagged album "Transformers", a Greenbacks 12'' track, a Mr. Robot track with a different album artist |
| `track-numbers` | 6 | all real: duplicated numbers in two (a Raddox album's two track 1s, the Mr. Robot unofficial soundtrack), one unnumbered Raddox track, and three albums mostly without numbers. The Wiz Khalifa album's gaps are *not* reported: its untagged files explain them |
| `id3v1-only` | 1 | right: `14.Cameras.mp3` ends in a `TAG` block and has nothing else |
| `implausible-years` | 0 | |
| filesystem, names | 0 | no bad names, twins or case collisions |
| `empty-dirs` | 8 | the same 8 task 15 found |
| `orphan-aux` | 2 | right: two `cover.jpg`-only directories in `unsorted/Autumn_Orange`, whose audio never arrived |
| `no-audio-dirs` | 3 | `Covers`, `Scans`, and a torrent's `My Uploads` — all part of a release |
| `partial-downloads` | 2 | the two `.parts` task 15 measured |
| `same-song` | 71 | true as stated; mostly an MF DOOM track on both a 12'' and an album. One is two different songs that share an artist and the title "Intro" (ODESZA) — a note, and literally what it says |
| `identical-files` (`--deep`) | 0 | 6 audio files share a size with another; none are the same bytes |

**Two false positives were found by this review and fixed before it was
recorded**, which is what the review is for: `orphan-aux` first reported 180
directories, because "audio below this directory" excluded the directory's own
files; and the Fakear release's `My Uploads` read as an orphan until the wrapper
shape was recognised. `track-numbers` also first reported gaps that were really
the untagged files of the same album, and listed 58 unnumbered file names where
a count reads better.

## Found along the way

**`mpdfm tag set --year 20004` stores `2000`.** The year check in
`tags/write.rs` (`check_edits` → `timestamp`) accepts a five-digit year and
`lofty` keeps the first four digits, so the edit is silently changed. Found
because the `implausible-years` fixture could not be built that way; the
fixture writes a free-text FLAC `DATE` instead. Not fixed here — it is task 17's
code, and worth its own change.

**The TUI's `:doctor`** still says "not yet" and points at this task. The core
API is ready for a view (`doctor::run` takes a `Library` the TUI already
holds); the view itself was not in this task's files.

## Files

`crates/core/src/doctor/{mod.rs,checks/*.rs}`, `src/cli/doctor.rs`

Also touched: `crates/core/src/tags/{read.rs,mod.rs}` (`read_tags_with_layout`,
`TagLayout`), `src/cli/mod.rs` (`DoctorArgs`), `crates/core/tests/doctor.rs`,
`tests/cli_doctor.rs`.

## Pitfalls

- Be very conservative about calling something a problem. A doctor that cries
  wolf gets ignored, and "referenced by no playlist" is normal, not broken.
- The `--deep` hash pass on 2 800 files is minutes of I/O; make the progress
  visible and the flag explicit.
