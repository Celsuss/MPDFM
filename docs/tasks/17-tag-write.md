# 17 — Tag writing

- **Phase:** M2 · Tag editing
- **Depends on:** 11, 16
- **Status:** done

## Goal

Write tags without losing anything that was already in the file, atomically, and
inside the same journal/undo machinery as file moves.

## Details

```rust
struct TagDelta { set: Vec<(Field, Option<String>)> }   // None = clear the field

fn write(abs: &Utf8Path, delta: &TagDelta, opts: &WriteOpts) -> Result<TagBackup>;
fn restore(backup: &TagBackup) -> Result<()>;
```

Requirements:

- **Only touch the fields in the delta.** Unmentioned frames, embedded artwork,
  ReplayGain tags, MusicBrainz ids, lyrics, and anything in `extra` survive
  untouched. This is the single most important property; a tag editor that
  silently drops embedded art or ReplayGain is worse than no editor.
- **Atomic**: write to a temp file in the same directory, `fsync`, `rename`.
  `lofty` writes in place, so copy → modify the copy → rename over the original.
  Preserve mode, owner where possible, and set mtime deliberately (see pitfalls).
- **ID3 version policy**: write ID3v2.4 by default; if the file already has
  v2.3, keep v2.3 unless configured otherwise, because some players and MPD
  builds handle v2.4 `TDRC` poorly. Make it a config option
  (`id3_version = "keep" | "v23" | "v24"`), defaulting to `keep`.
- **Backup for undo**: before writing, record the complete original tag chunk
  (simplest robust approach: copy the whole original file into the transaction
  backup dir when it is small, or store the serialized original tag blob plus a
  hash). Choose based on size; document the choice. Undo must restore the file
  byte-for-byte.

  **Decided: the whole original file, always.** A serialized tag blob cannot
  promise the file comes back byte for byte, because the padding and the frame
  order around it are not in the blob — and `docs/tasks/19-cli-tags.md` asks for
  exactly that promise, for every file in a bulk edit. The copy costs one file's
  worth of I/O, which a tag write spends anyway making the temp file it publishes
  through, and retention prunes it with the rest of the transaction's backups.
  It also makes a rollback **idempotent**, which is what lets `recover` put a tag
  write back without having to work out whether it ran: restoring the original
  over a file that was never written is a no-op worth doing.
- Integrate as `Operation::WriteTags` so tag edits appear in previews, are
  committed in the same transaction as moves, and are undone together.
- Refuse read-only files in preflight with a clear message.

**Each container is edited in its own vocabulary, not through a generic model.**
`lofty` offers `SplitTag`/`MergeTag` as a lossless round trip, and for *values* it
is one. Measured against real files it is **not** lossless about the frames MPDFM
was not asked to change, which is the property this task exists to deliver.
Changing one `genre` through the generic tag:

| | |
| --- | --- |
| `TCMP=PMEDIA` | **gone** — it maps to a compilation *flag*, and a value that is not a flag is discarded on the way back |
| `TXXX:ITUNESADVISORY=PMEDIA` | **gone**, the same way |
| `TRCK=07` | became `TRCK=7` — the split parses the number and the merge reprints it |
| `USLT` | re-encoded from Latin-1 to UTF-8 |
| m4a `disk` | re-encoded from the file's own byte layout into `lofty`'s |
| FLAC `title=` | re-cased to `TITLE=`, and every other key with it |
| FLAC `encoder=` | **gone** — the merge turns the first `EncoderSoftware` item into the vendor string |

So `tags::write` removes the edited field's native keys and puts the new value
back the way that container spells it, touching nothing else. The one table
shared with the reader is `read::native_keys`, so the two cannot disagree about
what a field *is*.

**An mp3 is written through its ID3v2 tag, not through its file**, which splices
one region at the front and leaves every byte after it alone. Writing the file
re-encodes every tag it parsed, and 25 of the library's mp3s also carry an APEv2
tag whose header flags `lofty` then corrects (they have the "has footer" bit clear
with a footer present, which is malformed, and `lofty` is right about it). It also
guarantees an mp3's ID3v1 tag survives untouched, which matters because MPD reads
it when there is nothing else. FLAC and MP4 are written through the file, because
there the tag is not a region that can be spliced.

**Every write is read back before it is published.** Not paranoia about `lofty`:
two real files carry two stacked ID3v2 tags, which `lofty` reads as one merged tag
and writes back over the *first* — so the second survives with the old value and
still wins on the next read. The write reported success and the file did not
change. "I pressed save and nothing happened" is the worst thing a tag editor can
do, so the temp file is re-read and every edited field checked before the rename.
One extra tag read, about a millisecond.

