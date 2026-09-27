# lane-av1-ibc444rect: intrabc rect-leaf chroma walk at lossless 4:4:4 (r1)

## Charter

Fix the deferred `av1llsub8` gap: intrabc rect leaves at lossless 444 routed
the rect arm's `read_coeffs_rect` path with the wrong unit structure.
Unblocked by the green inter side (av1llinter2). Work in
`edith_codecs-av1ibc444rect` @ 9e77c16b. 4:2:0 stays identical; existing
lossless gates stay green. At most 8 encodes for the fixture.

## Defect (both planes of the intrabc route)

`decode_leaf_rect8`'s intrabc arm at lossless pushed ONE rect unit:

- luma: `read_coeffs_rect(LumaRect8x4Inter, 8x4)` — one TX_8X4-shaped read,
- chroma: `sub8_leaf_chroma444`'s non-square arm, one `ChromaRect8x4` read.

The oracle codes per-4x4 INTER TX_4X4 units, plane-major: `read_tx_size`'s
lossless first line (`decodeframe.c:1203`) forces TX_4X4 before any tree, the
frame's `TxMode` is `ONLY_4X4` (spec 6.8.20) so no var-tx tree either, and
`av1_read_tx_type`'s `qindex == 0` early return (`decodemv.c:681`) drops the
tx_type symbol the LOSSY 4x4 inter sets DO carry. Fail-before, measured: the
single-rect read desynced the tile and tripped the lossless WHT assert
`(4, 8) != (4, 4)` at `TxParams::run` — the same panic class av1llinter2
attributed on the inter side.

## Fix (same shape as the non-intrabc arm)

