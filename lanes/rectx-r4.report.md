# lane-rectx r4 — the gate is GREEN: two defects, one cherry-picked, one found here

Branch `lane-rectx`, commits on top of `main` 3808cf8. Round 3 left the lane's own gate red
with the mode-neighbour defect precisely located; this round fixed it and one more.

## What changed

1. **Cherry-picked lane-sub8 `1841406`** (mi-exact intra mode neighbours + `has_tr_4x4`/
   `has_bl_4x4`): `Neighbours::sub8_mode_col/row`, `record_mode_mi`, `mode_above_mi`/
   `mode_left_mi` (`crates/ec-av1/src/decode.rs` ~1830, ~2533-2570), written by `record_rect`
   and `record_split_luma`/`decode_leaf8`, plus `crates/ec-av1/src/encode.rs`'s reach rows.
   The commit's `decode_leaf_split4`/`read_intra_mode_sub8` hunks were dropped — those
   functions exist only on lane-sub8.
2. **`decode_leaf_rect` joins that map** (`decode.rs` ~3644 read, ~3786 write): it now reads
   `mode_above_mi`/`mode_left_mi` after the `prev_leaf` override and records its own
   `mi_w x mi_h` span, not one cell. This is r3's located defect (leaf mi=(4,12) read
   `kf_y_mode` from the coarse 16x16 slot, decoding `mode=0` where the oracle decodes
   `mode=2`).
3. **ROOT CAUSE FOUND THIS ROUND — the rect reach tables were the wrong block size.**
   `Reach::of_rect` (`encode.rs` ~1096) indexed `HAS_TOP_RIGHT_RECT`/`HAS_BOTTOM_LEFT_RECT`
   by `bw == 16`; both arrays only ever held the 32x16/16x32 rows (lane-intradisp's sizes).
   A 16x8 block read `has_tr_16x32`/`has_bl_16x32` (4 bytes) where libaom reads
   `has_tr_16x8`/`has_bl_16x8` (16 bytes), an 8x16 read the 32x16 rows, and the index
   arithmetic (`table_stride = 128/side`, `% (len*8)`) was not libaom's either.
   `top_right_rect`/`bottom_left_rect` are now libaom `has_top_right`/`has_bottom_left`
   verbatim at block granularity (`row_off`/`col_off` 0, luma), with `has_tr_rect_table`/
   `has_bl_rect_table` carrying 16x8, 8x16, 32x16, 16x32, 64x32 and 32x64 transcribed from
   `~/.cache/aom-oracle/src/av1/common/reconintra.c`. (`has_tr_vert_tables`/`has_bl_vert_tables`
   hold the SAME pointers for every W<H entry and NULL for W>H, so the partition type selects
   no different table at these sizes.)
4. Gate recipe (`stream.rs` ~5366): mandelbrot `start_x=-0.6`, `--cq-level=16`, and
   `--enable-filter-intra=0` added (filter intra ON a strip is a separate, still-refused
   predictor).
5. `examples/decode_probe.rs`: lane-sub8's optional second arg dumping raw yuv420p, which is
   what made the ffmpeg pixel diff a one-liner outside the test harness.

## Evidence

EVIDENCE: /tmp/claude-1000/.../scratchpad/{x.obu,pre_a.yuv.f0,pre_o.yuv.f0} | ffmpeg lavfi
rgbtestsrc 64x64 -> aomenc cq50 --enable-rect-partitions=1 --enable-ab-partitions=0
--enable-filter-intra=0 --reduced-tx-type-set=1 --min-partition-size=8 --max-partition-size=32;
`EC_AV1_PREFILT_DUMP` on instrumented aomdec vs `examples/decode_probe` | pre-loop-filter luma
diffs 1440 -> 0, final vs `ffmpeg -f rawvideo` 1567 -> 0 bytes. The 1440 luma diffs were a
D203 (`mode=7`) 16x8 strip at mi=(4,8): exact for the first rows, first mismatch at pixel
(44,18) with a diagonal wavefront — the signature of a ray that has started reading BELOW the
block's own height, i.e. wrong bottom-left availability.

EVIDENCE: gate `a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact` |
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16 --nocapture` |
`pixel-exact, rect_leaf_coeff_hits=8`; the stream's own symbol trace shows 6 non-skip TX_16X8
luma leaves + 1 non-skip TX_8X16 (both orientations), 9 non-skip TX_8X4 and 3 non-skip TX_4X8
chroma halves.

## Refusals

- `a coded (non-skip) HORZ/VERT rect strip below 16x16` — the string was already removed from
  `decode.rs` in r2 (never in `refusal_inventory::REFUSALS`); this round is what finally PROVES
  it, with the gate above. Nothing to delete, the inventory test stays green.
- `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding`
  (`decode.rs:9138`) — **NOT lifted.** It sits in the INTER square-block path
  (`reject_residual`, lane-warp r5), which this key-frame gate never reaches. Lifting it needs
  an inter gate.
- `a coded (non-skip) HORZ_B/VERT_B rect strip below 16x16` — **NOT lifted**, HORZ_B/VERT_B
  never fired in the gate stream.

## Open residue

- accepted: `read_coeffs_rect` still refuses `V_DCT`/`H_DCT` (`a non-2D tx class on a
  rectangular transform`), hence `--reduced-tx-type-set=1` in the gate.
- deferred(a lane for AB partitions below 16x16): the dominant refusal across the recipe sweep
  is still `a HORZ_A/HORZ_B/VERT_A partition below 16x16`. NOTE: unlike r3's premise, this
  survives the mode-grid fix, so at THIS size it is not (only) our own desync — 8 of 24 swept
  cells still refuse on it after both fixes.
- fix-now(next round): two swept cells decode to completion but are NOT pixel-exact
  (rgbtestsrc cq24 3540 bytes, cq32 1714; mandelbrot start_x=-0.6 cq16 with
  `--enable-filter-intra=1` 126). Same ladder applies; the gate's own cell is exact.

## Totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (CARGO_TARGET_DIR=~/.cache/cargo-target-rectx):
**270 passed, 0 failed, 24 ignored** (738.88s) — r3's single failure (this lane's own gate) is
gone and nothing regressed (269 -> 270 passed is exactly this gate).

That full run was built BEFORE the last hunk (the explicit 64x32/32x64 rows in
`has_tr_rect_table`/`has_bl_rect_table`, which previously fell through to the 16x32 row).
On the final build the four gates that can see those sizes were re-run:
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`,
`..._with_mandelbrot_fires_the_vert_b_partition_arm`, `..._with_directional_chroma_...` and
this lane's own gate — 4 passed, 0 failed, 1 ignored (21.26s).
