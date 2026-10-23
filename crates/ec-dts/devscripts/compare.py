#!/usr/bin/env python3
"""Compare ec-dts output (f32le, interleaved, 24-bit scaled by 2^-23) against
an ffmpeg decode (s32le or f32le, interleaved).

usage: compare.py ours.f32 ref.raw nch kind [perm [skip]]
  kind: s32 (ffmpeg -flags +bitexact, 24-bit << 8) or f32 (ffmpeg default float)
  perm: comma list, for each reference channel the index of our channel
  skip: samples per channel ours leads the reference by (ffmpeg trims the
        start of .dtshd files by 1024)
Exit status 0 when every sample is identical.
"""
import sys
from array import array

nch = int(sys.argv[3])
kind = sys.argv[4]
perm = [int(x) for x in sys.argv[5].split(",")] if len(sys.argv) > 5 and sys.argv[5] else list(range(nch))
ours = array("f")
with open(sys.argv[1], "rb") as f:
    ours.frombytes(f.read())
ref = array("i" if kind == "s32" else "f")
with open(sys.argv[2], "rb") as f:
    ref.frombytes(f.read())
skip = int(sys.argv[6]) if len(sys.argv) > 6 else 0
ours = ours[skip * nch:]
n = len(ours) // nch
m = len(ref) // nch
k = min(n, m)
mx = [0] * nch
nz = [0] * nch
for i in range(k):
    for c in range(nch):
        o = round(ours[i * nch + perm[c]] * 8388608.0)
        r = ref[i * nch + c]
        r = (r >> 8) if kind == "s32" else round(r * 8388608.0)
        d = abs(o - r)
        if d:
            nz[c] += 1
            if d > mx[c]:
                mx[c] = d
print(f"samples/ch ours={n} ref={m} compared={k} ch={nch}")
print(f"max abs diff (24-bit LSB) = {max(mx)}; mismatching samples = {sum(nz)} of {k * nch}")
print("  per channel max:", mx, "nonzero:", nz)
sys.exit(0 if max(mx) == 0 and abs(n - m) < 1 else 1)
