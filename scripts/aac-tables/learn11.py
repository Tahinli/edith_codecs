#!/usr/bin/env python3
"""Codebook 11 (escape) and the scalefactor codebook.

cb11: an all-ones filler makes the escape's unary length run away, so the
filler is `1^6 0` repeated -- bounded escapes -- and probes that land their
END element on one of those zeros are retried at another phase.

sfb: the probe sits in the scale_factor_data field of a one-band frame whose
spectral field is the all-ones filler; the band's scalefactor is read back from
the dequantised magnitudes (|X| = x^(4/3) * 2^((sf-100)/4), x integral).
"""

import json
import os
import subprocess
import sys

import numpy as np

import aacprobe as A
import learn as L

FILLERS = [([1] * k + [0]) * (140 // (k + 1)) for k in (12, 20, 9, 6, 16, 24, 7, 30)]


def eval_cb11(nodes, cache):
    todo = [b for b in nodes if b not in cache]
    pending = list(todo)
    for filler in FILLERS:
        if not pending:
            break
        nxt = []
        for i in range(0, len(pending), 400):
            chunk = pending[i : i + 400]
            res = L.run_batch(11, chunk, filler)
            for b, r in zip(chunk, res):
                if r is None:
                    nxt.append(b)
                else:
                    cache[b] = tuple(min(abs(v), 16) for v in r[:2])
        pending = nxt
    for b in pending:
        cache[b] = None


def learn_cb11():
    cache = {}
    codewords = {}
    frontier = [()]
    depth = 0
    while frontier and depth < 32:
        eval_cb11(frontier + [b + (0,) for b in frontier], cache)
        nxt = []
        for b in frontier:
            g, g0 = cache[b], cache[b + (0,)]
            if g is None or g0 is None:
                print("  UNRESOLVED", b, file=sys.stderr)
                continue
            if g == g0:
                codewords[b] = g
            else:
                nxt += [b + (0,), b + (1,)]
        frontier = nxt
        depth += 1
        print(f"  cb11 depth {depth}: {len(codewords)} codewords, {len(frontier)} open",
              file=sys.stderr)
    return codewords


# --- scalefactor codebook ---------------------------------------------------


def sf_batch(probes, tag=None):
    """Scalefactor of the second coded band, one frame per probe."""
    frames = [A.probe_frame(3, [0] + list(p), [1] * 96, gg=160, max_sfb=2) for p in probes]
    out = []
    for blk in L._blocks(frames):
        if blk is None:
            out.append(None)
            continue
        peak = float(np.abs(L.spectrum(blk)[4:8]).max())
        if peak < 1e-9:
            out.append(None)
            continue
        # |X| = x^(4/3) * 2^((sf-100)/4) with x in {1, 2} (codebook 3, lav 2);
        # only one of the two makes sf integral.
        best = None
        for x in (1, 2):
            sf = 100 + 4 * np.log2(peak / x ** (4 / 3))
            if abs(sf - round(sf)) < 0.02:
                best = int(round(sf))
        out.append(best)
    return out


def learn_sf():
    cache = {}

    def evaluate(nodes):
        todo = [b for b in nodes if b not in cache]
        for i in range(0, len(todo), 400):
            chunk = todo[i : i + 400]
            for b, r in zip(chunk, sf_batch(chunk)):
                cache[b] = r

    codewords = {}
    frontier = [()]
    depth = 0
    while frontier and depth < 32:
        evaluate(frontier + [b + (0,) for b in frontier])
        nxt = []
        for b in frontier:
            g, g0 = cache[b], cache[b + (0,)]
            if g is not None and g == g0:
                codewords[b] = g
            else:
                nxt += [b + (0,), b + (1,)]
        frontier = nxt
        depth += 1
        print(f"  sfb depth {depth}: {len(codewords)} codewords, {len(frontier)} open",
              file=sys.stderr)
    return codewords


if __name__ == "__main__":
    if sys.argv[1] == "11":
        cw = learn_cb11()
        print(f"cb11: {len(cw)} codewords, {len(set(cw.values()))} tuples, "
              f"kraft={L.kraft(cw):.6f}")
        json.dump({"".join(map(str, b)): list(t) for b, t in cw.items()},
                  open("cb11.json", "w"))
    else:
        cw = learn_sf()
        print(f"sfb: {len(cw)} codewords, {len(set(cw.values()))} values, "
              f"kraft={L.kraft(cw):.6f}, range={min(cw.values())}..{max(cw.values())}")
        json.dump({"".join(map(str, b)): v for b, v in cw.items()},
                  open("sfb.json", "w"))
