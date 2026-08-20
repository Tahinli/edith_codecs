#!/usr/bin/env bash
# Deterministic HE-AAC v1 fixture matrix for tests/sbr_real_library.rs's
# `synthetic_heaac_matrix`: two sample-rate families (48000, 44100) x four
# bitrates (32k/48k/64k/96k) x two profiles (aac_he, aac_low control) = 16
# .m4a files, sweeping the SBR crossover (kx/k2) that bitrate controls
# without depending on any one real file's content.
#
# `ffmpeg`'s system libfdk_aac (the `fdk-aac-free` package) has SBR/PS
# encoding compiled out (patent-restricted tools) -- `-profile:a aac_he`
# fails with "Unable to set the AOT 5: Invalid config" against it. RPM
# Fusion's full `fdk-aac` package carries the SBR-capable encoder but
# installs to its OWN /usr/lib64/fdk-aac dir specifically so it doesn't
# clobber the system one; this script fetches (no root) and extracts just
# that .so into a local cache and points ffmpeg at it via
# LD_LIBRARY_PATH for the encode step only, never touching the system
# library.
set -euo pipefail

SCRATCH="${EC_AAC_HEAAC_FIXTURES:-$(cd "$(dirname "$0")/../.." && pwd)/.cache/heaac-fixtures}"
mkdir -p "$SCRATCH"
CACHE="$SCRATCH/.fdk-aac-full"
mkdir -p "$CACHE"

fdk_libdir="$CACHE/usr/lib64/fdk-aac"
if [ ! -f "$fdk_libdir/libfdk-aac.so.2" ]; then
    echo "fetching SBR-capable libfdk-aac (no root, local cache only)..."
    ( cd "$CACHE" && dnf download fdk-aac.x86_64 )
    rpm_file=$(find "$CACHE" -maxdepth 1 -name 'fdk-aac-*.rpm' | head -1)
    ( cd "$CACHE" && rpm2cpio "$rpm_file" | cpio -idm --quiet )
fi
export LD_LIBRARY_PATH="$fdk_libdir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# Three summed sweeps 100 Hz -> 18 kHz over the full 30 s (phase(t) =
# 2*pi*(f0*t + (f1-f0)/(2T)*t^2), f0=100 f1=18000 T=30 ->
# (f1-f0)/(2T)=298.333), offset in phase per partial and per channel so
# left/right are correlated but not identical -- plus low-level pink noise
# (seeded, so every run of this script reproduces bit-identical fixtures).
sweep() { echo "0.15*sin(2*PI*(100*t+298.333*t*t)+$1)+0.15*sin(2*PI*(100*t+298.333*t*t)+$2)+0.15*sin(2*PI*(100*t+298.333*t*t)+$3)"; }
L=$(sweep 0 1.047 2.094)
R=$(sweep 0.5 1.571 2.618)

synth_source() {
    local rate=$1 out=$2
    ffmpeg -y -hide_banner -loglevel error -filter_complex "
      aevalsrc=exprs='${L}|${R}':channel_layout=stereo:sample_rate=${rate}:duration=30 [tone];
      anoisesrc=color=pink:amplitude=0.05:seed=42:sample_rate=${rate}:duration=30 [n0];
      anoisesrc=color=pink:amplitude=0.05:seed=43:sample_rate=${rate}:duration=30 [n1];
      [n0][n1] amerge=inputs=2 [noise];
      [tone][noise] amix=inputs=2:normalize=0 [out]
    " -map "[out]" -ar "$rate" -sample_fmt s16 -f wav "$out"
}

for rate in 48000 44100; do
    src="$SCRATCH/src_${rate}.wav"
    [ -f "$src" ] || synth_source "$rate" "$src"
    for br in 32k 48k 64k 96k; do
        he="$SCRATCH/heaac_${rate}_${br}.m4a"
        lc="$SCRATCH/lc_${rate}_${br}.m4a"
        [ -f "$he" ] || ffmpeg -y -hide_banner -loglevel error -i "$src" \
            -c:a libfdk_aac -profile:a aac_he -b:a "$br" "$he"
        [ -f "$lc" ] || ffmpeg -y -hide_banner -loglevel error -i "$src" \
            -c:a libfdk_aac -profile:a aac_low -b:a "$br" "$lc"
    done
done

echo "fixtures in $SCRATCH:"
ls "$SCRATCH"/*.m4a
