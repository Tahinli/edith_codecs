# lane-tiles report

## What landed
Groundwork only, commit `1231cd5` on branch `lane-tiles`:
`crates/ec-av1/src/decode.rs`
- `Neighbours` gained `tile_row0_mi`/`tile_col0_mi` (spec `MiRowStart`/
  `MiColStart`), default `0`, and `start_tile(row0_mi, col0_mi, col1_mi)`
  (spec `decode_tile`'s `clear_above_context` over the tile's own column
  span; left-context reset per SB row was already unconditional and needed
  no change).
- Every availability check that compared a raw mi position against literal
  `0` now compares against the tile origin instead: `skip_txfm_ctx`,
  `tx_size_context`, `tx_size_context_rect`, and `decode_inter_block`/
  `decode_inter_block8`'s `has_above`/`has_left`.
- Behaviourally a no-op today: nothing calls `start_tile`, so the origin
  stays `(0, 0)` everywhere and every existing test is bit-identical.

The refusal at `crates/ec-av1/src/stream.rs:292-297` ("a frame with more
than one tile") is **unchanged and still fires** — it was not removed.

## Why it stops here (turn budget)
Recon (charter + call-site tracing across `decode.rs`, `mvstack.rs`,
`ec-av1-syntax/frame.rs`'s `TileInfo`/`parse_tile_group`) took the first
~35 tool calls to establish the real shape of the three hard parts. What
full removal of the refusal needs, none of which is done:
1. **Entropy per tile.** `decode_key_frame_tile_with_cdfs` /
   `decode_inter_frame_tile_with_cdfs` each take one `data: &[u8]` and run
   one `SymbolDecoder`/`cdfs` over the whole `for sb_r in 0..sb_rows { for
   sb_c in 0..sb_cols }` walk. This needs restructuring into a `for tile in
   &tiles` outer loop: fresh `SymbolDecoder` per tile's own byte range
   (`Tile::offset` is relative to the OBU, already handled correctly in
   stream.rs for tile 0 today), fresh `cdfs = initial_cdfs.clone()` per
   tile, sb range narrowed to `mi_row_starts[t]/SB_MI..mi_row_starts[t+1]/
   SB_MI` (same for cols), and only `context_update_tile_id`'s own
   end-of-tile `cdfs` returned as the frame's adapted table. `TileInfo`
   already carries everything this needs (`frame.rs:115`); the derivation
   at `frame.rs:1292` confirms `mi_col_starts`/`mi_row_starts` are always
   fully populated by the real parser, including the `cols==rows==1` case.
2. **Context clipping — partially wired, NOT complete.** This commit's
   `Neighbours` change covers the context-symbol reads it touched. Two
   things it does **not** cover:
   - `PlaneBuf::edges`/`edges_rect` (`decode.rs` ~2653-2696) still gate
     intra-prediction pixel reach on `x > 0`/`y > 0` — a block at a tile's
     own left/top edge would read real reconstructed pixels from the
     PREVIOUS tile (decoded earlier, sitting right there in the shared
     buffer) instead of treating them as unavailable, which is a real
     pixel-value bug per spec (AV1 tiles are independently decodable;
     intra prediction does not cross tile boundaries). Needs a
     `tile_x0`/`tile_y0` pair on `PlaneBuf` (luma-pixel and chroma-pixel
     scaled, i.e. `mi_col0*4`/`mi_col0*2`), set before each tile, checked
     in `edges`/`edges_rect` in place of `0`.
   - `mvstack.rs`'s `find_mv_stack_with_sign_bias` (the actual MV
     neighbour scan for every inter block) still hardcodes
     `mi_row > 0`/`mi_col > 0` for `max_row_offset`/`max_col_offset` and
     `found_above`/`found_left` (its own doc comment at line ~690 already
     flags this: *"clamped to the tile's near edge (single-tile-per-frame
     here, so that's just the frame edge at 0)"*). `find_samples`/
     `num_proj_ref` (`decode.rs` ~5488/5617, warp-sample gathering for
     `WARPED_CAUSAL`) have the same gap. Neither was touched this round.
   - Deblock/CDEF were confirmed (not just assumed) to run across the
     whole picture after all tiles decode (`apply_deblock`/`apply_cdef`
     calls sit after the SB loop, once, over the full `y`/`u`/`v`) —
     these need **no** tile clipping, matching the charter's note.
3. **Tile-group ordering.** Not started. `ObuKind::Frame(header, tiles)`'s
   `tiles: Vec<Tile>` already spans every tile in `tg_start..tg_end` for a
   *single* OBU (`parse_tile_group`, `ec-av1-syntax/src/frame.rs:1992`),
   which covers the common `--tile-columns`/`--tile-rows` aomenc case (one
   `OBU_FRAME` carries every tile). The separate multi-OBU case
   (`ObuKind::FrameHeader` followed by one or more standalone
   `ObuKind::TileGroup(Vec<Tile>)` OBUs) is not collected at all today —
   `decode_stream`'s `while pos < data.len()` loop only matches
   `ObuKind::Frame`/`FrameHeader` (for `show_existing_frame`) and silently
   `continue`s past everything else, including a bare `TileGroup` OBU.

No gate was added — the charter's mandatory gate asserts a tile-firing
counter `> 1` on real decoded output, which requires part 1 (entropy per
tile) to exist first; writing the gate against the untouched refusal would
either be vacuous or fail immediately.

## Remaining refusal strings (verbatim, unchanged)
- `"a frame with more than one tile (this decoder only ever decodes tile 0)"`
  — `stream.rs:294-296`, still fires for every `tile_info.cols > 1 ||
  tile_info.rows > 1` frame, intra or inter.

## Verification
`cargo test -p ec-av1 --lib` (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`,
`EC_AV1_REQUIRE_AOMENC=1`): 232 passed, 0 failed, 17 ignored, 174.28s —
baseline unchanged, no regression from the groundwork commit.

## Next lever (for whoever picks this up)
Land in the charter's own staged order, each its own commit:
1. Restructure `decode_key_frame_tile_with_cdfs` to accept
   `tiles: &[(u32 /* tile_num */, &[u8])]` + `&TileInfo` in place of a
   single `data: &[u8]`, wrap the existing SB double loop (currently
   `decode.rs` lines ~4547 header `for sb_r in 0..sb_rows` through its
   matching close ~4947) in `for &(tile_num, bytes) in tiles`, call
   `neighbours.start_tile(...)` (already written, unused) and set
   `PlaneBuf` tile pixel origins (not yet added) at each tile's start,
   narrow the sb range, and capture `cdfs` as the returned table only when
   `tile_num == tile_info.context_update_tile_id`. Update the one caller
   (`decode_key_frame_tile`, `decode.rs:4478`) and `stream.rs`'s key-frame
   branch to build the tile list instead of `tiles.first()`. Prove stage 1
   (two tiles, column split, all-intra) before touching anything else.
2. Same restructuring for `decode_inter_frame_tile_with_cdfs`, PLUS
   threading `tile_row0_mi`/`tile_col0_mi` into `find_mv_stack_with_sign_bias`
   and `find_samples`/`num_proj_ref` (both currently frame-relative-only).
3. Multi-tile-group OBU collection in `decode_stream`'s main loop
   (`ObuKind::FrameHeader` + trailing `ObuKind::TileGroup` accumulation
   before dispatch, mirroring the existing `pending: Option<FrameHeader>`
   field already on `Av1Parser` internally, though that field is private —
   check whether it's exposed or the collection has to happen in
   `decode_stream` itself against raw OBU events).
4. `context_update_tile_id != 0` pin test once tables save correctly.
5. The mandatory gate: `--tile-columns=1 --tile-rows=1` aomenc recipe
   (log2 values), thread-local `TILE_HITS` counter (not yet added)
   incremented once per tile actually decoded, hard-asserted `> 1`.