1. `decode_leaf_rect8` intrabc arm, `lossless && !skip`: the leaf's luma
   coefficients become `(bw/4)*(bh/4)` 4x4 units in the EXISTING per-unit
   loop (var-tx `(tw,th)==(4,4)` mechanics: `inter_txbset_for(4)`,
   `scan4`, per-unit `around_mi`, `luma_skip_ctx`, DV copy prediction via
   the palette slot, `push_intra`). The TXFM band publish moved AFTER the
   unit loop (`set_txfm_ctxs` runs at `parse_decode_block`'s tail, so the
   units' own `txb_skip_ctx` reads must see the pre-block bands). The units'
   `coding.tx_type` is nulled at lossless — WITHOUT this the ladder diverges
   immediately (ours +4096 rng on a symbol the oracle never codes; found by
   the `EC_TRACE_COEFF` ladder, oracle O#1353 vs ours G#1359-1361 on the
   fixture's first leaf).
2. The units' residual goes through `TxParams { lossless: lossless(fctx) }`
   (read_plane's own route), NOT `dequant_and_inverse_typed_wh` — the latter
   has no lossless flag and ran a DCT inverse over the WHT-ordered levels
   (±1..30 sample errors on every walked leaf; gone after the fix).
3. `sub8_leaf_chroma444`'s intrabc non-square arm, `lossless`: per-4x4
   `TxbSet::Chroma4` walk mirroring the lane-av1-llsub8 fix (per-unit
   `around_mi_rect`, `Reach::of`, per-unit entropy stamps, `Some(3)` big-row
   fold), with the intra predictor swapped for the CURRENT frame at the DV.
4. Skip arms untouched. 4:2:0: the luma walk also applies at 420 lossless
   (luma is subsampling-independent) — that route was equally broken before
   and no committed gate reaches it; every currently-green stream is
   bit-identical (all 16 battery gates below).

Counters `INTRABC_RECT8_LOSSLESS_LUMA_HITS`/`_CHROMA_HITS` + accessors
(`stream::intrabc_rect8_lossless_{luma,chroma}_hits`,
`reset_ibc444rect_hits`) pin the walk for the gate.

## Fixture (bounded grid: 8 encodes, budget used 8/8)

`fixtures/ll444_ibc_rect.obu`, 99614 bytes, sha256
`482dedca0201beea9b0a05720b5da7fe93c350c9fea7699cdb010c3fbc227ec0`.
Source: `lanes/av1ibc444rect_fixture_gen.py` (sha256
`3420a1f80603efc16a66a37be72d04dcd66836df0dc79d28710511c165ef92f1`,
y4m sha256 `5d3b8a2cbce29766852806787811f28a0b7b7df75409915666bad087e0cc9768`):
320x240 C444, dithered chaotic base + single-half 8x4/4x8 near-match stamps
(per-copy bias + random flips — never an exact match). Encoder:
`$HOME/.cache/aom-oracle/build/aomenc --codec=av1 --bit-depth=8
--input-bit-depth=8 --passes=1 --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1
--limit=1 --threads=1 --tile-columns=0 --lossless=1 --sb-size=128
--enable-1to4-partitions=1 --profile=1 --i444 --enable-intrabc=1
--tune-content=screen --min-partition-size=4 --max-partition-size=8 --obu`.
The stream codes hundreds of unskipped intrabc rect leaves (oracle inspect
census: 8x4 + 4x8 + 4x4 intrabc, most non-skip).

Grid summary (all probed with env-gated shape prints + the counters):
testsrc2-based and five crafted variants; every stream that codes intrabc
rect leaves ALSO codes an intrabc `BLOCK_8X8` leaf (aomenc's dominant
intrabc shape — 284 of them in the E7 census) and/or skipped intrabc leaves.
`--max-partition-size` cannot forbid 8x8 `PARTITION_NONE` while keeping
8x4/4x8 leaves; content shaping reduced but never eliminated 8x8 intrabc.

## Verification (measured)

- Entropy: `EC_TRACE_COEFF` ladders aligned ours-vs-instrumented-aomdec
  (`~/.cache/aom-oracle/build`, `EC_TRACE_COEFF=1`); post-fix the tile runs
  through all walked leaves with no desync (pre-fix: desync at the first
  leaf's eob read, then the WHT assert).
- Pixel exactness of the walk vs the oracle `aomdec --rawvideo`, measured
  with a THROWAWAY two-line patch of the sibling 8x8 defect (16->block-size
  chroma prediction buffers in `decode_leaf8`) — applied ONLY to measure,
  reverted before commit, never part of this lane's diff:
  - E4 fixture: luma 0 differing samples; chroma diffs ONLY on skipped
    intrabc cells (36727 bytes, 100% `(intrabc=1, skip=1)` cells + their
    poisoned neighbours; zero on walked leaves).
  - E8 (= the committed fixture): 339 walked rect leaves; luma 0 differing
    samples; chroma 5624 bytes, again exclusively on `(intrabc=1, skip=1)`
    cells + neighbours.
- Gate `a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units` (green):
  pins fixture len/FNV, asserts the walk fires exactly (3, 3) before the
  known leaf8 death and that the death signature is the leaf8
  `palette_window` slice — i.e. the entropy ladder survived the walked
  leaves. Fail-before: pre-fix the death was the `(4, 8) != (4, 4)` WHT
  assert. Mutation check: flipping the chroma `Some(3)` ctx to `None`
  fails the gate (desync moves the death).
- Battery, all green: `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`,
  `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`,
  `a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`,
  `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`, the new
  gate, plus the ten lossy intrabc gates (screen-kf intrabc reads, 16x4
  pair strip, both-orientation rect intrabc, sb128 screen + rect strip,
  tx4 leaf chroma, sub8 census, var-tx census, mixed var-tx tree, tx-select
  census). `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.

## Not claimed

The committed tree does NOT decode either candidate fixture to a full frame:
every reaching stream panics at its first intrabc `BLOCK_8X8` leaf on the
llintra-deferred defect. Whole-fixture 0-diff is therefore NOT claimed and
cannot be measured on this tree; the walk's own samples are measured 0-diff
as above.

## Blocked-by / deferred (spoken, not hidden)

1. `decode_leaf8`'s intrabc chroma prediction buffers, hardcoded 4x4
   (16 samples for a 64-sample 4:4:4 reconstruct) — panics at
   `palette_window` (decode.rs ~2460). OWNER: the `av1llintra` deferral; this
   lane did NOT touch `decode_leaf8` (different function, sibling
   ownership). Fix is its own two lines (buffer + predict size follow
   `chroma_444`); after it lands, decode completes and this lane's gate
   flips to the full-frame exact comparison.
2. Skipped-intrabc sub-8x8 chroma predictor: `sub8_leaf_chroma444`'s skip
   arm (and the 4:2:0 group-tail skip arm) predict INTRA-DC where libaom
   predicts the frame copy at the DV (`av1_build_inter_predictors_sb` runs
   regardless of skip for an intrabc block). All remaining E4/E8 chroma
   diffs sit exactly on those cells. UNNAMED in any prior report (loss64
   fixed the STRIP path only); class-adjacent to the RESERVED 4:2:0
   group-tail skip arm, so per the standing reservation this lane stops and
   escalates instead of editing. Needs: consult the DV in the skip arms
   (armed palette slot), plus a fixture gate.
3. `av1llintra2`'s rect64 lossless chroma gap (`EC_RECTCHROMA_GAP`) remains
   deferred in its own lane; not touched here.

## Function-collision statement

This lane's diff lands only in `decode_leaf_rect8` and `sub8_leaf_chroma444`
(+ counters in decode.rs, accessors/gate in stream.rs). `decode_leaf8` —
where the blocking sibling defect lives — is untouched, byte-for-byte.
