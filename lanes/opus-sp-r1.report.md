# lane-opus-sp r1 — spreading_decision port + TF analysis on

## What
- `celt_enc.rs`: port of libopus `spreading_decision` (tonal_average / hf_average / tapset state, `spread_weight` from dynalloc mask). Spread histogram now matches libopus on the 14-row sweep.
- `TF_ANALYSIS = true` (was false): libopus-style tf_analysis drives tf_res instead of the `tf_res = is_transient` default.

## Gate (14 rows, equal size, vs lanes/opus-sb-r1.sweep.txt)
- corr gap delta per row: |Δ| ≤ .0002 (sadie@64 +.0037 → +.0039; hein@96 −.0003 → −.0002); all others unchanged to 4 dp.
- err_ratio down where it mattered: sadie@64 .69 → .63, her@96 1.21 → 1.07.
- RATE GATE: all rows within ±5% (max −1.8%). DROPOUT GATE: 0 ours-only.
- Rows: lanes/opus-sp-r1.sweep.txt (EVIDENCE: scratchpad lanes/opus-sp-r2.gate14.log, 723 s).

## Suite
`cargo test -p ec-opus --release`: 34 + 27 passed, 9 ignored (sweep tests).

## Residue (unchanged)
- sadie@64 +.0039, hein@64 +.0022 corr behind libopus.
- Next divergence: intensity band (ours 8160 Hz vs libopus 6720 Hz); short-frame trim ours 0–2 vs libopus 0.
