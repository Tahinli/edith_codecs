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

STATUS (dead end, not chased further): the isolated-single-MDCT-line core
(`tone_core`) does not round-trip -- most HCB3 codewords beyond the one
tuple earlier probes happened to exercise are unverified and desync the
bitstream ("Input buffer exhausted before END element found"); see the
`(2,0,0,0)`/`(2,0,2,2)`/`(0,0,0,0)` isolation test in the project ledger.
Falling back to the already-proven-clean broadband `sce_core` comb content
(`broadband_core_bits`) decodes without framing errors, but every real SBR
sweep built on top of it (`stream()`, any `n_low`/`n_high`/`n_q` combination
tried, any `env0`/`noise0` magnitude) makes the reference decoder print
"No quantized data read for sbr_dequant." and then produce PCM that is
*value-independent* of both `bs_invf_mode` (all four levels byte-identical)
and the envelope amplitude itself (`env0=0` vs `env0=100`: power differs by
<0.02%, when a real amp_res=0 dequant law would differ by orders of
magnitude) -- i.e. the reference's SBR envelope/gain stage is not engaging
with this harness's `sbr_data` payload at all, for a reason not yet
isolated (framing/byte-count is provably correct per `oracle_check`, so the
fault is in a field *value* or ordering `oracle_check`'s byte-count-only
check cannot see). No bw fingerprint could be extracted; Task 1 could not be
completed this round. Do not reuse `stream()`'s SCE/FIXFIX(num_env=1) recipe
believing it produces a working real decode -- it does not, despite passing
the byte-count oracle.
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
    ngroups = (swb[sfb_idx + 1] - swb[sfb_idx]) // 4
    for i in range(ngroups):
        quad = (2, 0, 0, 0) if i == group_active else (0, 0, 0, 0)
        length, code = P.HCB3[P.tuple_index_u(quad, 2, 4)]
        signs = [1 for v in quad if v != 0]
        w.w(code, length)
        for _ in signs:
            w.w(0, 1)
    w.w(0, 3)  # pulse/tns/gain-control absent
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
    """`n_frames` FIXFIX/1-env frames, header on the first, `bs_invf_mode`
    taken from `invf_seq[i]` (repeats last if shorter)."""
    grid = dict(frame_class=0, num_env=1, freq_res=0)
    frames = [P.silent(SF_INDEX)]
    for i in range(n_frames):
        mode = invf_seq[min(i, len(invf_seq) - 1)]
        c = dict(cfg, invf=mode, header=1)
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
        w.w(13, 4)
        w.wbits(body.bits)
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
