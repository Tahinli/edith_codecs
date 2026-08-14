#!/usr/bin/env python3
"""Black-box derivation of the ISO/IEC 14496-3 AAC-LC Huffman codebooks.

Clean-room: no AAC implementation source is read.  The only oracle is ffmpeg's
`aac` decoder driven as a black box.  We synthesise ADTS frames whose spectral
payload is a bit string we choose, decode them, invert the (windowed) MDCT to
recover the quantised spectrum, and learn the prefix code from bits -> tuple.

Frame shape (mono, 48 kHz, ONLY_LONG, sine window):
    SCE(3)=0 tag(4)=0 global_gain(8) ics_info section_data sf_data flags spectral
max_sfb=2 gives one section of 8 coefficients, so the spectral field always
outlives the probe prefix and the trailing all-ones run lands on an END element
(id 7 = 0b111).  The ones run is bounded -- a long 0xFF tail makes ffmpeg's ADTS
parser resync on a false 0xFFF sync word.
"""

import numpy as np
import subprocess
import os

SR = 48000
N = 1024
ONES_RUN = 96  # bits of `1` filler after the probe, then zero padding


class BitW:
    def __init__(self):
        self.bits = []

    def w(self, val, n):
        for i in range(n - 1, -1, -1):
            self.bits.append((val >> i) & 1)
        return self

    def wbits(self, seq):
        self.bits.extend(seq)
        return self

    def __len__(self):
        return len(self.bits)

    def pack(self, total_bytes=None, pad=0):
        if total_bytes is None:
            total_bytes = (len(self.bits) + 7) // 8
        b = (self.bits + [pad] * (total_bytes * 8))[: total_bytes * 8]
        out = bytearray()
        for i in range(0, len(b), 8):
            v = 0
            for bit in b[i : i + 8]:
                v = (v << 1) | bit
            out.append(v)
        return bytes(out)


def adts(payload: bytes, sf_index=3, chan_cfg=1) -> bytes:
    h = BitW()
    h.w(0xFFF, 12).w(0, 1).w(0, 2).w(1, 1)  # sync, MPEG-4, layer, no CRC
    h.w(1, 2).w(sf_index, 4).w(0, 1).w(chan_cfg, 3)  # AAC-LC
    h.w(0, 1).w(0, 1).w(0, 1).w(0, 1)  # original, home, copyright id/start
    h.w(len(payload) + 7, 13).w(0x7FF, 11).w(0, 2)
    return h.pack(7) + payload


def probe_frame(cb, sf_bits, probe_bits, gg=200, max_sfb=2):
    w = BitW()
    w.w(0, 3).w(0, 4)  # SCE, instance tag
    w.w(gg, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(max_sfb, 6).w(0, 1)  # ONLY_LONG, sine window
    w.w(cb, 4).w(max_sfb, 5)  # one section covering every band
    w.wbits(sf_bits)
    w.w(0, 3)  # pulse / tns / gain-control absent
    w.wbits(probe_bits).wbits([1] * ONES_RUN)
    return adts(w.pack(pad=0))


def silent_frame():
    w = BitW()
    w.w(0, 3).w(0, 4).w(128, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(0, 6).w(0, 1)  # max_sfb = 0: nothing coded
    w.w(0, 3).w(7, 3)  # flags, END
    return adts(w.pack(4))


# --- MDCT read-back ---------------------------------------------------------

_PINV = None


def recover(block, ncoef=12):
    """Quantised-spectrum-proportional coefficients from 2048 output samples."""
    global _PINV
    if _PINV is None:
        cache = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pinv.npy")
        if os.path.exists(cache):
            _PINV = np.load(cache)
        else:
            n = np.arange(2 * N)
            k = np.arange(N)
            m = np.cos(2 * np.pi / N * np.outer(n + 0.5 + N / 2, k + 0.5))
            win = np.sin(np.pi / (2 * N) * (n + 0.5))
            _PINV = np.linalg.pinv(m * win[:, None])[:32].astype(np.float64)
            np.save(cache, _PINV)
    return _PINV[:ncoef] @ block


def decode_probes(frames, tag="p"):
    """One 2048-sample block per probe frame, or None when ffmpeg refused it."""
    d = os.path.dirname(os.path.abspath(__file__))
    src = os.path.join(d, f"probe-{tag}.aac")
    sil = silent_frame()
    with open(src, "wb") as f:
        f.write(sil + b"".join(fr + sil + sil for fr in frames))
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", src, "-f", "f32le", "-ac", "1", "-"],
        capture_output=True,
    )
    pcm = np.frombuffer(raw.stdout, dtype="<f4")
    nblk = len(pcm) // N
    pcm = pcm[: nblk * N].reshape(nblk, N).astype(np.float64)
    os.remove(src)
    if nblk != 3 * len(frames) + 1:
        return None, raw.stderr.decode()
    return [np.concatenate([pcm[3 * i + 1], pcm[3 * i + 2]]) for i in range(len(frames))], ""


if __name__ == "__main__":
    frs = [probe_frame(3, [0] * 2, [1] * 40)]
    blocks, err = decode_probes(frs, "smoke")
    if blocks is None:
        print("MISALIGNED", err[:400])
    else:
        print("coefs", np.round(recover(blocks[0], 12), 6))
