#!/usr/bin/env bash
# bench-benchaudio.sh: the audio rows of lanes/bench.md ("lane benchaudio", DONE.md gate G6).
#
# Local machine only (DONE.md D7 exception): build with -j6 into CARGO_TARGET_DIR and run each
# row as the best of 5 inside the bench binary. Before each row, wait until the 1-minute load is
# below 3. Clips are synthetic and generated here with the local ffmpeg (never personal media);
# they live in $BENCH_CLIPS (default ~/.cache/benchaudio-clips), outside the repo.
#
# Output: one "ROW|..." line per bench row (crate, op, content, best time, x realtime or rates,
# and the output check), then "CHECK|..." lines with the comparison details. The lanes/bench.md
# "lane benchaudio" table is written from these lines.
#
# usage: scripts/bench-benchaudio.sh [--no-clips] [--no-build]
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
CLIPS="${BENCH_CLIPS:-$HOME/.cache/benchaudio-clips}"
BIN="${BENCH_BIN:-$HOME/.cache/benchaudio-bin}"
OUT="$CLIPS/out"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/tgt-benchaudio}"
export RUSTC_WRAPPER=
export TMPDIR="${TMPDIR:-$HOME/.cache/tmp-benchaudio}"
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
echo "# host=local cores=$(nproc) commit=$COMMIT ffmpeg=$(ffmpeg -version | head -1 | awk '{print $3}')"

