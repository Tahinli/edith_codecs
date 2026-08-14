"""Measure the Layer III synthesis window D[512] from the ffmpeg decoder.

Probes carry only +-1 spectral values, written with Huffman table 1 (four
codewords), so nothing but the header/side-info syntax is assumed. The decode
chain up to the synthesis window is modelled exactly, leaving a linear system
pcm = A @ D that a least-squares solve inverts.
"""

import numpy as np
import sys
from mp3probe import (BitWriter, Granule, frame_bytes, ffmpeg_decode,
                      granules_to_subband, synthesis_basis)

# table 1: (x,y) -> (length, code), index = x*2 + y
TABLE1 = [(1, 0b1), (3, 0b001), (2, 0b01), (3, 0b000)]

GLOBAL_GAIN = 170  # 2^((gg-210)/4) = 2^-10 spectral scale, far from clipping


def write_pairs(pairs):
    bw = BitWriter()
    for (x, sx), (y, sy) in pairs:
        ln, code = TABLE1[x * 2 + y]
        bw.w(code, ln)
        if x:
            bw.w(sx, 1)
        if y:
            bw.w(sy, 1)
    return bw.bits


def probe_frames(rng, nframes=6, npairs=288):
    """Random +-1 spectra; returns (mp3 bytes, list of granule spectra)."""
    stream = b""
    spectra = []
    for _ in range(nframes):
        grans, mains = [], []
        for _g in range(2):
            vals = rng.integers(0, 2, size=(npairs, 2))
            signs = rng.integers(0, 2, size=(npairs, 2))
            pairs = [((int(vals[i, 0]), int(signs[i, 0])),
                      (int(vals[i, 1]), int(signs[i, 1]))) for i in range(npairs)]
            bits = write_pairs(pairs)
            grans.append(Granule(part2_3_length=len(bits), big_values=npairs,
                                 global_gain=GLOBAL_GAIN, table_select=(1, 1, 1)))
            mains.append(bits)
            xr = np.zeros(576)
            scale = 2.0 ** ((GLOBAL_GAIN - 210) / 4.0)
            for i in range(npairs):
                for j in range(2):
                    v = float(vals[i, j])
                    if signs[i, j]:
                        v = -v
                    xr[2 * i + j] = v * scale
            spectra.append(xr)
        stream += frame_bytes(grans, mains)
    return stream, spectra


def main():
    rng = np.random.default_rng(0xC0DE)
    mp3, spectra = probe_frames(rng)
    pcm, err = ffmpeg_decode(mp3)
    print(f"ffmpeg: {len(pcm)} samples, stderr={err.strip()!r}, peak={np.abs(pcm).max():.6g}")
    slots = granules_to_subband(spectra)
    A = synthesis_basis(slots)
    print(f"model: {A.shape[0]} samples from {len(slots)} slots")
    n = min(len(pcm), A.shape[0])
    # frame-granularity alignment search
    best = None
    for off in range(0, len(pcm) - n + 1, 1152 // 2):
        D, res, rank, _ = np.linalg.lstsq(A[:n], pcm[off:off + n], rcond=None)
        resid = np.linalg.norm(A[:n] @ D - pcm[off:off + n]) / max(np.linalg.norm(pcm[off:off + n]), 1e-30)
        if best is None or resid < best[0]:
            best = (resid, off, D, rank)
    resid, off, D, rank = best
    print(f"offset={off} rank={rank} relative residual={resid:.3e}")
    print(f"D peak={np.abs(D).max():.6g}  D[0..4]={D[:5]}")
    np.save("D.npy", D)


if __name__ == "__main__":
    sys.exit(main())
