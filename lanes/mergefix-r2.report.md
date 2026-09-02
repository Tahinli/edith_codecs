# lane-mergefix r2 report

## Root cause found and fixed: the var-tx neighbour contexts were never reset at a tile boundary

Instrumented the oracle: added an `EC_VARTX`-gated `fprintf` in libaom
`read_tx_size_vartx` (`av1/decoder/decodeframe.c`, right after the
`txfm_partition_cdf[ctx]` read) printing `mi_row/mi_col/bsize/depth/blk_row/blk_col/
tx_size/ctx/above/left/val/rng`. Built into a PRIVATE tree
(`~/.cache/mergefix-tmp/aom-build`, cmake + ninja, `CONFIG_AV1_ENCODER=0`); the shared
`~/.cache/aom-oracle/build` was not touched. The edit in the oracle source is additive and
env-gated, so no other lane's trace changes.

Ladder at the r1 first-divergence element (frame 1, mi(0,32), 32x64 VERT inter strip):

    aomdec: depth0 tx=TX_32X64 ctx=0 above=64 left=64 val=1 rng=48128
            depth1 tx=TX_32X32 ctx=3 above=64 left=64 val=1 rng=49648
            depth1 blk_row=8  ctx=4 above=16 left=64 val=1 rng=43818
    ours  : ctx=1 rng=51444 / ctx=4 rng=45408 / ctx=4 rng=43372   (BEFORE)

`ctx` is off by exactly the `left` term while `above` agrees: our `left_txfm` at mi row 0
still held `16`, written by the block at mi(0,24) -- which lives in the PREVIOUS TILE COLUMN
(the stream is `--tile-columns=1`, i.e. 2 tile columns, boundary at mi_col 32).

libaom resets both bands and we reset neither:
- `av1_zero_left_context` memsets `left_txfm_context_buffer` to
  `tx_size_high[TX_SIZES_LARGEST]` (64) at every superblock row of every tile
  (`decodeframe.c:2796` / `:3238`, inside the per-tile `mi_row` loop).
- `av1_zero_above_context` memsets `above_contexts.txfm[tile_row] + mi_col_start` to
  `tx_size_wide[TX_SIZES_LARGEST]` over the tile's own column span (`:2789` / `:3231`).

### Hunks (by function name, for porting onto main) -- `crates/ec-av1/src/decode.rs`

- `Neighbours::start_tile` (end of the function, after the `sub8_mode_col`/`uv_mode_col`
  loop): reset `self.above_txfm[i] = TXFM_CTX_INIT` over `col0_mi..col1_mi` (clamped).
- `Neighbours::start_row` (end of the function, after the palette bands):
  `self.left_txfm.iter_mut().for_each(|t| *t = TXFM_CTX_INIT);`
- (diagnostic, env-gated, keep or drop) `EC_PARTG` eprintln in the three gathered
  edge-partition branches of the inter tile path (64/32/16), printing `ctx` + `rng`.

Class: `new-map-ignores-tile-edge` -- COMMON's NEIGHBOUR MAPS rule ("reset in start_tile AND
start_row") had never been applied to the two txfm bands.

EVIDENCE: ~/.cache/mergefix-tmp/vartx.log + o.vartx | instrumented aomdec EC_VARTX vs our
EC_TRACE_MODE_STEP name=txfm_split on the pinned 192x68 cq61 tile_cols=1 stream (md5
a14892ed0ba88b6ad2b566e251ea2d33) | all 14 txfm_partition symbols now agree in ctx AND range
(48128/49648/43818), 0 of 14 before at mi(0,32)

EVIDENCE: gate output | `cargo test -p ec-av1 --lib -- a_frame_edge_straddling_band_decodes_pixel_exact`
| frame 1 plane Y differing pixels 6859 -> 3622 (still RED, second defect below)

## Still RED: `a_frame_edge_straddling_band_decodes_pixel_exact`

A SECOND, independent defect owns the residue. Full cross-decoder range ladder
(EC_MODE/EC_ISTEP/EC_COEFF_STEP, `tag=br|sign|golomb|tx_type` and the two decoders'
differently-named var-tx lines dropped) is element-identical until the LAST superblock row
of frame 1 in the SECOND tile column: SB mi(16,32), where only 1 of 16 mi rows is inside the
frame (`mi_rows == 17`).

    aomdec: EC_PART bsize=12 ctx=13 rng=51641 -> SPLIT
            EC_PART bsize=9  ctx=8  rng=38495 -> SPLIT
            EC_PART bsize=6  ctx=4  rng=45016 -> SPLIT
            EC_PART bsize=3  ctx=0  rng=51116 -> NONE   (8x8 inter leaves)
    ours  : EC_PARTG bsize=12 ctx=0 rng=51641 -> (gathered bit 0) HORZ
            ... a 16x16-wide block at mi(16,32), EC_MODE rng=40004 vs aomdec 42754

aomdec's ctx print is `bsl * 4 + ctx`, so its 64-level ctx is 1 (above=1, left=0) and ours
is 0. `Neighbours::partition_ctx_mi` computes `above = above_side_mi[mi_c] * 2 <= side`, so
ours says the block above is 64 wide -- but the block above IS the 32x64 VERT strip at
mi(0,32). The SB-level PARTITION_VERT inter strip publishes the PARENT's 64 into
`above_side_mi` instead of its own 32 (class `tx-grid-published-block-side`). The 32-level
and 16-level ctx (0/0) already agree with aomdec, so this is a single-site defect.

fix-now (handed off): see `lanes/mergefix-r2.handoff.md`.

## Test totals
Not run to completion this round (hard 50-minute deadline). The one gate above was run by
name. Sibling gates (split-transform, sb128c intra_rect_block/gathered_edge_horz,
uv8/intersub8/inter16ab/rectchroma2) are NOT yet re-run --
deferred(one `cargo test -p ec-av1 --lib` invocation with those names).
