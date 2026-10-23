#!/usr/bin/env bash
# h264-lane-check.sh: recon-match, rate-control and thread-speed checks for the
# ec-h264 encoder, driven through the h264_clip_encode example.
#
# Usage (from the repo root, normally via scripts/remote-run.sh):
#   scripts/h264-lane-check.sh recon     md5 of ffmpeg's decode vs the encoder's reconstruction
#   scripts/h264-lane-check.sh rate [size:bitrate:kind ...]
#                                        60 s rate-control runs, achieved vs target bitrate
#   scripts/h264-lane-check.sh fps       1080p30 encode fps at 1, 2, 4 and 6 threads
#
# Sources are ffmpeg testsrc2 with temporal noise. No real camera clip exists in
# fixtures/video (every one is generated from testsrc2), so that is the input.
# Work files go to ${H264_WORK:-$TMPDIR/h264-lane-check} and nothing is left in /tmp.
set -euo pipefail

mode=${1:-}
root=$(cd "$(dirname "$0")/.." && pwd)
work=${H264_WORK:-${TMPDIR:-$HOME/tmp}/h264-lane-check}
mkdir -p "$work"
cd "$root"

cargo build --release -q -p ec-h264 --example h264_clip_encode
enc="$root/target/release/examples/h264_clip_encode"
[ -n "${CARGO_TARGET_DIR:-}" ] && enc="$CARGO_TARGET_DIR/release/examples/h264_clip_encode"

# $1 size, $2 rate, $3 duration seconds, $4 source kind (testsrc2 | noisy)
source_graph() {
  local g="testsrc2=size=$1:rate=$2:duration=$3"
  [ "$4" = noisy ] && g="$g,noise=alls=24:allf=t"
  printf '%s' "$g"
}

recon_case() {
  local size=$1 kind=$2 threads=$3 qp=$4 bitrate=$5 tag=$6
  local prefix="$work/$tag"
  ffmpeg -nostdin -v error -f lavfi -i "$(source_graph "$size" 30 1 "$kind")" \
    -pix_fmt yuv420p -f rawvideo - \
    | "$enc" - "${size%x*}" "${size#*x}" 30 "$bitrate" "$qp" "$threads" "$prefix" >/dev/null
  ffmpeg -nostdin -v error -i "$prefix.h264" -f rawvideo -pix_fmt yuv420p "$prefix.dec.yuv"
  local a b
  a=$(md5sum < "$prefix.recon.yuv" | cut -d' ' -f1)
  b=$(md5sum < "$prefix.dec.yuv" | cut -d' ' -f1)
  local verdict=MATCH
  [ "$a" = "$b" ] || verdict=MISMATCH
  printf '%-10s %-8s t=%d qp=%-3s br=%-8s recon=%s decode=%s %s\n' \
    "$size" "$kind" "$threads" "$qp" "$bitrate" "$a" "$b" "$verdict"
  rm -f "$prefix.h264" "$prefix.recon.yuv" "$prefix.dec.yuv"
  [ "$verdict" = MATCH ]
}

rate_case() {
  # $1 size, $2 bitrate, $3 kind; a 60 s clip piped straight into the encoder.
  local size=$1 target=$2 kind=$3 threads=${RATE_THREADS:-4}
  local fps=30
  local out
  out=$(ffmpeg -nostdin -v error -f lavfi -i "$(source_graph "$size" "$fps" 60 "$kind")" \
      -pix_fmt yuv420p -f rawvideo - \
    | "$enc" - "${size%x*}" "${size#*x}" "$fps" "$target" 26 "$threads" none)
  local bytes frames
  bytes=$(echo "$out" | sed -n 's/.*bytes \([0-9]*\).*/\1/p')
  frames=$(echo "$out" | sed -n 's/^frames \([0-9]*\).*/\1/p')
  awk -v s="$size" -v k="$kind" -v t="$target" -v b="$bytes" -v f="$frames" -v r="$fps" '
    BEGIN { a = b * 8 / (f / r); d = (a / t - 1) * 100;
      printf "%-10s %-8s target=%9d achieved=%11.0f b/s error=%+6.2f%% %s\n",
        s, k, t, a, d, (d >= -5 && d <= 5) ? "PASS" : "FAIL" }'
}

fps_case() {
  # $1 threads: 10 s of 1080p30 noisy testsrc2 at 4 Mb/s, timed inside the encoder.
  local threads=$1
  ffmpeg -nostdin -v error -f lavfi -i "$(source_graph 1920x1080 30 10 noisy)" \
      -pix_fmt yuv420p -f rawvideo - \
    | "$enc" - 1920 1080 30 4000000 26 "$threads" none
}

case "$mode" in
  recon)
    # Even sizes only: the encoder refuses odd dimensions. Covers sizes that are
    # not macroblock multiples, odd macroblock counts, and 1080p.
    fail=0
    for size in 176x144 64x48 350x242 210x118 352x288 1280x720 1920x1080; do
      for kind in testsrc2 noisy; do
        recon_case "$size" "$kind" 4 30 0 "rc-$size-$kind-cqp" || fail=1
        recon_case "$size" "$kind" 4 0 2000000 "rc-$size-$kind-abr" || fail=1
      done
    done
    exit $fail
    ;;
  rate)
    # Cases are size:bitrate:kind arguments, or all six noisy cases by default.
    cases=("${@:2}")
    [ ${#cases[@]} -gt 0 ] || cases=(1280x720:1000000:noisy 1280x720:2000000:noisy
      1280x720:4000000:noisy 1920x1080:1000000:noisy 1920x1080:4000000:noisy
      1920x1080:8000000:noisy)
    for c in "${cases[@]}"; do
      IFS=: read -r size target kind <<<"$c"
      rate_case "$size" "$target" "$kind"
    done
    ;;
  fps)
    for t in 1 2 4 6; do
      printf 'threads=%d ' "$t"
      fps_case "$t"
    done
    ;;
  *)
    sed -n '2,12p' "$0" >&2
    exit 2
    ;;
esac
