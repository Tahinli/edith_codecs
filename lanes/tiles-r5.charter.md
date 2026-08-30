# lane-tiles r5 — inter-frame multi-tile, then the right-edge reach bound

At the lane tip (main merged in — r4 is merged to main at 53b319b). Read
`lanes/tiles-r4.report.md`.

## State
Key frames with two tile columns decode through `decode_stream` itself,
20/20 pixel-exact, tile count hard-asserted. Four scoped refusals remain:
inter frames with >1 tile, >1 tile row, >2 tile columns, and multi-tile with
loop filtering on.

Fact r4 established: AV1 has NO `loop_filter_across_tiles` bit (unlike HEVC) —
deblocking crosses tile boundaries unconditionally. So the loop-filter refusal
and the >2-columns refusal are the SAME underlying gap: `PlaneBuf`'s tile origin
clips left/top reach only, and a non-last column's right-edge reach bound is
unimplemented. Fixing that one thing lifts both.

## Order
1. `decode_inter_frame_tile_with_cdfs` takes `tiles: &[&[u8]]` and grows the
   same per-tile loop the key-frame path has (mirror
   `decode_key_frame_tile_with_cdfs`'s loop), then thread the tile origin into
   `mvstack.rs` so an MV candidate scan cannot reach across a tile boundary.
   That lifts the inter-frame refusal. Gate it on a real inter stream. COMMIT.
2. The right-edge reach bound in `PlaneBuf` — lifts the >2-columns and the
   loop-filter refusals together. Re-run the two-tile gate with
   `--loopfilter-control=0` REMOVED; it must still be pixel-exact. COMMIT.
3. Tile rows, non-uniform spacing, several tile-group OBUs per frame, and
   `context_update_tile_id != 0` proven with a stream where it is not tile 0.

Gate rules: `EC_AV1_REQUIRE_AOMENC=1`; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source`; aomenc `--threads=1 --row-mt=0
--sb-size=64` (this decoder hardcodes 64px superblocks); firing counts
HARD-asserted. And remember r3's false PASS: `Tile::offset` is relative to the
slice `parse_obu` was handed — forgetting `obu_offset` decodes flat DC garbage
that compares EQUAL instead of erroring.

Merge note: main has `refusal_inventory.rs`, which pins all four of your tile
refusal strings. Report every one you remove or reword, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. End with `lanes/tiles-r5.report.md`, VERDICT on line 1.
