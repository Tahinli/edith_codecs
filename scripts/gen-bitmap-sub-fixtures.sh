#!/usr/bin/env bash
# gen-bitmap-sub-fixtures.sh: regenerate the DVD/DVB subtitle fixtures of ec-dvdsub and ec-dvbsub.
#
# A synthetic 720x576 PGS source (scripts/gen-bitmap-sub-fixtures.py, PIL text, four cues at
# 0.5-1.5, 2.0-3.2, 3.5-4.5, 5.0-6.0 s) is encoded by ffmpeg's dvdsub and dvbsub encoders (they
# take bitmaps only). Needs ffmpeg and python3 with PIL. dvd.idx is written by hand from dvd.sub.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/bitmapsub.XXXXXX")
trap 'rm -rf "$T"' EXIT
python3 "$ROOT/scripts/gen-bitmap-sub-fixtures.py" "$T/src.sup"
S=(-fix_sub_duration -f sup -i "$T/src.sup")
DVD="$ROOT/crates/ec-dvdsub/tests/data"; DVB="$ROOT/crates/ec-dvbsub/tests/data"
mkdir -p "$DVD" "$DVB"
ffmpeg -v error -y "${S[@]}" -c:s dvdsub -map 0 "$DVD/dvd.mkv"
ffmpeg -v error -y -f lavfi -i color=c=black:s=720x576:r=25:d=7 "${S[@]}" -c:v mpeg2video -b:v 200k \
  -maxrate 9000k -bufsize 1835k -c:s dvdsub -map 0 -map 1 -target pal-dvd -an "$DVD/dvd.vob"
ffmpeg -v error -y -f lavfi -i color=c=black:s=64x64:r=5:d=7 "${S[@]}" -c:v mpeg4 -c:s dvdsub -map 0 -map 1 "$DVD/dvd.mp4"
ffmpeg -v error -y "${S[@]}" -c:s dvdsub -map 0 -f vob "$DVD/dvd.sub"
ffmpeg -v error -y -f lavfi -i color=c=black:s=64x64:r=5:d=7 "${S[@]}" -c:v mpeg2video -b:v 50k -c:s dvbsub -map 0 -map 1 "$DVB/dvb.ts"
ffmpeg -v error -y "${S[@]}" -c:s dvbsub -map 0 "$DVB/dvb.mkv"
echo "fixtures written (dvd.idx is hand-made; see the test)"
