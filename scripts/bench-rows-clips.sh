#!/usr/bin/env bash
# bench-rows-clips.sh: generate the ffmpeg clips used by scripts/bench-rows.sh.
# Clips go to $BENCH_CLIPS (default ~/.cache/benchrows-clips), never into the repo.
# The h264 input is the personal 1080p30 clip that ec-bench's real_video() picks first
# (first h264 mp4 in ~/Videos or ~/Downloads, sorted by path); it is only read, never copied.
set -euo pipefail
d=${BENCH_CLIPS:-$HOME/.cache/benchrows-clips}
h264_src=${H264_SRC:-$HOME/Downloads/reelk.mp4}
mkdir -p "$d"
ff() { ffmpeg -nostdin -v error -y "$@"; }

# VP8 1080p30, libvpx, 10 s, 4 Mb/s, realtime deadline.
[ -s "$d/vp8-1080p30.ivf" ] || ff -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 \
  -c:v libvpx -b:v 4M -deadline realtime -cpu-used 8 -pix_fmt yuv420p "$d/vp8-1080p30.ivf"

# VP9 profile 0 1080p30, libvpx-vp9, 10 s, 4 tile columns (threads have work to split).
[ -s "$d/vp9-1080p30-tile4.ivf" ] || ff -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 \
  -c:v libvpx-vp9 -tile-columns 2 -row-mt 1 -b:v 4M -deadline realtime -cpu-used 8 \
  -pix_fmt yuv420p "$d/vp9-1080p30-tile4.ivf"

# AC-3 5.1 at 448 kb/s, 60 s: six independent pink-noise channels, one per 5.1 speaker
# (a mono source upmixed would fill only the centre channel and make the parity test vacuous).
ac3_in=()
for seed in 1 2 3 4 5 6; do
  ac3_in+=(-f lavfi -i "anoisesrc=d=60:c=pink:r=48000:a=0.2:seed=$seed")
done
[ -s "$d/ac3-5.1-448k.ac3" ] || ff "${ac3_in[@]}" -filter_complex \
  "[0:a][1:a][2:a][3:a][4:a][5:a]amerge=inputs=6,pan=5.1(side)|c0=c0|c1=c1|c2=c2|c3=c3|c4=c4|c5=c5[a]" \
  -map "[a]" -ar 48000 -c:a ac3 -b:a 448k -f ac3 "$d/ac3-5.1-448k.ac3"

# H264 Annex-B, first 10 s of the h264 input, stream copy (same extraction as ec-bench).
[ -s "$d/h264-1080p30-10s.264" ] || ff -t 10 -i "$h264_src" -an -c:v copy \
  -bsf:v h264_mp4toannexb -f h264 "$d/h264-1080p30-10s.264"

# ffmpeg reference outputs (raw, the decoded samples the crates must match).
[ -s "$d/vp8-1080p30.yuv.md5" ] || ffmpeg -nostdin -v error -i "$d/vp8-1080p30.ivf" \
  -f rawvideo -pix_fmt yuv420p - | md5sum | cut -d' ' -f1 > "$d/vp8-1080p30.yuv.md5"
[ -s "$d/vp9-1080p30-tile4.yuv.md5" ] || ffmpeg -nostdin -v error -i "$d/vp9-1080p30-tile4.ivf" \
  -f rawvideo -pix_fmt yuv420p - | md5sum | cut -d' ' -f1 > "$d/vp9-1080p30-tile4.yuv.md5"
[ -s "$d/ac3-5.1-448k.ref.f32" ] || ffmpeg -nostdin -v error -i "$d/ac3-5.1-448k.ac3" \
  -f f32le -acodec pcm_f32le -ac 6 "$d/ac3-5.1-448k.ref.f32"
ls -l "$d"
