#!/usr/bin/env bash
# bench-benchhw.sh: the VA-API rows of lanes/bench.md ("lane benchhw", DONE.md gate G6).
#
# Local machine only, with the GPU (radeonsi VA-API; `vainfo` lists the profiles). Clips are
# synthetic, generated here with the local ffmpeg (testsrc2 + noise, 1080p30, 60 s), and live in
# $BENCH_CLIPS (default ~/.cache/benchhw-clips), outside the repo.
#
# Rows:
#   decode  ec-hw Decoder, H.264 High, HEVC Main, HEVC Main10, VP9 profile 0, AV1 8-bit.
#           Each row: best of 5 timed runs (GPU waited per picture), then one readback pass whose
#           frames are compared md5-per-frame with ffmpeg's software decode of the same file.
#   encode  ec-hw Encoder, H.264 High and HEVC Main, 1080p30 CBR 8 Mb/s, GOP 30, 1800 pictures.
#           Best of 5; the stream is decoded by ffmpeg (exit status, frame count, PSNR vs source).
#
# Output: one "ROW|..." line per row, then "CHECK|..." lines with the comparison details.
# The lanes/bench.md "lane benchhw" table is written from these lines.
#
# usage: scripts/bench-benchhw.sh [--no-clips] [--no-build]
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
CLIPS="${BENCH_CLIPS:-$HOME/.cache/benchhw-clips}"
BIN="${BENCH_BIN:-$HOME/.cache/benchhw-bin}"
OUT="$CLIPS/out"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/tgt-benchhw}"
export RUSTC_WRAPPER=
export TMPDIR="${TMPDIR:-$HOME/.cache/tmp-benchhw}"
FF=(ffmpeg -hide_banner -loglevel error -y)
DO_CLIPS=1
DO_BUILD=1
for a in "$@"; do
  case "$a" in
    --no-clips) DO_CLIPS=0 ;;
    --no-build) DO_BUILD=0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done
mkdir -p "$CLIPS" "$BIN" "$OUT" "$TMPDIR"
COMMIT="$(git -C "$REPO" rev-parse --short HEAD)"
echo "# host=local cores=$(nproc) commit=$COMMIT ffmpeg=$(ffmpeg -version | head -1 | awk '{print $3}') gpu=$(vainfo 2>/dev/null | sed -n 's/.*Driver version: //p' | head -1)"

SRC="testsrc2=size=1920x1080:rate=30:duration=60"
W=1920
H=1080

