VERDICT: PARTIAL -- step 1 (confirm) and step 2 (prove) done and committed; step 3
(lift the refusal) not attempted, out of turn budget.

## Step 1: confirmation run
`cargo check -p ec-av1 --tests` clean. `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1
--lib -j4`: 232 passed, 0 failed, 17 ignored, 207.55s -- matches the corrected charter
note exactly. Nothing to commit (tree was already clean at d1c2071).

## Step 2: prove the per-tile loop on a real two-tile-column stream
Added `a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact` to
`crates/ec-av1/src/stream.rs` (committed 1648317). It:
- builds a 128x64 aomenc key frame with `--tile-columns=1 --tile-rows=0
  --sb-size=64 --loopfilter-control=0`, 20-seed sweep (structural tile split
  isn't content-dependent, but this decoder's pre-existing chroma
  smooth/paeth refusal is -- accepted as a named refusal like every sibling
  gate does for unrelated capabilities);
- parses the stream itself (not `decode_stream`, whose `cols > 1 || rows > 1`
  refusal is exactly what this gate runs ahead of) to get the real `Vec<Tile>`
  `Av1Parser::parse_obu` already produces;
- calls `decode_key_frame_tile_with_cdfs(&[tile0, tile1], &header.tile_info,
  ...)` directly;
- hard-asserts `tile_hits()` delta `> 1` and pixel-exactness against ffmpeg.

Result: 20/20 pixel-exact matches, 0 refusals, tile_hits > 1 every time.
`cargo check -p ec-av1 --tests` clean after.

Two things needed beyond the charter's one-liner:
- `--sb-size=64`: this decoder hardcodes 64px superblocks everywhere
  (SB_MI=16); aomenc's own default is 128px SBs, which makes 128x64 exactly
  one superblock -- nothing for `--tile-columns=1` to split, `cols` came back
  1 without this.
- `--loopfilter-control=0`: deblocking crosses tile boundaries by spec
  default, and `PlaneBuf`'s `tile_x0`/`tile_y0` only ever clip a tile's own
  left/top reach (the exact known gap r2 flagged) -- a non-last column's
  right-edge deblock read into the next tile wrong without this. Sidestepped
  on purpose; not fixed, deblocking is not what this gate proves.

Also caught and fixed a real bug in my own first draft while wiring the test:
`Tile::offset` is relative to the buffer `parse_obu` was handed, not absolute
in `stream` -- forgetting to add `obu_offset` back (the way `decode_stream`
already does at its own tile-extraction site) fed the decoder garbage-offset
bytes, which silently decoded as an all-skip flat DC block per tile (128/127
uniform) instead of erroring -- a quiet false negative, not a crash. Fixed
before committing; the committed test carries a comment on this.

## Step 3: not attempted
Lifting the `cols > 1 || rows > 1` refusal in `decode_stream` needs more than
deleting the `if`: the call site currently hardcodes `&[tile_bytes]` (single
tile, `tiles.first()`), so it needs building the full `tile_bufs: Vec<&[u8]>`
the same way this gate's test code now does (including the `obu_offset` fix),
threaded through for BOTH `decode_key_frame_tile_with_cdfs` and
`decode_inter_frame_tile_with_cdfs`. More importantly, real streams
`decode_stream` sees do NOT get `--loopfilter-control=0` for free -- lifting
the refusal for general (non-test-controlled) multi-tile streams would need
either the right-edge reach gap actually fixed in `PlaneBuf`, or deblocking
proven to not cross tile boundaries in the streams under test
(`loop_filter_across_tiles_enabled` -- unread, not checked this round).
Doing this without checking would very likely reintroduce the exact luma
mismatch this round diagnosed (uncaught first-draft bug notwithstanding) for
any real multi-tile stream with loop filtering on. Left for the next round;
the intra-only case specifically (per charter step 3) still needs the deblock
question answered first.

## Refusal strings
None added, renamed or removed this round -- the multi-tile refusal in
`stream.rs` is untouched. (Step 3, when attempted, will need to report this
line verbatim before deleting it, per the charter's merge note.)
