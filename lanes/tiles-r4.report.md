VERDICT: PARTIAL -- steps 1-3 done and committed (9962fb1); step 4 (tile rows,
non-uniform spacing, inter frames, several tile-group OBUs,
context_update_tile_id != 0) not attempted, out of scope this round.

## Step 1: `loop_filter_across_tiles_enabled`
Grepped `crates/ec-av1-syntax/src/frame.rs`'s `read_loop_filter_params` (spec
5.9.11) end to end: the field does not exist, and it isn't a gap in this
crate's parser -- AV1's spec `loop_filter_params()` has no such syntax
element at all (unlike HEVC's `loop_filter_across_tiles_enabled` PPS flag,
which the charter's naming echoes but AV1 has no equivalent of). Deblocking
crosses tile boundaries unconditionally whenever `loop_filter.level` is
non-zero; there is no per-stream bit that turns it off. That answers the
charter's question: the refusal cannot be conditioned on a header field,
only on whether loop filtering is on at all (`level.iter().any(|&l| l != 0)`)
combined with the already-known `PlaneBuf` gap (tile origin clips a tile's
own left/top reach, never a non-last column's right-edge reach).

## Step 2: thread real `tile_bufs` through `decode_stream`
`crates/ec-av1/src/stream.rs`'s key-frame call site now builds
`tile_bufs: Vec<&[u8]>` from every element of the OBU's own `tiles: Vec<Tile>`
(the same `obu_offset + t.offset` correction r3's gate proved necessary) and
passes `&tile_bufs` to `decode_key_frame_tile_with_cdfs` instead of the old
`&[tile_bytes]` (tile 0 only). The inter-frame call site still passes the
single `tile_bytes` it always did -- `decode_inter_frame_tile_with_cdfs`
takes one `&[u8]`, not a tile list, and has no per-tile loop at all; widening
that signature is real work left for step 4 ("inter frames (tile origin into
mvstack.rs)"), not attempted this round.

## Step 3: lift the refusal, scoped; re-run r3's gate through `decode_stream`
The blanket `cols > 1 || rows > 1` refusal is now four scoped refusals, only
firing for what is genuinely still unproven:
- inter frame with `cols > 1` (no per-tile loop on that path at all)
- `rows > 1` (tile rows never tested)
- `cols > 2` (non-last column's right-edge reach bound past column 0 is
  unimplemented -- r3's own documented gap, matters starting at 3 columns)
- multi-tile frame with `loop_filter.level` non-zero anywhere (deblocking
  crossing gap from step 1)

A key frame with exactly 2 tile columns, 1 tile row, and loop filtering off
now decodes through `decode_stream` itself. New test
`a_real_aomenc_stream_with_two_tile_columns_decodes_through_decode_stream`
(`stream.rs`, next to r3's gate) runs the identical aomenc recipe through
`decode_stream(&stream)` instead of the hand-parsed bypass: 20/20 pixel-exact,
`tile_hits()` delta `> 1` every attempt, `pictures.len() == 1`.

Did not drop `--loopfilter-control=0` from either gate -- step 1 established
that would hit the still-unfixed `PlaneBuf` cross-tile-column reach gap
(the new refusal explicitly names this and blocks it before it can silently
miscode), not something worth testing-then-reverting this round.

## Full suite
`cargo check -p ec-av1 --tests`: clean (only pre-existing doc-lint warnings).
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: 234 passed, 0
failed, 17 ignored, 146.97s -- both tile gates included, nothing else moved.

## Refusal strings

Removed (verbatim, was the single blanket refusal):
> a frame with more than one tile (this decoder only ever decodes tile 0)

Added, four in its place (all in `stream.rs`, all `Error::unsupported("AV1
decode_stream", ...)`):
> an inter frame with more than one tile (the inter tile-decode path has no per-tile loop, only key frames do)

> a frame with more than one tile row (only tile columns are proven pixel-exact so far)

> a frame with more than two tile columns (a non-last column's right-edge reach bound past column 0 is unimplemented)

> a multi-tile frame with loop filtering enabled (deblocking crosses tile boundaries by spec default; PlaneBuf's tile origin does not clip a non-last column's right-edge reach)

## Still open (per charter step 4, unattempted)
Tile rows, non-uniform tile spacing, inter-frame multi-tile decode (needs
`decode_inter_frame_tile_with_cdfs` to grow the same per-tile loop
`decode_key_frame_tile_with_cdfs` already has, plus tile origin threaded into
`mvstack.rs` for inter prediction/MV candidates that reach across a tile
boundary), several tile-group OBUs per frame, `context_update_tile_id != 0`
(this decoder still only ever keeps tile 0's adapted CDF table as the frame's
output regardless of what the header names), and the 3+-column right-edge
reach bound in `PlaneBuf` itself (currently hard-refused via the `cols > 2`
check above rather than silently miscoding).
