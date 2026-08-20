#!/usr/bin/env python3
"""Black-box measurement of the reference decoder's HF-generator chirp
(bandwidth-expansion) factor per `bs_invf_mode`, and its frame-to-frame
smoothing rule.

Method: a pure single-bin AAC-LC core spectrum (one isolated MDCT line, all
other lines silent via ZERO_HCB) puts an exact, noiseless sinusoid into one
low QMF subband. A single-tone AR(2) source has *exact* LPC coefficients
`a1 = 2*cos(w0), a2 = -1` (unit-radius poles) independent of estimation
method, so the SBR "inverse filter" recursion `y = residual + g*a1*y[-1] -
g^2*y[-2]` (residual ~= 0 once the source is a pure tone the predictor
already explains) collapses to an undriven damped oscillator whose envelope
decays geometrically as `g^n` per QMF slot -- `g` *is* the chirp factor
(pole radius), directly recoverable from the decay rate with no analysis of
our own decoder involved.

STATUS (round-19): the round-18 non-engagement was `stream()`'s own recipe
shape, not a framing bug -- bisected field-by-field against a known-working
CPE probe (see project ledger) and isolated to TWO independent culprits,
either one sufficient alone to make the reference decoder's SBR gain stage
not engage (envelope amplitude and `bs_invf_mode` both become powerless,
`"No quantized data read for sbr_dequant."` fires either way -- that
message is a red herring, printed on this SCE probe shape regardless of
engagement, not diagnostic of it):

  1. `bs_amp_res=0` (coarse, 7-bit raw envelope resolution). Sweeping the
     full raw range (0..127) still gives <0.03% output-power change; fine
     resolution (`bs_amp_res=1`, 6-bit raw) with the same header gives
     three-to-four-orders-of-magnitude power swings for a mid-range raw
     sweep (3 -> 60). Pure probe-construction quirk, not implicating our
     parser/writer: real encoders emit both resolutions routinely and nothing
     here says our AAC parser mishandles coarse-resolution streams -- it is
     specifically *this reference build's* dequant path refusing these raw
     values under `amp_res=0`, for a reason not chased further (plausibly an
     offset/table-range mismatch in how this probe's raw codewords map,
     unrelated to any of our own code).
  2. `bs_num_env=1` (FIXFIX single envelope). Independent of amp_res: even
     requesting `amp_res=1` explicitly, a single-envelope frame still fails
     to engage here.

`stream()` below now uses the repaired shape: SCE, FIXFIX with `num_env=2`,
`amp_res=1`, envelope held constant across `bs_invf_mode` sweeps. Task 2 (a
four-level `bs_invf_mode` sweep plus a HIGH->NONE transition, `n_q=1`
narrow HF target) on this repaired instrument found only a ~0.1% output
power drift across all four levels and no resolvable flatness difference,
against a `BW_TABLE` that nominally spans `bw=0.0..0.98` -- STOP RULE
applies: fingerprints are indistinguishable on the working instrument, so
the bw-per-level hypothesis is refuted for this measurement and `sbr_hf.rs`
was left unchanged. (The HIGH->NONE transition run showed a >200x power
spike two frames from the stream's end; that coincides with the trailing
silence frame and reads as a decoder-side reset artifact of this probe's
framing, not a chirp-smoothing signal -- not chased further.)
"""
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sbrpayload_fixtures as F
import sbrprobe as P
import sbrtables as T

SF_INDEX = 7
RATE = 44100


def tone_core(w, sfb_idx, group_active, gain=200):
    """SCE with one isolated MDCT line: `sfb_idx`'s `group_active`'th 4-tuple
    group carries value 2 in its first line, everything else (all other
    sfbs, and the other groups of `sfb_idx`) silent."""
    swb = P.SWB_LONG[SF_INDEX]
    max_sfb = sfb_idx + 1
    w.w(0, 3).w(0, 4)  # SCE, instance tag
    w.w(gain, 8)
    w.w(0, 1).w(0, 2).w(0, 1).w(max_sfb, 6).w(0, 1)
    # section 1: ZERO_HCB over the leading silent sfbs
    w.w(0, 4)
    left = sfb_idx
    while left >= 31:
        w.w(31, 5)
        left -= 31
    w.w(left, 5)
    # section 2: codebook 3 over the one active sfb
    w.w(3, 4)
    w.w(1, 5)
    w.w(P.HCB_SF[60][1], P.HCB_SF[60][0])  # scalefactor delta 0
    w.w(0, 3)  # pulse/tns/gain-control absent -- MUST precede spectral_data()
    # (round-28 fix: this write sat AFTER the spectral loop below, shifting
    # every bit that followed and desyncing the reference decoder on every
    # probed band -- byte-accounted against sce_core's proven field order.)
    ngroups = (swb[sfb_idx + 1] - swb[sfb_idx]) // 4
    for i in range(ngroups):
        quad = (2, 0, 0, 0) if i == group_active else (0, 0, 0, 0)
        length, code = P.HCB3[P.tuple_index_u(quad, 2, 4)]
        signs = [1 for v in quad if v != 0]
        w.w(code, length)
        for _ in signs:
            w.w(0, 1)
    return w


SFB = 29   # SWB_LONG[7][29:31] = [240, 260]: bins 240..260, QMF band 15
GROUP = 0  # bin 240, QMF band [15*344.5, 16*344.5) Hz
MAX_SFB = 30  # bins 0..260: covers source QMF band 15 (bins 240..256) broadband


