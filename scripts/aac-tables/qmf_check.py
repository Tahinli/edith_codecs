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


class Analysis64:
    """4.6.18.4: 64 new real samples in -> 64 complex subbands out per slot
    (same-rate variant, x of 640, full c[n] tap, folded to 128)."""

    def __init__(self, window):
        self.c = window
        self.x = np.zeros(640)  # x[0]=newest

    def process_slot(self, new64):
        x = self.x
        x[64:640] = x[0:576]
        x[0:64] = new64[::-1]  # newest at x[0]
        Z = x * self.c
        u = np.zeros(128)
        for n in range(128):
            u[n] = Z[n] + Z[n + 128] + Z[n + 256] + Z[n + 384] + Z[n + 512]
        k = np.arange(64)
        n = np.arange(128)
        phase = 1j * np.pi / 128.0 * np.outer(2 * n - 1, k + 0.5)
        X = u @ np.exp(phase)
        self.x = x
        return X  # shape (64,) complex


class Synthesis32Downsampled:
    """4.6.18.8.3: 32 complex subbands in -> 32 real samples out per slot
    (downsampled, same-rate variant)."""

    def __init__(self, window):
        self.c = window
        self.v = np.zeros(640)

    def process_slot(self, X32):
        v = self.v
        v[64:640] = v[0:576]
        k = np.arange(32)
        n = np.arange(64)
        phase = 1j * np.pi / 64.0 * np.outer(2 * n - 63, k + 0.5)
        vv = (1.0 / 32.0) * (np.exp(phase) @ X32)
        v[0:64] = vv.real
        self.v = v
        g = np.zeros(320)
        for i in range(5):
            g[64 * i : 64 * i + 32] = v[128 * i : 128 * i + 32]
            g[64 * i + 32 : 64 * i + 64] = v[128 * i + 96 : 128 * i + 128]
        w = g * self.c[0::2]
        y = np.zeros(32)
        for k_ in range(10):
            y += w[32 * k_ : 32 * k_ + 32]
        return y


class Analysis64Param:
    """64-band analysis with the shift-direction (T1) and modulation-index
    (T2) axes exposed as parameters, for cross-check bisection."""

    def __init__(self, window, shift_mode="reverse_newest_at_0", mod_sign=-1.0):
        self.c = window
        self.x = np.zeros(640)
        self.shift_mode = shift_mode
        self.mod_sign = mod_sign  # -1.0 -> (2n-1), +1.0 -> (2n+1)

    def process_slot(self, new64):
        x = self.x
        if self.shift_mode == "reverse_newest_at_0":
            x[64:640] = x[0:576]
            x[0:64] = new64[::-1]
        else:  # "forward_newest_at_top": x[n] <- x[n+64], new64 forward into x[576:640]
            x[0:576] = x[64:640]
            x[576:640] = new64
        Z = x * self.c
        u = np.zeros(128)
        for n in range(128):
            u[n] = Z[n] + Z[n + 128] + Z[n + 256] + Z[n + 384] + Z[n + 512]
        k = np.arange(64)
        n = np.arange(128)
        expo = 2 * n + 1 if self.mod_sign > 0 else 2 * n - 1
        phase = 1j * np.pi / 128.0 * np.outer(expo, k + 0.5)
        X = u @ np.exp(phase)
        self.x = x
        return X


class Synthesis64Param:
    """64-band synthesis with shift-direction (T1), modulation offset (T2:
    2n-255 vs 2n+1) and the 1/64 scale (T3) exposed as parameters."""

    def __init__(self, window, shift_mode="reverse_newest_at_0", mod_variant="2n-255", scale=True):
        self.c = window
        self.v = np.zeros(1280)
        self.shift_mode = shift_mode
        self.mod_variant = mod_variant
        self.scale = scale

    def process_slot(self, X64):
        v = self.v
        k = np.arange(64)
        n = np.arange(128)
        expo = (2 * n + 1) if self.mod_variant == "2n+1" else (2 * n - 255)
        phase = 1j * np.pi / 128.0 * np.outer(expo, k + 0.5)
        scale_factor = (1.0 / 64.0) if self.scale else 1.0
        vv = scale_factor * (np.exp(phase) @ X64)
        if self.shift_mode == "reverse_newest_at_0":
            v[128:1280] = v[0:1152]
            v[0:128] = vv.real
        else:  # "forward_newest_at_top"
            v[0:1152] = v[128:1280]
            v[1152:1280] = vv.real
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


