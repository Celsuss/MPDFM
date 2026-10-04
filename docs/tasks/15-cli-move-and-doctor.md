# 15 — Wire up the M1 CLI end to end

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 04, 05, 07, 10, 11, 12, 13, 14
- **Status:** done

## Goal

The milestone deliverable: a CLI that can inspect the library and move things
safely, with `--dry-run`, confirmation, and undo. This is what proves the engine
before any UI exists.

## Details

```
mpdfm scan [--json]
    counts by format, album dirs, warnings (non-UTF-8, unreadable, symlinks)

mpdfm doctor [--json]
    broken playlist references (0 in this library — see "What the hand
      verification found"; task 07 had already established that)
    playlist entries pointing outside music_dir
    audio files referenced by nothing (informational)
    empty album dirs, dirs with no audio
    (the fuller checks land in task 29)

mpdfm move <SRC> <DST> [--dry-run] [--yes] [--merge] [--verify]
    SRC/DST relative to music_dir, or absolute inside it
    prints the Effects preview (task 10 renderer); asks to confirm unless --yes
    refuses on conflicts and exits 2

mpdfm undo [TXID] | undo --list [--force] [--yes]
mpdfm recover [TXID] [--forward] [--force] [--yes]
```

Exit codes: `0` success, `1` unexpected error, `2` conflicts blocked the
operation, `3` user declined. Honour `--json` for machine-readable `Effects` and
results. Respect `NO_COLOR` and non-TTY output (no colour, no prompts — require
`--yes`).

Write the integration tests at this level too: a test that drives the actual
binary against a fixture library via `assert_cmd`, because that is the same path
the user takes.

**Three flags the sketch above did not spell out, and what they turned into.**
`undo --force` had to exist because task 12's own report prints "Re-run with
--force" when a file has changed since the transaction. `recover --forward` had
to exist because task 12 gives the command two engines — `roll_back` and
`roll_forward` — and a command cannot pick for the user; rolling back is the
default, because finishing a transaction whose preview the user never saw is the
more surprising of the two. And both commands ask before they write, like
`move`, for the same reason: they are the code paths that can lose data.

**Exit code 2 covers one case the sketch did not name.** A commit can also be
refused by step 1 of the two-phase commit — re-validation against a fresh scan
finding that the library moved since the preview ([`CommitError::Stale`]). That
is a refusal with nothing written, which is what a script wants to tell apart
from a failure, so it shares the conflict code rather than becoming a generic
error. `src/output.rs` is where that is written down.

**`--merge` needed one change in core.** `Plan::validate` hard-coded
`Merge::Refuse` for a destination no other operation was vacating, so there was
no way for a flag to reach `exec_fs::expand_dir_move`. The seam added is
`ops::Prefs`, passed to `Plan::validate_with` *and* to `commit::Options::prefs` —
both, because commit re-validates, and re-validating a `--merge` plan without the
flag expands into a different number of steps and refuses as drift. That is the
same arrangement `Live` already has for MPD, for the same reason.

## Acceptance criteria

- [x] `mpdfm scan` on the real library reports 2 440 mp3 / 364 flac / 5 m4a and
      the aux counts (numbers may drift; assert against a fixture, eyeball the
      real one — re-measured 2026-10-04, flac was 357 when this task was
      written). **Exact, eyeballed 2026-10-04**; the table is below.
      `tests/cli_scan.rs` asserts the shape against a fixture.
- [x] `mpdfm doctor` reports exactly the one known broken reference — **amended:
      it reports 0, which is correct.** See "What the hand verification found";
      task 07 had already established this and this task confirmed it through
      the binary. `tests/cli_doctor.rs` asserts the "exactly one" behaviour
      against the fixture, which *does* have a deliberately broken reference.
- [x] `mpdfm move --dry-run` writes nothing (snapshot before/after) —
      `cli_move.rs::a_dry_run_writes_nothing`, which also checks the journal
      stayed empty, because a dry run is not a transaction.
- [x] `mpdfm move` then `mpdfm undo` returns a fixture to a byte-identical state
      — `cli_undo.rs::move_then_undo_is_byte_identical`, including MPD's state
      file, which is restored wholesale from the backup rather than re-derived.
