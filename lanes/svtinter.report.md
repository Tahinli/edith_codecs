# lane-svtinter — the palette colour-index map read past the frame edge

Class: **map-read-past-frame-edge** — a syntax loop whose extent the spec
clamps to the block's ON-SCREEN part, read at the full block size, so the
decoder consumes symbols the encoder never wrote.

## First divergent symbol

Stream `smptebars=s=384x152:r=12,drawgrid=w=8:h=8:t=1:c=black` through
`libsvtav1 -preset 8 -crf 30 -svtav1-params lp=1:screen-content-mode=1`,
frame 0 (the KEY frame — the defect is not inter-only; lane-svt1's
"key frames are exact" held only for its own kept stream).

* Pixels: first wrong luma sample at row 128, col 296; everything after it in
  the raster diverges (1599 luma + 28 chroma in frame 0, ~80k per inter
  frame).
* Partitions agree with an instrumented aomdec (`EC_TRACE`) through the whole
  frame up to `mi_row=32 mi_col=72 bsize=BLOCK_32X32 PARTITION_NONE`, which is
  exactly the block covering pixel (128, 288).
* Mode ladder agrees symbol for symbol into that block (`EC_TRACE_MODE_STEP`,
  `EC_PALSYN`): `skip=1`, `y_mode=DC`, `uv_mode=DC`, `palette_y_mode=1`,
  `palette_y_size=3`, colours `[7,16,24]`, `rng=39432` on both sides.
* **First divergent symbol**: the colour-index map's wavefront, diagonal
  `i = 24`. aomdec's last symbol on that diagonal is `row=23 col=1` and it
  moves on to `row=0 col=25`; ours reads one more, `row=24 col=0`, and every
  symbol after that is one position out of step (`rng` 42060 vs 42068 from
  there on).

## Root cause

The block is 32x32 at mi row 32 of a 38-mi-tall frame: only 24 of its 32 rows
are inside the mi grid. Spec 5.11.50 `palette_tokens` /
`av1_get_block_dimensions` (blockd.h:1512) read the colour-index map over
`onscreenWidth x onscreenHeight` only and REPLICATE the last on-screen column
and row over the rest (detokenize.c:71). `decode_color_index_map_wh` walked
the full `bw x bh` grid — its own doc comment asserted the clamp was
unnecessary ("palette blocks ... never straddle the frame's true edge").

Both reported failure kinds are this one defect: the extra symbols either
shift the rest of the tile (~200k-sample misses) or eventually make a coeff
Golomb tail read as absurdly long ("a Golomb tail longer than this decoder
reads" — a desync symptom, never a reader bound, as the standing class says).

## Fix

`crates/ec-av1/src/decode.rs`:

* `palette_onscreen(mi_r, mi_c, bw, bh, fctx)` — the on-screen extent in luma
  pixels, from `fctx.seg_mi_dims` (set once per frame for every frame).
* `palette_onscreen_uv(luma, on, chroma)` — the same carried into chroma,
  keeping the `is_chroma_sub8` +2 bump in step with the caller's own
  `(bw / 2).max(4)`.
* `decode_color_index_map{,_wh}` take `(onscreenWidth, onscreenHeight)`, read
  the wavefront over that corner at the full-block stride, then replicate the
  last on-screen column and row (libaom's own two extension loops).

Same-shape sweep — all five reader call sites take the clamp: key-frame square
(`read_intra_mode`), rect strip (`read_intra_mode_rect`), intra-in-inter rect
(`decode_intra_rect_in_inter`), inter block intra (`decode_inter_block`), 8x8
inter leaf (`decode_inter_block8`), luma and chroma each.

## Pass table

Every decode compares EVERY plane of EVERY frame against `ffmpeg -pix_fmt
yuv420p` (harness asserts our decode exited 0 and produced N frames first).

| streams | before | after |
|---|---|---|
| fresh sweep: `smptebars,drawgrid` x {384x152, 320x184, 640x366} x crf {20,30,35,45}, 4 frames | 8 exact, 2 DIFF (22397/1599 samples in frame 0 alone), 3 REFUSED (Golomb tail) — 12 total | **12/12 exact** |
| kept `$HOME/.cache/svt1-keep/w3/*.obu` (180 streams) | (lane-svt1: 2 refusals + 6 x ~200k-sample misses among the 8 that reach its arm) | **180/180 exact** |

No refusal string survives on any stream in either set.

## Witness

`stream::tests::an_svt_screen_palette_map_cut_by_the_frame_edge_decodes_exactly`
BUILDS its stream in-test (the recipe above at crf 30), SKIPs with a printed
reason when ffmpeg/libsvtav1 is missing, asserts the new
`decode::palette_onscreen_cut_hits()` counter is non-zero (so it cannot pass
on a stream that never cuts a palette map) and compares all three planes of
all four frames against ffmpeg.

* RED (reader reverted to the full-grid walk, counter kept): `test result:
  FAILED. 0 passed; 1 failed`, panic `frame 0 luma vs ffmpeg`.
* GREEN: `test result: ok. 1 passed`.

## Open

* `open|` The WRITE side has the same shape: `tile.rs:3457
  write_color_index_map` walks `side x side` with no on-screen clamp
  (callers at 3498 / 3908 / 3913). Reachable only when our own encoder codes
  a palette block straddling the mi grid (screen-content mode at a frame size
  whose bottom/right block is cut) — not fixed here because a writer change
  moves the encoder pins, which this lane must leave byte-identical.
