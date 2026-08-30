VERDICT: PARTIAL -- green, stage 1 (order) + stage 2 landed, stage 3 not started (turn budget).

## Stage 1 -- gated r6's inter tile-loop lift
Added `a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`): `--sb-size=64`, `--tile-columns=1` (2
columns), `--loopfilter-control=0`, `--limit=2 --kf-max-dist=1000` so frame 1
is a genuine inter frame, `gradients_source`'s default `speed=0.01` rotation
gives real motion so the tile-bounded MV stack (`MiGrid::set_tile_bounds`)
actually has work to do across the boundary. Confirms via a header parse that
aomenc really produced a >1-tile-column inter frame (not a collapsed/kf
fallback) before trusting a match. 20/20 pixel-exact vs ffmpeg, 0 named
refusals, hard `tile_hits > frames` assert.

Building this gate found `decode_inter_frame_tile_with_cdfs`'s per-tile loop
(r6, commit 8899fc1) never incremented `TILE_HITS` -- only the key-frame loop
did (`decode.rs:4928`). Without the fix the gate's own tile-count assert
would have been vacuously blind on the inter path (it failed first-run:
"tile_hits delta was 2, expected > 2" -- both hits were from frame 0's key
tiles, frame 1 contributed zero visible signal despite decoding correctly).
Fixed by adding the same `TILE_HITS.with(|c| c.set(c.get() + 1));` call at
the equivalent point in the inter loop (`decode.rs:9814`, right after
`grid.set_tile_bounds`). Re-ran: 20/20 pixel-exact, `tile_hits` counted
correctly.

Committed bd1c01f.

## Stage 2 -- lifted >2-columns and multi-tile-loop-filter refusals
Charter order: re-run the key-frame 2-column gate
(`a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact`) with
`--loopfilter-control=0` removed. Did so: 20/20 pixel-exact, confirming
`PlaneBuf`'s `tile_x1`/`tile_y1` clamps (real values at every call site since
r6) already make real deblocking across a tile boundary correct. Made this
permanent (flag removed for real, not just the trial run) and updated the
gate's doc comment.

That gate's own known-gap note said this only proves the boundary-is-also-
the-frame-edge case (2 columns, tile 1 = right edge), so also added
`a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact`
(`--tile-columns=2` -> `TileCols=4`, 256x64 = four 64px superblocks, loop
filter ON, no bypass of the flag): proves an *interior* tile boundary and the
`>2 columns` case in one shot. 20/20 pixel-exact, 0 refusals.

With both proven, lifted the `cols > 2` and `cols > 1 && loop_filter_on`
refusals from `decode_stream` (`crates/ec-av1/src/stream.rs`) -- they were,
per r5's note, the same underlying `PlaneBuf` reach-bound gap, now closed on
both fronts. Updated `refusal_inventory.rs`'s pinned list to match (both
strings removed). Tile *rows* stay refused (`tile_info.rows > 1`) --
unproven, untouched.

Lifted refusals (verbatim, removed from `stream.rs` and
`refusal_inventory.rs`'s pinned list):
- "a frame with more than two tile columns (a non-last column's right-edge reach bound past column 0 is unimplemented)"
- "a multi-tile frame with loop filtering enabled (deblocking crosses tile boundaries by spec default; PlaneBuf's tile origin does not clip a non-last column's right-edge reach)"

Committed 325e8e7.

## Stage 3 -- NOT STARTED
Tile rows, non-uniform tile spacing, several tile-group OBUs per frame, and
`context_update_tile_id != 0` on a non-tile-0 stream: none attempted this
round (turn budget). The `tile_info.rows > 1` refusal is untouched and still
guards tile rows correctly (no false claim made about it).
deferred: tile-row gate + lift, non-uniform tile spacing, multi-tile-group
OBU handling, `context_update_tile_id != 0` proof -- unblocks with a fresh
turn budget; recipe likely `--tile-rows=1` mirroring this round's
`--tile-columns` approach, gated the same way (bypass gate first, then
`decode_stream`-level gate, then lift).

## Files changed
- `crates/ec-av1/src/decode.rs` -- `TILE_HITS` increment added to the inter
  per-tile loop (was silently missing, made the stage-1 gate's own assert
  meaningless until fixed)
- `crates/ec-av1/src/stream.rs` -- 3 new gates (inter+2-col,
  4-col+loopfilter-on, plus doc updates on the 2 pre-existing gates), the
  `cols > 2` and multi-tile-loop-filter refusals removed from
  `decode_stream`, `loop_filter_on` local removed (now unused)
- `crates/ec-av1/src/refusal_inventory.rs` -- 2 pinned strings removed to
  match

## Commits
- bd1c01f test(av1): gate r6's inter tile-loop lift -- real >1-tile-column INTER stream
- 325e8e7 feat(av1): lift >2-tile-column and multi-tile-loop-filter refusals

Both `cargo check -p ec-av1 --tests` clean and the scoped subset
(`tile mvstack refusal_inventory gate_coverage`, `EC_AV1_REQUIRE_AOMENC=1`)
green: 78/0 after stage 2 (was 77/0 after stage 1). `refusal_inventory`'s own
"capability_claims_are_declared_not_scattered" and
"the_decode_path_refuses_exactly_the_listed_cases" tests both pass, so the
pinned-list edits match the live refusal set exactly.

Never pushed, never merged, never touched main, worked only inside
edith_codecs-tiles.
