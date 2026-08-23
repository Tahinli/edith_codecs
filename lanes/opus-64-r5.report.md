# lane-opus-64 r5 — transient_analysis port shipped; tf_estimate consumers rejected by measurement

State shipped: r4 `transient_analysis` port (tf_estimate/tf_chan computed, `TF_ANALYSIS=false`).

## 14-row gate (lanes/opus-64-r5.sweep.txt) vs lanes/opus-naz-r2.sweep.txt
- her@96 err_ratio 10.13 -> 1.18 (Q -900 -> -234), her@64 4.65 -> 0.76. Residue (b) closed.
- sadie@64 gap +.0043 -> +.0046 (minsec .9151 -> .9085); hein@64 gap +.0020 -> +.0031 (corr -.0011).
- All other rows within +-.0005; RATE GATE all within +-5%; DROPOUT GATE passed.

## Ablations on sadie/hein@64 (logs opus-64-r5.*.log)
- libopus VBR `target += 2*(tf_estimate-0.044)*target` replacing the 7/4 stand-in: rate -1.8%,
  gap +.0074/+.0052, 1 s dropouts each -> rejected.
- libopus trim `-= 2*tf_estimate`: gap +.0049/+.0034 (= baseline noise) -> rejected.
- Both: gap +.0077/+.0056, drops -> rejected.

## Cause narrowed (spectral_divergence_vs_libopus, new transient count, SWEEP_KBPS=64)
transient frames ours/ref: her 529/533, sadie 1807/1788, hein 2028/2054 (<=1.5%).
Detector parity holds; the sadie/hein 64k residue is how short-block frames are coded
(~30% of frames on those sources), not how many are declared. tf_analysis ON was neutral.

## Test surface
- `spectral_divergence_vs_libopus`: `SWEEP_KBPS` env, sources her/sadie/hein, prints
  decoder-seen transient counts ours vs libopus.
