# 21 — Configurable vim-style keymap

- **Phase:** M3 · TUI
- **Depends on:** 20
- **Status:** done

## Goal

Decision D8: vim-style keys that match the rmpc muscle memory the user already
has, remappable from a config file, with a command mode.

## Details

Default bindings:

```
h j k l / ← ↓ ↑ →   navigate            g g / G      top / bottom
enter               enter dir / open    -  or  backspace   parent dir
tab                 switch pane         ctrl-d / ctrl-u    half page
space               mark / unmark       v            visual-select range
a                   mark all in view    A            unmark all
e                   edit tags           m            stage a move of the marks
r                   rename              d            stage a delete
o                   organize by template (M4)
p                   show pending ops    c            commit pending
x                   discard pending     u            undo last transaction
/                   search              n / N        next / prev match
f                   filter              esc          clear filter / close overlay
:                   command mode        ?            help overlay
R                   rescan library      q            quit (warn if ops pending)
```

Command mode (`:`) mirrors the CLI so knowledge transfers both ways:
`:move <dst>`, `:organize <template>`, `:undo [txid]`, `:doctor`, `:set <k>=<v>`,
`:q`, `:q!`.

Implementation:

- `Action` enum, decoupled from keys.
- `KeyMap: HashMap<(Mode, KeySeq), Action>` where `Mode` is
  `Browser | TagEdit | Pending | Search | Command`, supporting two-key sequences
  (`gg`) with a short timeout.
- Loaded from `~/.config/mpdfm/keys.toml`, merged over the defaults:

  ```toml
  [browser]
  "ctrl-r" = "rescan"
  "J" = "half_page_down"
  ```

- Unknown action name in the config → warning at startup listing valid actions,
  not a hard failure.
- The help overlay (task 26) is generated from the live keymap, so it can never
  document a binding the user has remapped away.

## Acceptance criteria

- [x] every default binding above dispatches its `Action` (table-driven test) —
      `every_default_binding_dispatches_its_action` in `src/tui/keys.rs`, which
      presses each one through normalization and the sequence state machine rather
      than looking it up, and then asserts that the table it drives is the **whole**
      of the browser's keymap, so a default cannot be added without being covered
- [x] `gg` works as a sequence, and a lone `g` followed by a timeout is a no-op —
      `gg_works_as_a_sequence_and_a_lone_g_times_out_into_nothing`, which checks the
      deadline against the *tick* and against the *next keypress*, because both
      enforce it and the second one happens first; plus
      `a_prefix_followed_by_something_else_falls_back_to_that_key_alone`
- [x] `keys.toml` overrides a default and adds a new binding —
      `keys_toml_overrides_a_default_and_adds_a_new_binding` (the task's own
      example), and `a_keys_toml_beside_the_config_file_is_found_and_applied` in
      `tests/tui_terminal.rs` for the same thing through the real binary
- [x] an unknown action name warns and the rest of the file still applies —
      `an_unknown_action_in_keys_toml_is_a_panel_at_startup_and_not_a_refusal_to_start`
      and `an_unknown_action_warns_and_the_rest_of_the_file_still_applies`, plus
      `every_other_kind_of_bad_line_warns_and_is_skipped` for the five other ways a
      line can be wrong
- [x] rebinding a key does not leave the old binding active —
      `rebinding_a_key_does_not_leave_the_old_binding_active`, and
      `a_remapped_key_works_and_the_key_it_replaced_does_not` end to end through
      `App`
- [x] the help overlay content is derived from the keymap (test that a remap
      changes the rendered help) — `the_rendered_help_follows_a_remap` asserts on the
      drawn frame, not on the model; `the_help_is_generated_from_the_table_and_a_remap_changes_it`
      on the model; and `the_help_overlay_documents_the_keymap_the_run_is_actually_using`
      through a pty
