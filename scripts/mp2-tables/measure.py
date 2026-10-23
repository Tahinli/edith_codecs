#!/usr/bin/env python3
"""Measures the Layer II bit-allocation tables (ISO 11172-3 Table 3-B.2a..d and the
ISO 13818-3 LSF table) out of an oracle decoder instead of copying them from anyone.

Method: a mono frame whose only non-zero bits are the allocation code `c` of subband
`s` decodes (all sample bits zero) to a constant -(L-1)/L * scalefactor in that subband,
L being the number of quantiser levels the code selects. Layer I's allocation is known
from the standard (nb = a+1 bits, L = 2^nb - 1), so a Layer I frame with allocation 14
in the same subband calibrates the oracle's gain for that subband. The ratio of the two
steady-state energies gives L. The per-subband bit widths (nbal) were found by setting one allocation bit at a
time and watching where the oracle stops reacting (a constant subband input only
excites lines at multiples of fs/32, so adjacent subbands pair up in frequency, and the
run lengths of the pairs give the widths).

Usage: measure.py > table.txt   (needs ffmpeg with the mp2 decoder)
"""
import math, os, struct, subprocess, sys, tempfile

SF = 12  # 2^(1-12/3) = 0.125, keeps the output clear of clipping
TMP = os.environ.get("TMPDIR", "/tmp")

def nbal_for(table):
    return {
        0: [4]*11 + [3]*12 + [2]*4,
        1: [4]*11 + [3]*12 + [2]*7,
        2: [4]*2 + [3]*6,
        3: [4]*2 + [3]*10,
        4: [4]*4 + [3]*7 + [2]*19,
    }[table]

# (version byte1, rate idx, bitrate idx, rate Hz) giving each table in a mono stream
CFG = {
    0: (0xFD, 1, 4, 48000),   # 48k 64k mono -> table 0
    1: (0xFD, 0, 8, 44100),   # 44.1k 128k mono -> table 1
    2: (0xFD, 0, 1, 44100),   # 44.1k 32k mono -> table 2
    3: (0xFD, 2, 1, 32000),   # 32k 32k mono -> table 3
    4: (0xF5, 0, 8, 22050),   # MPEG-2 22.05k 64k mono -> LSF table
}

def frame_len(table):
    b1, r, br, hz = CFG[table]
    kbps = {4: 64, 0: 64, 1: 128, 2: 32, 3: 32}[table]
    return 144 * kbps * 1000 // hz

class Bits:
    def __init__(self, n): self.b = bytearray(n); self.p = 0
    def put(self, v, n):
        for i in range(n - 1, -1, -1):
            if (v >> i) & 1: self.b[self.p >> 3] |= 0x80 >> (self.p & 7)
            self.p += 1

def l2_frame(table, sb, code):
    b1, r, br, hz = CFG[table]
    n = frame_len(table)
    f = Bits(n)
    f.put(0xFF, 8); f.put(b1, 8); f.put((br << 4) | (r << 2), 8); f.put(0xC0, 8)
    for i, nb in enumerate(nbal_for(table)):
        f.put(code if i == sb else 0, nb)
    if code:
        f.put(0, 2)           # scfsi 0: three scalefactors
        for _ in range(3): f.put(SF, 6)
    return bytes(f.b)

def l1_frame(sb):
    f = Bits(192)
    f.put(0xFF, 8); f.put(0xFF, 8); f.put((6 << 4) | (1 << 2), 8); f.put(0xC0, 8)
    for i in range(32):
        f.put(14 if i == sb else 0, 4)
    f.put(SF, 6)
    return bytes(f.b)

def decode(frames, n):
    path = os.path.join(TMP, "mp2probe.mp2")
    with open(path, "wb") as fh: fh.write(frames * n)
    out = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-"],
                         capture_output=True, check=True).stdout
    return struct.unpack("<%df" % (len(out) // 4), out)

def window(x):
    return x[4 * 1152:5 * 1152]

def energy(w): return sum(v * v for v in w)

def goertzel_frac(w, hz, sb):
    f = (sb + 0.5) / 64.0  # cycles per sample, for a stream sampled at hz
    re = sum(v * math.cos(2 * math.pi * f * i) for i, v in enumerate(w))
    im = sum(v * math.sin(2 * math.pi * f * i) for i, v in enumerate(w))
    return (re * re + im * im) * 2 / len(w) / max(energy(w), 1e-30)

K = {}
def gain(sb):
    if sb not in K:
        w = window(decode(l1_frame(sb), 36))
        K[sb] = math.sqrt(energy(w)) / (1 - 1 / 32767.0)  # per unit of |v|, scalefactor included
    return K[sb]

LEVELS = [3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535]

for table in range(5):
    nbal = nbal_for(table)
    print("table", table, "sblimit", len(nbal))
    for sb in range(len(nbal)):
        row = []
        for code in range(1 << nbal[sb]):
            if code == 0:
                row.append(0); continue
            w = window(decode(l2_frame(table, sb, code), 8))
            e = energy(w)
            if e == 0:
                row.append(-1); continue
            frac = goertzel_frac(w, CFG[table][3], sb)
            v = math.sqrt(e) / gain(sb)
            lv = 1.0 / (1.0 - v) if 0 < v < 1 else 1e-9
            near = min(LEVELS, key=lambda L: abs(math.log(L / lv)))
            ok = abs(math.log(near / lv)) < 0.3
            row.append(near if ok else -(int(lv)))
        print(" sb", sb, nbal[sb], row)
    sys.stdout.flush()
