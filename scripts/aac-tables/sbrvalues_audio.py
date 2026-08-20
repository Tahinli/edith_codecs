#!/usr/bin/env python3
"""Reads the delta each 3.0 dB envelope codeword carries off the reference
decoder's AUDIO, not its range check.

`sbrtables.py --stage values` learns a codeword's delta from the decoder's
scalefactor range complaint, bisecting the raw start value. That only sees a
delta that pushes the accumulated value OUT of range: the 3.0 dB books ride a
6-bit (5-bit balance) raw field but are range-checked against the 1.5 dB
books' 7-bit ceiling, so no positive 3.0 dB delta can ever trip it and every
one was emitted as 0 (ENV30_F/ENV30_T/ENVB30_F/ENVB30_T: 32/32/13/13 zeros).

Here the same single-codeword probe frames are decoded to PCM instead and the
delta is the log2 energy ratio between the two bands (`_F` books) or the two
envelopes (`_T` books) it separates -- a 3 dB step per unit, trivially
resolvable. Negative deltas the range check DID see are re-read the same way
as a cross-check. Writes the corrected values back into sbr_tables.rs.
"""

import math
import os
import re
import subprocess
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import sbrtables as T  # noqa: E402

RS = os.path.join(T.ROOT, "crates", "ec-aac", "src", "sbr_tables.rs")
KINDS = ["ENV30_F", "ENV30_T", "ENVB30_F", "ENVB30_T"]


def load_books():
    src = open(RS).read()
    books = {}
    for m in re.finditer(r"static (\w+): \[\(u8, u32, i32\); \d+\] = \[(.*?)\n\];", src, re.S):
        books[m.group(1)] = {
            (int(a), int(b)): int(c)
            for a, b, c in re.findall(r"\((\d+), (\d+), (-?\d+)\)", m.group(2))
        }
    return books


def decode(data, channels):
    src = f"/tmp/sbrvalues-{os.getpid()}.aac"
    with open(src, "wb") as f:
        f.write(data)
    r = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", src, "-f", "f32le", "-ac", str(channels), "-"],
        capture_output=True,
    )
    os.remove(src)
    pcm = np.frombuffer(r.stdout, dtype=np.float32).reshape(-1, channels)
    return pcm


def band_energy(x, rate, k0, k1):
    """Energy of 64-band QMF bands k0..k1 (at the doubled output rate)."""
    spec = np.abs(np.fft.rfft(x)) ** 2
    hz = rate / len(x)
    band_hz = rate / 128.0
    lo, hi = int(k0 * band_hz / hz), int(k1 * band_hz / hz)
    return float(spec[lo:hi].sum())


def frame(cfg, tables, words):
    """`sbrtables.frame` with the FIL payload padded out to its byte count
    and that count written exactly: the length probes deliberately under-
    declare it to provoke the parser's byte-count complaint, but a decoder
    that is to produce AUDIO must find END right where the count says."""
    from sbrprobe import BitW, adts

    cpe = cfg.get("element") == "cpe"
    w = BitW()
    T.core_element(w, cfg["sf_index"], 4, 2 if cpe else 1)
    body = T.sbr_bits(cfg, tables, words)
    bits = 4 + len(body.bits)
    cnt = (bits + 7) // 8
    w.w(6, 3)  # FIL
    if cnt >= 15:
        w.w(15, 4)
        w.w(cnt - 15 + 1, 8)
    else:
        w.w(cnt, 4)
    w.w(13, 4)  # EXT_SBR_DATA
    w.wbits(body.bits)
    w.w(0, cnt * 8 - bits)
    w.w(7, 3)  # END
    return adts(w.pack(pad=0), cfg["sf_index"], 2 if cpe else 1)


def probe(kind, cfg, tables, key, word, books, raw):
    words = T.fill_words(cfg, tables, kind, word, books, extra=0)
    v = dict(cfg)
    arr = list(v[key])
    arr[0] = raw
    v[key] = arr
    v["limiter_gains"] = 3
    data = frame(v, tables, words) * 8
    ch = 2 if cfg.get("element") == "cpe" else 1
    pcm = decode(data, ch)
    return pcm


