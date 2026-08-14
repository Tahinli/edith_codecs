"""Measure the MPEG-1 long-block scalefactor band edges: flat spectrum, one
band attenuated at a time, read the edges off the recovered coefficients."""

import sys
import numpy as np
from mp3probe import BitWriter, Granule, frame_bytes, ffmpeg_decode
from probe_engine import GLOBAL_GAIN, SCALE

TABLE1 = [(1, 0b1), (3, 0b001), (2, 0b01), (3, 0b000)]
SC = 13          # slen1 = slen2 = 3
SLEN = 3
NBANDS = 21      # scalefactors transmitted for sfb 0..20


def probe_bits(band):
    bw = BitWriter()
    for b in range(NBANDS):
        bw.w(7 if b == band else 0, SLEN)
    ln, code = TABLE1[3]                      # (1,1)
    for _ in range(288):
        bw.w(code, ln)
        bw.w(0, 1)
        bw.w(0, 1)
    return bw.bits


def main():
    Minv = np.load("Minv.npy")
    for srate_idx, srate in enumerate([44100, 48000, 32000]):
        stream = b""
        for band in range(NBANDS):
            bits = probe_bits(band)
            g = Granule(part2_3_length=len(bits), big_values=288,
                        global_gain=GLOBAL_GAIN, scalefac_compress=SC,
                        table_select=(1, 1, 1))
            stream += frame_bytes([g, Granule()], [bits, []], srate_idx=srate_idx)
            stream += frame_bytes([Granule(), Granule()], [[], []], srate_idx=srate_idx)
        pcm, err = ffmpeg_decode(stream)
        assert not err.strip(), err
        edges = [0]
        for band in range(NBANDS):
            xr = (pcm[band * 2304:(band + 1) * 2304] @ Minv)[:576]
            att = np.abs(xr) / SCALE
            low = np.where(att < 0.5)[0]       # attenuated by 2^-3.5
            if len(low) == 0:
                print(f"  {srate}: band {band} showed no attenuation")
                continue
            edges.append(int(low[-1]) + 1)
        widths = [edges[i + 1] - edges[i] for i in range(len(edges) - 1)]
        widths.append(576 - edges[-1])
        print(f"{srate} long widths ({sum(widths)}): {widths}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
