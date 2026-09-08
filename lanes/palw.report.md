# lane-palw — the palette colour-index map WRITTEN past the frame edge

Class: **writer-phantom-units-past-edge** (previous instance: the residual
transform-unit writer coding TUs past the right frame edge, lane-av1straddle).
The reader half was lane-svtinter's `map-read-past-frame-edge`; this is its
mirror on our own encoder.

## Defect

Spec 5.11.50 `palette_tokens` codes a block's colour-index map over its
`onscreenWidth x onscreenHeight` corner ONLY and replicates the last on-screen
column/row outward (detokenize.c:71). `tile::write_color_index_map` walked the
full `side x side` grid, so every palette block our encoder coded straddling
the frame's right/bottom mi edge carried symbols a conformant decoder never
reads: libaom/dav1d/ffmpeg desync from that block on. Our own trial decode
agreed with itself because `FrameCtx::for_encoder` never runs
`set_segmentation`, so `decode::palette_onscreen` read `(0, 0)` mi dims and
took the `corner-cut:` branch that mirrored the unclamped writer.

## Fix (commit `16f7d271`)

* `encode.rs` (both frame encoders, beside their `true_width`/`true_height`):
  publish the header's `(mi_rows, mi_cols)` onto the encoder's `FrameCtx` and
  arm them on every tile writer (`tile::arm_frame_mi_dims`, beside
  `arm_screen`).
* `encode.rs` palette search, luma and chroma: the searched map is folded to
  the on-screen corner through the decoder's own
  `decode::palette_replicate` before it is priced, predicted from and
  committed -- so the map priced, the map reconstructed and the map written
  are one map. The colour search itself still runs over the full block.
* `tile.rs`: `write_color_index_map` takes `(onscreenWidth, onscreenHeight)`
  and walks the wavefront over that corner at the full-block stride;
  `palette_bits`/`palette_uv_bits` price the same corner;
  `write_palette_syntax` derives it with `decode::palette_onscreen_dims` (the
  reader's own function) and `decode::palette_onscreen_uv` for chroma.
  Counter `palette_cut_maps_written()`.
* `decode.rs`: the `corner-cut:` mirror-the-writer branch is GONE -- both
  sides now clamp everywhere. `palette_onscreen_dims`/`palette_replicate` are
  shared by the two sides so they cannot drift.

## Witness

`encoder::tests::a_palette_block_cut_by_the_frame_edge_decodes_exactly_through_ffmpeg`
(encoder.rs). Drives the ENTRY SURFACE (`Av1Encoder`, what the editor's export
calls) on a 384x152 8-px two-tone screen card: 152 rows = 38 mi, so the bottom
superblock row's 16x16 blocks at y=144 and 32x32 blocks at y=128 hang off the
mi grid. Asserts `tile::palette_cut_maps_written() > 0` (12 here), decodes
through `decode_stream` AND ffmpeg and compares EVERY plane of every frame.

* RED (`write_color_index_map` forced back to the full grid, everything else
  as shipped): `our decoder: Unsupported { what: "AV1 tile", why: "a Golomb
  tail longer than this decoder reads" }` -- `test result: FAILED. 0 passed;
  1 failed`.
* GREEN: `test result: ok. 1 passed`, `12 cut colour-index maps written`.

`screen_card`'s 4-px bands were tried first and fire 9 palettes, NONE of them
cut (`cut 0`) -- the row would have passed blind (class gate-blind-to-feature),
which is what the counter assert catches.

## Conformance sweep (our encoder -> ffmpeg, all three planes, 3 frames)

Two screen sources, four sizes whose mi grid cuts blocks, two quantizers.
`cut maps` is `palette_cut_maps_written()` for that point: every row really
exercises the fixed path.

Checkerboard card (8-px two-tone):

| size | q | bytes | cut maps | ours vs ffmpeg |
|---|---|---|---|---|
| 384x152 | 60 | 286 | 12 | EXACT |
| 384x152 | 150 | 288 | 12 | EXACT |
| 320x184 | 60 | 256 | 5 | EXACT |
| 320x184 | 150 | 257 | 5 | EXACT |
| 640x366 | 60 | 706 | 10 | EXACT |
| 640x366 | 150 | 707 | 10 | EXACT |
| 1000x562 | 60 | 1499 | 24 | EXACT |
| 1000x562 | 150 | 1500 | 24 | EXACT |

`smptebars=s=WxH,drawgrid=w=8:h=8:t=1:c=black` (lavfi):

| size | q | bytes | cut maps | ours vs ffmpeg |
|---|---|---|---|---|
| 384x152 | 60 | 1398 | 2 | EXACT |
| 384x152 | 150 | 1397 | 6 | EXACT |
| 320x184 | 60 | 1488 | 1 | EXACT |
| 320x184 | 150 | 1406 | 2 | EXACT |
| 640x366 | 60 | 2708 | 2 | EXACT |
| 640x366 | 150 | 2596 | 0 | EXACT |
| 1000x562 | 60 | 3924 | 6 | EXACT |
| 1000x562 | 150 | 3731 | 2 | EXACT |

Before the fix, the same recipe refuses (the RED run above is 384x152 of the
first table).

## Same-shape sweep, WRITER side

Every per-cell syntax family this writer emits, at a block cut by the mi grid:

| family | writer site | state |
|---|---|---|
| palette colour-index map, luma + chroma | tile.rs `write_color_index_map` | **fixed here** |
| residual transform units, luma | tile.rs:4367 `tu_mi.0 >= neighbours.mi_rows \|\| tu_mi.1 >= neighbours.mi_cols` | clipped (lane-av1straddle) |
| residual transform units, chroma | tile.rs:2612 `bound_h`/`bound_w` with libaom's `ROUND_POWER_OF_TWO` round-up | clipped |
| var-tx split units | tile.rs:4520 -- the encoder only picks depth 1 for a block wholly inside the frame, `debug_assert`ed per unit | clipped |
| partition | tile.rs:184 / 2972 / 3052 `has_cols`/`has_rows` inferred split at 128, 64 and 32 | clipped |
| segment ids | none: this encoder writes no segmentation map (`write_segment_id` exists only in decode.rs) | n/a |
| CfL, intrabc | one alpha pair / one vector per block, no per-cell loop | n/a |
