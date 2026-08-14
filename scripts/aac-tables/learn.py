#!/usr/bin/env python3
"""Learn one AAC spectral Huffman codebook by black-box probing of ffmpeg.

A bit string b is a complete codeword iff the first decoded tuple does not
change when a 0 is appended (prefix codes are injective, and the filler after
the codeword is identical in both probes).  BFS over the binary tree therefore
enumerates the whole codebook in ~2M probes for M codewords.
"""

import numpy as np
import subprocess
import os
import sys
import json

import aacprobe as A

N = 1024
_M = None


def basis():
    global _M
    if _M is None:
        n = np.arange(2 * N)
        _M = np.cos(np.pi / N * np.outer(n + 512.5, np.arange(16) + 0.5))
    return _M


WIN = np.sin(np.pi / (2 * N) * (np.arange(2 * N) + 0.5))


def spectrum(block):
    return (basis().T @ (block / WIN)) / N


def quant(x):
    """Dequantised coefficient -> signed quantised integer."""
    a = abs(x)
    if a < 1e-3:
        return 0
    v = int(round(a ** 0.75))
    return -v if x < 0 else v


def _decode(frames, tag=None):
    d = os.path.dirname(os.path.abspath(__file__))
    src = os.path.join(d, f"probe-{tag or os.getpid()}.aac")
    sil = A.silent_frame()
    with open(src, "wb") as f:
        f.write(sil + b"".join(fr + sil + sil for fr in frames))
    raw = subprocess.run(
        ["ffmpeg", "-v", "quiet", "-i", src, "-f", "f32le", "-ac", "1", "-"],
        capture_output=True,
    )
    pcm = np.frombuffer(raw.stdout, dtype="<f4").astype(np.float64)
    nblk = len(pcm) // N
    os.remove(src)
    if nblk != 3 * len(frames) + 1:
        return None
    pcm = pcm[: nblk * N].reshape(nblk, N)
    out = []
    for i in range(len(frames)):
        # the third packet of every triple decodes to silence; if it does not,
        # the stream lost a packet and every later index is off by one
        if float(np.abs(pcm[3 * i + 3]).max()) > 1e-9:
            return None
        out.append(np.concatenate([pcm[3 * i + 1], pcm[3 * i + 2]]))
    return out


def _blocks(frames):
    """One 2048-sample block per frame; None for frames ffmpeg refused.

    A refused frame costs the whole batch its packet alignment, so a failed
    batch is bisected until the offenders are isolated.
    """
    got = _decode(frames)
    if got is not None:
        return got
    if len(frames) == 1:
        return [None]
    h = len(frames) // 2
    return _blocks(frames[:h]) + _blocks(frames[h:])


def run_batch(cb, probes, filler, gg=200, tag=None):
    """Decode one frame per probe; returns a list of 8 quantised values or None."""
    frames = [A.probe_frame(cb, [0, 0], list(p) + filler, gg=gg) for p in probes]
    return [
        None if b is None else tuple(quant(v) for v in spectrum(b)[:8])
        for b in _blocks(frames)
    ]


def learn(cb, dim, signed, esc=False, filler=None):
    filler = filler if filler is not None else [1] * 96
    cache = {}

    def evaluate(nodes):
        todo = [b for b in nodes if b not in cache]
        for i in range(0, len(todo), 400):
            chunk = todo[i : i + 400]
            res = run_batch(cb, chunk, filler)
            for b, r in zip(chunk, res):
                if r is None:
                    cache[b] = None
                else:
                    t = r[:dim]
                    if not signed:
                        t = tuple(abs(v) for v in t)
                    if esc:
                        t = tuple(min(v, 16) if v >= 0 else max(v, -16) for v in t)
                    cache[b] = t

    codewords = {}
    frontier = [()]
    depth = 0
    while frontier and depth < 22:
        evaluate(frontier + [b + (0,) for b in frontier])
        nxt = []
        for b in frontier:
            g, g0 = cache[b], cache[b + (0,)]
            if g is None:
                continue  # frame refused: subtree unreachable, retried below
            if g == g0:
                codewords[b] = g
            else:
                nxt += [b + (0,), b + (1,)]
        frontier = nxt
        depth += 1
        print(f"  cb{cb} depth {depth}: {len(codewords)} codewords, {len(frontier)} open",
              file=sys.stderr)
    return codewords


def kraft(codewords):
    return sum(2.0 ** -len(b) for b in codewords)


if __name__ == "__main__":
    cb = int(sys.argv[1])
    dim = int(sys.argv[2])
    signed = sys.argv[3] == "s"
    esc = len(sys.argv) > 4 and sys.argv[4] == "esc"
    cw = learn(cb, dim, signed, esc)
    tuples = set(cw.values())
    print(f"cb{cb}: {len(cw)} codewords, {len(tuples)} distinct tuples, kraft={kraft(cw):.6f}")
    out = {"".join(map(str, b)): list(t) for b, t in cw.items()}
    with open(f"cb{cb}.json", "w") as f:
        json.dump(out, f)
