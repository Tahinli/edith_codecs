#!/usr/bin/env python3
"""Derives the ten SBR Huffman codebooks, black box, from a reference decoder.

The instrument is the reference decoder's own bit accounting.  An SBR payload
rides in a FIL element whose declared byte count `cnt` we choose; the decoder
parses the payload with its own syntax knowledge and complains

    Expected to read <cnt> SBR bytes actually read <n>

whenever `n > cnt`.  Declaring `cnt = 1` therefore turns every probe into a
readout of `n = ceil((4 + bits_the_parser_consumed) / 8)`.

That is byte-granular, and codeword lengths are bits.  Two devices sharpen it:

* A configuration with exactly one envelope band (`bs_freq_res = 0` and an
  `bs_xover_band` chosen so `n_high = 2`) has a payload with *no* Huffman
  codewords at all -- every bit is one this script wrote.  Sweeping such
  configurations validates the frequency-band derivation (`n_master`, `n_high`,
  `n_low`, `n_q`) against the decoder without any codebook being known.
* With the band tables trusted, the same pattern repeated in `m` of the `N - 1`
  Huffman-coded bands makes the consumed length `K + m * L`; the byte count is a
  staircase in `m` whose slope pins `L` exactly.

Values come from a second observable in the same probe: the decoder range-checks
the accumulated envelope and noise scalefactors and says so on stderr.  Bisecting
the raw start value for the point where that complaint appears gives the delta a
codeword carries, with no audio analysis at all.

Run `--stage sweep` to check the band tables, `--stage lengths` for the prefix
walk, `--stage values` for the assignment, `--stage all` for the lot; the result
is written as Rust to crates/ec-aac/src/sbr_tables.rs.
"""

import argparse
import json
import math
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)

from sbrprobe import BitW, adts, sce_core  # noqa: E402

CACHE = os.path.join(HERE, "sbr-cache.json")

# ---------------------------------------------------------------- band tables


def make_bands(start, stop, num):
    """`num` band widths geometrically spaced from `start` to `stop`."""
    base = (stop / start) ** (1.0 / num)
    prod = float(start)
    previous = start
    out = []
    for _ in range(num - 1):
        prod *= base
        present = int(math.floor(prod + 0.5))
        out.append(present - previous)
        previous = present
    out.append(stop - previous)
    return out


