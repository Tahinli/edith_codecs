#!/usr/bin/env python3
"""Round-29: measures the reference decoder's ACTUAL SBR HF patch map (which
low QMF source band feeds which high target band), by isolating one source
band at a time and reading off the FRACTIONAL FREQUENCY OFFSET of the
resulting HF line(s), not their power.

Round-28 found that a power-bucket readout cannot work: the envelope adjuster
renormalizes every target band's POWER to the transmitted envelope regardless
of patch content, so every source band lit the SAME target range at similar
power (expected physics, not a bug). Gains rescale amplitude, never a line's
frequency position within its band, so the fix is to read frequency instead.

Method: the core spectrum for a probe frame is a single isolated MDCT bin --
`tone_core()` in `sbrchirp.py` -- placed not at low QMF band `p`'s first bin
(a group/band boundary, ambiguous under FFT leakage and coincident with the
target band edge too) but at its SECOND 4-bin group (`k = p*16 + 4`), i.e. a
known, non-zero fractional offset `D = 0.25` of a QMF band's width inside
band `p` (0.25 is the finest offset `tone_core`'s group granularity allows;
any of 0.25/0.5/0.75 is equally "distinctive", 0.25 chosen arbitrarily).
Patching copies that source band's complex QMF series verbatim into every
target band any patch maps it to, translating the tone in frequency by an
integer number of band widths -- so the offset is preserved (or, per the
charter's note on filterbank parity, possibly mirrored to `1 - D`) in
whichever target band(s) the patch actually lit up. An averaged FFT of the
decoded output finds every HF line clearly above the local noise floor
(power only GATES detection); for each line, `target band q = floor(f/bw)`
and `frac = f/bw - q` are computed, and a line is read as "source `p` ->
target `q`" iff `frac` is within a few percent of `D` (direct) or `1 - D`
(mirrored, logged separately as data, not discarded).

Runs the same probes through our own decoder via the `adts_to_pcm` example
(`cargo build -p ec-aac --example adts_to_pcm`) for the sanity cross-check the
charter requires: our measured map must equal `build_patches`' prediction, or
the fingerprint method itself is the thing that's broken. Also checks the
reference's stderr for "No quantized data read for sbr_dequant." -- if
present the run used a shape that leaves the SBR gain stage unengaged and the
decode is not trustworthy.

STATUS (round-29): the sanity gate still FAILS after two real fixes landed
this round (see the ledger's round-29 facts): (1) `sbr_bits_full`'s `slot()`
default (`T.FILL`, 24 raw zero bits) desyncs the bitstream whenever a
multi-band delta run is used -- ENV30_F/NOISE_F's shortest codeword (the one
"0" bits actually decode as) is only 1 bit, so 23 excess zero bits per slot
leak into every field transmitted after it; `stream_one_band` now supplies
exactly enough real 1-bit zero-delta codewords instead (fixed, confirmed via
`EC_AAC_SBR_DEBUG`: `e_q[1]`/`q_q[1]` now read back the intended raw values,
were silently zeroed before). (2) The tone's fractional-offset placement
(`GROUP_OFFSET`) is unaffected by that fix. What remains OPEN: `k = active_p
* 16` (the MDCT-bin-per-QMF-band assumption every prior round's readout used,
inherited unchanged here) does not track the true source-band placement --
calibration by sweeping raw bin `k` and reading which target band lights up
shows a clean *32*-bin-per-band slope (not 16) locally, but with an offset
that is NOT a fixed constant across headers (k=64 lands on `target == kx`
for headers A and C, whose `patches[0]` happens to have `source_start == 0`,
but for header B -- `xover_band=1`, whose `patches[0]` has `source_start ==
2` -- k=64 *also* lands on `target == kx`, meaning it tracked `source_start`
itself, not a fixed `p`). The low/source band index space is not a simple
`p*N + offset` function of MDCT bin position; it needs deriving from the
tables' own low-band structure rather than assumed, which this round did not
reach. Sanity gate: DO NOT trust `measured(ref)` output below until this is
resolved -- Task 2/3 (reading and acting on the reference's map) are blocked
on it.
"""
import os
import subprocess
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sbrchirp as C  # noqa: E402
import sbrpayload_fixtures as F  # noqa: E402
import sbrprobe as P  # noqa: E402
import sbrtables as T  # noqa: E402

