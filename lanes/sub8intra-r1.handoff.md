# lane-sub8intra r1 HANDOFF (tip = this commit)

Env switch: **`EC_SUB8INTRA_DECODE=1`** enables the decode path; `EC_SUB8INTRA=1` prints the
measurement line at the refusal sites. WITHOUT the switch both refusals are unchanged
(verified: hg-0 and the 1920x792 @900 cut both still stop at
"an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition").
No refusal lifted, `refusal_inventory.rs` / `gate_coverage.rs` untouched, no gate added.

## Step 1 measurement (`EC_SUB8INTRA=1 EC_RECT64_SPLIT=1 EC_INTRA16X4_DECODE=1`, decode_probe)

| probe | shape | sub-block | is chroma ref | skip | tx_select |
|---|---|---|---|---|---|
| 10-bit 3840x1608 cut 0   | 4x8 | 0 | false | 0 | 1 |
| 10-bit 3840x1608 cut 300 | 8x4 | 0 | false | 0 | 1 |
| 10-bit 1920x792 @900     | 4x8 | 0 | false | 0 | 1 |
| 10-bit 1920x792 @5400    | 4x8 | 0 | false | 0 | 1 |
| 10-bit 1920x792 @6300    | 4x8 | 0 | false | 0 | 1 |
| 10-bit 1920x792 @8100    | -- (stops earlier on the clipped-16x4 arm) |

5/6 fire first on sub-block 0 (NOT the chroma reference), non-skip, `--enable-tx-size-search` on.
The 4x4 SPLIT refusal is never the first wall, but becomes the wall on 3/6 probes once the
HORZ/VERT arm decodes, so both arms are implemented.

## What is implemented (crates/ec-av1/src/decode.rs)

* `decode_intra_sub8_leaf(.., (bw,bh), has_chroma, skip, ..)` -- one INTRA 8x4/4x8 **or 4x4**
  leaf on the inter path. Reads `y_mode[0]` (`size_group_lookup[BLOCK_8X4]==0`), NO angle delta
  (`av1_use_angle_delta` needs >= 8x8), `uv_mode` off `uv_mode_cfl[mode]` + CfL alphas only on
  the chroma reference, `filter_intra` off the shape's own class row
  (`filter_intra_size_class_rect` / `_class(4)`), then the tx-depth symbol
  (`tx_size_cat0`, ctx from `tx_size_context_txfm_rect` -- the INTER frame's live
  `above_txfm`/`left_txfm`, not the key frame's deblock approximation; `BLOCK_4X4` reads none,
  `block_signals_txsize` is false). Luma = `decode_leaf_rect8`'s body (whole `TX_8X4`/`TX_4X8`
  via `read_coeffs_rect` + `LumaRect4x8[Set1]` + `SCAN_8X4`/`SCAN_4X8`, or two `TX_4X4` units at
  depth 1; square `read_plane` for 4x4). Chroma, when this leaf IS the chroma reference: the
  group's one 4x4 unit intra-predicted at the 8x8 group origin with `group_reach = Reach::of(8,..)`,
  CfL AC from the group's 8x8 luma -- `decode_leaf_rect8`'s post-loop chroma verbatim.
  Publishes: mode into `record_mode_mi` + the coarse `above_mode`/`left_mode`, skip grid,
  lf grid at tx dims, `set_txfm_ctxs` equivalent (`above_txfm = tx_w`, `left_txfm = tx_h`),
  `record_inter_rect_mi(.., is_inter=false, ref 0, filter [3,3])`, and `MiInfo { is_inter:false,
  ref_frame:-1 }` over every mi cell.
* `decode_inter_sub8_rect2` / `decode_inter_sub8_split4`: the `!is_inter` arm calls it (behind
  the env switch), sets `intra_chroma` when the leaf was the chroma reference, and the whole
  chroma tail (prediction, residual, `record_uv_mode_mi`, the u/v neighbour-state loop) is
  guarded by `if !intra_chroma`.
* MIXED groups (chroma reference INTER, a sibling INTRA): `is_sub8x8_inter`
  (reconinter_template.inc:54) returns false as soon as any mi of the group is intra, so the
  chroma is predicted WHOLE 4x4 from the chroma-reference block's own mv at the group's chroma
  origin instead of in 2x2/4x2 pieces -- implemented in both functions (`mixed` flag).
