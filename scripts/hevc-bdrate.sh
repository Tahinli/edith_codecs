#!/usr/bin/env bash
# hevc-bdrate.sh: BD-rate of ec-h265's IDR+P sequence encoder against libx265 at the
# same QPs, with x265 at bframes=0 and the same keyframe interval.
#
# Each side is encoded at QPS (default "22 27 32 37") on one clip; the points are
# (bytes, Y and YUV PSNR of the decoded/reconstructed frames against the source,
# from ffmpeg's psnr filter, see ffmpeg-psnr.sh). The
# anchor is libx265 (-preset veryfast, -qp, -x265-params bframes=0:keyint=GOP), the
# test is ec-h265. Positive bd_rate_pct means ec-h265 needs more bytes.
#
# The anchor is run with `-tune psnr` (X265_TUNE, default psnr; set X265_TUNE= to run
# x265 at its default psy-rd/aq tuning). psy-rd and aq trade PSNR for perceptual detail, and
# ec-h265 has neither, so the PSNR-only tune is the matched anchor for a BD-PSNR comparison.
#
# Each point file row: bytes, psnr_y, psnr_yuv, psnr_u, psnr_v, kbps (30 fps, so kbps is
# bytes*8 divided by the clip duration in seconds).
#
# Usage: scripts/hevc-bdrate.sh <clip.yuv> <width> <height> [GOP] [QPS]
# Environment: DATA (default $HOME/.cache/hevcencp-data), X265_TUNE (default psnr).
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
clip=$1
w=$2
h=$3
gop=${4:-8}
qps=${5:-"22 27 32 37"}
data=${DATA:-$HOME/.cache/hevcencp-data}
mkdir -p "$data"
[ -f "$clip" ] || clip="$data/$clip"
tag=$(basename "$clip" .yuv)-${w}x${h}-k$gop
# PIX=yuv420p10le runs the Main 10 comparison: 16-bit samples, EC_H265_BITDEPTH=10.
pix=${PIX:-yuv420p}
bps=1
[ "$pix" = yuv420p10le ] && bps=2
export EC_H265_BITDEPTH=$([ $bps = 2 ] && echo 10 || echo 8)
frames=$(( $(stat -c %s "$clip") / (w * h * 3 / 2 * bps) ))

cargo build --release -q -p ec-h265 --example hevc_gop --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
gopbin="$target/release/examples/hevc_gop"

anchor="$data/$tag.x265.tsv"
test_="$data/$tag.ec.tsv"
: > "$anchor"
: > "$test_"
for qp in $qps; do
  x265="$data/$tag-qp$qp.x265.hevc"
  tune=()
  [ -n "${X265_TUNE-psnr}" ] && tune=(-tune "${X265_TUNE-psnr}")
  profile=()
  [ "$bps" = 2 ] && profile=(-profile:v main10)
  ffmpeg -v error -y -f rawvideo -pix_fmt "$pix" -s "${w}x${h}" -r 30 -i "$clip" \
    -c:v libx265 -preset veryfast -qp "$qp" "${tune[@]}" "${profile[@]}" \
    -x265-params "bframes=0:keyint=$gop:min-keyint=$gop:scenecut=0:log-level=error" \
    -f hevc "$x265"
  xdec="$data/$tag-qp$qp.x265.yuv"
  ffmpeg -v error -y -i "$x265" -f rawvideo -pix_fmt "$pix" "$xdec"
  xline=$(PIX=$pix "$here/scripts/ffmpeg-psnr.sh" "$clip" "$xdec" "$w" "$h")
  xpy=$(echo "$xline" | sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p')
  xpsnr=$(echo "$xline" | sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p')
  xpu=$(echo "$xline" | sed -n 's/.*psnr_u=\([^ ]*\).*/\1/p')
  xpv=$(echo "$xline" | sed -n 's/.*psnr_v=\([^ ]*\).*/\1/p')
  xbytes=$(stat -c %s "$x265")
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$xbytes" "$xpy" "$xpsnr" "$xpu" "$xpv" "$(awk -v b="$xbytes" -v f="$frames" 'BEGIN{printf "%.2f", b*8*30/f/1000}')" >> "$anchor"

  ours="$data/$tag-qp$qp.ec.hevc"
  recon="$data/$tag-qp$qp.ec.recon.yuv"
  "$gopbin" encode "$clip" "$w" "$h" "$gop" "$qp" "$ours" "$recon" ${EXTRA:-} >/dev/null
  oline=$(PIX=$pix "$here/scripts/ffmpeg-psnr.sh" "$clip" "$recon" "$w" "$h")
  opy=$(echo "$oline" | sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p')
  opsnr=$(echo "$oline" | sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p')
  opu=$(echo "$oline" | sed -n 's/.*psnr_u=\([^ ]*\).*/\1/p')
  opv=$(echo "$oline" | sed -n 's/.*psnr_v=\([^ ]*\).*/\1/p')
  obytes=$(stat -c %s "$ours")
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$obytes" "$opy" "$opsnr" "$opu" "$opv" "$(awk -v b="$obytes" -v f="$frames" 'BEGIN{printf "%.2f", b*8*30/f/1000}')" >> "$test_"
  echo "qp=$qp x265: bytes=$(stat -c %s "$x265") psnr_y=$xpy psnr_yuv=$xpsnr | ec-h265: bytes=$(stat -c %s "$ours") psnr_y=$opy psnr_yuv=$opsnr" >&2
done
echo "clip=$(basename "$clip") size=${w}x${h} frames=$frames gop=$gop qps=[$qps]"
echo "points (bytes, psnr_y, psnr_yuv):"
echo "  anchor x265:"; sed 's/^/    /' "$anchor"
echo "  test ec-h265:"; sed 's/^/    /' "$test_"
echo "metric=yuv (6:1:1):"
python3 -I "$here/scripts/bdrate.py" --metric yuv "$anchor" "$test_"
echo "metric=y:"
python3 -I "$here/scripts/bdrate.py" --metric y "$anchor" "$test_"
