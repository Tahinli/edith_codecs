# lane opus-vbr r1 — libopus `compute_vbr` ported whole — KEEP

## What
- `celt_enc.rs`: VBR target now follows libopus celt_encoder.c:1605-1716 term by term: `coded_bins` from `lastCodedBands` + intensity, stereo_saving term (`max_frac`/`coded_stereo_dof`), `tot_boost-(19<<LM)`, TF boost `(tf_estimate-0.044)*target`, floor-depth cap from `dynalloc_analysis` `maxDepth`, constrained-VBR 0.67 pull, temporal VBR (`spec_avg` follower state, `amount` from equiv_rate), 2x base cap. Replaces the `7/4` transient stand-in, the `0.012*target` tf stand-in and the peak-level activity stand-in.
- `alloc_trim_analysis` now also maintains `stereo_saving` (logXC/logXC2 from LF and min inter-channel correlation up to the intensity band); the trim value it returns is unchanged (the trim port alone was rejected, lanes/opus-trim-r1.report.md).
- `lastCodedBands` moves at most one band per frame (libopus); measured neutral (r3 == r1 to 4 dp).
- Intensity decision moved before the trim block (libopus order); skipped terms named in code: tonality/activity (analysis invalid), surround mask, pitch_change, lfe, qext 2-pass.

## Deviation kept by measurement
TF boost factor: C has `SHL32(MULT16_32_Q15(tf_estimate-0.044, target), 1)` (x2). r2 (x2 + lastCodedBands ±1) vs r3 (x1 + ±1): sadie@64 +.0023 vs −.0007, hein@64 +.0011 vs −.0014, sadie@96 +.0003 vs −.0016, rate −1.0% vs −0.3%. x1 kept.

## Gate (14 rows, gap = ref corr − ours; negative = ours ahead) vs lanes/opus-int-r1.sweep.txt
```
nik@64k: r 69.2 o 69.1 kbps (-0.1%), o=0.9887 r=0.9864 gap=-0.0024 base=-0.0020
nik@96k: r 101.0 o 100.9 kbps (-0.1%), o=0.9947 r=0.9940 gap=-0.0007 base=-0.0006
zaur@64k: r 63.6 o 63.6 kbps (-0.0%), o=0.9893 r=0.9866 gap=-0.0027 base=-0.0018
zaur@96k: r 93.9 o 93.4 kbps (-0.5%), o=0.9947 r=0.9937 gap=-0.0009 base=-0.0008
her@64k: r 67.9 o 67.7 kbps (-0.4%), o=0.9834 r=0.9794 gap=-0.0040 base=-0.0031
her@96k: r 101.5 o 101.0 kbps (-0.5%), o=0.9914 r=0.9902 gap=-0.0012 base=-0.0008
naz@64k: r 71.3 o 71.3 kbps (-0.0%), o=0.9908 r=0.9880 gap=-0.0029 base=-0.0028
naz@96k: r 105.4 o 105.3 kbps (-0.0%), o=0.9956 r=0.9948 gap=-0.0008 base=-0.0008
sadie@64k: r 63.3 o 63.1 kbps (-0.3%), o=0.9866 r=0.9859 gap=-0.0007 base=+0.0038
sadie@96k: r 84.8 o 84.7 kbps (-0.2%), o=0.9935 r=0.9920 gap=-0.0016 base=-0.0000
dl8a@64k: r 65.8 o 65.7 kbps (-0.1%), o=0.9889 r=0.9865 gap=-0.0025 base=-0.0017
dl8a@96k: r 96.8 o 96.5 kbps (-0.3%), o=0.9942 r=0.9934 gap=-0.0009 base=-0.0006
hein@64k: r 64.9 o 64.8 kbps (-0.2%), o=0.9888 r=0.9874 gap=-0.0014 base=+0.0022
hein@96k: r 86.9 o 86.8 kbps (-0.2%), o=0.9946 r=0.9930 gap=-0.0016 base=-0.0002
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```
Every row gains or holds; ours ahead of libopus on all 14 rows at equal size. Rate shortfall on sadie/hein gone (−1.6/−1.8% → −0.3/−0.2%).

## Suite
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 27 passed; 0 failed; 9 ignored; 0 measured; 0 filtered out; finished in 34.12s
(r3 code differs from r2 only by the TF factor; suite rerun on main after merge.)
