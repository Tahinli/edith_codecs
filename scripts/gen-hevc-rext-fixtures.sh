#!/usr/bin/env bash
# gen-hevc-rext-fixtures.sh — regenerate crates/ec-h265/tests/data/rext/*.265 and their .md5.
#
# Each stream is synthetic (ffmpeg lavfi testsrc2 plus temporal noise, no personal media),
# encoded by libx265 in one RExt pixel format with one set of tool switches. The sidecar .md5
# is the MD5 of ffmpeg's own decode of the stream as raw planar samples (8-bit: 1 byte,
# above 8 bits: 2 bytes little endian). The ec-h265 test `rext_generated` decodes the stream
# and must match that MD5.
#
# Usage: scripts/gen-hevc-rext-fixtures.sh [outdir]   (needs ffmpeg with libx265)
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
OUT=${1:-$ROOT/crates/ec-h265/tests/data/rext}
mkdir -p "$OUT"
SRC=(-f lavfi -i "testsrc2=size=128x96:rate=25,noise=alls=8:allf=t" -frames:v 6)

gen() { # name pix_fmt x265-params
  local name=$1 fmt=$2 params=$3
  ffmpeg -v error -y "${SRC[@]}" -pix_fmt "$fmt" -c:v libx265 -preset fast \
    -x265-params "log-level=error:$params" -f hevc "$OUT/$name.265"
  ffmpeg -v error -y -i "$OUT/$name.265" -f rawvideo -pix_fmt "$fmt" - | md5sum | cut -d' ' -f1 \
    >"$OUT/$name.md5"
  echo "$name $fmt $(stat -c %s "$OUT/$name.265") bytes"
}

BASE="keyint=8:bframes=2:weightp=1:weightb=1:qp=22"
for f in yuv444p yuv444p10le yuv444p12le yuv422p yuv422p10le yuv422p12le gray gray10le gray12le; do
  gen "gen_${f}" "$f" "$BASE"
done
# Tool switches on the formats that matter.
for f in yuv444p yuv444p10le yuv422p10le yuv444p12le; do
  gen "tskip_${f}" "$f" "$BASE:tskip=1:tskip-fast=0:rect=1:amp=1:sao=1"
  gen "lossless_${f}" "$f" "keyint=8:bframes=2:lossless=1"
  gen "wpp_${f}" "$f" "$BASE:wpp=1:constrained-intra=1:strong-intra-smoothing=0"
done
