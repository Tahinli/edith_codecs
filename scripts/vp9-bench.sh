#!/usr/bin/env bash
# vp9-bench.sh: ec-vp9 decode throughput sweep. Run on a server through remote-run.sh.
#
# Usage: scripts/vp9-bench.sh [clip.ivf ...]
# Builds the decode_ivf example in release, then decodes each clip at 1, 2 and 4 tile
# threads. With no arguments it uses tile1080.ivf (fixtures/vp9-bench) and a libvpx-encoded
# 1080p30 testsrc2 clip with 4 tile columns, encoded here with ffmpeg.
# Prints one line per run: clip threads fps digest.
set -euo pipefail
cd "$(dirname "$0")/../crates/ec-vp9"
cargo build --release --example decode_ivf 2>&1 | tail -1
bin=$(cd ../.. && pwd)/target/release/examples/decode_ivf
[ -x "$bin" ] || bin=${CARGO_TARGET_DIR:-../../target}/release/examples/decode_ivf
tmp=${TMPDIR:-/tmp}/vp9bench
mkdir -p "$tmp"
clips=("$@")
if [ ${#clips[@]} -eq 0 ]; then
  clips=("$HOME/fixtures/vp9-bench/tile1080.ivf")
  if [ ! -s "$tmp/tile4-1080p30.ivf" ]; then
    ffmpeg -v error -y -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 \
      -c:v libvpx-vp9 -tile-columns 2 -row-mt 1 -b:v 4M -deadline realtime -cpu-used 8 \
      "$tmp/tile4-1080p30.ivf"
  fi
  clips+=("$tmp/tile4-1080p30.ivf")
fi
for clip in "${clips[@]}"; do
  for t in 1 2 4; do
    line=$("$bin" "$clip" "$t")
    echo "$(basename "$clip") threads=$t $line"
  done
done
