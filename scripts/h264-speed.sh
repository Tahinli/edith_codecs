#!/usr/bin/env bash
# h264-speed.sh <frames> <threads> [features]: 1080p30 encode speed, noisy and clean testsrc2.
# Run it through scripts/remote-run.sh. Clips are cached in $HOME/.cache/h264speed.
set -euo pipefail
frames=${1:-60}; threads=${2:-1}; feat=${3:-}
d=$HOME/.cache/h264speed; mkdir -p "$d"
for kind in noisy clean; do
  f=$d/$kind-$frames.yuv
  if [ ! -s "$f" ]; then
    g="testsrc2=size=1920x1080:rate=30:duration=$(awk -v n="$frames" 'BEGIN{print n/30}')"
    [ $kind = noisy ] && g="$g,noise=alls=24:allf=t"
    ffmpeg -nostdin -v error -y -f lavfi -i "$g" -pix_fmt yuv420p -f rawvideo "$f"
  fi
done
cargo build --release -p ec-h264 --example h264_clip_encode ${feat:+--features $feat} 2>&1 | tail -2
bin=${CARGO_TARGET_DIR:-target}/release/examples/h264_clip_encode
for kind in noisy clean; do
  echo "== $kind threads=$threads"
  bash -c "time \"\$0\" \"\$@\"" "$bin" "$d/$kind-$frames.yuv" 1920 1080 30 4000000 26 "$threads" none
done
