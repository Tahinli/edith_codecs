#!/usr/bin/env bash
# bench-benchrest.sh: the still-image, Matroska, FLAC and zlib rows of lanes/bench.md
# ("lane benchrest", DONE.md gate G6).
#
# Local machine only (DONE.md D7 exception): build with -j6 into CARGO_TARGET_DIR and run each row
# as the best of 5 inside the bench binary. Before each row, wait up to 40 minutes for the 1-minute
# load to be at most 2; the load at the start of the row is printed. Inputs are generated here with
# the local ffmpeg and python (never personal media) into $BENCH_INPUTS, which lives in the tmp dir
# $TMPDIR (default ~/.cache/tmp-benchrest), outside the repo.
#
# Output: one "ROW|..." line per bench row, then "CHECK|..." lines with the self-check. The
# lanes/bench.md "lane benchrest" table is written from these lines.
#
# usage: scripts/bench-benchrest.sh [--no-inputs] [--no-build]
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
export RUSTC_WRAPPER=
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/tgt-benchrest}"
export TMPDIR="${TMPDIR:-$HOME/.cache/tmp-benchrest}"
IN="${BENCH_INPUTS:-$TMPDIR/inputs}"
BIN="$TMPDIR/bin"
OUT="$IN/out"
FF=(ffmpeg -hide_banner -loglevel error -y)
DO_INPUTS=1
DO_BUILD=1
for a in "$@"; do
  case "$a" in
    --no-inputs) DO_INPUTS=0 ;;
    --no-build) DO_BUILD=0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done
mkdir -p "$IN" "$BIN" "$OUT" "$TMPDIR"
COMMIT="$(git -C "$REPO" rev-parse --short HEAD)"
echo "# host=local cores=$(nproc) commit=$COMMIT ffmpeg=$(ffmpeg -version | head -1 | awk '{print $3}') python=$(python3 -c 'import numpy; print("numpy " + numpy.__version__)')"