def run_same_rate_64_param(shift_mode, mod_sign, mod_variant, scale, n_slots=160, lag_max=1300):
    rng = np.random.default_rng(0)
    x_in = rng.standard_normal(64 * n_slots)
    ana = Analysis64Param(c, shift_mode=shift_mode, mod_sign=mod_sign)
    syn = Synthesis64Param(c, shift_mode=shift_mode, mod_variant=mod_variant, scale=scale)
    out = np.zeros(64 * n_slots)
    for s in range(n_slots):
        X64 = ana.process_slot(x_in[64 * s : 64 * s + 64])
        out[64 * s : 64 * s + 64] = syn.process_slot(X64)
    energy_ratio = np.sum(out**2) / np.sum(x_in**2)
    corr, lag, ratio = best_lag_corr(out, x_in, lag_max)
    return corr, lag, ratio, energy_ratio


def bisect_t1_t2_t3():
    print()
    print("T1/T2/T3 cross-check bisection (64->64, same family):")
    rows = []
    for shift_mode in ("reverse_newest_at_0", "forward_newest_at_top"):
        for mod_sign in (-1.0, 1.0):  # analysis (2n-1) vs (2n+1)
            for mod_variant in ("2n-255", "2n+1"):  # synthesis
                for scale in (True, False):
                    corr, lag, ratio, energy = run_same_rate_64_param(shift_mode, mod_sign, mod_variant, scale)
                    name = f"shift={shift_mode} a_mod={'2n+1' if mod_sign>0 else '2n-1'} s_mod={mod_variant} scale64={scale}"
                    rows.append((name, corr, lag, ratio, energy))
    rows.sort(key=lambda r: -r[1])
    for name, corr, lag, ratio, energy in rows:
        print(f"  {name}: corr={corr:.6f} lag={lag} amp_ratio={ratio} energy_ratio={energy:.6f}")
    return rows


def best_lag_corr(out, x_in, lag_max):
    """Same-rate (no decimation) lag search."""
    best_corr, best_lag, best_ratio = -2.0, None, None
    for lag in range(0, lag_max):
        a = out[lag:]
        n = min(len(a), len(x_in)) - 10
        if n < 4000:
            continue
        a, b = a[:n], x_in[:n]
        if np.std(a) < 1e-12:
            continue
        corr = np.corrcoef(a, b)[0, 1]
        if corr > best_corr:
            best_corr, best_lag, best_ratio = corr, lag, np.std(a) / np.std(b)
    return best_corr, best_lag, best_ratio


def run_same_rate_64(n_slots=128):
    rng = np.random.default_rng(0)
    x_in = rng.standard_normal(64 * n_slots)
    ana = Analysis64(c)
    syn = Synthesis64(c, sign=1.0, use_re=True)
    out = np.zeros(64 * n_slots)
    for s in range(n_slots):
        X64 = ana.process_slot(x_in[64 * s : 64 * s + 64])
        out[64 * s : 64 * s + 64] = syn.process_slot(X64)
    energy_ratio = np.sum(out**2) / np.sum(x_in**2)
    corr, lag, ratio = best_lag_corr(out, x_in, 900)
    print(f"(1) 64->64 same-family round-trip: corr={corr:.6f} lag={lag} amp_ratio={ratio} energy_ratio={energy_ratio:.6f}")
    return corr


