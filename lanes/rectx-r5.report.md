# lane-rectx r5 -- three "decodes fine, wrong pixels" cells: one root cause, luma now exact in all three

Branch `lane-rectx`, on top of r4's `fad4d5a`. r4 left three swept cells decoding to
completion with WRONG pixels (the worst class). All three had the SAME root cause; after the
fix two are byte-exact and all three are LUMA-exact, with a chroma-only residue below.

## Root cause (found this round)

`decode_leaf_rect` took the mi-exact intra-mode neighbour map in r4 (`mode_above_mi` /
`mode_left_mi`). The three OTHER `kf_y_mode` readers did not:
`decode_block` (square, `decode.rs` ~4960), `decode_block_rect` (32x16/16x32, ~3274) and
`decode_block_rect64` (64x32/32x64, ~3850) each read the COARSE per-16x16 `above_mode[c]` /
`left_mode[r]` slots -- which no sub-16x16 block ever writes (`record_rect` loops
`w / SUB` = 0 cells for a 16x8). So a 32x16 strip whose left neighbour was a 16x8 rect leaf
read an older, larger block's mode, indexed a DIFFERENT `kf_y_mode` CDF row, decoded the same
mode VALUE off a different distribution, and desynced from that block's first coefficient on.
This is the memory class "context read from one cell" / "CDF row held constant", one level up
from r4's own instance.

Fix (`decode.rs`): one `Neighbours::modes_above_left(r, c)` helper (mi-exact if the map holds
exactly this block's above/left mi, else the coarse slot) used by all three readers, for the
mode CDF row AND for `smooth_neighbor` (libaom's `get_filt_type` reads the same `above_mi`/
`left_mi`). New `MODE_MI_OVERRIDE_HITS` counter (`decode.rs` ~760) counts the reads where the
two disagree.

Also applied here, per the coordinator's mid-round correction (defect found by lane-sub8's
verifier in the code r4 cherry-picked from that lane): `mode_above_mi`/`mode_left_mi` guarded
availability with `mi_r > 0`/`mi_c > 0` instead of the tile-relative
`mi_r > tile_row0_mi`/`mi_c > tile_col0_mi`, and the maps were never reset -- `start_tile` now
clears `sub8_mode_col` over the tile's mi columns and `start_row` clears `sub8_mode_row`,
exactly like every other neighbour array beside them. (This lane's sweep is single-tile, so
that half is code-verified here, not gate-proven; lane-sub8 r6 owns the multi-tile gate.)

## Evidence

EVIDENCE: /tmp/.../scratchpad/rx/{rgb240.obu,pre_a.f0,pre_o.f0,o.step.txt,a.step.txt} |
ffmpeg lavfi `rgbtestsrc=size=64x64` -> aomenc `--cq-level=24 --reduced-tx-type-set=0
--enable-rect-partitions=1 --min-partition-size=8 --max-partition-size=32
--enable-filter-intra=0 --enable-cdef=0 --enable-restoration=0` (sha256
22113636b7ab950b5d1a175a60b9e01251dcead819f99a9ef14dcd1378ccfad6, reproducible across two
encodes); `EC_TRACE_MODE_STEP` ladder ours vs instrumented aomdec, `EC_AV1_PREFILT_DUMP` both |
first divergent element = `mi_row=8 mi_col=8 name=mode val=2` at range 53452 (oracle) vs 33402
(ours) -- same value, different CDF row, entry ranges equal (56121) so it is the ROW not the
symbol; that block's left neighbour is the 16x8 leaf at mi(8,4) mode=12. Pre-filter luma diff
1006 -> 0, total 1650 -> 12 bytes (all chroma).

