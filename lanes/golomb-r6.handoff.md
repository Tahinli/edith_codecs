# lane-golomb r6 HANDOFF

## Merge state (STEP 1: DONE)
* `899d7cf` = `git merge main` (main `18bf7dc`). Real merge commit, not a squash.
* `crates/ec-av1/src/mvstack.rs`: `git diff main -- mvstack.rs` is EMPTY -> ONE copy of the
  compound tpl-extension hunk (lane-sb128 `d6cca7d` == our `13aebbe`), byte-identical to main.
* `crates/ec-av1/src/cdf.rs` and `cdf_state.rs`: `git diff main` EMPTY (NZ_MAP/FILTER_INTRA
  byte-identical to main).
* 7 `decode.rs` conflicts resolved: 4 were comment-only (took main's text); 3 were the counter
  pair -- main's `bump_edge_part` and this lane's `bump_edge32` are INDEPENDENT counters, BOTH
  kept at every site (`decode.rs:13094-13101`, `13169-13179`, and the two `!has_rows32/!has_cols32`
  guards).
* `#[ignore = "open r2 defect: ..."]` on
  `a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact`
  (`stream.rs:18414`) SURVIVED the merge -- verified by grep after the merge commit.

## Suite r5
`$HOME/.cache/golomb-suite-r5.log` had NO `test result:` line: the unit was still `active` and
I stopped it (`systemctl --user stop golomb-suite-r5`) to free `CARGO_TARGET_DIR` for r6 -- it
was building the PRE-merge tree, so its result would have been superseded anyway. Every `test`
line present in it up to that point was `ok`. r6 suite armed as unit `golomb-suite-r6`
-> `$HOME/.cache/golomb-suite-r6.log`; READ THAT, not r5.

## STEP 2: chroma residue -- ROOT CAUSE #1 FOUND AND FIXED

Stream: `g35.obu`, md5 `9037f5b21db95d35e71f91f040fc33e1` (regenerated, hash confirmed;
recipe `scratchpad/gl-gen.sh 35 <out>`), 192x80 cq35 5 frames 8-bit, 6 decode-order pictures.

### Localization method (reusable)
Three-stage compare, decode order, ours vs the oracle:
`EC_AV1_PREFILT_DUMP` (pre-loopfilter) / `EC_AV1_POSTDEBLOCK_DUMP` (post-deblock, pre-CDEF) /
`EC_AV1_FINAL_DUMP` (post everything). Ours dumps a PADDED plane (192x96 luma), aomdec dumps
CROPPED (192x80) -- the comparator must use different strides per side.
NOTE: `EC_AV1_PREFILT_DUMP16`/`..._DUMP16` SEGFAULT the oracle aomdec on an 8-bit stream
(`ec_dump16` does `CONVERT_TO_SHORTPTR` unconditionally, decodeframe.c:3968). Use the 8-bit
`*_DUMP` vars for 8-bit streams.
Ours' `dump_stage` used to write `.f0` for EVERY frame (overwrite); r6 gave it a per-var
thread-local index (`decode.rs`, `dump_stage`) so POSTDEBLOCK is now `.f0..fN` like the oracle.

### The measurement that localized it
Before the fix: prefilt decode-f1 EXACT, post-deblock decode-f1 U342/V380 wrong, luma exact at
every stage -> the defect was the DEBLOCK filter on chroma, not prediction, not CDEF
(`--enable-cdef=0` did NOT fix it), not LR (disabled in the recipe).

### Root cause (fixed)
`decode.rs`, inter block tail: the var-tx / split-tx LEAF loop called
`Neighbours::fill_lf_grid_rect(leaf_mi, tx_px/MI, tx_px/MI, ...)`, and that function DERIVES
the chroma grids from the span it is given (`uv_tx_w = (w_mi*MI/2).clamp(4,32)`). So every
var-tx leaf OVERWROTE the block's correct `uv_tx_grid`/`uv_tx_h_grid` (the block's
`av1_get_max_uv_txsize`) with `leaf_tx/2`, inventing chroma transform edges the deblocker then
filtered. libaom `get_transform_size` (`av1/common/av1_loopfilter.c:207`) applies the var-tx
override ONLY `if ((plane == AOM_PLANE_Y) && is_inter_block(mbmi) && !mbmi->skip_txfm)`;
chroma always keeps `av1_get_max_uv_txsize(mbmi->bsize, ss_x, ss_y)`.
FIX: new `Neighbours::fill_lf_grid_leaf_luma` writes ONLY `tx_grid`/`tx_h_grid` over the leaf
span (`ref_grid`/`delta_lf_grid` are already correct from the block's own fill over the same
span), and the leaf loop calls it. Sweep: this was the ONLY `fill_lf_grid_rect` caller passing
a TU span; all 12 others pass block dims (checked each).

