#!/usr/bin/env bash
# gen-bitstream-fixtures.sh — raw VP9/AV1 elementary streams for the syntax crates.
#
# ffmpeg is ORACLE/test-input tooling only; no ec-* crate depends on it at runtime.
# Two sources: (a) the container fixtures from gen-fixtures.sh, remuxed to IVF so a
# header parser sees the elementary stream with no container in the way, and (b)
# small purpose-built clips for the header branches the 4:2:0 8/10-bit set never
# reaches — VP9 superframes with a hidden ALTREF plus show_existing_frame, 4:4:4
# (VP9 profile 1/3, AV1 profile 1), monochrome AV1, multi-tile AV1.
#
# Idempotent: non-empty outputs are skipped, partial outputs are deleted on failure.
#
# Usage: scripts/gen-bitstream-fixtures.sh [-f]     (-f: regenerate everything)
# Env:   EC_FIXTURES=<dir>  (default <repo>/fixtures)

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
OUT=$FIXTURES/bitstreams
FORCE=0
[ "${1:-}" = "-f" ] && FORCE=1

command -v ffmpeg >/dev/null || { echo "gen-bitstream-fixtures: ffmpeg not found" >&2; exit 2; }

mkdir -p "$OUT" || exit 2
TMPLOG=$(mktemp) || exit 2
trap 'rm -f "$TMPLOG"' EXIT

fail=0

# run <output> <ffmpeg args...>
run() {
    local out=$1
    shift
    if [ "$FORCE" = 0 ] && [ -s "$out" ]; then
        echo "skip $(basename "$out")"
        return 0
    fi
    if ffmpeg -nostdin -y -v error "$@" "$out" >"$TMPLOG" 2>&1 && [ -s "$out" ]; then
        echo "ok   $(basename "$out")"
    else
        echo "FAIL $(basename "$out")" >&2
        sed 's/^/     /' "$TMPLOG" >&2
        rm -f "$out"
        fail=1
    fi
}

# (a) remux every VP9/AV1 container fixture into IVF, keeping the codec bytes.
for src in "$FIXTURES"/video/*vp9*.mp4 "$FIXTURES"/video/*av1*.mp4; do
    [ -e "$src" ] || continue
    base=$(basename "$src")
    run "$OUT/${base%.*}.ivf" -i "$src" -map 0:v:0 -c:v copy -f ivf
done

# (a2) H.264/HEVC elementary streams in Annex B form, for the hardware decoder:
# a stateless decoder is fed NAL units, and the mp4 fixtures carry them
# length-prefixed. `-bsf` puts the start codes and the in-band parameter sets back.
for src in "$FIXTURES"/video/*h264*.mp4; do
    [ -e "$src" ] || continue
    base=$(basename "$src")
    run "$OUT/${base%.*}.264" -i "$src" -map 0:v:0 -c:v copy -bsf:v h264_mp4toannexb -f h264
done
for src in "$FIXTURES"/video/*hevc*.mp4; do
    [ -e "$src" ] || continue
    base=$(basename "$src")
    run "$OUT/${base%.*}.265" -i "$src" -map 0:v:0 -c:v copy -bsf:v hevc_mp4toannexb -f hevc
done

# (b) branch coverage the container set does not have.
SYN="-f lavfi -i testsrc2=size=320x240:rate=30:duration=2"

# VP9 superframes: a hidden ALTREF is packed with the next frame, and the frame
# that finally shows it is a show_existing_frame header. libvpx only picks
# alternate references in two-pass mode ("2-pass only" in its own option help),
# so a single-pass encode here yields no superframe at all.
if [ "$FORCE" = 1 ] || [ ! -s "$OUT/vp9-superframe-altref.ivf" ]; then
    PASSLOG=$(mktemp -d)
    # shellcheck disable=SC2086
    ffmpeg -nostdin -y -v error $SYN -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 300k \
        -auto-alt-ref 1 -lag-in-frames 25 -cpu-used 5 \
        -pass 1 -passlogfile "$PASSLOG/vp9" -f null - >"$TMPLOG" 2>&1
    # shellcheck disable=SC2086
    run "$OUT/vp9-superframe-altref.ivf" $SYN -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 300k \
        -auto-alt-ref 1 -lag-in-frames 25 -cpu-used 5 \
        -pass 2 -passlogfile "$PASSLOG/vp9" -f ivf
    rm -rf "$PASSLOG"
else
    echo "skip vp9-superframe-altref.ivf"
fi
# VP9 profile 1 (4:4:4 8-bit) and profile 3 (4:4:4 10-bit): the color_config
# branches where subsampling_x/y are coded rather than assumed.
# shellcheck disable=SC2086
run "$OUT/vp9-profile1-444.ivf" $SYN -c:v libvpx-vp9 -pix_fmt yuv444p \
    -cpu-used 5 -b:v 300k -f ivf
# shellcheck disable=SC2086
run "$OUT/vp9-profile3-444-10bit.ivf" $SYN -c:v libvpx-vp9 -pix_fmt yuv444p10le \
    -cpu-used 5 -b:v 300k -f ivf
# VP9 tiles: 4 tile columns needs a frame wide enough for log2_tile_cols > 0.
run "$OUT/vp9-tiles-1280.ivf" -f lavfi -i testsrc2=size=1280x720:rate=30:duration=1 \
    -c:v libvpx-vp9 -pix_fmt yuv420p -tile-columns 2 -cpu-used 5 -b:v 1M -f ivf

# AV1 profile 1 (4:4:4) and monochrome, plus a multi-tile frame.
# shellcheck disable=SC2086
run "$OUT/av1-profile1-444.ivf" $SYN -c:v libaom-av1 -pix_fmt yuv444p \
    -cpu-used 8 -b:v 300k -f ivf
# shellcheck disable=SC2086
run "$OUT/av1-monochrome.ivf" $SYN -c:v libaom-av1 -pix_fmt gray \
    -cpu-used 8 -b:v 300k -f ivf
run "$OUT/av1-tiles-1280.ivf" -f lavfi -i testsrc2=size=1280x720:rate=30:duration=1 \
    -c:v libaom-av1 -pix_fmt yuv420p -tiles 2x2 -cpu-used 8 -b:v 1M -f ivf
# H.264 with B frames and an open GOP: reordering, so the decoder's output order
# is exercised rather than assumed, plus 4 reference frames for the DPB.
# shellcheck disable=SC2086
run "$OUT/h264-bframes-320.264" $SYN -c:v libx264 -pix_fmt yuv420p -bf 3 -refs 4 \
    -g 30 -b:v 500k -f h264
# HEVC with B frames, for the same reason on the reference-picture-set side.
# shellcheck disable=SC2086
run "$OUT/hevc-bframes-320.265" $SYN -c:v libx265 -pix_fmt yuv420p \
    -x265-params "bframes=3:ref=3:keyint=30:log-level=none" -b:v 500k -f hevc

# AV1 with a hidden ALTREF: forward keyframe + show_existing_frame headers.
# shellcheck disable=SC2086
run "$OUT/av1-altref.ivf" $SYN -c:v libaom-av1 -pix_fmt yuv420p \
    -lag-in-frames 25 -cpu-used 6 -b:v 300k -f ivf

exit $fail
