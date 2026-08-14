#!/usr/bin/env python3
"""Black-box probing rig for the SBR (HE-AAC v1) tool. WORK IN PROGRESS.

Same method as the AAC tables: the vehicle is an ADTS access unit whose core is
an AAC-LC frame of exactly known bit length -- so the FIL element carrying the
SBR payload starts at a position we control -- followed by a FIL element holding
an sbr_extension_data built field by field. A reference decoder is the only
oracle.

State, so a successor does not repeat the ground already covered:

* The core writer is correct: `sce_core` frames decode clean at every max_sfb,
  and the FIL length accounting is right (the reference decoder reads the
  element without complaint about its framing).
* `sbr_header` is parsed by the reference decoder -- `start_freq=14` comes back
  as "Invalid n_master: 0", which is a semantic complaint about a field it read,
  not a framing one.
* `sbr_data` is NOT yet accepted: the decoder consumes 16 bits more than this
  writer emits ("Expected to read 7 SBR bytes actually read 9"), constant across
  start_freq, stop_freq and freq_res. The gap is most likely the envelope
  Huffman reads for bands beyond the first, whose codebooks are still unknown --
  which is the bootstrap problem: only the first envelope value of a
  frequency-delta frame is raw (6 or 7 bits), every later one is coded.
* The decoder reports the exact byte count its parser consumed, which is a
  precise, cheap observable: that is the instrument to bootstrap the ten SBR
  codebooks by the same prefix walk the AAC tables came from.
"""

import os
import re
import subprocess
import sys

import numpy as np

TABLES = "/home/tahinli/Documents/Code/Rust/edith_codecs/crates/ec-aac/src/tables.rs"
SR_TABLE = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]


def load_table(name):
    src = open(TABLES).read()
    body = re.search(rf"static {name}: \[\(u8, u32\); \d+\] = \[(.*?)\n\];", src, re.S).group(1)
    return [tuple(int(x) for x in m) for m in re.findall(r"\((\d+),\s*(\d+)\)", body)]


def load_swb(kind):
    src = open(TABLES).read()
    body = re.search(rf"pub static SWB_{kind}: \[&\[u16\]; 12\] = \[(.*?)\n\];", src, re.S).group(1)
    return [[int(x) for x in re.findall(r"\d+", row)] for row in re.findall(r"&\[([^\]]*?)\]", body, re.S)]


HCB3 = load_table("HCB3")
HCB_SF = load_table("HCB_SF")
SWB_LONG = load_swb("LONG")


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


def adts(payload, sf_index, chan_cfg=1):
    h = BitW()
    h.w(0xFFF, 12).w(0, 1).w(0, 2).w(1, 1)
    h.w(1, 2).w(sf_index, 4).w(0, 1).w(chan_cfg, 3)
    h.w(0, 1).w(0, 1).w(0, 1).w(0, 1)
    h.w(len(payload) + 7, 13).w(0x7FF, 11).w(0, 2)
    return h.pack(7) + payload


def tuple_index_u(vals, lav, dim):
    span = lav + 1
    idx = 0
    for i in range(dim):
        idx = idx * span + min(abs(vals[i]), lav)
    return idx