SF_INDEX = 7
RATE = 44100
OUR_DECODER = os.path.expanduser("~/.cache/cargo-target-sbr/debug/examples/adts_to_pcm")
BAND_HZ = RATE / 128  # QMF band width, both source and target index units
D = 0.25  # fractional offset (of one band width) the probe tone is placed at
GROUP_OFFSET = 4  # bins: 4/16 == D, the finest offset tone_core's grouping allows
NO_QUANT_WARNING = "No quantized data read for sbr_dequant."


def build_patches_predicted(kx, k2, f_high):
    """Python port of `build_patches` (crates/ec-aac/src/sbr_hf.rs), for
    comparing the reference's measured map against our own prediction."""
    patches = []
    if kx == 0 or k2 <= kx:
        return patches
    msb, sb = kx, kx
    while sb < k2:
        width, probe = 0, sb
        while True:
            border = next((b for b in f_high if b > probe), None)
            if border is None:
                break
            iv = border - probe
            if width + iv > msb or border > k2:
                break
            width += iv
            probe = border
        if width == 0:
            width = min(k2 - sb, max(msb, 1))
        source_start = msb - min(width, msb)
        patches.append((source_start, sb, width))
        sb += width
        msb = kx if source_start == 0 else source_start
    return patches


def predicted_targets(patches, p):
    out = []
    for source_start, target_start, width in patches:
        if source_start <= p < source_start + width:
            out.append(target_start + (p - source_start))
    return out


def bin_to_sfb_group(k, swb):
    for i in range(len(swb) - 1):
        if swb[i] <= k < swb[i + 1]:
            return i, (k - swb[i]) // 4
    return None


def make_tables(start_freq, stop_freq, freq_scale, alter_scale, noise_bands, xover_band):
    cfg = dict(T.DEFAULT, sf_index=SF_INDEX, start_freq=start_freq, stop_freq=stop_freq,
               freq_scale=freq_scale, alter_scale=alter_scale, noise_bands=noise_bands,
               xover_band=xover_band)
    t = T.freq_tables(RATE, cfg)
    return cfg, t


def stream_one_band(cfg, tables, active_p, n_frames=24, invf_mode=0):
    """`n_frames` FIXFIX/2-env amp_res=1 frames (sbrchirp.py's `stream()`
    engaging shape, byte-for-byte -- FIXFIX, num_env=2, amp_res=1, env0 at a
    healthy mid value, noise0 at the quiet end so noise floor cannot bury the
    line), core = one isolated tone at low QMF band `active_p`'s SECOND 4-bin
    group (offset `D` of a band width into the band, not the ambiguous
    boundary bin), held fixed; invf NONE, no harmonics."""
    swb = P.SWB_LONG[SF_INDEX]
    k = active_p * 16 + GROUP_OFFSET
    found = bin_to_sfb_group(k, swb)
    assert found is not None, f"band {active_p} bin {k} not in SWB_LONG[{SF_INDEX}]"
    sfb_idx, group_active = found
    grid = dict(frame_class=0, num_env=2, freq_res=0)
    frames = [P.silent(SF_INDEX)]
    # sbrtables.py's `slot()` falls back to `T.FILL` (24 raw zero bits) for
    # any delta slot the caller doesn't supply explicit bits for -- fine for
    # the bootstrap probes that don't yet know a codebook's length, but for
    # amp_res=1/coupling=0/df_env=df_noise=0 (this probe's exact config) the
    # book is known: ENV30_F and NOISE_F (crates/ec-aac/src/sbr_tables.rs)
    # both give delta=0 its SHORTEST codeword, (len=1, code=0). 24 zero bits
    # where only 1 is consumed leaves 23 unconsumed zero bits per slot that
    # bleed into every field transmitted after it (env1's raw start value,
    # noise0/1, ...), silently corrupting it to whatever a run of stray zero
    # bits happens to decode as -- this is round-29's "readout is wrong"
    # candidate B made concrete, and the fix.
    zero_delta = [0]
    n_slots = 2 * (tables["n_low"] - 1) + 2 * (tables["n_q"] - 1)
    for _ in range(n_frames):
        c = dict(cfg, invf=invf_mode, header=1, amp_res=1, element="sce", coupling=0,
                 df_env=[0] * 8, df_noise=[0, 0], env0=[40] * 8, noise0=[2, 2])
        w = P.BitW()
        C.tone_core(w, sfb_idx, group_active)
        body = F.sbr_bits_full(c, tables, grid, [zero_delta] * n_slots)
        w.w(6, 3)
        cnt = (len(body.bits) + 4 + 7) // 8
        if cnt >= 15:
            w.w(15, 4)
            w.w(cnt - 15 + 1, 8)
        else:
            w.w(cnt, 4)
        start = len(w)
        w.w(13, 4)
        w.wbits(body.bits)
        while len(w) - start < cnt * 8:
            w.w(0, 1)
        w.w(7, 3)
        frames.append(P.adts(w.pack(pad=0), SF_INDEX))
    frames.append(P.silent(SF_INDEX))
    return frames


