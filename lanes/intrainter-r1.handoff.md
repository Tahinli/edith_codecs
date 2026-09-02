# lane-intrainter r1 handoff

Branch `lane-intrainter`, based on main `c729a38` (worktree was already at main's head; no rebase needed).
Code commit: `955ecf9` (`feat(av1): intra blocks in inter frames may split their luma transform`).

## Implemented (all in `crates/ec-av1/src/decode.rs`)

- `read_block_tx_size` (~9470): intra branch no longer refuses a split `tx_depth`. It calls
  `set_txfm_ctxs` with the RESOLVED transform (so later blocks' txfm ctx is right) and returns
  the transform-unit leaf list `(row_mi, col_mi, tx_px)` in raster order — the same shape the
  inter var-tx tree returns.
- `decode_inter_block` intra arm (~15490): the leaves ride the existing `vartx_leaves` slot; a
  per-TU loop predicts + codes each unit with `txbset_for(tx_px, reduced)` (INTRA tx set),
  `default_scan(tx_px)`, `Reach::of_tu` (block-relative availability), `luma_skip_ctx`
  (neighbour-magnitude txb_skip ctx, not the lone-TU 0), `around_mi`, `record_mi_luma`.
  Skip blocks predict PER UNIT too (an intra block reads tx_depth even when skip).
  Chroma is unchanged (its transform is not split). The function tail already ran
  `record_split_luma` + per-TU deblock edges whenever `vartx_leaves.is_some()`.
- Inter-frame intra syntax differences all pre-existed in that arm and are unchanged:
  `y_mode_cdf[size_group]` (not kf_y_mode), `uv_mode_cfl[mode]`, angle deltas, CfL alphas,
  palette-mode probes; angle_delta_y is refused nonzero, filter intra is not read in this arm.
- Counter `INTRA_IN_INTER_SPLIT_TX_HITS` + `intra_in_inter_split_tx_hits() -> [usize;4]`
  bucketed by block side 8/16/32/64, one increment per split block.

## Sizes / refusals

- Squares 16x16..64x64 through `decode_inter_block` are lifted (the code is size-generic).
  OBSERVED FIRING: 16x16 only (12 blocks 8-bit, 1 block 10-bit). 32/64 buckets stayed 0 under
  `--min/max-partition-size=16/32`; unproven above 16x16.
- NEW refusal (`decode_inter_block8`, ~17070, listed in refusal_inventory.rs:63 replacing the
  lifted string): "an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4
  transform units" — that leaf path has no per-TU intra prediction loop.
- RECT intra strips in inter frames stay refused earlier ("an intra-coded HORZ/VERT strip needs
  rectangular intra prediction this decoder does not code yet"), so no rect work was needed.

## Gate state — PASSING, both depths

`a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact` and
`a_real_aomenc_10bit_...` (`intra_in_inter_split_tx_gate` in stream.rs).
Real aomenc, 64x64, 8 frames, mandelbrot zoom with a HARD CUT at frame 4
(`overlay=enable='gte(n,4)'`), `--kf-min-dist=1000 --kf-max-dist=1000` (stops the scene-cut
detector inserting a second key frame), cq 30, overrides appended LAST. All frames Y/U/V vs
ffmpeg; decode errors only continue if they contain "unsupported"; exhausting 40 attempts panics.

Run:
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrainter cargo test -p ec-av1 --lib -- split_transform_intra_block --nocapture`

Result: 8-bit seed 45 `16x16=12`, 10-bit seed 72 `16x16=1`, 0 differing samples, both `ok`.
No entropy ladder was needed — no divergence was ever observed.

## Suite

`$HOME/.cache/intrainter-suite.log` (unit `intrainter-suite-1788315424.service`). At turn cap it
was still running (~250/372 tests, no FAILED line so far, currently inside the aomenc gates).
(measured after the turn cap; nothing further owed on the suite.)


## Film

Both `.../b6d8a07f.../scratchpad/troy-head.obu` and `.../cdc46329.../scratchpad/troy5.obu` stop at
"a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here" (lane-rectchroma
territory), NOT at this lane's refusal — the wall string in the charter was not reproducible on
these extracts (matches an existing ledger dead-end line).

## Exact next step

1. Read the suite result line; if green, merge.
2. Optional follow-up round: prove 32x32/64x64 split intra-in-inter blocks (raise
   `--max-partition-size` to 64 and lower cq, assert the 32/64 buckets), and lift the 8x8 leaf's
   TX_4X4 2x2 grid in `decode_inter_block8`.
