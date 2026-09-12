#!/usr/bin/env python3
"""Dixie-faithful full key-frame VP8 decode model (modes + tokens, no
pixels) with per-decision tracing, interleaved per MB row exactly like
the reference decoder. Diff its `B` lines against the Rust decoder's
EC_VP8_TRACE output.

Usage: python3 vp8ref.py frame.ivf
"""
import re
import sys

TABLES_RS = "crates/ec-vp8/src/tables.rs"


class BD:
    def __init__(self, data):
        self.data = data
        self.pos = 2 if len(data) >= 2 else len(data)
        v = 0
        if len(data) >= 1:
            v = data[0] << 8
        if len(data) >= 2:
            v |= data[1]
        self.value = v
        self.range = 255
        self.bit_count = 0

    def bool(self, prob):
        global DEC
        DEC += 1
        split = 1 + (((self.range - 1) * prob) >> 8)
        big = split << 8
        if self.value >= big:
            self.range -= split
            self.value -= big
            bit = 1
        else:
            self.range = split
            bit = 0
        while self.range < 128:
            self.value <<= 1
            self.range <<= 1
            self.bit_count += 1
            if self.bit_count == 8:
                self.bit_count = 0
                if self.pos < len(self.data):
                    self.value |= self.data[self.pos]
                    self.pos += 1
        print(f"B {self.pos} {self.bit_count} {prob} {bit}")
        return bit

    def lit(self, n):
        v = 0
        for _ in range(n):
            v = (v << 1) + self.bool(128)
        return v

    def maybe_int(self, n):
        if not self.bool(128):
            return 0
        v = self.lit(n)
        return -v if self.bool(128) else v

    def tree(self, tree, probs):
        i = 0
        while True:
            b = self.bool(probs[i >> 1])
            i = tree[i + b]
            if i <= 0:
                return -i


def table(name):
    src = open(TABLES_RS).read()
    m = re.search(rf"pub const {name}: [^=]+= ", src)
    body = src[m.end(): src.index("];", m.end())]
    return [int(x) for x in re.findall(r"-?\d+", body)]


DEC = 0


