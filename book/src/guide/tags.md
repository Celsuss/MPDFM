# Editing tags

```console
$ mpdfm tag show -r "hiphop/Mac + Devin Go To High School"
$ mpdfm tag diff --genre "Hip Hop" --renumber-tracks -r "hiphop/Mac + Devin Go To High School"
$ mpdfm tag set  --genre "Hip Hop" --renumber-tracks -r "hiphop/Mac + Devin Go To High School"
```

- `tag show` prints the tags of a file, or of every audio file in a directory.
- `tag diff` shows what `tag set` would change, and writes nothing.
- `tag set` previews the change, asks, then writes.

`-r` / `--recursive` includes audio files in subdirectories.

## Setting fields

`--title`, `--artist`, `--album-artist`, `--album`, `--year`, `--track`, `--disc`,
`--genre`, `--comment` and `--composer` set a field on every selected file.
`--clear FIELD` removes one entirely.

## Bulk helpers

| Flag | Effect |
| --- | --- |
| `--renumber-tracks` | number the selected files 1..n in listed order and set the total |
| `--title-from-filename` | take each title from the file name, stripping a leading track number and the extension |
| `--album-artist-from-artist` | give each file its own artist as its album artist |
| `--strip-comment` | remove the comment from every selected file |
| `--trim-whitespace` | strip leading and trailing space from every text field |

## What does not change

Nothing else in a file changes when its tags do: embedded artwork, ReplayGain,
MusicBrainz ids, lyrics and every unknown frame survive, and the audio stream is
bit-identical. The original file is backed up before it is rewritten.

See also the [`tag` reference](../reference/cli/tag.md).
