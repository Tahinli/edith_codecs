#!/usr/bin/env bash
# gen-mp4-parity-corpus.sh: write the generated mp4/mov cases that the ec-mp4 parity
# test (crates/ec-mp4/tests/it/edith_parity.rs) compares against ffprobe.
#
# Usage: scripts/gen-mp4-parity-corpus.sh <out-dir>
#
# Each case isolates one thing the demuxer must read the way edith does:
#   bframes.mp4    B-frames: ctts v0 offsets, a sample order that differs from display order
#   negcts.mp4     negative composition offsets (ctts v1), -movflags negative_cts_offsets
#   rot90.mp4      tkhd display matrix of a quarter turn (copied from tests/data)
#   rot270.mp4     the other quarter turn (copied from tests/data)
#   qt.mov         QuickTime container (-f mov): mp4a in a sound description
#   emptyedit.mp4  an audio track delayed by 0.5 s: an empty edit ahead of the media
#   aac-prime.mp4  AAC audio alone: the encoder delay is an edit list (elst) of its own
#   mp3.mp4        an mp4a entry carrying MP3
#
# Needs ffmpeg with libx264. Output is deterministic for a given ffmpeg build.
set -euo pipefail
out=${1:?usage: gen-mp4-parity-corpus.sh <out-dir>}
here=$(cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$out"
cd "$out"
ff() { ffmpeg -hide_banner -v error -y "$@"; }
ff -f lavfi -i testsrc2=size=320x240:rate=24:duration=2 \
   -f lavfi -i sine=frequency=440:duration=2 \
   -c:v libx264 -bf 2 -g 24 -pix_fmt yuv420p -c:a aac -b:a 128k bframes.mp4
ff -f lavfi -i testsrc2=size=320x240:rate=24:duration=2 \
   -c:v libx264 -bf 2 -pix_fmt yuv420p -movflags negative_cts_offsets negcts.mp4
# The display matrices: stream copies of the committed rotation clips, whose tkhd
# matrix ffmpeg wrote with -display_rotation (crates/ec-mp4/tests/data/gen.sh).
ff -i "$here/../crates/ec-mp4/tests/data/rot90.mp4" -c copy rot90.mp4
ff -i "$here/../crates/ec-mp4/tests/data/rot270.mp4" -c copy rot270.mp4
ff -f lavfi -i testsrc2=size=320x240:rate=24:duration=2 \
   -f lavfi -i sine=frequency=440:duration=2 \
   -c:v libx264 -bf 2 -pix_fmt yuv420p -c:a aac -f mov qt.mov
ff -f lavfi -i sine=frequency=440:duration=2 -itsoffset 0.5 \
   -f lavfi -i testsrc2=size=64x64:rate=24:duration=2 \
   -map 0:a -map 1:v -c:v libx264 -c:a aac -shortest emptyedit.mp4
ff -f lavfi -i sine=frequency=440:duration=2 -c:a aac -b:a 96k aac-prime.mp4
ff -f lavfi -i sine=frequency=440:duration=2 -c:a libmp3lame -b:a 96k -f mp4 mp3.mp4
