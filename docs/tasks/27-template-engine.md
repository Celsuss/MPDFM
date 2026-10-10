# 27 — Organize template engine

- **Phase:** M4 · Organize by template
- **Depends on:** 02, 05, 16
- **Status:** done

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

- [x] every token and modifier has a test, including `{album?}` and `{genre|Unsorted}`
- [x] `/` in a tag value (e.g. genre `Hip Hop/Rap`) is replaced, never creating a
      directory level
- [x] `: ? " *` replacement is on by default and can be disabled
- [x] a 300-byte album name truncates to ≤255 bytes, stays valid UTF-8, keeps `.mp3`
- [x] a file missing `album` is reported unplaceable and not moved
- [x] the multi-disc fixture produces distinct, ordered paths for both discs
- [x] two files colliding on one destination is a conflict naming both
- [x] aux files follow a wholly-moved album directory
- [x] an album whose files would split across destinations warns and leaves aux
      files alone
- [x] case-only destination differences are detected (feeds task 08's warning)
- [x] a template that produces a path outside `music_dir` is rejected
- [x] real names from this library are used as fixtures, including
      `Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube`

## Hand-verified against the real library

Read-only, as `docs/PLAN.md` §8 allows, with the default template: 2 808 of
2 809 tracks read (the ADTS file from task 16 is the one that is not). **1 852
tracks and 135 aux files are placeable**, 956 are unplaceable, 0 collide, 27
album directories would split. The unplaceable list is the tagging to-do list task
28 will print: 153 files carry none of the fields the template uses, 251 lack
only `genre` (or `genre` and `year`), 141 only `track`. The six case warnings are real tagging
inconsistencies, not noise — `Hip-Hop/MF DOOM` vs `Hip-Hop/MF Doom`,
`Dance/ZHU` vs `Dance/Zhu` — plus `Pop`/`pop`-style genre directories next to the
existing lower-case ones that keep unplaceable files. Mapping takes 134 ms in a
debug build, after 4 s of tag reading.

## Decisions

**A field written plainly is required, whichever it is.** The spec singles out
`album` and `albumartist` because the default template uses them; the rule
implemented is the general one — any field the template needs without `?` or
`|default` makes a file without it unplaceable. A template that does not
mention `album` does not need it. All missing fields are reported at once, so the
to-do item is complete.

**A blank tag is an absent tag.** A genre of three spaces would otherwise reach
sanitization, be refused as an empty name, and be reported as unnamable instead
of as missing — the wrong to-do item. `?` and `|default` treat it as absent too.

**`?` is refused in the file-name segment**, at parse time: omitting the
segment that names the file has no meaning. `{title|Untitled}` is the way to
make it optional.

**A default is a literal.** `{genre:upper|Unsorted}` renders `Unsorted`, not
`UNSORTED`: the modifier shapes a tag value, the default is what the user typed.
It is still sanitized, so `{genre|Rock/Pop}` cannot add a directory level.

**`ext` is always appended; `.{ext}` at the end is accepted and means the same
thing.** Anywhere else `{ext}` is an ordinary value (`{ext}/{artist}/…` files by
format).

**`original_dir` is the whole current directory and must be a segment on its
own.** It is the one token that is a path rather than a value, so it expands to
several segments — each re-sanitized — and cannot be mixed with other text or
take a modifier. At the library root it expands to nothing. `filename` is the
current name without its extension.

**Multi-valued tags render joined with `; `**, as the editor shows them
(`tags::Values::joined`). Taking only the first would silently drop Wiz Khalifa.

**Multi-disc: a `Disc N` directory is inserted before the file name** when a
file is one disc of several — its disc tag has a total over 1 or a number over
1, or it sits in a disc directory per `AlbumDir::set_root` — and the template
does not mention `{disc}` itself. A directory rather than a `1-01` prefix,
because each disc carries its own cue sheet and cover, and two discs' aux files in
one directory would collide. A file that is visibly one disc of several but has
no disc tag is unplaceable (`missing disc`): inferring the number from `CD 1` in
the directory name would be inventing metadata.

**A set root's own aux files follow the set** when every disc moves whole and
the discs land together (in one directory, or side by side under one parent).
Otherwise they stay, and the set is reported as split.

**Aux files include subdirectories without audio.** `Scans/back.jpg` travels
with its album; a subdirectory holding audio anywhere below it is an album of its
own and does not.

**A split album leaves its aux files by default; `SplitAux::FollowMajority` is
the confirmed alternative**, sending them with the most tracks. A partial
selection of an album is a split too — the unselected tracks are staying behind.

**Conflicts are two kinds.** `Collision` — several sources rendering to one
destination, including one that is already there — names every source, and none
of them moves. `Occupied` — a destination taken by a file that is not itself
moving away, or by a directory — is found to a fixpoint, since blocking one move
can block another that was counting on it vacating.

**Case differences fold ASCII only, like task 08**, and are reported only at
the shallowest level: `Pop` vs `pop` is one warning, not one per track below.
Compared against the library as it will be after the moves, so a directory that
is being emptied does not count.

**Sanitizing replaces with `_`, for every refused character.** One rule makes
names predictable from tags. With portability on, C0 control characters go too,
since FAT refuses them. Truncation cuts the middle and puts `…` there, keeping
both ends (artist at the start, year and release group at the end); the whole
path is brought under `PATH_MAX` by cutting the file name first — it is the only
per-file segment, so cutting an album directory differently per track would split
the album — then the longest directory, down to a 32-byte floor, after which the
file is unplaceable.

**`organize_portable_names` is a new config key**, default `true`, documented in
`docs/config.example.toml`. Lower-casing the extension is an `Options` field
only, for task 28 to expose as a flag if it wants one.

**Escaping the music directory is refused at parse time**: a leading `/`, and a
literal `.` or `..` segment. Values cannot do it — `..` as a tag sanitizes to
nothing and the file is unplaceable — and every rendered path goes through
`RelPath::parse` as a final guard. Symlinks are resolved by the executor's own
`paths::contains` check at commit time, as for every move.

## Files

`crates/core/src/organize/{mod.rs,template.rs,sanitize.rs,plan.rs}`,
`crates/core/tests/organize.rs`

Also touched: `lib.rs` (`Error::Template`), `config.rs` and
`testing/mod.rs` (`organize_portable_names`), `docs/config.example.toml`.

## Pitfalls

- The 255-byte limit is per path component on ext4 and is in *bytes*; a CJK album
  name hits it at ~85 characters.
- Never invent metadata to make a file placeable. Leaving a file alone is always
  the safer default, and the warning tells the user what to fix.
