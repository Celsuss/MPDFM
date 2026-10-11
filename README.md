# MPDFM — MPD File Manager

A terminal application for managing an [MPD](https://www.musicpd.org/) music
library: edit tags, and re-organize directories **without breaking your
playlists**.

```text
MPDFM  ~/Music
┌ music ────────────────┐┌ hiphop/MF DOOM - Mm..Food (2004) [V0] scene-ta┐┌ details ───────────────┐
│⚠ ▾ /                  ││  ⚠ 01 Beef Rap.mp3                     ·     ·││01 Beef Rap.mp3         │
│    ▸ coding-music     ││    02 Hoe Cakes.mp3                    ·     ·││                        │
│    ▸ electronic       ││    03 Potholderz (feat. Count Bas…     ·     ·││reading…                │
│⚠   ▾ hiphop           ││    eac.log                                    ││                        │
│⚠       MF DOOM - Mm..…││    folder.jpg                                 ││Kind    mp3             │
│⚠       Snoop Dogg & W…││    info.nfo                                   ││Size    1 kB            │
│    ▸ jazz             ││    Mm..Food.m3u                               ││                        │
│⚠   ▸ pop              ││    mm..food.sfv                               ││⚠ 2 lines in 2 playlists│
│                       ││                                               ││Hip hop                 │
│                       ││                                               ││MF Doom                 │
│                       ││                                               ││                        │
└───────────────────────┘└ 1/8 ──────────────────────────────────────────┘└────────────────────────┘
hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag · 0 marked · 0 pending · focus files         ○ mpd ?
scanned 24 files in 14 directories in 7 ms
```

<sup>`mpdfm` in a terminal, browsing a test library: the tree, the album's files,
and the details pane, whose `⚠` marks say which entries a playlist refers to and
what is wrong with them. Drawn by the same code that draws it on your screen.</sup>

**Documentation:** <https://celsuss.github.io/MPDFM/> — guides, the TUI, the safety model and a command reference.

## Why it exists — the safety model in three sentences

MPD identifies every track by its path relative to `music_directory`, and that
path is copied into every `.m3u` in your playlist directory *and* into the saved
queue in MPD's `state` file, so a plain `mv` silently breaks both. MPDFM makes
moving a file and rewriting every reference to it **one** transaction: previewed
first, backed up and journaled before the first change, and reversible byte for
byte. If it is killed halfway, `mpdfm recover` finishes or rolls back what it
started — and [`docs/SAFETY.md`](docs/SAFETY.md) says how to do the same by hand.

## Start here — nothing in these two writes a byte

```console
$ mpdfm doctor                                           # what is wrong, today
$ mpdfm move --dry-run "hiphop/MF DOOM" "hiphop/Doom"    # what a move would do
```

`doctor` reports broken playlist references, missing tags, inconsistent albums and
the like, and says plainly when the library is fine. `--dry-run` prints the same
preview that appears before every commit: the moves, each playlist line that
changes, and anything that would stop it. Run those on your real library first.
Then try your first real command on a copy.

## Install

MPDFM needs a Rust toolchain (1.93 or later) and nothing else — tag handling is
pure Rust, and MPD is optional.

```console
$ git clone https://github.com/Celsuss/MPDFM && cd MPDFM
$ cargo install --path .          # puts `mpdfm` in ~/.cargo/bin
```

Then, once:

```console
$ mpdfm config show               # where it found your library, and why
```

MPDFM reads `music_directory`, `playlist_directory` and the state file from your
`mpd.conf`, so on a normal MPD setup there is nothing to configure. If `config
show` says otherwise, [`docs/config.example.toml`](docs/config.example.toml)
documents every setting.

### Man page and shell completions

Both are generated from the argument definitions, so they always match the
binary:

```console
$ mpdfm man | man -l -                                    # read it now
$ mpdfm man --dir ~/.local/share/man/man1                 # install it: mpdfm.1 + one per subcommand
```

**zsh** (put the file anywhere on `$fpath` *before* `compinit`):

```console
$ mkdir -p ~/.zfunc
$ mpdfm completions zsh > ~/.zfunc/_mpdfm
```

```zsh
# ~/.zshrc, before compinit
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

Open a new shell, and `mpdfm <TAB>` completes subcommands and `mpdfm move --<TAB>`
its flags. If it does not, `rm -f ~/.zcompdump*` and start another shell.

**bash**: `mpdfm completions bash > ~/.local/share/bash-completion/completions/mpdfm`<br>
**fish**: `mpdfm completions fish > ~/.config/fish/completions/mpdfm.fish`

## Quick start

Paths are **relative to your music directory**, which is what MPD's own paths are.

```console
$ mpdfm scan                                   # counts by format, album dirs, warnings
$ mpdfm doctor --full                          # every finding, not the first ten

$ mpdfm move --dry-run "rock/Old Name" "rock/New Name"
$ mpdfm move "rock/Old Name" "rock/New Name"   # shows the preview, asks, commits
$ mpdfm undo                                   # changed your mind: byte-for-byte back
```

Editing tags:

```console
$ mpdfm tag show -r "hiphop/Mac + Devin Go To High School"
$ mpdfm tag diff --genre "Hip Hop" --renumber-tracks -r "hiphop/Mac + Devin Go To High School"
$ mpdfm tag set  --genre "Hip Hop" --renumber-tracks -r "hiphop/Mac + Devin Go To High School"
```

Nothing else in a file changes when its tags do: embedded artwork, ReplayGain,
MusicBrainz ids, lyrics and every unknown frame survive, and the audio stream is
bit-identical.

Re-organizing a whole library to a template, a little at a time:

```console
$ mpdfm organize --dry-run --limit 20          # a cautious first look
$ mpdfm organize --limit 20                    # and the first twenty tracks' worth
$ mpdfm organize --only-missing                # then the rest, whose tags are complete
```

The default template is `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`;
change it with `-t` or `organize_template`. A track whose tags the template cannot
fill is left where it is and listed, never filed under "Unknown".

Cover art:

```console
$ mpdfm cover show "rock/Album"                # embedded and loose art, and what disagrees
$ mpdfm cover set --dry-run "rock/Album"       # embed the album's cover.jpg in every track
$ mpdfm cover extract "rock/Album"             # or write the embedded one out as cover.jpg
```

Every command has `--help` with an example, and a `man mpdfm-<command>` page.
Every command takes `--json`; every one that edits tags, moves or re-files takes
`--dry-run`; and every one that changes the library asks before it commits
(`--yes` skips the question, and is required when stdin is not a terminal). Exit codes: `0` success, `1` error, `2` refused before
anything was written, `3` you declined.

## The TUI

`mpdfm` with no subcommand opens it. It is vim-style: `j`/`k` and `h`/`l` walk the
tree and the files, `space` marks, `e` edits tags, `m` stages a move, `o` stages
an organize, `p` shows what is pending, `c` commits, `u` undoes, `/` searches,
`:` is a command line, and `?` shows the keymap you actually have.

Nothing a key does touches the library until you commit, and the pending view
draws the very preview `mpdfm move --dry-run` prints. The terminal is always given
back, including after a panic or a `kill`.

Rebind anything in `~/.config/mpdfm/keys.toml`; [`docs/keys.example.toml`](docs/keys.example.toml)
lists every action. Cover art is drawn in kitty, iTerm2/WezTerm and sixel
terminals, and as a plain placeholder everywhere else (never inside tmux).

## What it will and will not do

* **MPD is optional.** With it running, MPDFM asks it to rescan after a commit and
  warns about moved files that are in the live queue. Without it, MPDFM rewrites
  the saved queue in the state file itself.
* **It never overwrites.** A move that would land on an existing file is a
  conflict, reported before anything happens.
* **It does not guess.** A path that is not valid UTF-8 is reported and skipped.
  A playlist line is rewritten only on an exact path match; URLs, comments,
  `#EXTINF` lines and anything it does not understand are written back byte for
  byte.
* **It has a delete switch.** `delete_enabled = false` makes every operation that
  would remove a file refuse.

## Where it keeps things

| | |
| --- | --- |
| `~/.config/mpdfm/config.toml` | settings (all optional) |
| `~/.config/mpdfm/keys.toml` | key bindings (all optional) |
| `~/.local/share/mpdfm/journal/` | one record per transaction |
| `~/.local/share/mpdfm/backups/` | playlists, the state file, and deleted or retagged originals |

`XDG_CONFIG_HOME` and `XDG_DATA_HOME` are respected.

## Design

| | |
| --- | --- |
| Language | Rust |
| TUI | ratatui + crossterm |
| Tags | lofty (no C dependencies) |
| MPD | optional, non-fatal; a minimal protocol client for `update` and status |
| Structure | `mpdfm-core` library + CLI + TUI over one API |

* [`docs/PLAN.md`](docs/PLAN.md) — architecture, the safety invariants, the edge cases
* [`docs/SAFETY.md`](docs/SAFETY.md) — what is backed up and where, and **how to recover by hand**
* [`docs/ROADMAP.md`](docs/ROADMAP.md) — what is built, task by task
* [`CHANGELOG.md`](CHANGELOG.md)

## Development

```console
$ just            # fmt-check + clippy + test: everything CI runs
$ just docs       # regenerate the man pages and completions under target/docs
```

## License

See [LICENSE](LICENSE).