def our_decode(frames, tag):
    src = f"/tmp/sbrpatchmap-our-{tag}.aac"
    with open(src, "wb") as f:
        f.write(b"".join(frames))
    env = dict(os.environ, EC_AAC_SBR_SAME_RATE="1", EC_AAC_SBR_CHANNELS="1")
    r = subprocess.run([OUR_DECODER, src], capture_output=True, env=env)
    os.remove(src)
    return np.frombuffer(r.stdout, dtype="<f4").astype(np.float64), r.stderr.decode(errors="replace")


def averaged_spectrum(pcm, n_fft=4096, hop=2048, skip_samples=5120):
    """Power spectrum averaged over several overlapping steady-state windows
    (Welch-style) for a cleaner noise floor than a single FFT window."""
    win = np.hanning(n_fft)
    specs = []
    start = skip_samples
    while start + n_fft <= len(pcm):
        seg = pcm[start : start + n_fft] * win
        specs.append(np.abs(np.fft.rfft(seg)) ** 2)
        start += hop
    assert specs, "not enough samples past skip_samples for even one FFT window"
    spec = np.mean(specs, axis=0)
    freqs = np.fft.rfftfreq(n_fft, 1 / RATE)
    return freqs, spec


def find_lines(freqs, spec, lo_hz, hi_hz, floor_mult=8.0):
    """Local-maximum bins within [lo_hz, hi_hz) whose power clearly exceeds
    the local noise floor (median of the in-range spectrum) -- power GATES
    detection only, never decides the mapping. Sub-bin frequency via
    parabolic interpolation around each peak."""
    idx = np.where((freqs >= lo_hz) & (freqs < hi_hz))[0]
    if len(idx) < 3:
        return []
    floor = np.median(spec[idx])
    if floor <= 0:
        floor = np.finfo(float).tiny
    df = freqs[1] - freqs[0]
    lines = []
    for j in range(1, len(idx) - 1):
        i = idx[j]
        if spec[i] > spec[i - 1] and spec[i] >= spec[i + 1] and spec[i] > floor_mult * floor:
            a, b, c = spec[i - 1], spec[i], spec[i + 1]
            denom = a - 2 * b + c
            delta = 0.5 * (a - c) / denom if denom != 0 else 0.0
            delta = max(-0.5, min(0.5, delta))
            lines.append((freqs[i] + delta * df, float(b)))
    return lines


