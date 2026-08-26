#!/usr/bin/env bash
# Still-image fixtures for ec-image's differential tests: every PNG colour
# type and bit depth, both JPEG processes at each chroma layout, and both WebP
# bitstreams with and without alpha.
#
# The corpus is generated rather than committed, so the repository stays free
# of binaries and every machine can rebuild it: ImageMagick writes the PNG and
# JPEG variants, ffmpeg the WebP ones.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/fixtures/stills"
mkdir -p "$out"

need() {
    command -v "$1" >/dev/null || { echo "missing: $1" >&2; exit 1; }
}
need magick
need ffmpeg

# Source pictures: one with smooth and hard content at even dimensions, one at
# odd dimensions (chroma subsampling and Adam7 both round there), and one with
# a real alpha channel.
# Seeded, so the corpus is byte-identical on every machine and a differential
# bar measured today still means the same thing tomorrow.
magick -seed 1234 -size 320x240 plasma:fractal -blur 0x1 \
    -fill white -stroke black -draw "rectangle 20,20 80,60" \
    -draw "line 0,239 319,0" "$out/source.png"
magick "$out/source.png" -resize 61x47! "$out/source-odd.png"
magick "$out/source.png" -alpha set -channel A \
    -fx "i/w" +channel "$out/source-alpha.png"

# --- PNG: every colour type, every bit depth, both interlace methods --------
magick "$out/source.png" -define png:color-type=2 -depth 8 "$out/rgb8.png"
magick "$out/source-alpha.png" -define png:color-type=6 -depth 8 "$out/rgba8.png"
magick "$out/source.png" -colorspace Gray -define png:color-type=0 -depth 8 "$out/gray8.png"
magick "$out/source-alpha.png" -colorspace Gray -define png:color-type=4 -depth 8 "$out/graya8.png"
magick "$out/source.png" -define png:color-type=2 -depth 16 "$out/rgb16.png"
magick "$out/source-alpha.png" -define png:color-type=6 -depth 16 "$out/rgba16.png"
magick "$out/source.png" -colorspace Gray -define png:color-type=0 -depth 16 "$out/gray16.png"
magick "$out/source.png" -colors 256 -define png:color-type=3 "$out/palette8.png"
magick "$out/source.png" -colors 16 -define png:color-type=3 -define png:bit-depth=4 "$out/palette4.png"
magick "$out/source.png" -colors 3 -define png:color-type=3 -define png:bit-depth=2 "$out/palette2.png"
magick "$out/source.png" -monochrome -define png:color-type=3 -define png:bit-depth=1 "$out/palette1.png"
magick "$out/source.png" -colorspace Gray -depth 4 -define png:color-type=0 -define png:bit-depth=4 "$out/gray4.png"
magick "$out/source.png" -colorspace Gray -depth 2 -define png:color-type=0 -define png:bit-depth=2 "$out/gray2.png"
magick "$out/source.png" -colorspace Gray -threshold 50% -define png:color-type=0 -define png:bit-depth=1 "$out/gray1.png"
magick "$out/source.png" -interlace PNG -define png:color-type=2 "$out/interlaced-rgb8.png"
magick "$out/source-alpha.png" -interlace PNG -define png:color-type=6 -depth 16 "$out/interlaced-rgba16.png"
magick "$out/source.png" -colors 64 -interlace PNG -define png:color-type=3 "$out/interlaced-palette.png"
magick "$out/source-odd.png" -define png:color-type=2 "$out/odd-rgb8.png"
magick "$out/source-odd.png" -interlace PNG -define png:color-type=2 "$out/odd-interlaced.png"
# Palette with a transparent entry, and a colour-keyed truecolour image.
magick "$out/source-alpha.png" -colors 32 PNG8:"$out/trns-palette.png"
magick "$out/source.png" -define png:color-type=2 -transparent white "$out/trns-rgb.png" || true
magick "$out/source.png" -colorspace Gray -depth 8 -transparent black \
    -define png:color-type=0 "$out/trns-gray.png" || true

