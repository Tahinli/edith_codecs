#!/usr/bin/env python3
"""Wrap raw planar yuv420p frames into a minimal y4m (for vpxenc).

    ffmpeg ... -f rawvideo - | raw_to_y4m.py W H FPS out.y4m DEPTH

DEPTH (8, 10 or 12) is REQUIRED and is the number of BYTES per sample in the
raw stream on stdin, not the stream's bit depth: ffmpeg's rawvideo muxer
never packs to the bit depth, it uses 1 byte for `yuv420p` and 2 bytes for
`yuv420p10le` / `yuv420p12le`. This script has no parser and no way to learn
it, so the depth is an ASSERTED argument: it is checked against the stdin
length, and a mismatch is a hard error naming the depth, the byte count it
implied, and the misframed y4m it would have produced. The old version
computed `w*h*3//2` unconditionally, so a 10-bit stream (2 bytes per sample)
was cut into frames at half length, the chroma planes of frame N+1 were read
as the tail of frame N, and the last partial frame was dropped silently --
a y4m that vpxenc accepts and encodes, with no error anywhere.

The y4m header's `C` tag follows the same argument, so the wrapped stream
carries the depth it actually has.

LIMIT, stated because it cannot be fixed here: raw planar video carries NO
depth of its own, so the length check below can only catch a stream whose
byte count is not a whole number of frames at the claimed depth. A 10-bit
stream of N frames is exactly 2N 8-bit frames, and no length test can tell
those apart -- that case is caught only by the caller asserting the right
DEPTH, which is why DEPTH is required and not defaulted.
"""

import sys


if len(sys.argv) != 6:
    sys.exit(
        "usage: raw_to_y4m.py W H FPS out.y4m DEPTH   "
        "(DEPTH = 8|10|12, the BYTES per sample on stdin)"
    )
w, h, fps, out, depth = sys.argv[1:6]
try:
    depth = int(depth)
except ValueError:
    sys.exit(f"raw_to_y4m.py: DEPTH {depth!r} is not a number (8, 10 or 12)")
if depth not in (8, 10, 12):
    sys.exit(
        f"raw_to_y4m.py: DEPTH {depth} is not 8, 10 or 12 -- the frame size below would be "
        "wrong and every frame after the first would be misframed"
    )
bpp = 1 if depth == 8 else 2
frame = int(w) * int(h) * 3 // 2 * bpp
raw = sys.stdin.buffer.read()
if len(raw) % frame:
    sys.exit(
        f"raw_to_y4m.py: stdin is {len(raw)} bytes, not a whole number of "
        f"{int(w)}x{int(h)} 4:2:0 frames at {depth}-bit ({bpp} bytes per sample, "
        f"{frame} bytes each): {len(raw) % frame} trailing bytes. At the other depth a "
        f"frame would be {int(w) * int(h) * 3 // 2 * (3 - bpp)} bytes and each frame would "
        "carry half a frame of the next one's planes. Refusing to write a misframed y4m."
    )
ctag = {8: "C420jpeg", 10: "C420p10", 12: "C420p12"}[depth]
with open(out, "wb") as f:
    f.write(f"YUV4MPEG2 W{w} H{h} F{fps}:1 Ip A1:1 {ctag}\n".encode())
    for i in range(len(raw) // frame):
        f.write(b"FRAME\n")
        f.write(raw[i * frame : (i + 1) * frame])