* tx_type rule for that chroma: `first_tx_type` now comes from the chroma-reference block's own
  first TU when sub-block 0 is intra (`xd->tx_type_map[0]` of the current mi). NOTE the
  pre-existing all-inter rule ("FIRST sub-block") is untouched but looks wrong against
  blockd.h:1288 -- `xd->tx_type_map` is rebased to the CURRENT (chroma-reference) mi, so the
  co-located luma (0,0) is the LAST sub-block's, not the first's. Flagged, not changed.
* Counter `sub8_intra_rect_hits() -> (8x4, 4x8, chroma_ref, mixed)`, printed by decode_probe
  (`sub8_intra_rect: ...`), exported through `stream.rs`.
* `EC_TRACE_MODE_STEP` prints `name=mode` and `name=tx_depth val/ctx` for the leaf, in the
  oracle's EC_ISTEP format.

## Pixel state under the bypass -- the gate is RED

* Film: all six probes move past this wall (hg-0 7 leaves, hg-300 10, @900 21 (+2 mixed),
  @5400 28 (+2 mixed), @6300 2) and stop at the NEXT wall (4x4 split intra on 3 of them,
  the 16x4 strip arms on the others). No film pixel comparison is possible: every firing frame
  also carries the unproven intra-16x4 strip.
* aomenc witness sweep `~/.cache/sub8intra-tmp/sweep.sh` and `sweep2.sh` (recipe: 128x128 /
  192x128 gray lavfi sources, cq 48..63, `--min-partition-size=4 --enable-1to4-partitions=0
  --max-partition-size=8|16 --enable-filter-intra=0 --sb-size=64 --enable-tx-size-search=1`),
  logs `sweep1.log` / `sweep2.log`: the counters fire on every arm (up to 293 8x4 leaves) but
  **only 1 of 24 arms decodes all 6 frames, and that one MISMATCHES at byte 24735**. Eleven
  arms encoded with `--max-partition-size=8` -- where a 16x4 block CANNOT exist -- still report
  "an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition", i.e. the refusal is
  hallucinated by our own desync (class `refusal-from-own-desync`).

## Where the desync is (bisected, do not redo this)

Witness `~/.cache/sub8intra-tmp/x_1t_48_8_128x128.obu` (8-bit 128x128, cq48, max-partition 8;
1 intra 8x4 leaf then the bogus refusal).

* EC_MODE ladder (ours vs instrumented aomdec, `r.l`/`o.l` in that dir): identical for 168
  entries; first divergence at `mi_row=24 mi_col=0` (ref rng=39238, ours rng=44268 and ours
  reads a 16-wide block where the reference reads four 4x4s).
* The intra leaf itself is EXACT: it is at mi(23,12) (HORZ pair with the inter leaf at
  mi(22,12), so the INTRA one is the chroma reference). Ours prints
  `EC_ISTEP mi_row=23 mi_col=12 name=mode val=0 rng=38508` and
  `name=tx_depth val=0 ctx=2 cat=0 rng=49194`; the oracle prints the identical tx_depth line
  (ref_step.log:2463). So `y_mode[0]`, the missing angle delta, the uv/CfL reads and the
  tx-depth ctx are all right.
* `tag=all_zero` ladder (`rz.l`/`oz.l`): the first 1405 luma/chroma transform units match
  exactly; unit 1406 diverges (ref `all_zero=0 rng=40294`, ours `all_zero=0 rng=52548`). Both
  decoders are still reading plane 0 there, so the divergence is in symbols read BETWEEN two
  luma TUs -- i.e. inside the block following the intra leaf (the group at mi(22..23,14), the
  8x8 immediately to its right) or in that group's chroma.

## Exact next step

1. On that witness, ladder the group at mi(22,14)/(23,14) element by element (aomdec
   `EC_ISTEP2 name=interp` / `EC_MODE`, ours `EC_MODE_MV`/`EC_MODE_VALR`) -- the intra leaf at
   mi(23,12) is its LEFT neighbour, so the prime suspects are the left-side bands the intra
   leaf publishes: `left_txfm` (tx dims vs block dims), `left_ref`/`left_inter` from
   `record_inter_rect_mi(ref 0)` (INTRA_FRAME is 0 vs the `-1` the mi grid gets), the
   coefficient state `left[..][0..2]`, and `left_side_mi` (the caller writes the group's
   partition ctx AFTER the leaf, check the order).
2. Then re-check the chroma of the MIXED group at mi(22,12): its chroma is coded by the intra
   leaf, so the inter sibling's own `piece` is dropped -- confirm against
   `build_inter_predictors_sub8x8`'s `is_sub8x8_inter == false` path.
3. Only then re-run the sweep; a green arm needs `--max-partition-size=8` (no 16x4 shape can
   exist there, so any 1:4 refusal is proof of a remaining desync).
