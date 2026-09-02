# lane-mtfix r1 — the multi-tile intra gate's U-plane failure on main 11633d7

## What was wrong

`Neighbours::smooth_uv_neighbour` (`crates/ec-av1/src/decode.rs`) decides the chroma
intra-edge filter strength from the above/left `uv_mode` neighbour (libaom
`get_intra_edge_filter_type`, reconintra.c). Its mi-exact maps (`uv_mode_col`/`uv_mode_row`)
were tile-guarded, but the COARSE fallbacks were not: on a miss it read
`above_uv_mode[c]` / `left_uv_mode[r]` unconditionally. `above_uv_mode` is a frame-wide
`SUB`-grid array and is the ONE `above_*` band `start_tile` never clears (checked by
enumerating the struct's `above_*` fields against `start_tile`'s reset list:
`above_txfm` and `above_palette_*` are missing from the reset too, but every reader of
those is already guarded by `mi_r > tile_row0_mi` / `mi_c > tile_col0_mi`).

So the first block of a tile ROW read the `uv_mode` of the block above it **in the tile
above**, where libaom has `above_mbmi == NULL`, and filtered its chroma edge at the wrong
strength — luma untouched.

This is class `new-map-ignores-tile-edge`. It was not a regression of working code: at
56ea250 seed 46 REFUSED (`palette syntax is consumed for square blocks only`); the
palette2 merge lifted that refusal and the defect behind it became visible
(class `refusal-hides-a-defect`).

## Bisect

| commit | gate `a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact` |
|---|---|
| 56ea250 (lane-ab16 merge) | ok — 5 pixel-exact matches, **1 named refusal at seed 46** |
| 11633d7 (lane-palette2 merge, main) | FAILED — frame 0 U vs ffmpeg, seed 46 |
| this branch | ok — 6 pixel-exact matches, 0 refusals, 4 tile-edge suppressions |

EVIDENCE: $HOME/.cache/mtfix-ab16.log, $HOME/.cache/mtfix-gate1.log, $HOME/.cache/mtfix-gate2.log | ran the one gate in a detached worktree at 56ea250 and in the lane worktree at 11633d7 | pass w/ seed-46 refusal vs FAILED U plane

## First wrong block

`EC_AV1_GATE_DUMP` pinned the seed-46 stream (166 bytes, 256x128, `cols=2 rows=2`); ours
vs the oracle's `EC_AV1_PREFILT_DUMP` frame 0:

* Y: 0 differing samples.
* U: 14 samples, V: 17 samples, all inside chroma x[120..127] y[32..37], magnitude 1..2.
* That is the 8x8 chroma / 16x16 luma block at luma (240, 64) — the FIRST block of the
  bottom-right tile (tile row boundary y=64, tile column boundary x=128), i.e. exactly a
  tile's top edge. Reconstruction was already wrong pre-loop-filter.

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/mtfix/{ours,ref}.f0 | decode_probe + aomdec, both under EC_AV1_PREFILT_DUMP, on the pinned seed-46 stream | Y diff 0, U diff 14 / V diff 17, bbox chroma x[120..127] y[32..37]

## Fix

`crates/ec-av1/src/decode.rs` `smooth_uv_neighbour`: availability is tile-relative on both
axes and on the coarse fallback too (`have_above = mi_r > tile_row0_mi`,
`have_left = mi_c > tile_col0_mi`); no neighbour ⇒ `DC_PRED` (not smooth), which is what
libaom's NULL `above_mbmi`/`left_mbmi` gives. Guarding the READ rather than adding
`above_uv_mode` to `start_tile`'s reset also covers the left axis and leaves the leak
observable, which is what the new counter counts.

`crates/ec-av1/src/decode.rs` `uv_tile_edge_suppressed_hits()`: counts reads suppressed at
a tile edge that is not a frame edge AND whose coarse slot held a non-DC mode — i.e. reads
that would have picked the wrong filter strength.

`crates/ec-av1/src/stream.rs` `run_multi_tile_gate` reports the per-gate delta, and
`a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact` (the only arm with a tile ROW
boundary) HARD-ASSERTS it is ≥ 1.

Mutation: the un-guarded code IS main 11633d7, and that tree fails this gate at seed 46
(row 2 of the bisect table above).

## Gates

* `cargo test -p ec-av1 --lib multi_tile cfl chroma_edge palette ab_partitions filter_intra refusal_inventory gate_coverage`: 31 passed, 0 failed, 3 ignored.
  Counters: intra 2x2 gate 6/6 matches, 4 tile-edge suppressions; 10-bit (4x1 tiles) and
  palette (2x1) arms report 0 — they have no tile row boundary, and the left band resets
  every SB row, so there is nothing to suppress there.
* Full suite (`systemd-run` unit, log `$HOME/.cache/mtfix-suite.log`): **339 passed, 0 failed, 31 ignored**.

## Residue

* `above_txfm`, `above_palette_size`, `above_palette_colors`, `above_palette_uv_*` are still
  absent from `start_tile`'s reset list — accepted: every reader of them is tile-guarded
  (decode.rs:3411/3422/3505/4837/4838/9149/9150), so the arrays are dead across a tile edge.
  COMMON's "reset in start_tile" rule would add 4 more loops for no behavioural change.
