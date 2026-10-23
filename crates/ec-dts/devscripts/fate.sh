#!/usr/bin/env bash
# Fetch the public FFmpeg FATE DTS samples (dcadec conformance suite) into $1.
set -e
d=${1:-$HOME/dts-fate}
mkdir -p "$d"
cd "$d"
base=https://fate-suite.ffmpeg.org/dts
for f in dcadec-suite/core_51_24_48_768_0.dtshd dcadec-suite/core_51_24_48_768_1.dtshd \
  dcadec-suite/x96_51_24_96_1509.dtshd dcadec-suite/x96_xch_61_24_96_3840.dtshd \
  dcadec-suite/xbr_51_24_48_3840.dtshd dcadec-suite/xch_61_24_48_768.dtshd \
  dcadec-suite/xll_51_24_48_768.dtshd dcadec-suite/xll_51_16_192_768_0.dtshd \
  dts_es.dts master_audio_7.1_24bit.dts; do
  b=$(basename "$f")
  [ -s "$b" ] || curl -sfO "$base/$f"
done
ls "$d"
