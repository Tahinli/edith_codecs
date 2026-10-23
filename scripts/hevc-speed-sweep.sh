#!/usr/bin/env bash
# hevc-speed-sweep.sh: speed and rate-distortion of ec-h265 sequence encodes under env overrides.
# Usage: scripts/hevc-speed-sweep.sh <label> [VAR=val ...]
# Encodes 3 synthetic 720p 10-bit clips at QP 22/27/32/37 (keyint 8, 4 threads), writes
# $D/<label>.<clip>.tsv (bytes, psnr_y, psnr_yuv) and prints fps. Compare labels with bdrate.py.
# Env: BITDEPTH (10), CLIPS (a b c), N (32).
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
label=$1; shift
D=${D:-$HOME/.cache/hevcfast-data2}; mkdir -p "$D"
N=${N:-32}; BD=${BITDEPTH:-10}; W=1280; H=720
pix=yuv420p; [ $BD = 10 ] && pix=yuv420p10le
src_a="testsrc2=size=${W}x${H}:rate=30,scroll=h=0.003:v=0.004,noise=alls=4:allf=t"
src_b="mandelbrot=size=${W}x${H}:rate=30"
src_c="gradients=size=${W}x${H}:rate=30:speed=0.02:n=4,noise=alls=5:allf=t"
for c in ${CLIPS:-a b c}; do
  f=$D/$c$BD.yuv
  [ -s $f ] || { eval s=\$src_$c; ffmpeg -loglevel error -y -f lavfi -i "$s" -frames:v $N -pix_fmt $pix -f rawvideo $f; }
done
(cd "$here" && cargo build --release -q -p ec-h265 --example hevc_gop)
BIN=${CARGO_TARGET_DIR:-$here/target}/release/examples/hevc_gop
echo "host=$(hostname) load=$(cut -d' ' -f1-3 /proc/loadavg)"
for c in ${CLIPS:-a b c}; do
  f=$D/$c$BD.yuv; out=$D/$label.$c.tsv; : > $out; tt=0; tc=0
  for qp in 22 27 32 37; do
    TIMEFORMAT='%U %S'; { time env "$@" EC_H265_BITDEPTH=$BD EC_H265_THREADS=4 EC_H265_TIMING=1 $BIN encode $f $W $H 8 $qp $D/o.hevc $D/r.yuv >/dev/null 2>$D/t.log; } 2>$D/cpu.log
    cpu=$(awk '{print $1+$2}' $D/cpu.log); tc=$(awk "BEGIN{print ${tc:-0}+$cpu}")
    t=$(cut -d' ' -f2 $D/t.log | tr -d s); tt=$(awk "BEGIN{print $tt+$t}")
    l=$(PIX=$pix $here/scripts/ffmpeg-psnr.sh $f $D/r.yuv $W $H)
    py=$(echo "$l" | sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p'); pyuv=$(echo "$l" | sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p')
    printf '%s\t%s\t%s\n' $(stat -c %s $D/o.hevc) $py $pyuv >> $out
  done
  fps=$(awk "BEGIN{printf \"%.2f\", 4*$N/$tt}")
  bd=""; [ "$label" != default ] && [ -s $D/default.$c.tsv ] && bd=$(python3 -I $here/scripts/bdrate.py --metric yuv $D/default.$c.tsv $out | tr '\n' ' ')
  echo "$label clip=$c fps=$fps cpu_ms_per_frame=$(awk "BEGIN{printf \"%.0f\", 1000*$tc/(4*$N)}") $bd"
done
