# lane-intra16x4 r1 HANDOFF (tip 17462bc + this file)

Env bypass name is **`EC_INTRA16X4_DECODE=1`** (decode path) and `EC_INTRA16X4=1` (traces
only) — the coordinator's message says `EC_INTRA16X4`; the decode switch is the `_DECODE`
one. Default behaviour without it: the original refusal, unchanged.

## Step 1 measurement (env print at the refusal site, `EC_INTRA16X4=1`, `EC_RECT64_SPLIT=1`)

| probe | shape | pair parity | has_chroma | prev strip single-ref inter | skip | tx_select |
|---|---|---|---|---|---|---|
| 10-bit 3840x1608 cut 0   | 16x4 | 0 (even) | false | – | 0 | 1 |
| 10-bit 3840x1608 cut 300 | 4x16 | 0 | false | – | 0 | 1 |
| 10-bit 1920x792 @900     | 4x16 | 0 | false | – | 0 | 1 |
| 10-bit 1920x792 @5400    | 16x4 | 0 | false | – | 0 | 1 |
| 10-bit 1920x792 @6300    | 16x4 | 0 | false | – | 0 | 1 |
| 10-bit 1920x792 @8100    | 4x16 | 0 | false | – | 0 | 1 |

6/6 fire first on an EVEN strip (`is_chroma_reference` false: no `uv_mode`, no chroma
residual), non-skip, `--enable-tx-size-search` on, no screen-content tools.

## What the path implements (all in crates/ec-av1/src/decode.rs)

* `decode_rect4_16_strip` — the per-strip body of the key frame's `decode_rect4_16`,
  lifted verbatim (mode info, luma whole-TU or split walk, the pair's single 8x4/4x8
  chroma on the ODD strip at the pair's origin, palette/skip/lf/partition bands).
  `decode_rect4_16` now calls it 4x — key-frame behaviour unchanged.
* `decode_intra_rect_in_inter(.., strip16: Option<(horz, has_chroma)>, ..)` — new 16x4/
  4x16 arm running that body with `INTRA_IN_INTER_MODE = Some((0, skip))`
  (`size_group_lookup[BLOCK_16X4] == 0`, oracle `common_data.h:61`). Mixed intra/inter
  pairs need no new chroma code: the odd strip carries the pair's chroma predicted per
  its own type, and the inter side's `InterStripChroma::prev == None` arm already covers
  intra-then-inter.
* The inter arm ALSO publishes `set_txfm_ctxs` (`above_txfm`/`left_txfm` = tx_w/tx_h) and
  `txfm_partition_update_rect`, and reads tx-depth from `tx_size_context_txfm_rect`.
  The key-frame body publishes none of that; without it the block AFTER the strip broke
  while the strip's own pixels were exact (128x128 8-bit: strips at mi(23,16)/(23,20)
  exact, damage from row 96, chroma exact).
* Counter `intra16x4_in_inter_hits()` `[16x4, 4x16, chroma-reference]`, printed by
  `decode_probe`.

## Pixel state under the bypass

* 10-bit 3840x1608 cut 0: no longer stops here — 14 intra 16x4 strips decoded, then
  `an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition`
  (another lane). Same new refusal on cuts 300 / @900 / @5400 / @6300; @8100 stops on
  `an intra-coded 16x4/4x16 strip on the inter block path` — that is the `_` arm of
  `decode_intra_rect_in_inter` reached by a CLIPPED (frame-edge) shape, not a 1:4 strip.
  No pixel comparison is possible on any cut: every firing frame also carries the
  sub-8x8 intra block, so no OBU prefix decodes clean (hg-0 prefixes 50 and 52 both
  refuse inside the same frame).
* aomenc witness, 8-bit 192x128 cq60 "bar" recipe (`~/.cache/intra16x4-tmp/s5.obu`,
  ref `ref5.yuv`, ours `ours5.yuv`): 6 frames decoded, frame 0 exact, **frame 1 LUMA
  mismatches from (175, 0), 8400 px**; the two intra strips themselves (mi(1,40) and
  mi(3,40) = px x160..175, y4..15) are exact and all chroma is exact. The first bad
  pixel is in the EVEN (inter) strip ABOVE them.
* The gate `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_
  decodes_pixel_exact` (stream.rs) is written, red, and `#[ignore]`d with that
  measurement in its reason string. No refusal lifted; refusal_inventory/gate_coverage
  untouched.

## Exact next step

1. Reproduce with `~/.cache/intra16x4-tmp/s5.obu` (recipe in the report). Ladder the msac
   RANGE (ours `EC_TRACE_COEFF` vs instrumented aomdec) across frame 1's superblock at
   mi_col 40, mi_row 0..3 — decide first whether the (175,0) damage is entropy or
   reconstruction; chroma being exact while luma is wrong argues reconstruction/deblock.
2. Prime suspects, in order: (a) the deblock grid the intra strip writes over the PAIR
   (its `fill_lf_grid_rect` runs at strip granularity while the caller's tail also
   records the block), (b) the mv/mi field the tail stamps (`is_inter: false, ref -1`)
   over rows the EVEN inter strip owns, (c) `above_side_mi`/`left_side_mi` partition
   context published twice (strip body + caller tail).
3. Only then re-run the gate; the 4x16 arm stays blocked on the sub-8x8 intra lane.
