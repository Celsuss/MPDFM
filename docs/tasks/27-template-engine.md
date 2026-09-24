# 27 — Organize template engine

- **Phase:** M4 · Organize by template
- **Depends on:** 02, 05, 16
- **Status:** not started

## Goal

Turn tags into destination paths, so `hiphop/Snoop Dogg & Wiz Khalifa - Mac +
Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3`
can become something predictable — safely, and with every awkward character
handled.

## Details

Template syntax:

```
{genre}/{albumartist}/{year} - {album}/{track:02} {title}
```

Tokens: `artist`, `albumartist` (falling back to `artist`), `album`, `title`,
`genre`, `year`, `track`, `disc`, `ext` (always appended automatically),
`original_dir`, `filename`. Modifiers: `{track:02}` zero-padding,
`{artist:upper}`/`:lower`/`:title`, `{album?}` (omit the segment entirely if the
tag is absent), and a literal-default form `{genre|Unsorted}`.

Sanitization, per path segment:

- replace `/` and NUL (always illegal)
- optionally replace `: * ? " < > | \` for portability to FAT/NTFS (config flag,
  default on — these files may end up on a phone or USB stick)
- collapse repeated whitespace, trim leading/trailing whitespace and dots
- refuse a segment that is empty after sanitization, or named `.` / `..`
- truncate segments to 255 **bytes** (not chars) while keeping valid UTF-8, and
  the whole path to 4096 bytes; truncate in the middle, keeping the extension
- keep the original extension exactly, lowercased optionally

Multi-disc handling: when `disc` total > 1, include a disc segment (or
`{disc}-{track:02}` numbering); this library's
`Imagine Dragons - Mercury (2 CD)/CD 1 - …` set is the test case.

Missing tags: a file missing `album` or `albumartist` cannot be placed reliably.
Emit it as a `Warning::Unplaceable` and **leave it where it is** rather than
inventing `Unknown Artist/Unknown Album` — unless the template used the
`{tag|default}` form, which is the explicit opt-in.

Aux files travel with their album directory: a directory whose audio files all
map to the same destination directory moves its aux files (jpg, nfo, cue, log,
sfv) there too. If the audio files of one directory map to *different*
destinations, the album is being split: warn loudly and move aux files only if
the user confirms, defaulting to leaving them.

Collisions: two files mapping to the same destination is a conflict, reported
with both sources. Do not auto-suffix ` (1)`.

## Acceptance criteria

- [ ] every token and modifier has a test, including `{album?}` and `{genre|Unsorted}`
- [ ] `/` in a tag value (e.g. genre `Hip Hop/Rap`) is replaced, never creating a
      directory level
- [ ] `: ? " *` replacement is on by default and can be disabled
- [ ] a 300-byte album name truncates to ≤255 bytes, stays valid UTF-8, keeps `.mp3`
- [ ] a file missing `album` is reported unplaceable and not moved
- [ ] the multi-disc fixture produces distinct, ordered paths for both discs
- [ ] two files colliding on one destination is a conflict naming both
- [ ] aux files follow a wholly-moved album directory
- [ ] an album whose files would split across destinations warns and leaves aux
      files alone
- [ ] case-only destination differences are detected (feeds task 08's warning)
- [ ] a template that produces a path outside `music_dir` is rejected
- [ ] real names from this library are used as fixtures, including
      `Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube`

## Files

`crates/core/src/organize/{mod.rs,template.rs,sanitize.rs,plan.rs}`

## Pitfalls

- The 255-byte limit is per path component on ext4 and is in *bytes*; a CJK album
  name hits it at ~85 characters.
- Never invent metadata to make a file placeable. Leaving a file alone is always
  the safer default, and the warning tells the user what to fix.