### Result
g35 ours vs aomdec, all three stages, decode order:
before  f0 exact, f1 U398/V457, f2 U325/V412, f3 U336/V408, f4 U~1000/V~1000, f5 U691/V746
after   f0-f3 EXACT at prefilt+postdeblock+final, f4 U974/V973 (prefilt), f5 inherits it.

## RESIDUE (still RED) -- exactly one defect left
Gate: `cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition`
=> `192x80 cq35 frames=5 10bit=false frame 3 plane U: 978 pixels differ, first at row 0 col 32
(ours 33 vs ffmpeg 8) [edge32=[0,36,0,0,0,18,0,0]]`.

* Stage: **PREFILTER** (prediction+residual), decode-order picture 4 only (f5 inherits).
  Deblock/CDEF/LR are exonerated for this one.
* Extent: EXACTLY chroma x 32..63, y 0..31 = luma x 64..127, y 0..63, i.e. one whole 64x64
  superblock. Luma bit-exact inside it.
* Shape of the error: HIGH-FREQUENCY, |delta| up to 52. aomdec's row 0 is smooth
  (13,14,14,14,...) where ours is (36,12,0,0,20,22,...). That is a RESIDUAL/inverse-transform
  shape, not a prediction-offset shape.
* Blocks in that SB (decode f4, from `EC_TRACE_MODE`): mi(0,16) mode=23 ref0=1 ref1=7 (compound),
  mi(0,24) mode=17 ref0=1 ref1=7, mi(8,16) mode=17 ref0=1 ref1=7, mi(8,24) mode=13 ref0=7
  single-ref. All 32x32.

### Ruled out (each with the command that ruled it out)
* deblock / CDEF / LR: prefilt already differs on f4.
* per-plane `delta_q_u/v_dc/ac`: consumed (`stream.rs:599-603` etc.) AND f0-f3 chroma is now
  bit-exact under the same quantizer.
* narrow-block (<=4-wide) chroma MC kernel and sub8x8 chroma MV derivation: impossible in this
  stream, `--min-partition-size=32` means the smallest chroma block is 16x16.
* chroma MC rounding: derived by hand that our always-2-pass convolve is bit-identical to
  libaom's `av1_convolve_x_sr_c`/`_y_sr_c` single-pass rounding when one fraction is 0
  (identity tap 128, round2(x*16,11) == ROUND_POWER_OF_TWO(x,7)); and f1-f3 are now exact.
* **DIFFWTD compound (the tempting one -- DO NOT re-chase it blind):** `--enable-diff-wtd-comp=0`
  made the whole sequence exact, BUT that is a DIFFERENT bitstream (class gate-recipe-confound).
  An `EC_DWTD` trace of the ORIGINAL g35 shows only 4 diffwtd blocks in the whole stream, in
  decode f1 (px=64,py=0) and decode f2 (px=0,py=0 / px=64,py=0 / px=128,py=32) -- **NONE in
  decode f4**, where the residue is. DIFFWTD is therefore EXONERATED for this residue.
* warp / global motion / OBMC / dual filter / masked-comp / interintra / dist-wtd: ablated on
  the post-fix build, all still show the f3(display) failure (`scratchpad/abl.sh`).

### Exact next step
The error is a chroma RESIDUAL in a 64x64 SB of decode-order picture 4 whose luma is exact and
whose entropy is (r5-proven) in sync. Bisect chroma coefficients, not prediction:
1. Run the oracle with `EC_TRACE_COEFF=1` and ours with `EC_TRACE_COEFF=1` on `g35.obu`, isolate
   decode picture 4, block mi(0,16), plane 1. Compare the DEQUANTIZED values element by element
   (and the msac RANGE per element, never `tell()`).
2. Prime suspect given "luma exact, chroma noisy, whole SB": the chroma `tx_type` inherited from
   the colocated luma unit (`decode.rs` `inherited_luma_tx_type`, `av1_get_tx_type`
   `blockd.h:1291`). For a var-tx-split luma block the chroma TU spans several luma TUs and
   libaom reads the luma tx_type at `blk_row<<ssy, blk_col<<ssx`; ours takes "the top-left unit".
   Verify that mapping against `av1_get_tx_type` for a 32x32 block whose luma split to 16x16.
   A wrong tx_type gives exactly this signature: right coefficients, wrong basis, chroma only.
3. Second suspect: `txsize_sqr_up_map[tx_size] > TX_32X32 -> DCT_DCT` clamp
   (ours clamps at `side >= 32`, on the CHROMA side length -- check that against
   `av1_get_ext_tx_set_type`'s use of the chroma tx size).

## Deferred
* the 192x68 / 68x192 gate arm from r3's verifier ask: **deferred -- the gate it would join is
  still RED, a new arm would only add noise -- unblocked by the residue above.** The r3 code ask
  itself stays accepted-as-already-satisfied (`decode.rs` `read_var_tx_size`'s
  `blk_row >= max_h_mi || blk_col >= max_w_mi` early return, both inter callers pass
  `mi_cols/rows - at_mi`).
