#!/usr/bin/env bash
# Regenerates the tiny program-stream fixtures and their expectations from synthetic
# sources (testsrc2 and sine only). Needs ffmpeg with mpeg2video, mpeg1video, ac3, mp2.
#   expected_ps.txt: <file> <stream-index-in-ffmpeg> <packets> <bytes> <fnv1a64 of ffmpeg's bytes>
set -euo pipefail
cd "$(dirname "$0")"
V=(-f lavfi -i testsrc2=size=128x96:rate=25:duration=0.5)
A=(-f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5)
ffmpeg -v error -y "${V[@]}" "${A[@]}" -c:v mpeg2video -g 12 -bf 2 -b:v 150k -c:a ac3 -b:a 96k -f vob ps_mpeg2_ac3.vob
ffmpeg -v error -y "${V[@]}" "${A[@]}" -c:v mpeg1video -g 9 -bf 2 -b:v 120k -c:a mp2 -b:a 96k -f mpeg ps_mpeg1_mp2.mpg
ffmpeg -v error -y "${V[@]}" -c:v mpeg2video -g 12 -bf 2 -b:v 150k -f mpeg2video ps_mpeg2.m2v
ffmpeg -v error -y "${V[@]}" -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5 -c:v mpeg2video -g 12 -b:v 150k -c:a pcm_dvd -f vob ps_pcm_dvd.vob
python3 -I - <<'PY'
import subprocess
def fnv(b):
    h = 0xcbf29ce484222325
    for x in b:
        h = ((h ^ x) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h
rows = []
for f, streams in [("ps_mpeg2_ac3.vob", 2), ("ps_mpeg1_mp2.mpg", 2), ("ps_mpeg2.m2v", 1), ("ps_pcm_dvd.vob", 2)]:
    for idx in range(streams):
        data = subprocess.run(["ffmpeg","-v","error","-i",f,"-map",f"0:{idx}","-c","copy","-f","data","-"],capture_output=True,check=True).stdout
        if "pcm_dvd" in f and idx == 1:
            data = subprocess.run(["ffmpeg","-v","error","-i",f,"-map","0:1","-f","s16be","-"],capture_output=True,check=True).stdout
        pk = subprocess.run(["ffprobe","-v","error","-select_streams",str(idx),"-show_entries","packet=pts","-of","csv=p=0",f],capture_output=True,check=True).stdout.split()
        rows.append(f"{f} {idx} {len(pk)} {len(data)} {fnv(data):016x}")
open("expected_ps.txt","w").write("\n".join(rows) + "\n")
PY
