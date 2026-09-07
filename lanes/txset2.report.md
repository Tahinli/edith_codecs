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

### The 12-frame gate (`encode::tests::bd_rate_screen_native`, 12 frames, gop 12)

Control re-run on this head (`EC_AV1_TXSET_INTER=0`, which is also the
default): it REPRODUCES the charter's numbers to the digit, byte counts
included.

| clip | control | arm: every frame, every size | delta | wall ctl -> arm |
|---|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | **+23.1% / +3.3%** | +24.1 / +20.1 | 177.2s -> 182.0s |
| bars 2160p | +9.4% / -12.7% | **+31.2% / +3.7%** | +21.8 / +16.4 | 155.9s -> 160.7s |
| film A | +21.7% / -4.4% | +21.5% / -4.5% | -0.2 / -0.1 | 154.9s -> 167.7s |
| film B | +26.9% / -0.6% | +27.2% / -0.3% | +0.3 / +0.3 | 161.5s -> 167.6s |
| screen capture | +20.8% / -30.1% | +20.1% / -30.4% | -0.7 / -0.3 | 112.2s -> 109.5s |

KEEP RULE on the unconditional arm: FAIL, and not marginally -- the two
synthetic bars rows lose TWENTY points (2 dB of luma PSNR at the same byte
count). Film A is 0.2/0.1 down, film B 0.3/0.3 up, the capture 0.7/0.3 down.

The identity transform is not mis-scaled at 32x32: the typed round trip now
covers `side = 32` for the two types any set names there (`DCT_DCT`, `IDTX`;
ADST is undefined above 16) and `IDTX`'s rmse stays inside the DCT's 2x bound.
The bars collapse is the `local-rd-on-references` class again, at its loudest:
a flat synthetic frame is all 32x32 inter blocks whose residual `IDTX` prices
cheapest locally, and every later frame predicts from that reconstruction.

## Step 3 -- what libaom actually picks (instrument, read off lane-libcen's census)

`lanes/libcen.report.md`'s census files (`~/.cache/lc-libcen/cen-*.txt`,
`crates/ec-av1/examples/syntax_census.rs`) already carry the per-frame
`tx_type` histogram keyed by CDF row length, so no new run was needed. Film B,
libaom cpu-used 6 crf41 (48 pictures), row length -> set: 3 = the 2-symbol
inter `DCT_IDTX` (32x32), 6 = the 5-symbol `TX_SET_INTRA_2` (intra 16x16 and,
under `reduced_tx_set`, all intra), 8 = the 7-symbol `TX_SET_INTRA_1` (intra
8x8/4x4), 13 = the 12-symbol `TX_SET_INTER_2` (inter 16x16), 17 = ALL16
(inter 8x8/4x4).

| frame | set3 (inter 32) | set6 (intra 16) | set8 (intra 8/4) | set13 (inter 16) | set17 (inter <=8) |
|---|---|---|---|---|---|
| key (718 symbols) | -- | 85.3% | 14.7% | -- | -- |
| arf hint 30 (1058) | 27.5% | 6.8% | 4.5% | 51.7% | 9.7% |
| arf hint 15 (364) | 34.1% | 1.7% | 1.6% | 46.8% | 15.7% |
| arf hint 7 (61) | 67.2% | 1.6% | -- | 19.7% | 11.5% |
| arf hint 3 (41) | 78.0% | -- | -- | 9.7% | 12.3% |

Two readings, both decisions:

1. **libaom never codes `IDTX` at 32x32 on film B.** Every one of its set3
   symbols is symbol 1 (`DCT_DCT`); symbol 0 does not appear in any frame of
   the stream. That is exactly the size our unconditional arm loses its twenty
   points on -- hence the `le16` arm below.
2. **41% of its inter `tx_type` symbols are in sets 13/17**, i.e. the alphabets
   `reduced_tx_set = 0` unlocks, and they are spread across nine or more
   distinct types (set13 symbols 3..11 at 1.5-14% each). Our own stream codes
   `set3/1` and `set6/1` only, 100% `DCT_DCT` in inter frames -- the gap step 2
   is aimed at.
