#!/usr/bin/env python3
"""Dixie-faithful multi-frame VP8 decode model (header + mode records +
tokens, no pixels) with per-read `B` and per-token-decision `G` tracing
that mirror the Rust decoder's EC_VP8_TRACE output byte-for-byte.

Usage: python3 vp8ref_model.py frame.ivf [max-frames]
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


# ---------------------------------------------------------------------------
# Inter-parse constants (RFC 6386; cross-checked against libvpx modecont.c /
# findnearmv.h / blockd.h — see managed skill ec-vp8-lane-recipe).
MODE_CONTEXTS = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
]
SUB_MV_REF_PROBS = [
    [147, 136, 18],
    [106, 145, 1],
    [179, 121, 1],
    [223, 1, 34],
    [208, 1, 1],
]
MV_PARTITION_TREE = [-3, 2, -2, 4, 0, -1]
MV_PARTITION_PROBS = [110, 111, 150]
SUB_MV_REF_TREE = [0, 2, -1, 4, -2, -3]
SMALL_MV_TREE = [2, 8, 4, 6, 0, -1, -2, -3, 10, 12, -4, -5, -6, -7]
MBSPLIT_OFFSET = [
    [0, 8], [0, 2], [0, 2, 8, 10], list(range(16)),
]
MBSPLIT_FILL_COUNT = [8, 8, 4, 1]
MBSPLIT_FILL_OFFSET = [
    list(range(16)),
    [0, 1, 4, 5, 8, 9, 12, 13, 2, 3, 6, 7, 10, 11, 14, 15],
    [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15],
    list(range(16)),
]


def main():
    CUP = table("COEFF_UPDATE_PROBS")
    KF_BMODE = table("KF_BMODE_PROB")
    DEF_COEF = table("DEFAULT_COEFF_PROBS")
    DEF_MV = table("DEFAULT_MV_PROBS")
    MV_UP = table("MV_UPDATE_PROBS")
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
    YMODE_TREE = [0, 2, 4, 6, -1, -2, -3, -4]
    YMODE_MAP = {0: 0, 1: 2, 2: 3, 3: 1}
    LEFT = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8]
    ABOVE = [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 8]

    raw = open(sys.argv[1], "rb").read()
    max_frames = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    frames = []
    pos = 32
    while pos + 12 <= len(raw):
        sz = int.from_bytes(raw[pos : pos + 4], "little")
        frames.append(raw[pos + 12 : pos + 12 + sz])
        pos += 12 + sz

    w = h = 0
    cols = rows = 0
    ctx = {
        "coef": DEF_COEF[:],
        "mv": DEF_MV[:],
        "ymode": [145, 156, 163, 128],
        "uv": [142, 114, 183],
    }
    probs = None
    seg_map = [0] * 65536

    for fi, frame in enumerate(frames[:max_frames]):
        tag = int.from_bytes(frame[0:3], "little")
        kf = (frame[0] & 1) == 0
        part0_sz = tag >> 5
        # Uncompressed data chunk: key frames carry start code + dims
        # (partition 0 at byte 10); inter frames start partition 0 at
        # byte 3 (RFC 6386 §9.1).
        hdr0 = 10 if kf else 3
        d = BD(frame[hdr0 : hdr0 + part0_sz])

        if kf:
            w = int.from_bytes(frame[6:8], "little") & 0x3FFF
            h = int.from_bytes(frame[8:10], "little") & 0x3FFF
            cols, rows = (w + 15) // 16, (h + 15) // 16
            d.lit(2)  # colour space + clamping
            ctx["coef"] = DEF_COEF[:]

        # segmentation
        seg_enabled = d.bool(128)
        seg_update_map = False
        seg_tree_probs = [255, 255, 255]
        if seg_enabled:
            seg_update_map = d.bool(128)
            if d.bool(128):
                d.bool(128)
                for _ in range(4):
                    d.maybe_int(7)
                for _ in range(4):
                    d.maybe_int(6)
        if seg_update_map:
            seg_tree_probs = [d.lit(8) for _ in range(3)]
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
        off = hdr0 + part0_sz
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

        # refresh header
        if kf:
            refresh_entropy = d.bool(128)
            sb = [False, False]
        else:
            refresh_gf = d.bool(128)
            if not refresh_gf:
                d.lit(2)
            refresh_arf = d.bool(128)
            if not refresh_arf:
                d.lit(2)
            sb = [False, d.bool(128), d.bool(128)]
            refresh_entropy = d.bool(128)
            d.bool(128)  # refresh_last
        snapshot = (ctx["coef"][:], ctx["mv"][:], ctx["ymode"][:], ctx["uv"][:])

        # coefficient probability updates
        idx = 0
        for _i in range(4):
            for _j in range(8):
                for _k in range(3):
                    for _l in range(11):
                        if d.bool(CUP[idx]):
                            ctx["coef"][idx] = d.lit(8)
                        idx += 1
        # mb_no_skip_coeff
        skip_enabled = d.bool(128)
        prob_skip = d.lit(8) if skip_enabled else 128

        probs = ctx["coef"]
        prob_intra = prob_last = prob_gf = 128
        if not kf:
            prob_intra = d.lit(8)
            prob_last = d.lit(8)
            prob_gf = d.lit(8)
            yflag = d.bool(128)
            print(f"HDR pi={prob_intra} pl={prob_last} pg={prob_gf} yflag={1 if yflag else 0}")
            if yflag:
                ctx["ymode"] = [d.lit(8) for _ in range(4)]
            uflag = d.bool(128)
            print(f"HDR uflag={1 if uflag else 0}")
            if uflag:
                ctx["uv"] = [d.lit(8) for _ in range(3)]
            for c in range(2):
                for j in range(19):
                    if d.bool(MV_UP[c * 19 + j]):
                        x = d.lit(7)
                        ctx["mv"][c * 19 + j] = (x << 1) if x else 1

        if not kf:
            print("YUV ymode =", ctx["ymode"], "uv =", ctx["uv"])
            print("MVT c0 =", ctx["mv"][0:19])
            print("MVT c1 =", ctx["mv"][19:38])

        # per-MB state (row/col 0 = border)
        mbmode = [[0] * (cols + 1) for _ in range(rows + 1)]
        bmode = [[[0] * 16 for _ in range(cols + 1)] for _ in range(rows + 1)]
        skips = [[False] * (cols + 1) for _ in range(rows + 1)]
        hasy2 = [[True] * (cols + 1) for _ in range(rows + 1)]
        mbref = [[0] * (cols + 1) for _ in range(rows + 1)]
        mbmv = [[(0, 0)] * (cols + 1) for _ in range(rows + 1)]
        mbsplit = [[False] * (cols + 1) for _ in range(rows + 1)]
        mbbmi = [[[(0, 0)] * 16 for _ in range(cols + 1)] for _ in range(rows + 1)]
        mbmvclamp = [[False] * (cols + 1) for _ in range(rows + 1)]
        above_ctx = [[0] * 9 for _ in range(cols)]

        def clamp_mv2(mv, edges):
            l, r, t, b = edges
            if mv[1] < l - 128:
                mv = (l - 128, mv[1])
            elif mv[1] > r + 128:
                mv = (r + 128, mv[1])
            if mv[0] < t - 128:
                mv = (t - 128, mv[1])
            elif mv[0] > b + 128:
                mv = (b + 128, mv[1])
            return mv

        def oob(mv, edges):
            l, r, t, b = edges
            return mv[1] < l or mv[1] > r or mv[0] < t or mv[0] > b

        def read_mv_component(p):
            if d.bool(p[0]):
                x = 0
                for i in range(3):
                    x += d.bool(p[9 + i]) << i
                for i in (8, 7, 6, 5, 4):
                    x += d.bool(p[9 + i]) << i
                if (x & 0xFFF0) == 0 or d.bool(p[9 + 3]):
                    x += 8
            else:
                x = d.tree(SMALL_MV_TREE, p[2:9])
            if x and d.bool(p[1]):
                x = -x
            return x

        def read_mv():
            r = read_mv_component(ctx["mv"][0:19]) * 2
            c = read_mv_component(ctx["mv"][19:38]) * 2
            return (r, c)

        def find_near(above, left, aboveleft, to_bias):
            mvs = [(0, 0), (0, 0), (0, 0), (0, 0)]
            cnt = [0, 0, 0, 0]
            ix = 0
            cx = 0

            def bias(mv, from_b):
                if from_b != to_bias:
                    return (-mv[0], -mv[1])
                return mv

            if not above["intra"]:
                if above["mv"] != (0, 0):
                    ix += 1
                    mvs[ix] = bias(above["mv"], above["sb"])
                    cx += 1
                cnt[cx] += 2
            for mb, wgt in ((left, 2), (aboveleft, 1)):
                if not mb["intra"]:
                    if mb["mv"] != (0, 0):
                        this = bias(mb["mv"], mb["sb"])
                        if this != mvs[ix]:
                            ix += 1
                            mvs[ix] = this
                            cx += 1
                        cnt[cx] += wgt
                    else:
                        cnt[0] += wgt
            # libvpx decodemv.c:366 — when the above-left pushed a THIRD
            # distinct candidate it counted into cnt[CNT_SPLITMV] (cntx
            # reached slot 3), and if that candidate equals the nearest,
            # NEAREST absorbs it. Alive, not dead.
            if cnt[3] != 0 and mvs[ix] == mvs[1]:
                cnt[1] += 1
            cnt[3] = (above["split"] * 2) + (left["split"] * 2) + aboveleft["split"]
            if cnt[2] > cnt[1]:
                cnt[1], cnt[2] = cnt[2], cnt[1]
                mvs[1], mvs[2] = mvs[2], mvs[1]
            return mvs, cnt

        def parse_inter_modes(r, c):
            mb_edge = (
                -(((c - 1) * 16) << 3),
                (((cols - 1 - (c - 1)) * 16) << 3),
                -(((r - 1) * 16) << 3),
                (((rows - 1 - (r - 1)) * 16) << 3),
            )
            to_bias = sb[mbref[r][c] - 1]

            def neigh(rr, cc):
                if rr == 0 or cc == 0 or mbref[rr][cc] == 0:
                    return {"intra": True, "zero": False, "mv": (0, 0), "split": False, "sb": False}
                return {
                    "intra": False,
                    "zero": mbmv[rr][cc] == (0, 0),
                    "mv": mbmv[rr][cc],
                    "split": mbsplit[rr][cc],
                    "sb": sb[mbref[rr][cc] - 1],
                }

            near_mvs, cnt = find_near(
                neigh(r - 1, c), neigh(r, c - 1), neigh(r - 1, c - 1), to_bias
            )
            print(f"CNT r={r-1} c={c-1} cnt={cnt} mvs={near_mvs}")
            MC = MODE_CONTEXTS
            if not d.bool(MC[cnt[0]][0]):
                mbmode[r][c] = -1  # ZEROMV marker
                mbmv[r][c] = (0, 0)
                mbref_marker = 1
                return
            if not d.bool(MC[cnt[1]][1]):
                mbmode[r][c] = -2  # NEAREST
                mv = clamp_mv2(near_mvs[1], mb_edge)
                mbmv[r][c] = mv
                mbref_marker = 1
                return
            if not d.bool(MC[cnt[2]][2]):
                mbmode[r][c] = -3  # NEAR
                mv = clamp_mv2(near_mvs[2], mb_edge)
                mbmv[r][c] = mv
                mbref_marker = 1
                return
            ni = 1 if cnt[1] >= cnt[0] else 0
            near_mvs[ni] = clamp_mv2(near_mvs[ni], mb_edge)
            best = near_mvs[ni]
            if not d.bool(MC[cnt[3]][3]):
                mbmode[r][c] = -4  # NEW
                mv = read_mv()
                mv = (mv[0] + best[0], mv[1] + best[1])
                mbmvclamp[r][c] = oob(mv, mb_edge)
                mbmv[r][c] = mv
                mbref_marker = 1
                return
            # SPLITMV
            s = d.tree(MV_PARTITION_TREE, MV_PARTITION_PROBS)
            num_p = [2, 2, 4, 16][s]
            bmi = [(0, 0)] * 16
            mbbmi[r][c] = bmi
            for j in range(num_p):
                k = MBSPLIT_OFFSET[s][j]
                if k % 4 == 0:
                    lm = r, c - 1
                    leftmv = (
                        mbbmi[lm[0]][lm[1]][k + 3]
                        if mbsplit[lm[0]][lm[1]]
                        else mbmv[lm[0]][lm[1]]
                    )
                else:
                    leftmv = bmi[k - 1]
                if k < 4:
                    am = r - 1, c
                    abovemv = (
                        mbbmi[am[0]][am[1]][k + 12]
                        if mbsplit[am[0]][am[1]]
                        else mbmv[am[0]][am[1]]
                    )
                else:
                    abovemv = bmi[k - 4]
                lez = leftmv == (0, 0)
                aez = abovemv == (0, 0)
                lea = leftmv == abovemv
                ctxi = 4 if (lez and aez) else (3 if lea else (1 if lez else (2 if aez else 0)))
                pr = d.tree(SUB_MV_REF_TREE, SUB_MV_REF_PROBS[ctxi])
                if pr == 0:
                    blockmv = leftmv
                elif pr == 1:
                    blockmv = abovemv
                elif pr == 2:
                    blockmv = (0, 0)
                else:
                    mv = read_mv()
                    blockmv = (mv[0] + best[0], mv[1] + best[1])
                print(
                    f"SP r={r-1} c={c-1} j={j} k={k} l=({leftmv[0]},{leftmv[1]}) a=({abovemv[0]},{abovemv[1]}) ctx={ctxi} mv=({blockmv[0]},{blockmv[1]}) best=({best[0]},{best[1]})"
                )
                mbmvclamp[r][c] = mbmvclamp[r][c] or oob(blockmv, mb_edge)
                cntf = MBSPLIT_FILL_COUNT[s]
                for o in MBSPLIT_FILL_OFFSET[s][j * cntf : (j + 1) * cntf]:
                    bmi[o] = blockmv
                mbbmi[r][c] = bmi
            mbsplit[r][c] = True
            mbmode[r][c] = 4
            hasy2[r][c] = False
            mbmv[r][c] = bmi[15]
            return

        # ---- mode records + tokens ----------------------------------------
        # Key frames interleave per row (like the Rust decode_keyframe);
        # inter frames read ALL mode records first, then token rows
        # (like decode_interframe). B-line order must match Rust exactly.
        d2s = [None] * nparts

        def row_modes(r):
            for c in range(1, cols + 1):
                if seg_enabled and seg_update_map:
                    seg_map[(r - 1) * cols + (c - 1)] = d.tree(
                        [2, 4, 0, -1, -2, -3], seg_tree_probs
                    )
                if skip_enabled:
                    skips[r][c] = bool(d.bool(prob_skip))
                if kf:
                    ym = d.tree(KF_YMODE_TREE, [145, 156, 163, 128])
                    mbmode[r][c] = ym
                    hasy2[r][c] = ym != 4
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
                            bmode[r][c][j] = d.tree(
                                BMODE_TREE, KF_BMODE[(a * 10 + l) * 9 : (a * 10 + l) * 9 + 9]
                            )
                    d.tree(UV_MODE_TREE, [142, 114, 183])
                else:
                    # ---- inter mode record (§16) ----
                    if not d.bool(prob_intra):
                        mbref[r][c] = 0
                        ym = d.tree(YMODE_TREE, ctx["ymode"])
                        mbmode[r][c] = ym
                        hasy2[r][c] = ym != 4
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
                                bmode[r][c][j] = d.tree(
                                    BMODE_TREE,
                                    KF_BMODE[(a * 10 + l) * 9 : (a * 10 + l) * 9 + 9],
                                )
                        d.tree(UV_MODE_TREE, ctx["uv"])
                    else:
                        if not d.bool(prob_last):
                            mbref[r][c] = 1
                        elif not d.bool(prob_gf):
                            mbref[r][c] = 2
                        else:
                            mbref[r][c] = 3
                        try:
                            parse_inter_modes(r, c)
                        except Exception as e:
                            import traceback; traceback.print_exc()
                            print(f"EXC at r={r} c={c}: {e}")
                            raise SystemExit(1)
                        mvref_print = 4 if mbsplit[r][c] else {-1: 0, -2: 1, -3: 2, -4: 3}.get(
                            mbmode[r][c], 0
                        )
                        print(
                            f"IMB r={r-1} c={c-1} ref={mbref[r][c]} mvref={mvref_print} "
                            f"mv=({mbmv[r][c][0]},{mbmv[r][c][1]}) clamp={mbmvclamp[r][c]}"
                        )
                        if 0 < mbref[r][c] and mbmode[r][c] >= 0:
                            hasy2[r][c] = True

        def row_tokens(r):
            pi = (r - 1) % nparts
            if d2s[pi] is None:
                d2s[pi] = BD(parts[pi])
            d2 = d2s[pi]
            left_ctx = [0] * 9

            def get_coeffs(ptype, n0, ctx0, out):
                base = ptype * 264
                n = n0
                band = BANDS[n0]
                c = ctx0
                p = base + band * 33 + c * 11

                def g(prob_):
                    bit = d2.bool(prob_)
                    print(f"G n={n} band={band} ctx={c} prob={prob_} bit={1 if bit else 0}")
                    return bit

                if not g(probs[p]):
                    return 0
                while True:
                    n += 1
                    if not g(probs[p + 1]):
                        c = 0
                        band = BANDS[n]
                        p = base + band * 33
                    else:
                        if not g(probs[p + 2]):
                            c = 1
                            band = BANDS[n]
                            p = base + band * 33 + 11
                            v = 1
                        else:
                            if not g(probs[p + 3]):
                                if not g(probs[p + 4]):
                                    v = 2
                                else:
                                    v = 3 + (1 if g(probs[p + 5]) else 0)
                            else:
                                if not g(probs[p + 6]):
                                    if not g(probs[p + 7]):
                                        v = 5 + (1 if g(159) else 0)
                                    else:
                                        v = 7 + 2 * (1 if g(165) else 0)
                                        v += 1 if g(145) else 0
                                else:
                                    bit1 = 1 if g(probs[p + 8]) else 0
                                    bit0 = 1 if g(probs[p + 9 + bit1]) else 0
                                    cat = 2 * bit1 + bit0
                                    v = 0
                                    for prob in CAT_EXTRA[cat]:
                                        v += v + (1 if g(prob) else 0)
                                    v += 3 + (8 << cat)
                            c = 2
                            band = BANDS[n]
                            p = base + band * 33 + 22
                        out[ZIGZAG[n - 1]] = -v if g(128) else v
                        if n < 16 and not g(probs[p]):
                            return n
                    if n == 16:
                        return 16

            for c in range(1, cols + 1):
                left_ctx = [0] * 9 if c == 1 else left_ctx

                def one(blkidx, ptype, n0):
                    o = [0] * 16
                    ctxv = min(2, left_ctx[LEFT[blkidx]] + above_ctx[c - 1][ABOVE[blkidx]])
                    print(f"T {r-1} {c-1} {blkidx} {ptype} {ctxv}")
                    eob = get_coeffs(ptype, n0, ctxv, o)
                    t = 1 if eob > 0 else 0
                    left_ctx[LEFT[blkidx]] = t
                    above_ctx[c - 1][ABOVE[blkidx]] = t

                if skips[r][c]:
                    for i in range(8):
                        above_ctx[c - 1][i] = 0
                        left_ctx[i] = 0
                    if hasy2[r][c]:
                        above_ctx[c - 1][8] = 0
                        left_ctx[8] = 0
                    continue
                if hasy2[r][c]:
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

        if kf:
            for r in range(1, rows + 1):
                row_modes(r)
                row_tokens(r)
        else:
            for r in range(1, rows + 1):
                row_modes(r)
            for r in range(1, rows + 1):
                row_tokens(r)

        if not refresh_entropy:
            ctx["coef"], ctx["mv"], ctx["ymode"], ctx["uv"] = snapshot


main()
