# 21 — Configurable vim-style keymap

- **Phase:** M3 · TUI
- **Depends on:** 20
- **Status:** not started

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

- [ ] every default binding above dispatches its `Action` (table-driven test)
- [ ] `gg` works as a sequence, and a lone `g` followed by a timeout is a no-op
- [ ] `keys.toml` overrides a default and adds a new binding
- [ ] an unknown action name warns and the rest of the file still applies
- [ ] rebinding a key does not leave the old binding active
- [ ] the help overlay content is derived from the keymap (test that a remap
      changes the rendered help)
- [ ] `:` command mode parses every command above, with errors shown inline
- [ ] `q` with pending ops asks for confirmation instead of discarding silently

## Files

`src/tui/keys.rs`, `src/tui/action.rs`, `docs/keys.example.toml`

## Pitfalls

- Terminals report `ctrl-shift-x` inconsistently; stick to widely supported
  combinations for defaults and let users discover the rest.
- Keep `Action` free of UI state so it can also be produced by command mode and
  by tests.
