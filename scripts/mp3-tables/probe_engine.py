"""Batch probe runner: many one-pair probes per ffmpeg invocation."""

import numpy as np
from mp3probe import Granule, frame_bytes, ffmpeg_decode

GLOBAL_GAIN = 150
SCALE = 2.0 ** ((GLOBAL_GAIN - 210) / 4.0)
PART2_3 = 48


class Engine:
    def __init__(self, Minv):
        self.Minv = Minv

    def run(self, probes):
        """probes: list of (bits, table_select, big_values, count1table_select).

        Returns one array of 576 requantised coefficients per probe.
        """
        stream = b""
        for i in range(0, len(probes), 2):
            pair = probes[i:i + 2]
            grans, mains = [], []
            for bits, tsel, bigv, c1 in pair:
                bits = list(bits) + [0] * (PART2_3 - len(bits))
                grans.append(Granule(part2_3_length=PART2_3, big_values=bigv,
                                     global_gain=GLOBAL_GAIN,
                                     table_select=(tsel, tsel, tsel),
                                     count1table_select=c1))
                mains.append(bits[:PART2_3])
            while len(grans) < 2:
                grans.append(Granule())
                mains.append([])
            stream += frame_bytes(grans, mains)
            stream += frame_bytes([Granule(), Granule()], [[], []])
        pcm, err = ffmpeg_decode(stream)
        nframes = (len(probes) + 1) // 2
        assert len(pcm) >= nframes * 2304, (len(pcm), nframes * 2304, err)
        windows = np.stack([pcm[i * 2304:(i + 1) * 2304] for i in range(nframes)])
        coeffs = windows @ self.Minv          # (nframes, 1152)
        out = []
        for i in range(len(probes)):
            out.append(coeffs[i // 2][(i % 2) * 576:((i % 2) + 1) * 576])
        return out


def values(xr, n):
    """First n integer magnitudes of a recovered granule."""
    mag = np.abs(xr[:n]) / SCALE
    return np.round(np.power(np.maximum(mag, 0.0), 0.75)).astype(np.int64)


def signs(xr, n):
    return np.sign(xr[:n]).astype(np.int64)
