VERDICT: PASS -- fixed, gate pixel-exact, committed d64ad23

## Order 1: TxbSet::Luma64 CDF-set resolution

Checked libaom `get_txsize_entropy_ctx(TX_32X64)` against `txsize_sqr_map`/
`txsize_sqr_up_map` (`common_data.h`): `(TX_32X32 + TX_64X64 + 1) >> 1 =
TX_64X64`. Matches this decoder's `TxbSet::Luma64` (used for both the plain
64x64 and the superblock-strip 32x64/64x32 corner). **CDF-set selection is
correct** -- ruled in as clean, per the charter's own "only if that matches"
gate to move on.

## Root cause: not base_ctx's neighbour math, but its position-offset table

libaom's `read_coeffs_txb` (`decodetxb.c`) passes the **raw, un-adjusted**
`tx_size` (`TX_32X64`/`TX_64X32`, not the corner-cropped `TX_32X32`) into
`get_scan`/`get_lower_levels_ctx`. `get_scan` resolves to the same
`default_scan_32x32` either way (checked `scan.c`, comment: "half of the
coefficients of tx64 at higher frequencies are set to zeros, so tx32's scan
order is used") -- that part this decoder already had right. But
`get_nz_map_ctx_from_stats` (`txb_common.h`) separately indexes
`av1_nz_map_ctx_offset[tx_size][coeff_idx]` by that same raw tx_size, and
`av1_nz_map_ctx_offset_32x64`/`_64x32` (`txb_common.c`) are **distinct
tables** from the square `av1_nz_map_ctx_offset_32x32` one -- e.g. at
`(row=1, col=0)` the square table gives offset 1, the 32x64 (width<height)
table gives offset 11. This decoder's `base_ctx` always used the square
`NZ_MAP_CTX_OFFSET_32` table, regardless of the true block shape, for every
`read_coeffs` caller including `decode_block_rect64`'s truncated-corner
luma read -- exactly the symptom r7 pinned (same range in, wrong symbol
out, no earlier desync).

This decoder already has the *right pattern* for this, just not wired to
the >32 corner case: `base_ctx_rect` (lane-rectwire, used by
`decode_block_rect`'s 32x16/16x32 strips) implements the same `w<h`/`w>h`
branch libaom's own doc comment names. It couldn't be reused directly here
because its `neighbour_rect` indexes the grid by the *true* `w`/`h`
(64/32), but the truncated-corner grid is genuinely only 32x32 -- reusing it
would break the neighbour bounds. Verified the two real libaom tables
(`av1_nz_map_ctx_offset_32x64`/`_64x32`) reduce to the same `row.min(4)`/
`col.min(4)` 5x5-clamp pattern `NZ_MAP_CTX_OFFSET_32` already uses (checked
programmatically against all 1024 entries of both tables), so added them as
plain 5x5 consts instead of trying to force-fit `base_ctx_rect`'s neighbour
function onto a 32-wide grid.

## Fix

- `cdf.rs`: added `NZ_MAP_CTX_OFFSET_32X64`/`NZ_MAP_CTX_OFFSET_64X32`
  (verified against libaom's real tables, not the approximate row/col<2
  formula in its comment -- that formula does not actually reproduce every
  cell, e.g. `(row=0, col=2)` is 6 in the real 32x64 table, not the naive
  ctx+11 the row<2 branch alone would give).
- `decode.rs`: `base_ctx` gained a `rect_shape: Option<(usize, usize)>`
  parameter, used only for `TxClass::TwoD` to pick the matching table
  (`w<h` -> 32x64, `w>h` -> 64x32, else/`None` -> square). Threaded through
  `read_coeffs` (`rect_shape` param, doc'd). All three `read_coeffs`
  callers updated: `decode_block_rect64`'s luma corner passes
  `Some((bw, bh))` (the real, un-adjusted strip shape); `decode_block`'s
  own 64x64 corner and the inter path pass `None` (genuinely square).

## Gate result

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
  a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact \
  -- --nocapture
```
Before this round: 0/40 pixel-exact, mismatches on every attempt that
reached a block2-shaped luma read. After: **15/40 pixel-exact matches, 25
named refusals (unrelated partition/palette/Golomb capability gaps, not this
defect), 0 pixel mismatches, `sb_rect_hits=36`** -- hard pass.

## Ruled out (per charter, not revisited)

`around_rect`, the CDEF per-SB guard, the reconstruction theory.

## Not attempted

Stage 2 (32x32 `part32`) and the inter path, per the charter's explicit
"do not start" instruction. The same class (offset table indexed by raw vs
adjusted tx_size) could in principle recur wherever a >32 transform corner
is truncated elsewhere in this codebase, but the only other truncation site
(`decode_block`'s plain 64x64) is genuinely square, so it does not need this
fix -- swept by inspection (both non-`decode_block_rect64` `read_coeffs`
call sites checked and confirmed square, see above).

## Hard rules followed

Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; foreground `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1`
on the gate; aomenc recipe unchanged/inherited (`--threads=1 --row-mt=0
--sb-size=64 --enable-tx-size-search=0`); read-only use of the shared
`~/.cache/aom-oracle` source tree (no rebuild, no patch left behind -- this
round needed only to read existing libaom `.c`/`.h` source, not to
instrument aomdec further); no other worktree touched; no push, no merge
into main.
