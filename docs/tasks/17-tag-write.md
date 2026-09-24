# 17 — Tag writing

- **Phase:** M2 · Tag editing
- **Depends on:** 11, 16
- **Status:** not started

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
- Integrate as `Operation::WriteTags` so tag edits appear in previews, are
  committed in the same transaction as moves, and are undone together.
- Refuse read-only files in preflight with a clear message.

## Acceptance criteria

- [ ] setting `genre` leaves every other frame byte-identical (compare full tag
      dumps before/after, including embedded art and ReplayGain)
- [ ] embedded cover art survives a tag write (explicit test with an art fixture)
- [ ] a v2.3 file stays v2.3 under the default policy; `id3_version = "v24"`
      upgrades it
- [ ] clearing a field removes the frame rather than writing an empty string
- [ ] FLAC multi-valued fields are written correctly and not collapsed
- [ ] the write is atomic: an injected failure leaves the original intact
- [ ] audio data is bit-identical after a tag write (hash the audio stream, not
      the file)
- [ ] `WriteTags` inside a mixed plan commits and undoes with the moves
- [ ] undo restores the original tags byte-for-byte
- [ ] read-only file fails preflight before any temp file is created

## Files

`crates/core/src/tags/write.rs`

## Pitfalls

- mtime: MPD detects changes by mtime, so **do** let the mtime advance on a tag
  write (or trigger an `update` afterwards, which task 11 already does).
  Preserving the old mtime would leave MPD showing stale tags.
- Some of these files came from scene releases with unusual or padded ID3 tags;
  test against real files copied out of the library, not just synthesized ones.
