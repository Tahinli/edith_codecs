VERDICT: PASS -- merged main, tile-rows refusal closed the r7 way (bypass gate, decode_stream gate loop-filter ON, then lift), plus a bonus finding: context_update_tile_id != 0 was already working and undocumented, now proven and correctly described. All scoped suite green throughout.

## Merge
`git merge main` at d145dea (main was at 92d8beb, a fast-forwardable dead-binding cleanup on top of the already-merged lane-tiles history plus the `bit_depth != 8` refusal from a sibling lane) resolved with no conflicts (`ort` strategy, clean). Suite (`tile mvstack refusal_inventory gate_coverage`, `EC_AV1_REQUIRE_AOMENC=1`) 80/0 immediately after. Committed ef641d9.

## Stage 1 -- bypass gate (tile rows, below decode_stream)
`a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact` (`crates/ec-av1/src/stream.rs`): 64x128, `--tile-rows=1` (`TileRows=2`), `--sb-size=64`, calls `decode_key_frame_tile_with_cdfs` directly since `decode_stream` still refused `tile_info.rows > 1` at this point. Loop filter left ON (no `--loopfilter-control=0`) per the charter -- `deblock_plane` runs once over the whole decoded picture with no tile-axis distinction, and the per-tile loop's row math (`tile_num / tile_info.cols`, `mi_row_starts`, `set_tile_origin`'s `y0`/`y1`) is exactly symmetric with the column path r7 already proved. 20/20 pixel-exact vs ffmpeg, 0 named refusals, hard `tile_hits > 1` assert. Confirmed via header parse that aomenc really emitted `rows=2` before trusting any match.

Committed d6b4d51.

## Stage 2/3 -- decode_stream gate + refusal lift (same commit)
Lifted `decode_stream`'s `tile_info.rows > 1` refusal and, in the same commit, added `a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream` (same fixture, calls `decode_stream` itself, hard `tile_hits > 1` assert) -- 20/20 pixel-exact, 0 refusals. Updated `refusal_inventory.rs`'s pinned list to drop the matching string (`capability_claims_are_declared_not_scattered` and `the_decode_path_refuses_exactly_the_listed_cases` both pass, confirming the pinned list now matches the live refusal set). This mirrors r6/r7's ordering exactly: the decode_stream-level gate can only pass once the refusal it exercises is gone, so lift and gate ship together.

Committed 491af97.

## Extra: interior tile-row boundary (mirroring r7's four-column rigor)
Two tile rows only exercises the edge case (row 1 is also the frame's bottom edge), same shape as r6's original two-column gate before r7's four-column gate proved an interior boundary. Uniform tile spacing is always a power of two, so the smallest row count with a genuine interior boundary is 4 (`--tile-rows=2` -> `TileRows=4`). Added `a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact` (64x256, loop filter ON, through `decode_stream`) -- 20/20 pixel-exact, 0 refusals, hard `tile_hits > 2` assert.

Committed 8d73fc2.

## Bonus finding: stale refusal-claim comment (context_update_tile_id)
While writing the four-tile-row gate's doc comment I checked the charter's next-up item (`context_update_tile_id != 0`) against the actual code and found `decode_stream`'s multi-tile comment claiming a `context_update_tile_id != 0` refusal existed "further down" in the file -- grepped the whole crate for `context_update_tile_id` and no such refusal exists anywhere; `decode.rs` reads `tile_info.context_update_tile_id` generically (never hardcoded to tile 0) at both the key-frame and inter-frame tile loops' `result_cdfs = cdfs` sites. Probed live with an eprintln on the four-tile-row gate's own 20-seed sweep: aomenc's RD-driven tile-size heuristic picked `context_update_tile_id` in {0,1,2,3} across the runs (seed 42 -> 2, seed 48 -> 1, seed 55 -> 3, etc.), and every one of those 20 attempts was pixel-exact. So the capability was already fully working, just undocumented and unproven -- not a real gap, a stale comment (class [[stale-premise-lanes]]). Removed the debug eprintln, fixed the comment to describe the real (generic) behaviour, and added a hard `saw_nonzero_context_update_tile_id` assert to the four-tile-row gate so this proof can't silently regress unnoticed. This closes the charter's first "if turns remain" item without any code change to `decode.rs` -- it was already correct.

Committed 013cb58.

## If turns remain (unchanged from charter, genuinely open)
`context_update_tile_id != 0` is now CLOSED (see above, no code change needed, already correct). Still open, in charter order: several tile-group OBUs per frame, non-uniform tile spacing. Neither attempted this round (turn budget). Each is a refusal today (grep for `"a frame OBU with no tile group"` and non-uniform-spacing handling in `frame.rs`'s tile_info parse -- not investigated this round for staleness the way `context_update_tile_id` was, so do that check first before assuming either still needs real work).
deferred: several-tile-group-OBUs-per-frame gate + lift, non-uniform tile spacing gate + lift -- unblocks with a fresh turn budget; check both for the same "already-correct, comment-stale" possibility before building new decode machinery, the way this round found `context_update_tile_id` was.

## Files changed
- `crates/ec-av1/src/stream.rs` -- 3 new gates (two-tile-row bypass, two-tile-row through `decode_stream`, four-tile-row interior boundary), `tile_info.rows > 1` refusal removed from `decode_stream`, multi-tile doc comment updated (rows closed + `context_update_tile_id` stale-claim correction), hard assert added proving `context_update_tile_id != 0` fires and matches
- `crates/ec-av1/src/refusal_inventory.rs` -- 1 pinned string removed (tile rows) to match the lifted refusal

## Commits
- ef641d9 merge main into lane-tiles (r8 start)
- d6b4d51 test(av1): gate tile rows below decode_stream's refusal (bypass)
- 491af97 feat(av1): lift the tile-rows refusal, gated through decode_stream
- 8d73fc2 test(av1): interior tile-row boundary gate (four rows), mirroring r7's four-column gate
- 013cb58 docs+test(av1): drop the stale context_update_tile_id refusal claim, hard-assert it live

Scoped suite (`tile mvstack refusal_inventory gate_coverage`, `EC_AV1_REQUIRE_AOMENC=1`) 82/0 green after every commit in this round. `cargo check -p ec-av1 --tests` clean throughout.

Never pushed, never merged, never touched main, worked only inside edith_codecs-tiles. `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles` used for every build/test.