# ---- 1. synthetic clips -------------------------------------------------------------------
if [ "$DO_CLIPS" = 1 ]; then
  cd "$CLIPS"
  # Raw 8-bit 4:2:0 source for the encode rows (1800 pictures, about 5.6 GB).
  [ -f src_1080p30_8bit.yuv ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p,noise=alls=10:allf=t" \
      -f rawvideo -pix_fmt yuv420p src_1080p30_8bit.yuv
  # Decode inputs. Short GOP (30 pictures = 1 s). Annex B for H.264/HEVC, IVF for VP9/AV1.
  [ -f h264_high_1080p30.264 ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p,noise=alls=10:allf=t" \
      -c:v libx264 -preset veryfast -profile:v high -crf 23 -g 30 -keyint_min 30 \
      -f h264 h264_high_1080p30.264
  [ -f hevc_main_1080p30.265 ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p,noise=alls=10:allf=t" \
      -c:v libx265 -preset veryfast -crf 23 -x265-params "keyint=30:min-keyint=30:log-level=error" \
      -profile:v main -f hevc hevc_main_1080p30.265
  [ -f hevc_main10_1080p30.265 ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p10le,noise=alls=20:allf=t" \
      -c:v libx265 -preset veryfast -crf 23 -x265-params "keyint=30:min-keyint=30:log-level=error" \
      -profile:v main10 -pix_fmt yuv420p10le -f hevc hevc_main10_1080p30.265
  [ -f vp9_profile0_1080p30.ivf ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p,noise=alls=10:allf=t" \
      -c:v libvpx-vp9 -deadline realtime -cpu-used 5 -row-mt 1 -b:v 0 -crf 31 -g 30 \
      -pix_fmt yuv420p -f ivf vp9_profile0_1080p30.ivf
  [ -f av1_8bit_1080p30.ivf ] || "${FF[@]}" -f lavfi -i "$SRC,format=yuv420p,noise=alls=10:allf=t" \
      -c:v libsvtav1 -preset 8 -crf 35 -g 30 -pix_fmt yuv420p -f ivf av1_8bit_1080p30.ivf
  # References: ffmpeg's software decoders, native pixel format, every picture.
  [ -f ref_h264.yuv ] || "${FF[@]}" -i h264_high_1080p30.264 -map 0:v:0 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p ref_h264.yuv
  [ -f ref_hevc.yuv ] || "${FF[@]}" -i hevc_main_1080p30.265 -map 0:v:0 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p ref_hevc.yuv
  [ -f ref_hevc10.yuv ] || "${FF[@]}" -i hevc_main10_1080p30.265 -map 0:v:0 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p10le ref_hevc10.yuv
  [ -f ref_vp9.yuv ] || "${FF[@]}" -i vp9_profile0_1080p30.ivf -map 0:v:0 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p ref_vp9.yuv
  [ -f ref_av1.yuv ] || "${FF[@]}" -i av1_8bit_1080p30.ivf -map 0:v:0 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p ref_av1.yuv
  cd "$REPO"
fi

# ---- 2. build the bench example -----------------------------------------------------------
if [ "$DO_BUILD" = 1 ]; then
  cargo build --release -j6 -p ec-hw --example bench
  cp "$CARGO_TARGET_DIR/release/examples/bench" "$BIN/ec-hw-bench"
fi
B="$BIN/ec-hw-bench"

# ---- 3. helpers --------------------------------------------------------------------------
wait_load() {
  # Start a row only when the 1-minute load is below 3.
  while :; do
    l=$(cut -d' ' -f1 /proc/loadavg)
    if awk -v l="$l" 'BEGIN{exit !(l < 3)}'; then echo "$l"; return; fi
    sleep 5
  done
}
field() { echo "$1" | tr ' ' '\n' | sed -n "s/^$2=//p"; }
# framecmp REF TEST FRAME_BYTES: per-frame md5 comparison; prints frames, equal count, first diff.
framecmp() {
  python3 -I - "$1" "$2" "$3" <<'PY'
import hashlib, sys
ref, test, fb = sys.argv[1], sys.argv[2], int(sys.argv[3])
def md5s(p):
    out = []
    with open(p, 'rb') as f:
        while True:
            b = f.read(fb)
            if not b:
                break
            out.append(hashlib.md5(b).hexdigest()[:12])
    return out
r, t = md5s(ref), md5s(test)
same = sum(1 for a, b in zip(r, t) if a == b)
first = next((i for i, (a, b) in enumerate(zip(r, t)) if a != b), None)
print(f"frames_ref={len(r)} frames_test={len(t)} frames_md5_equal={same} first_diff={first if first is not None else 'none'}")
PY
}

# ---- 4. decode rows ----------------------------------------------------------------------
cd "$CLIPS"
dec_row() {  # label codec stream ref frame_bytes
  local label=$1 codec=$2 stream=$3 ref=$4 fb=$5
  local l res best fps x pics
  l=$(wait_load)
  res=$("$B" dec "$codec" "$stream" --out "$OUT/dump-$label.yuv" --runs 5)
  best=$(echo "$res" | sed -n 's/^BEST|.*best_s=\([^ ]*\).*/\1/p')
  fps=$(echo "$res" | sed -n 's/^BEST|.*fps=\([^ ]*\).*/\1/p')
  x=$(echo "$res" | sed -n 's/^BEST|.*x_realtime=\([^ ]*\).*/\1/p')
  pics=$(echo "$res" | sed -n 's/^BEST|.*pictures=\([^ ]*\).*/\1/p')
  echo "ROW|ec-hw|decode $label|VA-API $label 1080p30 60 s, $pics pictures|best ${best} s|${fps} fps, ${x}x realtime|load1=$l runs=5 best"
  echo "CHECK|ec-hw decode $label|$(framecmp "$ref" "$OUT/dump-$label.yuv" "$fb")"
  rm -f "$OUT/dump-$label.yuv"
}
FB8=$((W * H * 3 / 2))
FB10=$((W * H * 3))
dec_row h264-high h264 h264_high_1080p30.264 ref_h264.yuv "$FB8"
dec_row hevc-main hevc hevc_main_1080p30.265 ref_hevc.yuv "$FB8"
dec_row hevc-main10 hevc hevc_main10_1080p30.265 ref_hevc10.yuv "$FB10"
dec_row vp9-profile0 vp9 vp9_profile0_1080p30.ivf ref_vp9.yuv "$FB8"
dec_row av1-8bit av1 av1_8bit_1080p30.ivf ref_av1.yuv "$FB8"

# ---- 5. encode rows ----------------------------------------------------------------------
enc_row() {  # label codec ffmt
  local label=$1 codec=$2 ffmt=$3
  local l res best fps x kbps
  l=$(wait_load)
  res=$("$B" enc "$codec" src_1080p30_8bit.yuv "$W" "$H" 1800 8000000 --out "$OUT/enc-$label.bin" --runs 5)
  best=$(echo "$res" | sed -n 's/^BEST|.*best_s=\([^ ]*\).*/\1/p')
  fps=$(echo "$res" | sed -n 's/^BEST|.*fps=\([^ ]*\).*/\1/p')
  x=$(echo "$res" | sed -n 's/^BEST|.*x_realtime=\([^ ]*\).*/\1/p')
  kbps=$(echo "$res" | sed -n 's/^BEST|.*kbps=\([^ ]*\).*/\1/p')
  echo "ROW|ec-hw|encode $label|VA-API $label 1080p30 CBR 8 Mb/s GOP 30, 60 s|best ${best} s|${fps} fps, ${x}x realtime, ${kbps} kb/s|load1=$l runs=5 best"
  # ffmpeg decodes our stream: exit status, picture count, and PSNR against the source.
  local ff_out nb psnr
  ff_out=$("${FF[@]}" -f "$ffmt" -i "$OUT/enc-$label.bin" -f rawvideo -pix_fmt yuv420p "$OUT/enc-$label.ref.yuv" 2>&1 && echo ok || echo FAIL)
  nb=$(stat -c%s "$OUT/enc-$label.ref.yuv")
  nb=$((nb / FB8))
  psnr=$(ffmpeg -hide_banner -nostats -f "$ffmt" -i "$OUT/enc-$label.bin" -f rawvideo -s ${W}x${H} -pix_fmt yuv420p -i src_1080p30_8bit.yuv -lavfi "[0:v][1:v]psnr" -f null - 2>&1 | sed -n 's/.*average:\([0-9.inf]*\).*/\1/p' | tail -1)
  echo "CHECK|ec-hw encode $label|ffmpeg decode: $ff_out, pictures=$nb of 1800, PSNR vs source avg ${psnr} dB"
  rm -f "$OUT/enc-$label.ref.yuv"
}
enc_row h264-high h264 h264
enc_row hevc-main hevc hevc

# Calibration for the PSNR above: ffmpeg's libx264 and libx265 at the same 8 Mb/s, GOP 30, the
# same source, decoded and measured the same way. Not a bench row; it says how much of the PSNR
# is the bitrate and the noisy source, not the hardware encoder.
for spec in "h264|libx264|h264" "hevc|libx265|hevc"; do
  IFS='|' read -r lbl enc ffmt <<<"$spec"
  "${FF[@]}" -f rawvideo -pix_fmt yuv420p -s ${W}x${H} -r 30 -i src_1080p30_8bit.yuv -c:v "$enc" \
    -preset veryfast -b:v 8M -maxrate 8M -bufsize 1M -g 30 -f "$ffmt" "$OUT/cal-$lbl.bin"
  psnr=$(ffmpeg -hide_banner -nostats -f "$ffmt" -i "$OUT/cal-$lbl.bin" -f rawvideo -s ${W}x${H} -pix_fmt yuv420p -i src_1080p30_8bit.yuv -lavfi "[0:v][1:v]psnr" -f null - 2>&1 | sed -n 's/.*average:\([0-9.inf]*\).*/\1/p' | tail -1)
  echo "CHECK|calibration $enc 8 Mb/s GOP 30|PSNR vs source avg ${psnr} dB (same source, ffmpeg software encoder)"
done

cd "$REPO"
echo "# done"
