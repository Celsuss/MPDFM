# Editing tags

Press `e` on a file, a directory, or a set of marks. The editor opens on a form with
one line per field and edits all selected files at once.

```text
┌ Edit tags — 14 files selected ───────────────────────────────┐
│ Title        <multiple>        (per-file — use actions)      │
│ Artist       MF DOOM                                         │
│ Album artist MF DOOM                                         │
│ Album        Mm..Food                                        │
│ Year         2004                                            │
│ Track        <multiple>        (per-file)                    │
│ Disc         1/1                                             │
│ Genre      ▸ Hip Hop_                                        │
│ Comment      <multiple>                                      │
│                                                              │
│ Actions: [T] title from filename  [N] renumber  [C] clear    │
│ modified: genre                                              │
│ [w] stage  [W] stage & commit  [esc] cancel                  │
└──────────────────────────────────────────────────────────────┘
```

(Layout illustrative; it adapts to the terminal.)

## `<multiple>`

When the selected files disagree about a field, it shows `<multiple>`. **Leaving it
alone changes nothing.** It only becomes *modified* when you type into it, and then
that value is written to every selected file. Modified fields are highlighted and
listed at the bottom, so it is always clear what will be written.

For a single file the editor also shows the path and read-only audio info
(duration, bitrate, sample rate, format).

## Keys

Between fields, a bare letter is a command:

| Key | Does |
| --- | --- |
| `j` / `k`, `tab` / `shift-tab` | next / previous field |
| `i` | type into the field |
| `C` | clear the field (removes the tag, which is different from an empty value) |
| `T` | set each title from its file name |
| `N` | renumber the tracks 1..n in listed order |
| `w` | **stage** the edit into the pending plan |
| `W` | stage and **commit** straight away |
| `esc` | leave (asks first if there are unsaved modifications) |

Inside a field you are typing into, letters are letters — `j` types a `j`.
`enter` accepts the text, `esc` leaves the field, `ctrl-u` clears it, and the arrow
keys move the cursor. Accented and CJK text types and writes correctly.

## Validation

Year must be a number or a date, and track must be `n` or `n/total`. An invalid
field shows its error inline and **staging is refused** until it is fixed. If any
selected file is not writable, the editor names it before you stage.

Nothing is written when you leave a field — only when you stage and commit.
A committed edit can be undone with `u`; the originals are backed up first, as with
[`mpdfm tag set`](../guide/tags.md).
