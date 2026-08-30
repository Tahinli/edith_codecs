# lane-tiles r6 — finish the inter per-tile loop

At 1227517 — r5's work, committed verbatim by the orchestrator at its cap
MID-EDIT. Its own last words were that two shorthand `width,` / `height,`
struct-init blocks (around decode.rs:11661 `ref_y` and :11673 `y`) still needed
the same fix as the rest, so **it almost certainly does not compile**. That is
job one. `lanes/tiles-r5.charter.md` is still binding.

1. `cargo check`, finish r5's mid-edit sweep, then the full suite (baseline
   234/0 on this tree). COMMIT the moment it is green.
2. Finish stage 1: `decode_inter_frame_tile_with_cdfs` takes `tiles: &[&[u8]]`
   with the same per-tile loop the key-frame path has, and the tile origin
   threaded into `mvstack.rs` so an MV candidate scan cannot reach across a
   tile boundary. Lift the inter-frame multi-tile refusal, gated on a real
   inter stream with a hard-asserted tile count. COMMIT.
3. Then `PlaneBuf`'s right-edge reach bound for a non-last tile column — that
   single fix lifts BOTH the >2-columns refusal and the loop-filter one (AV1
   has no loop_filter_across_tiles bit; deblocking always crosses tiles). Re-run
   the two-column gate with `--loopfilter-control=0` REMOVED; it must stay
   pixel-exact. COMMIT.

Gate rules: `EC_AV1_REQUIRE_AOMENC=1`; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source`; aomenc `--threads=1 --row-mt=0
--sb-size=64` (this decoder hardcodes 64px superblocks). Firing counts are HARD
asserts. Main now FAILS the suite if a gate turns a decode error into a printed
SKIP — write the gate as an attempt loop requiring at least one decode.
And remember r3's false PASS: `Tile::offset` is relative to the slice
`parse_obu` was handed; forgetting `obu_offset` decodes flat DC garbage that
compares EQUAL instead of erroring.

Merge note: main is at 91a08e8 with `gate_coverage.rs` and
`refusal_inventory.rs`, which pin your four tile refusal strings among others.
Report every one you remove or reword, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP — three rounds on this lane have now ended mid-edit. End with
`lanes/tiles-r6.report.md`, VERDICT on line 1.
