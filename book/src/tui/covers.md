# Cover art

Press `C` in the browser to open the cover view on a directory: the folder you are
looking at, or the subfolder under the cursor in the listing. `:cover` does the same.

```text
┌ covers · coding-music/Singles ──────────────────────────┬───────────────────┐
│ images                                                   │                   │
│ ▸ cover.jpg      image/jpeg 1200×1200, 312.0 kB  in 1 track                  │
│   ghost.jpg      image/jpeg 600×600, 84.2 kB             │     (picture)     │
│   newtech.jpg    image/jpeg 600×600, 91.0 kB             │                   │
│ tracks                                                   │                   │
│     Artist_-_Ghost_[…].mp3     = cover.jpg   → ghost.jpg │                   │
│     Artist_-_New_Tech_[…].mp3  no cover      → newtech.jpg                   │
└ s assign · n pair by name · w stage · ? more · esc back ┴───────────────────┘
```

(Layout illustrative; the picture is drawn in terminals that support it, as in the
[details pane](browsing.md#the-panes).)

The **images** are every image file in the folder, `cover.*` first, then
`folder.*` and `front.*`, then the rest by name. Each shows what its bytes are
(a `.jpg` that is really a PNG says so) and how many tracks already embed it.

The **tracks** are the audio files directly in the folder, each with its embedded
front cover now:

- `= cover.jpg` means it embeds that image, byte for byte;
- `own 600×600` means it embeds a picture that is not one of the files here;
- `no cover` and `unreadable` mean what they say.

To the right of that, in yellow, is the track's **draft**: what its cover will
become once you stage it.

## Drafts, then stage

Nothing is written, and nothing is staged, until you press `w`. Until then you are
editing drafts, as you do in the [tag editor](tags.md).

| Key | Does |
| --- | --- |
| `s`, `enter` | assign the image under the cursor to the marked tracks, or to **every track** in the folder when none is marked |
| `x` | draft removing the embedded front cover (marks, or every track) |
| `n` | pair each track with the image of the same name (see below) |
| `h` / `l` on a track | step that track's draft through keep, each image, and remove: the fix for anything `n` got wrong or could not match |
| `space`, `a`, `A` | mark / mark all / unmark all tracks |
| `tab` | jump between the images and the tracks |
| `z` | downscale images larger than 1000 px when embedding them (a JPEG, quality 90) |
| `w` | stage every draft |
| `esc` | back to the browser; asks first if drafts would be lost |

`w` opens the [pending view](pending.md) with one tag write per track, in track
order, and a note of how much the library grows by. A cover is embedded once
**per track**, so a 5 MB scan into 14 tracks is 70 MB. Tracks that already carry
exactly that cover are left out. Everything else in the file (other pictures,
every tag) is kept, and `u` undoes the commit byte for byte. After a commit the
view reads the folder again, so you can see the result.

## A folder of singles: `n`

When a folder holds several singles, each beside its own image, `n` matches them
up by name. Case, punctuation and a leading track number are ignored, and the
track's title tag counts as well as its file name. So all of these pair up:

| Track | Image |
| --- | --- |
| `01 - Song A.mp3` | `song_a.JPG` |
| `Artist - Song A.flac` | `Song A.png` |
| `Artist_-_Song_Title_(AMV)_[dQw4w9WgXcQ].mp3` | `songtitle.jpg` |

A track no image matches, or that two images match equally well, keeps its draft,
and the message names it. Put the cursor on it and press `l` until it shows the
right image. `n` only drafts, so check the list, fix what needs fixing, then `w`.

With tracks marked, `n` pairs only those.

## Tidying the image files

These stage straight away, like the browser's `d` and `r`:

| Key | Does |
| --- | --- |
| `e` | write the embedded cover out as `cover.<ext>`: the cover of the track under the cursor, or the first track's that has one |
| `C` | rename the image under the cursor to `cover.<ext>`, the name MPD's `albumart` and most clients look for first |
| `d` | delete the image under the cursor |
| `D` | delete every image in the folder except the one under the cursor |

`C` will not overwrite an existing `cover.jpg`. Press `D` on the image you want
first, then `C`. An image a staged cover edit is going to embed cannot be renamed
or deleted until that edit is committed or unstaged, because the edit reads it at
commit time.

## From the command line

`:cover set <image>`, `:cover remove` and `:cover extract [name]` stage the same
operations without opening the view, for the folder you are in: the tracks marked
there, or every track. `<image>` is a file name in that folder, a path in the
library, or an absolute path. They mirror [`mpdfm cover`](../guide/cover.md).

## In the browser

On a folder, the details pane shows its best image, and says `· 3 images` when
there is more than one: that is a folder worth opening with `C`.
