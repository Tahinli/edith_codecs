#!/usr/bin/env bash
# hevc-thread-bench.sh: encode lavfi clips with ec-h265 at 1, 2, 4 and 6 worker threads,
# report fps per thread count, and check that every thread count writes the same bytes.
#
# Usage: scripts/remote-run.sh hevcencthreads --host b -- scripts/hevc-thread-bench.sh
#
# Environment: DATA (default $HOME/.cache/hevcencthreads-data), GOP (keyframe interval,
# default 8), QP (default 30), FRAMES (default 12), CLIPS (default "1280x720 1920x1080"),
# THREADS (default "1 2 4 6"), RUNS (timed runs per point, default 1, best time kept).
# RC_KBPS: when set (e.g. 4000), encode with bitrate control at that rate and FPS (default 30)
# through the hevc_rc example instead of constant QP through hevc_gop.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
data=${DATA:-$HOME/.cache/hevcencthreads-data}
gop=${GOP:-8}
qp=${QP:-30}
frames=${FRAMES:-12}
clips=${CLIPS:-"1280x720 1920x1080"}
threads_list=${THREADS:-"1 2 4 6"}
runs=${RUNS:-1}
mkdir -p "$data"

rc_kbps=${RC_KBPS:-}
rc_fps=${FPS:-30}
if [ -n "$rc_kbps" ]; then
  cargo build --release -q -p ec-h265 --example hevc_rc --manifest-path "$here/Cargo.toml"
else
  cargo build --release -q -p ec-h265 --example hevc_gop --manifest-path "$here/Cargo.toml"
fi
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
gopbin="$target/release/examples/hevc_gop"
[ -n "$rc_kbps" ] && gopbin="$target/release/examples/hevc_rc"

printf '%-10s %-8s %-10s %-10s %-10s %-s\n' clip threads secs fps bytes identical
for size in $clips; do
  w=${size%x*}
  h=${size#*x}
  src="$data/src-$size-$frames.yuv"
  if [ ! -s "$src" ]; then
    ffmpeg -v error -y -f lavfi -i "testsrc2=size=${size}:rate=30" \
      -vf "scroll=v=0.02,format=yuv420p" -frames:v "$frames" -f rawvideo "$src"
  fi
  ref_md5=""
  for t in $threads_list; do
    out="$data/out-$size-t$t.hevc"
    recon="$data/recon-$size-t$t.yuv"
    best=""
    for _ in $(seq "$runs"); do
      start=$(date +%s.%N)
      if [ -n "$rc_kbps" ]; then
        EC_H265_THREADS="$t" "$gopbin" encode "$src" "$w" "$h" "$gop" "$rc_kbps" "$rc_fps" "$out" "$recon" >/dev/null
      else
        EC_H265_THREADS="$t" "$gopbin" encode "$src" "$w" "$h" "$gop" "$qp" "$out" "$recon" >/dev/null
      fi
      end=$(date +%s.%N)
      secs=$(python3 -I -c "print(f'{$end - $start:.3f}')")
      if [ -z "$best" ] || python3 -I -c "import sys; sys.exit(0 if $secs < $best else 1)"; then
        best=$secs
      fi
    done
    bytes=$(stat -c %s "$out")
    md5=$(md5sum "$out" | cut -d' ' -f1)
    if [ -z "$ref_md5" ]; then ref_md5=$md5; fi
    same=yes
    [ "$md5" = "$ref_md5" ] || same=NO
    [ "$same" = yes ] || { echo "FAIL: $size threads $t bytes differ from the first thread count" >&2; exit 1; }
    fps=$(python3 -I -c "print(f'{$frames / $best:.2f}')")
    printf '%-10s %-8s %-10s %-10s %-10s %-s\n' "$size" "$t" "$best" "$fps" "$bytes" "$same"
  done
done
