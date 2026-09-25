# lane-av1-422filter — the 4:2:2 loop-filter chroma class: NAMED and FIXED

## Outcome

1. REMEASURED first (pinned fixture `/tmp/t422.obu` sha256
   `d3fa4beeb1309033e4a03f5ae5c431c8fa8386ba1ada088684710423c7938129`,
   single-frame cut `/tmp/t422f0.obu` sha256
   `3dd3f768ef553c02316848c2d3184c3765030c0a6fcb95887e4719add9ded3af`,
   256x144 / chroma 128x144, local `EC_AV1_ALLOW_422_PROBE` bypass applied
   and reverted byte-identical after): with `EC_AV1_PREFILT_DUMP` /
   `EC_AV1_POSTDEBLOCK_DUMP` / `EC_AV1_POSTCDEF_DUMP` / `EC_AV1_FINAL_DUMP`
   on both sides, the frame-0 pre-filter AND post-deblock planes are
   byte-exact (deblock did identical work both sides: 797/637/727 samples
   changed, same set) — the 1236/1263 final chroma diffs are introduced
   AFTER deblock. Attribution ladder, ours vs the instrumented aomdec:

   | stage                | Y          | U               | V               |
   |----------------------|------------|-----------------|-----------------|
   | pre-filter           | 0          | 0               | 0               |
   | post-deblock         | 0          | 0               | 0               |
   | post-CDEF            | 0          | **251** (50,0)  | **259** (48,0)  |
   | final                | 0          | **1236→253**    | **1263→266**    |

   (the pre-fix row shows the parent's numbers; the ticket's first final
   sample V (48,0) is literally the first post-CDEF defect sample.)
2. The oracle had NO 8-bit post-CDEF rung (its `EC_AV1_POSTCDEF_DUMP` was
   16-bit only; the earlier `aom/pcd.f0` measurement was actually its
   post-deblock output — the rung had to be re-derived to isolate CDEF from
   LR). Added an 8-bit `EC_AV1_POSTCDEF_DUMP` twin to the oracle's
   `decodeframe.c` (right after the `do_cdef` block, full aligned buffer,
   same shape as the POSTDEBLOCK rung) and rebuilt
   `$HOME/.cache/aom-oracle/build` aomdec with ninja. The CDEF verdict was
   then direct: 251/259 samples, first exactly U (50,0) / V (48,0).
3. DEFECT 1 (the big one) — **the 4:2:2 LR boundary-band snapshot is taken
   on a 4:2:0 stripe grid**. `plane_snapshot` copies only the 4-row bands
   LR's stripe substitution reads (`stripe_h - offset` per 64 rows); the
   chroma calls hardcoded `ss_y = 1`, so for 4:2:2 (chroma stripes on the
   luma 64-row grid, offset 8) the bands landed at rows 26/60/92/124 while
   LR read rows 54..57/118..121 — every boundary substitution read
   uninitialised scratch (zeros): 999 U / 963 V samples filtered where aom
   filters nothing, |delta| up to 24 at the top rows. Fixed at all three
   sites of the class: the keyframe and inter non-pipelined
   `plane_snapshot(&u/&v, ss_y(fctx))`, the pipeline's `snap_band` band rows
   `(j*64) >> ss_y`, and `pipe_run_lr`'s chroma `dims` (plane extents
   `round_ss`, ss carried, not `div_ceil(2)`+1). 4:2:0 is bit-identical
   (ss_y=1 reproduces the old bands/rows/dims exactly).