- [x] conflicts exit 2 and print what conflicted —
      `cli_move.rs::a_conflict_exits_2_and_says_what_conflicted`, plus a test
      that `--yes` does not override a refusal.
- [x] declining the prompt exits 3 and changes nothing —
      `cli_move.rs::declining_the_prompt_exits_3_and_changes_nothing`, and
      `only_yes_means_yes` for the seven answers that are not yes.
- [x] non-TTY without `--yes` refuses rather than hanging on a prompt —
      `cli_move.rs::a_non_interactive_run_without_yes_refuses_rather_than_prompting`,
      and the `--json` equivalent. The preview is still printed first, so a
      script's log says what MPDFM declined to do.
- [x] `--json` output parses and contains the same counts as the text output —
      asserted for all three commands, and for `scan` by reading the numbers
      back *out of the table* rather than trusting that both came from one value.
- [x] end-to-end test on a *copy* of the user's real playlists: move an album,
      verify all 231 references still resolve, undo, verify byte-identical —
      **done by hand on a real copy** (tier 3; the transcript is below), and
      automated at the fixture tier over the seventeen committed playlist
      reproductions in
      `cli_move.rs::every_reference_that_resolved_before_the_move_still_resolves_and_undo_restores_the_bytes`.
- [x] a guard proves no test touched `~/Music` or `~/.config/mpd` —
      `tests/harness/mod.rs::assert_hermetic`, which runs
      `mpdfm config show --json` and refuses any resolved path that is inside
      `testing::real_library_roots()` or outside the fixture. It asks the binary
      where it is looking rather than asserting that it looked in the right
      place. Belt and braces: every run also has its environment **cleared**,
      with `HOME` and the three `XDG_*` variables pointed into the fixture, so
      even a command that ignored the config file could not resolve to a real
      root.

## Verifying it by hand

Agreed with the user before this task started, because the milestone is the
first one with a binary a person can point at a real library.

**Tests only ever run against a `Fixture`.** The fixture set reproduces the real
library's shapes, so no automated test needs the real one, and the guard
(`testing::real_library_roots`, which refuses `~/Music`, `~/.config/mpd`,
`~/.local/share/mpdfm`) enforces it. Note what that guard does *not* cover: it
lives behind the `testing` feature, so it is not in the loop when the `mpdfm`
binary is run by hand. The binary does what its configuration tells it to.

So, by tier:

1. **Fixture** — every test, and all `move`/`undo`/`recover` exercising.
2. **Read-only against the real library** — `scan`, `doctor`, and
   `move --dry-run` write nothing, and running them against `~/Music` is
   *authorized*. It is the only way to find the name nothing in the fixture set
   anticipated, and to see whether a 27 GB / 2 809-file scan is fast enough to
   sit behind a TUI keystroke.
3. **A real copy, for anything that writes.** Copy one genre directory, not the
   whole 27 GB. **Not `cp -al`**: a hardlink tree looks cheap and is exactly
   wrong here, because a rename is fine through a hardlink but a tag write
   (tasks 17–18) would go straight into the user's real bytes.

Nothing outside a fixture or an explicit copy gets written without asking first.

## What the hand verification found

Run on 2026-10-04 with the release binary, in the three tiers above.

### Tier 2 — read-only against `~/Music`

`scan` agrees with the table below **exactly** on the three audio formats and
the directory count:

```
audio          2809  2440 mp3, 364 flac, 5 m4a
images          238
cue sheets        4
playlists        24
sidecars         54
other             3
files          3132
directories     318
album dirs      236  12 of them discs of 5 multi-disc set(s)
```

Three numbers drifted from the table since it was measured, all upwards and all
explicable as the library having been added to: images 236 → 238, album-internal
`.m3u` 23 → 24, `.parts` 2 → 3. `sidecars` is 54, which is the table's
21 + 23 + 4 + 2 + 2 + 2 exactly.

