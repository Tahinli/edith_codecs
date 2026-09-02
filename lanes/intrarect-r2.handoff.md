# lane-intrarect r2 handoff — DEFECT CLOSED, one item left open

State at `f2efbcd` (branch `lane-intrarect`, worktree clean). r1's pixel defect is FIXED and
both gates are GREEN; this handoff exists only because the full-suite run had not finished
when the turn cap hit. Full detail: `lanes/intrarect-r2.report.md`.

## Which stage differed (charter STEP 1, answered)

PREFILT (pre-deblock RECON), not deblock, not CDEF/LR. 8-bit seed 69 frame 5: first wrong
luma sample (80,64), 512 luma samples = exactly the 16x32 strip at mi(16,20) + 120 V samples.
Frames 0..4, 6, 7 byte-identical. So the loop filter, the lf grid and `record_inter_rect`
were all EXONERATED and the charter's deblock branch is a dead end.

## Root cause (found, fixed, committed)

`decode_rect_split`'s rect-TU arm read `txb_skip` from `luma_skip_ctx_rect` even when the
transform covers the whole block. libaom `get_txb_ctx` gives luma a flat ctx **0** when
`plane_bsize == txsize_to_bsize[tx_size]`. Ours read ctx 5, aomdec ctx 0 -- same decoded
value, different CDF, msac range diverging at the block's first coefficient symbol
(ours 58117, aomdec 45075, identical ENTRY range 60236). Fix: `decode.rs:5009`, ctx 0 when
`tx_w == bw && tx_h == bh`. After it, all 8 frames of the seed-69 stream are byte-identical.

## Ruled out (do not re-investigate)

- Deblock / lf grid / `record_inter_rect` / delta_lf: the mismatch is already in PREFILT.
- Prediction and edge availability: the strip's DC prediction was correct (aomdec's own
  edges average 96.9 -> 97, and after the ctx fix the block is exact with no prediction change).
- Mode info (skip / is_inter / y_mode size_group 2 / uv_mode cfl-allowed / tx depth): the
  coefficient read's ENTRY range matched aomdec exactly (60236), so every mode symbol before
  it was right.
- Every other `luma_skip_ctx*` call site (`decode.rs:5054`, 7947, 8301, 8829, 9648, 14534,
  15368) -- none can reach the unit == block case; the sweep is in the report.
- 10-bit was NOT separately bisected: the 10-bit gate seed 78/84 now passes pixel-exact,
  so there is no 10-bit residue to measure.

## Gates + suite

`cargo test -p ec-av1 --lib intra_rect_strip` -> `2 passed; 0 failed` (12.20s),
`$HOME/.cache/intrarect-gate-r2.log`, both depths, 45 attempts each, all pixel-compared.
Negative control for r1's `reject_residual` move: 8 attempts in the same sweep still refuse
"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding".

FULL SUITE: FINISHED GREEN after the cap -- `test result: ok. 337 passed; 0 failed; 29
ignored; 0 measured; 0 filtered out; finished in 257.15s`
(`$HOME/.cache/intrarect-suite-r2.log`).

## Exact next step for r3 (one item)

The only open item is the MERGE, not a defect: this branch is **not rebased onto main**.
   main now carries lane-fiinter (square intra-in-inter reads `use_filter_intra`) and
   lane-rect1d; `decode_intra_rect_in_inter` reads no such symbol, so on the merged tree the
   rect arm needs the same read wired (this lane's gate spells `--enable-filter-intra=0`, so
   its own streams never carry it and the gate cannot catch a missing read).
