#!/usr/bin/env bash
# Deterministic HE-AAC v1 (SBR) and v2 (PS) fixtures for crates/ec-aac/tests/it/he_parity.rs.
# Each fixture is written as ADTS (.aac) and MP4 (.m4a). The reference is FFmpeg's own decode
# of the ADTS file, as signed 16-bit little-endian PCM at the decoded rate, in FFmpeg's own
# channel count and order (`ffmpeg -i x.aac -f s16le`). s16 keeps 5.1 under the 300 KB cap.
#
# libfdk_aac in the system ffmpeg may need the full fdk-aac library for SBR/PS encoding:
# pass its directory as $1 (it is prepended to LD_LIBRARY_PATH). Pass "" to use the system one.
# Usage: scripts/aac-tables/make-he-fixtures.sh <fdk-lib-dir-or-empty> <out-dir>
set -euo pipefail
LIBDIR=${1:-}; OUT=$2; mkdir -p "$OUT"
if [ -n "$LIBDIR" ]; then
  export LD_LIBRARY_PATH="$LIBDIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
FF="ffmpeg -v error -y -hide_banner"
RATE=22050

# Mono: a tone plus a chirp (the chirp exercises the SBR patch range), with pink noise above it.
MONO="aevalsrc=0.35*sin(2*PI*220*t)+0.15*sin(2*PI*(300*t+700*t*t)):d=2:s=$RATE[t];anoisesrc=d=2:c=pink:a=0.12:r=$RATE:seed=7[n];[t][n]amix=inputs=2:normalize=0[a]"

# Stereo: each side a different tone, chirp and noise, so a swapped channel fails the parity test.
STEREO="aevalsrc=0.3*sin(2*PI*330*t)+0.1*sin(2*PI*(500*t+600*t*t)):d=2:s=$RATE[l];aevalsrc=0.3*sin(2*PI*470*t)+0.1*sin(2*PI*(900*t+400*t*t)):d=2:s=$RATE[r];anoisesrc=d=2:c=pink:a=0.1:r=$RATE:seed=11[n];[l][r][n]amerge=inputs=3,pan=stereo|c0=c0+0.6*c2|c1=c1+0.4*c2[a]"

# 5.1 (0.6 s): one distinct tone per channel plus pink noise on the rear pair, in FL FR FC LFE BL BR.
# The LFE tone is 40 Hz: real LFE content sits below 120 Hz, and a tone at the encoder cutoff is ill-conditioned.
SURROUND="aevalsrc=0.25*sin(2*PI*200*t):d=1:s=$RATE[c0];aevalsrc=0.25*sin(2*PI*300*t):d=1:s=$RATE[c1];aevalsrc=0.25*sin(2*PI*400*t):d=1:s=$RATE[c2];aevalsrc=0.4*sin(2*PI*40*t):d=1:s=$RATE[c3];aevalsrc=0.25*sin(2*PI*(500*t+400*t*t)):d=1:s=$RATE[c4];aevalsrc=0.25*sin(2*PI*(650*t+300*t*t)):d=1:s=$RATE[c5];anoisesrc=d=1:c=pink:a=0.08:r=$RATE:seed=13[n];[c0][c1][c2][c3][c4][c5][n]amerge=inputs=7,pan=5.1|c0=c0+0.5*c6|c1=c1+0.5*c6|c2=c2|c3=c3|c4=c4+0.3*c6|c5=c5+0.3*c6,channelmap=channel_layout=5.1[a]"

encode() { # name, filter graph (labelled [a]), channels, profile, bitrate, duration
  local name=$1 filter=$2 ch=$3 profile=$4 br=$5 dur=$6
  $FF -filter_complex "$filter" -map "[a]" -t "$dur" -ac "$ch" -c:a libfdk_aac -profile:a "$profile" -b:a "$br" -ar "$RATE" -f adts "$OUT/$name.aac"
  $FF -i "$OUT/$name.aac" -c:a copy -f ipod "$OUT/$name.m4a"
  $FF -i "$OUT/$name.aac" -f s16le -acodec pcm_s16le "$OUT/$name.s16"
}

encode he_v1_mono "$MONO" 1 aac_he 32k 2
encode he_v1_stereo "$STEREO" 2 aac_he 48k 2
encode he_v1_51 "$SURROUND" 6 aac_he 160k 0.6
encode he_v2_stereo "$STEREO" 2 aac_he_v2 32k 2
