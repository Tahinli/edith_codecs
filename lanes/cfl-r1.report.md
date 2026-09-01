# lane-cfl r1+r2 report — chroma intra-edge filter type below 16x16

Branch `lane-cfl` @ `7148dba` (off `lane-rectx` bb41f82; not rebased, per charter).
r1 and r2 were both killed at their turn/session cap; their work was preserved as WIP
commits 38d096e + 455a142 and is squashed into `7148dba` by this round.

## Root cause

libaom `get_filt_type` (reconintra.c) chooses the chroma intra-edge filter STRENGTH from
`chroma_above_mbmi`/`chroma_left_mbmi`'s `uv_mode` (smooth-family neighbour => filtered
edge). Two defects stacked on the sub-16x16 path:

1. **Constant `false`.** `decode_leaf_rect` and `decode_leaf_8x8` passed a literal `false`
   as `smooth_neighbor_uv` into every chroma `reconstruct_rect` call, so a chroma edge was
   never smooth-filtered below 16x16 regardless of the neighbour.
2. **Neighbour map coarser than the block.** `above_uv_mode`/`left_uv_mode` are written in
   `SUB` (16px) cells by `for cell in 0..w / SUB`, which iterates ZERO times for a block
   narrower/shorter than 16. A sub-16 block therefore never *wrote* its uv_mode, and every
   reader (including the 16x16-and-up ones) saw whatever the last 16x16-or-larger block had
   left in that slot. Class: *context read from one cell* / neighbour map coarser than the
   block, the chroma twin of lane-rectx r5's luma `mode_above_mi`/`mode_left_mi`.

Observed symptom before the fix: the mandelbrot cq16 cell filtered a chroma left edge at
`ft=0` where the instrumented aomdec prints `ft=1` (left edge `95,59,30,28` vs `90,61,37,29`).

## Files changed

- `crates/ec-av1/src/decode.rs:1886,2015,2089,2349,2595` — `Neighbours::sub8_mode_col/row`
  widened from `(pos, mode)` to `(pos, mode, uv_mode)`; every reset/init site updated.
- `crates/ec-av1/src/decode.rs:2622-2680` — `uv_mode_above_mi`/`uv_mode_left_mi` and
  `uv_modes_above_left{,_mi}`: mi-exact chroma neighbour with the coarse `SUB` slot as
  fallback; tile-edge guards (`mi_r > tile_row0_mi` / `mi_c > tile_col0_mi`) inherited from
  the luma twin, per the COMMON neighbour-map rule.
- `crates/ec-av1/src/decode.rs:3422,3825,4001,5122,5552` — every `smooth_neighbor_uv`
  derivation now goes through `uv_modes_above_left{,_mi}`; `decode_leaf_rect` (3825) and
  `decode_leaf_8x8` (5552) previously had none at all.
- `crates/ec-av1/src/decode.rs:3854,3858,3910,3926, 5216..5428` — chroma reconstruct call
  sites take the derived flag. **Sweep: `grep -c "None, false," decode.rs` = 0** — no
  constant-false chroma edge-filter site remains.
- `crates/ec-av1/src/decode.rs:775-805,3294` — gate counters `uv_mode_mi_override_hits`,
  `cfl_block_hits`, `uv_angle_delta_hits`.
- `crates/ec-av1/src/stream.rs:5597-5742` — the gate (below).

No refusal string was lifted this lane (none in `refusal_inventory.rs` covered this path);
`gate_coverage.rs` unchanged.

## Gate

`a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact`
— real aomenc, `mandelbrot=size=64x64:start_x=-0.6`, `--cq-level=16 --cpu-used=4
--sb-size=64 --enable-rect-partitions=1 --enable-filter-intra=1 --reduced-tx-type-set=1
--min-partition-size=8 --max-partition-size=32`, aomenc output hashed twice for
reproducibility, pixel-compared against ffmpeg. Hard asserts `cfl_blocks > 0`,
`angle_blocks > 0`, `uv_overrides > 0`, and `compared > 0` (no vacuous pass). The 10-bit
arm is tolerated ONLY on a message containing "unsupported" and is then not compared.

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib chroma_edge_filter_reads_a_sub16 -- --nocapture
```

EVIDENCE: $HOME/.cache/cfl-suite.log (gate line) | aomenc mandelbrot -0.6 64x64 cq16 rtx=1 filter-intra=1, decode_stream vs ffmpeg_decode_sequence, all three planes | pixel-exact at 8-bit, cfl_blocks=15 angle_delta_uv_blocks=19 uv_mi_overrides=52; test result ok 1 passed

EVIDENCE: scratchpad/ours.yuv vs scratchpad/ff.yuv | `cargo run -p ec-av1 --example decode_probe -- cfl.obu ours.yuv` on the same aomenc stream; `ffmpeg -i cfl.obu -pix_fmt yuv420p -f rawvideo ff.yuv`; `cmp` | byte-identical, 6144 bytes, sha256 c8cfa9b332f9c6e8...

## Suite

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (log `$HOME/.cache/cfl-suite.log`):
**273 passed; 0 failed; 24 ignored; 0 filtered out** (963.81s), exit 0.

Siblings re-run green in that suite (COMMON sibling-gate rule): `a_real_libaom_cfl_stream_decodes_pixel_exact`,
`a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact`,
`a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact`,
`a_rect_strip_below_16x16_reads_its_own_filter_intra_cdf_row`,
`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`,
`a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact`,
`a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact`,
`a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact`,
`rect_reach_tables_are_indexed_with_a_32_mi_row_stride`, plus all three
`refusal_inventory::tests::*` and both `gate_coverage::tests::*`.

## Residue

- deferred(lane owning sub-16 AB partitions): the 10-bit twin of this recipe stops at
  `a HORZ_A/HORZ_B/VERT_A partition below 16x16` and is therefore never pixel-compared.
  The gate records this explicitly rather than passing silently.
- accepted: no refusal string lifted — this round is a correctness fix behind existing
  capability, not a capability claim.
