#!/usr/bin/env bash
# hevc-gop-roundtrip.sh: encode lavfi clips with ec-h265's IDR+P encoder, decode the
# stream with ec-h265 and with ffmpeg, and compare each against the encoder's
# reconstruction. Also reports the size and PSNR of the GOP against an intra-only
# stream at the same QP.
#
# Usage: scripts/hevc-gop-roundtrip.sh   (run it where the cargo build happens;
#        scripts/remote-run.sh hevcencp -- scripts/hevc-gop-roundtrip.sh)
#
# Environment: DATA (default $HOME/.cache/hevcencp-data), GOP (keyframe interval,
# default 8), QP (default 30), FRAMES (default 30), CLIPS (default "352x288 1280x720 350x286"),
# EXTRA (encoder flags, e.g. "sh" for sign data hiding).
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
data=${DATA:-$HOME/.cache/hevcencp-data}
gop=${GOP:-8}
qp=${QP:-30}
frames=${FRAMES:-30}
clips=${CLIPS:-"352x288 1280x720 350x286"}
mkdir -p "$data"

cargo build --release -q -p ec-h265 --example hevc_gop --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
gopbin="$target/release/examples/hevc_gop"

status=0
printf '%-10s %-6s %-8s %-7s %-12s %-12s %-18s %-18s\n' clip gop bytes ratio psnr_y psnr_yuv ec_h265_decode ffmpeg_decode
for size in $clips; do
  w=${size%x*}
  h=${size#*x}
  src="$data/src-$size.yuv"
  if [ ! -s "$src" ]; then
    # testsrc2 with a slow vertical scroll: real motion, not a static chart.
    ffmpeg -v error -y -f lavfi -i "testsrc2=size=${size}:rate=30" \
      -vf "scroll=v=0.02,format=yuv420p" -frames:v "$frames" -f rawvideo "$src"
  fi
  for keyint in 1 "$gop"; do
    tag="$size-k$keyint"
    stream="$data/$tag.hevc"
    recon="$data/$tag.recon.yuv"
    dec="$data/$tag.ec.yuv"
    ff="$data/$tag.ff.yuv"
    "$gopbin" encode "$src" "$w" "$h" "$keyint" "$qp" "$stream" "$recon" ${EXTRA:-} >/dev/null
    "$gopbin" decode "$stream" "$dec" >/dev/null
    ffmpeg -v error -y -i "$stream" -f rawvideo -pix_fmt yuv420p "$ff"
    if cmp -s "$dec" "$recon"; then ec=exact; else ec=MISMATCH; status=1; fi
    if cmp -s "$ff" "$recon"; then fv=exact; else fv=MISMATCH; status=1; fi
    bytes=$(stat -c %s "$stream")
    if [ "$keyint" = 1 ]; then intra_bytes=$bytes; fi
    ratio=$(python3 -I -c "print(f'{$bytes/$intra_bytes:.3f}')")
    metrics=$(python3 -I "$here/scripts/yuv_psnr.py" "$w" "$h" "$src" "$recon")
    py=$(sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p' <<<"$metrics")
    pyuv=$(sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p' <<<"$metrics")
    printf '%-10s %-6s %-8s %-7s %-12s %-12s %-18s %-18s\n' \
      "$size" "$keyint" "$bytes" "$ratio" "$py" "$pyuv" "$ec" "$fv"
  done
done
exit $status
