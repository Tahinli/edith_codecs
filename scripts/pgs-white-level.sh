#!/usr/bin/env bash
# What a player draws a PGS subtitle's white as, measured rather than reasoned:
# ffmpeg burns fixtures/subs/pgs-1080p.sup over black and the brightest red
# channel in the first painted frame is printed. It is 235 -- the palette's own
# Y', not an expansion of the 16..235 video range to 255 -- which is what
# `ec_pgs::rgba` matches.
#
#   scripts/pgs-white-level.sh [file.sup]
set -euo pipefail

sup="${1:-$(dirname "$0")/../fixtures/subs/pgs-1080p.sup}"
[ -r "$sup" ] || { echo "no such .sup: $sup (fixtures are gitignored)" >&2; exit 1; }

size=$(ffprobe -v error -select_streams s:0 -show_entries stream=width,height \
    -of csv=p=0:s=x "$sup")
raw=$(mktemp -t pgs-white.XXXXXX)
trap 'rm -f "$raw"' EXIT

ffmpeg -v error -f lavfi -i "color=c=black:s=$size:r=1:d=60,format=rgba" -i "$sup" \
    -filter_complex "[0:v][1:s]overlay=format=rgb" -frames:v 60 \
    -pix_fmt rgba -f rawvideo -y "$raw"

python3 - "$raw" "$size" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
w, h = (int(v) for v in sys.argv[2].split('x'))
frame = w * h * 4
for i in range(len(data) // frame):
    reds = data[i * frame:(i + 1) * frame:4]
    peak = max(reds)
    if peak:
        print(f"frame {i}: brightest red {peak}, "
              f"{sum(1 for v in reds if v)} pixels drawn on")
        break
else:
    print("nothing painted in any frame", file=sys.stderr)
    sys.exit(1)
PY
