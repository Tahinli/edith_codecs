# lane-av1-intrapred — stop-report: first divergence is a chroma txb_skip CDF-content divergence at mi (36,66) plane 2, not intra prediction

Base 7f863c5f (lane-av1-ibcvtx), worktree edith_codecs-av1intra, branch lane-av1-intrapred.
Tree left CLEAN (all temporary probes reverted; decode.rs/tile.rs byte-identical to base). No push.

## Reproduction (step 1 — done)

* R5 (testsrc2 320x180 tiled 2x2 → 640x360, sb64, maxp64, cq20, square-only, screen, intrabc, 1 frame)
  reuses `~/.cache/av1ibcvtx-tmp/R5.obu`. Our decode (decode_probe) vs aomdec/ffmpeg:
  first diff byte 82217 = luma (x=296, y=128), 189 vs 170; 84510 luma + 21005 U + 22347 V diffs total.
* `EC_AV1_PREFILT_DUMP` (ours, cropped, vs oracle): identical diff positions ⇒ the filter chain
  (deblock/cdef/lr) is NOT the defect.
* Fresh witness: W2 = same recipe at `--cq-level=8`: ours vs aomdec diverge at byte 353 = pixel
  (353, 0) — top row, near frame start. Much earlier, much tighter witness for this class.
* R9 (smptebars same recipe): byte-exact pre-fix, consistent with coarse-q smooth content
  (DC-only residuals hiding the defect).

## Localization (step 2 — done, decisive)

Per-symbol trace diff (ours `EC_TRACE_COEFF` ↔ instrumented aomdec, aligned on the per-txb
`all_zero` reads; one per txb on both sides, 1:1 through the whole aligned prefix):

* Symbols, order, rngs and txb shapes match **1:1 through txb index 2161** (including every
  partition read, mode read and coefficient read — the "isolated sample" fingerprint of the
  ibcvtx report is downstream noise).
* **txb 2162 = mi (36,66), plane 2 (V chroma), TX_4X4, the txb_skip (all_zero) symbol**:
  both sides decode the same value (`all_zero=0`, "has coefficients") but consume different
  bits — aom 44076→55196, ours 44076→42907. Same entry state, same symbol, different
  probability ⇒ **the CDF row content differs at that moment**.
* Measured content: aom `txb_skip_cdf[txs=4x4][8][0] = 8480`; ours `TxbSet::Chroma4` row 1
  `cdf[0] = 24154`. No aom chroma-4x4 row (18102 / 8614 / 9170 / 16384) matches ours.
* The context VALUE mapping is verified correct at every chroma txb through 2162
  (aom ctx = ours row + 7 everywhere; aom above0=15/left=0 ↔ ours (1,0) at this unit),
  so the bug is not the around-flag arithmetic — it is the **adaptation history (or default)
  of our Chroma4 txb_skip rows** diverging from aom's shared `txb_skip_cdf[4x4][7..10]` rows.
* Everything after 2162 — the whole "cascade" (rows 128+, the +19 at (296,128) whose block
  even reads a different var-tx tree, the chroma 4x4 cell diffs at rows 4..9, and the ibcvtx
  lane's "mi (32,32) 64x64 all_zero" fingerprint, whose block is actually a 32x32 PAETH at
  mi (32,72) that aom codes as one all-zero txb) — is **post-desync noise**, not defects.

## Why this is a stop for this lane

The defect is inside the intra block's **entropy layer** (chroma txb_skip CDF contents for
`TxbSet::Chroma4`), not in intra prediction arithmetic, mode reads, or the filter chain.
Prediction at every pre-desync site is pixel-exact (DC 170 / DC 103 cells verified against
aom recon with identical inputs). Fixing it is a cdf.rs/decode.rs chroma-CDF-set task
(defaults + update path for `TxbSet::Chroma4` rows vs aom's `default_txb_skip_cdf[0][7..10]`),
which I could not complete within this run's budget.

## Unblock (exact, machine-checked)

1. Diff the DEFAULT of our `TxbSet::Chroma4` txb_skip rows against aom's
   `default_txb_skip_cdf[0][7..10]` (aom cdf.c) — aom's first chroma-4x4 ctx=8 read shows
   cdf0=15696 post-adapt.
2. Diff the adaptation sequence: instrument `read_coeffs`' `all_zero` read (chroma only) to
   log row index + content per read (the `tmp_chroma` plumbing I used and reverted is the
   template), and compare against the oracle's `tag=all_zero ... cdf0=` lines
   (the oracle build at `~/.cache/aom-oracle/build` now prints `cdf0=` and `EC_ROWS8` per
   chroma-4x4 read — reusable).
3. R5 must become byte-exact vs aomdec AND ffmpeg; W2 (cq8) is the sharper gate
   (diverges at byte 353 pre-fix); R9 must stay exact.

## Notes

* A parity-flip experiment on the default-scan zigzag was performed and fully reverted; it
  proved our scan tables are entropy-correct as-is (the flip desyncs at the first
  multi-coeff txb). Scan tables are NOT the defect.
* The oracle build `~/.cache/aom-oracle` gained extra env-gated probes during this hunt
  (EC_R5CELL, EC_ROWS8, `cdf0=` in the all_zero line, EC_DBGCTX retargeted to mi 36,66).
  They are env-gated and outside the repo; retarget/keep as needed.
