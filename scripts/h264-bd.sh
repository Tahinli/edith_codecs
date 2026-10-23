#!/usr/bin/env bash
# h264-bd.sh: BD-rate of the Fast preset vs x264 veryfast on the 1080p library clip
# (the h264encbd method). Run through scripts/remote-run.sh. Prints bd_rate_pct and bd_psnr_db.
set -euo pipefail
d=${TMPDIR:-/tmp}/h264bd.$$; mkdir -p "$d"
EC_REQUIRE_FIXTURES=1 EC_H264_X264_PRESET=veryfast \
EC_H264_CLIP=$HOME/fixtures/video/h264-1080p-23.976-8bit.mp4 \
  cargo test --release -p ec-h264 --test it encode::bd_psnr_vs_x264 -- --nocapture 2>&1 | tee "$d/log" | grep -E "^(test |BD-)" || true
python3 -I - "$d" <<'PY'
import re,sys
d=sys.argv[1]
o=open(d+"/ours.tsv","w"); x=open(d+"/x264.tsv","w")
for l in open(d+"/log"):
    m=re.match(r"qp \d+: ours (\d+) bits ([\d.]+) dB \(([\d.]+) yuv\) \| x264 (\d+) bits ([\d.]+) dB \(([\d.]+) yuv\)",l)
    if m:
        o.write(f"{int(m[1])//8}\t{m[2]}\t{m[3]}\n"); x.write(f"{int(m[4])//8}\t{m[5]}\t{m[6]}\n")
PY
python3 -I scripts/bdrate.py --metric yuv "$d/x264.tsv" "$d/ours.tsv"
