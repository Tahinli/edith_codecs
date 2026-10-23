#!/usr/bin/env bash
# hevc-rc-bdrate.sh: BD-rate of ec-h265's bitrate-controlled sequence encoder against libx265
# at the same bitrates, with x265 at bframes=0 and the same keyframe interval.
#
# The bitrate counterpart of hevc-bdrate.sh (which sweeps constant QP). Each side is encoded at
# the KBPS targets (ec-h265 through the hevc_rc example, x265 with -b:v at veryfast); the points
# are (bytes, Y and YUV PSNR against the source, from scripts/ffmpeg-psnr.sh). Positive
# bd_rate_pct means ec-h265 needs more bytes at the same quality.
#
# Usage: scripts/hevc-rc-bdrate.sh <clip.yuv> <width> <height> [GOP] [KBPS list]
# Environment: DATA (default $HOME/.cache/hevcencrc-data), FPS (default 30).
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
clip=$1
w=$2
h=$3
gop=${4:-30}
kbpss=${5:-"1000 2000 4000 8000"}
fps=${FPS:-30}
data=${DATA:-$HOME/.cache/hevcencrc-data}
mkdir -p "$data"
[ -f "$clip" ] || clip="$data/$clip"
tag=$(basename "$clip" .yuv)-${w}x${h}-k$gop-rc

cargo build --release -q -p ec-h265 --example hevc_rc --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
rcbin="$target/release/examples/hevc_rc"

anchor="$data/$tag.x265.tsv"
test_="$data/$tag.ec.tsv"
: > "$anchor"
: > "$test_"
for kbps in $kbpss; do
  x265="$data/$tag-$kbps.x265.hevc"
  ffmpeg -v error -y -f rawvideo -pix_fmt yuv420p -s "${w}x${h}" -r "$fps" -i "$clip" \
    -c:v libx265 -preset veryfast -b:v "${kbps}k" \
    -x265-params "bframes=0:keyint=$gop:min-keyint=$gop:scenecut=0:log-level=error" \
    -f hevc "$x265"
  xdec="$data/$tag-$kbps.x265.yuv"
  ffmpeg -v error -y -i "$x265" -f rawvideo -pix_fmt yuv420p "$xdec"
  xline=$("$here/scripts/ffmpeg-psnr.sh" "$clip" "$xdec" "$w" "$h")
  xpy=$(echo "$xline" | sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p')
  xpsnr=$(echo "$xline" | sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p')
  rm -f "$xdec"
  printf '%s\t%s\t%s\n' "$(stat -c %s "$x265")" "$xpy" "$xpsnr" >> "$anchor"

  ours="$data/$tag-$kbps.ec.hevc"
  recon="$data/$tag-$kbps.ec.recon.yuv"
  "$rcbin" encode "$clip" "$w" "$h" "$gop" "$kbps" "$fps" "$ours" "$recon" >/dev/null
  oline=$("$here/scripts/ffmpeg-psnr.sh" "$clip" "$recon" "$w" "$h")
  rm -f "$recon"
  opy=$(echo "$oline" | sed -n 's/.*psnr_y=\([^ ]*\).*/\1/p')
  opsnr=$(echo "$oline" | sed -n 's/.*psnr_yuv=\([^ ]*\).*/\1/p')
  printf '%s\t%s\t%s\n' "$(stat -c %s "$ours")" "$opy" "$opsnr" >> "$test_"
  echo "kbps=$kbps x265: bytes=$(stat -c %s "$x265") psnr_y=$xpy | ec-h265: bytes=$(stat -c %s "$ours") psnr_y=$opy" >&2
done
echo "clip=$(basename "$clip") size=${w}x${h} gop=$gop kbps=[$kbpss]"
echo "points (bytes, psnr_y, psnr_yuv):"
echo "  anchor x265:"; sed 's/^/    /' "$anchor"
echo "  test ec-h265:"; sed 's/^/    /' "$test_"
echo "metric=yuv (6:1:1):"
python3 -I "$here/scripts/bdrate.py" --metric yuv "$anchor" "$test_"
echo "metric=y:"
python3 -I "$here/scripts/bdrate.py" --metric y "$anchor" "$test_"
