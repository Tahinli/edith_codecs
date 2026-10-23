#!/usr/bin/env bash
# hevc-rc.sh: bitrate control of ec-h265's sequence encoder on a real clip.
#
# For each target bitrate: encode the clip with RateControl::Bitrate, decode the
# stream with ec-h265 and with ffmpeg, and check both decodes byte-for-byte against
# the encoder's own reconstruction. Reports the achieved bitrate and the PSNR of the
# reconstruction against the source.
#
# Usage: scripts/hevc-rc.sh <clip> [SECONDS] [KBPS list]
#   clip     any video ffmpeg can read; it is cut to SECONDS and scaled to 1280x720 at 30 fps
#   SECONDS  clip length (default 60)
#   KBPS     targets in kbit/s (default "1000 2500 5000")
# Environment: DATA (default $HOME/.cache/hevcencrc-data), KEYINT (default 30).
#
# Run it where cargo builds: scripts/remote-run.sh hevcencrc -- scripts/hevc-rc.sh <clip>
# Intermediate YUV files are deleted as soon as they are checked: the 60 s 720p source is
# 2.5 GB and each decode is the same again.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
clip=$1
secs=${2:-60}
kbpss=${3:-"1000 2500 5000"}
data=${DATA:-$HOME/.cache/hevcencrc-data}
keyint=${KEYINT:-30}
fps=30
w=1280
h=720
mkdir -p "$data"

src="$data/src-${w}x${h}-${fps}-${secs}s.yuv"
if [ ! -f "$src" ]; then
  ffmpeg -v error -y -i "$clip" -t "$secs" \
    -vf "scale=${w}:${h}:flags=bicubic,fps=${fps}" -pix_fmt yuv420p -f rawvideo "$src"
fi
frames=$(( $(stat -c %s "$src") / (w * h * 3 / 2) ))

cargo build --release -q -p ec-h265 --example hevc_rc --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
bin="$target/release/examples/hevc_rc"

# PSNR of a raw I420 file against the source, the average over Y, U and V as ffmpeg reports it.
psnr() {
  ffmpeg -v info -f rawvideo -pix_fmt yuv420p -video_size ${w}x${h} -framerate $fps -i "$1" \
    -f rawvideo -pix_fmt yuv420p -video_size ${w}x${h} -framerate $fps -i "$src" \
    -lavfi psnr -f null - 2>&1 | sed -n 's/.*PSNR .* average:\([0-9.inf]*\).*/\1/p' | tail -1
}

status=0
echo "clip=$clip secs=$secs frames=$frames keyint=$keyint size=${w}x${h}@${fps}"
printf '%-8s %-12s %-10s %-10s %-8s %-10s %-12s\n' target_kbps achieved_kbps err_pct psnr_y_avg mean_qp ec_decode ffmpeg_decode
for kbps in $kbpss; do
  stream="$data/rc-${kbps}.hevc"
  rec="$data/rc-${kbps}-recon.yuv"
  dec="$data/rc-${kbps}-ec.yuv"
  ffdec="$data/rc-${kbps}-ff.yuv"
  line=$("$bin" encode "$src" $w $h "$keyint" "$kbps" "$fps" "$stream" "$rec")
  achieved=$(sed -n 's/.*kbps=\([0-9.]*\).*/\1/p' <<<"$line")
  qp=$(sed -n 's/.*mean_qp=\([0-9.]*\).*/\1/p' <<<"$line")
  err=$(python3 -I -c "print(f'{($achieved - $kbps) / $kbps * 100:+.2f}')")

  "$bin" decode "$stream" "$dec" >/dev/null
  if cmp -s "$rec" "$dec"; then ec_ok=match; else ec_ok=DIFFER; status=1; fi
  rm -f "$dec"

  ffmpeg -v error -y -i "$stream" -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$ffdec"
  if cmp -s "$rec" "$ffdec"; then ff_ok=match; else ff_ok=DIFFER; status=1; fi
  rm -f "$ffdec"

  y_psnr=$(psnr "$rec")
  rm -f "$rec"
  printf '%-8s %-12s %-10s %-10s %-8s %-10s %-12s\n' "$kbps" "$achieved" "$err" "$y_psnr" "$qp" "$ec_ok" "$ff_ok"
done
exit $status
