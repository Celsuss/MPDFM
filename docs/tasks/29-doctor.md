# 29 — Library health checks (`mpdfm doctor`)

- **Phase:** M5 · Extras
- **Depends on:** 07, 15, 16
- **Status:** not started

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

- [ ] every check above is implemented and has a fixture that triggers it
- [ ] `doctor` on the real library reports the one known broken reference and no
      false positives (review the output by hand once and record the result here)
- [ ] `--json` output is stable and parseable
- [ ] `--check tags` runs only the tag checks
- [ ] a full run over ~2 800 files completes in a few seconds without `--deep`
- [ ] `--deep` duplicate detection finds a deliberately duplicated fixture file
- [ ] inconsistent-album detection catches one track with a different `album`
      spelling in an otherwise consistent directory
- [ ] output is capped by default and `--full` shows everything
- [ ] suggested commands printed are valid and copy-pasteable

## Files

`crates/core/src/doctor/{mod.rs,checks/*.rs}`, `src/cli/doctor.rs`

## Pitfalls

- Be very conservative about calling something a problem. A doctor that cries
  wolf gets ignored, and "referenced by no playlist" is normal, not broken.
- The `--deep` hash pass on 2 800 files is minutes of I/O; make the progress
  visible and the flag explicit.
