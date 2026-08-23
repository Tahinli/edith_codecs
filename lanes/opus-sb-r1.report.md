# lane-opus-sb r1 — short-block coding at 64k: dynalloc was the divergence

## Instrument
`CeltDecDiag` now exposes the allocation as decoded (alloc_trim, pulses, fine_quant, intensity,
dual_stereo, coded_bands, balance, dynalloc offsets). New ignored test
`short_block_bits_vs_libopus` (SWEEP_ONLY, SWEEP_KBPS) decodes both streams with our decoder and
tabulates per-band means for long vs short frames, ours vs libopus → lanes/opus-sb-r1.bits.txt.

## Finding (sadie/hein @64k, before)
- Dynalloc boost on the top coded band 14.4–18.7 kHz: ours 219–260 1/8-bit per frame, libopus 3.
- libopus boosts 240–3000 Hz on short frames (720 Hz: 98 vs ours 4); ours never.
- Net: top band +164..+455 pulses, 720 Hz band −73..−76 pulses vs libopus on short frames.
- Cause: our dynalloc was the old second-difference spike heuristic; the near-empty band above
  14.4 k makes the top band a "spike" every frame.

## Fix
Port of libopus `dynalloc_analysis` (noise floor, follower + median-of-5 masking over long-window
`bandLogE2` — second MDCT pass on transient frames —, stereo cross-talk, CVBR steady-frame halving,
i<8 ×2 / i≥12 ×½, width-scaled boost units, 2/3 cap). The coding loop already matched.

## After (bits table, short frames): 720 Hz boost ours 100 / ref 98; top band 4.6 / 3.4.

## Gate (sadie,hein,her,naz; lanes/opus-sb-r1.gate4.txt) vs lanes/opus-64-r5.sweep.txt
sadie@64 gap +.0046 → +.0037 (Q −389 → −293, err_ratio .99 → .69), hein@64 +.0031 → +.0022,
naz@64 −.0024 → −.0028, naz@96 err_ratio 3.33 → 2.35; worst row change −.0003 corr (her@96 gap
−.0011 → −.0008); drops 0; rate within ±5%. Verdict KEEP. 14-row gate: lanes/opus-sb-r1.sweep.txt.

## Rejected on top of this (r2): `trim -= 2*tf_estimate` → sadie +.0040, hein +.0025, naz worse.

## Residue
Remaining short-frame trim histogram: ours 0–2, libopus ≈0; top band still +20..+40 pulses on
long frames. sadie@64 +.0037 / hein@64 +.0022 remain the largest ref-better rows.
