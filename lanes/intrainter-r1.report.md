# lane-intrainter r1 — intra blocks in inter frames with a split luma transform

Branch `lane-intrainter` off main `c729a38` (worktree already at main's head; no rebase needed).
Commit: `955ecf9`.

## What changed

- `crates/ec-av1/src/decode.rs` `read_block_tx_size` (~9470): the intra branch no longer
  refuses a `tx_depth` that resolves below the block side. It runs `set_txfm_ctxs` with the
  resolved transform (so later blocks' txfm ctx is right) and returns the transform-unit leaf
  list in the same `(row_mi, col_mi, tx_px)` shape the inter var-tx tree uses, raster order.
- `crates/ec-av1/src/decode.rs` `decode_inter_block` intra arm (~15490): the returned leaves ride
  the existing `vartx_leaves` slot, and a per-TU loop predicts + codes each unit
  (`txbset_for(tx_px, reduced)`, `default_scan(tx_px)`, `Reach::of_tu`, `luma_skip_ctx`,
  `around_mi`, `record_mi_luma`) exactly as `decode_block`'s key-frame multi-TU branch does —
  including the skip case, which predicts per unit rather than whole-block. Chroma is unchanged
  (its transform is not split). The function tail already did `record_split_luma` and per-TU
  deblock edges whenever `vartx_leaves.is_some()`, so intra split blocks inherit both.
- `crates/ec-av1/src/decode.rs` counter `INTRA_IN_INTER_SPLIT_TX_HITS` +
  `intra_in_inter_split_tx_hits() -> [usize; 4]` (bucketed 8/16/32/64), incremented once per
  split intra-in-inter block.
- `crates/ec-av1/src/decode.rs` `decode_inter_block8` (~17070): an 8x8 intra leaf whose depth
  resolves to TX_4X4 has no per-TU intra loop in that path — refused by name.
- `crates/ec-av1/src/refusal_inventory.rs:63`: lifted string replaced by the narrower
  `"an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4 transform units"`.
- `crates/ec-av1/src/stream.rs`: new gate `intra_in_inter_split_tx_gate` + its 8/10-bit tests.

## Gate

`a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact`
and `..._10bit_...`. Real aomenc, 64x64, 8 frames, mandelbrot zoom with a HARD CUT at frame 4
(`overlay=enable='gte(n,4)'` of a second fractal) so aomenc codes intra blocks inside an inter
frame; `--kf-min-dist=1000 --kf-max-dist=1000` stops the scene-cut detector inserting a second
key frame. Overrides (`--enable-tx-size-search=1`, `--enable-rect-partitions=0`,
`--max-partition-size=32`, `--min-partition-size=16`) are appended AFTER the base recipe
(last occurrence wins). Every decoded frame's Y, U and V are compared with ffmpeg; a decode
error only continues the attempt loop if it contains "unsupported"; exhausting all 40 attempts
is a panic, never a SKIP.

Run:
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrainter cargo test -p ec-av1 --lib -- split_transform_intra_block --nocapture`

EVIDENCE: $HOME/.cache/intrainter-suite.log | aomenc cut-fractal stream, ours vs ffmpeg, all 8 frames Y/U/V | 8-bit seed 45: 12 split intra-in-inter 16x16 blocks, 0 differing samples; 10-bit seed 72: 1 such block, 0 differing samples

## Suite

`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrainter cargo test -p ec-av1 --lib`
(as user unit `intrainter-suite-1788315424.service`, log `$HOME/.cache/intrainter-suite.log`):
`test result: ok. 341 passed; 0 failed; 31 ignored; 0 measured` in 457.31s — includes every sibling
(inter_sequence*, tx_select, 8x8_leaf_split, split_transform_*, filter_intra_*, cfl,
refusal_inventory, gate_coverage).

## Film

`troy-head.obu` and `troy5.obu` both stop at "a coded HORZ/VERT strip whose chroma transform has no
rect coefficient tables here" (lane-rectchroma), not at this lane's refusal.

## Residue

- deferred(a follow-up arm at --max-partition-size=64 and a lower cq): 32x32/64x64 split
  intra-in-inter blocks never fired; the per-TU loop is size-generic but unproven above 16x16.
- deferred(a per-TU intra loop inside decode_inter_block8): the 8x8 leaf's TX_4X4 2x2 grid,
  now the narrower named refusal.