# ---- 1. inputs ----------------------------------------------------------------------------------
if [ "$DO_INPUTS" = 1 ]; then
  cd "$IN"
  # Still images: one testsrc2 frame at 1920x1080.
  [ -f png_rgb.png ] || "${FF[@]}" -f lavfi -i "testsrc2=size=1920x1080:rate=1" -frames:v 1 -pix_fmt rgb24 png_rgb.png
  [ -f ref_png_rgb.rgb ] || "${FF[@]}" -i png_rgb.png -f rawvideo -pix_fmt rgb24 ref_png_rgb.rgb
  [ -f jpg_420_q2.jpg ] || "${FF[@]}" -f lavfi -i "testsrc2=size=1920x1080:rate=1" -frames:v 1 -c:v mjpeg -q:v 2 -pix_fmt yuvj420p jpg_420_q2.jpg
  [ -f ref_jpg.rgb ] || "${FF[@]}" -i jpg_420_q2.jpg -f rawvideo -pix_fmt rgb24 ref_jpg.rgb
  [ -f src_rgba.raw ] || "${FF[@]}" -f lavfi -i "testsrc2=size=1920x1080:rate=1" -frames:v 1 -pix_fmt rgba -f rawvideo src_rgba.raw
  # Matroska: H.264 1080p30 + AAC 128k stereo, 60 s.
  [ -f h264_aac_1080p30.mkv ] || "${FF[@]}" -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=60" \
      -f lavfi -i "sine=frequency=220:sample_rate=48000:duration=60" -ac 2 \
      -c:v libx264 -preset veryfast -b:v 4M -pix_fmt yuv420p -c:a aac -b:a 128k -f matroska h264_aac_1080p30.mkv
  [ -f ref_mkv.v.bin ] || "${FF[@]}" -i h264_aac_1080p30.mkv -map 0:v:0 -c copy -f data ref_mkv.v.bin
  [ -f ref_mkv.a.bin ] || "${FF[@]}" -i h264_aac_1080p30.mkv -map 0:a:0 -c copy -f data ref_mkv.a.bin
  # FLAC: 60 s stereo 16-bit 44.1 kHz (tones plus pink noise, as benchaudio) and 60 s 5.1 24-bit 48 kHz.
  [ -f st16_44k.flac ] || "${FF[@]}" \
      -f lavfi -i "aevalsrc='0.25*sin(2*PI*220*t)+0.12*sin(2*PI*277.18*t)*(0.5+0.5*sin(2*PI*2*t))|0.25*sin(2*PI*222*t)+0.12*sin(2*PI*280*t)*(0.5+0.5*sin(2*PI*2*t))':s=44100:d=60" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=44100:r=44100:seed=1" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=44100:r=44100:seed=2" \
      -filter_complex "[1:a][2:a]amerge=inputs=2,aformat=channel_layouts=stereo[n];[0:a][n]amix=inputs=2:normalize=0,aformat=channel_layouts=stereo[o]" \
      -map "[o]" -c:a flac -sample_fmt s16 st16_44k.flac
  [ -f ref_st16.pcm ] || "${FF[@]}" -i st16_44k.flac -f s16le ref_st16.pcm
  [ -f m51_48k_24.flac ] || "${FF[@]}" \
      -f lavfi -i "aevalsrc='0.25*sin(2*PI*220*t)|0.25*sin(2*PI*222*t)|0.2*sin(2*PI*110*t)|0.1*sin(2*PI*440*t)|0.15*sin(2*PI*330*t)|0.15*sin(2*PI*150*t)':s=48000:d=60" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=3" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=4" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=5" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=6" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=7" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=8" \
      -filter_complex "[1:a][2:a][3:a][4:a][5:a][6:a]amerge=inputs=6,aformat=channel_layouts=5.1[n];[0:a][n]amix=inputs=2:normalize=0,aformat=channel_layouts=5.1[o]" \
      -map "[o]" -c:a flac -sample_fmt s32 -bits_per_raw_sample 24 m51_48k_24.flac
  [ -f ref_m51.s32 ] || "${FF[@]}" -i m51_48k_24.flac -f s32le ref_m51.s32
  [ -f ref_m51.s24 ] || "${FF[@]}" -i m51_48k_24.flac -f s24le ref_m51.s24
  # zlib: 64 MiB of mixed text and PCM (python zlib level 6), and 20k blocks of 3 KiB (python zlib
  # level 6 each, length-prefixed). The refs are python's md5 of zlib.decompress.
  [ -f big_mixed.zlib ] || python3 -I - "$IN" <<'PY'
