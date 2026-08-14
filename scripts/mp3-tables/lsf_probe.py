"""Short-block reorder probes for the low sampling frequencies: one non-zero
coefficient per granule, so the spectral position it lands in reveals the
scalefactor band widths."""
import numpy as np
from mp3probe import BitWriter, Granule
from learn_window import TABLE1, GLOBAL_GAIN

RATES = {8000: (0, 2), 11025: (0, 0), 12000: (0, 1), 16000: (2, 2), 22050: (2, 0), 24000: (2, 1)}
BITRATES_V2 = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160]


def lsf_frame(granule, main_bits, rate, bitrate_idx=14):
    version_id, rate_idx = RATES[rate]
    frame_len = 72 * BITRATES_V2[bitrate_idx] * 1000 // rate
    bw = BitWriter()
    bw.w(0x7FF, 11); bw.w(version_id, 2); bw.w(0b01, 2); bw.w(1, 1)
    bw.w(bitrate_idx, 4); bw.w(rate_idx, 2); bw.w(0, 1); bw.w(0, 1)
    bw.w(0b11, 2); bw.w(0, 2); bw.w(0, 1); bw.w(1, 1); bw.w(0, 2)
    bw.w(0, 8)   # main_data_begin
    bw.w(0, 1)   # private
    g = granule
    bw.w(g.part2_3_length, 12); bw.w(g.big_values, 9); bw.w(g.global_gain, 8)
    bw.w(g.scalefac_compress, 9)
    bw.w(1 if g.block_type else 0, 1)
    if g.block_type:
        bw.w(g.block_type, 2); bw.w(g.mixed_block, 1)
        for t in g.table_select[:2]:
            bw.w(t, 5)
        for s in g.subblock_gain:
            bw.w(s, 3)
    else:
        for t in g.table_select:
            bw.w(t, 5)
        bw.w(g.region0_count, 4); bw.w(g.region1_count, 3)
    bw.w(g.scalefac_scale, 1); bw.w(g.count1table_select, 1)
    assert len(bw) == 32 + 9 * 8, len(bw)
    bw.bits.extend(main_bits)
    assert len(bw) <= frame_len * 8, (len(bw), frame_len * 8)
    return bw.bytes(pad_to=frame_len)


def pair_bits(x, y):
    bw = BitWriter()
    ln, code = TABLE1[x * 2 + y]
    bw.w(code, ln)
    if x:
        bw.w(0, 1)
    if y:
        bw.w(0, 1)
    return bw.bits


def build(rate, positions):
    stream = b""
    for pos in positions:
        bits = []
        for i in range(288):
            bits += pair_bits(1 if 2 * i == pos else 0, 1 if 2 * i + 1 == pos else 0)
        g = Granule(part2_3_length=len(bits), big_values=288, global_gain=GLOBAL_GAIN,
                    table_select=(1, 1, 1), block_type=2, mixed_block=0)
        stream += lsf_frame(g, bits, rate)
        stream += lsf_frame(Granule(), [], rate)
    return stream


if __name__ == "__main__":
    import sys
    rate = int(sys.argv[1])
    positions = list(range(576))
    open(f"/tmp/lsf{rate}.mp3", "wb").write(build(rate, positions))
    print("wrote", rate, len(positions))
