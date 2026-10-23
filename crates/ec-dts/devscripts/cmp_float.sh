#!/usr/bin/env bash
# Compare ec-dts against ffmpeg's default (floating point) decoder path.
set -u
d=${1:-$HOME/dts-fate}
dec=${CARGO_TARGET_DIR:-target}/release/examples/dec
here=$(cd "$(dirname "$0")" && pwd)
out=${TMPDIR:-/tmp}/dtscmpf
mkdir -p "$out"
for f in "$d"/*.dtshd "$d"/dts_es.dts "$here"/../fixtures/*.dts; do
  b=$(basename "$f")
  case "$b" in xll*|xbr*) continue;; esac
  co="-core_only 1"; case "$b" in xch*|dts_es*|x96*|*.dts) co="";; esac
  "$dec" "$f" "$out/$b.ours" 2>/dev/null
  ffmpeg -v error -y $co -i "$f" -f f32le "$out/$b.ref" 2>&1 | tail -2
  nch=$(ffprobe -v error -show_entries stream=channels -of csv=p=0 "$f")
  perm=""; [ "$nch" = 7 ] && perm="0,1,2,3,6,4,5"
  skip=0; case "$b" in x96*) skip=2048;; *.dtshd) skip=1024;; esac
  echo "== $b"
  python3 "$here/compare.py" "$out/$b.ours" "$out/$b.ref" "$nch" f32 "$perm" "$skip" | sed -n 2p
done
