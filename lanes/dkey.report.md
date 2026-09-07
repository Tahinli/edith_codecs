# lane-dkey — palette state never cleared by sub-8x8 / inter blocks

## Root cause (closed, measured)

`crates/ec-av1/src/decode.rs`: the mi-granular above/left **palette bands**
(`Neighbours::above_palette_size/colors`, `left_palette_*`) were written ONLY by
the >=8x8 intra readers. Every other block reader — the sub-8x8 leaves
(`decode_leaf_split4`, `decode_leaf_rect8`, `decode_intra_sub8_leaf`,
`decode_inter_sub8_split4`, `decode_inter_sub8_rect2`) and the inter block
readers (`decode_inter_block`, `decode_inter_block8`) — left the previous
block's palette size/colours standing in those cells. libaom's mi grid says
`palette_size == 0` there (`av1_allow_palette` needs BLOCK_8X8 and up; an inter
block has no palette at all), so the NEXT block's `av1_get_palette_mode_ctx`
and `av1_get_palette_cache` read 0 where ours read the stale neighbour.

**Class: `cdf-row-held-constant`** (a context read off a table row libaom never
selects), with the same shape as `trial-map-not-restored`: state published by
one block path and never cleared by the paths that cannot produce it.

## First differing symbol (measured, two instrumented decoders)

Synthetic 1920x1024 libaom stream (`-cpu-used 6 -crf 35 -g 12`, dark
screen-content-detected source), KEY frame, block **mi(76,88)**, BLOCK_16X16,
DC_PRED:

* both decoders identical through `skip/cdef/dq/mode/angle_y/uv_mode/angle_uv`
  (`rng=41844`);
* aomdec `EC_PALSYN_AOM mi=76,88 bsize=6 mode=0 uv=0 ctx=0 y=0` — palette ctx 0,
  no palette, then `EC_ISTEP name=use_filter_intra val=0 rng=57408`;
* ours `TRACE pre_palette_y_mode bsize_ctx=2 mode_ctx=1` → `palette_y_mode
  value=1` → decoded a 2-colour palette and its whole colour-index map, and
  then **skipped the `use_filter_intra` symbol** (a palette-Y block has none).
* the stale `mode_ctx=1` came from the 8x8 palette block at mi(72,88); the 4x4
  group at mi(74,88) between them published nothing.

One symbol short → tile desync → whole-frame mismatch from that block on.

## Fix

Publish the block's own zeros from every reader that cannot code palette, at
the same footprint it publishes skip/lf state (7 sites, `record_palette_y_rect`
/ `record_palette_uv_rect` with `size = 0`). `decode_block_128rect` already did
exactly this ("the unconditional stamp clears any stale neighbour palette
state, as every other block path does") — the comment was true of the >=8x8
paths only.

## Sweep

`fill_skip_grid*` (every leaf block publishes it) vs `record_palette_*` over
all 16 block readers: 9 readers had skip but no palette publication. Fixed the
7 that provably code no palette. **Remaining, deferred:** `decode_rect_split`,
`decode_leaf_rect`, `decode_block_rect4`, `decode_rect4_16_strip` and
`decode_intra_rect_in_inter` are >=8x8 intra readers that CAN code palette
(`decode_intra_rect_in_inter` reads `palette_y_mode` at decode.rs:10131) and
publish nothing — the mirror defect (a real palette block invisible to its
neighbours). Needs each reader's decoded palette threaded to the tail.

## Verification

* synthetic gate stream: ours vs `ffmpeg`'s own decode — **byte-exact**
  (before the fix: all 12 frames differ from sample 491904, mi(76,88)).
* film B, libaom `-cpu-used 6 -crf 5 -g 48`, 48 frames: the KEY frame's
  `EC_IMODE` trace is now **identical to aomdec for all 15774 blocks** — the
  charter's "first divergent intra block mi(184,344)" premise is STALE, that
  desync is gone. crf 5 still fails, now in an INTER frame ("a Golomb tail
  longer than this decoder reads", last block read mi(124,412)); narrowed and
  deferred below.

## Files

* `crates/ec-av1/src/decode.rs` — the 7 zero-stamps (2 in the key-frame sub8
  readers, 5 in the inter/sub8 readers).
* `crates/ec-av1/src/encode.rs` — `external_ladder` now decodes EVERY reference
  ladder point through `decode_stream` and asserts sample-exactness against
  ffmpeg, naming encoder/params/frame/plane/sample (the assertion that would
  have caught both this and the `force_integer_mv` defect).
* `crates/ec-av1/src/motion_field.rs` — `EC_INTMV` trace print when
  `force_integer_mv` rounding actually changes a projected candidate.

## OPEN / deferred

1. **crf 5 inter-frame Golomb refusal** — key frame is exact now; the failure
   is in an inter frame (`EC_MM` trace, last read mi(124,412)). Unblocks: diff
   aomdec `EC_MODE`/`EC_STACK` against our `EC_MM` from the first inter frame.
2. **The palette-publishing mirror gap** above (5 readers).
3. **The sensitive `force_integer_mv` fixture**: a 1920x1024 source with a
   textured patch moving 2.5 px per duplicate-pair (half-pel motion, exact
   duplicate frames) makes libaom round 72 projected candidates
   (`EC_TRACE_TPL` → `EC_INTMV`), and the stream STILL decodes byte-exact with
   the fix stubbed (`if false &&`): the rounded candidates never change a coded
   block's mv. Sensitivity needs the rounded candidate to win a
   NEARESTMV/NEARMV slot.
