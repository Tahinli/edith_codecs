# lane-txset -- the transform-type search

Branch `lane-txset` off main `3ebe0de9`. Scope: which `tx_type` an intra luma
transform unit is coded with, the forward kernels the missing types need, and
the writer symbol that names them.

## 1. Instrument: what was coded, what was searched (read off the code)

The writer coded ONE transform type, everywhere, always:

    tile.rs:5216   enc.symbol(TX_TYPE_DCT_DCT_SET2, tx_type);   // a constant

and the search never varied a luma type either -- `search_block` and
`code_tx_depth` both go through `code_from_prediction`/`trial`, which pass
`TxType::DctDct` (`encode.rs:1993`, `:2278`). The only non-DCT types this
encoder has ever produced are CHROMA's, which code no symbol at all: the
decoder derives them from the chroma mode (`search_chroma` ->
`default_intra_tx_type`).

STALE PREMISE IN THE CHARTER, corrected: this encoder writes
`reduced_tx_set: true` (`encode.rs:1660`, `:1760`) -- not 0. Under that bit
`get_tx_set` gives, per block class:

| block class | tx size | set the WRITER uses | alphabet | types the search tried (before) | now |
|---|---|---|---|---|---|
| intra luma | 4x4, 8x8, 16x16 | `TX_SET_INTRA_2` (`intra_tx_type_{4,8,16}`) | 5 | DCT_DCT only | all 5 |
| intra luma | 32x32, 64x64 | none (`TxbSet::Luma{32,64}.tx_type == None`) | 1 | -- | -- |
| inter luma | 4x4..32x32 | `TX_SET_INTER_3` (`inter_tx_type_*`) | 2 (IDTX, DCT_DCT) | DCT_DCT only | DCT_DCT only |
| chroma | any | none, derived from uv_mode | -- | mode's default | unchanged |

So the bytes reachable WITHOUT touching the frame header were the four
non-DCT members of `TX_SET_INTRA_2` on every intra luma unit at 16x16 and
below -- an alphabet the writer was already coding into, one symbol wide, and
never using. That is what this lane spends.

Sets 13/17 (`DTT9_IDTX_1DDCT`, `ALL16`) that the charter names are NOT
reachable from here: they need `reduced_tx_set == 0` in the frame header,
which changes the alphabet of EVERY luma `tx_type` symbol in the stream
(`TxbSet::Luma*Set1`/`Set2` exist on the decode side with their CDFs; the
writer's set map at `tile.rs:4208` would have to take the bit). Deferred --
see below.

## 2. Forward kernels

`forward_transform_2d_typed` is basis-driven (`TxType::axes`), so
`V_DCT`/`H_DCT`/`V_ADST`/`H_ADST` and the plain ADST family already had
kernels; the FLIPADST family was refused by a `debug_assert`. A flipped axis
is the plain ADST kernel with the DECODER's output mirrored (`ud_flip` ->
row `h-1-i`, `lr_flip` -> column `w-1-j`), and a mirror is its own inverse,
so the analysis half is the same basis over the MIRRORED residual -- 12 lines,
no new kernel.

Check: `transform::tests::a_typed_forward_round_trips_like_the_dct_one`, now
over all SIXTEEN types at 4x4/8x8/16x16 (was 3), each round trip's rmse under
2x `DCT_DCT`'s. PASS.

## 3. The search + the writer

* `encode::tx_type_candidates(set)` returns exactly the alphabet the writer
  can name for that set, so a searched type is always codable.
* `code_tx_depth` picks a type PER TRANSFORM UNIT (RD: sse + lambda * bits,
  the trial's bits carrying the `tx_type` symbol itself through
  `coeff_bits_typed`/`rdoq`), stored at `tu_row * n + tu_col` -- the writer's
  own index, so a unit the frame edge cuts away cannot slide the rest.
