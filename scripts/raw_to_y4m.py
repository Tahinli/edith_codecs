#!/usr/bin/env python3
"""Wrap raw planar yuv420p frames into a minimal y4m (for vpxenc).

    ffmpeg ... -f rawvideo - | raw_to_y4m.py W H FPS out.y4m
"""
import sys

w, h, fps, out = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
frame = int(w) * int(h) * 3 // 2
raw = sys.stdin.buffer.read()
with open(out, "wb") as f:
    f.write(f"YUV4MPEG2 W{w} H{h} F{fps}:1 Ip A1:1 C420jpeg\n".encode())
    for i in range(len(raw) // frame):
        f.write(b"FRAME\n")
        f.write(raw[i * frame : (i + 1) * frame])
