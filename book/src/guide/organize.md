# Organizing to a template

`mpdfm organize` re-files tracks into paths derived from their tags, rewriting
every playlist reference on the way — the same transaction as `move`.

```console
$ mpdfm organize --dry-run --limit 20          # a cautious first look
$ mpdfm organize --limit 20                    # the first twenty tracks' worth
$ mpdfm organize --only-missing                # then the rest, whose tags are complete
```

The default template is

```text
{genre}/{albumartist}/{year} - {album}/{track:02} {title}
```

Change it with `-t` / `--template`, or set `organize_template` in the
[configuration](../reference/config.md). A track whose tags the template cannot
fill is left where it is and listed — never filed under "Unknown".

## Template syntax

A template is `/`-separated segments of literal text and `{fields}`. The source
file's extension is always appended to the last segment.

| Field | Value |
| --- | --- |
| `artist` | the track artist |
| `albumartist` | the album artist, falling back to `artist` |
| `album`, `title`, `genre` | as tagged |
| `year` | the leading four digits of the date |
| `track`, `disc` | the number, without its total |
| `ext` | the source file's extension, without the dot |
| `original_dir` | the directory the file is in now — a whole segment on its own |
| `filename` | the file's current name, without its extension |

A multi-valued tag renders joined with `; `.

| Form | Meaning |
| --- | --- |
| `{track:02}` | zero-pad a number to two digits (`year`, `track`, `disc`) |
| `{artist:upper}`, `:lower`, `:title` | change case (text fields) |
| `{album?}` | when `album` is absent, leave out the **whole segment** |
| `{genre\|Unsorted}` | when `genre` is absent, use `Unsorted` |
| `{{`, `}}` | a literal brace |

A field written plainly is **required**: a file without it stays where it is. `?`
and `|default` are the explicit opt-ins to place it anyway.

## Multi-disc sets

When a file is one disc of several and the template does not place the disc
itself, a `Disc N` directory is added in front of the file name. A file that is
clearly one disc of several but has no disc number is unplaceable.

## Working through a big library

- `--limit N` stages at most N tracks' worth of moves.
- `--only-missing` only moves files whose tags are complete for the template and
  that are not already in place.
- `--no-aux` moves tracks only; cover art, `.nfo` and `.cue` files stay put.

See also the [`organize` reference](../reference/cli/organize.md).
