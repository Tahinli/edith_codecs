# lane-tiles r4 — lift the multi-tile refusal

At 477f972. Read `lanes/tiles-r3.report.md`.

## State
The per-tile loop works and is proven: gate
`a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact` (stream.rs
~5484) decodes 20/20 pixel-exact with `tile_hits()` hard-asserted `> 1`, by
calling `decode_key_frame_tile_with_cdfs(&[tile0, tile1], ...)` directly and
bypassing the still-live refusal in `decode_stream`.

Facts r3 paid for: `--sb-size=64` is required (this decoder hardcodes 64px
superblocks; aomenc's default 128px makes a 128x64 frame a single superblock
with nothing to split). `--loopfilter-control=0` is required *for now* because
deblocking crosses tile boundaries by spec default while `PlaneBuf`'s
`tile_x0`/`tile_y0` clip left/top reach only. And a trap that produced a FALSE
PASS: `Tile::offset` is relative to the slice handed to `parse_obu`, not
absolute — forgetting to add `obu_offset` back decoded flat all-skip DC garbage
that compared "equal" instead of erroring.

## Your job
1. Read `loop_filter_across_tiles_enabled` in `ec-av1-syntax`'s header parse —
   grep it first, it may not be parsed at all. Real multi-tile streams reaching
   `decode_stream` get no `--loopfilter-control=0` escape hatch, so this
   question has to be answered before the refusal comes off.
2. Thread a real `tile_bufs: Vec<&[u8]>` through `decode_stream`'s key-frame AND
   inter-frame tile-decode call sites (both currently hardcode
   `&[tile_bytes]`), mirroring r3's `obu_offset`-corrected extraction.
3. Lift the `cols > 1 || rows > 1` refusal for the cases you can prove, and
   refuse the rest BY NAME with an accurate string. Re-run r3's gate through
   `decode_stream` itself, and drop `--loopfilter-control=0` from it once the
   crossing question is resolved to see whether it still holds. COMMIT.
4. Then tile rows, non-uniform spacing, inter frames (tile origin into
   `mvstack.rs`), several tile-group OBUs, and `context_update_tile_id != 0`.
   Also still open: a non-last tile column's right-edge reach bound is
   unimplemented (fine for 2 columns, wrong for 3+).

Merge note: main is at 06d29ee with `gate_coverage.rs` and `refusal_inventory.rs`,
which pin the aomenc tools no gate exercises and every decode-path refusal
string — including the multi-tile refusal you are here to remove. Report every
refusal string you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. End with `lanes/tiles-r4.report.md`, VERDICT on line 1.
