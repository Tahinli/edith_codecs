#!/usr/bin/env bash
# hevc10-roundtrip.sh: Main 10 round trip of ec-h265. Encodes testsrc2 clips as 10-bit
# 4:2:0 (yuv420p10le), decodes the stream with ec-h265 and with ffmpeg, and compares
# each against the encoder's reconstruction byte for byte. Also reports the size, the
# 10-bit PSNR of the reconstruction against the source, and for bitrate mode the rate
# achieved against the target.
#
# Usage: scripts/hevc10-roundtrip.sh   (run it where the cargo build happens:
#        scripts/remote-run.sh h265m10enc -- scripts/hevc10-roundtrip.sh)
#
# Environment: DATA (default $HOME/.cache/hevc10-data), GOP (keyframe interval for the
# GOP rows, default 8), QP (constant QP, default 30), RC (bitrate mode as kbps@fps, e.g.
# 2500@30; when set, the constant-QP rows are skipped), FRAMES (default 64), CLIPS
# (default "416x240 1280x720"), EXTRA (encoder flags, e.g. "sh").
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
data=${DATA:-$HOME/.cache/hevc10-data}
gop=${GOP:-8}
qp=${QP:-30}
rc=${RC:-}
frames=${FRAMES:-64}
clips=${CLIPS:-"416x240 1280x720"}
mkdir -p "$data"

cargo build --release -q -p ec-h265 --example hevc_gop --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
gopbin="$target/release/examples/hevc_gop"

status=0
export EC_H265_BITDEPTH=10
if [ -n "$rc" ]; then modes="rc"; else modes="qp"; fi
printf '%-10s %-6s %-6s %-8s %-9s %-10s %-10s %-10s %-10s\n' clip gop mode bytes kbps psnr_y psnr_yuv ec_decode ffmpeg_decode
for size in $clips; do
  w=${size%x*}
  h=${size#*x}
  src="$data/src10-$size.yuv"
  if [ ! -s "$src" ]; then
    ffmpeg -v error -y -f lavfi -i "testsrc2=size=${size}:rate=30" \
      -vf "scroll=v=0.02,format=yuv420p10le" -frames:v "$frames" -f rawvideo "$src"
  fi
  for keyint in 1 "$gop"; do
    for mode in $modes; do
      tag="$size-k$keyint-$mode"
      stream="$data/$tag.hevc"
      recon="$data/$tag.recon.yuv"
      dec="$data/$tag.ec.yuv"
      ff="$data/$tag.ff.yuv"
      if [ "$mode" = rc ]; then
        EC_H265_RC=$rc "$gopbin" encode "$src" "$w" "$h" "$keyint" "$qp" "$stream" "$recon" ${EXTRA:-} >/dev/null
      else
        "$gopbin" encode "$src" "$w" "$h" "$keyint" "$qp" "$stream" "$recon" ${EXTRA:-} >/dev/null
      fi
      "$gopbin" decode "$stream" "$dec" >/dev/null
      ffmpeg -v error -y -i "$stream" -f rawvideo -pix_fmt yuv420p10le "$ff"
      if cmp -s "$dec" "$recon"; then ec=exact; else ec=MISMATCH; status=1; fi
      if cmp -s "$ff" "$recon"; then fv=exact; else fv=MISMATCH; status=1; fi
      bytes=$(stat -c %s "$stream")
      kbps=$(awk -v b="$bytes" -v f="$frames" 'BEGIN{printf "%.1f", b*8*30/f/1000}')
      metrics=$(python3 -I "$here/scripts/yuv_psnr.py" "$w" "$h" "$src" "$recon" 10)
      py=$(sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p' <<<"$metrics")
      pyuv=$(sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p' <<<"$metrics")
      printf '%-10s %-6s %-6s %-8s %-9s %-10s %-10s %-10s %-10s\n' \
        "$size" "$keyint" "$mode" "$bytes" "$kbps" "$py" "$pyuv" "$ec" "$fv"
    done
  done
done
exit $status