- [x] `:` command mode parses every command above, with errors shown inline —
      `every_command_in_the_task_parses` and
      `a_command_that_cannot_be_run_says_what_is_wrong_with_it` in
      `src/tui/command.rs`; `command_mode_types_a_command_and_runs_it` and
      `a_command_that_does_not_parse_leaves_the_line_open_with_the_reason_under_it`
      for the line itself. Seen on the real library, below
- [x] `q` with pending ops asks for confirmation instead of discarding silently —
      `q_with_staged_operations_asks_before_discarding_them`, plus
      `q_with_nothing_staged_does_not_ask`,
      `ctrl_c_asks_once_and_then_leaves_whatever_the_answer_would_have_been` and
      `a_confirmation_cannot_be_rebound_out_of_existence`

## Files

`src/tui/keys.rs`, `src/tui/action.rs`, `docs/keys.example.toml`, as planned, plus
one the plan did not name:

- **`src/tui/command.rs`** — the `:` line. It was going to be part of `action.rs`,
  and it is two things that are not the action vocabulary: a grammar (`:move <dst>`
  → `Command`) and a line editor with a cursor in it. Keeping it separate is also
  what lets `action.rs` stay free of UI state, which is the task's second pitfall.

Changed: `src/tui/app.rs` (the `match` on `KeyCode` is gone; `App::dispatch`
replaces it), `src/tui/mod.rs` (loading the keymap, step 2 of start-up).

One addition to core: **`config::keys_file_path`**, next to `config_file_path`, so
that where the file lives is stated once. The keymap itself is read by the
front-end and not by `config::resolve`: a key binding is `crossterm`'s vocabulary,
which `crates/core/tests/manifest_deps.rs` keeps out of core.

Tests: unit tests in all four modules, and three pty checks in
`tests/tui_terminal.rs` for the part only a real run has — that the file is found
at all.

## Pitfalls

- Terminals report `ctrl-shift-x` inconsistently; stick to widely supported
  combinations for defaults and let users discover the rest.
- Keep `Action` free of UI state so it can also be produced by command mode and
  by tests.

## How it is designed

Four decisions worth writing down, because each of them is a thing that could
reasonably have gone the other way.

**`shift` is folded into the character's case, not kept as a modifier.** A capital
`J` arrives as `Char('J')` from one terminal and as `Char('j')` with `SHIFT` from
another; `KeyChord::new` normalizes both to `Char('J')` with no modifier. Three
consequences: `"J"` and `"shift-j"` are one binding; `?`, `:` and `/` match on
every terminal, because uppercasing them changes nothing and the stray `SHIFT`
disappears; and the task's pitfall is *answered* rather than avoided — both
spellings of `ctrl-shift-d` become the single chord `ctrl-D`. The defaults still
use no `ctrl-shift` combination, because normalization cannot make a terminal send
one at all.

**A complete binding beats a prefix.** If a `keys.toml` binds `g` on its own, `g`
fires at once and `gg` becomes unreachable. The alternative — waiting out the
deadline on every `g` — would make a bound key feel broken, and it contradicts the
criterion that a lone `g` does nothing. So the rule is "act now", and the
unreachable sequence is a `KeyWarning::Unreachable` at start-up:
`a_binding_that_swallows_a_sequence_is_reported_rather_than_left_to_surprise`.

**Two keys are not in the keymap.** `ctrl-c`, and the `y`/`n` of a confirmation
prompt. Both are escape hatches, and an escape hatch that can be remapped away is
not one — `a_confirmation_cannot_be_rebound_out_of_existence`. `ctrl-c` means what
`q` means, so it asks about a staged plan; a second one, from the prompt it raised,
leaves regardless. Two presses always get out, and neither of them loses a plan
quietly.

**The start-up warning is a panel, not a toast.** This is the one place the task
departs from task 20's "warnings are transient". The warning for an unknown action
has to list the valid names — that is what the user needs at that moment — and
three dozen names cannot be read on a one-line message queue that moves on after
four seconds. So `keys.toml` warnings open a dismissible `View::Notice`, all of
them at once, wrapped.

