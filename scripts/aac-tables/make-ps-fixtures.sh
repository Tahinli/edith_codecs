#!/usr/bin/env bash
# NOTE: the FDK library is taken from the Fedora fdk-aac RPM (dnf download fdk-aac.x86_64,
# then rpm2cpio | cpio -idm into a scratch dir); it is not installed system-wide and no root is needed.
# Deterministic HE-AAC v2 (Parametric Stereo) fixtures for crates/ec-aac/tests/it/ps_oracle.rs.
# libfdk_aac in the system ffmpeg has SBR/PS encoding compiled out; the full fdk-aac
# library (RPM Fusion / Fedora fdk-aac package, extracted locally, no root) carries it.
# Usage: scripts/aac-tables/make-ps-fixtures.sh <fdk-lib-dir> <out-dir>
set -euo pipefail
LIBDIR=$1; OUT=$2; mkdir -p "$OUT"
export LD_LIBRARY_PATH="$LIBDIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
FF="ffmpeg -v error -y -hide_banner"
for rate in 44100 48000; do
  # Sine pair: distinct tones per channel (strong, simple PS content).
  $FF -f lavfi -i "sine=440:d=2:r=$rate" -f lavfi -i "sine=660:d=2:r=$rate" \
    -filter_complex "[0][1]join=inputs=2:channel_layout=stereo[a]" -map "[a]" \
    -c:a libfdk_aac -profile:a aac_he_v2 -b:a 32k -ar "$rate" "$OUT/ps_${rate}_sine.m4a"
  # Music-like: pink noise plus a chirp, mixed with a different gain per channel.
  $FF -f lavfi -i "anoisesrc=d=2:c=pink:a=0.25:r=$rate:seed=7" \
    -f lavfi -i "aevalsrc=0.4*sin(2*PI*(300*t+900*t*t)):d=2:s=$rate" \
    -filter_complex "[0][1]amerge=inputs=2,pan=stereo|c0=0.9*c0+0.6*c1|c1=0.4*c0+1.1*c1[a]" -map "[a]" \
    -c:a libfdk_aac -profile:a aac_he_v2 -b:a 40k -ar "$rate" "$OUT/ps_${rate}_music.m4a"
  $FF -i "$OUT/ps_${rate}_music.m4a" -c:a copy -f adts "$OUT/ps_${rate}_music.aac"
done