import hashlib, random, string, struct, sys, zlib
import numpy as np
d = sys.argv[1]
rng = random.Random(1)
words = [''.join(rng.choices(string.ascii_lowercase, k=rng.randint(2, 9))) for _ in range(4000)]
def text(n):
    return ' '.join(rng.choices(words, k=n // 5)).encode()[:n]
def pcm(n):
    gen = np.random.default_rng(7)
    t = np.arange(n // 4)
    x = 6000 * np.sin(2 * np.pi * 440 * t / 44100) + 2500 * np.sin(2 * np.pi * 659.3 * t / 44100) \
        + gen.normal(0, 300, t.size)
    return np.clip(x, -32768, 32767).astype('<i2').repeat(2).tobytes()[:n]
big = text(32 << 20) + pcm(32 << 20)
comp = zlib.compress(big, 6)
open(f"{d}/big_mixed.zlib", "wb").write(comp)
open(f"{d}/ref_big.md5", "w").write(hashlib.md5(zlib.decompress(comp)).hexdigest() + "\n")
del big
blocks = open(f"{d}/blocks_3k.bin", "wb")
digest = hashlib.md5()
for _ in range(20000):
    c = zlib.compress(text(3072), 6)
    blocks.write(struct.pack('<I', len(c)) + c)
    digest.update(zlib.decompress(c))
blocks.close()
open(f"{d}/ref_blocks.md5", "w").write(digest.hexdigest() + "\n")
PY
  cd "$REPO"
fi

# ---- 2. build the four examples (one binary per crate, copied before the next build) ------------
crates=(ec-image ec-matroska ec-flac ec-inflate)
if [ "$DO_BUILD" = 1 ]; then
  for c in "${crates[@]}"; do
    cargo build --release -j6 -p "$c" --example bench
    cp "$CARGO_TARGET_DIR/release/examples/bench" "$BIN/$c-bench"
  done
fi

# ---- 3. helpers ----------------------------------------------------------------------------------
wait_load() {
  # Start a row when the 1-minute load is at most 2, or after 40 minutes at most (the row then
  # records the load it actually ran at).
  local waited=0 l
  while :; do
    l=$(cut -d' ' -f1 /proc/loadavg)
    if awk -v l="$l" 'BEGIN{exit !(l <= 2)}' || [ "$waited" -ge 2400 ]; then echo "$l"; return; fi
    sleep 5
    waited=$((waited + 5))
  done
}
field() { echo "$1" | tr ' ' '\n' | sed -n "s/^$2=//p"; }
md5_of() { md5sum "$1" | cut -c1-32; }
# psnr CHECK: max abs diff, mean abs diff and PSNR of two 8-bit RGB raw files of the same size.
psnr() {
  python3 -I - "$1" "$2" "$3" "$4" <<'PY'
import sys
import numpy as np
a = np.fromfile(sys.argv[1], np.uint8).astype(np.int32)
b = np.fromfile(sys.argv[2], np.uint8).astype(np.int32)
w, h = int(sys.argv[3]), int(sys.argv[4])
assert a.size == b.size == w * h * 3, (a.size, b.size)
d = np.abs(a - b)
mse = float(np.mean((a - b) ** 2))
psnr = 10 * np.log10(255 * 255 / mse) if mse > 0 else float("inf")
print(f"max_abs_diff={d.max()} mean_abs_diff={d.mean():.4f} psnr={psnr:.2f}dB exact_samples={np.mean(d == 0) * 100:.2f}%")
PY
}

# ---- 4. rows -------------------------------------------------------------------------------------
cd "$IN"
B="$BIN"
FFV="$(ffmpeg -version | head -1 | awk '{print $3}')"

# ec-image: PNG decode, 1920x1080 RGB8.
l=$(wait_load)
res=$("$B/ec-image-bench" png-dec png_rgb.png "$OUT/png_rgb.rgb" 5)
best=$(field "$res" best_decode_s)
echo "ROW|ec-image|png decode|PNG RGB8 1920x1080 (ffmpeg testsrc2 frame)|$best|$(field "$res" mpix_per_s) Mpix/s, $(field "$res" mb_per_s) MB/s (RGB out)|load1=$l"
if [ "$(md5_of ref_png_rgb.rgb)" = "$(md5_of "$OUT/png_rgb.rgb")" ]; then
  echo "CHECK|ec-image png decode|md5 $(md5_of ref_png_rgb.rgb | cut -c1-8) = ffmpeg rawvideo rgb24 (bit-exact, $(stat -c%s ref_png_rgb.rgb) bytes)"
else
  echo "CHECK|ec-image png decode|md5 MISMATCH vs ffmpeg: ours $(md5_of "$OUT/png_rgb.rgb") ffmpeg $(md5_of ref_png_rgb.rgb)"
fi

# ec-image: JPEG decode, 1920x1080 4:2:0 q2.
l=$(wait_load)
res=$("$B/ec-image-bench" jpeg-dec jpg_420_q2.jpg "$OUT/jpg.rgb" 5)
best=$(field "$res" best_decode_s)
echo "ROW|ec-image|jpeg decode|JPEG 4:2:0 q2 1920x1080 (ffmpeg mjpeg -q:v 2)|$best|$(field "$res" mpix_per_s) Mpix/s, $(field "$res" mb_per_s) MB/s (RGB out)|load1=$l"
echo "CHECK|ec-image jpeg decode|vs ffmpeg rawvideo rgb24 decode: $(psnr "$OUT/jpg.rgb" ref_jpg.rgb 1920 1080) (not bit-exact by design; see report)"

# ec-image: PNG encode of the 1920x1080 RGBA frame, decoded back by ffmpeg.
l=$(wait_load)
res=$("$B/ec-image-bench" png-enc src_rgba.raw 1920 1080 "$OUT/enc.png" 5)
best=$(field "$res" best_encode_s)
echo "ROW|ec-image|png encode|PNG RGBA8 1920x1080 (raw input from ffmpeg testsrc2)|$best|$(field "$res" mpix_per_s) Mpix/s, $(field "$res" mb_per_s) MB/s (raw in), out $(field "$res" out_bytes) bytes|load1=$l"
"${FF[@]}" -i "$OUT/enc.png" -f rawvideo -pix_fmt rgba "$OUT/enc-ffdec.rgba"
if [ "$(md5_of src_rgba.raw)" = "$(md5_of "$OUT/enc-ffdec.rgba")" ]; then
  echo "CHECK|ec-image png encode|ffmpeg decode of our PNG md5 $(md5_of src_rgba.raw | cut -c1-8) = source RGBA (bit-exact)"
else
  echo "CHECK|ec-image png encode|ffmpeg decode of our PNG md5 MISMATCH: ours $(md5_of "$OUT/enc-ffdec.rgba") source $(md5_of src_rgba.raw)"
fi

# ec-matroska: demux, H.264 1080p30 + AAC 128k stereo, 60 s.
file_bytes=$(stat -c%s h264_aac_1080p30.mkv)
l=$(wait_load)
res=$("$B/ec-matroska-bench" demux h264_aac_1080p30.mkv "$OUT/mkv" 5)
pk=$(field "$res" packets)
echo "ROW|ec-matroska|demux|H.264 1080p30 + AAC 128k stereo, 60 s (${file_bytes} bytes)|$(field "$res" best_demux_s)|$(field "$res" packets_per_s) packets/s, $(field "$res" mb_per_s) MB/s (payload)|load1=$l packets=$pk"
v_ok=$([ "$(md5_of "$OUT/mkv.s0.bin")" = "$(md5_of ref_mkv.v.bin)" ] && echo match || echo MISMATCH)
a_ok=$([ "$(md5_of "$OUT/mkv.s1.bin")" = "$(md5_of ref_mkv.a.bin)" ] && echo match || echo MISMATCH)
ff_pk=$(( $(ffprobe -v error -count_packets -select_streams v:0 -show_entries stream=nb_read_packets -of csv=p=0 h264_aac_1080p30.mkv) + $(ffprobe -v error -count_packets -select_streams a:0 -show_entries stream=nb_read_packets -of csv=p=0 h264_aac_1080p30.mkv) ))
echo "CHECK|ec-matroska demux|payload md5 vs ffmpeg -c copy -f data: video $v_ok, audio $a_ok; packets ours=$pk ffprobe=$ff_pk"

# ec-flac: decode, 16-bit stereo 44.1k 60 s and 24-bit 5.1 48k 60 s.
for spec in "st16_44k.flac|ref_st16.pcm|FLAC 16-bit stereo 44.1 kHz 60 s (ffmpeg)|2|16|44100" \
            "m51_48k_24.flac|ref_m51.s32|FLAC 24-bit 5.1 48 kHz 60 s (ffmpeg, s32 container)|6|24|48000"; do
  IFS='|' read -r in ref content _ _ _ <<<"$spec"
  l=$(wait_load)
  res=$("$B/ec-flac-bench" dec "$in" "$OUT/flac-$in.pcm" 5)
  best=$(field "$res" best_decode_s); xrt=$(field "$res" x_realtime)
  echo "ROW|ec-flac|decode|$content|$best|${xrt}x realtime|load1=$l"
  if [ "$(md5_of "$ref")" = "$(md5_of "$OUT/flac-$in.pcm")" ]; then
    echo "CHECK|ec-flac decode $in|md5 $(md5_of "$ref" | cut -c1-8) = ffmpeg PCM ($(stat -c%s "$ref") bytes, bit-exact)"
  else
    echo "CHECK|ec-flac decode $in|md5 MISMATCH vs ffmpeg: ours $(md5_of "$OUT/flac-$in.pcm") ffmpeg $(md5_of "$ref")"
  fi
done

# ec-flac: encode at the export level (EncoderConfig::default()), then ffmpeg decode of our output.
for spec in "ref_st16.pcm|2|16|44100|s16le|FLAC encode 16-bit stereo 44.1 kHz 60 s (default level)" \
            "ref_m51.s24|6|24|48000|s24le|FLAC encode 24-bit 5.1 48 kHz 60 s (default level)"; do
  IFS='|' read -r src ch bits rate fmt content <<<"$spec"
  l=$(wait_load)
  res=$("$B/ec-flac-bench" enc "$src" "$ch" "$bits" "$rate" "$OUT/enc-$bits.flac" 5)
  best=$(field "$res" best_encode_s); xrt=$(field "$res" x_realtime)
  echo "ROW|ec-flac|encode|$content|$best|${xrt}x realtime, $(field "$res" kbps) kb/s|load1=$l"
  "${FF[@]}" -i "$OUT/enc-$bits.flac" -f "$fmt" "$OUT/enc-$bits-ffdec.pcm"
  if [ "$(md5_of "$src")" = "$(md5_of "$OUT/enc-$bits-ffdec.pcm")" ]; then
    echo "CHECK|ec-flac encode $bits-bit|ffmpeg decode of our FLAC md5 $(md5_of "$src" | cut -c1-8) = source PCM (bit-exact)"
  else
    echo "CHECK|ec-flac encode $bits-bit|ffmpeg decode of our FLAC md5 MISMATCH: ours $(md5_of "$OUT/enc-$bits-ffdec.pcm") source $(md5_of "$src")"
  fi
done

# ec-inflate: one zlib stream, 64 MiB mixed text and PCM.
l=$(wait_load)
res=$("$B/ec-inflate-bench" zlib big_mixed.zlib "$OUT/big.bin" 5)
best=$(field "$res" best_inflate_s)
echo "ROW|ec-inflate|zlib inflate|zlib, 64 MiB mixed text (32 MiB) and 16-bit PCM (32 MiB), python level 6|$best|$(field "$res" mb_per_s) MB/s out|load1=$l in_bytes=$(field "$res" in_bytes)"
if [ "$(md5_of "$OUT/big.bin")" = "$(cut -d' ' -f1 ref_big.md5)" ]; then
  echo "CHECK|ec-inflate zlib|md5 $(cut -c1-8 ref_big.md5) = python zlib.decompress ($(field "$res" out_bytes) bytes)"
else
  echo "CHECK|ec-inflate zlib|md5 MISMATCH vs python: ours $(md5_of "$OUT/big.bin") python $(cut -d' ' -f1 ref_big.md5)"
fi
rm -f "$OUT/big.bin"

# ec-inflate: 20k length-prefixed zlib streams of 3 KiB text each (subtitle-sized packets).
l=$(wait_load)
res=$("$B/ec-inflate-bench" blocks blocks_3k.bin "$OUT/blocks.bin" 5)
best=$(field "$res" best_inflate_s)
echo "ROW|ec-inflate|zlib inflate, many blocks|20000 zlib streams of 3 KiB text (python level 6, subtitle-sized)|$best|$(field "$res" mb_per_s) MB/s out, $(field "$res" streams) streams|load1=$l"
if [ "$(md5_of "$OUT/blocks.bin")" = "$(cut -d' ' -f1 ref_blocks.md5)" ]; then
  echo "CHECK|ec-inflate blocks|md5 $(cut -c1-8 ref_blocks.md5) = python zlib.decompress per block ($(field "$res" out_bytes) bytes)"
else
  echo "CHECK|ec-inflate blocks|md5 MISMATCH vs python: ours $(md5_of "$OUT/blocks.bin") python $(cut -d' ' -f1 ref_blocks.md5)"
fi
rm -f "$OUT/blocks.bin"
echo "# ffmpeg $FFV"