## Acceptance criteria

- [x] setting `genre` leaves every other frame byte-identical (compare full tag
      dumps before/after, including embedded art and ReplayGain)
- [x] embedded cover art survives a tag write (explicit test with an art fixture)
- [x] a v2.3 file stays v2.3 under the default policy; `id3_version = "v24"`
      upgrades it
- [x] clearing a field removes the frame rather than writing an empty string
- [x] FLAC multi-valued fields are written correctly and not collapsed
- [x] the write is atomic: an injected failure leaves the original intact
- [x] audio data is bit-identical after a tag write (hash the audio stream, not
      the file)
- [x] `WriteTags` inside a mixed plan commits and undoes with the moves
- [x] undo restores the original tags byte-for-byte
- [x] read-only file fails preflight before any temp file is created

## Hand-verified against the real library

The pitfall below asks for real files, so **every one of them** was used: all
2 808 readable audio files were copied out of `~/Music` one at a time into a temp
directory, given a `--genre` edit, compared, and restored from the backup. The
library itself was only ever read (`docs/PLAN.md` §8).

| | |
| --- | --- |
| **2 792** | written, with only the edited field changed, audio stream bit-identical, and restored byte-for-byte |
| **16** | refused in preflight, nothing written: 14 whose container cannot be identified from their own bytes, 2 with stacked ID3v2 tags |
| **25** | lose exactly one unmodeled frame `lofty` will not re-emit (12 × `TDRC=2020-25-12`, 12 × a `WXXX` with an empty description, 1 × `TDRL` on a v2.3 file) |

Four shapes came out of this that no synthesized fixture would have produced, and
all four are now fixtures (`testing::tags`) so they stay fixed:

- **`COMM` with language `\x00\x00\x00`** (18 files). No conforming writer will
  emit that, so `lofty` refused the whole tag and a `--genre` edit failed with a
  message about frame languages. Repaired to `XXX`, ID3v2's own "unknown
  language"; the comment's text, description and encoding are untouched. It is the
  one thing in the writer that changes something the delta did not name, and the
  alternative was eighteen files the editor cannot touch because of three bytes
  that were never valid.
- **More than 1 KiB of padding between the ID3v2 tag and the first MPEG frame**
  (2 albums, ~2 KiB each). `lofty`'s default search window is 1 KiB and its own
  documentation says some files need more, so MPDFM asks for 64 KiB.
- **Megabytes of zero bytes instead of audio** (14 files, 55–95 % zeros — broken
  downloads). Their tags are intact, so reading them works and the editor can show
  the user what is there; writing is refused as `TagError::Damaged`, because
  `lofty` needs the container identified from the bytes and no probe can get past
  that.
- **Two stacked ID3v2 tags** (2 files). Caught by the read-back check above.
- **Three `COMM` frames, two of them with descriptions** — found when task 19 ran
  the real command against a copy of a real album. The read-back check refused the
  write, correctly: the reader was treating all three as the comment and the
  writer only touches the undescribed one. Fixed in the *reader* (task 16), which
  is where a field's meaning belongs.

A second pass over all 2 808 files with `--genre X --clear comment` confirms the
same three categories and nothing new.

## Files

`crates/core/src/tags/write.rs`, `crates/core/tests/tags_write.rs`

Also touched: `ops/exec_fs.rs` (`FsStep::WriteTags`, `Done::TagsWritten`, and the
`revert` that restores from the backup), `ops/op.rs` (`Operation::WriteTags`),
`ops/plan.rs` (its expansion, the ordering rule that a tag edit runs before a move
of the same file, and `Conflict::NotTaggable` / `Conflict::DuplicateEdit`),
`ops/effects.rs` (`Summary::tags_written` / `fields_changed`), `ops/render.rs` (the
`TAG` rows), `ops/commit.rs` (backup retargeting and the drift check),
`journal/undo.rs` and `journal/recover.rs` (reversing and reconstructing a tag
write), `config.rs` (`id3_version`), and `testing/tags.rs` (the four fixtures
above).

## Pitfalls

- mtime: MPD detects changes by mtime, so **do** let the mtime advance on a tag
  write (or trigger an `update` afterwards, which task 11 already does).
  Preserving the old mtime would leave MPD showing stale tags.
- Some of these files came from scene releases with unusual or padded ID3 tags;
  test against real files copied out of the library, not just synthesized ones.
