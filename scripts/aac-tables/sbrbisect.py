#!/usr/bin/env python3
"""Round-32: field-by-field bisection of `sbrchirp.stream()` (proven to
engage the reference's SBR dequant, env sweep moves power 2941x) against
`sbrpatchmap.stream_one_band()` (never engages -- flat env sweep, warning on
every probe), under the wrap_sbr explicit-ASC harness so implicit-signaling
detection (round-30/31's already-closed blocker) cannot confound the result.

The two writers differ in exactly three respects (read side-by-side against
`sbr_bits_full`, `write_grid` -- FIL type/count/padding, header cadence, and
grid shape fields are IDENTICAL between them, so those charter-listed
candidates are refuted by inspection, not measurement):
  (H) the header/table values: `stream()` uses `one_band_header()` (tiny,
      n_low=1/n_q=1, so no delta slots are ever needed); `stream_one_band()`
      uses a real multi-band header (A: kx=14/k2=43/n_q=3).
  (C) the core content: `broadband_core_bits` (proven-clean comb) vs
      `tone_core` (round-27/31 flagged, still-live suspicion).
  (W) the envelope/noise delta-Huffman words: `[]` (fine for H's n_slots=0)
      vs explicit `[zero_delta] * n_slots` (round-29's FILL-desync fix,
      needed once H has real bands).
(W) is mechanically forced by (H) -- header A has n_slots > 0, so it MUST
carry real words or decode is bit-desynced regardless of anything else; it
is not an independent flip, it is included with H below.

Builds each combination via `stream_one_band`'s own frame writer (broadband
core is swapped in unmodified for the (C=broadband) rows), wraps every env0
value through `wrap_sbr` (explicit-SBR ASC, mp4/esds), decodes with the
reference, and reports whether power in the predicted in-band window moves
across an env0 sweep (engagement) or not.
"""
import os
import subprocess
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sbrchirp as C  # noqa: E402
import sbrpatchmap as PM  # noqa: E402
import sbrpayload_fixtures as F  # noqa: E402
import sbrprobe as P  # noqa: E402

SF_INDEX = 7
RATE = 44100
WRAP = os.path.expanduser("~/.cache/cargo-target-sbr/debug/examples/wrap_sbr")
NO_QUANT_WARNING = "No quantized data read for sbr_dequant."


def build(header_tables, use_words, core, active_p, env0_raw, n_frames=24):
    """One stream, mirroring `stream_one_band`'s frame writer exactly, but
    with (header_tables, use_words, core) as independent flips."""
    cfg, tables = header_tables
    swb = P.SWB_LONG[SF_INDEX]
    if core == "tone":
        k = active_p * 32 + 16
        sfb_idx, group_active = PM.bin_to_sfb_group(k, swb)
    grid = dict(frame_class=0, num_env=2, freq_res=0)
    zero_delta = [0]
    n_slots = 2 * (tables["n_low"] - 1) + 2 * (tables["n_q"] - 1)
    frames = [P.silent(SF_INDEX)]
    for _ in range(n_frames):
        c = dict(cfg, invf=0, header=1, amp_res=1, element="sce", coupling=0,
                 df_env=[0] * 8, df_noise=[0, 0], env0=[env0_raw] * 8, noise0=[2, 2])
        w = P.BitW()
        if core == "tone":
            C.tone_core(w, sfb_idx, group_active)
        else:
            C.broadband_core_bits(w)
        words = ([zero_delta] * n_slots) if use_words else []
        body = F.sbr_bits_full(c, tables, grid, words)
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


def wrap_and_decode(frames, tag, kx):
    raw = f"/tmp/sbrbisect-{tag}.aac"
    mp4 = f"/tmp/sbrbisect-{tag}.mp4"
    with open(raw, "wb") as f:
        f.write(b"".join(frames))
    r = subprocess.run([WRAP, raw, "22050", "44100", "1", mp4], capture_output=True)
    assert r.returncode == 0, f"wrap_sbr failed: {r.stderr.decode()}"
    dec = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", mp4, "-f", "f32le", "-ac", "1", "-"],
        capture_output=True,
    )
    pcm = np.frombuffer(dec.stdout, dtype="<f4").astype(np.float64)
    err = dec.stderr.decode(errors="replace")
    os.remove(raw)
    os.remove(mp4)
    return pcm, err


def inband_power(pcm, kx, k2, sr=RATE, n_fft=8192, skip=5):
    lo, hi = kx * sr / 128, k2 * sr / 128
    start = skip * 1024
    seg = pcm[start : start + n_fft]
    if len(seg) < n_fft:
        seg = np.pad(seg, (0, n_fft - len(seg)))
    spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg))))
    freqs = np.fft.rfftfreq(n_fft, 1 / sr)
    mask = (freqs >= lo) & (freqs < hi)
    return float(np.sum(spec[mask] ** 2))


def engagement_sweep(label, header_tables, use_words, core, active_p=0):
    cfg, tables = header_tables
    kx, k2 = tables["kx"], tables["k2"]
    pwrs, warns = [], []
    for env0 in (3, 12, 20, 30, 40, 50, 60):
        frames = build(header_tables, use_words, core, active_p, env0)
        pcm, err = wrap_and_decode(frames, f"{label}-{env0}", kx)
        warns.append(NO_QUANT_WARNING in err)
        pwrs.append(inband_power(pcm, kx, k2))
    ratio = (max(pwrs) / min(pwrs)) if min(pwrs) > 0 else float("inf")
    engaged = ratio > 10.0
    print(f"{label:40s} kx={kx:3d} k2={k2:3d} pwrs={['%.3e' % p for p in pwrs]} "
          f"ratio={ratio:.3e} warn_every_frame={all(warns)} "
          f"=> {'ENGAGED' if engaged else 'flat'}")
    return engaged


if __name__ == "__main__":
    assert os.path.exists(WRAP), f"build first: cargo build -p ec-aac --example wrap_sbr ({WRAP})"

    default_ct = F.one_band_header()
    header_a_ct = PM.make_tables(start_freq=5, stop_freq=8, freq_scale=2, alter_scale=1,
                                  noise_bands=2, xover_band=0)

    print("== baseline: default header, broadband core (stream()'s own shape) ==")
    engagement_sweep("H=default,C=broadband,W=[]", default_ct, False, "broadband")

    print("\n== flip H only: header A, broadband core, real words ==")
    engagement_sweep("H=A,C=broadband,W=real", header_a_ct, True, "broadband")

    print("\n== flip C only: default header, tone core, W=[] ==")
    engagement_sweep("H=default,C=tone,W=[]", default_ct, False, "tone")

    print("\n== flip both (stream_one_band's actual shape): header A, tone core, real words ==")
    engagement_sweep("H=A,C=tone,W=real", header_a_ct, True, "tone")
