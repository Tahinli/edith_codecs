#!/usr/bin/env python3
"""bench-rows-parity.py REF.f32 OUT.f32 CHANNELS: per-channel correlation of two
interleaved float32 streams (ffmpeg reference vs the crate's output).
Prints one line per channel and the sample counts. Run as: python3 -I <this file> ..."""
import sys
import numpy as np

ref = np.fromfile(sys.argv[1], dtype="<f4")
out = np.fromfile(sys.argv[2], dtype="<f4")
ch = int(sys.argv[3])
n = min(len(ref), len(out)) // ch * ch
print(f"ref_samples={len(ref) // ch} out_samples={len(out) // ch} compared={n // ch}")
r = ref[:n].reshape(-1, ch)
o = out[:n].reshape(-1, ch)
worst = 1.0
for c in range(ch):
    a, b = r[:, c].astype(np.float64), o[:, c].astype(np.float64)
    corr = np.corrcoef(a, b)[0, 1] if a.std() > 0 and b.std() > 0 else float("nan")
    err = np.max(np.abs(a - b))
    worst = min(worst, corr)
    print(f"ch{c} corr={corr:.6f} max_abs_err={err:.6f}")
print(f"min_corr={worst:.6f} tolerance=0.999 {'PASS' if worst >= 0.999 else 'FAIL'}")
