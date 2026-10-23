#!/usr/bin/env bash
# ffmpeg-psnr.sh: per-plane PSNR of a decoded raw I420 file against its source,
# computed by ffmpeg's own psnr filter (per-frame values from its stats file).
#
# Prints `frames psnr_y psnr_u psnr_v psnr_yuv` where each plane is the mean over
# frames of ffmpeg's per-frame PSNR, and psnr_yuv is the JVET 6:1:1 weighting of
# those plane means, (6 Y + U + V) / 8.
#
# Usage: scripts/ffmpeg-psnr.sh <ref.yuv> <dec.yuv> <width> <height>
# Environment: PIX (default yuv420p; yuv420p10le for 10-bit files).
set -euo pipefail

ref=$1
dec=$2
w=$3
h=$4
stats=$(mktemp "$HOME/.cache/psnr-stats.XXXXXX")
trap 'rm -f "$stats"' EXIT
ffmpeg -v error -f rawvideo -pix_fmt ${PIX:-yuv420p} -s "${w}x${h}" -i "$dec" \
  -f rawvideo -pix_fmt ${PIX:-yuv420p} -s "${w}x${h}" -i "$ref" \
  -lavfi "[0:v][1:v]psnr=stats_file=$stats" -f null -
awk '
  # ffmpeg prints inf for a frame that matches exactly; such frames are counted
  # apart and left out of the means, which is what the PSNR of an exact frame is.
  {
    for (i = 1; i <= NF; i++) {
      split($i, kv, ":")
      if (kv[1] == "psnr_y" || kv[1] == "psnr_u" || kv[1] == "psnr_v") {
        if (kv[2] == "inf") { if (kv[1] == "psnr_y") exact++; continue }
        if (kv[1] == "psnr_y") { sy += kv[2]; ny++ }
        if (kv[1] == "psnr_u") { su += kv[2]; nu++ }
        if (kv[1] == "psnr_v") { sv += kv[2]; nv++ }
      }
    }
  }
  END {
    if (ny == 0) { printf "frames=%d exact=%d psnr_y=inf psnr_u=inf psnr_v=inf psnr_yuv=inf\n", exact, exact; exit 0 }
    y = sy / ny; u = su / nu; v = sv / nv
    printf "frames=%d exact=%d psnr_y=%.4f psnr_u=%.4f psnr_v=%.4f psnr_yuv=%.4f\n", ny + exact, exact, y, u, v, (6 * y + u + v) / 8
  }' "$stats"
