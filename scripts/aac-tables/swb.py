#!/usr/bin/env python3
"""Derive the scalefactor-band offset tables (ISO 14496-3 tbl 4.140) by probing.

For a frame coded with max_sfb = k, the highest non-zero spectral coefficient is
swb_offset[k] - 1.  Long windows are read back through the exact MDCT inverse;
short windows through the FFT of the 2048-sample frame, whose bin spacing
(fs/2048) is eight times finer than one short-block coefficient (fs/256).
"""

import json
import os
import subprocess
import sys

import numpy as np

import aacprobe as A
import learn as L

BASIS = np.cos(np.pi / 1024 * np.outer(np.arange(2048) + 512.5, np.arange(1024) + 0.5))


def sect_len(w, k, bits):
    esc = (1 << bits) - 1
    left = k
    while left >= esc:
        w.w(esc, bits)
        left -= esc
    w.w(left, bits)


def frame_long(k, sf_index):
    w = A.BitW()
    w.w(0, 3).w(0, 4).w(200, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(k, 6).w(0, 1)
    w.w(3, 4)
    sect_len(w, k, 5)
    w.wbits([0] * k)  # the one-bit delta-zero scalefactor codeword per band
    w.w(0, 3)
    w.wbits([1] * 6000)
    return A.adts(w.pack(pad=0), sf_index=sf_index)


def frame_short(k, sf_index):
    w = A.BitW()
    w.w(0, 3).w(0, 4).w(200, 8)
    w.w(0, 1).w(2, 2).w(0, 1).w(k, 4).w(0x7F, 7)  # EIGHT_SHORT, one group of 8
    w.w(3, 4)
    sect_len(w, k, 3)
    w.wbits([0] * k)
    w.w(0, 3)
    w.wbits([1] * 6000)
    return A.adts(w.pack(pad=0), sf_index=sf_index)


def silent(sf_index):
    w = A.BitW()
    w.w(0, 3).w(0, 4).w(128, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(0, 6).w(0, 1)
    w.w(0, 3).w(7, 3)
    return A.adts(w.pack(4), sf_index=sf_index)


def blocks(frames, sf_index):
    src = f"swb-{os.getpid()}.aac"
    sil = silent(sf_index)
    with open(src, "wb") as f:
        f.write(sil + b"".join(fr + sil + sil for fr in frames))
    raw = subprocess.run(
        ["ffmpeg", "-v", "quiet", "-i", src, "-f", "f32le", "-ac", "1", "-"],
        capture_output=True,
    )
    pcm = np.frombuffer(raw.stdout, dtype="<f4").astype(np.float64)
    os.remove(src)
    nb = len(pcm) // 1024
    if nb != 3 * len(frames) + 1:
        if len(frames) == 1:
            return [None]
        h = len(frames) // 2
        return blocks(frames[:h], sf_index) + blocks(frames[h:], sf_index)
    pcm = pcm[: nb * 1024].reshape(nb, 1024)
    return [np.concatenate([pcm[3 * i + 1], pcm[3 * i + 2]]) for i in range(len(frames))]


def long_offsets(sf_index):
    out = []
    got = blocks([frame_long(k, sf_index) for k in range(1, 52)], sf_index)
    for blk in got:
        if blk is None:
            break
        spec = (BASIS.T @ (blk / L.WIN)) / 1024
        nz = np.where(np.abs(spec) > 1e-3)[0]
        if len(nz) == 0:
            break
        out.append(int(nz.max()) + 1)
    return out


def short_offsets(sf_index):
    out = []
    got = blocks([frame_short(k, sf_index) for k in range(1, 17)], sf_index)
    for blk in got:
        if blk is None:
            break
        mag = np.abs(np.fft.rfft(blk * np.hanning(2048)))
        top = np.where(mag > mag.max() * 3e-4)[0].max()
        out.append(top / 8.0)
    return out


if __name__ == "__main__":
    out = {}
    for idx in range(0, 12):
        lo = long_offsets(idx)
        sh = short_offsets(idx)
        out[idx] = {"long": lo, "short": sh}
        print(idx, "long", len(lo), lo, file=sys.stderr)
        print(idx, "short", [round(v, 1) for v in sh], file=sys.stderr)
    json.dump(out, open("swb.json", "w"))
