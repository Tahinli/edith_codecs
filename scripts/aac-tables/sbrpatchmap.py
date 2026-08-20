#!/usr/bin/env python3
"""Round-27: measures the reference decoder's ACTUAL SBR HF patch map (which
low QMF source band feeds which high target band), by isolating one source
band at a time and reading off which target bands carry power in the output.

Method (one-band-at-a-time, simpler than a multi-line comb and avoids any
ambiguity from overlapping lines): the core spectrum for a probe frame is a
single isolated MDCT bin -- `tone_core()` in `sbrchirp.py`, already proven to
place value 2 in one 4-bin group and ZERO_HCB everywhere else -- at the first
bin of low QMF band `p` (`k = p*16`, always a group boundary since a QMF band
is 16 MDCT bins wide and a group is 4). Patching copies that source band's
complex QMF series verbatim (mixed by LPC/chirp, which round-19 already
measured as a <0.1% power effect regardless of `bs_invf_mode`) into every
target band any patch maps it to, so an FFT of the decoded output that finds
elevated power in target band `q` means "patch measured: source `p` -> target
`q`" directly, no fractional-offset bookkeeping needed.

Runs the same probes through our own decoder via the `adts_to_pcm` example
(`cargo build -p ec-aac --example adts_to_pcm`) for the sanity cross-check the
charter requires: our measured map must equal `build_patches`' prediction, or
the fingerprint method itself is the thing that's broken.
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


def stream_one_band(cfg, tables, active_p, n_frames=16, invf_mode=0):
    """`n_frames` FIXFIX/2-env amp_res=1 frames (round-19's engaging shape),
    core = one isolated tone at low QMF band `active_p`'s first bin, held
    fixed; envelope flat/loud, noise floor minimal, invf NONE, no harmonics."""
    swb = P.SWB_LONG[SF_INDEX]
    k = active_p * 16
    found = bin_to_sfb_group(k, swb)
    assert found is not None, f"band {active_p} bin {k} not in SWB_LONG[{SF_INDEX}]"
    sfb_idx, group_active = found
    grid = dict(frame_class=0, num_env=2, freq_res=0)
    frames = [P.silent(SF_INDEX)]
    for _ in range(n_frames):
        c = dict(cfg, invf=invf_mode, header=1, amp_res=1, element="sce", coupling=0,
                 df_env=[0] * 8, df_noise=[0, 0], env0=[40] * 8, noise0=[2, 2])
        w = P.BitW()
        C.tone_core(w, sfb_idx, group_active)
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


def our_decode(frames, tag):
    src = f"/tmp/sbrpatchmap-our-{tag}.aac"
    with open(src, "wb") as f:
        f.write(b"".join(frames))
    env = dict(os.environ, EC_AAC_SBR_SAME_RATE="1", EC_AAC_SBR_CHANNELS="1")
    r = subprocess.run([OUR_DECODER, src], capture_output=True, env=env)
    os.remove(src)
    return np.frombuffer(r.stdout, dtype="<f4").astype(np.float64), r.stderr.decode(errors="replace")


def band_powers(pcm, kx, k2, n_fft=8192, skip_samples=5120):
    """Power in each target band `q in [kx, k2)`, from a steady-state FFT
    window (RATE/128 Hz per band, `n_fft=8192` gives ~5.4 Hz/bin -- fine
    enough that a single active source band's line falls cleanly in one
    band's bin range even without knowing its exact intra-band position)."""
    seg = pcm[skip_samples : skip_samples + n_fft]
    if len(seg) < n_fft:
        seg = np.pad(seg, (0, n_fft - len(seg)))
    spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg)))) ** 2
    freqs = np.fft.rfftfreq(n_fft, 1 / RATE)
    powers = {}
    for q in range(kx, k2):
        lo, hi = q * RATE / 128, (q + 1) * RATE / 128
        mask = (freqs >= lo) & (freqs < hi)
        powers[q] = float(spec[mask].sum())
    return powers


def measured_targets(powers, rel_threshold=0.05):
    """Bands whose power is at least `rel_threshold` of the loudest band's --
    the loudest is always the patch that actually fired, background/leakage
    bands sit orders of magnitude lower."""
    if not powers:
        return []
    top = max(powers.values())
    if top <= 0:
        return []
    return sorted(q for q, p in powers.items() if p >= rel_threshold * top)


def run_header(label, start_freq, stop_freq, freq_scale, alter_scale, noise_bands, xover_band):
    cfg, tables = make_tables(start_freq, stop_freq, freq_scale, alter_scale, noise_bands, xover_band)
    assert tables is not None, f"{label}: header does not produce valid tables"
    kx, k2 = tables["kx"], tables["k2"]
    print(f"\n== {label}: start={start_freq} stop={stop_freq} scale={freq_scale} alter={alter_scale}"
          f" noise={noise_bands} xover={xover_band} -> kx={kx} k2={k2} n_q={tables['n_q']} ==")
    patches = build_patches_predicted(kx, k2, tables["f_high"])
    print(f"  predicted patches (src_start, tgt_start, width): {patches}")

    ref_map, our_map = {}, {}
    for p in range(kx):
        frames = stream_one_band(cfg, tables, p, n_frames=16)
        pcm_ref, err_ref = P.decode(frames, SF_INDEX, f"{label}-p{p}")
        pcm_our, err_our = our_decode(frames, f"{label}-p{p}")
        pw_ref = band_powers(pcm_ref, kx, k2)
        pw_our = band_powers(pcm_our, kx, k2)
        tgt_ref = measured_targets(pw_ref)
        tgt_our = measured_targets(pw_our)
        pred = predicted_targets(patches, p)
        ref_map[p] = tgt_ref
        our_map[p] = tgt_our
        flag = "" if tgt_ref == pred else "  <<< DIFFERS FROM PREDICTION"
        our_flag = "" if tgt_our == pred else "  <<< OUR DECODER DIFFERS FROM build_patches"
        print(f"  p={p:2d} predicted={pred} measured(ref)={tgt_ref}{flag} measured(ours)={tgt_our}{our_flag}"
              f"  [ref_err={err_ref.strip()[:60]!r} our_err={err_our.strip()[:40]!r}]")
    return kx, k2, patches, ref_map, our_map


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
