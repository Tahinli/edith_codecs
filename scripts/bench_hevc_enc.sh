#!/usr/bin/env bash
# bench_hevc_enc.sh: 1080p30 synthetic encode bench for ec-h265 (8 and 10 bit), prints fps and sha1.
# Run on a server via scripts/remote-run.sh. Env: N (frames, 30), RUNS (3), THREADS (4).
set -e
N=${N:-30}; RUNS=${RUNS:-3}; T=${THREADS:-4}
D=$HOME/.cache/hevc10simd2-bench; mkdir -p "$D"
[ -s $D/c8.yuv ] || ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=1,scroll=v=0.02" -frames:v $N -pix_fmt yuv420p -f rawvideo $D/c8.yuv
[ -s $D/c10.yuv ] || ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=1,scroll=v=0.02" -frames:v $N -pix_fmt yuv420p10le -f rawvideo $D/c10.yuv
cargo build --release -p ec-h265 --example hevc_gop 2>&1 | tail -1
BIN=${CARGO_TARGET_DIR:-target}/release/examples/hevc_gop
echo "load: $(cat /proc/loadavg)"
for bd in 8 10; do
  for r in $(seq $RUNS); do
    s=$(date +%s.%N)
    EC_H265_BITDEPTH=$bd EC_H265_THREADS=$T EC_H265_TIMING=1 $BIN encode $D/c$bd.yuv 1920 1080 8 30 $D/o$bd.hevc $D/r$bd.yuv >/dev/null 2>$D/t.log
    e=$(date +%s.%N)
    echo "bd=$bd run=$r enc=$(cut -d" " -f2 $D/t.log) fps=$(awk "BEGIN{printf \"%.2f\", $N/($e-$s)}") sha=$(sha1sum $D/o$bd.hevc | cut -c1-12) rec=$(sha1sum $D/r$bd.yuv | cut -c1-12)"
  done
done
echo "load after: $(cat /proc/loadavg)"
