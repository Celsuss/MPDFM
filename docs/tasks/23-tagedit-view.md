# 23 — Tag editor view

- **Phase:** M3 · TUI
- **Depends on:** 16, 17, 18, 21, 22
- **Status:** done

## Goal

The form the user will spend most of their time in: edit one file's tags, or a
whole album's, with `<multiple>` handled honestly.

## Details

```
┌ Edit tags — 14 files selected ───────────────────────────────┐
│ Title        <multiple>        (per-file — use actions)      │
│ Artist       MF DOOM                                          │
│ Album artist MF DOOM                                          │
│ Album        Mm..Food                                         │
│ Year         2004                                             │
│ Track        <multiple>        (per-file)                      │
│ Disc         1/1                                              │
│ Genre      ▸ Hip Hop_                                         │
│ Comment      <multiple>                                       │
│                                                               │
│ Actions: [T] title from filename  [N] renumber  [C] clear     │
│ modified: genre                                               │
│ [w] stage  [W] stage & commit  [esc] cancel                   │
└───────────────────────────────────────────────────────────────┘
```

Requirements:

- Fields are a vertical list; `j`/`k` moves, `i`/enter edits, `esc` leaves the
  field. A simple single-line text input per field (history not needed).
- A field showing `<multiple>` is visually distinct and only becomes "modified"
  when the user actually types in it (task 18's rule). Modified fields are
  highlighted and listed at the bottom so it is obvious what will be written.
- `w` stages the edit into the pending plan (task 24); `W` stages and commits
  immediately. Never write on field exit.
- Per-file actions (`title from filename`, `renumber tracks`) show their own
  preview list before staging.
- Validation as you type: year must be a number or a date; track must be `n` or
  `n/total`. Show the error inline, refuse to stage until fixed.
- `esc` with unsaved modifications asks for confirmation.
- For a single file, also display read-only `AudioInfo` (duration, bitrate,
  sample rate, format) and the file's path.

## Acceptance criteria

- [x] editing one file's genre and staging produces one `WriteTags` op
- [x] a `<multiple>` field left alone produces no change (the critical test)
- [x] typing into a `<multiple>` field marks it modified and writes to all files
- [x] invalid year/track shows an inline error and blocks staging
- [x] `esc` with modifications prompts; `esc` without modifications exits directly
- [x] `W` commits and the change is visible in the browser immediately afterwards
- [x] `undo` from the browser reverses it
- [x] editing 200 selected files across two albums works and previews correctly
- [x] a non-writable file in the selection is reported before staging, naming it
- [x] UTF-8 input (accented characters, CJK) can be typed and is written correctly

## Files

`src/tui/views/tagedit.rs`, `src/tui/widgets/input.rs`

Also touched: `src/tui/app.rs` (the view, the key routing, staging, the commit and
undo workers' answers, the frame), `src/tui/action.rs` (six verbs), `src/tui/keys.rs`
(the `[tagedit]` defaults), `src/tui/msg.rs` and `src/tui/work.rs` (the selection
read, and the two workers that write), `src/tui/command.rs` (the `:` line now holds
an `Input` instead of its own copy of one), `src/tui/views/browser.rs`
(`Browser::marks` is no longer test-only — this is the task it was the seam for),
`crates/core/src/tags/bulk.rs` (`tags::merge`, moved out of `src/cli/tag.rs` so both
front-ends fold edit sets the same way), and `docs/keys.example.toml`.

## Pitfalls

- The empty-string vs cleared-field distinction must be visible in the UI, or the
  user cannot tell what `w` will do.
- Keep the form's state separate from `TagSet` so cancelling is trivially correct.

## How the `<multiple>` rule is actually enforced — again, and from the other end

Task 18 made `BulkView` read-only and gave `delta_for` the fields the user
changed, so that *nothing derives what to write from what is shown*. This task is
the front-end that has to decide what "the user changed it" means, and that turns
out to be one question with a non-obvious answer.

**The obvious answer is wrong.** "The text in the box differs from the value that
was displayed" cannot work, because a `<multiple>` field is displayed as the word
`<multiple>` and opens **empty** — prefilling it would mean the first keystroke
appended to a value no file has. So a user who opens such a field and changes
their mind leaves a box holding exactly what a user who emptied it on purpose
leaves. One of those must write nothing to fourteen files; the other must remove
the field from all fourteen.

So the editor does not compare. [`Input::touched`] is set by the *keystroke* —
`insert`, `backspace`, `clear` — and a field enters the form's `changed` map only
when it is set. `TagEdit::end` is the one place that records anything, it is the
only path from the open input to the map, and it also drops a field the user typed
back to what it already said, which keeps the `modified:` line honest.

`ctrl-u` on an empty field counts as a touch for the same reason: it is the user
saying "nothing", deliberately, and that is a request to clear the field rather
than the absence of a request.

[`Input::touched`]: ../../src/tui/widgets/input.rs

## Empty, cleared, and absent are three different things on screen

The task's first pitfall. The form shows:

| | |
| --- | --- |
| a value | what every selected file says |
| `—` | the field is absent from all of them |
| `<multiple>` | they disagree — in magenta italic, the one thing on screen that is MPDFM talking rather than the file |
| `<cleared>` | the user has asked for the field to be removed |

`<cleared>` is the word [`Edit::rendered`] prints in a plan preview, so the form
and the pending view cannot call the same thing by two names. It is reached by
emptying a field by hand *or* by `C`, and both produce [`Edit::Clear`] — never
`Edit::Set("")`, which would write a frame holding nothing. Core is explicit that
those are different writes, and a tag editor that conflated them would leave empty
frames behind every time somebody emptied a field.

[`Edit::rendered`]: ../../crates/core/src/tags/write.rs
[`Edit::Clear`]: ../../crates/core/src/tags/write.rs

## One mode, two states, and no second `keys.toml` section

The form has two states — moving between fields, and typing into one — and in the
first a bare letter is a verb while in the second it is a letter. Task 21 had
already written `[tagedit]` with no bare letter bound, on the assumption that
every one of them has to reach the field; this task needs `j`, `i`, `w`, `W`, `T`,
`N` and `C` as verbs as well.

A second mode (`[tagedit-insert]`) was the obvious shape and is the wrong one: it
would make a user configuring the editor work out which of two sections a key
lands in, and it would need a section that has to stay unbound for the letters to
stay letters. So there is one mode and one rule, in `App::on_field_key`:

> **Inside a field, a key that types a character types it. Every other key keeps
> its binding, filtered to the actions a field can use.**

Which is eight: `left`, `right`, `up`, `down`, `delete_char`, `clear_line`,
`submit`, `cancel`. `ctrl-u` still clears the line and still follows a remap;
`tab` and the arrows still move; `j` types a `j`. The keymap is not consulted for
a character at all, which also settles what a half-finished `gg` means in a text
field: nothing, because `g` is a letter there.

`up` and `down` inside a field accept it and move on, the way a form behaves —
which is why they are in that list even though they are not *about* text.

## `esc` means two things, and that is the point

- **inside a field** it leaves the field, keeping what was typed. It can, because
  leaving a field writes nothing anywhere — the task is explicit — so there is no
  asymmetry for `esc` and `enter` to express, and both do the same thing;
- **on the form** it leaves the editor, asking first when there is anything to
  lose. The question names the fields and the number of files.

That second one is why `Confirm::on_yes` stopped being an `Action` and became an
`Answer`. Most answers are an action, which is what makes `q` → "2 staged
operations would be lost" → `y` work with no second code path; "throw away a
form's unsaved fields" is not a verb in the vocabulary and should not become one,
because nothing outside this view could mean it.

## The per-file actions are previewed, not applied

`title from filename` is a guess about scene naming and `renumber tracks`
renumbers by the order the browser was showing. Both compute a *different* value
per file, so neither can be shown in a one-line field. `T` and `N` therefore
produce a `Preview` — a scrollable list of `file`, `what it says now`,
`what it would say` — which is a question the form will not go past until it is
answered, because a preview that could be navigated away from is a preview nobody
had to look at.

Accepting one folds it into the form as a set of per-file deltas, listed under
`modified:` with the action that produced it. Staging then merges the typed fields
with the accepted actions through [`tags::merge`], which is where that function
moved to: `mpdfm tag set --genre X --renumber-tracks` was already folding two edit
sets into one operation per file, and the rule for which of two edits to a shared
field wins should not exist twice. One file still ends up with **one** operation,
which matters because the unit of reversal is the file and the planner refuses two
edits of the same one.

A file the action would not change is not in the preview at all, which is
`delta_for`'s own rule showing through: `title from filename` over an album whose
titles already match its names previews as nothing to do rather than as fourteen
rewrites.

[`tags::merge`]: ../../crates/core/src/tags/bulk.rs

## Four refusals, and nothing is staged unless all four pass

In this order, in `App::stage_tags`:

1. **a value that will not parse.** `year` must be a year or a date and
   `track`/`disc` a number or `n/total`, checked *as it is typed* — the open
   field's live text is validated before it has been recorded, so the reason is
   beside the cursor that caused it. Deliberately not a calendar check: the real
   library holds `2004-00-00`, and the question is whether the shape is one
   `TagSet::year` can narrow;
2. **a per-file field typed across a selection.** `BulkView::per_file_in`, the
   same function `mpdfm tag set --title` is refused by. The form also refuses to
   *open* such a field for more than one file and names the action instead, which
   is better than letting somebody type a title and refusing afterwards — but the
   check at staging is the one that cannot be got round;
3. **a form that would change nothing**, which is information rather than a
   failure: "every selected file already says that";
4. **a plan that cannot be committed.** The prospective plan is validated before
   it replaces the real one, so a file MPDFM may not write is reported *by name*
   in a panel that has to be dismissed, with nothing staged. This is the
   acceptance criterion about a non-writable file, and it is answered by the same
   validation the CLI and the pending view use rather than by a check this view
   invented: a read-only file reaches `exec_fs::check` as `TagError::ReadOnly` and
   comes out as a `Conflict` naming the path.

## `W`, and the two workers that write

`w` stages and closes the form. `W` stages and then commits **the whole plan**,
which is the honest reading of "commit": a commit is one transaction over what is
staged, and committing a subset would make the pending view (task 24) lie about
what is left.

The commit runs on a worker, and not only for responsiveness: a tag write copies
the whole original file into the transaction's backup before touching it (task
17), so `W` on two hundred marked files is seconds of I/O. It is handed **clones**
of the plan, the library and the effects the user agreed to, which is safe because
commit re-validates and refuses on `Drift` if the answer has changed since.

`u` is here for the same reason — the next thing a user reaches for after a commit
is the key that takes it back, and the toast after a commit says so. Task 24 owns
the progress indicator, the cancellation and the version of `u` that offers itself
from the pending view; what is here is the path this task's acceptance criteria
need, on the worker that task can build on.

MPD is told which directories changed, on the worker's own connection. Not
`cli::mpd::Link`: that one reports a refused connection on stderr, which here is
the alternate screen, and a warning printed over a drawn frame is a corrupted
frame. The TUI's link logs instead, and deliberately reads no queue — the preview
was made with `Live::default()`, and a queue read here would make the two disagree,
which is drift and a refused commit rather than extra information.

## The selection is read on a worker too

Opening the editor pushes the form **before** the tags exist, with "reading 310
file(s)…" on it, and a worker answers with `TaskOutcome::Selection`. The bulk view
cannot say `<multiple>` honestly until it has every file's tags, so this is the one
read whose cost is the user's selection rather than the screen.

A file that will not read refuses the whole form, naming the files — the rule
`mpdfm tag set` follows, and for the same reason: a bulk view of nine of ten files
answers a question nobody asked.

What the editor opens on is the marks, or the row under the cursor when nothing is
marked, with a marked **directory** standing for the audio files in it, one level
down (`mpdfm tag set` without `--recursive`). Anything else that can be marked — a
`.cue`, a `folder.jpg`, an `.nfo` — is counted and left out rather than refusing
the lot, because a user who marked a whole listing with `a` meant the tracks in it;
the count is reported so the number in the title is never a surprise.

## How it is tested

**The form, with no terminal and no files** (`src/tui/views/tagedit.rs`, 28
tests). The view is built from a `Vec<(RelPath, TagSet)>`, which is what
`BulkView` takes, so the rules can be tested as rules:
`a_multiple_field_left_alone_produces_no_change`,
`opening_a_multiple_field_and_changing_your_mind_writes_nothing`,
`emptying_a_field_by_hand_is_a_clear_and_says_so_on_screen`,
`typing_a_field_back_to_what_it_said_is_not_a_modification`,
`a_per_file_field_will_not_open_across_a_selection_but_will_for_one_file`. Plus
`every_row_is_exactly_as_wide_as_the_pane` over six widths with a `ノスタルジア`
in a field, which is the border-corrupting bug this project keeps finding.

**The line editor** (`src/tui/widgets/input.rs`, 9 tests), including
`the_window_is_exactly_as_wide_as_asked_for_whatever_is_in_it` at every cursor
position of every width, and
`a_wide_character_cut_by_the_left_edge_becomes_a_space`.

**The whole path, against real audio** (`src/tui/app.rs`, 19 tests). A `Fixture`,
a `TestBackend` and real worker threads: `e` opens the editor, the tags are read
off disk, the form is drawn, and `w`/`W`/`u` do what they say.
`capital_w_commits_and_the_browser_shows_the_change_afterwards` asserts the new
genre is on the **screen** after the commit, which is the criterion as written;
`undo_from_the_browser_reverses_a_committed_tag_edit` reads the file back;
`a_file_that_cannot_be_written_is_named_before_anything_is_staged` chmods one file
to `0444`; `two_hundred_files_across_two_albums_open_and_preview_correctly`
builds the 200-file selection and asserts 400 edits across exactly two fields.

**Through a pseudo-terminal** (`tests/tui_terminal.rs`). One test for the thing
in-process tests cannot see: `ï` typed at a real terminal arrives as two bytes and
has to become one character in the field. It also needed the pty harness to learn
`esc`, which is now `\033` through `printf '%b'`.

## Verifying it by hand

Run on 2026-10-07 against the release binary.

**Read-only, on the author's real library** (3 132 files, 318 directories).
`e` on one track — `chinese/Kimberly_Chen/Kimberley Chen 陳芳語 …` — read it in
**64 µs** and drew the whole form, path and `320 kbps · 44100 Hz · 2 ch`
included. Marking all of `electronic/` with `a` and pressing `e` opened the editor
on **310 files across 23 albums**, with `7 marked paths hold no audio and were
left out`: `reading 310 file(s)…` was on screen first, the read took **300 ms
cold and 13 ms warm** on the worker, and all ten fields came back `<multiple>`
with `modified: nothing — no file would be written`. This pass found a real bug:
the note beside a per-file field was measured without the live keymap and drawn
with it, so `per-file — T titles from filenames` rendered as `per-file…` at 120
columns. Fixed by giving `TagEdit::columns` the same `Hints` the row is drawn
with, with
`the_note_beside_a_per_file_field_names_the_key_rather_than_being_cut_off` to hold
it.

**Writing, on a copy of a real album.** `~/Music` is only ever read
(`docs/PLAN.md` §8), so the write half ran against a copy of
`lofi/Various Artists - Lofi Game Night by Lola (2023) Mp3 320kbps [PMEDIA] ⭐️` —
72 files, 70 of them audio, with the `TXXX:ARTISTS` and `TLEN` frames and the
`PMEDIA` comment that scene releases carry. Driven through a pty: `l l a e`, seven
`j`s to the genre, `i`, `ctrl-u`, `Lo-Fi`, `esc`, `W`.

| | |
| --- | --- |
| staged | 70 tag edits, 2 non-audio files reported and left out |
| committed | `20261007T203452Z-001e82`, 70 steps, **210 ms** |
| on disk | all 70 say `genre Lo-Fi`; `year`, `comment`, and the per-file `artist` and `composer` are untouched; the 2 non-audio files are byte-identical |
| afterwards | the commit's rescan reloaded the library and the browser re-read the window, so the new genre was on screen |
| `u` in the browser | reversed it in **100 ms**, and all **72 files are byte-for-byte identical to the originals** |

The terminal's modes were unchanged across every run (`stty -g` before and after).
