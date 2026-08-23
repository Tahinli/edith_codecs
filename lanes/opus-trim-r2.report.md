# opus-trim/r2 — alloc_trim_analysis trim-value port (REJECTED)

Re-measured libopus 1.3+ `alloc_trim_analysis` trim VALUE on top of the
exact `compute_vbr` port (baseline = `opus-vbr-r1`). The stereo_saving
block was already ported in r1 and left untouched; only the trim-value
formula was changed:

- base trim 4 below 64 kb/s ramping to 5 at 80 kb/s (via `equiv_rate`)
- continuous stereo term: `trim += max(-4, 0.75 * log2(1.001 - xc^2))`
- continuous tilt: `trim -= clamp((diff+1)/6, -2, +2)` with `end` (not `NB_BANDS`)
- tf term: `trim -= 2*tf_estimate` (variant A on, B off)
- `floor(0.5 + trim)` clamped 0..10

## Sign rule

gap = ref_corr - ours_corr. **More negative = ours better.**
A rise in gap (less negative) = regression.

KEEP if: no row's gap rises >.0005 vs baseline AND ≥1 row improves ≥.0005.

## Verdict

| Variant | Rows rising >.0005 | Rows improving ≥.0005 | Result |
|---------|--------------------|-----------------------|--------|
| A (tf on)  | 9 of 14 | 0 | **REJECT** |
| B (tf off) | 8 of 14 | 0 | **REJECT** |

Both variants regress the majority of rows by >.0005. No row improves by
≥.0005 in either variant. The continuous trim formula systematically
moves trim toward less negative gaps (worse) across the board — the
quantized heuristic in the baseline was already closer to libopus.

celt_enc.rs reverted to baseline. No code change ships.

## Side-by-side gap table

| Row | Baseline gap | A gap | A Δ | B gap | B Δ |
|-----|-------------|-------|-----|-------|-----|
| nik@64k   | -0.0024 | -0.0003 | +0.0021 ❌ | -0.0003 | +0.0021 ❌ |
| nik@96k   | -0.0007 | -0.0001 | +0.0006 ❌ | -0.0002 | +0.0005    |
| zaur@64k  | -0.0027 | -0.0003 | +0.0024 ❌ | -0.0005 | +0.0022 ❌ |
| zaur@96k  | -0.0009 | -0.0007 | +0.0002    | -0.0007 | +0.0002    |
| her@64k   | -0.0040 | -0.0017 | +0.0023 ❌ | -0.0019 | +0.0021 ❌ |
| her@96k   | -0.0012 | -0.0007 | +0.0005    | -0.0008 | +0.0004    |
| naz@64k   | -0.0029 | -0.0008 | +0.0021 ❌ | -0.0009 | +0.0020 ❌ |
| naz@96k   | -0.0008 | -0.0003 | +0.0005    | -0.0004 | +0.0004    |
| sadie@64k | -0.0007 | +0.0001 | +0.0008 ❌ | -0.0001 | +0.0006 ❌ |
| sadie@96k | -0.0016 | -0.0016 |  0.0000    | -0.0017 | -0.0001 ✅ |
| dl8a@64k  | -0.0025 | +0.0003 | +0.0028 ❌ | +0.0002 | +0.0027 ❌ |
| dl8a@96k  | -0.0009 | -0.0002 | +0.0007 ❌ | -0.0003 | +0.0006 ❌ |
| hein@64k  | -0.0014 | -0.0005 | +0.0009 ❌ | -0.0006 | +0.0008 ❌ |
| hein@96k  | -0.0016 | -0.0015 | +0.0001    | -0.0016 |  0.0000    |

❌ = rises >.0005 (gate failure). ✅ = improves (but <.0005, below threshold).

## Raw sweep data

### Variant A (TRIM_TF_TERM = true)

