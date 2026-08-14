"""Black-box probe rig for MPEG-1 Layer III.

Builds legal Layer III frames whose main data we control bit for bit, decodes
them with ffmpeg, and models the decode chain analytically so the spec's
constant tables (synthesis window, Huffman codes, scalefactor bands) can be
*measured* rather than copied from an implementation.
"""

import math
import subprocess
import tempfile
import os
import numpy as np

SRATES = [44100, 48000, 32000]
BITRATES = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320]


class BitWriter:
    def __init__(self):
        self.bits = []

    def w(self, value, n):
        for i in range(n - 1, -1, -1):
            self.bits.append((value >> i) & 1)

    def __len__(self):
        return len(self.bits)

    def bytes(self, pad_to=None):
        bits = list(self.bits)
        if pad_to is not None:
            bits += [0] * (pad_to * 8 - len(bits))
        while len(bits) % 8:
            bits.append(0)
        out = bytearray()
        for i in range(0, len(bits), 8):
            b = 0
            for j in range(8):
                b = (b << 1) | bits[i + j]
            out.append(b)
        return bytes(out)


class Granule:
    """One granule of side info; long blocks only (all the probes need)."""

    def __init__(self, part2_3_length=0, big_values=0, global_gain=210,
                 scalefac_compress=0, table_select=(0, 0, 0), region0_count=15,
                 region1_count=1, preflag=0, scalefac_scale=0, count1table_select=0,
                 block_type=0, mixed_block=0, subblock_gain=(0, 0, 0)):
        self.block_type = block_type
        self.mixed_block = mixed_block
        self.subblock_gain = subblock_gain
        self.part2_3_length = part2_3_length
        self.big_values = big_values
        self.global_gain = global_gain
        self.scalefac_compress = scalefac_compress
        self.table_select = table_select
        self.region0_count = region0_count
        self.region1_count = region1_count
        self.preflag = preflag
        self.scalefac_scale = scalefac_scale
        self.count1table_select = count1table_select

    def write(self, bw):
        bw.w(self.part2_3_length, 12)
        bw.w(self.big_values, 9)
        bw.w(self.global_gain, 8)
        bw.w(self.scalefac_compress, 4)
        bw.w(1 if self.block_type else 0, 1)  # window_switching_flag
        if self.block_type:
            bw.w(self.block_type, 2)
            bw.w(self.mixed_block, 1)
            for t in self.table_select[:2]:
                bw.w(t, 5)
            for g in self.subblock_gain:
                bw.w(g, 3)
        else:
            for t in self.table_select:
                bw.w(t, 5)
            bw.w(self.region0_count, 4)
            bw.w(self.region1_count, 3)
        bw.w(self.preflag, 1)
        bw.w(self.scalefac_scale, 1)
        bw.w(self.count1table_select, 1)


def frame_bytes(granules, main_data, srate_idx=0, bitrate_idx=14, padding=0):
    """One mono MPEG-1 Layer III frame. `main_data` is a list of bit lists."""
    srate = SRATES[srate_idx]
    frame_len = 144 * BITRATES[bitrate_idx] * 1000 // srate + padding
    bw = BitWriter()
    bw.w(0x7FF, 11)
    bw.w(0b11, 2)   # MPEG-1
    bw.w(0b01, 2)   # Layer III
    bw.w(1, 1)      # no CRC
    bw.w(bitrate_idx, 4)
    bw.w(srate_idx, 2)
    bw.w(padding, 1)
    bw.w(0, 1)
    bw.w(0b11, 2)   # mono
    bw.w(0, 2)
    bw.w(0, 1)
    bw.w(1, 1)
    bw.w(0, 2)
    # side info, mono: 17 bytes
    bw.w(0, 9)      # main_data_begin
    bw.w(0, 5)      # private_bits
    bw.w(0, 4)      # scfsi
    for g in granules:
        g.write(bw)
    assert len(bw) == 32 + 17 * 8, len(bw)
    for bits in main_data:
        bw.bits.extend(bits)
    assert len(bw) <= frame_len * 8, (len(bw), frame_len * 8)
    return bw.bytes(pad_to=frame_len)


def ffmpeg_decode(mp3: bytes, channels=1):
    """Decode with ffmpeg, returning float32 samples (no resampling)."""
    with tempfile.NamedTemporaryFile(suffix=".mp3", delete=False) as f:
        f.write(mp3)
        path = f.name
    try:
        p = subprocess.run(
            ["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-ac", str(channels), "-"],
            capture_output=True, check=True)
        return np.frombuffer(p.stdout, dtype=np.float32).astype(np.float64), p.stderr.decode()
    finally:
        os.unlink(path)


# ---------------------------------------------------------------- model side

def imdct36(X):
    """ISO Layer III IMDCT, n = 36."""
    n = 36
    out = np.zeros(n)
    for i in range(n):
        s = 0.0
        for k in range(n // 2):
            s += X[k] * math.cos(math.pi / (2 * n) * (2 * i + 1 + n // 2) * (2 * k + 1))
        out[i] = s
    return out


WIN_NORMAL = np.array([math.sin(math.pi / 36 * (i + 0.5)) for i in range(36)])

ALIAS_CI = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037]
ALIAS_CS = np.array([1.0 / math.sqrt(1.0 + c * c) for c in ALIAS_CI])
ALIAS_CA = np.array([c / math.sqrt(1.0 + c * c) for c in ALIAS_CI])


def alias_reduce(xr):
    """In-place butterflies across the 31 long-block subband boundaries."""
    xr = xr.copy()
    for sb in range(1, 32):
        for i in range(8):
            lo = sb * 18 - 1 - i
            hi = sb * 18 + i
            a, b = xr[lo], xr[hi]
            xr[lo] = a * ALIAS_CS[i] - b * ALIAS_CA[i]
            xr[hi] = b * ALIAS_CS[i] + a * ALIAS_CA[i]
    return xr


def granules_to_subband(granule_spectra):
    """Requantised spectra (long blocks) -> subband samples [slot][32]."""
    overlap = np.zeros((32, 18))
    slots = []
    for xr in granule_spectra:
        xr = alias_reduce(xr)
        block = np.zeros((18, 32))
        for sb in range(32):
            y = imdct36(xr[sb * 18: sb * 18 + 18]) * WIN_NORMAL
            block[:, sb] = y[:18] + overlap[sb]
            overlap[sb] = y[18:]
        for t in range(18):
            for sb in range(1, 32, 2):
                if t % 2 == 1:
                    block[t, sb] = -block[t, sb]
        slots.extend(block)
    return np.array(slots)


def synthesis_basis(slots):
    """Per output sample, the 512 coefficients multiplying D (the window).

    Returns an (nsamples, 512) matrix A with pcm = A @ D.
    """
    nslots = len(slots)
    N = np.array([[math.cos((16 + i) * (2 * k + 1) * math.pi / 64) for k in range(32)]
                  for i in range(64)])
    blocks = slots @ N.T                      # [slot][64] V blocks
    A = np.zeros((nslots * 32, 512))
    for t in range(nslots):
        for j in range(32):
            row = A[t * 32 + j]
            for i in range(16):
                p = j + 32 * i
                a, b = divmod(p, 64)
                m = 2 * a + (1 if b >= 32 else 0)
                s = t - m
                if s >= 0:
                    row[p] = blocks[s][b]
    return A


def synthesise(slots, D):
    return synthesis_basis(slots) @ D