4. DEFECT 2 — **the CDEF chroma direction remap was missing**. libaom
   `av1_cdef_filter_fb` (cdef_block.c) remaps the luma direction through
   `conv422 = [7,0,2,4,5,6,6,6]` (in place; V then inherits U's remapped
   grid) whenever `xdec != ydec` — the direction grid lives on the
   subsampled chroma grid. The port filtered chroma with the raw luma
   direction: after the LR fix the residual was exactly the CDEF stage,
   251/259 samples all ±1/±2 (168 we-filtered-aom-doesn't, 71
   aom-filters-we-don't, 12 value diffs on shared samples; scalar and AVX2
   kernels byte-identical, so shared logic, not the SIMD path). Fixed with
   `CDEF_DIR_CONV422`/`CDEF_DIR_CONV440` applied for `(ss_x, ss_y)` =
   (1,0)/(0,1); 4:2:0 and 4:4:4 take `_ => dir` unchanged.
5. MEASURED after both fixes: **frame 0 of the pinned 4:2:2 fixture is
   byte-exact vs the oracle at PREFILT, POSTDEBLOCK, POSTCDEF and FINAL on
   Y, U and V** (0 diffs everywhere). Non-vacuity on this frame:
   `chroma422_square` = 24, `chroma422_rect` = 96, `chroma422_sub8` = 26 —
   the touched 422 arms provably ran. Instrument for the LR half: a python
   re-computation of the stripe walk + `lr_src_row` substitution + the
   port's own wiener rounding reproduced the oracle's post-LR chroma plane
   EXACTLY from the oracle's post-CDEF input (0 mismatches), which pinned
   the residual defect to the snapshot input, not the filter math.
6. REMAINING: none for this class. The `read_coeffs_rect` frame-9 scratch
   panic (inter frames of the 22-frame stream) is untouched, as chartered.

## Decision

- The header refusal STAYS unconditional in `stream.rs` — the probe bypass
  was local-only, applied and reverted byte-identical before the commit
  (`git diff` after the revert carries only the decode.rs fix); the
  refusal-by-name gate re-verified firing.
- `read_coeffs_rect` untouched. The 4:2:0 group-tail SKIP arm untouched.
  `sub8_leaf_chroma422` (parent lane's three fixes) untouched.
- The 8-bit `EC_AV1_POSTCDEF_DUMP` rung was added to the ORACLE's source
  tree (`$HOME/.cache/aom-oracle/src/av1/decoder/decodeframe.c`, rebuilt
  with ninja) and transcribed into
  `scripts/instrument-aom-oracle.sh` as rung 15 — its generator block
  reproduces the hand patch byte-for-byte (verified by strip-and-replay on
  the live tree; the marker `EC_INSTRUMENTED_POSTCDEF` keeps re-runs
  no-op), so `build-aom-oracle.sh` + the script rebuild the full ladder
  from scratch again.

## Verification

- `cargo check -p ec-av1` (target `~/.cache/cargo-target-av1422filter`):
  0 warnings, 0 errors.
- Frame-0 stage dumps, ours vs the instrumented aomdec (cropped
  comparisons; our stage dumps are 256x160/128x160 padded, the oracle's
  cropped 256x144/128x144 — the comparator crops): all four stages, all
  three planes, 0 diffs.
- 420 gates green (all with the fix in place):
  `stream::tests::a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_
  whose_tx_split_decodes_pixel_exact` under `EC_AV1_REQUIRE_AOMENC=1`
  (1 passed), `stream::tests::a_real_aomenc_stream_with_restoration_reads_
  lr_symbols_correctly` (1 passed — the LR-snapshot regression gate),
  `a_superres_key_frame_with_cdef_and_loop_restoration_decodes_pixel_exact`
  (1 passed), `a_non_420_subsampled_sequence_header_is_refused_by_name`
  (1 passed).
- Scalar-vs-AVX2 CDEF chroma A/B on this frame: 0 diffs between kernels
  (the 4x8 chroma shape was already SIMD-faithful; the defect was the
  direction input).

## State

- Commit on lane-av1-422filter (no push): the LR snapshot ss_y fix (three
  sites) + the CDEF conv422 direction remap + this report.
- The instrumented oracle build at `$HOME/.cache/aom-oracle/build` now
  carries the 8-bit `EC_AV1_POSTCDEF_DUMP` rung for future lanes.
