#!/usr/bin/env python3
"""Objective comparison of two AAC encodes of one reference clip.

Usage:
  aac_score.py <ref.f32le> <channels> <label>=<decoded.f32le>:<encoded.aac> [...]

`ref.f32le` is the reference PCM (interleaved f32le, 48 kHz). Each decoded file is the
same clip after decoding the encoder's output with ffmpeg. The decoder delay is removed
per encoder by cross-correlating the mono mix over the first five seconds (lag 0..4096).

Metrics, all over the aligned overlap, per channel then averaged:
  snr_db     global signal-to-error ratio, 10 log10(sum ref^2 / sum err^2).
  segsnr_db  mean of per-frame SNR over 2048-sample frames, each clipped to [-10, 35] dB.
  nsr_db     band noise-to-signal proxy for NMR: STFT (2048 Hann, hop 2048), 20 log-spaced
             bands from 100 Hz to 20 kHz, per-band 10 log10(noise / signal), clipped to
             [-40, 10] dB, averaged over frames and bands. Lower is better. It has no
             psychoacoustic masking threshold, so it is a proxy, not a standard NMR.
  kbps       encoded size over the reference duration, ADTS headers included.
"""

import sys

import numpy as np

FRAME = 2048
BANDS = np.geomspace(100.0, 20000.0, 21)


def load(path, channels):
    x = np.fromfile(path, dtype="<f4")
    n = len(x) // channels
    return x[: n * channels].reshape(n, channels).astype(np.float64)


def align(ref, dec):
    """Lag (samples, >= 0) that best matches dec to ref on the first five seconds."""
    span = min(len(ref), len(dec), 5 * 48000)
    max_lag = min(4096, len(dec) - span)
    a = ref[:span].mean(axis=1)
    b = dec[: span + max_lag].mean(axis=1)
    size = 1 << int(np.ceil(np.log2(len(b) + span)))
    corr = np.fft.irfft(np.fft.rfft(b, size) * np.conj(np.fft.rfft(a, size)), size)
    lags = corr[: max_lag + 1]
    return int(np.argmax(lags))


def metrics(ref, dec):
    lag = align(ref, dec)
    n = min(len(ref), len(dec) - lag)
    r = ref[:n]
    d = dec[lag : lag + n]
    snr, segs, nsr = [], [], []
    window = np.hanning(FRAME)
    freqs = np.fft.rfftfreq(FRAME, 1.0 / 48000)
    edges = np.searchsorted(freqs, BANDS)
    for c in range(r.shape[1]):
        rc, dc = r[:, c], d[:, c]
        err = rc - dc
        snr.append(10 * np.log10(max(np.sum(rc**2), 1e-20) / max(np.sum(err**2), 1e-20)))
        frames = n // FRAME
        rf = rc[: frames * FRAME].reshape(frames, FRAME)
        ef = err[: frames * FRAME].reshape(frames, FRAME)
        pr = np.sum(rf**2, axis=1)
        pe = np.sum(ef**2, axis=1)
        keep = pr > 1e-10
        seg = 10 * np.log10(np.maximum(pr[keep], 1e-20) / np.maximum(pe[keep], 1e-20))
        segs.append(np.mean(np.clip(seg, -10, 35)))
        R = np.abs(np.fft.rfft(rf * window, axis=1)) ** 2
        E = np.abs(np.fft.rfft(ef * window, axis=1)) ** 2
        rows = []
        for b in range(len(BANDS) - 1):
            lo, hi = edges[b], max(edges[b + 1], edges[b] + 1)
            s = np.sum(R[:, lo:hi], axis=1)
            e = np.sum(E[:, lo:hi], axis=1)
            rows.append(10 * np.log10(np.maximum(e, 1e-20) / np.maximum(s, 1e-20)))
        nsr.append(np.mean(np.clip(np.array(rows), -40, 10)))
    return {
        "lag": lag,
        "snr_db": float(np.mean(snr)),
        "segsnr_db": float(np.mean(segs)),
        "nsr_db": float(np.mean(nsr)),
    }


def main(argv):
    if len(argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    ref_path, channels = argv[1], int(argv[2])
    ref = load(ref_path, channels)
    duration = len(ref) / 48000.0
    for spec in argv[3:]:
        label, rest = spec.split("=", 1)
        dec_path, enc_path = rest.split(":", 1)
        m = metrics(ref, load(dec_path, channels))
        kbps = os_size(enc_path) * 8 / duration / 1000.0
        print(
            f"{label} lag={m['lag']} snr={m['snr_db']:.2f} segsnr={m['segsnr_db']:.2f} "
            f"nsr={m['nsr_db']:.2f} kbps={kbps:.2f}"
        )
    return 0


def os_size(path):
    import os

    return os.path.getsize(path)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
