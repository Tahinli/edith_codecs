# lane-tile2 r1 HANDOFF (turn cap; work is COMMITTED, tree clean)

Tip: `93a4839` on `lane-tile2` (parent chain: `ac7271d` -> `33afd53` -> `f8fee86` merge of main).
NOTE: a second builder was started on this same worktree mid-round and authored `93a4839`
(var-tx context resets + gate arms) with the same session trailer. Both sets of changes are on
the branch and compile together; the r1 report `lanes/tile2-r1.report.md` carries both halves.

## ROOT CAUSE (found, fixed, gated) — class new-map-ignores-tile-edge, INVERTED
`decode::build_motion_field` (crates/ec-av1/src/decode.rs:2408, now :2413) read the frame's mi
grid through `MiGrid::get`, which still carried the LAST decoded tile's bounds. Every cell of
every other tile read `None`, so those tiles' stored motion field stayed empty and the NEXT
frame's temporal MV candidates there were all `(0,0)`.
Fix: `MiGrid::get_any` (crates/ec-av1/src/mvstack.rs, right above `MiGrid::set`) = raw cell read
ignoring tile bounds, used by `build_motion_field`. `MiGrid::get`'s narrowing is a
NEIGHBOUR-SCAN rule only; any consumer running after the tile walk must use `get_any`.

## Ladder evidence (reproducer `a_real_aomenc_compound_mv_stack_across_two_tile_columns...`)
Pinned stream: /home/tahinli/.cache/tile2/pin.obu (575 B, md5 feff5ede5318d27fe5dbb3891f1911e7,
hashed twice), regenerate with /home/tahinli/.cache/tile2/pin.sh (seed 200, 128x64,
--tile-columns=1, cq45). Ablation twin pin0.sh (--tile-columns=0) is exact 16/16 -> tile-specific.
- `EC_AV1_PREFILT_DUMP` both sides: first bad decode-order frame mismatches ONLY in columns
  0..63 = tile column 0 (later frames bleed to col ~69 via MC+deblock) -> reconstruction, not filters.
- `EC_TRACE_MODE` both sides (/home/tahinli/.cache/tile2/{aom,ours}.mode.txt):
  FIRST DIVERGING ELEMENT = the block's MV VALUE, never a symbol. `rng=` is IDENTICAL at every
  block (entropy in sync); `EC_MODE_VAL mi_row=0 mi_col=0` reads ours `mv0=(0,0)` vs oracle
  `mv0=(14,0)` (and `(34,0)` on the earlier frame), for `mi_col < 16` only; `mi_col` 16/24
  (tile 1) match exactly. `EC_STACK` shows the SAME candidate WEIGHTS (688/692/740/2) with
  zeroed MVs -> the scan found the right neighbours, the stored temporal MVs were empty.
- BEFORE: 14 of 16 decode-order frames differ (worst 2033 samples, max |d| 6).
  AFTER: 0 of 16 differ, all three planes, hidden frames included.

## Maps that GOT a tile guard this round (file:line)
- crates/ec-av1/src/decode.rs `build_motion_field` — `get_any`, frame-wide read (root cause).
- crates/ec-av1/src/decode.rs — `INTRABC_MI_GRID` now takes `set_tile_bounds(mi_row0, mi_col0,
  mi_row1, mi_col1)` at BOTH tile-loop starts (key path ~:13124 and inter path ~:20786, right
  after `neighbours.start_tile`). It was built whole-frame and never narrowed. DONE, but
  UNGATED: no aomenc recipe here produces intrabc (palette-shaped content); a gate needs one.
- crates/ec-av1/src/decode.rs (commit 93a4839) — `above_txfm` reset at tile start, `left_txfm`
  reset at superblock-row start (libaom av1_zero_above_context / av1_zero_left_context,
  av1_common_int.h:1622 and :1628). libaom-cited, stream-inert on everything measured.

## Maps ALREADY tile-relative (verified by grep, do not re-chase)
`Neighbours` bands via `tile_row0_mi`/`tile_col0_mi` (decode.rs:3987, :3998, :4081, :4223-4224,
:4602-4609, :4649-4651, :4698-4730, :5637-5638, :9259, :11174-11189, :16344-16345, :19061-19062);
`MiGrid::get` tile window (mvstack.rs:264-272); mvstack reach clamps (mvstack.rs:831-845, :1624-1638);
`CurrentQIndex`/`DeltaLF` reset per tile (decode.rs:13106, :20723); per-tile CDFs + fresh
`SymbolDecoder`; `PlaneBuf::set_tile_origin` per plane.

## Maps NOT yet swept for a tile guard (next builder's list)
comp_group_idx map, palette cache/colour context, segment map (`segment_id_at`), cdef_idx band,
skip_mode neighbour reads, `delta_lf_grid` reads at a tile edge (decode.rs:12129-12132),
and the motion-field PROJECTION source (`motion_field.rs`) — none inspected this round.

## Gate state
GREEN, all in crates/ec-av1/src/stream.rs:
- `a_real_aomenc_compound_mv_stack_across_two_tile_columns_decodes_pixel_exact` — un-ignored, FIRING.
- `a_real_aomenc_multi_tile_gate` + arms (256x128, sb 64, compound refs + ref-frame-mvs + OBMC,
  palette/intrabc/CDEF/LR off, 3 seeds x cq {45,55,35}, EVERY decode-order frame incl. hidden
  compared all-plane vs instrumented aomdec through `decode_all_frames_vs_oracle`; per attempt
  asserts `decode::tile_hits() >= 2*frames` and `mvstack::tile_reach_clips() > 0`):
  two_tile_columns / two_tile_rows / two_by_two_tile_grid (8-bit) / 10bit_two_by_two_tile_grid.
  Measured: 12/12 attempts exact, tile_reach_clips +240..+384, tiles 32..68 per attempt.
- `#[ignore]`d RED (93a4839's arm): var-tx multi-tile arm, pre-existing chroma residual
  (decode-order frame 3, U plane byte 35079, 16 bytes; identical with the two resets ablated).

## EXACT NEXT STEP
1. Re-run the full suite on `93a4839` — my run was started on `ac7271d` and a SECOND suite unit
   (`tile2-suite-r1-1788346456`) was armed by the parallel builder onto the SAME log
   (`$HOME/.cache/tile2-suite-r1.log`), so the log is interleaved and its totals are NOT usable.
   One unit only: `systemd-run --user --unit=tile2-suite-r2 -p MemoryMax=10G --same-dir bash -lc
   'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tile2
   nice -n 10 cargo test -p ec-av1 --lib -j3 > $HOME/.cache/tile2-suite-r2.log 2>&1'`
2. Re-measure the two still-ignored multi-tile inter tests on this tree — the motion-field fix
   may have closed `a_real_aomenc_multi_tile_inter_stream_decodes_pixel_exact` ("seed-42 32x32
   block at x[224..255] y[32..63], tile attribution unproven").
3. Then chase the var-tx arm's chroma residual (frame 3, U byte 35079) — ablation says it is
   NOT the two new resets.
