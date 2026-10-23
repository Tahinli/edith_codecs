#!/usr/bin/env bash
# seed.sh: write a handful of small MPEG audio seeds into fuzz/corpus/decode, using ffmpeg.
#
# Covers MP2 (Layer II) mono and stereo at 32 kHz, 44.1 kHz and 48 kHz, MPEG-2 LSF MP2 at
# 22.05 kHz, and MP3 (Layer III) mono and stereo at 44.1 kHz and an LSF 22.05 kHz stream. Each
# is a 1 s sine tone, so every seed is under 25 KB. Layer I has no encoder in ffmpeg and no
# generator in tests/, so it has no seed; the decoder still reaches it through the same reader.
# The corpus is not committed (see .gitignore); run this before a fuzz run.
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)
dst="$here/corpus/decode"
mkdir -p "$dst"
src="sine=frequency=440:duration=1"

enc() { # enc <name> <codec> <ext> <ar> <ac> <bitrate> <format>
  ffmpeg -v error -y -f lavfi -i "$src" -c:a "$2" -ar "$4" -ac "$5" -b:a "$6" -f "$7" "$dst/$1.$3"
}

enc mp2-mono-32k   mp2 mp2 32000 1 64k mp2
enc mp2-stereo-32k mp2 mp2 32000 2 128k mp2
enc mp2-mono-44k   mp2 mp2 44100 1 96k mp2
enc mp2-stereo-44k mp2 mp2 44100 2 192k mp2
enc mp2-stereo-48k mp2 mp2 48000 2 192k mp2
enc mp2-lsf-22k    mp2 mp2 22050 1 32k mp2
enc mp3-stereo-44k libmp3lame mp3 44100 2 128k mp3
enc mp3-mono-44k   libmp3lame mp3 44100 1 64k mp3
enc mp3-lsf-22k    libmp3lame mp3 22050 1 32k mp3

echo "seed.sh: $(ls "$dst" | wc -l) seeds in $dst"