# ---- 1. synthetic clips -------------------------------------------------------------------
if [ "$DO_CLIPS" = 1 ]; then
  cd "$CLIPS"
  if [ ! -f src_stereo.wav ]; then
    # 60 s stereo: tones with a slow AM, plus two independent pink-noise channels.
    "${FF[@]}" -f lavfi -i "aevalsrc='0.25*sin(2*PI*220*t)+0.12*sin(2*PI*277.18*t)*(0.5+0.5*sin(2*PI*2*t))+0.1*sin(2*PI*329.63*t)|0.25*sin(2*PI*222*t)+0.12*sin(2*PI*280*t)*(0.5+0.5*sin(2*PI*2*t))+0.1*sin(2*PI*331*t)':s=48000:d=60" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=1" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=2" \
      -filter_complex "[1:a][2:a]amerge=inputs=2,aformat=channel_layouts=stereo[n];[0:a][n]amix=inputs=2:normalize=0,aformat=sample_fmts=s16:channel_layouts=stereo[o]" \
      -map "[o]" -c:a pcm_s16le src_stereo.wav
  fi
  if [ ! -f src_stereo.f32 ]; then "${FF[@]}" -i src_stereo.wav -f f32le -acodec pcm_f32le src_stereo.f32; fi
  if [ ! -f src_51.thd ]; then
    # 60 s 5.1 for TrueHD: six tones plus six independent pink-noise channels.
    "${FF[@]}" -f lavfi -i "aevalsrc='0.25*sin(2*PI*220*t)|0.25*sin(2*PI*222*t)|0.2*sin(2*PI*110*t)|0.1*sin(2*PI*440*t)|0.15*sin(2*PI*330*t)|0.15*sin(2*PI*150*t)':s=48000:d=60" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=3" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=4" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=5" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=6" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=7" \
      -f lavfi -i "anoisesrc=d=60:c=pink:a=0.03:s=48000:r=48000:seed=8" \
      -filter_complex "[1:a][2:a][3:a][4:a][5:a][6:a]amerge=inputs=6,aformat=channel_layouts=5.1[n];[0:a][n]amix=inputs=2:normalize=0,aformat=sample_fmts=s32:channel_layouts=5.1[o]" \
      -map "[o]" -c:a truehd -strict -2 -f truehd src_51.thd
  fi
  # AAC-LC 128k stereo (libfdk_aac) and Opus 96k, Vorbis q5 (libvorbis), MP3 192k reference.
  [ -f aac_lc_128k.adts ] || "${FF[@]}" -i src_stereo.wav -c:a libfdk_aac -profile:a aac_low -b:a 128k -f adts aac_lc_128k.adts
  [ -f opus_96k.ogg ] || "${FF[@]}" -i src_stereo.wav -c:a libopus -b:a 96k -f ogg opus_96k.ogg
  [ -f vorbis_q5.ogg ] || "${FF[@]}" -i src_stereo.wav -c:a libvorbis -q:a 5 -f ogg vorbis_q5.ogg
  # HE-AAC v1: the local libfdk_aac cannot encode SBR (it fails "Unable to set the AOT 5" at every
  # rate and bitrate tried), so the stream is the 2.1 s HE-AAC v1 fixture of the repo
  # (ec-aac tests/data/heaac_44100_48k_implicit_2s.aac, implicit SBR), repeated 28 times to 60 s.
  if [ ! -f aac_he_v1_60s.adts ]; then
    fx="$REPO/crates/ec-aac/tests/data/heaac_44100_48k_implicit_2s.aac"
    : > aac_he_v1_60s.adts
    for _ in $(seq 28); do cat "$fx" >> aac_he_v1_60s.adts; done
  fi
  # H.264 1080p30 + AAC stereo, 60 s synthetic (testsrc2 video, the tones+noise audio).
  [ -f h264_aac_1080p30.mp4 ] || "${FF[@]}" -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=60" -i src_stereo.wav \
      -c:v libx264 -preset veryfast -b:v 4M -pix_fmt yuv420p -c:a aac -b:a 128k -shortest h264_aac_1080p30.mp4
  # References for the output checks (ffmpeg's own decoders and remux).
  [ -f ref_aac_lc.f32le ] || "${FF[@]}" -i aac_lc_128k.adts -f f32le -acodec pcm_f32le ref_aac_lc.f32le
  [ -f ref_aac_he.f32le ] || "${FF[@]}" -i aac_he_v1_60s.adts -f f32le -acodec pcm_f32le ref_aac_he.f32le
  [ -f ref_opus.f32le ] || "${FF[@]}" -i opus_96k.ogg -f f32le -acodec pcm_f32le ref_opus.f32le
  [ -f ref_vorbis.f32le ] || "${FF[@]}" -i vorbis_q5.ogg -f f32le -acodec pcm_f32le ref_vorbis.f32le
  [ -f ref_truehd.s32le ] || "${FF[@]}" -i src_51.thd -f s32le -acodec pcm_s32le ref_truehd.s32le
  [ -f ref_mp4.v.bin ] || "${FF[@]}" -i h264_aac_1080p30.mp4 -map 0:v:0 -c copy -f data ref_mp4.v.bin
  [ -f ref_mp4.a.bin ] || "${FF[@]}" -i h264_aac_1080p30.mp4 -map 0:a:0 -c copy -f data ref_mp4.a.bin
  cd "$REPO"
fi

# ---- 2. build the six examples (one binary per crate, copied before the next build) --------
crates=(ec-aac ec-opus ec-vorbis ec-mp3 ec-truehd ec-mp4)
if [ "$DO_BUILD" = 1 ]; then
  for c in "${crates[@]}"; do
    cargo build --release -j6 -p "$c" --example bench
    cp "$CARGO_TARGET_DIR/release/examples/bench" "$BIN/$c-bench"
  done