# --- JPEG: both processes, every chroma layout, restarts, EXIF -------------
magick "$out/source.png" -quality 90 -sampling-factor 1x1 "$out/baseline-444.jpg"
magick "$out/source.png" -quality 90 -sampling-factor 2x1 "$out/baseline-422.jpg"
magick "$out/source.png" -quality 90 -sampling-factor 2x2 "$out/baseline-420.jpg"
magick "$out/source.png" -quality 50 -sampling-factor 2x2 "$out/baseline-420-q50.jpg"
magick "$out/source.png" -quality 90 -sampling-factor 2x2 -interlace JPEG "$out/progressive-420.jpg"
magick "$out/source.png" -quality 90 -sampling-factor 1x1 -interlace JPEG "$out/progressive-444.jpg"
magick "$out/source.png" -colorspace Gray -quality 90 "$out/gray.jpg"
magick "$out/source.png" -quality 90 -sampling-factor 2x2 \
    -define jpeg:restart-interval=4 "$out/restart-420.jpg"
magick "$out/source-odd.png" -quality 90 -sampling-factor 2x2 "$out/odd-420.jpg"
magick "$out/source-odd.png" -quality 90 -sampling-factor 2x1 "$out/odd-422.jpg"

# --- WebP: lossy and lossless, with and without alpha ----------------------
ffmpeg -loglevel error -y -i "$out/source.png" -c:v libwebp -lossless 0 -quality 80 \
    "$out/lossy.webp"
ffmpeg -loglevel error -y -i "$out/source.png" -c:v libwebp -lossless 0 -quality 40 \
    "$out/lossy-q40.webp"
ffmpeg -loglevel error -y -i "$out/source.png" -c:v libwebp -lossless 1 "$out/lossless.webp"
ffmpeg -loglevel error -y -i "$out/source-alpha.png" -c:v libwebp -lossless 1 \
    "$out/lossless-alpha.webp"
ffmpeg -loglevel error -y -i "$out/source-alpha.png" -c:v libwebp -lossless 0 -quality 80 \
    "$out/lossy-alpha.webp"
ffmpeg -loglevel error -y -i "$out/source-odd.png" -c:v libwebp -lossless 0 -quality 80 \
    "$out/odd-lossy.webp"
ffmpeg -loglevel error -y -i "$out/source-odd.png" -c:v libwebp -lossless 1 "$out/odd-lossless.webp"
# A palette-friendly picture, so the lossless colour-indexing transform is
# exercised rather than only the predictor one.
magick "$out/source.png" -colors 8 "$out/source-flat.png"
ffmpeg -loglevel error -y -i "$out/source-flat.png" -c:v libwebp -lossless 1 \
    "$out/lossless-palette.webp"
# An animation, which the decoder must refuse by name rather than decode.
# ffmpeg's libwebp_anim silently writes a single still here, so ImageMagick
# writes this one: its output really does carry ANIM/ANMF chunks.
magick -delay 10 "$out/source.png" "$out/source-flat.png" "$out/source-odd.png" \
    -loop 0 "$out/animated.webp"

# --- GIF: still, interlaced, transparent, odd-size, animated ---------------
# GIF is palette-only, so every source is quantised first; the differential bar
# is pixel-exact, which only means anything if both decoders see the same file.
magick "$out/source.png" -colors 256 "$out/still.gif"
magick "$out/source.png" -colors 256 -interlace GIF "$out/interlaced.gif"
magick "$out/source.png" -colors 64 -transparent white "$out/trns.gif"
magick "$out/source-odd.png" -colors 256 "$out/odd.gif"
# Three frames, each with its own delay, and a disposal method that clears the
# frame's rectangle -- the compositing path only runs when frames differ.
magick -delay 7 "$out/source.png" -delay 13 "$out/source-flat.png" \
    -delay 4 "$out/source-odd.png" -dispose Background -loop 0 -colors 64 \
    "$out/animated.gif"

# Small versions of each format, for the fuzz sweep: ten thousand mutations
# per format is only affordable on a picture that decodes in microseconds.
magick "$out/source.png" -resize 32x24! "$out/tiny.png"
magick "$out/tiny.png" -quality 80 -sampling-factor 2x2 "$out/tiny.jpg"
ffmpeg -loglevel error -y -i "$out/tiny.png" -c:v libwebp -lossless 0 -quality 75 \
    "$out/tiny-lossy.webp"
ffmpeg -loglevel error -y -i "$out/tiny.png" -c:v libwebp -lossless 1 "$out/tiny-lossless.webp"
magick "$out/tiny.png" -colors 64 "$out/tiny.gif"

echo "wrote $(ls "$out" | wc -l) files to $out"