def main():
    books = load_books()
    out = {}
    for kind in KINDS:
        made = T.values_config(kind)
        if made is None:
            print(f"{kind}: no config")
            continue
        cfg, tables, key, width, scale = made
        balance = kind.startswith("ENVB")
        if not balance:
            # A mono SBR stream is re-laid-out as implicit stereo by the
            # reference decoder and then dropped; ride a CPE instead (both
            # channels use the same books, the word lands in ch1).
            cfg = dict(cfg, element="cpe", coupling=0)
        rate = T.SBR_RATE_FOR_SF[cfg["sf_index"]]
        raw = 12 if balance else 30
        f_high = tables["f_high"]
        frame = 2048
        codes = sorted(books[kind])
        # Alignment for _T books: the trusted -1 codeword (second envelope
        # 3 dB below the first) picks the frame phase.
        offset = None
        if kind.endswith("_T"):
            cal = next(lc for lc in codes if books[kind][lc] == -1)
            pcm = probe(kind, cfg, tables, key, T.as_bits(*cal), books, raw)
            x = pcm[:, 1]
            best = None
            for o in range(0, frame, 64):
                a = b = 0.0
                for s in range(3 * frame + o, len(x) - frame, frame):
                    a += float((x[s : s + 1024] ** 2).sum())
                    b += float((x[s + 1024 : s + 2048] ** 2).sum())
                c = math.log2(b / a) if a > 0 and b > 0 else 0.0
                if best is None or c < best[0]:
                    best = (c, o)
            offset = best[1]
            print(f"{kind}: alignment offset {offset}, calibration log2 ratio {best[0]:.3f} (expect -1)")
        got = {}
        for lc in codes:
            pcm = probe(kind, cfg, tables, key, T.as_bits(*lc), books, raw)
            if kind.endswith("_F"):
                x = pcm[3 * frame :]
                e = [
                    [band_energy(x[:, c], rate, f_high[b], f_high[b + 1]) for b in (0, 1)]
                    for c in range(x.shape[1])
                ]
                # Both envelopes of the `_F` probe carry the same row (the
                # second is T-coded with the trusted 0 codeword), so the
                # whole-signal band ratio is the delta itself.
                if balance:
                    rho = (e[0][1] / e[1][1]) / (e[0][0] / e[1][0])
                    d = math.log2(rho) / 2
                else:
                    d = math.log2(e[1][1] / e[1][0])
            else:
                x = pcm
                a = b = 0.0
                al = bl = 0.0
                for s in range(3 * frame + offset, len(x) - frame, frame):
                    a += float((x[s : s + 1024, 1] ** 2).sum())
                    b += float((x[s + 1024 : s + 2048, 1] ** 2).sum())
                    al += float((x[s : s + 1024, 0] ** 2).sum())
                    bl += float((x[s + 1024 : s + 2048, 0] ** 2).sum())
                if balance:
                    # ch1 carries the balance word: its L/R swing against ch0.
                    rho = (bl / b) / (al / a)
                    d = math.log2(rho) / 2
                else:
                    d = math.log2(b / a)
            got[lc] = d
            old = books[kind][lc]
            flag = "" if (old != 0 and abs(d - old) < 0.3) or (old == 0) else "  <-- DISAGREES with range-check value"
            print(f"  {kind} {lc}: old {old:>4} read {d:7.3f}{flag}")
        out[kind] = got
    # Rewrite: every codeword takes the rounded readout.
    src = open(RS).read()
    # Targeted replacement per book block.
    def patch_block(m):
        name, body = m.group(1), m.group(2)
        if name not in out:
            return m.group(0)
        def sub(t):
            l, c, v = int(t.group(1)), int(t.group(2)), int(t.group(3))
            d = out[name].get((l, c))
            if d is None or math.isnan(d):
                return t.group(0)
            return f"({l}, {c}, {round(d)})"
        return m.group(0).replace(body, re.sub(r"\((\d+), (\d+), (-?\d+)\)", sub, body))
    src = re.sub(r"static (\w+): \[\(u8, u32, i32\); \d+\] = \[(.*?)\n\];", patch_block, src, flags=re.S)
    open(RS, "w").write(src)
    print(f"wrote {RS}")


if __name__ == "__main__":
    main()