fi

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
ratio() { awk -v a="$1" -v b="$2" 'BEGIN{ if (b > 0) printf "%.2f", a / b; else print "nan" }'; }
# corr CHECK: min per-channel Pearson correlation at the best lag (+-4096 samples), ref vs test.
corr() {
  python3 -I - "$1" "$2" "$3" "$4" "$5" <<'PY'
import sys
import numpy as np
ref_p, test_p, ch, rate, fmt = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
def load(p):
    a = np.fromfile(p, '<f4' if fmt == 'f32' else '<i4').astype(np.float64)
    if fmt == 's32':
        a /= 2147483648.0
    n = (a.size // ch) * ch
    return a[:n].reshape(-1, ch)
r, t = load(ref_p), load(test_p)
L = min(2 * rate, r.shape[0] // 2)
s0 = min(10 * rate, r.shape[0] - L)
seg = r[s0:s0 + L, 0] - r[s0:s0 + L, 0].mean()
seg_n = np.linalg.norm(seg)
best_c, best_lag = -2.0, 0
for lag in range(-4096, 4097):
    s = s0 + lag
    if s < 0 or s + L > t.shape[0]:
        continue
    x = t[s:s + L, 0]
    x = x - x.mean()
    c = float(np.dot(seg, x) / (seg_n * np.linalg.norm(x) + 1e-30))
    if c > best_c:
        best_c, best_lag = c, lag
i0, i1 = max(0, -best_lag), min(r.shape[0], t.shape[0] - best_lag)
cs = []
for k in range(ch):
    a = r[i0:i1, k] - r[i0:i1, k].mean()
    b = t[i0 + best_lag:i1 + best_lag, k] - t[i0 + best_lag:i1 + best_lag, k].mean()
    cs.append(float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-30)))
print(f"corr_min={min(cs):.6f} corr_max={max(cs):.6f} lag={best_lag} ref_frames={r.shape[0]} test_frames={t.shape[0]} overlap_frames={i1 - i0}")
PY
}
md5_of() { md5sum "$1" | cut -c1-8; }

# ---- 4. rows -----------------------------------------------------------------------------
cd "$CLIPS"
B="$BIN"

# AAC decode, LC and HE-AAC v1 (ADTS), 2 ch.
for spec in "ec-aac|aac_lc_128k.adts|AAC-LC stereo 128 kb/s 60 s (libfdk_aac)|ref_aac_lc.f32le" \
            "ec-aac|aac_he_v1_60s.adts|HE-AAC v1 stereo 64 kb/s 2.1 s fixture x28 (implicit SBR)|ref_aac_he.f32le"; do
  IFS='|' read -r crate in content ref <<<"$spec"
  l=$(wait_load)
  res=$("$B/$crate-bench" dec "$in" "$OUT/aacdec-$in.f32le" 5)
  best=$(field "$res" best_decode_s); samples=$(field "$res" samples); fails=$(field "$res" frames_failed)
  audio=$(awk -v s="$samples" 'BEGIN{printf "%.3f", s/2/48000}')
  echo "ROW|$crate|decode|$content|$best|$(ratio "$audio" "$best")x|load1=$l frames_failed=$fails"
  c=$(corr "$ref" "$OUT/aacdec-$in.f32le" 2 48000 f32)
  echo "CHECK|$crate decode $in|vs ffmpeg $(ffmpeg -version | head -1 | awk '{print $3}') native aac: $c"
done

# AAC-LC encode, stereo 128k: encode the source, then decode the result with ffmpeg and compare.
l=$(wait_load)
res=$("$B/ec-aac-bench" enc src_stereo.f32 2 48000 128 "$OUT/aacenc.adts" 5)
best=$(field "$res" best_encode_s); audio=$(field "$res" audio_s); bytes=$(field "$res" bytes)
echo "ROW|ec-aac|encode|AAC-LC stereo 128 kb/s from 60 s PCM|$best|$(ratio "$audio" "$best")x|load1=$l kbps=$(awk -v b="$bytes" -v a="$audio" 'BEGIN{printf "%.1f", b*8/a/1000}')"
"${FF[@]}" -i "$OUT/aacenc.adts" -f f32le -acodec pcm_f32le "$OUT/aacenc-ffdec.f32le"
echo "CHECK|ec-aac encode|ffmpeg decode of our ADTS vs source: $(corr src_stereo.f32 "$OUT/aacenc-ffdec.f32le" 2 48000 f32)"
# Calibration: ffmpeg's own AAC encoders at the same 128k, through the same decode and check.
"${FF[@]}" -i src_stereo.wav -c:a aac -b:a 128k -f adts "$OUT/cal-native.adts"
"${FF[@]}" -i "$OUT/cal-native.adts" -f f32le -acodec pcm_f32le "$OUT/cal-native.f32le"
echo "CHECK|ec-aac encode calibration|ffmpeg native aac 128k, same check: $(corr src_stereo.f32 "$OUT/cal-native.f32le" 2 48000 f32)"
"${FF[@]}" -i src_stereo.wav -c:a libfdk_aac -profile:a aac_low -b:a 128k -f adts "$OUT/cal-fdk.adts"
"${FF[@]}" -i "$OUT/cal-fdk.adts" -f f32le -acodec pcm_f32le "$OUT/cal-fdk.f32le"
echo "CHECK|ec-aac encode calibration|ffmpeg libfdk_aac 128k, same check: $(corr src_stereo.f32 "$OUT/cal-fdk.f32le" 2 48000 f32)"

