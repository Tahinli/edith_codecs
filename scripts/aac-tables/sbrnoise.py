#!/usr/bin/env python3
"""Black-box measurement of the reference decoder's SBR noise-floor
dequantization law (round-21 Task 1).

Method: reuse `sbrchirp.py`'s repaired probe shape (SCE, FIXFIX num_env=2,
amp_res=1, broadband comb core so the HF-signal envelope target is fixed
and non-zero), fix the envelope raw value, and sweep only the noise-floor
raw value `q_q` (`noise0`) across its 5-bit range. Noise is spec-additive
on top of the gain-adjusted HF signal (module doc in sbr_env.rs), so the
total power measured in the target band at each `q_q` is
`signal_energy(fixed) + noise_energy(q_q)`; subtracting the high-`q_q`
(minimal-noise) baseline isolates `noise_energy(q_q)` and its shape can be
fit against `q_q` directly, independent of our own decoder.
"""
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sbrchirp as C
import sbrpayload_fixtures as F

RATE = C.RATE


def noise_stream(cfg, tables, n_frames, q, env=40):
    """Like `sbrchirp.stream()` but `bs_invf_mode` held at NONE (0, so the
    injected noise is not reshaped by the inverse filter) and `noise0`
    swept to `[q, q]` instead of `bs_invf_mode`; `env` is the fixed
    envelope raw value."""
    grid = dict(frame_class=0, num_env=2, freq_res=0)
    frames = [C.P.silent(C.SF_INDEX)]
    for _ in range(n_frames):
        c = dict(cfg, invf=0, header=1, amp_res=1, element="sce", coupling=0,
                  df_env=[0] * 8, df_noise=[0, 0], env0=[env] * 8, noise0=[q, q])
        w = C.P.BitW()
        C.broadband_core_bits(w)
        body = F.sbr_bits_full(c, tables, grid, [])
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
        frames.append(C.P.adts(w.pack(pad=0), C.SF_INDEX))
    frames.append(C.P.silent(C.SF_INDEX))
    return frames


def band_power(pcm, lo_hz, hi_hz, skip=6, n_fft=8192):
    start = skip * 1024
    seg = pcm[start : start + n_fft]
    if len(seg) < n_fft:
        seg = np.pad(seg, (0, n_fft - len(seg)))
    spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg)))) ** 2
    freqs = np.fft.rfftfreq(n_fft, 1 / RATE)
    mask = (freqs >= lo_hz) & (freqs < hi_hz)
    return float(np.sum(spec[mask]))


if __name__ == "__main__":
    cfg0, tables0 = F.one_band_header()
    lo, hi = 19 * RATE / 128, 23 * RATE / 128
    print(f"target region {lo:.1f}-{hi:.1f} Hz, RAW_NOISE bits={C.T.RAW_NOISE} (q range 0..{2**C.T.RAW_NOISE - 1})")

    print("\n== value-engagement check: envelope raw sweep (fixed q_q) must move power by orders ==")
    for e in (10, 40, 60):
        cfg = dict(cfg0)
        frames = noise_stream(cfg, tables0, 20, 30, env=e)
        pcm, err = C.decode(frames, f"env_check{e}")
        pwr = band_power(pcm, lo, hi)
        print(f"  env0={e} (q_q=30, minimal noise) pwr={pwr:.4e} err={err.strip()[:60]!r}")

    print("\n== Task 1: noise-floor raw (q_q) sweep, fixed envelope ==")
    qs = list(range(0, 31, 3))
    pwrs = {}
    for q in qs:
        cfg = dict(cfg0)
        frames = noise_stream(cfg, tables0, 20, q)
        pcm, err = C.decode(frames, f"q{q}")
        pwr = band_power(pcm, lo, hi)
        pwrs[q] = pwr
        print(f"  q_q={q:2d} pwr={pwr:.6e} err={err.strip()[:80]!r}")

    baseline = pwrs[qs[-1]]
    print(f"\nbaseline (q_q={qs[-1]}, minimal noise) pwr={baseline:.6e}")
    print("q_q, noise_energy=pwr-baseline, log2(noise_energy)")
    fit_x, fit_y = [], []
    for q in qs[:-1]:
        n = pwrs[q] - baseline
        if n > 0:
            l2 = np.log2(n)
            print(f"  {q:2d}  {n:.6e}  {l2:.4f}")
            fit_x.append(q)
            fit_y.append(l2)
        else:
            print(f"  {q:2d}  {n:.6e}  (non-positive, skipped)")
    if len(fit_x) >= 2:
        slope, intercept = np.polyfit(fit_x, fit_y, 1)
        print(f"\nfit: log2(noise_energy) = {slope:.4f} * q_q + {intercept:.4f}")
        print(f"(spec/ours form: log2(noise_energy) = -1*q_q + 6  =>  slope~-1, intercept~6 expected if law matches)")
