# lane-av1-intrapred — r2: the chroma txb_skip CDF fork (intrabc TX4 chroma tx inheritance) and the intrabc chroma frame-copy prediction fork are FIXED; R5/W2 byte-exact

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

---

# lane-av1-intrapred r2 — the reported CDF fork is FIXED; the follow-up intrabc chroma prediction fork is also FIXED

Base 3f6748d5, same worktree/branch. The reported "chroma txb_skip CDF-content" fork was
neither the Chroma4 defaults (all four q tables match aom `token_cdfs.h` exactly) nor the
adaptation — it was the missing `INTRABC_CHROMA_TX` arming in `decode_leaf8`'s TX4 arm.

## Root cause (machine-checked against `~/.cache/aom-oracle` R5 traces)

* mi (36,66) is an **intrabc** 8x8 leaf whose var-tx tree split luma to 4x TX_4X4.
  libaom classifies intrabc as `is_inter_block` (`blockd.h:373`), so:
  * each luma TU codes `tx_type` off the **16-symbol inter set**
    (`av1_get_ext_tx_set_type(TX_4X4, inter=1)` = `EXT_TX_SET_ALL16`) — TU (0,0) coded
    `H_ADST(13)` in R5;
  * each chroma TU **inherits** the co-located luma type (`av1_get_tx_type`'s
    `is_inter_block` branch reads `tx_type_map`) → class `TX_CLASS_HORIZ` → the class-1
    `eob_flag_cdf16[1][1]` row, the HORIZ scan and the HORIZ nz-context offsets.
* `decode_leaf8`'s `resolved != 4` arm already armed `INTRABC_CHROMA_TX`; the
  `resolved == 4` (TX4) arm did not. Its chroma reads fell back to
  `default_intra_tx_type(DC_PRED)` = `DCT_DCT` → 2D eob row → at the U txb of (36,66)
  ours decoded `eob=11` where aomdec decoded `eob=13` (same entry rng 44076), and the
  tile desynced one read later than the all_zero ladder could see.
* The r5-era `tx_type` handling of the luma side was already correct (ours read the same
  16-symbol symbol and got `H_ADST`); only the chroma arming was missing.

## Fix (two hunks in `decode_leaf8`, decode.rs)

1. TX4 arm: capture the first TU's `tx_type` (`first_leaf_tx`, mirroring `decode_block`'s
   multi-TU capture) and arm `INTRABC_CHROMA_TX` with it before the group's chroma reads.
2. TX4 arm: disarm the slot after the two chroma reads — without this the slot leaked
   into the NEXT leaf (seen live: a regular-intra leaf at mi (54,24) took the previous
   intrabc leaf's `V_ADST` for its chroma eob row).

## Gates (all machine-captured, same fixtures as the stop report)

* R5 (`~/.cache/av1ibcvtx-tmp/R5.obu`): pre-fix first diff byte 82216 / 127862 differing
  bytes → post-fix first diff byte **231864** / 3305 differing bytes (luma byte-exact;
  aomdec and ffmpeg agree with each other, sha256 `c91229ee…` both).
* W2 (re-encoded at `--cq-level=8`, `/tmp/w2.obu` — the report's W2 was not in the cache):
  pre-fix first diff byte 352 (report said 353 ✓ reproduces) / 305032 diffs → post-fix
  231848 / 3254. 23x further into the stream, 94x fewer diffs.
* R9 (smptebars): still byte-exact vs ffmpeg post-fix (regression clean).
* Entropy: ALL 3321 `all_zero` reads now align 1:1 rng-for-rng against the instrumented
  aomdec (was 2162/4248).
* Pre-fix non-vacuity: scratch worktree at 3f6748d5 rebuild reproduces the byte-352 fork
  on W2 and byte-82216 on R5.

## Next fork — FIXED in the same pass (r2b): intrabc chroma frame-copy prediction

`R5` first differing byte 231864 = chroma U pixel **(184,4)**; 3305 differing bytes, all
chroma (U 1730 / V 1575), origin unit = the chroma 4x4 at cpx (184,4), i.e. the 8x8
**intrabc** leaf at luma (368,8), mi **(2,92)**, var-tx TX4 split. Entropy was fully
aligned (3321/3321 `all_zero` reads rng-identical, luma byte-exact), the DV matched
exactly (`EC_DV mi_row=2 mi_col=92 dv_col=-2560 dv_row=0`, same as aom's `mv0=(0,-2560)`,
source = integer-phase copy of U(24,4)), so the divergence was prediction-stage: the TX4
arm's chroma reads override the intra prediction through `palette_uv_bufs`, but only
`decode_block` wrapped that pair with the leaf's `intrabc_bufs` frame-copy buffers
(decode.rs:15833) — `decode_leaf8` wrapped only `palette_y_buf` (lane-t900 r32), so an
intrabc TX4 leaf's chroma predicted INTRA from the UV mode (intrabc's is DC) instead of
copying the frame at the DV.

**Fix (r2b, third hunk):** `decode_leaf8` wraps `palette_uv_bufs` with `intrabc_bufs`'s
`ub`/`vb` right after the `palette_y_buf` wrap — the chroma twin of decode.rs:15833.

**Gates after r2b (all machine-captured):**

* R5: **byte-exact vs aomdec AND ffmpeg** (the stop report's primary gate).
* W2 (cq8 re-encode): **byte-exact vs aomdec**.
* R9: byte-exact vs ffmpeg (regression clean).
* Cached-fixture sweep A1/A2/A3/D1/D2/D3: byte-exact vs their ffmpeg raws.
* Targeted tests (intrabc/lossless/screen filters): 14 passed, 0 failed. Full suite on
  the r2 fix alone: 607 passed / 0 failed (exit 0).