**The scan takes 10 ms** for 3 132 files across 27 GB (warm cache; `-v` prints
it). That settles the question this task raised — whether a scan can sit behind
a TUI keystroke — with three orders of magnitude to spare, so task 20 needs no
scan cache and `~/.cache/mpdfm/scan.json` (`PLAN.md` §7, "only if scanning
proves slow") should stay unbuilt.

`doctor` on the real library: **0 broken references**, 0 unrewritable entries, 0
unreadable playlists, 8 empty directories, 5 leaf directories with files but no
audio, 2 598 unreferenced tracks out of 2 809. Every one of those was reviewed
by hand and every one is right:

- the 8 empty directories are real leftovers (`podcasts`, `nostalgia`,
  `unsorted/loving_caliber`, …) and `rmdir` is the right suggestion;
- the 5 no-audio directories are `Scans`, `Covers` and `My Uploads`
  subdirectories of albums — which is why that check is a **note** and not a
  problem;
- 2 598 of 2 809 tracks being in no playlist is simply true: 17 playlists hold
  231 references between them.

**No false positives.** That is the half of the `doctor` criterion that
mattered.

### The "one broken reference" is not broken

`PLAN.md` §3 recorded "1 broken reference out of 231" and this task inherited it
as an expectation. Task 07 had already looked and concluded otherwise; checking
it again from the command line confirms task 07:

```
Pop.m3u:7  pop/…/CD 1 - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue/track0017
```

The `.cue` sheet exists. It is a **multi-`FILE` sheet** — one `FILE` line per
track, 18 of them, not a single-image rip — it does contain `TRACK 17 AUDIO`,
and the file that track names (`17 - Wrecked (Live From the Bunker).flac`) is on
disk. So the reference resolves, MPD can play it, and all 231 references are
good.

What made it *look* broken is that there is no `Imagine Dragons - Mercury -
Acts 1.flac` — the single-file image the path `…Acts 1.flac.cue` suggests. There
does not need to be: MPD reads the sheet and follows its `FILE` lines. Task 07
considered and rejected exactly this heuristic ("treating `X.flac.cue` as broken
unless `X.flac` exists"), and this is the library that proves it was right to.

The deeper check — *does the sheet contain the track the playlist names?* — is
task 29's, and `PlaylistIndex::cue_refs()` exists to hand it over. Recorded here
so that task 29 does not have to re-measure: on this library, that check also
finds nothing, because `TRACK 17` is there.

### Tier 3 — a real copy, which is where the writes happened

`~/Music/japanese` (12 MB, the smallest genre directory with real playlist
references) copied with `cp -r` — **not** `cp -al`, for the reason the task
gives — together with all 17 real playlists (`cp -rL`, so the copy is
self-contained and the dotfiles repository behind `Radios.m3u` is never a write
target) and the real `state` file. Every copied file was checked to have a link
count of 1, so nothing in the copy shared an inode with the user's library.

Then, against the copy:

| | |
|---|---|
| before | 231 references, 228 of them unresolvable (only one genre was copied) |
| `mpdfm move japanese/STUTSxSIKK-O "japanese/STUTS x SIKK-O" --yes` | 4 files, 3 lines of `Summer situation.m3u` rewritten |
| after | 231 references, the **same** 228, listed identically |
| `mpdfm undo` | all 22 files byte-identical to before the move |
| and | every one of the 17 playlists still `cmp`-identical to the user's original |

The three lines that were rewritten are the interesting ones, because they are
the real library's names rather than a fixture's:

```
japanese/STUTS x SIKK-O/STUTS × SIKK-O × 鈴木真海子 - Summer Situation (…) [HIZzYz1xk18].mp3
japanese/STUTS x SIKK-O/STUTS × SIKK-O × 鈴木真海子 - 愛をさわれたら (…) [W93ZloXye44].mp3
japanese/STUTS x SIKK-O/0℃の日曜 [8YqiaNYwoMY].mp3
```

The invariant asserted is the one `PLAN.md` §8 states — *every entry that
resolved before still resolves* — rather than "nothing is broken", because 228
of these never resolved in the copy. A move must change that set in neither
direction: not break a good reference, and not quietly appear to fix a bad one.

`~/Music` was not modified, and `~/.local/share/mpdfm` **was never created** —
checked after every tier-2 run, which is the observable proof that `scan`,
`doctor` and `move --dry-run` write nothing at all, not even a journal
directory.

### One thing worth knowing about the prompt

Confirmation is gated on stdin being a terminal, and a test harness has no
terminal to give it — so the "declined" path, which is the one that must exit 3
and change nothing, would have been the only untested branch in the command.
`MPDFM_ASSUME_TTY` (documented in `src/output.rs`) is the seam that fixes that.
It is safe if anyone sets it by accident: it only makes MPDFM *ask*, and the
read then hits end-of-file, which is not `y`, so the answer is no.

## What the real library actually holds

Measured read-only on 2026-10-04, so `scan`'s output can be eyeballed against
something rather than just *looked* plausible:

| | | task 05 calls it |
|---|---|---|
| audio | 2 440 mp3, 364 flac, 5 m4a | `Audio` |
| directories | 318, and **no symlinks anywhere under `~/Music`** | — |
| playlists | 17, all `.m3u` (no `.m3u8`); `Radios.m3u` is a symlink into `~/workspace/dotfiles` | — |
| playlist entries | 235, of which 4 are stream URLs → **231 file references** | — |
| cover art | 184 `.jpg`, 34 `.jpeg`, 18 `.png` | `Image` |
| album-internal | 23 `.m3u` **inside** the library | `Playlist` |
| scene clutter | 21 `.nfo`, 23 `.txt`, 4 `.sfv`, 2 `.log`, 2 `.pdf` (booklets), 2 `.sfk` (editor peak files) | `Sidecar` |
| CUE | 4 `.cue` sheets | `Cue` |
| in progress | 2 `.parts` — partial downloads | `Other` |

Every one of those lands in the right `Kind` already: `Kind::from_extension`
knows `jpeg` as well as `jpg`, and the `sfk`/`pdf` spellings; a `.m3u` inside the
library is `Kind::Playlist` and never mistaken for one of MPD's own. So this
table is a **check that was passed**, not a list of gaps — but it is the number
`scan` has to agree with, and the fixture set's choice of shapes was validated by
measuring rather than by assuming.

The two `.parts` are the one genuinely new thing. `Kind::Other` is the right
answer — scanned, counted, moved with its album, never interpreted — and
`doctor` should not have an opinion about them either. Worth a thought while
writing `move`: relocating a file something is still downloading into is a real
way to confuse the downloader, and MPDFM has no way to know. Not a reason to
refuse; possibly a reason for `doctor` to mention a `.parts` that is newer than
everything around it.

## Files

As built:

`src/output.rs` — exit codes, the output mode, the prompt.
`src/cli/{scan.rs,doctor.rs,move.rs,undo.rs}` — one module per command;
`undo.rs` holds `recover` too, because both act on a journal record.
`src/cli/mpd.rs` — the daemon, shared by the three commands that write: the
queue on the way in and `update` on the way out, which core takes as values
rather than owning a connection (`PLAN.md` D6).
`tests/{harness/mod.rs,cli_scan.rs,cli_doctor.rs,cli_move.rs,cli_undo.rs}` — 50
tests, all driving the built binary.

In core, `ops::Prefs` and `Plan::validate_with` (see the `--merge` note above).

## Pitfalls

- Confirmation prompts must show the *full* preview, not a summary line. The
  whole design rests on the user seeing which playlist lines change.
- Print the txid on every successful commit so `undo <txid>` is copy-pasteable.
- **The width.** `Effects::render` takes a column count and MPDFM links no
  terminal library yet, so the CLI reads `COLUMNS` and falls back to 80.
  `crossterm` arrives with the TUI in task 20 and `terminal::size()` is the
  better answer then; pulling the crate in early for one `ioctl` was not worth
  it. The failure mode of guessing low is a shortened path, never a mangled
  layout, because the renderer never exceeds the width it is given.
- **Padding a painted string does not align it.** An escape sequence has zero
  width on screen and full width in a format string. `src/cli/doctor.rs` pads
  *before* painting; a column that lines up only when `NO_COLOR` is set is the
  bug this caught.