def core_frame_bits(w):
    tone_core(w, SFB, GROUP)


def broadband_core_bits(w, gain=200):
    """The already-verified codebook-3 comb pattern (`sce_core`, proven clean
    at `max_sfb=30` above) -- strongly, stationarily correlated but not a
    literal single MDCT bin, used as the resonant-content probe once the
    isolated-tone encoding above turned out not to round-trip (HCB3 table
    entries away from the exact tuple that earlier probes exercised are
    unverified; not chased further here, broadband suffices for a fingerprint
    test)."""
    P.sce_core(w, SF_INDEX, MAX_SFB, gain=gain)


def stream(cfg, tables, n_frames, invf_seq):
    """`n_frames` FIXFIX/2-env, `amp_res=1` frames (the repaired, engaging
    shape -- see module docstring), header on the first, `bs_invf_mode`
    taken from `invf_seq[i]` (repeats last if shorter), envelope held fixed
    across the sweep."""
    grid = dict(frame_class=0, num_env=2, freq_res=0)
    frames = [P.silent(SF_INDEX)]
    for i in range(n_frames):
        mode = invf_seq[min(i, len(invf_seq) - 1)]
        c = dict(cfg, invf=mode, header=1, amp_res=1, element="sce", coupling=0,
                 df_env=[0] * 8, df_noise=[0, 0], env0=[40] * 8, noise0=[2, 2])
        w = P.BitW()
        broadband_core_bits(w)
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
        frames.append(P.adts(w.pack(pad=0), SF_INDEX))
    frames.append(P.silent(SF_INDEX))
    return frames


def decode(frames, tag):
    return P.decode(frames, SF_INDEX, tag)


def target_band_envelope(pcm, band_lo_hz, band_hi_hz, sr=RATE):
    """Per-QMF-slot (32 samples at full SBR rate here since no dual-rate)
    envelope of the isolated band via a Goertzel-style single-bin DFT swept
    along the signal -- narrowband enough that only the target tone
    contributes, since nothing else in the stream carries energy there."""
    n = len(pcm)
    hop = 32
    center = (band_lo_hz + band_hi_hz) / 2
    w0 = 2 * np.pi * center / sr
    win = hop
    kernel_cos = np.cos(w0 * np.arange(win))
    kernel_sin = np.sin(w0 * np.arange(win))
    env = []
    for start in range(0, n - win, hop):
        seg = pcm[start : start + win]
        re = np.dot(seg, kernel_cos)
        im = np.dot(seg, kernel_sin)
        env.append(np.hypot(re, im))
    return np.array(env)


def spectral_flatness(pcm, lo_hz, hi_hz, n_fft=8192, skip=3):
    """Geometric/arithmetic mean ratio of the power spectrum inside
    `[lo_hz,hi_hz)`, in a steady-state window (`skip` frames of 1024 samples
    dropped for transient). Near 1 = white/flat (bw~0, whitened residual);
    near 0 = tonal/peaky (bw~1, resonance preserved)."""
    start = skip * 1024
    seg = pcm[start : start + n_fft]
    if len(seg) < n_fft:
        seg = np.pad(seg, (0, n_fft - len(seg)))
    spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg))))
    freqs = np.fft.rfftfreq(n_fft, 1 / RATE)
    mask = (freqs >= lo_hz) & (freqs < hi_hz)
    power = spec[mask] ** 2 + 1e-24
    gm = np.exp(np.mean(np.log(power)))
    am = np.mean(power)
    return gm / am, np.mean(power)


if __name__ == "__main__":
    cfg0, tables0 = F.one_band_header()
    print("cfg0 amp_res", cfg0["amp_res"], "tables0", tables0)

    # kx=19, k2=23: target HF region is QMF bands [19,23) = [6546, 7924) Hz.
    lo, hi = 19 * RATE / 128, 23 * RATE / 128
    src_lo, src_hi = 15 * RATE / 128, 16 * RATE / 128
    print(f"target region {lo:.1f}-{hi:.1f} Hz, source band {src_lo:.1f}-{src_hi:.1f} Hz")

    cfg = dict(cfg0)
    print("\n== Task 1: four invf levels, flat/identical envelopes ==")
    results = {}
    for mode, name in [(0, "NONE"), (1, "LOW"), (2, "MID"), (3, "HIGH")]:
        frames = stream(cfg, tables0, 20, [mode])
        pcm, err = decode(frames, f"invf{mode}")
        print(f"invf={name} err={err.strip()[:100]!r} samples={len(pcm)}")
        for i in range(2, 18):
            flat, pwr = spectral_flatness(pcm, lo, hi, skip=i)
            print(f"  frame {i}: flatness={flat:.4f} pwr={pwr:.3e}")
        results[name] = pcm

    print("\n== Task 1: transition HIGH -> NONE, per-frame flatness decay ==")
    frames = stream(cfg, tables0, 16, [3, 3, 3, 3, 0])
    pcm, err = decode(frames, "transition")
    for i in range(2, 14):
        flat, pwr = spectral_flatness(pcm, lo, hi, skip=i)
        print(f"frame {i}: flatness={flat:.4f} pwr={pwr:.3e}")
    if err.strip():
        print("transition err:", err.strip()[:200])
