#!/usr/bin/env bash
# Regenerates the tiny committed fixtures and their expectations from synthetic sources
# (testsrc2 and sine only). Needs ffmpeg with libx264 and libx265.
#   expected.txt: <file> <stream-index-in-ffmpeg> <codec> <packets> <bytes> <fnv1a64 of ffmpeg's bytes>
# LPCM is hashed over ffmpeg's s16be decode, which is what the demuxer's packets are.
set -euo pipefail
cd "$(dirname "$0")"
V=(-f lavfi -i testsrc2=size=160x120:rate=25:duration=0.6)
A=(-f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.6)
X=(-c:v libx264 -preset ultrafast -g 12 -bf 2)
ffmpeg -v error -y "${V[@]}" "${A[@]}" "${X[@]}" -c:a aac -f mpegts tiny_h264_aac.ts
ffmpeg -v error -y "${V[@]}" "${A[@]}" "${X[@]}" -c:a ac3 -f mpegts -mpegts_m2ts_mode 1 tiny_h264_ac3.m2ts
ffmpeg -v error -y "${V[@]}" "${A[@]}" "${X[@]}" -c:a eac3 -f mpegts tiny_h264_eac3.ts
ffmpeg -v error -y -f lavfi -i testsrc2=size=160x120:rate=25:duration=0.3 -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.3 "${X[@]}" -c:a pcm_bluray -f mpegts -mpegts_m2ts_mode 1 tiny_h264_lpcm.m2ts
ffmpeg -v error -y "${V[@]}" "${A[@]}" -c:v libx265 -preset ultrafast -g 12 -x265-params log-level=none -c:a aac -f mpegts tiny_hevc_aac.ts
python3 -I - <<'PY'
import subprocess
def fnv(b):
    h = 0xcbf29ce484222325
    for x in b:
        h = ((h ^ x) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h
rows = []
for f in ["tiny_h264_aac.ts", "tiny_h264_ac3.m2ts", "tiny_h264_eac3.ts", "tiny_h264_lpcm.m2ts", "tiny_hevc_aac.ts"]:
    for idx, codec in enumerate(["video", "audio"]):
        if codec == "audio" and "lpcm" in f:
            data = subprocess.run(["ffmpeg","-v","error","-i",f,"-map","0:1","-f","s16be","-"],capture_output=True,check=True).stdout
        else:
            data = subprocess.run(["ffmpeg","-v","error","-i",f,"-map",f"0:{idx}","-c","copy","-f","data","-"],capture_output=True,check=True).stdout
        pk = subprocess.run(["ffprobe","-v","error","-select_streams",str(idx),"-show_entries","packet=pts","-of","csv=p=0",f],capture_output=True,check=True).stdout.split()
        rows.append(f"{f} {idx} {len(pk)} {len(data)} {fnv(data):016x}")
open("expected.txt","w").write("\n".join(rows) + "\n")
PY
