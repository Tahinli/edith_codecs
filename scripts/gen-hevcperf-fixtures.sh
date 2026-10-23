#!/usr/bin/env bash
# gen-hevcperf-fixtures.sh: libx265 Annex-B streams for the ec-h265 decode benchmark.
#
# Writes raw H.265 elementary streams (Annex B, no container) to
#   ${HEVCPERF_FIXTURES:-$HOME/.cache/hevcperf-fixtures}
# Nothing is written under the repo or /tmp. The streams are large, so they are never
# committed; this script is the reproducible recipe.
#
# Streams:
#   hevc1080_moving.hevc  1920x1080 30 fps, 4 s, testsrc2 blended with a mandelbrot zoom plus
#                         grain (moving, not trivially easy)
#   hevc4k_moving.hevc    3840x2160 30 fps, 4 s, same content
#   hevc1080_test.hevc    1920x1080 30 fps, 4 s, plain testsrc2 (easy reference point)
#
# All use keyint=60 and bframes=3 (open-GOP B pyramids exercise inter, merge and DPB paths).

set -euo pipefail

OUT=${HEVCPERF_FIXTURES:-$HOME/.cache/hevcperf-fixtures}
mkdir -p "$OUT"
X265="keyint=60:bframes=3"

gen_moving() {  # gen_moving <size> <out-name>
  local size=$1 name=$2
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "mandelbrot=size=${size}:rate=30" \
    -f lavfi -i "testsrc2=size=${size}:rate=30" \
    -filter_complex "[0:v][1:v]blend=all_mode=average,noise=alls=8:allf=t,format=yuv420p[v]" \
    -map "[v]" -t 4 \
    -c:v libx265 -preset medium -x265-params "$X265" \
    -f hevc "$OUT/$name"
}

gen_test() {  # gen_test <size> <out-name>
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=$1:rate=30:duration=4" \
    -pix_fmt yuv420p \
    -c:v libx265 -preset medium -x265-params "$X265" \
    -f hevc "$OUT/$2"
}

gen_moving 1920x1080 hevc1080_moving.hevc
gen_moving 3840x2160 hevc4k_moving.hevc
gen_test 1920x1080 hevc1080_test.hevc

ls -l "$OUT"
