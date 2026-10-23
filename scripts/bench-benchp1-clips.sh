#!/usr/bin/env bash
# bench-benchp1-clips.sh: generate the inputs for the subtitle, Ogg and RIFF bench rows of
# lanes/bench.md ("lane benchp1"). Clips go to $BENCH_CLIPS (default ~/.cache/benchp1-clips),
# never into the repo. The .sup input is written by `ec-pgs --example benchp1 gen` (ffmpeg has no
# PGS encoder), so it is not made here.
set -euo pipefail
d=${BENCH_CLIPS:-$HOME/.cache/benchp1-clips}
mkdir -p "$d"
ff() { ffmpeg -nostdin -v error -y "$@"; }

# ASS with many styled events: 20 styles, one event every 0.5 s for 2 h (14400 events).
# awk writes the script; ffmpeg converts it to SRT and WebVTT (14400 cues each) for ec-subs.
if [ ! -s "$d/styled-2h.ass" ]; then
  awk 'BEGIN {
    print "[Script Info]\nTitle: benchp1\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\n"
    print "[V4+ Styles]"
    print "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
    for (s = 0; s < 20; s++) {
      printf "Style: S%02d,Arial,%d,&H00%02X%02X%02X,&H000000FF,&H00000000,&H64000000,%d,0,0,0,100,100,0,0,1,2,1,%d,10,10,40,1\n", s, 36 + s, (s * 37) % 256, (s * 91) % 256, (s * 53) % 256, s % 2, 1 + (s % 9)
    }
    print "\n[Events]"
    print "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
    for (i = 0; i < 14400; i++) {
      t0 = i * 50; t1 = t0 + 120
      s = i % 20
      printf "Dialogue: %d,%d:%02d:%02d.%02d,%d:%02d:%02d.%02d,S%02d,,0,0,0,,{\\an%d\\pos(%d,%d)\\fad(200,200)\\c&H%02X%02X%02X&\\b%d}Event %d {\\i1}styled{\\i0} text \\N line two of %d {\\fscx%d\\frz%d}\n",
        i % 3, int(t0 / 360000), int(t0 / 6000) % 60, int(t0 / 100) % 60, t0 % 100,
        int(t1 / 360000), int(t1 / 6000) % 60, int(t1 / 100) % 60, t1 % 100,
        s, 1 + (i % 9), 100 + (i * 7) % 1700, 80 + (i * 13) % 900,
        (i * 31) % 256, (i * 17) % 256, (i * 5) % 256, i % 2, i, i, 90 + i % 30, (i % 60) - 30
    }
  }' > "$d/styled-2h.ass"
fi
[ -s "$d/styled-2h.srt" ] || ff -i "$d/styled-2h.ass" -c:s srt "$d/styled-2h.srt"
[ -s "$d/styled-2h.vtt" ] || ff -i "$d/styled-2h.ass" -c:s webvtt "$d/styled-2h.vtt"

# Vorbis and Opus in Ogg, 10 min, stereo pink noise.
[ -s "$d/vorbis-10m.ogg" ] || ff -f lavfi -i "anoisesrc=d=600:c=pink:r=48000:a=0.2:seed=7" \
  -ac 2 -c:a libvorbis -q:a 4 "$d/vorbis-10m.ogg"
[ -s "$d/opus-10m.ogg" ] || ff -f lavfi -i "anoisesrc=d=600:c=pink:r=48000:a=0.2:seed=7" \
  -ac 2 -c:a libopus -b:a 128k "$d/opus-10m.ogg"

# 24-bit 5.1 WAV, 10 min at 48 kHz: six independent pink-noise channels joined as 5.1.
inputs=()
for seed in 1 2 3 4 5 6; do
  inputs+=(-f lavfi -i "anoisesrc=d=600:c=pink:r=48000:a=0.2:seed=$seed")
done
[ -s "$d/wav-51-24b-10m.wav" ] || ff "${inputs[@]}" -filter_complex \
  "[0:a][1:a][2:a][3:a][4:a][5:a]amerge=inputs=6,pan=5.1(side)|c0=c0|c1=c1|c2=c2|c3=c3|c4=c4|c5=c5[a]" \
  -map "[a]" -ar 48000 -c:a pcm_s24le -f wav "$d/wav-51-24b-10m.wav"
[ -s "$d/wav-51-24b-10m.s24le.md5" ] || ffmpeg -nostdin -v error -i "$d/wav-51-24b-10m.wav" -f s24le - \
  | md5sum | cut -d' ' -f1 > "$d/wav-51-24b-10m.s24le.md5"
echo "clips in $d"
