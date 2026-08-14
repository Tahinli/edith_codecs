"""The linear map (requantised spectrum -> PCM) for one probe frame, and its
pseudo-inverse: what turns an ffmpeg decode back into the integers a probe's
Huffman bits decoded to."""

import math
import numpy as np
from mp3probe import ALIAS_CS, ALIAS_CA, WIN_NORMAL

NSLOTS = 72          # probe frame (36 slots) + trailing silent frame
NSAMPLES = NSLOTS * 32


def imdct_matrix():
    n = 36
    return np.array([[math.cos(math.pi / (2 * n) * (2 * i + 1 + n // 2) * (2 * k + 1))
                      for k in range(18)] for i in range(n)]) * WIN_NORMAL[:, None]


def alias_matrix():
    ar = np.eye(576)
    for sb in range(1, 32):
        for i in range(8):
            lo, hi = sb * 18 - 1 - i, sb * 18 + i
            a, b = ar[lo].copy(), ar[hi].copy()
            ar[lo] = a * ALIAS_CS[i] - b * ALIAS_CA[i]
            ar[hi] = b * ALIAS_CS[i] + a * ALIAS_CA[i]
    return ar


def spectrum_to_slots():
    """(2 granules x 576) -> (72 slots x 32 subbands), flattened columns."""
    imw = imdct_matrix()
    ar = alias_matrix()
    L = np.zeros((1152, NSLOTS * 32))
    for g in range(2):
        for sb in range(32):
            for k in range(18):
                col = np.zeros(NSLOTS * 32)
                for i in range(18):
                    t0 = 18 * g + i
                    sign = -1.0 if (sb % 2 == 1 and i % 2 == 1) else 1.0
                    col[t0 * 32 + sb] += imw[i, k] * sign
                    t1 = 18 * (g + 1) + i
                    sign = -1.0 if (sb % 2 == 1 and i % 2 == 1) else 1.0
                    col[t1 * 32 + sb] += imw[18 + i, k] * sign
                # the alias butterflies mix coefficients before the imdct
                L[g * 576:(g + 1) * 576] += np.outer(ar[sb * 18 + k, :], col)
    return L


def slots_to_pcm(D):
    N = np.array([[math.cos((16 + i) * (2 * k + 1) * math.pi / 64) for k in range(32)]
                  for i in range(64)])
    S = np.zeros((NSLOTS * 32, NSAMPLES))
    for i in range(16):
        Ci = np.zeros((32, 32))
        for j in range(32):
            b = j if i % 2 == 0 else j + 32
            Ci[j] = D[j + 32 * i] * N[b]
        for t in range(NSLOTS):
            s = t - i
            if s < 0:
                continue
            S[s * 32:(s + 1) * 32, t * 32:(t + 1) * 32] += Ci.T
    return S


def build(D):
    M = spectrum_to_slots() @ slots_to_pcm(D)
    return M, np.linalg.pinv(M)


if __name__ == "__main__":
    D = np.load("D.npy")
    M, Minv = build(D)
    np.save("M.npy", M)
    np.save("Minv.npy", Minv)
    print("M", M.shape, "cond ok")

    # self-check: round-trip a random spectrum through the model
    rng = np.random.default_rng(7)
    c = rng.normal(size=1152) * 1e-3
    pcm = c @ M
    back = pcm @ Minv
    print("model round-trip max err", np.abs(back - c).max())
