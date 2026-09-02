# lane-intersub8 r3 — HANDOFF (4x8 gated GREEN, 8x4 implemented but refused)

## What is wired (decode.rs `decode_inter_sub8_rect2`, ~line 19100)
One function serves both shapes (`vert: bool`); the call site
(`part8 == PARTITION_VERT` arm of the sub-16 SPLIT leaf loop, ~decode.rs:22630) currently
only reaches it with `vert = true`.
* mode info per sub-block in `decodemv.c` `read_inter_frame_mode_info` order: segment_id,
  skip, cdef/dq/dlf, intra_inter, single ref only (`is_comp_ref_allowed(BLOCK_8X4)` false ->
  no skip_mode, no comp_mode), no interintra / motion_mode / OBMC / warp (all need min dim 8),
  `read_mb_interp_filter` DOES run.
* tx: `block_signals_txsize(BLOCK_8X4)` true (blockd.h:1027) -> ONE `txfm_partition` symbol
  under TX_MODE_SELECT at `max_txsize_rect_lookup[BLOCK_8X4] = TX_8X4`, through r14's general
  `read_var_tx_size` with max `(bw,bh)` and `blk_max_px = 8`; `sub_tx_size_map[TX_8X4]=TX_4X4`
  so a split resolves to two TX_4X4 units (libaom's early return). Skip -> `set_txfm_ctxs`
  skip_inter arm.
* luma sets NEW in cdf_state.rs: `TxbSet::LumaRect8x4Inter` / `LumaRect8x4InterSet1` =
  `LumaRect4x8`'s tables (8x8 coefficient class, 32-position eob_pt) with `inter_tx_type_4` /
  `inter_tx_type_4_set1` (EXT_TX_SET_ALL16 at the TX_4X4 inter row). Split TX_4X4 units read
  `Luma4Inter{,Set1}`. Whole-leaf unit passes `luma_skip_ctx: None` (it IS the plane block,
  `get_txb_ctx_general` fixes 0); split units pass `luma_skip_ctx_rect`.
* chroma once, on the SECOND sub-block (`is_chroma_reference` under 4:2:0: bottom 8x4 /
  right 4x8), as one TX_4X4 per plane (`TxbSet::Chroma4`), inheriting the FIRST luma TU's
  tx_type; prediction is the sub8x8 one (`reconinter.c build_inter_predictors_sub8x8`,
  `row_start = -(block_size_high==4 && ss_y)`): two 4x2 (HORZ) / 2x4 (VERT) pieces, each from
  its own sub-block's ref/mv/filters.
* neighbour writes per mi: `grid_stamp_rect` (size=w_mi, size_h=h_mi over the true span),
  `record_mode_mi`, `record_inter_rect_mi`, `fill_skip_grid_rect`, `fill_lf_grid_rect`,
  `record_mi_luma_rect` per TU; partition tail `above_side_mi = bw`, `left_side_mi = bh`.

## The 8x4 defect (refused by name, NOT fixed)
Stream: `~/.cache/intersub8-tmp/gen_t.sh 14 3 8 d1.obu --enable-rect-partitions=1`
(transposed-structure geq source: `128+58*sin((Y+N*3)/6)+18*sin(X/23)`, 192x128, 6 frames).
Decode-order frame 5 mismatches; frames 0..4 exact. 4 HORZ groups, all at mi rows 30/31,
cols 4 and 6.
Range ladder (`EC_TRACE_PART=1` ours, new gated print at each partition read, vs aomdec
`EC_TRACE=1` `EC_PART`; and `EC_TRACE_MODE` `EC_MODE`; and `EC_TRACE_COEFF` `tag=all_zero`):
* every element matches through the 16x16 partition read at mi(28,8) (ctx 2 == oracle's 6,
  entry rng 37489 both) and through `EC_MODE mi_row=30 mi_col=8 rng=46505` (both);
* that slot is PARTITION_HORZ -> two BLOCK_16X8 at (28,8) and (30,8);
* block (30,8)'s LEFT neighbour is our HORZ 8x4 group at (30,6);
* first divergence is INSIDE (30,8)'s mode info, AFTER its `EC_MODE` print (so skip and
  intra_inter are still right) and BEFORE its first coefficient TU: our TU entry rng 54923 vs
  oracle 61760 (`EC_COEFF plane=0 tx_size=8` = TX_16X8, all_zero=0 both). all_zero #468 of
  470/473.
=> some per-ROW neighbour band our HORZ group leaves at rows 30/31 is wrong. Remaining
suspects, in read order after the print: `left_ref[30]`, the mv stack (MiGrid cells (30,7)/
(31,7)), `left_filter[30]`, `left_txfm[30]`. All four were transcribed against libaom and
looked right on inspection -- not bisected. NOTE ledger: this oracle's
`EC_AV1_PREFILT_DUMP` is already post-deblock, so pixel-stage bisection is confounded; use
the range ladder only.

## Provable vs not
* PROVABLE (gated): two-BLOCK_4X8 (PARTITION_VERT) inter leaves. Sweep
  `~/.cache/intersub8-tmp/sweep2.sh` (28 arms, cq {8,12,14,16,20,26,32} x sp {3,6,9,12} x
  8/10-bit, original source, rect on): every arm with `vert4x8 > 0` outside the known-bad
  cq8 band decoded pixel-exact; the two cq8 mismatches are r2's CDEF |d|=1 defect.
* NOT PROVABLE: 8x4 (HORZ). `sweep3.sh` (transposed source): 4 of 28 arms mismatch with
  `horz8x4 > 0` at cq 12/14/16/32, i.e. outside the CDEF band. Refused by name.

## Gate
`cargo test -p ec-av1 --lib sub8x8_inter_split` -> ok, 1 passed / 0 failed (414 filtered),
28.94s. The test now runs 16 attempts per depth: 0..7 `--enable-rect-partitions=0` (SPLIT),
8..15 `=1` (VERT). Asserts `fired >= 2`, NEW `rect_fired >= 2`, `out_of_scope_mismatch == 0`,
every decode-order frame Y/U/V. `refusal_inventory` 3/3 ok, `gate_coverage` 9/9 ok.

## Not done
* Full `cargo test -p ec-av1 --lib` suite: NOT RUN this round (turn cap). r2's unit
  `intersub8-suite-1788328909` was still running on a stale binary at round start and is now
  gone from the unit list; `$HOME/.cache/intersub8-suite-r2.log` has 218 ok / 0 FAILED and no
  `test result:` line. Start r3's suite per COMMON's SUITE RUNS recipe, log
  `$HOME/.cache/intersub8-suite-r3.log`.
* HG film probe: not run.
* `--tile-columns=1` gate arm: still owed (no NEW per-mi band was added, all reused).

## Exact next step
1. Start the r3 suite. 2. Bisect the 8x4 defect on `d1.obu` frame 5 block (30,8) by dumping,
one at a time, the four left-band values our HORZ group writes at rows 30/31 and comparing
against an instrumented aomdec print of `xd->left_mbmi`/`left_txfm_context[0]` at that block.
3. Lift the HORZ refusal with a `sweep3.sh`-based arm once green.
