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

### Step 1, second round: `le16` (the census's own prune) and the screen gate

| clip | control | `le16` (16x16 and below) | screen-gated (SHIPPED) |
|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | +22.9% / +3.1% | -1.0% / -16.8% (byte-identical) |
| bars 2160p | +9.4% / -12.7% | +31.6% / +4.0% | +9.4% / -12.7% (byte-identical) |
| film A | +21.7% / -4.4% | +21.6% / -4.5% | +21.7% / -4.4% (byte-identical) |
| film B | +26.9% / -0.6% | +27.4% / -0.1% | +26.9% / -0.6% (byte-identical) |
| screen capture | +20.8% / -30.1% | +19.9% / -30.4% | **+20.1% / -30.4%** |

`le16` does NOT recover the bars rows (+22.9/+31.6 against the unconditional
arm's +23.1/+31.2), so the size restriction the libaom census suggested is
refuted as a fix: the damage is in the 16x16-and-below inter units, not the
32x32 ones. It also drifts film B further up (+27.4/-0.1).

DECISION: ship SCREEN-GATED, the same shape the intra half of lane-txset
ships behind. The four non-screen rows land on the control's own byte counts
to the digit (194497/299067/419042/567789, 205409/321883/435765/564573,
64392/102508/170169/413430, 27442/50374/105485/296813 -- the clips classify
0 screen frames of 4, the capture 48 of 48), so nothing outside screen
content moves, pins included. The capture is 0.7 down against libaom and 0.3
against rav1e, at no wall cost (112.2s -> 109.4s, inside the noise).

Levers as shipped: `speed::TX_TYPE_SEARCH_INTER` true at presets 0-6 (the row
the intra search carries), `inter_tx_type_search()` defaulting to
`Screen` on top of it. `EC_AV1_TXSET_INTER=1` is the unconditional arm,
`le16` the pruned one, `0` off.

## Step 2 -- `reduced_tx_set = 0` and the 13/17 alphabets: NOT ATTEMPTED, scoped

`deferred: reduced_tx_set = 0 (sets 13/17, and the 7-symbol intra set at
8x8/4x4) -- not started; the two gate slots and the budget went to step 1's
three arms. What it needs, from this lane's reading:`

* The DECODER half is complete and needs nothing: `decode::txbset_for` /
  `txbset_for_inter` already pick `Luma{8,4}Set1`, `Luma{16,8,4}InterSet1`,
  `LumaRect16x8Set1`, `LumaRect{4x8,16x4}Set1`, `LumaRect8x4InterSet1` off the
  frame's own bit, and `decode::tx_type_symbol` answers for the 7/12/16-symbol
  alphabets. Note the spec shape the charter's table simplifies: intra 16x16
  stays the FIVE-type `TX_SET_INTRA_2` even with the bit off (there is no
  `Luma16Set1` and none is needed), only intra 8x8/4x4 widen to seven; inter
  32x32 stays the two-type `DCT_IDTX`, inter 16x16 becomes the 12-symbol set
  and inter 8x8/4x4 ALL16.
* The WRITER half is the cross-cutting part (class `tx-class-cross-cutting`):
  the frame-header bit (`encode.rs:1660`, `:1760`) plus EVERY luma `TxbSet`
  the writer and the pricer pick, which is `tile.rs` 2844, 4087, 4231-4238
  (`write_luma_tus`), 4611-4616, 6078, 7004/7006, 7217/7219 and `encode.rs`
  `code_tx_depth` / `commit_inter_luma` / `inter_luma_set` / the trial set
  arguments. A site that misses the bit codes the right value against the
  wrong CDF -- the `wrong-alphabet-same-value` class -- so the bit belongs in
  one accessor both sides read, not in nine `match` arms.
* The gate shape this lane measured says what to expect: a wider set is worth
  BD only where the reference chain is short or intra-heavy. Both halves of
  the search now measured (intra 5-type, inter 2-type) win on the capture and
  lose on film, so a 13/17 lane should be armed screen-gated from the start
  and spend its slots on the FILM question separately (the rate term, class
  `rd-rate-term-calibration`, is the suspect: at equal bytes the bars rows
  lose 2 dB of PSNR, which is a pricer that under-charges the cheap type, not
  a decoder or kernel defect -- the typed round trip at 32 is inside its
  bound).
