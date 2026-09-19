#!/usr/bin/env bash
# gen.sh — regenerate the display-matrix fixtures in this directory.
#
# `rot<deg>.mp4` are one 320x240 testsrc2 clip (deterministic content) written
# back out with an ISO-BMFF `tkhd` display matrix asking for `deg` degrees:
# exactly how a phone records a portrait video (the pixels stay landscape and
# the container says which way to turn them). A player that ignores the matrix
# shows the picture sideways, which is the bug these fixtures exist to catch.
#
# `-display_rotation` is an *input* option here, so it goes **before** the `-i`
# it applies to; the `-metadata:s:v rotate=90` form every search result gives
# writes no display matrix at all with this machine's ffmpeg.
#
# Bit-exact reproduction (ffmpeg 8.1.2, libx264): the whole directory is
# regenerated with the same bytes the committed fixtures carry -- the four
# differs only inside the tkhd matrix. Verify with `sha256sum rot*.mp4`.
#
# Usage: crates/ec-mp4/tests/data/gen.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)
src=$(mktemp --suffix=.mp4)
trap 'rm -f -- "$src"' EXIT

ffmpeg -nostdin -y -v error -f lavfi \
    -i "testsrc2=size=320x240:rate=24:duration=1" \
    -c:v libx264 -profile:v baseline -pix_fmt yuv420p "$src"

for deg in 0 90 180 270 45; do
    ffmpeg -nostdin -y -v error -display_rotation "$deg" -i "$src" \
        -c copy "$here/rot$deg.mp4"
done
