#!/usr/bin/env bash
# hevc-intra-bd.sh: intra-only (keyint=1) BD-rate of ec-h265 against x265 intra on the three
# clips the ec-h265 encode gate names. Thin wrapper over scripts/hevc-bdrate.sh with GOP 1.
# Clips are generated with ffmpeg lavfi (no copyrighted media) into $DATA.
#
# The x265 anchor uses -tune psnr by default (see hevc-bdrate.sh). Set X265_TUNE= for x265's
# default tuning. Each tuning writes its own point files under $DATA/tune-<name>/.
#
# Usage: scripts/hevc-intra-bd.sh <clip-name|all> [QPS]
#   clip-name: testsrc2-352x288 | testsrc2-1280x720 | natural-1280x720 | all
# Environment: DATA (default $HOME/.cache/hevcintra-data).
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
which=${1:-all}
qps=${2:-"22 27 32 37"}
data=${DATA:-$HOME/.cache/hevcintra-data}
mkdir -p "$data"
frames=30
gen() {  # gen <out.yuv> <lavfi graph>
  [ -f "$1" ] || ffmpeg -v error -y -f lavfi -i "$2" -frames:v $frames -pix_fmt yuv420p -f rawvideo "$1"
}
gen "$data/testsrc2-352x288.yuv" "testsrc2=size=352x288:rate=30"
gen "$data/testsrc2-1280x720.yuv" "testsrc2=size=1280x720:rate=30"
# "natural": a moving mandelbrot zoom blended with testsrc2 and film-like noise (the
# gen-hevcperf-fixtures.sh moving clip), so it has texture rather than flat synthetic edges.
[ -f "$data/natural-1280x720.yuv" ] || ffmpeg -v error -y \
  -f lavfi -i "mandelbrot=size=1280x720:rate=30" \
  -f lavfi -i "testsrc2=size=1280x720:rate=30" \
  -filter_complex "[0:v][1:v]blend=all_mode=average,noise=alls=8:allf=t,format=yuv420p[v]" \
  -map "[v]" -frames:v $frames -f rawvideo "$data/natural-1280x720.yuv"
for spec in "testsrc2-352x288 352 288" "testsrc2-1280x720 1280 720" "natural-1280x720 1280 720"; do
  set -- $spec
  [ "$which" = all ] || [ "$which" = "$1" ] || continue
  DATA="$data/tune-${X265_TUNE-psnr}" "$here/scripts/hevc-bdrate.sh" "$data/$1.yuv" "$2" "$3" 1 "$qps"
  echo "----"
done
