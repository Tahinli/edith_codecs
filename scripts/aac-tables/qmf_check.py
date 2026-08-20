#!/usr/bin/env python3
"""Numpy prototype of the ISO/IEC 14496-3 4.6.18.4.1 (32-band analysis) and
4.6.18.8.2 (64-band synthesis) QMF banks, run back-to-back as a passthrough
round-trip (upper 32 synthesis subbands zeroed) to verify the exact carrier
equations before porting to Rust. See ledger sbr-hunt-resume for context.

Usage: python3 qmf_check.py
"""
import numpy as np

TAPS_PATH = "/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/041875c4-7950-4e24-950e-7b6e9eb5ba3c/scratchpad/qmf_proto_640.txt"
c = np.array([float(x) for x in open(TAPS_PATH)])
assert c.shape == (640,)


class Analysis32:
    """4.6.18.4.1: 32 new real samples in -> 32 complex subbands out per slot."""

    def __init__(self, window):
        self.c = window
        self.x = np.zeros(320)  # analysis history buffer, x[0]=newest

    def process_slot(self, new32, newest_at_zero=True, tap=None, sign=-1.0):
        x = self.x
        # shift: x[n] = x[n-32] for n=319..32 ; new samples into x[31..0]
        x[32:320] = x[0:288]
        if newest_at_zero:
            x[0:32] = new32[::-1]  # newest at x[0]
        else:
            x[0:32] = new32  # oldest-at-front variant
        if tap == "c2n":
            Z = x * self.c[0::2][: len(x)]
        else:
            Z = x * self.c[: len(x)]
        u = np.zeros(64)
        for n in range(64):
            u[n] = Z[n] + Z[n + 64] + Z[n + 128] + Z[n + 192] + Z[n + 256]
        k = np.arange(32)
        n = np.arange(64)
        # X[k] = sum_n u[n] * exp(sign * j*pi/64*(k+0.5)*(2n-1))
        phase = sign * 1j * np.pi / 64.0 * np.outer(2 * n - 1, k + 0.5)
        X = u @ np.exp(phase)
        self.x = x
        return X  # shape (32,) complex


class Synthesis64:
    """4.6.18.8.2: 64 complex subbands in -> 64 real samples out per slot."""

    def __init__(self, window, sign=1.0, use_re=True):
        self.c = window
        self.v = np.zeros(1280)
        self.sign = sign
        self.use_re = use_re

    def process_slot(self, X64):
        v = self.v
        v[128:1280] = v[0:1152]
        k = np.arange(64)
        n = np.arange(128)
        phase = self.sign * 1j * np.pi / 128.0 * np.outer(2 * n - 255, k + 0.5)
        vv = (1.0 / 64.0) * (np.exp(phase) @ X64)
        v[0:128] = vv.real if self.use_re else (vv.real + vv.imag)
        self.v = v
        g = np.zeros(640)
        for i in range(5):
            g[128 * i : 128 * i + 64] = v[256 * i : 256 * i + 64]
            g[128 * i + 64 : 128 * i + 128] = v[256 * i + 192 : 256 * i + 256]
        w = g * self.c
        y = np.zeros(64)
        for k_ in range(10):
            y += w[64 * k_ : 64 * k_ + 64]
        return y


def run_trial(name, syn_sign=1.0, syn_use_re=True, **kw):
    rng = np.random.default_rng(0)
    n_slots = 128  # 8192 samples / 64
    x_in = rng.standard_normal(8192)
    ana = Analysis32(c)
    syn = Synthesis64(c, sign=syn_sign, use_re=syn_use_re)
    out = np.zeros(8192 * 2)
    for s in range(n_slots):
        new32 = x_in[s * 32 : s * 32 + 32]
        X32 = ana.process_slot(new32, **{k: v for k, v in kw.items() if k in ("newest_at_zero", "tap", "sign")})
        X64 = np.zeros(64, dtype=complex)
        X64[0:32] = X32
        y64 = syn.process_slot(X64)
        out[s * 64 : s * 64 + 64] = y64

    best_corr = -2.0
    best_lag = None
    best_ratio = None
    for lag in range(0, 800):
        a = out[lag : lag + 8192]
        b = x_in[: len(a)]
        if len(a) < 4000:
            continue
        # correlate against decimated-out-at-even-samples vs input directly:
        oa = out[lag::2][: len(x_in)]
        n = min(len(oa), len(x_in)) - 10
        oa = oa[:n]
        bb = x_in[:n]
        if np.std(oa) < 1e-12:
            continue
        corr = np.corrcoef(oa, bb)[0, 1]
        if corr > best_corr:
            best_corr = corr
            best_lag = lag
            best_ratio = np.std(oa) / np.std(bb)
    pass  # summarized in table below
    return best_corr


if __name__ == "__main__":
    results = {}
    for newest_at_zero in (True, False):
        for tap in (None, "c2n"):
            for sign in (-1.0, 1.0):
                for syn_sign in (-1.0, 1.0):
                    for syn_use_re in (True, False):
                        name = (
                            f"newest0={newest_at_zero} tap={tap} asign={sign:+.0f} "
                            f"ssign={syn_sign:+.0f} re_only={syn_use_re}"
                        )
                        results[name] = run_trial(
                            name,
                            newest_at_zero=newest_at_zero,
                            tap=tap,
                            sign=sign,
                            syn_sign=syn_sign,
                            syn_use_re=syn_use_re,
                        )
    print()
    print("Trial table (sorted):")
    for k, v in sorted(results.items(), key=lambda kv: -kv[1]):
        print(f"  {k}: corr={v:.6f}")