def main():
    CUP = table("COEFF_UPDATE_PROBS")
    KF_BMODE = table("KF_BMODE_PROB")
    BANDS = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0]
    ZIGZAG = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15]
    CAT_EXTRA = [
        [173, 148, 140],
        [176, 155, 140, 135],
        [180, 157, 141, 134, 130],
        [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
    ]
    KF_YMODE_TREE = [-4, 2, 4, 6, 0, -1, -2, -3]
    BMODE_TREE = [0, 2, -1, 4, -2, 6, 8, 12, -3, 10, -5, -6, -4, 14, -7, 16, -8, -9]
    UV_MODE_TREE = [0, 2, -1, 4, -2, -3]
    YMODE_MAP = {0: 0, 1: 2, 2: 3, 3: 1}  # 16x16 mode -> bmode context
    LEFT = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8]
    ABOVE = [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 8]

    raw = open(sys.argv[1], "rb").read()
    sz = int.from_bytes(raw[32:36], "little")
    frame = raw[44 : 44 + sz]
    tag = int.from_bytes(frame[0:3], "little")
    part0_sz = tag >> 5
    w = int.from_bytes(frame[6:8], "little") & 0x3FFF
    h = int.from_bytes(frame[8:10], "little") & 0x3FFF
    cols, rows = (w + 15) // 16, (h + 15) // 16

    d = BD(frame[10 : 10 + part0_sz])
    d.lit(2)  # colour space + clamping
    # segmentation
    if d.bool(128):
        d.bool(128)
        if d.bool(128):
            d.bool(128)
            for _ in range(4):
                d.maybe_int(7)
            for _ in range(4):
                d.maybe_int(6)
    # loop filter header
    d.lit(1)
    d.lit(6)
    d.lit(3)
    if d.bool(128):
        if d.bool(128):
            for _ in range(4):
                d.maybe_int(6)
            for _ in range(4):
                d.maybe_int(6)
    # token partitions
    log2 = d.lit(2)
    nparts = 1 << log2
    off = 10 + part0_sz
    sizes = []
    for i in range(nparts):
        if i < nparts - 1:
            sizes.append(int.from_bytes(frame[off : off + 3], "little"))
            off += 3
        else:
            sizes.append(len(frame) - off)
    parts = []
    po = off
    for i in range(nparts):
        parts.append(frame[po : po + sizes[i]])
        po += sizes[i]
    # quant
    d.lit(7)
    for _ in range(5):
        d.maybe_int(4)
    # reference header: refresh_entropy is read on key frames too
    d.bool(128)
    # coefficient probability updates
    probs = table("DEFAULT_COEFF_PROBS")
    idx = 0
    for _i in range(4):
        for _j in range(8):
            for _k in range(3):
                for _l in range(11):
                    if d.bool(CUP[idx]):
                        probs[idx] = d.lit(8)
                    idx += 1
    # mb_no_skip_coeff
    skip_enabled = d.bool(128)
    prob_skip = d.lit(8) if skip_enabled else 128

    # per-MB state; row 0 / col W are the DC_PRED border
    mbmode = [[0] * (cols + 1) for _ in range(rows + 1)]
    bmode = [[[0] * 16 for _ in range(cols + 1)] for _ in range(rows + 1)]
    skips = [[False] * (cols + 1) for _ in range(rows + 1)]
    above_ctx = [[0] * 9 for _ in range(cols)]

    DEC = [0]
    def get_coeffs(ptype, n0, ctx, out):
        base = ptype * 264
        n = n0
        p = base + BANDS[n0] * 33 + ctx * 11
        DEC[0] += 1
        if DEC[0] == 2563:
            print(f"# at2563: ptype={ptype} n0={n0} n={n} band={BANDS[n]} ctx={ctx} nodeoff=0 entry={p} val={probs[p]}")
        if not d2.bool(probs[p]):
            return 0
        while True:
            n += 1
            DEC[0] += 1
            if DEC[0] == 2563:
                print(f"# at2563: ptype={ptype} n={n} band={BANDS[n]} nodeoff=1 entry={p+1} val={probs[p+1]}")
            if not d2.bool(probs[p + 1]):
                p = base + BANDS[n] * 33
            else:
                DEC[0] += 1
                if DEC[0] == 2563:
                    print(f"# at2563: nodeoff=2 n={n} band={BANDS[n]} ctx={ctx} p={p}")
                if not d2.bool(probs[p + 2]):
                    p = base + BANDS[n] * 33 + 11
                    v = 1
                else:
                    if not d2.bool(probs[p + 3]):
                        if not d2.bool(probs[p + 4]):
                            v = 2
                        else:
                            v = 3 + d2.bool(probs[p + 5])
                    else:
                        if not d2.bool(probs[p + 6]):
                            if not d2.bool(probs[p + 7]):
                                v = 5 + d2.bool(159)
                            else:
                                v = 7 + 2 * d2.bool(165)
                                v += d2.bool(145)
                        else:
                            bit1 = d2.bool(probs[p + 8])
                            bit0 = d2.bool(probs[p + 9 + bit1])
                            cat = 2 * bit1 + bit0
                            v = 0
                            for prob in CAT_EXTRA[cat]:
                                v += v + d2.bool(prob)
                            v += 3 + (8 << cat)
                    p = base + BANDS[n] * 33 + 22
                out[ZIGZAG[n - 1]] = -v if d2.bool(128) else v
                if n == 16 or not d2.bool(probs[p]):
                    return n
            if n == 16:
                return 16

    for r in range(1, rows + 1):
        d2 = BD(parts[(r - 1) % nparts])
        left_ctx = [0] * 9
        # phase 1: every mode record of the row (partition 0)
        for c in range(1, cols + 1):
            print(f"M {r-1} {c-1}")
            if skip_enabled:
                skips[r][c] = bool(d.bool(prob_skip))
            ym = d.tree(KF_YMODE_TREE, [145, 156, 163, 128])
            mbmode[r][c] = ym
            if ym == 4:
                for j in range(16):
                    if j < 4:
                        am = mbmode[r - 1][c]
                        a = bmode[r - 1][c][j + 12] if am == 4 else YMODE_MAP[am]
                    else:
                        a = bmode[r][c][j - 4]
                    if j % 4 == 0:
                        lm = mbmode[r][c - 1]
                        l = bmode[r][c - 1][j + 3] if lm == 4 else YMODE_MAP[lm]
                    else:
                        l = bmode[r][c][j - 1]
                    bmode[r][c][j] = d.tree(BMODE_TREE, KF_BMODE[(a * 10 + l) * 9 : (a * 10 + l) * 9 + 9])
            d.tree(UV_MODE_TREE, [142, 114, 183])
        # phase 2: tokens of the row (partition r-1 % n)
        for c in range(1, cols + 1):
            ac = above_ctx[c - 1]
            hy2 = mbmode[r][c] != 4
            if skips[r][c]:
                for i in range(8):
                    ac[i] = 0
                    left_ctx[i] = 0
                if hy2:
                    ac[8] = 0
                    left_ctx[8] = 0
                continue

            def one(blkidx, ptype, n0):
                o = [0] * 16
                ctxv = min(2, left_ctx[LEFT[blkidx]] + ac[ABOVE[blkidx]])
                print(f"T {r-1} {c-1} {blkidx} {ptype} {ctxv}")
                eob = get_coeffs(ptype, n0, ctxv, o)
                t = 1 if eob > 0 else 0
                left_ctx[LEFT[blkidx]] = t
                ac[ABOVE[blkidx]] = t

            if hy2:
                one(24, 1, 0)
                for b in range(16):
                    one(b, 0, 1)
            else:
                for b in range(16):
                    one(b, 3, 0)
            for b in range(4):
                one(16 + b, 2, 0)
            for b in range(4):
                one(20 + b, 2, 0)


main()