def measured_targets(freqs, spec, kx, k2, d=D, tol=0.06):
    """Patch map read directly from fractional frequency offset: a detected
    HF line in target band `q` counts as "source -> q" iff its offset within
    the band matches `d` (direct) or `1 - d` (mirrored, per the charter's
    filterbank-parity note -- logged, not discarded). Returns
    (direct_targets, mirrored_targets, all_lines_for_debug)."""
    lines = find_lines(freqs, spec, kx * BAND_HZ, k2 * BAND_HZ)
    direct, mirrored, debug = [], [], []
    for f, power in lines:
        q = int(f // BAND_HZ)
        frac = f / BAND_HZ - q
        debug.append((q, round(frac, 3), power))
        if abs(frac - d) <= tol:
            direct.append(q)
        elif abs(frac - (1 - d)) <= tol:
            mirrored.append(q)
    return sorted(set(direct)), sorted(set(mirrored)), debug


def run_header(label, start_freq, stop_freq, freq_scale, alter_scale, noise_bands, xover_band):
    cfg, tables = make_tables(start_freq, stop_freq, freq_scale, alter_scale, noise_bands, xover_band)
    assert tables is not None, f"{label}: header does not produce valid tables"
    kx, k2 = tables["kx"], tables["k2"]
    print(f"\n== {label}: start={start_freq} stop={stop_freq} scale={freq_scale} alter={alter_scale}"
          f" noise={noise_bands} xover={xover_band} -> kx={kx} k2={k2} n_q={tables['n_q']} ==")
    patches = build_patches_predicted(kx, k2, tables["f_high"])
    print(f"  predicted patches (src_start, tgt_start, width): {patches}")

    ref_map, our_map, warnings_seen = {}, {}, []
    our_sane = True
    for p in range(kx):
        frames = stream_one_band(cfg, tables, p)
        pcm_ref, err_ref = P.decode(frames, SF_INDEX, f"{label}-p{p}")
        pcm_our, err_our = our_decode(frames, f"{label}-p{p}")
        if NO_QUANT_WARNING in err_ref:
            warnings_seen.append(p)
        freqs_ref, spec_ref = averaged_spectrum(pcm_ref)
        freqs_our, spec_our = averaged_spectrum(pcm_our)
        d_ref, m_ref, dbg_ref = measured_targets(freqs_ref, spec_ref, kx, k2)
        d_our, m_our, dbg_our = measured_targets(freqs_our, spec_our, kx, k2)
        pred = predicted_targets(patches, p)
        ref_map[p] = d_ref
        our_map[p] = d_our
        if d_our != pred:
            our_sane = False
        flag = "" if d_ref == pred else "  <<< DIFFERS FROM PREDICTION"
        our_flag = "" if d_our == pred else "  <<< OUR DECODER DIFFERS FROM build_patches"
        mir = f" mirrored(ref)={m_ref}" if m_ref else ""
        print(f"  p={p:2d} predicted={pred} measured(ref)={d_ref}{flag}{mir} measured(ours)={d_our}{our_flag}"
              f"  [ref_err={err_ref.strip()[:60]!r} our_err={err_our.strip()[:40]!r}]")

    if warnings_seen:
        print(f"  !! {NO_QUANT_WARNING!r} present on p={warnings_seen} -- run NOT trustworthy, fix shape first")
    else:
        print(f"  {NO_QUANT_WARNING!r} absent on every probe -- reference decode confirmed engaged")

    if not our_sane:
        print("  !! SANITY GATE FAILED: our decoder's measured map != build_patches prediction -- "
              "do not trust the reference read below until this is resolved")
    else:
        print("  sanity gate passed: our decoder's measured map == build_patches prediction on every band")

    return kx, k2, patches, ref_map, our_map, our_sane, warnings_seen


if __name__ == "__main__":
    assert os.path.exists(OUR_DECODER), f"build first: cargo build -p ec-aac --example adts_to_pcm ({OUR_DECODER})"
    # Header A: the real file's ACTUAL SBR header, read straight off
    # Nikbinler's own bitstream via `EC_AAC_SBR_DEBUG=1` on our own decoder
    # (start_freq=5 stop_freq=8 xover_band=0 freq_scale=2 alter_scale=1
    # noise_bands=2 -> kx=14 k2=43 n_q=3) -- NOT the `sbr_hf.rs` unit test's
    # `real_file_tables()`, whose stop_freq/xover_band guess (3/2) was wrong
    # and gives a different, smaller kx=16 k2=29 header.
    run_header("A-nikbinler", start_freq=5, stop_freq=8, freq_scale=2, alter_scale=1,
               noise_bands=2, xover_band=0)
    # Headers B and C: different start/stop/xover -> differently-shaped
    # kx/k2, to pin the algorithm's shape rather than one lucky header.
    run_header("B-alt", start_freq=8, stop_freq=10, freq_scale=2, alter_scale=1,
               noise_bands=2, xover_band=1)
    run_header("C-alt", start_freq=3, stop_freq=5, freq_scale=2, alter_scale=1,
               noise_bands=2, xover_band=0)