**`:set` parses and does nothing.** Deliberately. The grammar is settled and
tested, and there is nowhere to put the value: no task on the roadmap owns live
settings, and inventing one here would be a promise the plan has not made. It
reports that rather than pretending. Every other command names the task that
implements it, which is also what an unimplemented *key* does — a binding that
silently did nothing would be indistinguishable from a binding that is broken.

## How it is tested

The split follows task 20's: what needs a real terminal, and what does not.

**Headless, in `src/tui/`.** Parsing, normalization, the sequence timeout (with
`Instant` passed in, so a deadline is stated rather than waited for), the merge and
every way a file can be wrong, the generated help, the command grammar, the line
editor, and `App`'s dispatch of all of it against a `TestBackend`. Two tests are
about the *whole* vocabulary rather than one case:
`every_action_in_the_vocabulary_is_answered_by_something` dispatches all 38, and
`the_shipped_example_file_applies_cleanly_once_uncommented` parses
`docs/keys.example.toml` twice — as shipped, where it must change nothing, and with
every section and binding uncommented, where it must still be valid. Documentation
that is wrong about its own syntax is a test failure.

**Through a pseudo-terminal, in `tests/tui_terminal.rs`.** Three checks, all of
them about the one thing in-process tests cannot see: that the file is found where
a user would put it, and that what it says reaches the keymap the loop is using.

Two harness subtleties found while writing those, both recorded in the tests:

- **the EOF character is a keypress.** stdin there is a file; when `script` reaches
  the end of it the pty delivers `VEOF`, which in raw mode is a literal `ctrl-d` —
  half a page down. It scrolled the help overlay out from under an assertion about
  what the help says. The test now unbinds `ctrl-d`, which is one more thing it
  proves works;
- **`\r` has to arrive after raw mode is on.** Fed from a file, the whole script is
  in the pty's input queue before the TUI starts, so `ICRNL` turns the `enter` into
  a `\n` — which crossterm, in raw mode, does not treat as `Enter`. Driving
  `script`'s stdin from a pipe with `sleep`s between the keystrokes is what makes a
  real `enter` reach a real command line.

## Verifying it by hand

Run on 2026-10-06 against the release binary and the author's real library, with a
`keys.toml` in a temporary `XDG_CONFIG_HOME`:

```toml
[browser]
"ctrl-r" = "rescan"
"J" = "half_page_down"
"K" = "half_page_up"
"x" = "none"
"D" = "discard_pending"
"g e" = "edit_tags"
```

**The help overlay, read off that live keymap**, showed `R / ctrl-r` for rescan,
`e / ge` for the tag editor, `D` for discard — and no `x` at all. Every one of the
six lines above is visible in the rendering, which is the whole criterion in one
screen.

**Command mode on the real library.** `:organize` with no argument left the line
open with `organize needs a template` on its own row above it, the command still
there to be fixed. Typing it moved nothing: `o`, `g`, `a`, `n` and `e` are all
browser verbs and none of them fired, because the mode decides.

**Still free.** `just verify-tui`: **0 clock ticks over 30 s** (0.00% of one core),
4 threads, **7.7 MB** resident — against 7.2–7.4 MB at task 20. The keymap's 78
entries are a few kilobytes of that at most; the rest is the code this task added,
and either way it is noise at this scale. `SIGTERM` exit 0, terminal
modes unchanged. The startup scan of `~/Music` is **3 132 files in 318 directories
in 11–13 ms**, unchanged.

**What could not be verified by hand:** the confirmation on `q`. Nothing stages an
operation yet — task 22 is the first thing that can — so the plan is always empty
in a real run and the prompt is unreachable outside the tests that put an operation
in it directly. It is covered four ways there, and task 24 is where it will be seen
for real.