# Opus decode 48k stereo 96k, and encode.
l=$(wait_load)
res=$("$B/ec-opus-bench" dec opus_96k.ogg "$OUT/opusdec.f32le" 5)
best=$(field "$res" best_decode_s); samples=$(field "$res" samples)
audio=$(awk -v s="$samples" 'BEGIN{printf "%.3f", s/48000}')
echo "ROW|ec-opus|decode|Opus 48k stereo 96 kb/s 60 s (libopus)|$best|$(ratio "$audio" "$best")x|load1=$l packets_failed=$(field "$res" failed)"
echo "CHECK|ec-opus decode|vs ffmpeg Opus decode: $(corr ref_opus.f32le "$OUT/opusdec.f32le" 2 48000 f32)"

l=$(wait_load)
res=$("$B/ec-opus-bench" enc src_stereo.f32 2 96 "$OUT/opusenc.ogg" 5)
best=$(field "$res" best_encode_s); audio=$(field "$res" audio_s); bytes=$(field "$res" bytes)
echo "ROW|ec-opus|encode|Opus 48k stereo 96 kb/s 60 s (vbr off, 20 ms frames)|$best|$(ratio "$audio" "$best")x|load1=$l kbps=$(awk -v b="$bytes" -v a="$audio" 'BEGIN{printf "%.1f", b*8/a/1000}')"
"${FF[@]}" -i "$OUT/opusenc.ogg" -f f32le -acodec pcm_f32le "$OUT/opusenc-ffdec.f32le"
echo "CHECK|ec-opus encode|ffmpeg decode of our Ogg Opus vs source: $(corr src_stereo.f32 "$OUT/opusenc-ffdec.f32le" 2 48000 f32)"

# Vorbis decode, q5 stereo.
l=$(wait_load)
res=$("$B/ec-vorbis-bench" dec vorbis_q5.ogg "$OUT/vorbisdec.f32le" 5)
best=$(field "$res" best_decode_s); samples=$(field "$res" samples)
audio=$(awk -v s="$samples" 'BEGIN{printf "%.3f", s/48000}')
echo "ROW|ec-vorbis|decode|Vorbis q5 stereo 60 s (libvorbis)|$best|$(ratio "$audio" "$best")x|load1=$l"
echo "CHECK|ec-vorbis decode|vs ffmpeg Vorbis decode: $(corr ref_vorbis.f32le "$OUT/vorbisdec.f32le" 2 48000 f32)"

# MP3 encode, 192k CBR stereo (Layer III); the output is decoded with ffmpeg and compared.
l=$(wait_load)
res=$("$B/ec-mp3-bench" enc src_stereo.f32 2 48000 192 "$OUT/mp3enc.mp3" 5)
best=$(field "$res" best_encode_s); audio=$(field "$res" audio_s); bytes=$(field "$res" bytes)
echo "ROW|ec-mp3|encode|MP3 192 kb/s CBR stereo 48k 60 s|$best|$(ratio "$audio" "$best")x|load1=$l kbps=$(awk -v b="$bytes" -v a="$audio" 'BEGIN{printf "%.1f", b*8/a/1000}')"
"${FF[@]}" -i "$OUT/mp3enc.mp3" -f f32le -acodec pcm_f32le "$OUT/mp3enc-ffdec.f32le"
echo "CHECK|ec-mp3 encode|ffmpeg decode of our MP3 vs source: $(corr src_stereo.f32 "$OUT/mp3enc-ffdec.f32le" 2 48000 f32)"

