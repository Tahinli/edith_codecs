#!/usr/bin/env bash
# Decode each FATE sample with ec-dts and ffmpeg (-flags +bitexact, integer
# path) and compare. Needs the dec example built (release).
set -u
d=${1:-$HOME/dts-fate}
dec=${CARGO_TARGET_DIR:-target}/release/examples/dec
here=$(cd "$(dirname "$0")" && pwd)
out=${TMPDIR:-/tmp}/dtscmp
mkdir -p "$out"
for f in "$d"/*; do
  b=$(basename "$f")
  case "$b" in *.f32|*.pcm) continue;; esac
  echo "== $b"
  "$dec" "$f" "$out/$b.ours" 2>&1 | tail -3
  co="-core_only 1"; case "$b" in xch*|dts_es*|x96*) co="";; esac
  ffmpeg -v error -y -flags +bitexact $co -i "$f" -f s32le "$out/$b.ref" 2>&1 | tail -2
  nch=$(ffprobe -v error -show_entries stream=channels -of csv=p=0 "$f")
  perm=""; [ "$nch" = 7 ] && perm="0,1,2,3,6,4,5"
  skip=0; case "$b" in x96*) skip=2048;; *.dtshd) skip=1024;; esac
  python3 "$here/compare.py" "$out/$b.ours" "$out/$b.ref" "$nch" s32 "$perm" "$skip" | head -${LINES_MAX:-12}
done