```
nik@64k: ref 69.2 ours 69.1 kbps (-0.1%), corr o=0.9866 r=0.9864 gap=-0.0003, Q o=-91.16 r=-154.78, err_ratio 0.689, minsec o=0.9753 r=0.9745, drop o=0 r=0
nik@96k: ref 101.0 ours 100.9 kbps (-0.1%), corr o=0.9942 r=0.9940 gap=-0.0001, Q o=-32.27 r=-31.55, err_ratio 1.006, minsec o=0.9880 r=0.9888, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.6 kbps (-0.0%), corr o=0.9870 r=0.9866 gap=-0.0003, Q o=-147.89 r=-123.36, err_ratio 1.147, minsec o=0.9679 r=0.9686, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.4 kbps (-0.5%), corr o=0.9944 r=0.9937 gap=-0.0007, Q o=-109.22 r=-125.18, err_ratio 0.910, minsec o=0.9859 r=0.9854, drop o=0 r=0
her@64k: ref 67.9 ours 67.7 kbps (-0.4%), corr o=0.9811 r=0.9794 gap=-0.0017, Q o=-367.28 r=-339.00, err_ratio 1.109, minsec o=0.9450 r=0.9406, drop o=0 r=0
her@96k: ref 101.5 ours 101.0 kbps (-0.5%), corr o=0.9910 r=0.9902 gap=-0.0007, Q o=-204.20 r=-191.50, err_ratio 1.062, minsec o=0.9719 r=0.9728, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9888 r=0.9880 gap=-0.0008, Q o=-253.87 r=-128.39, err_ratio 1.839, minsec o=0.9705 r=0.9644, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9951 r=0.9948 gap=-0.0003, Q o=-165.27 r=-29.16, err_ratio 2.458, minsec o=0.9847 r=0.9804, drop o=0 r=0
sadie@64k: ref 63.3 ours 63.1 kbps (-0.3%), corr o=0.9858 r=0.9859 gap=+0.0001, Q o=-265.73 r=-392.46, err_ratio 0.619, minsec o=0.9326 r=0.9100, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.7 kbps (-0.2%), corr o=0.9936 r=0.9920 gap=-0.0016, Q o=-145.54 r=-231.16, err_ratio 0.660, minsec o=0.9686 r=0.9511, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.7 kbps (-0.1%), corr o=0.9862 r=0.9865 gap=+0.0003, Q o=-201.52 r=-378.84, err_ratio 0.490, minsec o=0.9745 r=0.9723, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.5 kbps (-0.3%), corr o=0.9936 r=0.9934 gap=-0.0002, Q o=-156.65 r=-423.47, err_ratio 0.336, minsec o=0.9870 r=0.9864, drop o=0 r=0
hein@64k: ref 64.9 ours 64.8 kbps (-0.2%), corr o=0.9878 r=0.9874 gap=-0.0005, Q o=-346.76 r=-388.31, err_ratio 0.861, minsec o=0.9324 r=0.9148, drop o=0 r=0
hein@96k: ref 86.9 ours 86.8 kbps (-0.2%), corr o=0.9945 r=0.9930 gap=-0.0015, Q o=-117.49 r=-246.46, err_ratio 0.527, minsec o=0.9686 r=0.9499, drop o=0 r=0
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```

### Variant B (TRIM_TF_TERM = false)

```
nik@64k: ref 69.2 ours 69.1 kbps (-0.1%), corr o=0.9867 r=0.9864 gap=-0.0003, Q o=-85.97 r=-154.78, err_ratio 0.666, minsec o=0.9753 r=0.9745, drop o=0 r=0
nik@96k: ref 101.0 ours 100.9 kbps (-0.1%), corr o=0.9942 r=0.9940 gap=-0.0002, Q o=-36.39 r=-31.55, err_ratio 1.043, minsec o=0.9881 r=0.9888, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.6 kbps (-0.0%), corr o=0.9872 r=0.9866 gap=-0.0005, Q o=-147.82 r=-123.36, err_ratio 1.146, minsec o=0.9684 r=0.9686, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.4 kbps (-0.5%), corr o=0.9945 r=0.9937 gap=-0.0007, Q o=-107.04 r=-125.18, err_ratio 0.898, minsec o=0.9862 r=0.9854, drop o=0 r=0
her@64k: ref 67.9 ours 67.7 kbps (-0.4%), corr o=0.9814 r=0.9794 gap=-0.0019, Q o=-367.92 r=-339.00, err_ratio 1.111, minsec o=0.9455 r=0.9406, drop o=0 r=0
her@96k: ref 101.5 ours 101.0 kbps (-0.5%), corr o=0.9911 r=0.9902 gap=-0.0008, Q o=-202.04 r=-191.50, err_ratio 1.051, minsec o=0.9721 r=0.9728, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9888 r=0.9880 gap=-0.0009, Q o=-227.93 r=-128.39, err_ratio 1.644, minsec o=0.9706 r=0.9644, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9951 r=0.9948 gap=-0.0004, Q o=-301.83 r=-29.16, err_ratio 4.499, minsec o=0.9848 r=0.9804, drop o=0 r=0
sadie@64k: ref 63.3 ours 63.1 kbps (-0.3%), corr o=0.9861 r=0.9859 gap=-0.0001, Q o=-265.73 r=-392.46, err_ratio 0.619, minsec o=0.9326 r=0.9100, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.7 kbps (-0.2%), corr o=0.9937 r=0.9920 gap=-0.0017, Q o=-145.55 r=-231.16, err_ratio 0.660, minsec o=0.9686 r=0.9511, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.7 kbps (-0.1%), corr o=0.9863 r=0.9865 gap=+0.0002, Q o=-203.11 r=-378.84, err_ratio 0.494, minsec o=0.9745 r=0.9723, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.5 kbps (-0.3%), corr o=0.9937 r=0.9934 gap=-0.0003, Q o=-152.34 r=-423.47, err_ratio 0.329, minsec o=0.9870 r=0.9864, drop o=0 r=0
hein@64k: ref 64.9 ours 64.8 kbps (-0.2%), corr o=0.9880 r=0.9874 gap=-0.0006, Q o=-346.76 r=-388.31, err_ratio 0.861, minsec o=0.9324 r=0.9148, drop o=0 r=0
hein@96k: ref 86.9 ours 86.8 kbps (-0.2%), corr o=0.9947 r=0.9930 gap=-0.0016, Q o=-117.49 r=-246.46, err_ratio 0.527, minsec o=0.9686 r=0.9499, drop o=0 r=0
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```

