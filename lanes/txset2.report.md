# lane-txset2 -- the inter transform-type search, and reduced_tx_set=0

Branch `lane-txset2` off main `5e923074`. Predecessor: `lanes/txset.report.md`
(intra 5-type reduced set, screen-gated).

## Step 1 -- inter luma IDTX vs DCT_DCT (`TX_SET_INTER_3`)

The writer already codes a two-symbol `tx_type` for every inter luma unit at
32x32 and below (`TxbSet::Luma{4,8,16,32}Inter`) and always named `DCT_DCT`.

* `encode::inter_tx_type_candidates(set, screen)` -- `[DCT_DCT, IDTX]` for
  those four sets, `[DCT_DCT]` for the 64-point transform (no symbol) and for
  chroma.
* `Plane::code_from_prediction_typed` / `mc_trial_typed` /
  `mc_trial_compound_typed`: the untyped entries are now `DCT_DCT` wrappers,
  so no call site outside the search moved.
* `commit_inter_luma` prices the candidates twice: once for the whole-block
  (depth 0) residual, once per var-tx split unit, both by `sse + lambda *
  bits` with the bits carrying the `tx_type` symbol through RDOQ -- the same
  rule the intra search uses. The chosen types leave through the new fourth
  element of its return and reach the writer as `BlockCoeffs::luma_tx_types`
  (flat = one entry, split = four in the writer's raster order).
* Levers: `speed::TX_TYPE_SEARCH_INTER` (OFF at every preset until the gate
  says otherwise), `EC_AV1_TXSET_INTER=0|screen|1`, plus a test-only
  process-global override (`encode::set_inter_tx_search`).
* Census: `encode::INTER_TX_TYPE_HITS`, counted at the COMMIT points only, so
  a split unit's winner the flat cost then beats is not counted.

Witness: `encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly`
-- PASS, hits `[DCT_DCT 154 .., IDTX 1368 ..]`, ours == ffmpeg every sample.

Gate rows: see below (arms running).
