#!/usr/bin/env bash
# hevc-preset-quick.sh: single-thread encode time and size of the 1080p 10-bit bench clip per preset.
set -e
cargo build --release -q -p ec-h265 --example hevc_gop 2>&1 | grep -E '^error' -A8 || true
B=${CARGO_TARGET_DIR:-target}/release/examples/hevc_gop; D=$HOME/.cache/hevc10simd2-bench
echo load $(cut -d' ' -f1-3 /proc/loadavg)
for p in ${PRESETS:-default fast faster}; do
  echo -n "$p: "; EC_H265_PRESET=$p EC_H265_BITDEPTH=10 EC_H265_THREADS=1 EC_H265_TIMING=1 $B encode $D/c10.yuv 1920 1080 8 30 $D/o.hevc $D/r.yuv 2>&1 | grep -v PROF | tr "\n" " "; echo
done