EVIDENCE: gate `a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact`
(`stream.rs` ~5460) | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- a_real_aomenc_stream_whose_square_block_reads_a_sub16 --nocapture` |
`pixel-exact, mode_mi_override_hits=2`, two pinned arms (`rgbtestsrc` 64x64 cq32 with
`--reduced-tx-type-set=1 --enable-filter-intra=0` and with `=0 --enable-filter-intra=1`), each
encoded twice and byte-compared before decode; the counter is read only on an attempt that
decoded AND is pixel-compared.

EVIDENCE: /tmp/.../scratchpad/rx/{rgb240,rgb241,rgb321,mandfi161}.{obu,ours.yuv,ref.yuv} |
`examples/decode_probe <obu> <yuv>` vs `ffmpeg -i <obu> -f rawvideo`, per-plane byte diff |
per cell, before -> after this round: rgbtestsrc cq24 rtx=0 1650 -> 12 (luma 1118 -> 0);
rgbtestsrc cq24 rtx=1 3461 -> 14 (luma 2304 -> 0); rgbtestsrc cq32 rtx=1 1622 -> 0;
rgbtestsrc cq32 `--enable-filter-intra=1` rtx=0 -> 0; mandelbrot start_x=-0.6 cq16
`--enable-filter-intra=1` rtx=1 126 -> 123 (luma 0, U 40, V 83).

## Second defect this round (coordinator-reported, verified and fixed)

`Reach::top_right_rect`/`bottom_left_rect` (`encode.rs` ~1162/~1196) indexed the rect
`has_tr_*`/`has_bl_*` tables with `(blk_row << (4 - bw_log2)) + blk_col`. libaom's
`has_top_right` uses `MAX_MIB_SIZE_LOG2` = **5** (`enums.h`; its tables are laid out for a
128x128 = 32-mi superblock), so r4 halved the row stride and every table row but the first
read the wrong byte -- 16x8 wrong at 80 mi positions in a 64-wide SB, 8x16 at 16, 32x16 at 64
(a regression against `main`). Fixed via a named `Reach::rect_table_index` used by both.

EVIDENCE: `cargo test -p ec-av1 --lib -- rect_reach_tables_are_indexed --nocapture` |
new `encode::tests::rect_reach_tables_are_indexed_with_a_32_mi_row_stride` walks every block
position of a 32-mi superblock for all six rect sizes and asserts the index set is a BIJECTION
onto the table's bits (plus `len * 8 == rows * cols`) | passes with the shift at 5; with r4's
`4 - bw_log2` the walk covers a quarter of the 16x8 table and the assert fails. No pixel change
in the five swept cells (they are all luma-exact either way), so this fix is table-verified,
not gate-differentiated.

## Open residue

- fix-now(next round, needs its own lane -- it is a CHROMA PREDICTION defect, not a rect one):
  every remaining mismatch is chroma-only with luma byte-exact and the entropy ladder
  IDENTICAL end to end (`diff` of the two `EC_ISTEP` traces on mandfi161 is empty apart from
  two `use_filter_intra` lines the oracle prints and we do not -- ranges match, so the symbol
  is consumed). Localised per 4x4 chroma block on mandelbrot cq16 fi=1: the three FULLY wrong
  blocks (16/16 samples) are mi(10,4) V, mi(10,6) V, mi(12,12) U -- all three `uv_mode=13`
  (`UV_CFL_PRED`), one plane wrong while the other is exact, i.e. the per-plane CFL alpha, not
  the shared luma AC. The partially wrong blocks (1-13 samples, max delta 9) are all
  directional chroma with a NON-ZERO `angle_uv` (mi(8,12) uv=4 delta -3, mi(10,2) uv=4 delta 3,
  mi(12,8) uv=6 delta -3, ...), i.e. the chroma intra-edge filter/upsample path. Ruled out this
  round by experiment: `smooth_neighbor_uv` is NOT the cause -- forcing it true and false
  globally leaves the diff at 123 bytes either way (rgb240: 12/13/12).
- deferred(a lane for AB partitions below 16x16): unchanged from r4, `a HORZ_A/HORZ_B/VERT_A
  partition below 16x16` still dominates the refusals in the sweep.
- accepted: `read_coeffs_rect` still refuses `V_DCT`/`H_DCT` on a rect transform
  (`--reduced-tx-type-set=0` cells can hit it: rgbtestsrc cq32 rtx=0 fi=0 refuses by name).
- accepted: `filter intra on a HORZ/VERT strip` still refuses (4 of the 6 filter-intra cells
  swept above stop there, non-silently).
- deferred(the rect `predict_filter_intra` predictor): the coordinator asked for a gate arm
  asserting a counter of `use_filter_intra` reads on 16x8/8x16 leaves. It cannot exist yet --
  every recipe where aomenc puts filter intra ON a rect leaf hits exactly that refusal
  (`filter intra on a HORZ/VERT strip`, 4 of 6 cells), and the two `--enable-filter-intra=1`
  recipes that DO decode (`rgbtestsrc` cq32 rtx=0, now a pinned arm of the new gate) contain no
  such leaf, so the counter would assert 0. A gate for those CDF rows unblocks when
  `predict_filter_intra` grows a rectangular arm; the r4 refusal is what holds the line
  meanwhile.

## Refusals

None lifted or added this round; `refusal_inventory` and `gate_coverage` unchanged and green.

## Totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (CARGO_TARGET_DIR=~/.cache/cargo-target-rectx,
final commit): **272 passed, 0 failed, 24 ignored** (825.42s) -- r4's 270 plus the two tests
added this round (`a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact`,
`rect_reach_tables_are_indexed_with_a_32_mi_row_stride`); nothing regressed.
