VERDICT: PASS -- r9's committed WIP compiles and is correct (verified, no code change needed); both charter items (several tile-group OBUs, non-uniform tile spacing) were staleness-checked and found already-working decode capabilities, each closed with a real-aomenc gate rather than new decode machinery.

## Stage 0 -- check r9's in-flight edit (5524499)
`cargo check -p ec-av1 --tests` was clean (warnings only, no errors). r9's edit adds a `pending_header`/`pending_tiles` accumulator to `decode_stream` keyed off a standalone `OBU_FRAME_HEADER` (`show_existing_frame == false`), collecting `OBU_TILE_GROUP` payloads (normalising each `Tile::offset` to absolute via `tiles_base_offset`) until `cols * rows` tiles have arrived. It also fixed a real bug the old code had: the `FrameHeader` arm fired unconditionally for every header, so a standalone frame header used to be misrouted into the `show_existing_frame` DPB-slot lookup. Ran the gate the edit added
(`a_real_aomenc_stream_with_several_tile_group_obus_decodes_pixel_exact`, `--num-tile-groups=4`, 20 seeds): 20/20 pixel-exact, 0 refusals, `saw_multiple_tile_group_obus` asserted true (real aomenc streams do split into >1 `OBU_TILE_GROUP`). Nothing to fix or revert -- item 1 was already fully done by r9, just unverified. No commit needed here (working tree was already clean after the check).

## Item 2 -- non-uniform tile spacing (`uniform_spacing_flag == 0`, spec 5.9.15)
Staleness-checked first, per charter. `decode.rs`'s tile loop (`decode_key_frame_tile_with_cdfs` / the inter-frame equivalent) already walks `tile_info.mi_col_starts`/`mi_row_starts` generically -- these arrays are populated identically by `read_tile_info` (`ec-av1-syntax/src/frame.rs`) for both the uniform and non-uniform branches, and nothing in `decode.rs` recomputes tile bounds from `cols_log2`/`rows_log2` (which would assume uniform spacing). Grepped the whole crate: the only "non-uniform tile spacing is not written" refusal is `frame.rs`'s OBU *writer* (`crates/ec-av1/src/frame.rs:79-82`), an unrelated encode/round-trip path this charter item does not touch. No decode-side refusal exists at all -- same shape as r8's `context_update_tile_id` finding.

**Making aomenc emit it was the actual work this round.** `--tile-columns`/`--tile-rows` always take libaom's `uniform_spacing = 1` branch (`av1/encoder/encoder.c` `set_tile_info`); `--auto-tiles` only reaches the non-uniform balancing path when `g_threads >= 2` (`av1_cx_iface.c` `set_auto_tiles`), which this charter's mandatory `--threads=1` rules out. Read `av1_cx_iface.c`'s `set_tile_info` (lines ~370-430) and found the one CLI surface that reaches the `else { tiles->uniform_spacing = 0; ... }` arm: `--tile-width=<sb-list>`/`--tile-height=<sb-list>` (`arg_defs.c` `.tile_width`/`.tile_height` -- registered as real flags, just not printed by `aomenc --help`). Hand-verified live before writing the gate: `aomenc --tile-width=1,3 --tile-height=1 --sb-size=64 --threads=1` on a 256x64 source produced a real 430-byte OBU stream.

New gate `a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact` (`crates/ec-av1/src/stream.rs`): probes the parsed `FrameHeader`/`Frame` OBU's `tile_info.uniform_spacing` directly via `Av1Parser` and hard-asserts it's false at least once (`saw_non_uniform_spacing`), plus asserts `cols > 1 || rows > 1` so the fixture is proven to actually exercise more than one tile. 20/20 pixel-exact vs ffmpeg, 0 named refusals. Committed 490ddb3.

## Verification
- `cargo check -p ec-av1 --tests`: clean (warnings only).
- Scoped suite (`tile`, `mvstack`, `refusal_inventory`, `gate_coverage`, `EC_AV1_REQUIRE_AOMENC=1`): 51+29+3+2 = 85/0 after the non-uniform-spacing commit.
- Full `stream::` module (`cargo test -p ec-av1 --lib stream::`, `EC_AV1_REQUIRE_AOMENC=1`): 52 passed, 6 pre-existing ignored (pinned-fixture tests that read paths outside the repo, untouched by this round), 0 failed.
- `refusal_inventory.rs`'s `the_decode_path_refuses_exactly_the_listed_cases` and `capability_claims_are_declared_not_scattered` both pass unmodified -- the one new error string r9's edit introduced (`"AV1 decode_stream: a tile group OBU with no preceding frame header"`) uses `Error::corrupt(...)`, not `unsupported(...)`, so it is correctly out of that inventory's scope (a malformed-stream error, not an unimplemented-capability refusal) and needed no pinning.

## Fallback inventory (r9 charter's "if both turn out to be closed already" item)
Both items closed this round with real gates, not by discovering them pre-closed, so the fallback sweep is only a light note, not the deliverable:
- **MV stack / neighbour context**: already tile-boundary-aware -- `neighbours.start_tile(mi_row0, mi_col0, mi_col1)` is called per-tile at both the key-frame (`decode.rs:5549`) and inter-frame (`decode.rs:10512`) tile loop entry, resetting neighbour state at each tile's own edge (spec 5.11.2's per-tile `clear_left_context`/`clear_above_context` shape). Proven live by every multi-tile gate in this file (two/four tile rows/cols, this round's two new gates) being pixel-exact.
- **Loop-restoration stripe boundaries**: not a live tile gap -- loop restoration is refused unconditionally today (`"a frame with loop restoration enabled (this decoder never reads the per-unit lr symbols)"`, pinned in `refusal_inventory.rs`), so stripe-boundary tile interaction is unreachable code, not an untested tile assumption.
No further tile-shaped gaps found worth naming this round.

## Files changed
- `crates/ec-av1/src/stream.rs` -- new gate `a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact` (180 lines). r9's earlier several-tile-group-OBUs edit (5524499) verified correct, unchanged.

## Commits
- 490ddb3 test(av1): non-uniform tile spacing gate -- decode already generic, staleness-checked

Never pushed, never merged, never touched main or sibling worktrees. `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles` used for every build/test. Both charter items are CLOSED; no deferred work.
