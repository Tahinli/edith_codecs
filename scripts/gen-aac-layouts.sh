#!/usr/bin/env bash
# Usage: scripts/gen-aac-layouts.sh <fixtures/audio dir>
# Generates the 3.0, 4.0, 5.0 and 7.1 48 kHz FLAC fixtures, same construction as gen-fixtures.sh's 5.1.
set -euo pipefail
OUT=$1
FREQS=(220 440 660 55 880 1320 1760 2200)
gen() { # name nch layout
  local name=$1 nch=$2 layout=$3 ain=() lbl="" i
  for ((i = 0; i < nch; i++)); do
    ain+=(-f lavfi -i "sine=frequency=${FREQS[i]}:sample_rate=48000:duration=3")
    lbl="${lbl}[${i}:a]"
  done
  ffmpeg -v error -y "${ain[@]}" -filter_complex "${lbl}join=inputs=${nch}:channel_layout=${layout}[a]" \
    -map "[a]" -c:a flac -compression_level 5 -ar 48000 "$OUT/flac-${name}-48000.flac"
}
gen 3.0 3 3.0
gen 4.0 4 4.0
gen 5.0 5 5.0
gen 7.1 8 7.1