def sce_core(w, sf_index, max_sfb, gain=200, quad=(2, 0, 2, 2)):
    """A single_channel_element of exactly known length: codebook 3 throughout,
    the same tuple in every position, so the bit cost is arithmetic."""
    swb = SWB_LONG[sf_index]
    max_sfb = min(max_sfb, len(swb) - 1)
    w.w(0, 3).w(0, 4)  # SCE, instance tag
    w.w(gain, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(max_sfb, 6).w(0, 1)  # ONLY_LONG, sine
    w.w(3, 4)  # one section, codebook 3
    left = max_sfb
    while left >= 31:
        w.w(31, 5)
        left -= 31
    w.w(left, 5)
    for _ in range(max_sfb):
        w.w(HCB_SF[60][1], HCB_SF[60][0])  # delta 0
    w.w(0, 3)  # pulse / tns / gain-control absent
    length, code = HCB3[tuple_index_u(quad, 2, 4)]
    signs = [1 for v in quad if v != 0]
    for _ in range(swb[max_sfb] // 4):
        w.w(code, length)
        for _ in signs:
            w.w(0, 1)  # positive
    return w


def sbr_payload(cfg):
    """sbr_extension_data for one single channel element, from `cfg`."""
    s = BitW()
    s.w(1, 1)  # bs_header_flag
    s.w(cfg["amp_res"], 1)
    s.w(cfg["start_freq"], 4)
    s.w(cfg["stop_freq"], 4)
    s.w(cfg["xover_band"], 3)
    s.w(0, 2)  # bs_reserved
    s.w(1, 1)  # bs_header_extra_1
    s.w(1, 1)  # bs_header_extra_2
    s.w(cfg["freq_scale"], 2)
    s.w(cfg["alter_scale"], 1)
    s.w(cfg["noise_bands"], 2)
    s.w(cfg["limiter_bands"], 2)
    s.w(cfg["limiter_gains"], 2)
    s.w(cfg["interpol_freq"], 1)
    s.w(cfg["smoothing_mode"], 1)
    # sbr_single_channel_element
    s.w(0, 1)  # bs_data_extra
    # sbr_grid: FIXFIX with one envelope
    s.w(0, 2)  # bs_frame_class = FIXFIX
    s.w(0, 2)  # bs_num_env_raw -> num_env = 1
    s.w(cfg["freq_res"], 1)
    # sbr_dtdf
    s.w(0, 1)  # bs_df_env[0] = frequency direction
    s.w(0, 1)  # bs_df_noise[0]
    # sbr_invf: one mode per noise band
    for _ in range(cfg["n_q"]):
        s.w(cfg["invf"], 2)
    # sbr_envelope: first value raw, the rest would need Huffman
    s.w(cfg["env0"], 6 if cfg["amp_res"] else 7)
    for value, (length, code) in cfg.get("env_deltas", []):
        s.w(code, length)
    # sbr_noise: first value raw, the rest Huffman
    s.w(cfg["noise0"], 5)
    for value, (length, code) in cfg.get("noise_deltas", []):
        s.w(code, length)
    # bs_add_harmonic_flag
    if cfg.get("harmonics") is None:
        s.w(0, 1)
    else:
        s.w(1, 1)
        for bit in cfg["harmonics"]:
            s.w(bit, 1)
    s.w(0, 1)  # bs_extended_data
    return s


def frame(cfg, sf_index=7, max_sfb=20):
    w = BitW()
    sce_core(w, sf_index, max_sfb, gain=cfg.get("gain", 200))
    body = sbr_payload(cfg)
    # extension_type nibble plus the payload, padded to whole bytes
    payload_bits = 4 + len(body)
    count = (payload_bits + 7) // 8
    w.w(6, 3)  # FIL
    if count >= 15:
        w.w(15, 4)
        w.w(count - 15 + 1, 8)
    else:
        w.w(count, 4)
    start = len(w)
    w.w(13, 4)  # EXT_SBR_DATA
    w.wbits(body.bits)
    while len(w) - start < count * 8:
        w.w(0, 1)
    w.w(7, 3)  # END
    return adts(w.pack(pad=0), sf_index)


def silent(sf_index):
    w = BitW()
    w.w(0, 3).w(0, 4).w(128, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(0, 6).w(0, 1)
    w.w(0, 3).w(7, 3)
    return adts(w.pack(4), sf_index)


def decode(frames, sf_index, tag=None):
    """(samples, stderr) from ffmpeg for a stream of frames."""
    src = f"/tmp/sbrprobe-{tag or os.getpid()}.aac"
    with open(src, "wb") as f:
        f.write(b"".join(frames))
    r = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", src, "-f", "f32le", "-ac", "1", "-"],
        capture_output=True,
    )
    os.remove(src)
    return np.frombuffer(r.stdout, dtype="<f4").astype(np.float64), r.stderr.decode()


DEFAULT = dict(
    amp_res=1, start_freq=5, stop_freq=3, xover_band=0, freq_scale=0, alter_scale=0,
    noise_bands=0, limiter_bands=0, limiter_gains=2, interpol_freq=1, smoothing_mode=1,
    freq_res=1, invf=1, env0=40, noise0=15, n_q=1,
)

if __name__ == "__main__":
    cfg = dict(DEFAULT)
    frames = [silent(7)] + [frame(cfg) for _ in range(6)] + [silent(7)]
    pcm, err = decode(frames, 7, "smoke")
    print("samples", len(pcm), "err", err.strip()[:300])