### Baseline (opus-vbr-r1)

```
nik@64k: ref 69.2 ours 69.1 kbps (-0.1%), corr o=0.9887 r=0.9864 gap=-0.0024, Q o=-95.23 r=-154.78, err_ratio 0.708, minsec o=0.9766 r=0.9745, drop o=0 r=0
nik@96k: ref 101.0 ours 100.9 kbps (-0.1%), corr o=0.9947 r=0.9940 gap=-0.0007, Q o=-29.53 r=-31.55, err_ratio 0.982, minsec o=0.9889 r=0.9888, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.6 kbps (-0.0%), corr o=0.9893 r=0.9866 gap=-0.0027, Q o=-202.82 r=-123.36, err_ratio 1.510, minsec o=0.9702 r=0.9686, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.4 kbps (-0.5%), corr o=0.9947 r=0.9937 gap=-0.0009, Q o=-114.74 r=-125.18, err_ratio 0.940, minsec o=0.9862 r=0.9854, drop o=0 r=0
her@64k: ref 67.9 ours 67.7 kbps (-0.4%), corr o=0.9834 r=0.9794 gap=-0.0040, Q o=-365.68 r=-339.00, err_ratio 1.102, minsec o=0.9476 r=0.9406, drop o=0 r=0
her@96k: ref 101.5 ours 101.0 kbps (-0.5%), corr o=0.9914 r=0.9902 gap=-0.0012, Q o=-202.16 r=-191.50, err_ratio 1.052, minsec o=0.9728 r=0.9728, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9908 r=0.9880 gap=-0.0029, Q o=-351.70 r=-128.39, err_ratio 2.697, minsec o=0.9720 r=0.9644, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9956 r=0.9948 gap=-0.0008, Q o=-301.83 r=-29.16, err_ratio 4.499, minsec o=0.9855 r=0.9804, drop o=0 r=0
sadie@64k: ref 63.3 ours 63.1 kbps (-0.3%), corr o=0.9866 r=0.9859 gap=-0.0007, Q o=-266.99 r=-392.46, err_ratio 0.623, minsec o=0.9326 r=0.9100, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.7 kbps (-0.2%), corr o=0.9935 r=0.9920 gap=-0.0016, Q o=-145.55 r=-231.16, err_ratio 0.660, minsec o=0.9687 r=0.9511, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.7 kbps (-0.1%), corr o=0.9889 r=0.9865 gap=-0.0025, Q o=-227.02 r=-378.84, err_ratio 0.551, minsec o=0.9787 r=0.9723, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.5 kbps (-0.3%), corr o=0.9942 r=0.9934 gap=-0.0009, Q o=-177.72 r=-423.47, err_ratio 0.374, minsec o=0.9881 r=0.9864, drop o=0 r=0
hein@64k: ref 64.9 ours 64.8 kbps (-0.2%), corr o=0.9888 r=0.9874 gap=-0.0014, Q o=-344.79 r=-388.31, err_ratio 0.855, minsec o=0.9324 r=0.9148, drop o=0 r=0
hein@96k: ref 86.9 ours 86.8 kbps (-0.2%), corr o=0.9946 r=0.9930 gap=-0.0016, Q o=-117.88 r=-246.46, err_ratio 0.528, minsec o=0.9686 r=0.9499, drop o=0 r=0
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```
