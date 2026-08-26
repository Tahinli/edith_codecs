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
# A second animation whose frames carry alpha and ask for dispose-to-background:
# the first one is opaque and disposes nothing, so it leaves the blending and
# the disposal rectangle untested.  Lossless, so the frames are exact and the
# only thing the comparison can catch is the compositing.
magick "$out/source-alpha.png" -resize 96x72! "$out/anim-a.png"
magick "$out/source-alpha.png" -resize 96x72! -channel A -evaluate multiply 0.5 \
    +channel "$out/anim-b.png"
magick "$out/source-flat.png" -resize 48x36! -background none -extent 96x72 \
    "$out/anim-c.png"
magick -dispose Background -delay 6 "$out/anim-a.png" -delay 9 "$out/anim-b.png" \
    -delay 3 "$out/anim-c.png" -loop 0 -define webp:lossless=true \
    "$out/anim-alpha.webp"
rm -f "$out/anim-a.png" "$out/anim-b.png" "$out/anim-c.png"
# libwebp's own composited frames, through Pillow, as the reference for that
# animation: the `image` crate ignores the dispose-to-background flag, so it
# cannot be the oracle for the frame that follows a disposal.
if python3 -c "import PIL" 2>/dev/null; then
    python3 - "$out" <<'PILLOW'
import sys
from PIL import Image

out = sys.argv[1]
animation = Image.open(f"{out}/anim-alpha.webp")
for i in range(animation.n_frames):
    animation.seek(i)
    animation.convert("RGBA").save(f"{out}/anim-alpha-f{i}.png")
PILLOW
else
    echo "skipped anim-alpha-f*.png: python3 Pillow is not installed" >&2
fi

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

# --- BMP: every depth, both compressions, both row orders ------------------
# ImageMagick writes the ordinary files; the shapes it has no switch for (16-bit
# bitfields, top-down rows, a 32-bit alpha channel, the 12-byte OS/2 header,
# and RLE4, which its RLE switch never chooses) are written by the helper.
magick "$out/source.png" -resize 64x48! "$out/bmp-source.png"
magick "$out/source-alpha.png" -resize 64x48! "$out/bmp-source-alpha.png"
magick "$out/bmp-source.png" -monochrome -type bilevel BMP3:"$out/mono.bmp"
magick "$out/bmp-source.png" -colors 16 -type palette -depth 4 -compress None \
    BMP3:"$out/pal4.bmp"
magick "$out/bmp-source.png" -colors 256 -type palette -compress None \
    BMP3:"$out/pal8.bmp"
magick "$out/bmp-source.png" -colors 256 -type palette -compress RLE \
    BMP3:"$out/rle8.bmp"
magick "$out/bmp-source.png" BMP3:"$out/rgb24.bmp"
magick "$out/source-odd.png" -resize 37x29! BMP3:"$out/odd24.bmp"
# A BITMAPV5HEADER, whose 124 bytes carry the colour space fields a v3 file
# has no room for; ImageMagick writes one when asked for alpha.
magick "$out/bmp-source-alpha.png" -define bmp:format=bmp5 BMP:"$out/v5alpha.bmp"
magick "$out/bmp-source.png" rgba:"$out/bmp-source.rgba"
magick "$out/bmp-source-alpha.png" rgba:"$out/bmp-source-alpha.rgba"
magick "$out/bmp-source.png" "$out/bmp-embed.png"
magick "$out/bmp-source.png" -quality 85 "$out/bmp-embed.jpg"
python3 "$(dirname "$0")/lib/gen-bmp-shapes.py" "$out" "$out/bmp-source.rgba" 64 48 \
    "$out/bmp-source-alpha.rgba" "$out/bmp-embed.png" "$out/bmp-embed.jpg"
rm -f "$out/bmp-source.png" "$out/bmp-source-alpha.png" "$out/bmp-source.rgba" \
    "$out/bmp-source-alpha.rgba" "$out/bmp-embed.png" "$out/bmp-embed.jpg"

