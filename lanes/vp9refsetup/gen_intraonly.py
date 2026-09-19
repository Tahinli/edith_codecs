#!/usr/bin/env python3
"""Turn an ffmpeg VP9 keyframe GOP into the intra-only witness.

Re-heads frame 0 (a visible keyframe) as an INTRA-ONLY inter frame
(frame_type=1, show_frame=0, intra_only=1, reset_frame_context=2,
refresh_frame_flags=0xff) and keeps every later frame as-is: the intra-only
frame must reconstruct exactly like the keyframe it replaces, so the whole GOP
decodes to the same shown frames.

reset_frame_context=2 matters: `vp9_setup_past_independence` resets the
selected stored context to the defaults, and the keyframe's compressed header
was coded against the defaults. With reset 0/1 the frame would read whatever
context the previous frame stored and the bool stream desyncs.

usage: gen_intraonly.py <keyframe-gop.ivf> <out.ivf>
The keyframe uncompressed header (profile 0, 8-bit, 4:2:0, error_resilient=0)
is 8 bits + 24 sync + 3 color_space + 1 color_range => the render flag sits at
bit 68 and the fields from bit 69 on (loop filter, quant, segmentation, tile
info, header_size) are copied verbatim. The new prefix is 76 bits, so the
field bits shift by 7 and the compressed header + tiles (orig[U:], with
U = ceil(E/8)) follow the re-padded header. E was found by trying every byte
offset until libvpx accepted the frame; the committed fixture's E is 140.
"""
import struct, sys

src, dst = sys.argv[1], sys.argv[2]
d = open(src, 'rb').read()
off, frames = 32, []
while off + 12 <= len(d):
    sz = struct.unpack('<I', d[off:off+4])[0]
    frames.append((d[off+4:off+12], d[off+12:off+12+sz]))
    off += 12 + sz

def bits_of(b):
    return [(x >> i) & 1 for x in b for i in range(7, -1, -1)]

K = frames[0][1]
kb = bits_of(K)
assert kb[5] == 0 and kb[6] == 1 and kb[7] == 0, "expected a visible, non-ER keyframe"
rf = kb[68]
p1 = 69 if rf == 0 else 101
prefix = kb[0:5] + [1, 0, kb[7], 1] + [1, 0] + kb[8:32] + [1] * 8 + kb[36:52] + kb[52:68] + [rf]
if rf:
    prefix += kb[69:101]
assert len(prefix) == 76

E = 140  # committed fixture; any E whose (E+7)>>3 == 18 also decodes
post = kb[p1:E]
nb = prefix + post
while len(nb) % 8:
    nb.append(0)
hdr = bytearray()
for i in range(0, len(nb), 8):
    v = 0
    for j in range(8):
        v = (v << 1) | nb[i+j]
    hdr.append(v)
Kp = bytes(hdr) + K[(E + 7) >> 3:]

out = bytearray(b'DKIF' + b'\0' * 28)
out[6:8] = struct.pack('<H', 32)
out[8:12] = b'VP90'
out[12:14] = struct.pack('<H', 320)
out[14:16] = struct.pack('<H', 240)
out[16:20] = struct.pack('<I', 24)
out[20:24] = struct.pack('<I', 1)
out[24:28] = struct.pack('<I', len(frames) + 1)
out += struct.pack('<I', len(K)) + frames[0][0] + K
out += struct.pack('<I', len(Kp)) + frames[0][0] + Kp
for ts, f in frames[1:]:
    out += struct.pack('<I', len(f)) + ts + f
open(dst, 'wb').write(out)
print("wrote", dst)
