# lane-inter16ab r2 — 16x16-level 1:4 partitions in inter frames

## Verdict: RED. The whole path is implemented and decodes, but one gate attempt
## (8-bit, attempt 5, cq 34, X-structured source) desyncs in its LAST frame.
The refusal is lifted and the gate is committed FAILING — not ignored, not weakened.
This branch must not merge into main until the residue below is closed.

## What changed (all in `crates/ec-av1/`)
- `src/decode.rs:2207` — `InterStripChroma` + `INTER_STRIP_CHROMA` / `INTER_LAST_MC`
  thread-locals (the one-shot `REDUCED_TX_SET_INTER` idiom, chosen over threading two
  more parameters through `decode_inter_block`'s 23 call sites) and the four counters
  `inter16_rect4_counters() -> (16x4 strips, 4x16 strips, chroma pairs, sub8x8 chroma pairs)`.
- `src/decode.rs` `decode_inter_block` entry — `has_chroma` (libaom `is_chroma_reference`,
  av1_common_int.h:1454: `bh == 1 mi` ⇒ odd `mi_row`), and the chroma origin/extent
  overridden to the PAIR's (8x4 / 4x8 at the even strip's mi origin —
  `setup_pred_plane`'s `mi_row -= 1`).
- same fn — `is_comp_ref_allowed` / `is_motion_variation_allowed_bsize`
  (blockd.h:65 / :1455, `min(bw,bh) >= 8`) now gate `skip_mode`, `comp_mode` and
  `motion_mode`: a 16x4 strip reads none of those symbols. `is_interintra_allowed_bsize`
  already excluded it (aspect 4:1).
- same fn — `build_inter_predictors_sub8x8` (reconinter_template.inc:87-160): the pair's
  chroma is built in `b4_w x b4_h` = 8x2 (2x8) pieces; the second piece is this strip's own
  mv (already written by the whole-block predictor, MC being position-invariant for a fixed
  mv), so only the FIRST piece is rebuilt, from the previous strip's mv/ref/filters, and only
  when `is_sub8x8_inter` holds (previous strip single-ref inter — `INTER_LAST_MC`).
- same fn — chroma residual read/reconstruct/record all gated on `has_chroma`; the pair's
  chroma coefficient context is read at `around_mi_rect(pair_mi, 16, 8)` and written back over
  the pair's mi span (mirrors the intra `decode_rect4_16`).
- `src/decode.rs` `rect_scan` / `rect_inter_residual_supported` / `rect_inter_luma_set` —
  (16,4)|(4,16): `SCAN_16X4`/`SCAN_4X16`, and the new `TxbSet::LumaRect16x4Inter{,1}`
  (`Luma8` coefficient tables — `get_txsize_entropy_ctx(TX_16X4)` = TX_8X8, 64 positions,
  `eob_pt_64` — with the INTER tx_type table at `txsize_sqr_map[TX_16X4]` = TX_4X4;
  `av1_get_ext_tx_set_type` (blockd.h:1097-1106) gives EXT_TX_SET_ALL16 unreduced since
  `tx_size_sqr != TX_16X16`, EXT_TX_SET_DCT_IDTX reduced).
- `src/cdf_state.rs:233,1854` — the two new `TxbSet` variants and their tables.
- `src/decode.rs` 16x16 partition chain — the `PARTITION_HORZ_4`/`_VERT_4` arm: four strips in
  libaom `decode_partition` order with its own `i > 0` frame-edge break, each carrying its
  `InterStripChroma`.
- `src/refusal_inventory.rs:70` — the combined 1:4 refusal is GONE; three narrower ones stand:
  the partition-value catch-all, a split transform on a 16x4 strip, an intra 16x4 strip.
- `src/stream.rs` — gate
  `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
  (r1's gate shape with `--enable-1to4-partitions=1 --min-partition-size=4`).
- `examples/decode_probe.rs:43` — prints `inter16_1to4: horz4= vert4= chroma_pairs= sub8_pieces=`.

## Gate result — RED
`EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter16ab cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_16x16_level_1to4 -- --nocapture`

EVIDENCE: gate stdout (suite log `$HOME/.cache/inter16ab-suite-r2.log`) | 192x128, 6 frames, real aomenc `--enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=16 --cpu-used=0`, every decode-order frame compared Y/U/V vs ffmpeg | 8-bit attempts 0..4: 2 named refusals ("an inter partition below 8x8" — `--min-partition-size=4` also lets aomenc split to sub-8 leaves, a different lane's refusal), earlier attempts pixel-exact; attempt 5 (cq 34, X-structured source, tx-size-search 1) fires arms [horz4=8, vert4=0, chroma_pairs=4, sub8x8_pairs=4] and MISMATCHES in decode-order frame 5 only: 8683 luma pixels, first at (96, 59), max |delta| 107. Frames 0-4 of that attempt are pixel-exact.

Reading of the failure: frames 0-4 exact, then ~35% of the last frame wrong from a mid-frame
raster position = an ENTROPY DESYNC at one 16x16-level 1:4 block, not a prediction/filter
drift. `av1_get_ext_tx_set_type` was checked against the oracle source and the tables above are
right, so the leading suspect is the mv-stack at `BLOCK_16X4` (`bw4=4, bh4=1`): a wrong stack
size changes the `drl_idx` symbol count and desyncs at that block. NOT confirmed — no range
ladder was run this round (tool budget).

## Film probe — moved one refusal deeper
`ffmpeg -ss 900 -t 2 -i <Hunger Games> -c:v copy -an -f obu` → `decode_probe`:
- before r2: `REFUSED: an inter 16x16-level 1:4 partition (HORZ_4/VERT_4 …)`
- after r2:  `REFUSED: a split transform on a 16x16-level 1:4 inter strip
  (sub_tx_size_map[TX_16X4] is the rectangular TX_8X4)`
1 frame completed (`EC_AV1_FINAL_DUMP` file count) in both. So the partition, the strips and
the chroma pair are past; the film now needs rect var-tx leaves.

## Residue
- fix-now (next round, this lane): the attempt-5 desync above. Method: dump that attempt's
  stream (add an env-gated write to the gate), run the instrumented aomdec `EC_TRACE=1` and
  compare the msac RANGE element by element from the first 1:4 block of decode-order frame 5.
  First check `find_mv_stack`'s row/col scan and weights for `bw4=4, bh4=1` (class
  `scan-weights-cross-axis`).
- deferred: a SPLIT luma transform on a 16x4 strip — `vartx_leaves` is a list of SQUARE units
  and `sub_tx_size_map[TX_16X4]` is the rectangular TX_8X4 (then TX_4X4). Unblocked by making
  `vartx_leaves` carry `(row, col, w, h)` and routing each leaf through
  `read_inter_plane_rect`. This is what BOTH films now stop at.
- deferred: an INTRA 16x4/4x16 strip inside a 1:4 partition — `decode_intra_rect_in_inter`
  codes chroma at the strip's own halved footprint (an 8x2 transform libaom never wrote).
  Unblocked by giving that path the same pair treatment.
- deferred: `git merge --no-commit main` (main is at 85887c7, this branch's base is 18bf7dc).
  Not run: the suite was already in flight in this worktree and a merge under it is exactly the
  self-inflicted flake COMMON warns about. The merge is owed before any merge to main.
- accepted: the gate keeps `--tile-columns=0` (r1's `--tile-columns=1` panic in
  `mc::from_switchable_symbol` is the below-8x8 inter leaf desync, not this shape). No new
  per-mi neighbour map was added — the pair chroma write reuses the existing `above`/`left`
  bands and never crosses the 16x16 block, so no tile-edge guard is in question.
