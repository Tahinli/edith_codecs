#!/usr/bin/env bash
# hevc10-hdr-check.sh: ffprobe/ffmpeg check of ec-h265 Main 10 HDR output. Encodes a
# synthetic 10-bit clip (testsrc2, mandelbrot, gradients) as intra and GOP with PQ and
# HLG tags, then asserts ffprobe reports profile=Main 10, pix_fmt=yuv420p10le and the
# colour tags, and that ffmpeg's decode equals the encoder reconstruction byte for byte.
# Usage (on a host with ffmpeg and cargo): scripts/hevc10-hdr-check.sh
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
data=${DATA:-$HOME/.cache/hevc10-hdr-data}
mkdir -p "$data"
cargo build --release -q -p ec-h265 --example hevc_gop --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
bin="$target/release/examples/hevc_gop"
export EC_H265_BITDEPTH=10
fail=0
for src in testsrc2 mandelbrot gradients; do
  case $src in
    gradients) lavfi="gradients=s=416x240:r=30" ;;
    *) lavfi="$src=s=416x240:r=30" ;;
  esac
  yuv="$data/$src.yuv"
  ffmpeg -v error -y -f lavfi -i "$lavfi" -t 2 -pix_fmt yuv420p10le -f rawvideo "$yuv"
  for mode in pq hlg; do
    for gop in 1 8; do
      bs="$data/$src-$mode-$gop.h265"; rec="$data/$src-$mode-$gop.rec"; dec="$data/$src-$mode-$gop.dec"
      EC_H265_HDR=$mode EC_H265_FULL_RANGE=$([ $mode = hlg ] && echo 1 || echo 0) \
        "$bin" encode "$yuv" 416 240 "$gop" 24 "$bs" "$rec" >/dev/null
      info=$(ffprobe -v error -select_streams v:0 -show_entries \
        stream=profile,pix_fmt,color_primaries,color_transfer,color_space,color_range,chroma_location \
        -of default=nw=1 "$bs" | tr '\n' ' ')
      want_tr=$([ $mode = hlg ] && echo arib-std-b67 || echo smpte2084)
      want_rg=$([ $mode = hlg ] && echo pc || echo tv)
      for k in "profile=Main 10" "pix_fmt=yuv420p10le" "color_primaries=bt2020" \
               "color_transfer=$want_tr" "color_space=bt2020nc" "color_range=$want_rg"; do
        case "$info" in *"$k"*) ;; *) echo "FAIL $src $mode gop$gop: missing '$k' in: $info"; fail=1 ;; esac
      done
      ffmpeg -v error -y -i "$bs" -pix_fmt yuv420p10le -f rawvideo "$dec"
      if cmp -s "$rec" "$dec"; then echo "ok   $src $mode gop$gop: $info"; else echo "FAIL $src $mode gop$gop: ffmpeg != recon"; fail=1; fi
    done
  done
done
exit $fail