# --- TIFF: both byte orders, four compressions, strips and tiles -----------
# TIFF fixes almost nothing about its layout, so the corpus is a cross-product:
# each compression, each sample depth, both byte orders, strips and tiles, and
# both plane arrangements.  ImageMagick writes all of them.
magick "$out/source.png" -resize 64x48! "$out/tiff-source.png"
magick "$out/source-alpha.png" -resize 64x48! "$out/tiff-source-alpha.png"
magick "$out/tiff-source.png" -depth 8 -compress None "$out/rgb8.tiff"
magick "$out/tiff-source.png" -depth 8 -compress LZW "$out/rgb8-lzw.tiff"
magick "$out/tiff-source.png" -depth 8 -compress RLE "$out/rgb8-packbits.tiff"
magick "$out/tiff-source.png" -depth 8 -compress Zip "$out/rgb8-zip.tiff"
# The horizontal-differencing predictor, which is what an encoder reaches for
# when the compression is LZW or Deflate.
magick "$out/tiff-source.png" -depth 8 -compress LZW -define tiff:predictor=2 \
    "$out/rgb8-lzw-pred.tiff"
magick "$out/tiff-source.png" -depth 8 -compress Zip -define tiff:predictor=2 \
    "$out/rgb8-zip-pred.tiff"
# Big-endian: the same picture with every field byte-swapped.
magick "$out/tiff-source.png" -depth 8 -compress None -define tiff:endian=msb \
    "$out/rgb8-be.tiff"
# Tiles rather than strips, and planes rather than interleaved samples.
magick "$out/tiff-source.png" -depth 8 -compress LZW \
    -define tiff:tile-geometry=32x32 "$out/rgb8-tiled.tiff"
magick "$out/tiff-source.png" -depth 8 -compress None -interlace Plane \
    "$out/rgb8-planar.tiff"
# Depths: bilevel, 4-bit palette, 8-bit grey, 16-bit grey and 16-bit colour.
magick "$out/tiff-source.png" -monochrome -type bilevel -compress None \
    "$out/bilevel.tiff"
magick "$out/tiff-source.png" -colors 16 -type palette -depth 4 -compress None \
    "$out/palette4.tiff"
# The incumbent refuses a 4-bit palette outright, so ffmpeg -- which reads it --
# writes the reference picture for that one fixture.
ffmpeg -loglevel error -y -i "$out/palette4.tiff" "$out/palette4-golden.png"
# -alpha off, or ImageMagick attaches an alpha channel to a grey picture that
# has none, and a two-sample grey file is a shape the incumbent refuses.
magick "$out/tiff-source.png" -colorspace gray -alpha off -depth 8 -compress None \
    "$out/gray8.tiff"
magick "$out/tiff-source.png" -colorspace gray -alpha off -depth 16 -compress None \
    "$out/gray16.tiff"
magick "$out/tiff-source.png" -depth 16 -compress Zip "$out/rgb16.tiff"
# Alpha, which TIFF carries as a fourth sample plus an ExtraSamples tag: both
# the straight and the premultiplied tagging, neither of which ImageMagick will
# write on request.
magick "$out/tiff-source-alpha.png" rgba:"$out/tiff-source-alpha.rgba"
python3 "$(dirname "$0")/lib/gen-tiff-shapes.py" "$out" "$out/tiff-source-alpha.rgba" 64 48
rm -f "$out/tiff-source.png" "$out/tiff-source-alpha.png" \
    "$out/tiff-source-alpha.rgba"

# Small versions of each format, for the fuzz sweep: ten thousand mutations
# per format is only affordable on a picture that decodes in microseconds.
magick "$out/source.png" -resize 32x24! "$out/tiny.png"
magick "$out/tiny.png" -quality 80 -sampling-factor 2x2 "$out/tiny.jpg"
ffmpeg -loglevel error -y -i "$out/tiny.png" -c:v libwebp -lossless 0 -quality 75 \
    "$out/tiny-lossy.webp"
ffmpeg -loglevel error -y -i "$out/tiny.png" -c:v libwebp -lossless 1 "$out/tiny-lossless.webp"
magick "$out/tiny.png" -colors 64 "$out/tiny.gif"
magick "$out/tiny.png" -colors 64 -type palette -compress None BMP3:"$out/tiny.bmp"
magick "$out/tiny.png" -depth 8 -compress LZW "$out/tiny.tiff"

echo "wrote $(ls "$out" | wc -l) files to $out"
