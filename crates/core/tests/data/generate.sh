#!/usr/bin/env bash
# Regenerate the committed audio templates used by `mpdfm_core::testing`.
#
# The output of this script is COMMITTED (task 03): tests must never depend on
# ffmpeg being installed. Run it only when a template needs to change, then
# commit the result and say why in README.md.
#
# Every file is a 0.2–0.3 s mono sine wave — small enough to commit, real enough
# that `lofty` (tasks 16–18) can read and rewrite its tags.
set -euo pipefail

cd "$(dirname "$0")"
command -v ffmpeg >/dev/null || { echo "ffmpeg is required to regenerate these" >&2; exit 1; }

ff() { ffmpeg -y -hide_banner -loglevel error "$@"; }
MP3_SINE="sine=frequency=440:duration=0.25:sample_rate=22050"
FLAC_SINE="sine=frequency=440:duration=0.2:sample_rate=8000"
MP3=(-ac 1 -c:a libmp3lame -b:a 32k -write_xing 0)

# mp3, ID3v2.4 — the common case. `TRCK 1/15` exercises number-and-total.
ff -f lavfi -i "$MP3_SINE" "${MP3[@]}" -id3v2_version 4 \
    -metadata title="Beef Rap" -metadata artist="MF DOOM" \
    -metadata album_artist="MF DOOM" -metadata album="Mm..Food" \
    -metadata date="2004" -metadata track="1/15" -metadata disc="1/1" \
    -metadata genre="Hip-Hop" -metadata comment="MPDFM fixture" \
    sine-id3v24.mp3

# mp3, ID3v2.3 — task 16 must read TYER as well as TDRC. The `&` in the album
# name is deliberate: it is what the real library looks like.
ff -f lavfi -i "$MP3_SINE" "${MP3[@]}" -id3v2_version 3 \
    -metadata title="Wrecked" -metadata artist="Imagine Dragons" \
    -metadata album_artist="Imagine Dragons" \
    -metadata album="Mercury - Acts 1 & 2" -metadata date="2022" \
    -metadata track="5/12" -metadata disc="1/2" -metadata genre="Pop" \
    sine-id3v23.mp3

# flac, Vorbis comments. `-metadata_header_padding 0` is what keeps this under
# 1 KB; the full DATE exercises the year-narrowing pitfall in task 16.
ff -f lavfi -i "$FLAC_SINE" -ac 1 -c:a flac -compression_level 12 \
    -metadata_header_padding 0 \
    -metadata title="So Hï" -metadata artist="KREAM" \
    -metadata album_artist="KREAM" -metadata album="So Hï" \
    -metadata date="2019-03-15" -metadata track="3" -metadata TRACKTOTAL="9" \
    -metadata genre="Electronic" \
    sine.flac

# m4a atoms — 5 files in the real library, best-effort support (PLAN D5).
ff -f lavfi -i "$MP3_SINE" -ac 1 -c:a aac -b:a 32k \
    -metadata title="Freddie Freeloader" -metadata artist="Miles Davis" \
    -metadata album="Kind of Blue" -metadata date="1959" \
    -metadata track="2/5" -metadata genre="Jazz" \
    sine.m4a

# No tags at all: must read as an empty TagSet, not an error (task 16).
ff -f lavfi -i "$MP3_SINE" "${MP3[@]}" -map_metadata -1 -id3v2_version 0 \
    untagged.mp3

ls -l ./*.mp3 ./*.flac ./*.m4a