* `search_block` picks one for a whole-block transform, on the mode and angle
  already settled (libaom's ordering, the same shape the angle refinement has).
* `BlockCoeffs::luma_tx_types` carries them to `write_luma_tus` /
  `write_block_planes`; empty = `DCT_DCT` throughout, which is every inter,
  palette and filter-intra block.
* The symbol comes from the DECODER's own map (`decode::tx_type_symbol`,
  keyed by the CDF's alphabet width), so writer and reader cannot drift --
  and it already answers for the 7/12/16-symbol sets a `reduced_tx_set == 0`
  lane would turn on.
* Lever: `speed::TX_TYPE_SEARCH` (presets 0-6 on, 7-10 off) AND a content
  gate -- SCREEN FRAMES ONLY, see the gate table below. `EC_AV1_TXSET=0`
  forces it off entirely.

## 4. Checks

| check | result |
|---|---|
| `transform::tests::a_typed_forward_round_trips_like_the_dct_one` (16 types) | PASS |
| `encoder::tests::an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` | PASS -- hits `[IDTX 12, DCT_DCT 15445, ADST_ADST 70, ADST_DCT 161, DCT_ADST 467]`, ours == ffmpeg every sample |
| `cargo test -p ec-av1 --lib --release round_trip` (19 tests) | PASS |
| `predicted_coeff_bits_track_the_tile_the_writer_wrote`, `tile_bytes_do_not_depend_on_the_thread_count --include-ignored`, `the_facade_codes_the_same_bytes_as_encode_sequence` | PASS |
| `cargo check -p ec-av1 --all-targets` | 0 errors, 0 warnings |


## 5. The 12-frame gate (`encode::tests::bd_rate_screen_native`, 12 frames, gop 12)

Control re-run on this head first (`EC_AV1_TXSET=0`), arm = the search on for
every frame. BD-rate vs libaom cpu-used 6 / vs rav1e speed 6, lower is better;
wall is ours per row.

| clip | control | arm (all frames) | delta | wall ctl -> arm |
|---|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | -2.3% / -17.8% | -1.3 / -1.0 | 247.2s -> 280.4s |
| bars 2160p | +9.4% / -12.7% | +10.2% / -11.6% | +0.8 / +1.1 | 168.8s -> 180.7s |
| film A | +21.7% / -4.4% | +22.1% / -4.0% | +0.4 / +0.4 | 153.9s -> 203.9s |
| film B | +26.9% / -0.6% | +28.1% / +0.7% | +1.2 / +1.3 | 133.5s -> 164.0s |
| screen capture | +27.8% / -26.0% | **+20.8% / -30.1%** | **-7.0 / -4.1** | 83.7s -> 120.6s |

KEEP RULE VERDICT on the unconditional arm: FAIL. The rule wants both film
rows down; both go UP (film A +0.4/+0.4, film B +1.2/+1.3). The capture wins
seven points against libaom and four against rav1e -- the largest single
screen move this lane has on record -- so the search ships behind the SAME
content gate palette, intrabc and the angle-delta refinement already stand
behind (`search.screen`): a non-screen frame is byte-identical to the encoder
before the lane.

Class note for the film loss: an ADST/IDTX unit is a locally cheaper
reconstruction that the next picture then predicts from -- the same
`local-rd-on-references` shape lane-fitu measured for filter intra, which is
why the film rows lose while the intra-heavy capture wins.

### Shipped state (screen gate on), confirm run

| clip | control | shipped | note |
|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | -1.0% / -16.8% | byte-identical (194497/299067/419042/567789 B) |
| bars 2160p | +9.4% / -12.7% | +9.4% / -12.7% | byte-identical (205409/321883/435765/564573 B) |
| film A | +21.7% / -4.4% | +21.7% / -4.4% | byte-identical (64392/102508/170169/413430 B) |
| film B | +26.9% / -0.6% | +26.9% / -0.6% | byte-identical (27442/50374/105485/296813 B) |
| screen capture | +27.8% / -26.0% | **+20.8% / -30.1%** | 36775/45758/57095/71711 B, wall 83.7s -> 96.2s (+15%) |

Every non-screen row lands on the control's own byte counts to the digit --
the gate is the frame classifier, and the four non-capture clips classify 0
screen frames of 4 -- so nothing outside screen content moves, pins included.

Pins: `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins`
PASSES UNCHANGED at 8562 / 33357 -- the pin clip is not screen, so nothing to
re-pin.

## 6. Preset lever and invariants

`speed::TX_TYPE_SEARCH = [true x7, false x4]` (presets 0-6 on) on top of the
screen content gate. The FIVE-type fire count is preset 0's: at preset 6 the
mode/partition pruning removes the blocks `ADST_ADST` and `IDTX` were winning
on (hits `[0, 6192, 0, 79, 284, ..]`), so the witness asserts the full set at
preset 0 and "some non-`DCT_DCT` type fires" at every preset that carries the
lever.

| invariant | preset 0 | preset 6 |
|---|---|---|
| `an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` | PASS | PASS |
| `tile_bytes_do_not_depend_on_the_thread_count --include-ignored` | PASS | PASS |
| `the_facade_codes_the_same_bytes_as_encode_sequence` | PASS | PASS |
| `predicted_coeff_bits_track_the_tile_the_writer_wrote` | PASS | PASS |
| all four above with `EC_COMP_MISMATCH=1` | 0 mismatches | 0 mismatches |

`every_speed_preset_decodes_sample_exact_through_both_decoders --ignored`
PASS (with `EC_COMP_MISMATCH=1`).
`the_encoders_own_streams_are_byte_identical_to_their_pins`: PASS at the
UNCHANGED 8562 / 33357 -- no re-pin, the pin clip is not screen content.

## 7. Deferred

* `deferred: inter luma IDTX vs DCT_DCT (the two-type TX_SET_INTER_3 the
  writer already codes) -- the plumbing is done (an inter block's
  BlockCoeffs carries luma_tx_types through the same writer path), what is
  missing is the candidate loop in code_from_prediction/commit_inter_luma and
  a gate round -- unblocked by ~40 min of gate wall; the screen row is where
  it would pay, same as the intra half.`
* `deferred: reduced_tx_set = 0 and the 13/17 alphabets (DTT9_IDTX_1DDCT,
  ALL16) -- the FORWARD kernels are DONE and round-trip tested (all sixteen
  types), decode::tx_type_symbol already answers for the 7/12/16-symbol sets,
  and the decoder reads them; what is missing is the frame-header bit plus
  the writer's set map (tile.rs:4208 -> the Luma*Set1/Set2 variants) and a
  rav1e-speed-6-shaped candidate list per class -- unblocked by a lane of its
  own. Note the shape this lane measured first: the wider a set gets, the
  more the FILM rows lose (this five-type set already costs film A +0.4 and
  film B +1.2), so that lane should gate by content from the start rather
  than measure an unconditional arm.`
* `deferred: libaom's own tx_type histogram on film B (syntax_census) -- the
  charter's instrument half. Not run: the two gate slots were the budget, and
  the decision it would have informed (which types to offer) was settled by
  the gate itself. Unblocked by one census run.`