def run_same_rate_32(n_slots=256):
    rng = np.random.default_rng(0)
    x_in = rng.standard_normal(32 * n_slots)
    ana = Analysis32(c)
    syn = Synthesis32Downsampled(c)
    out = np.zeros(32 * n_slots)
    for s in range(n_slots):
        X32 = ana.process_slot(x_in[32 * s : 32 * s + 32], newest_at_zero=True, tap="c2n", sign=1.0)
        out[32 * s : 32 * s + 32] = syn.process_slot(X32)
    energy_ratio = np.sum(out**2) / np.sum(x_in**2)
    corr, lag, ratio = best_lag_corr(out, x_in, 700)
    print(f"(2) 32->32 same-family round-trip: corr={corr:.6f} lag={lag} amp_ratio={ratio} energy_ratio={energy_ratio:.6f}")
    return corr


def run_single_band_impulse_spectrum(k_impulse=10):
    """(3)-fallback: single-band impulse at slot 0, k=k_impulse into
    Synthesis64; report where the output spectrum peaks land."""
    syn = Synthesis64(c, sign=1.0, use_re=True)
    n_slots = 20
    out = np.zeros(64 * n_slots)
    for s in range(n_slots):
        X64 = np.zeros(64, dtype=complex)
        if s == 0:
            X64[k_impulse] = 1.0
        out[64 * s : 64 * s + 64] = syn.process_slot(X64)
    spec = np.fft.rfft(out)
    mags = np.abs(spec)
    top2 = np.argsort(mags)[-2:]
    freqs = np.fft.rfftfreq(len(out))
    print(f"single-band impulse k={k_impulse}: top-2 bins {sorted(top2)} at normalized freq {sorted(freqs[top2])}")
    print(f"  expected bin center (band k of 64, full band 0..0.5): {(k_impulse + 0.5) / 128.0:.5f}")


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
    print("Trial table (sorted, 32->64 cross-family):")
    for k, v in sorted(results.items(), key=lambda kv: -kv[1]):
        print(f"  {k}: corr={v:.6f}")

    print()
    print("Same-family bisection:")
    c1 = run_same_rate_64()
    c2 = run_same_rate_32()
    if c1 >= 0.999 and c2 >= 0.999:
        print("Both same-family round-trips pass -> the 32->64 cross-family pairing is the bug (see charter step 3).")
    if c1 < 0.999:
        run_single_band_impulse_spectrum(k_impulse=10)

    rows = bisect_t1_t2_t3()
    best_name, best_corr, best_lag, best_ratio, best_energy = rows[0]
    print()
    print(f"Best T1/T2/T3 configuration: {best_name} corr={best_corr:.6f} amp_ratio={best_ratio} energy_ratio={best_energy:.6f}")

    print()
    print("32-band-analysis -> confirmed-correct-64-band-synthesis cross-family, scale/conjugate probe:")
    for scale_k in (1.0, 2.0, 0.5, np.sqrt(2)):
        for conj in (False, True):
            rng = np.random.default_rng(0)
            n_slots = 160
            x_in = rng.standard_normal(32 * n_slots)
            ana = Analysis32(c)
            syn = Synthesis64Param(c, shift_mode="reverse_newest_at_0", mod_variant="2n-255", scale=True)
            out = np.zeros(64 * n_slots)
            for s in range(n_slots):
                X32 = ana.process_slot(x_in[32 * s : 32 * s + 32], newest_at_zero=True, tap="c2n", sign=1.0)
                if conj:
                    X32 = np.conj(X32)
                X64 = np.zeros(64, dtype=complex)
                X64[0:32] = X32 * scale_k
                out[64 * s : 64 * s + 64] = syn.process_slot(X64)
            energy = np.sum(out**2) / np.sum(x_in**2)
            corr, lag, ratio = best_lag_corr(out, x_in, 900)
            print(f"  scale_k={scale_k:.4f} conj={conj}: corr={corr:.6f} lag={lag} amp_ratio={ratio} energy_ratio={energy:.6f}")
