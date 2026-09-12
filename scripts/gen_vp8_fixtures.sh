#!/usr/bin/env bash
# Generate the ec-vp8 test fixtures (gitignored, like the rest of the
# workspace's fixtures/). Every vector is libvpx-encoded via ffmpeg; the
# decoder's witnesses compare its raw YUV output against ffmpeg's.
#
# Usage: scripts/gen_vp8_fixtures.sh [worktree-root]
set -euo pipefail
ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
OUT="$ROOT/fixtures/vp8"
mkdir -p "$OUT"

enc() { # name size frames q extra-source-args...
    local name=$1 size=$2 frames=$3 q=$4
    shift 4
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "$@" \
        -frames:v "$frames" -c:v libvpx -qmin "$q" -qmax "$q" -crf "$q" \
        -auto-alt-ref 0 -cpu-used 1 -g "$frames" \
        "$OUT/$name.ivf"
    echo "wrote $OUT/$name.ivf"
}

# Key-frame vectors (M2): several sizes incl. non-multiple-of-16, several q.
enc kf-160x96-q20 160x96  4 20 testsrc2=size=160x96:rate=30
enc kf-160x96-q40 160x96  4 40 testsrc2=size=160x96:rate=30
enc kf-76x52-q30   76x52  4 30 testsrc2=size=76x52:rate=30
enc kf-32x16-q35   32x16  4 35 testsrc2=size=32x16:rate=30
enc kf-172x144-q25 172x144 4 25 testsrc2=size=172x144:rate=30
enc kf-96x80-q63   96x80  4 63 testsrc2=size=96x80:rate=30

# Multi-frame GOP (M3): keyframe + interframes, golden/altref in play.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i testsrc2=size=160x96:rate=30 \
    -frames:v 60 -c:v libvpx -qmin 30 -qmax 45 -crf 38 \
    -auto-alt-ref 0 -g 15 -keyint_min 15 -cpu-used 1 -lag-in-frames 0 \
    "$OUT/gop-160x96.ivf"
echo "wrote $OUT/gop-160x96.ivf"

# Larger short GOP with the simple loop filter via profile (version 1)?
# libvpx picks profile 0 normally; the version field is exercised through
# -profile:v 1 (simple filter, bilinear reconstruction).
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i testsrc2=size=96x80:rate=30 \
    -frames:v 12 -c:v libvpx -qmin 30 -qmax 30 -crf 30 -auto-alt-ref 0 -g 6 \
    -profile:v 1 -cpu-used 1 -lag-in-frames 0 \
    "$OUT/gop-96x80-v1.ivf" 2>/dev/null || \
    echo "SKIP gop-96x80-v1 (profile 1 unsupported by this libvpx build)"

# Multi-partition token partitions (M3): ffmpeg's libvpx wrapper has no
# token-part option, so this one vector comes from a locally built vpxenc
# (checked out at ~/.cache/vp8/libvpx-src; `./configure && make -j`).
VPXENC="$HOME/.cache/vp8/libvpx-src/vpxenc"
if [ -x "$VPXENC" ]; then
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i testsrc2=size=160x96:rate=30 -frames:v 30 \
        -pix_fmt yuv420p -f rawvideo /tmp/ecvp8-mparts.raw
    { printf 'YUV4MPEG2 W160 H96 F30:1 Ip A1:1 C420jpeg\n'
      python3 - <<'PYEOF'
raw = open("/tmp/ecvp8-mparts.raw", "rb").read()
frame = 160 * 96 * 3 // 2
for i in range(len(raw) // frame):
    open("/dev/stdout", "wb").write(b"FRAME\n" + raw[i*frame:(i+1)*frame])
PYEOF
    } | "$VPXENC" --codec=vp8 -w 160 -h 96 --token-parts=3 --limit=30 \
        --ivf --cpu-used=1 --end-usage=q --cq-level=32 --min-q=30 --max-q=45 \
        --kf-max-dist=10 -o "$OUT/mparts-160x96.ivf" - 2>/dev/null
    echo "wrote $OUT/mparts-160x96.ivf"
else
    echo "SKIP mparts (build libvpx at ~/.cache/vp8/libvpx-src for vpxenc)"
fi

# VP8-in-WebP still (same key-frame payload in a RIFF container).
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i testsrc2=size=96x80:rate=30 -frames:v 1 \
    "$OUT/still.png"
if ffmpeg -hide_banner -loglevel error -y -i "$OUT/still.png" -q:v 60 "$OUT/still.webp" 2>/dev/null; then
    echo "wrote $OUT/still.webp"
else
    echo "SKIP still.webp (libwebp support missing)"
    rm -f "$OUT/still.png"
fi

# Note on the three vectors below (review flag): every fixture in the set
# must come from THIS script — no ad-hoc leftovers.
#
# kf-16x16-black-q0: 1-MB all-black frame at q0 (the M2 "no ONE tokens"
# degenerate vector: exercises the zero-coefficient and overread paths).
enc kf-16x16-black-q0 16x16 1 0 color=black:size=16x16:rate=30

# clip-obs-320x192: screen-content-shaped 320x192 clip. The original was
# an OBS window capture; the recorded reproducible stand-in is testsrc2
# at the same size (same decoder paths: 20x12 MB grid, small-MV motion).
enc clip-obs-320x192 320x192 169 28 testsrc2=size=320x192:rate=30

# altref-160x96: vpxenc --auto-alt-ref=1 in TWO passes (one-pass CQ/VBR
# never inserts hidden altrefs for this content) with --lag-in-frames, so
# libvpx emits HIDDEN alternate-reference frames (frame tag show_frame=0,
# currently 6 of 90 at decode indices 1,17,33,49,65,81). The witnesses
# census the tags and refuse a shown-only stream: the hidden-frame
# contract (no output, reference buffers still update, later frames
# reference the hidden altref) must be exercised.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i testsrc2=size=160x96:rate=30 -frames:v 90 \
    -pix_fmt yuv420p -f rawvideo /tmp/ecvp8-altref.raw
# Two passes need a seekable input (stdin cannot rewind for pass 2), so
# the y4m goes through a temp FILE, not a pipe.
{ printf 'YUV4MPEG2 W160 H96 F30:1 Ip A1:1 C420jpeg\n'
  cat /tmp/ecvp8-altref.raw | python3 -c '
import sys
raw = sys.stdin.buffer.read()
frame = 160 * 96 * 3 // 2
for i in range(len(raw) // frame):
    sys.stdout.buffer.write(b"FRAME\n" + raw[i*frame:(i+1)*frame])
' > /tmp/ecvp8-altref.y4m
} 
"$VPXENC" --codec=vp8 -w 160 -h 96 --limit=90 --ivf --cpu-used=1 \
    --end-usage=vbr --target-bitrate=400 --kf-max-dist=15 \
    --auto-alt-ref=1 --lag-in-frames=15 --passes=2 --fpf=/tmp/ecvp8-altref.fpf \
    --quiet -o "$OUT/altref-160x96.ivf" /tmp/ecvp8-altref.y4m
rm -f /tmp/ecvp8-altref.raw /tmp/ecvp8-altref.y4m /tmp/ecvp8-altref.fpf
echo "wrote $OUT/altref-160x96.ivf"

ls -la "$OUT"