# `k0` by SBR sample rate and `bs_start_freq`, measured with `--stage k0`: a
# header whose reconstruction span exceeds the tool's limit is refused with the
# span in the message, and `k2` is known from `bs_stop_freq`, so each refusal
# reads `k0` straight out.  The tail every row shares (0, 1, 2 ... 7, 9, 11, 13,
# 16, 20, 24) continues past the last cell any refusal could reach; those cells
# are the fitted ones, checked by `--stage fill`.
K0_OFFSET = {
    16000: [-8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7],
    22050: [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13],
    24000: [-5, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
    32000: [-6, -4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
    44100: [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
    48000: [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
    64000: [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
    88200: [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24],
    96000: [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24],
}


def start_stop_min(rate):
    """`(start_min, stop_min)`: the QMF band a 3, 4 or 5 kHz border falls in."""
    temp = 3000 if rate < 32000 else (4000 if rate < 64000 else 5000)
    return ((temp << 7) + (rate >> 1)) // rate, ((temp << 8) + (rate >> 1)) // rate


def span_limit(rate):
    """The widest `k2 - k0` the tool will reconstruct, measured the same way."""
    if rate < 32000:
        return 48
    return 35 if rate < 64000 else 32


def f_master(rate, start_freq, stop_freq, freq_scale, alter_scale):
    """The master frequency band table (ISO/IEC 14496-3 4.6.18.3.2.1).

    `rate` is the SBR rate: twice the core's.  The two band-count rules and the
    `k0` table are the reference parser's, read off it by `--stage k0` and
    `--stage sweep`; the shapes are the standard's.
    """
    offsets = K0_OFFSET.get(rate)
    if offsets is None:
        return None
    start_min, stop_min = start_stop_min(rate)
    k0 = start_min + offsets[start_freq]
    if stop_freq < 14:
        k2 = stop_min + sum(sorted(make_bands(stop_min, 64, 13))[:stop_freq])
    elif stop_freq == 14:
        k2 = 2 * k0
    else:
        k2 = 3 * k0
    k2 = min(k2, 64)
    if k2 <= k0 or k2 - k0 > span_limit(rate):
        return None

    if freq_scale == 0:
        # Bands of a constant width, an even number of them: the count is the
        # span in whole double-width steps, biased by one step when the width
        # is doubled.  Both edges of that rule are measured, not assumed.
        dk = 2 if alter_scale else 1
        num_bands = 2 * ((k2 - k0 + 2 * (dk - 1)) // (2 * dk))
        if num_bands <= 0:
            return None
        vk = [dk] * num_bands
        k2diff = k2 - k0 - num_bands * dk
        if k2diff < 0:
            at, incr = 0, 1
        else:
            at, incr = num_bands - 1, -1
        while k2diff and 0 <= at < num_bands:
            vk[at] -= incr
            at += incr
            k2diff += incr
        table = [k0]
        for d in vk:
            if d <= 0:
                return None
            table.append(table[-1] + d)
        return table

    half_bands = 7 - freq_scale
    two_regions = 49 * k2 > 110 * k0
    k1 = 2 * k0 if two_regions else k2
    num_bands_0 = 2 * int(math.floor(half_bands * math.log2(k1 / k0) + 0.5))
    if num_bands_0 <= 0:
        return None
    vk0 = sorted(make_bands(k0, k1, num_bands_0))
    vdk0_max = vk0[-1]
    table = [k0]
    for d in vk0:
        if d <= 0:
            return None
        table.append(table[-1] + d)
    if not two_regions:
        return table
    invwarp = 1.0 / 1.3 if alter_scale else 1.0
    num_bands_1 = 2 * int(math.floor(half_bands * invwarp * math.log2(k2 / k1) + 0.5))
    if num_bands_1 <= 0:
        return None
    vk1 = sorted(make_bands(k1, k2, num_bands_1))
    if vdk0_max > vk1[0]:
        change = min(vdk0_max - vk1[0], (vk1[-1] - vk1[0]) >> 1)
        vk1[0] += change
        vk1[-1] -= change
        vk1.sort()
    for d in vk1:
        if d <= 0:
            return None
        table.append(table[-1] + d)
    return table


def freq_tables(rate, cfg):
    """`(n_master, n_high, n_low, n_q, f_high, f_low, f_noise)` or None."""
    master = f_master(rate, cfg["start_freq"], cfg["stop_freq"], cfg["freq_scale"], cfg["alter_scale"])
    if master is None:
        return None
    n_master = len(master) - 1
    xover = cfg["xover_band"]
    if xover > n_master:
        return None
    n_high = n_master - xover
    if n_high < 1:
        return None
    n_low = (n_high + 1) >> 1
    f_high = master[xover:]
    f_low = [f_high[0]]
    if n_high & 1:
        f_low += [f_high[2 * k - 1] for k in range(1, n_low + 1)]
    else:
        f_low += [f_high[2 * k] for k in range(1, n_low + 1)]
    kx = f_high[0]
    k2 = master[-1]
    n_q = max(1, int(math.floor(cfg["noise_bands"] * math.log2(k2 / kx) + 0.5)))
    if n_q > 5:
        return None
    f_noise = [f_low[0]]
    temp = 0
    for k in range(1, n_q + 1):
        temp += (n_low - temp) // (n_q + 1 - k)
        f_noise.append(f_low[temp])
    return dict(
        n_master=n_master, n_high=n_high, n_low=n_low, n_q=n_q,
        f_high=f_high, f_low=f_low, f_noise=f_noise, kx=kx, k2=k2,
    )


# ------------------------------------------------------------- payload writer

# Stand-in bits for a Huffman position whose codeword this probe does not care
# about; the parser reads as many as its own syntax says, not as many as these.
FILL = [0] * 24
# Raw start value widths: `(balance, amp_res)` for an envelope, and the noise
# floor's. Measured by `--stage widths`, which reads them off the byte count.
RAW_ENV = {(0, 0): 7, (0, 1): 6, (1, 0): 6, (1, 1): 5}
RAW_NOISE = 5


def amp_res_of(cfg):
    """FIXFIX with a single envelope forces the 1.5 dB resolution."""
    return 0 if cfg["num_env"] == 1 else cfg["amp_res"]


def env_kind(cfg, balance, direction):
    res = "30" if amp_res_of(cfg) else "15"
    return f"ENV{'B' if balance else ''}{res}_{direction}"


def noise_kind(balance, direction):
    return f"NOISE{'B' if balance else ''}_{direction}"


def channel_slots(cfg, tables, balance):
    """The Huffman slots one channel spends, as `(kind, raw_bits)` steps."""
    kinds = []
    raw = 0
    bands = tables["n_high"] if cfg["freq_res"] else tables["n_low"]
    for i in range(cfg["num_env"]):
        if cfg["df_env"][i] == 0:
            raw += RAW_ENV[(balance, amp_res_of(cfg))]
            kinds += [env_kind(cfg, balance, "F")] * (bands - 1)
        else:
            kinds += [env_kind(cfg, balance, "T")] * bands
    num_noise = 1 if cfg["num_env"] == 1 else 2
    for i in range(num_noise):
        if cfg["df_noise"][i] == 0:
            raw += RAW_NOISE
            kinds += [noise_kind(balance, "F")] * (tables["n_q"] - 1)
        else:
            kinds += [noise_kind(balance, "T")] * tables["n_q"]
    return kinds, raw


def plan(cfg, tables):
    """`(kinds, known_bits)`: the codebook of every Huffman slot, in order, and
    the bits the payload spends outside them."""
    n = 0
    if cfg.get("header", 1):
        n += 1 + 1 + 4 + 4 + 3 + 2 + 1 + 1
        n += 5 if cfg.get("extra1", 1) else 0
        n += 6 if cfg.get("extra2", 1) else 0
    else:
        n += 1
    cpe = cfg.get("element", "sce") == "cpe"
    n += 1 + ((8 if cpe else 4) if cfg.get("data_extra") else 0)
    channels = [0]
    if cpe:
        n += 1  # bs_coupling
        channels = [0, 1] if cfg.get("coupling", 1) else [0, 1]
    n += 2 + 2 + 1  # sbr_grid, FIXFIX
    num_noise = 1 if cfg["num_env"] == 1 else 2
    n += (cfg["num_env"] + num_noise) * len(channels)  # sbr_dtdf per channel
    n += 2 * tables["n_q"]  # sbr_invf, sent once under coupling
    kinds = []
    for ch in channels:
        balance = 1 if (cpe and cfg.get("coupling", 1) and ch == 1) else 0
        k, raw = channel_slots(cfg, tables, balance)
        kinds += k
        n += raw
    for ch in channels:
        n += 1 + (tables["n_high"] if cfg.get("add_harmonic", [0, 0])[ch] else 0)
    n += 1
    if cfg.get("extended"):
        n += 4 + 8 * cfg["extended"]
    return kinds, n


def known_bits(cfg, tables):
    return plan(cfg, tables)[1]


def sbr_bits(cfg, tables, words):
    """The SBR payload as a bit list; `words` fills the Huffman slots in order."""
    s = BitW()
    words = list(words)

    def slot():
        s.wbits(words.pop(0) if words else FILL)

    if cfg.get("header", 1):
        s.w(1, 1)
        s.w(cfg["amp_res"], 1)
        s.w(cfg["start_freq"], 4)
        s.w(cfg["stop_freq"], 4)
        s.w(cfg["xover_band"], 3)
        s.w(0, 2)
        s.w(1 if cfg.get("extra1", 1) else 0, 1)
        s.w(1 if cfg.get("extra2", 1) else 0, 1)
        if cfg.get("extra1", 1):
            s.w(cfg["freq_scale"], 2)
            s.w(cfg["alter_scale"], 1)
            s.w(cfg["noise_bands"], 2)
        if cfg.get("extra2", 1):
            s.w(cfg["limiter_bands"], 2)
            s.w(cfg["limiter_gains"], 2)
            s.w(cfg["interpol_freq"], 1)
            s.w(cfg["smoothing_mode"], 1)
    else:
        s.w(0, 1)
    cpe = cfg.get("element", "sce") == "cpe"
    extra = 1 if cfg.get("data_extra") else 0
    s.w(extra, 1)
    if extra:
        s.w(0, 8 if cpe else 4)
    if cpe:
        s.w(1 if cfg.get("coupling", 1) else 0, 1)
    num_env = cfg["num_env"]
    num_noise = 1 if num_env == 1 else 2
    channels = [0, 1] if cpe else [0]
    s.w(0, 2)  # bs_frame_class = FIXFIX
    s.w(int(math.log2(num_env)), 2)
    s.w(cfg["freq_res"], 1)
    for _ in channels:
        for i in range(num_env):
            s.w(cfg["df_env"][i], 1)
        for i in range(num_noise):
            s.w(cfg["df_noise"][i], 1)
    for _ in range(tables["n_q"]):
        s.w(cfg["invf"], 2)
    for ch in channels:
        balance = 1 if (cpe and cfg.get("coupling", 1) and ch == 1) else 0
        bands = tables["n_high"] if cfg["freq_res"] else tables["n_low"]
        for i in range(num_env):
            if cfg["df_env"][i] == 0:
                key = "env0b" if balance else "env0"
                s.w(cfg[key][i], RAW_ENV[(balance, amp_res_of(cfg))])
                for _ in range(bands - 1):
                    slot()
            else:
                for _ in range(bands):
                    slot()
        for i in range(num_noise):
            if cfg["df_noise"][i] == 0:
                s.w(cfg["noise0b" if balance else "noise0"][i], RAW_NOISE)
                for _ in range(tables["n_q"] - 1):
                    slot()
            else:
                for _ in range(tables["n_q"]):
                    slot()
    for ch in channels:
        on = cfg.get("add_harmonic", [0, 0])[ch]
        s.w(1 if on else 0, 1)
        if on:
            for _ in range(tables["n_high"]):
                s.w(0, 1)
    if cfg.get("extended"):
        s.w(1, 1)
        s.w(cfg["extended"], 4)
        s.w(0, 8 * cfg["extended"])
    else:
        s.w(0, 1)
    return s


DEFAULT = dict(
    amp_res=0, start_freq=5, stop_freq=3, xover_band=0, freq_scale=0, alter_scale=0,
    noise_bands=0, limiter_bands=0, limiter_gains=2, interpol_freq=1, smoothing_mode=1,
    freq_res=0, invf=1, num_env=1, df_env=[0, 0, 0, 0, 0, 0, 0, 0], df_noise=[0, 0],
    env0=[60] * 8, noise0=[0, 0], env0b=[32] * 8, noise0b=[16, 16],
    sf_index=7, element="sce", coupling=1,
    header=1, extra1=1, extra2=1, data_extra=0, add_harmonic=[0, 0], extended=0,
)


def core_element(w, sf_index, max_sfb, channels):
    """An AAC-LC core element of exactly known length, one or two channels."""
    if channels == 1:
        sce_core(w, sf_index, max_sfb)
        return
    body = BitW()
    sce_core(body, sf_index, max_sfb)
    ics = body.bits[7:]  # everything after the element id and instance tag
    w.w(1, 3).w(0, 4).w(0, 1)  # CPE, tag, common_window = 0
    w.wbits(ics)
    w.wbits(ics)


def frame(cfg, tables, words=(), cnt=1):
    """One ADTS access unit: an AAC-LC core plus the SBR FIL element."""
    w = BitW()
    core_element(w, cfg["sf_index"], 4, 2 if cfg.get("element") == "cpe" else 1)
    body = sbr_bits(cfg, tables, words)
    w.w(6, 3)  # FIL
    if cnt >= 15:
        w.w(15, 4)
        w.w(cnt - 15 + 1, 8)
    else:
        w.w(cnt, 4)
    w.w(13, 4)  # EXT_SBR_DATA
    w.wbits(body.bits)
    w.w(7, 3)  # END
    # Slack, so a parser that reads a few bits more than this writer laid down
    # still has bits to read: what it consumes is the measurement.
    w.w(0, 32)
    return adts(w.pack(pad=0), cfg["sf_index"], 2 if cfg.get("element") == "cpe" else 1)


# -------------------------------------------------------------------- oracle

READ = re.compile(r"Expected to read (\d+) SBR bytes actually read (\d+)")


class Oracle:
    """Runs the reference decoder on probe frames, with a disk cache."""

    def __init__(self, workers=8):
        self.workers = workers
        self.hits = 0
        self.runs = 0
        try:
            with open(CACHE) as f:
                self.cache = json.load(f)
        except (OSError, ValueError):
            self.cache = {}

    def save(self):
        tmp = CACHE + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self.cache, f)
        os.replace(tmp, CACHE)

    def _run(self, item):
        serial, key, data = item
        src = f"/tmp/sbrtables-{os.getpid()}-{serial}.aac"
        with open(src, "wb") as f:
            f.write(data)
        r = subprocess.run(
            ["ffmpeg", "-v", "error", "-i", src, "-f", "f32le", "-ac", "1", "-"],
            capture_output=True,
        )
        os.remove(src)
        return key, r.stderr.decode(errors="replace")

    def many(self, items):
        """`items` is a list of `(key, frame_bytes)`; returns {key: stderr}."""
        todo = [(k, d) for k, d in items if k not in self.cache]
        seen = set()
        uniq = []
        for k, d in todo:
            if k not in seen:
                seen.add(k)
                uniq.append((k, d))
        if uniq:
            numbered = [(i, k, d) for i, (k, d) in enumerate(uniq)]
            with ThreadPoolExecutor(max_workers=self.workers) as pool:
                for key, err in pool.map(self._run, numbered):
                    self.cache[key] = err
                    self.runs += 1
        self.hits += len(items) - len(uniq)
        return {k: self.cache[k] for k, _ in items}

    def one(self, key, data):
        return self.many([(key, data)])[key]


def bytes_read(err):
    """The byte count the parser consumed, from its last complaint."""
    m = READ.findall(err)
    return int(m[-1][1]) if m else None


def probe_frames(cfg, tables, words, reps=2):
    """A probe file: silence, a header-bearing frame when the probe carries no
    header of its own, then the probe frames."""
    from sbrprobe import silent

    out = [silent(cfg["sf_index"])]
    if not cfg.get("header", 1):
        lead = dict(cfg, header=1, words=None)
        _, known = plan(lead, tables)
        out += [frame(lead, tables, [], cnt=(known + 4 + 7) // 8)]
    f = frame(cfg, tables, words)
    return b"".join(out + [f] * reps)


def key_for(cfg, tables, words):
    return json.dumps(
        [
            {k: v for k, v in sorted(cfg.items())},
            [tables["n_high"], tables["n_low"], tables["n_q"]],
            ["".join(map(str, w)) for w in words],
        ],
        sort_keys=True,
    )


def measure(oracle, cases):
    """`cases` is a list of `(cfg, tables, words)`; returns `(bytes_read, err)`."""
    items = [(key_for(c, t, w), probe_frames(c, t, w)) for c, t, w in cases]
    got = oracle.many(items)
    return [(bytes_read(got[key]), got[key]) for key, _ in items]


def predicted(cfg, tables, words):
    bits = known_bits(cfg, tables) + sum(len(w) for w in words)
    return (bits + 4 + 7) // 8


# ----------------------------------------------------------------- codebooks

KINDS = [
    "ENV15_F", "ENV15_T", "ENV30_T", "ENV30_F", "NOISE_T", "NOISE_F",
    "ENVB15_F", "ENVB15_T", "ENVB30_T", "ENVB30_F", "NOISEB_T", "NOISEB_F",
]


# The values the parser assumes when `bs_header_extra_1` is absent.  A probe
# that carries exactly these can drop the field without changing a single
# derived table, which is what makes the five-bit shift usable as a knob.
EXTRA1_DEFAULTS = dict(freq_scale=2, alter_scale=1, noise_bands=2)


def find_config(sf_index, over, n_high=None, n_low=None, n_q=None, defaults=True):
    """A header whose derived band tables have the shape a probe needs."""
    rate = SBR_RATE_FOR_SF[sf_index]
    scales = (2,) if defaults else (0, 1, 2, 3)
    alters = (1,) if defaults else (0, 1)
    noises = (2,) if defaults else (0, 1, 2, 3)
    for start_freq in range(16):
        for stop_freq in range(16):
            for freq_scale in scales:
                for alter_scale in alters:
                    for noise_bands in noises:
                        base = dict(DEFAULT)
                        base.update(
                            sf_index=sf_index, start_freq=start_freq, stop_freq=stop_freq,
                            freq_scale=freq_scale, alter_scale=alter_scale,
                            noise_bands=noise_bands, xover_band=0,
                        )
                        t0 = freq_tables(rate, base)
                        if t0 is None:
                            continue
                        for xover in range(min(8, t0["n_master"])):
                            cfg = dict(base, xover_band=xover)
                            t = freq_tables(rate, cfg)
                            if t is None or t["kx"] > 32:
                                continue
                            if n_high is not None and t["n_high"] != n_high:
                                continue
                            if n_low is not None and t["n_low"] != n_low:
                                continue
                            if n_q is not None and t["n_q"] != n_q:
                                continue
                            cfg.update(over)
                            return cfg, t
    return None


# One probe per codebook.  Each is a payload in which the slots of that
# codebook come last and everything before them is either a field this script
# wrote or a codeword already derived: the bits the parser spends past the
# known part are the codeword under test, and nothing else.
RECIPES = {
    "ENV15_F": dict(over=dict(num_env=1, amp_res=0, df_env=[0] * 8, df_noise=[0, 0],
                              freq_res=1, element="sce"), n_high=2, n_q=1),
    "ENV15_T": dict(over=dict(num_env=2, amp_res=0, df_env=[0, 1] + [0] * 6,
                              df_noise=[0, 0], freq_res=0, element="sce"),
                    n_high=2, n_low=1, n_q=1),
    "ENV30_T": dict(over=dict(num_env=2, amp_res=1, df_env=[0, 1] + [0] * 6, env0=[32] * 8,
                              env0b=[16] * 8, df_noise=[0, 0], freq_res=0, element="sce"),
                    n_high=2, n_low=1, n_q=1),
    "ENV30_F": dict(over=dict(num_env=2, amp_res=1, df_env=[0] * 8, df_noise=[0, 0], env0=[32] * 8,
                              env0b=[16] * 8, freq_res=1, element="sce"), n_high=2, n_q=1),
    "NOISE_T": dict(over=dict(num_env=2, amp_res=0, df_env=[0] * 8, df_noise=[0, 1],
                              freq_res=0, element="sce"), n_high=2, n_low=1, n_q=1),
    "NOISE_F": dict(over=dict(num_env=2, amp_res=0, df_env=[0] * 8, df_noise=[0, 0],
                              freq_res=0, element="sce"), n_q=2),
    "ENVB15_F": dict(over=dict(num_env=1, amp_res=0, df_env=[0] * 8, df_noise=[0, 0],
                               freq_res=1, element="cpe", coupling=1), n_high=2, n_q=1),
    "ENVB15_T": dict(over=dict(num_env=2, amp_res=0, df_env=[0, 1] + [0] * 6,
                               df_noise=[0, 0], freq_res=0, element="cpe", coupling=1),
                     n_high=2, n_low=1, n_q=1),
    "ENVB30_T": dict(over=dict(num_env=2, amp_res=1, df_env=[0, 1] + [0] * 6, env0=[32] * 8,
                               env0b=[16] * 8, df_noise=[0, 0], freq_res=0, element="cpe", coupling=1),
                     n_high=2, n_low=1, n_q=1),
    "ENVB30_F": dict(over=dict(num_env=2, amp_res=1, df_env=[0] * 8, df_noise=[0, 0], env0=[32] * 8,
                               env0b=[16] * 8, freq_res=1, element="cpe", coupling=1), n_high=2, n_q=1),
    "NOISEB_T": dict(over=dict(num_env=2, amp_res=0, df_env=[0] * 8, df_noise=[0, 1],
                               freq_res=0, element="cpe", coupling=1),
                     n_low=2, n_q=1),
    "NOISEB_F": dict(over=dict(num_env=2, amp_res=0, df_env=[0] * 8, df_noise=[0, 0],
                               freq_res=0, element="cpe", coupling=1), n_q=2),
}


def recipe_config(kind, sf_index=7):
    r = RECIPES[kind]
    found = find_config(sf_index, r["over"], n_high=r.get("n_high"),
                        n_low=r.get("n_low"), n_q=r.get("n_q"))
    if found is None:
        found = find_config(sf_index, r["over"], n_high=r.get("n_high"),
                            n_low=r.get("n_low"), n_q=r.get("n_q"), defaults=False)
    if found is None:
        return None
    cfg, tables = found
    kinds, _ = plan(cfg, tables)
    if not kinds or kinds[-1] != kind:
        return None
    return cfg, tables


def knob_variants(cfg, tables):
    """Payloads that differ only in fields of a size this script knows.

    A byte count pins the bits the parser read to within a byte; shifting the
    payload by 4, 5 and 6 bits and reading the count again pins it exactly.
    None of these knobs changes the frequency tables or the slots.
    """
    out = []
    base = known_bits(cfg, tables)
    can_drop_extra1 = all(cfg[k] == v for k, v in EXTRA1_DEFAULTS.items())
    # Only fields *before* the slot under test can serve as a knob: the parser
    # stops inside the padded slot word, so everything this script writes after
    # it sits at an offset the parser never reaches.
    for data_extra in (0, 1):
        for extra2 in (1, 0):
            for extra1 in ((1, 0) if can_drop_extra1 else (1,)):
                v = dict(cfg, data_extra=data_extra, extra2=extra2, extra1=extra1)
                delta = known_bits(v, tables) - base
                out.append((v, delta))
    seen = set()
    uniq = []
    for v, delta in out:
        if delta % 8 in seen:
            continue
        seen.add(delta % 8)
        uniq.append((v, delta))
    return uniq


# A decoded value the tool refuses is a value it never finished reading, so a
# probe that says this measured nothing.
BAD_VALUE = re.compile(r"is invalid|out of range|overflow|SBR reset failed")

# Raw start values to try: a noise floor's accumulated value has no centre that
# survives both ends of its table (0 and 30 are the limits the tool enforces),
# so both ends are tried and the probe it does not refuse is the one that
# measured something.
# A balance value is checked doubled (a coupled channel's noise floor is the
# base channel's combined with the ratio the balance value codes), so 16 -- the
# middle of the raw field -- can already read as 32 and refuse on its own,
# before the codeword under test ever contributes: the extra, smaller-magnitude
# pairs are the fallback once that is observed (`exact_bits` requires a sane
# 1..32 answer, not just an unrefused one).
RAW_TRIES = [(0, 16), (30, 16), (0, 0), (30, 30), (8, 8), (0, 8), (8, 0)]


def as_bits(length, code):
    return [(code >> (length - 1 - i)) & 1 for i in range(length)]


def filler(books, kind, extra=0):
    """A derived codeword of this book, `extra` bits longer than its shortest.

    Swapping one filler for a longer one shifts the payload by a known number of
    bits, which is the knob a coupled probe would otherwise lack.
    """
    if kind not in books:
        return None
    lengths = sorted({l for l, _ in books[kind]})
    want = lengths[0] + extra
    for length, code in sorted(books[kind]):
        if length == want:
            return as_bits(length, code)
    return None


def fill_words(cfg, tables, kind, word, books, extra=0):
    """Slot words: `word` in the last slot of `kind`, derived codewords before."""
    kinds, _ = plan(cfg, tables)
    last = max(i for i, k in enumerate(kinds) if k == kind)
    # An envelope value has room for any delta its table holds, so swapping an
    # envelope filler is safe; a noise floor's does not, so it is a last resort.
    order = [i for i, k in enumerate(kinds) if i != last and k.startswith("ENV")]
    order += [i for i, k in enumerate(kinds) if i != last and not k.startswith("ENV")]
    swap_at = None
    for i in order:
        if filler(books, kinds[i], extra) is not None:
            swap_at = i
            break
    words = []
    shifted = not extra
    for i, k in enumerate(kinds):
        if i == last:
            words.append(list(word))
            continue
        if not shifted and i == swap_at:
            got = filler(books, k, extra)
            if got is not None:
                words.append(got)
                shifted = True
                continue
        got = filler(books, k, 0)
        if got is None:
            return None
        words.append(got)
    return words if shifted else None


def exact_bits(oracle, cfg, tables, kind, word, books):
    """The length of the codeword `word` starts with, exactly.

    A byte count pins the bits the parser read to within a byte.  Shifting the
    payload ahead of the slot -- by dropping a header group, by adding the
    reserved data-extra field, or by swapping one already-derived filler
    codeword for a longer one -- and reading the count again pins it exactly.
    """
    knobs = knob_variants(cfg, tables)
    cases = []
    residues = set()
    for extra in range(0, 8):
        words = fill_words(cfg, tables, kind, word, books, extra)
        if words is None:
            continue
        for v, _delta in knobs:
            base = known_bits(v, tables) + sum(len(w) for w in words[:-1])
            cases.append((v, words, base))
            residues.add(base % 8)
        if len(residues) == 8:
            break
    if len(residues) < 8:
        return None
    # Refusing a value stops the parse right after the codeword that carried
    # it, so the fields the payload spends after the slot go unread: that is a
    # measurement too, of the same codeword, with a known amount less around it.
    trailing = 1 + (2 if cfg.get("element") == "cpe" else 1)
    for noise0, noise0b in RAW_TRIES:
        tries = [(dict(v, noise0=[noise0] * 2, noise0b=[noise0b] * 2), w, b)
                 for v, w, b in cases]
        got = measure(oracle, [(v, tables, w) for v, w, _ in tries])
        lo, hi, bad = -10 ** 6, 10 ** 6, False
        for (_v, _w, base), (n, err) in zip(tries, got):
            if n is None:
                bad = True
                break
            top = 8 * n - 4 - base + (trailing if BAD_VALUE.search(err) else 0)
            lo, hi = max(lo, top - 7), min(hi, top)
        if not bad and lo == hi and 1 <= lo <= 32:
            return lo
    return None


def walk(oracle, kind, cfg, tables, books, verbose=True):
    """Enumerates a complete prefix code by walking its intervals.

    A complete code tiles the unit interval: the codeword at `cur` is the first
    `L` bits of `cur`, and the next one starts at `cur + 2^-L`.  One length
    measurement per codeword enumerates the whole book, and the walk landing
    exactly on 1 is the completeness check.
    """
    codes = []
    cur, depth, kraft = 0, 0, 0.0
    while True:
        bits = [(cur >> (depth - 1 - i)) & 1 for i in range(depth)]
        length = exact_bits(oracle, cfg, tables, kind, bits + [0] * 32, books)
        if length is None and kraft < 1.0:
            # A complete code ends on a single interval, and its width is the
            # last codeword's length: that one needs no probe.
            rest = 1.0 - kraft
            exact = int(round(math.log2(1.0 / rest)))
            if abs(rest - 2.0 ** -exact) < 1e-12 and 1 <= exact <= 32:
                length = exact
                if verbose:
                    print(f"  {kind}: last codeword closed by completeness at {exact} bits")
        if length is None or not 1 <= length <= 32:
            print(f"  {kind}: no length after {len(codes)} at "
                  f"{''.join(map(str, bits)) or '(empty)'}: {length}")
            return None
        while depth < length:
            cur <<= 1
            depth += 1
        codes.append((length, cur >> (depth - length)))
        kraft += 2.0 ** -length
        cur += 1 << (depth - length)
        while depth and cur % 2 == 0:
            cur >>= 1
            depth -= 1
        if depth == 0 and cur >= 1:
            break
        if len(codes) > 200:
            print(f"  {kind}: walk did not close")
            return None
    if verbose:
        print(f"  {kind}: {len(codes)} codewords, Kraft {kraft:.9f}, lengths "
              f"{min(l for l, _ in codes)}..{max(l for l, _ in codes)}")
    return codes


def bootstrap(oracle, kind, cfg, tables, books, span=(1, 33)):
    """The first codeword of a book, when there is no filler for it yet.

    Every slot of the kind carries the same candidate word, so the length that
    explains the bit count is the one the parser actually read.
    """
    kinds, _ = plan(cfg, tables)
    if kinds.count(kind) == 1:
        books[kind] = [(1, 0)]
        return True
    for length in range(span[0], span[1]):
        books[kind] = [(length, 0)]
        if exact_bits(oracle, cfg, tables, kind, [0] * 33, books) == length:
            return True
    books.pop(kind, None)
    return False


def stage_books(oracle, sf_index=7, only=None, books=None):
    """Derives every codebook, each with the ones it leans on already in hand."""
    books = books if books is not None else {}
    out = {}
    for kind in KINDS:
        if only and kind not in only:
            continue
        made = recipe_config(kind, sf_index)
        if made is None:
            print(f"  {kind}: no configuration")
            continue
        cfg, tables = made
        kinds, _ = plan(cfg, tables)
        missing = {k for k in kinds if k != kind and k not in books}
        if missing:
            print(f"  {kind}: needs {sorted(missing)} first")
            continue
        if not bootstrap(oracle, kind, cfg, tables, books):
            print(f"  {kind}: no bootstrap codeword")
            continue
        codes = walk(oracle, kind, cfg, tables, books)
        if codes is None:
            books.pop(kind, None)
            continue
        out[kind] = books[kind] = codes
    return out


# -------------------------------------------------------------- values stage

# `env_facs_q`/`noise_facs_q` are the reference decoder's own accumulated
# scalefactor: it prints the value it computed the instant that value leaves
# the valid range, which is `raw + delta` for a plain codebook or, measured
# below, `2 * (raw + delta)` for a balance one (the coupled channel's value is
# reconstructed from double the coded ratio). Placing the codeword under test
# at `raw = 0` and at the field's own maximum brackets every delta the ten
# tables carry: a delta that overflows high is read straight off the
# maximum-raw probe, a delta that overflows low wraps through the decoder's
# own byte-sized accumulator and is read off the zero-raw probe once
# unwrapped -- the write-up above is what pins the wrap width at 256 and the
# scale at 1 or 2, not an assumption.
VALUE_RE_CACHE = {}


def value_re(field):
    if field not in VALUE_RE_CACHE:
        VALUE_RE_CACHE[field] = re.compile(rf"{field} (-?\d+) is invalid")
    return VALUE_RE_CACHE[field]


def values_config(kind, sf_index=7):
    """A config in which `kind` carries exactly one codeword, plus the raw
    field/width/scale that codeword's delta is read against."""
    balance = kind.startswith("ENVB") or kind.startswith("NOISEB")
    is_env = kind.startswith("ENV")
    amp_res = 1 if "30" in kind else 0
    scale = 2 if balance else 1
    element = "cpe" if balance else "sce"
    if kind.endswith("_F"):
        if is_env:
            over = dict(element=element, coupling=1, num_env=1, amp_res=amp_res,
                        df_env=[0] * 8, df_noise=[0, 0], freq_res=1)
            found = find_config(sf_index, over, n_high=2, n_q=1)
            key, width = ("env0b" if balance else "env0"), RAW_ENV[(1 if balance else 0, amp_res)]
        else:
            over = dict(element=element, coupling=1, num_env=1, amp_res=0,
                        df_env=[0] * 8, df_noise=[0, 0], freq_res=0)
            found = find_config(sf_index, over, n_q=2)
            key, width = ("noise0b" if balance else "noise0"), RAW_NOISE
        if found is None:
            return None
        cfg, tables = found
    else:
        found = recipe_config(kind, sf_index)
        if found is None:
            return None
        cfg, tables = found
        if is_env:
            key, width = ("env0b" if balance else "env0"), RAW_ENV[(1 if balance else 0, amp_res_of(cfg))]
        else:
            key, width = ("noise0b" if balance else "noise0"), RAW_NOISE
    kinds, _ = plan(cfg, tables)
    if kinds.count(kind) != 1:
        return None
    return cfg, tables, key, width, scale


def book_values(oracle, kind, cfg, tables, key, width, scale, books):
    """`{(length, code): delta}` for every codeword `stage_books` found."""
    codes = sorted(books[kind])
    raw_max = (1 << width) - 1
    field = "env_facs_q" if kind.startswith("ENV") else "noise_facs_q"
    pat = value_re(field)
    cases, meta = [], []
    for length, code in codes:
        # The exact codeword, no trailing pad: `stage_books` already closed the
        # length for every codeword here, so (unlike the length-search bisection,
        # which pads because the length is what it is looking for) padding would
        # only shift every field after this slot and desync the frame.
        word = as_bits(length, code)
        words = fill_words(cfg, tables, kind, word, books, extra=0)
        if words is None:
            continue
        for raw in (0, raw_max):
            v = dict(cfg)
            arr = list(v[key])
            arr[0] = raw
            v[key] = arr
            cases.append((v, tables, words))
            meta.append((length, code, raw))
    got = measure(oracle, cases)
    seen = {}
    crashed = set()
    clean = set()
    for (length, code, raw), (_n, err) in zip(meta, got):
        m = pat.search(err)
        if not m:
            # Not a range-check complaint: either a clean decode (only the
            # routine byte-count line) or the parser aborting the element for
            # an unrelated reason -- only the former licenses the "both ends
            # valid" inference below, so the two are told apart here.
            if "is not allocated" in err or "Invalid data found" in err:
                crashed.add((length, code))
            else:
                clean.add((length, code))
            continue
        val = int(m.group(1))
        if raw == 0:
            v = val % 256
            if v > 128:
                v -= 256
            if v % scale:
                continue
            delta = v // scale
        else:
            if val % scale:
                continue
            delta = val // scale - raw
        seen.setdefault((length, code), set()).add(delta)
    values, gaps = {}, []
    for length, code in codes:
        vs = seen.get((length, code))
        if not vs:
            if (length, code) in clean and (length, code) not in crashed:
                # Neither end complained and neither crashed either: the
                # codeword's delta and the raw value it rides with both
                # stayed inside `[0, raw_max]`, which only `delta = 0` can do
                # at *both* ends of the field at once.
                values[(length, code)] = 0
            else:
                gaps.append((length, code, "crashed" if (length, code) in crashed else "no signal"))
            continue
        if len(vs) > 1:
            gaps.append((length, code, vs))
            continue
        values[(length, code)] = next(iter(vs))
    return values, gaps


def stage_values(oracle, books, sf_index=7):
    """Assigns the delta value every codeword of every closed book carries."""
    values = {}
    for kind in KINDS:
        if kind not in books:
            print(f"  {kind}: no lengths, skipped")
            continue
        made = values_config(kind, sf_index)
        if made is None:
            print(f"  {kind}: no single-codeword configuration for values")
            continue
        cfg, tables, key, width, scale = made
        vals, gaps = book_values(oracle, kind, cfg, tables, key, width, scale, books)
        if gaps:
            print(f"  {kind}: {len(gaps)}/{len(books[kind])} codewords unresolved: {gaps[:5]}")
        if len(vals) == len(books[kind]):
            values[kind] = vals
            lo, hi = min(vals.values()), max(vals.values())
            print(f"  {kind}: {len(vals)} values, range {lo}..{hi}")
        else:
            print(f"  {kind}: incomplete ({len(vals)}/{len(books[kind])}), not emitted")
    return values


def write_rust(books, values, path=None):
    """Emits the closed, fully-valued books as `crates/ec-aac/src/sbr_tables.rs`."""
    path = path or os.path.join(ROOT, "crates", "ec-aac", "src", "sbr_tables.rs")
    ready = [k for k in KINDS if k in books and k in values and len(values[k]) == len(books[k])]
    skipped = [k for k in KINDS if k not in ready]
    lines = [
        "//! SBR (HE-AAC v1) Huffman codebooks (ISO/IEC 14496-3 §4.6.18.3.6).",
        "//!",
        "//! Derived black box from a reference decoder's own bit accounting by",
        "//! `scripts/aac-tables/sbrtables.py`: no source was consulted. The",
        "//! decoder's \"Expected to read N SBR bytes actually read M\" complaint",
        "//! reads codeword lengths off a controlled FIL payload, and its",
        "//! `env_facs_q`/`noise_facs_q` range check reads the delta value each",
        "//! codeword carries off the same payload. Every table here closed with",
        "//! a Kraft sum of exactly 1 (asserted below).",
        "#![allow(dead_code)]",
        "",
    ]
    if skipped:
        lines.append(f"// Not emitted (values incomplete or book did not close): {skipped}")
        lines.append("")
    for kind in ready:
        codes = sorted(books[kind])
        vals = values[kind]
        lines.append(f"/// SBR Huffman book `{kind}`: (length, code, delta) by codeword.")
        lines.append(f"pub(crate) static {kind}: [(u8, u32, i32); {len(codes)}] = [")
        for length, code in codes:
            lines.append(f"    ({length}, {code}, {vals[(length, code)]}),")
        lines.append("];")
        lines.append("")
    lines.append("#[cfg(test)]")
    lines.append("mod tests {")
    lines.append("    use super::*;")
    lines.append("")
    lines.append("    fn kraft_is_one(codes: &[(u8, u32, i32)]) {")
    lines.append("        let sum: f64 = codes.iter().map(|&(l, _, _)| 2f64.powi(-(l as i32))).sum();")
    lines.append("        assert!((sum - 1.0).abs() < 1e-9, \"Kraft sum {sum}\");")
    lines.append("    }")
    lines.append("")
    lines.append("    fn is_prefix_free(codes: &[(u8, u32, i32)]) {")
    lines.append("        for (i, &(li, ci, _)) in codes.iter().enumerate() {")
    lines.append("            for &(lj, cj, _) in codes.iter().skip(i + 1) {")
    lines.append("                let (short, long) = if li <= lj { (li, lj) } else { (lj, li) };")
    lines.append("                let (sc, lc) = if li <= lj { (ci, cj) } else { (cj, ci) };")
    lines.append("                assert_ne!(sc, lc >> (long - short), \"{ci:?} prefixes {cj:?}\");")
    lines.append("            }")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    for kind in ready:
        lines.append("    #[test]")
        lines.append(f"    fn {kind.lower()}_is_a_complete_prefix_code() {{")
        lines.append(f"        kraft_is_one(&{kind});")
        lines.append(f"        is_prefix_free(&{kind});")
        lines.append("    }")
        lines.append("")
    lines.append("}")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {path}: {len(ready)} books, {len(skipped)} skipped")
    return ready, skipped


# ---------------------------------------------------------------- raw widths

WIDTH_VARS = ["E00", "E01", "E10", "E11", "N0", "N1"]


def width_cases(sf_index=7):
    """Configurations with no Huffman slot at all, and what they spend on raw
    start values -- the widths are the only thing in them this script guesses."""
    out = []
    combos = [
        # Four envelopes still carry two noise floors, which is what separates
        # an envelope's raw width from a noise floor's.  Eight is refused: the
        # tool caps a FIXFIX frame's envelope count below that.
        ("sce", 4, 0, {"E00": 4, "N0": 2}),
        ("sce", 4, 1, {"E01": 4, "N0": 2}),
        ("cpe", 4, 0, {"E00": 4, "N0": 2, "E10": 4, "N1": 2}),
        ("cpe", 4, 1, {"E01": 4, "N0": 2, "E11": 4, "N1": 2}),
        ("sce", 1, 0, {"E00": 1, "N0": 1}),
        ("sce", 2, 0, {"E00": 2, "N0": 2}),
        ("sce", 2, 1, {"E01": 2, "N0": 2}),
        ("cpe", 1, 0, {"E00": 1, "N0": 1, "E10": 1, "N1": 1}),
        ("cpe", 2, 0, {"E00": 2, "N0": 2, "E10": 2, "N1": 2}),
        ("cpe", 2, 1, {"E01": 2, "N0": 2, "E11": 2, "N1": 2}),
    ]
    for element, num_env, amp_res, coeff in combos:
        over = dict(element=element, coupling=1, num_env=num_env, amp_res=amp_res,
                    df_env=[0] * 8, df_noise=[0, 0], freq_res=0)
        found = find_config(sf_index, over, n_high=2, n_low=1, n_q=1)
        if found is None:
            continue
        cfg, tables = found
        assert not plan(cfg, tables)[0], "width probe must have no Huffman slot"
        for knob in ({}, {"add_harmonic": [1, 0]}, {"add_harmonic": [1, 1]},
                     {"extended": 1}, {"extended": 1, "add_harmonic": [1, 0]},
                     {"data_extra": 1}, {"header": 0}):
            v = dict(cfg, **knob)
            out.append((v, tables, coeff))
    return out


def stage_widths(oracle, sf_index=7):
    """Solves the raw start value widths against the byte counts."""
    global RAW_ENV, RAW_NOISE
    cases = width_cases(sf_index)
    # The widths themselves are what the probe is measuring, so they must not
    # enter the prediction: zero them while the equations are built.
    guess = dict(E00=RAW_ENV[(0, 0)], E01=RAW_ENV[(0, 1)], E10=RAW_ENV[(1, 0)],
                 E11=RAW_ENV[(1, 1)], N0=RAW_NOISE, N1=RAW_NOISE)
    got = measure(oracle, [(c, t, []) for c, t, _ in cases])
    eqs = []
    for (cfg, tables, coeff), (n, err) in zip(cases, got):
        if n is None:
            continue
        # The written payload keeps the guessed widths -- what it holds cannot
        # change what the parser reads -- so only the prediction drops them.
        base = known_bits(cfg, tables) - sum(c * guess[k] for k, c in coeff.items())
        eqs.append((coeff, 8 * n - 4 - base - 7, 8 * n - 4 - base))
    best = []
    ranges = range(3, 10)
    for e00 in ranges:
        for e01 in ranges:
            for e10 in ranges:
                for e11 in ranges:
                    for n0 in ranges:
                        for n1 in ranges:
                            v = dict(E00=e00, E01=e01, E10=e10, E11=e11, N0=n0, N1=n1)
                            if all(lo <= sum(c * v[k] for k, c in coeff.items()) <= hi
                                   for coeff, lo, hi in eqs):
                                best.append(v)
    print(f"widths: {len(eqs)} equations, {len(best)} solutions: {best[:4]}")
    if len(best) == 1:
        v = best[0]
        RAW_ENV = {(0, 0): v["E00"], (0, 1): v["E01"], (1, 0): v["E10"], (1, 1): v["E11"]}
        RAW_NOISE = v["N0"]
        if v["N1"] != v["N0"]:
            print(f"  balance noise raw width differs: {v['N1']} vs {v['N0']}")
    return best


# ---------------------------------------------------------------- stage sweep


def one_band_cfg(rate, start_freq, stop_freq, freq_scale, alter_scale, noise_bands, sf_index):
    """A configuration whose payload holds no Huffman codeword at all.

    `bs_xover_band` is chosen so `n_high` is 2, which with `bs_freq_res = 0`
    leaves a single envelope band: the raw start value and nothing else.
    """
    cfg = dict(DEFAULT)
    cfg.update(
        start_freq=start_freq, stop_freq=stop_freq, freq_scale=freq_scale,
        alter_scale=alter_scale, noise_bands=noise_bands, sf_index=sf_index,
    )
    probe = dict(cfg, xover_band=0)
    t = freq_tables(rate, probe)
    if t is None or t["n_master"] < 2:
        return None
    xover = t["n_master"] - 2
    if xover > 7:
        return None
    cfg["xover_band"] = xover
    t = freq_tables(rate, cfg)
    if t is None or t["n_high"] != 2 or t["n_low"] != 1 or t["n_q"] != 1:
        return None
    # The core is analysed with 32 QMF bands, so a crossover above band 32 is
    # refused: measured, and the reason several sweep cells used to disagree.
    if t["kx"] > 32:
        return None
    return cfg, t


def stage_sweep(oracle, verbose=True):
    """Checks the band derivation against the decoder, over many headers."""
    cases = []
    for sf_index, rate in sorted(SBR_RATE_FOR_SF.items()):
        for start_freq in range(0, 16):
            for stop_freq in (0, 2, 4, 6, 8, 10, 12, 13, 14, 15):
                for freq_scale in (0, 1, 2, 3):
                    for alter_scale in (0, 1):
                        for noise_bands in (0, 1, 2, 3):
                            made = one_band_cfg(
                                rate, start_freq, stop_freq, freq_scale,
                                alter_scale, noise_bands, sf_index,
                            )
                            if made:
                                cases.append(made)
    if verbose:
        print(f"sweep: {len(cases)} single-band configurations")
    got = measure(oracle, [(c, t, []) for c, t in cases])
    bad = 0
    for (cfg, t), (n, err) in zip(cases, got):
        want = predicted(cfg, t, [])
        if n != want:
            bad += 1
            if bad < 15 and verbose:
                print(
                    f"  MISMATCH rate_sf={cfg['sf_index']} start={cfg['start_freq']} "
                    f"stop={cfg['stop_freq']} scale={cfg['freq_scale']} alter={cfg['alter_scale']} "
                    f"nb={cfg['noise_bands']} xover={cfg['xover_band']}: want {want} got {n} "
                    f"n_master={t['n_master']} :: {err.strip()[:120]}"
                )
    print(f"sweep: {len(cases) - bad}/{len(cases)} configurations agree with the reference parser")
    return bad == 0


SBR_RATE_FOR_SF = {11: 16000, 10: 22050, 9: 24000, 8: 32000, 7: 44100, 6: 48000, 5: 64000, 4: 88200, 3: 96000}


def stop_table(rate):
    """`k2` for each `bs_stop_freq` below 14 (4.6.18.3.2.1)."""
    stop_min = start_stop_min(rate)[1]
    widths = sorted(make_bands(stop_min, 64, 13))
    out = [stop_min]
    for w in widths:
        out.append(out[-1] + w)
    return out


def stage_k0(oracle):
    """Measures `k0` for every rate and `bs_start_freq`, from the decoder.

    A header whose reconstruction span `k2 - k0` exceeds what the tool allows is
    refused with the span itself in the message, and `k2` follows from
    `bs_stop_freq` alone: one refusal per configuration therefore reads `k0`
    straight out, and the several `bs_stop_freq` values that all refuse have to
    agree on it, which is the check that the stop table is right too.
    """
    span = re.compile(r"too many QMF subbands: (\d+)")
    cases = []
    for sf_index, rate in sorted(SBR_RATE_FOR_SF.items()):
        for start_freq in range(16):
            for stop_freq in (10, 11, 12, 13):
                cfg = dict(DEFAULT)
                cfg.update(sf_index=sf_index, start_freq=start_freq, stop_freq=stop_freq)
                cases.append((cfg, dict(n_q=1, n_high=2, n_low=1), []))
    got = measure(oracle, cases)
    found = {}
    for (cfg, _t, _e), (_n_bytes, err) in zip(cases, got):
        m = span.search(err)
        if not m:
            continue
        rate = SBR_RATE_FOR_SF[cfg["sf_index"]]
        k2 = stop_table(rate)[cfg["stop_freq"]]
        k0 = k2 - int(m.group(1))
        found.setdefault((rate, cfg["start_freq"]), set()).add(k0)
    print("measured k0 (SBR rate, bs_start_freq):")
    table = {}
    for rate in sorted(set(SBR_RATE_FOR_SF.values())):
        row = []
        for start_freq in range(16):
            v = found.get((rate, start_freq))
            row.append(sorted(v) if v else None)
        table[rate] = row
        flat = [(r[0] if r and len(r) == 1 else r) for r in row]
        temp = 3000 if rate < 32000 else (4000 if rate < 64000 else 5000)
        start_min = ((temp << 7) + (rate >> 1)) // rate
        offs = [(v - start_min) if isinstance(v, int) else v for v in flat]
        print(f"  {rate:6d} start_min={start_min:3d} k0={flat}")
        print(f"         offsets={offs}")
    return table


def stage_diag(oracle):
    """Measures the decoder's own `n_master` and compares it with ours.

    `bs_xover_band > n_master` is refused with a message of its own, so the
    largest accepted crossover *is* `n_master` whenever it fits in the field.
    """
    grid = []
    for sf_index, rate in ((7, 44100), (6, 48000), (8, 32000)):
        for start_freq in range(0, 14):
            for stop_freq in range(0, 16):
                for freq_scale in (0, 1, 2, 3):
                    for alter_scale in (0, 1):
                        cfg = dict(DEFAULT)
                        cfg.update(
                            sf_index=sf_index, start_freq=start_freq, stop_freq=stop_freq,
                            freq_scale=freq_scale, alter_scale=alter_scale,
                        )
                        grid.append((rate, cfg))
    cases = []
    for rate, cfg in grid:
        for xover in range(8):
            c = dict(cfg, xover_band=xover)
            t = freq_tables(rate, dict(c, xover_band=0)) or dict(n_q=1, n_high=2, n_low=1)
            cases.append((c, t, []))
    got = measure(oracle, cases)
    rows = {}
    for (cfg, _t, _e), (n, err) in zip(cases, got):
        key = (cfg["sf_index"], cfg["start_freq"], cfg["stop_freq"], cfg["freq_scale"], cfg["alter_scale"])
        ok = "crossover band index" not in err
        rows.setdefault(key, {})[cfg["xover_band"]] = (ok, err)
    disagree = 0
    total = 0
    shown = 0
    for (sf_index, start_freq, stop_freq, freq_scale, alter_scale), by_x in sorted(rows.items()):
        rate = {7: 44100, 6: 48000, 8: 32000}[sf_index]
        cfg = dict(DEFAULT)
        cfg.update(sf_index=sf_index, start_freq=start_freq, stop_freq=stop_freq,
                   freq_scale=freq_scale, alter_scale=alter_scale, xover_band=0)
        mine = freq_tables(rate, cfg)
        accepted = [x for x in range(8) if by_x[x][0]]
        theirs = max(accepted) if accepted else -1
        # An out-of-range header is refused for every crossover, ours says None.
        if mine is None:
            valid_here = theirs < 0 or all("Invalid" in by_x[x][1] or "invalid" in by_x[x][1] for x in range(8))
            total += 1
            if not valid_here:
                disagree += 1
                if shown < 20:
                    shown += 1
                    print(f"  we refuse, they take: rate={rate} start={start_freq} stop={stop_freq} "
                          f"scale={freq_scale} alter={alter_scale} maxx={theirs}")
            continue
        total += 1
        if theirs == 7 and mine["n_master"] >= 7:
            continue  # the field caps at 7: no information above it
        if theirs != mine["n_master"]:
            disagree += 1
            if shown < 20:
                shown += 1
                print(f"  n_master rate={rate} start={start_freq} stop={stop_freq} scale={freq_scale} "
                      f"alter={alter_scale}: ours {mine['n_master']} theirs {theirs}")
    print(f"diag: {total - disagree}/{total} headers agree on n_master")


def run_books_to_fixpoint(oracle, sf_index=7):
    """Repeats `stage_books` until no further kind closes, honoring the
    dependency order `stage_books` itself discovers via its `missing` check."""
    books = {}
    remaining = set(KINDS)
    while remaining:
        made = stage_books(oracle, sf_index, only=remaining, books=books)
        if not made:
            break
        remaining -= set(made)
    if remaining:
        print(f"books: did not close: {sorted(remaining)}")
    return books


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--stage", default="sweep")
    ap.add_argument("--workers", type=int, default=12)
    args = ap.parse_args()
    oracle = Oracle(args.workers)
    try:
        if args.stage in ("sweep", "all"):
            stage_sweep(oracle)
        if args.stage == "diag":
            stage_diag(oracle)
        if args.stage == "k0":
            stage_k0(oracle)
        if args.stage in ("widths", "all"):
            stage_widths(oracle)
        books = None
        if args.stage in ("books", "values", "all"):
            books = run_books_to_fixpoint(oracle)
        if args.stage in ("values", "all"):
            values = stage_values(oracle, books)
            write_rust(books, values)
    finally:
        oracle.save()
        print(f"oracle: {oracle.runs} decoder runs, {oracle.hits} cache hits")