# TrueHD decode, 5.1 24-bit (S32 container). Lossless: the output must equal ffmpeg's bytes.
l=$(wait_load)
res=$("$B/ec-truehd-bench" dec src_51.thd "$OUT/truehd-dec.s32le" 5)
best=$(field "$res" best_decode_s); audio=$(field "$res" audio_s); fails=$(field "$res" check_failures)
echo "ROW|ec-truehd|decode|TrueHD 5.1 24-bit 48k 60 s|$best|$(ratio "$audio" "$best")x|load1=$l self_check_failures=$fails"
if cmp -s ref_truehd.s32le "$OUT/truehd-dec.s32le"; then
  echo "CHECK|ec-truehd decode|md5 $(md5_of ref_truehd.s32le) = ffmpeg (bit-exact, $(stat -c%s ref_truehd.s32le) bytes)"
else
  echo "CHECK|ec-truehd decode|md5 MISMATCH vs ffmpeg: ours $(md5_of "$OUT/truehd-dec.s32le") ffmpeg $(md5_of ref_truehd.s32le)"
fi

# MP4 demux and mux, H.264 1080p30 + AAC, 60 s.
file_bytes=$(stat -c%s h264_aac_1080p30.mp4)
l=$(wait_load)
res=$("$B/ec-mp4-bench" demux h264_aac_1080p30.mp4 "$OUT/mp4" 5)
pps=$(field "$res" packets_per_s); mbs=$(field "$res" mb_per_s); pk=$(field "$res" packets)
echo "ROW|ec-mp4|demux|H.264 1080p30 + AAC 128k stereo, 60 s (${file_bytes} bytes)|$(field "$res" best_demux_s)|$pps packets/s, $mbs MB/s (payload)|load1=$l packets=$pk"
v_ok=$([ "$(md5_of "$OUT/mp4.s0.bin")" = "$(md5_of ref_mp4.v.bin)" ] && echo match || echo MISMATCH)
a_ok=$([ "$(md5_of "$OUT/mp4.s1.bin")" = "$(md5_of ref_mp4.a.bin)" ] && echo match || echo MISMATCH)
echo "CHECK|ec-mp4 demux|payload md5 vs ffmpeg -c copy -f data: video $v_ok, audio $a_ok; packets ours=$pk ffprobe=$(( $(ffprobe -v error -count_packets -select_streams v:0 -show_entries stream=nb_read_packets -of csv=p=0 h264_aac_1080p30.mp4) + $(ffprobe -v error -count_packets -select_streams a:0 -show_entries stream=nb_read_packets -of csv=p=0 h264_aac_1080p30.mp4) ))"

l=$(wait_load)
res=$("$B/ec-mp4-bench" mux h264_aac_1080p30.mp4 "$OUT/mux.mp4" 5)
echo "ROW|ec-mp4|mux|H.264 1080p30 + AAC 128k stereo, 60 s (remux of the same packets)|$(field "$res" best_mux_s)|$(field "$res" packets_per_s) packets/s, $(field "$res" mb_per_s) MB/s (payload)|load1=$l"
"${FF[@]}" -i "$OUT/mux.mp4" -map 0:v:0 -c copy -f data "$OUT/mux.v.bin"
"${FF[@]}" -i "$OUT/mux.mp4" -map 0:a:0 -c copy -f data "$OUT/mux.a.bin"
v_ok=$([ "$(md5_of "$OUT/mux.v.bin")" = "$(md5_of ref_mp4.v.bin)" ] && echo match || echo MISMATCH)
a_ok=$([ "$(md5_of "$OUT/mux.a.bin")" = "$(md5_of ref_mp4.a.bin)" ] && echo match || echo MISMATCH)
echo "CHECK|ec-mp4 mux|ffmpeg -c copy -f data of our output vs source payload: video $v_ok, audio $a_ok"
