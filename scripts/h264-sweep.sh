#!/usr/bin/env bash
# h264-sweep.sh "<ENV=.. ENV=..>" : speed (60 frames, 1 thread) and BD-rate for one knob setting.
# Run on one host: scripts/remote-run.sh h264encspeed2 --host a -- bash scripts/h264-sweep.sh "EC_H264_ME_SUB=1"
set -uo pipefail
for kv in $1; do export "$kv"; done
echo "### $1"
bash scripts/h264-speed.sh 60 1 2>&1 | grep -E "^(==|frames)"
bash scripts/h264-bd.sh 2>&1 | grep -E "bd_rate|BD-PSNR-YUV"
