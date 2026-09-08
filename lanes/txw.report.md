# lane-txw -- the tile writer coded every transform unit as TX_CLASS_2D

## First divergent symbol

`EC_AV1_TXSET_WIDE=1 EC_AV1_REFPROBE=1 cargo test -p ec-av1 --release --lib
an_inter_clip_codes_both` refused on BWDREF ("a reference frame selected with
no picture at this frame's own ref_frame_idx slot"), raised out of the
ENCODER's own trial decode (`filter_search::pick_filters` ->
`decode_inter_frame_tiles_lr`). No symbol-stream diff was needed: the writer
side has no `TxClass` at all (`grep -n "TxClass" crates/ec-av1/src/tile.rs`
was empty), while the reader resolves one for every unit
(`decode::TxClass::of`). The FIRST symbol that can diverge is therefore the
`eob_pt` of the first `V_DCT`/`H_DCT` unit -- the writer coded it into the 2D
row, the reader reads `eob_pt_class1` -- and every context after it.

## Root cause

`tile::write_coeffs` treated every unit as `TX_CLASS_2D`:

* default zigzag scan instead of `Mrow_Scan`/`Mcol_Scan`
  (`decode::class_scan_table`),
* `coding.eob_pt` instead of the class-1 row (`eob_pt_class1`, the split
  lane-av1tx4 r5 put on the reader),
* `base_ctx`/`br_ctx` with the 2D taps, the 2D DC short-circuit and
  `NZ_MAP_CTX_OFFSET_32` instead of the 1-D taps and
  `nz_map_ctx_offset_1d`.

Under the reduced alphabets no search ever offered a 1-D type, so the writer
was right by accident. The `reduced_tx_set = 0` intra set (`TX_SET_INTRA_1`)
is the first alphabet whose candidate list holds `V_DCT`/`H_DCT`, so the
lever exposed it. The refusal is the `refusal-from-own-desync` class: the
desynced tile named a reference the frame holds no picture in.

## Class and sweep

Class: `writer half of a decoder-side split` -- a per-unit property the
reader derives (here `TxClass`) that the writer never mirrored. Swept every
writer site that codes coefficients:

* `tile::write_coeffs` -- the one coefficient writer (luma and chroma, all
  four square sides, both the tile writer and the pricer through
  `coeff_bits_typed`): FIXED. Every other pricer funnels through it, so the
  class reaches them with the fix: `coeff_bits_typed` (the search's price),
  `coeff_bits` and `predicted_coeff_bits`/`predicted_coeff_bits_sb` (the
  `#[cfg(test)]` pricers, `DCT_DCT` only by construction -- `TxClass::TwoD`)
  and `luma_32_coeff_bits` (32x32, which codes no `tx_type` symbol at all).
* `tile::rdoq` -- prices every candidate through `coeff_bits_typed`, so its
  DECISIONS were already class-correct, but it spends its budget from the
  TAIL of the scan backwards and read the 2D zigzag's tail on a 1-D unit:
  now walks `class_scan_of` (commit "RDOQ walked the 2D zigzag's tail").
* `tile::write_dc_coeffs` -- the fixed-CDF 32x32 luma DC path; 32x32 codes no
  `tx_type` symbol and is always `DCT_DCT`, so 2D is correct there.
* `tile::write_block_planes` chroma -- codes chroma as `DCT_DCT` while an
  INTER block's decoder INHERITS the luma type
  (`decode::reduce_inherited_chroma_tx_type`): a real sibling defect, see
  "deferred" below. Intra chroma takes its type from its own UV mode, never
  from luma, so the intra half is unaffected.
* `encode::recode_inter_chroma` -- reduces against a `FrameCtx` whose
  `reduced_tx_set_inter` the encoder NEVER sets (always `true`): same
  sibling.

## Fix

`crates/ec-av1/src/tile.rs`: `write_coeffs` resolves `crate::decode::TxClass::of(tx_type)`
once per unit and takes its scan (`class_scan_of`, cached, built from the
decoder's own `class_scan_table`), its `eob_pt` row (`write_eob` now picks
`eob_pt_class1` exactly as `decode::read_eob` does) and class-aware
`base_ctx`/`br_ctx` from it. `crates/ec-av1/src/decode.rs`: `TxClass`,
`TxClass::of`, `class_scan_table` and `nz_map_ctx_offset_1d` are
`pub(crate)` so the writer reads the reader's own tables (class
`table-and-reader-move-together`).

## Witness

`encoder::tests::a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly`
un-ignored as a blocker (it keeps a "run it alone" ignore: it sets
process-global search levers). At preset 0 it codes
`intra [962, 25435, 1630, 1665, 1888, 1820, 2435, 0..]` -- 1820 `V_DCT` and
2435 `H_DCT` units, the two types only `TX_SET_INTRA_1` can name -- 8724
bytes, and both decoders (ours and ffmpeg) read every frame sample-exact.

## Deferred

* `deferred: the INTER half of the widening (`encode::inter_luma_set` still
  names the narrow sets) -- with it widened the same refusal returns AFTER
  this fix, and the remaining cause is the chroma inheritance above: the
  encoder never sets `FrameCtx::reduced_tx_set_inter` (so
  `recode_inter_chroma` quantises chroma against the REDUCED allowance while
  a `reduced_tx_set = 0` decoder allows the full one) and
  `write_block_planes` codes chroma as `DCT_DCT` whatever luma chose --
  unblocked by threading the frame bit into the encoder's `FrameCtx` and
  giving the writer the inherited chroma type.
## Gate and decision

12-frame `bd_rate_screen_native`, SCREEN row (`EC_AV1_NATIVE_SCREEN=1`),
preset 0, control vs `EC_AV1_TXSET_WIDE=screen`:

| arm | ours (4 points) | BD vs libaom | BD vs rav1e | wall |
|---|---|---|---|---|
| control | 45.50/36806, 48.20/45672, 50.78/56996, 53.14/71310 | +19.8% | -30.5% | 92.8s |
| wide | 45.38/35791, 48.19/43938, 50.88/55290, 53.24/68250 | +14.8% | -32.9% | 123.3s |

The row's intra census goes `IDTX 7.5% / DCT_DCT 80.1% / ADST_ADST 3.3% /
ADST_DCT 5.3% / DCT_ADST 3.8%` to `IDTX 4.1% / DCT_DCT 78.0% / ADST_ADST
2.4% / ADST_DCT 3.0% / DCT_ADST 2.3% / V_DCT 2.8% / H_DCT 7.4%`: the win is
the two types that only exist in the wide set.

FILM rows (`EC_AV1_NATIVE_FILM4K=1`), the HEADER BIT ALONE -- the intra type
search left screen-gated as shipped, so a film frame searches no type and
only the ALPHABET it names changes (`EC_AV1_TXSET_WIDE=1`, every frame):

| row | control | bit on every frame |
|---|---|---|
| bars 2160p | +9.4% / -12.7%, 205409 B at point 1 | +9.6% / -12.6%, 208456 B |
| film B 2160p | +26.9% / -0.6%, 27442 B at point 1 | +27.2% / -0.3%, 27562 B |

So the answer to the charter's question is YES: the header bit alone MOVES
BYTES on a film. Every intra 8x8/4x4 luma unit still codes a `tx_type`
symbol, and coding `DCT_DCT` into the seven-symbol `TX_SET_INTRA_1` CDF is
not the same number of bits as coding it into the five-symbol
`TX_SET_INTRA_2` one -- both films come out 0.2/0.3 WORSE against libaom
(and 0.1/0.3 better against rav1e) for it. That fails the keep rule's film
half, which is exactly why the lever ships behind the SCREEN gate:
`encode::wide_tx_set` returns the frame's own screen flag, so a non-screen
frame writes `reduced_tx_set = 1` and the same bytes as before the lane.
The byte pins (8562 / 33357) unmoved with the lever ON are the in-suite
instance of that.

DECISION: `speed::WIDE_TX_SET[0] = true` (preset 0 only). Presets 1..6 carry
the type search but were not measured with the wider alphabets, so they stay
off; the wall cost at preset 0 is +33% on this row.

## Invariants

* `ec-av1` lib suite, split under `$HOME/.cache/txw` (final state, lever ON
  at preset 0): `--skip stream::` 332 passed / 31 ignored; `stream:: --skip
  10bit` 198 passed / 15 ignored; `10bit` 42 passed / 1 ignored -- 0 failed
  in all three (572 / 47 with the `10bit` rows counted twice; one earlier
  unsplit run of the same tree read 570 passed / 47 ignored). The witness
  KEEPS an `#[ignore]` -- it sets process-global levers, like
  `every_speed_preset` -- so the ignored count is unchanged from the
  charter's 47.
* `--include-ignored thread_count`: 2 passed. `--ignored
  every_speed_preset` (presets 0/3/6/10, both decoders sample-exact): 1
  passed. Byte pins: passed inside the first split, 8562 / 33357 unmoved
  WITH the lever on. `predicted_coeff_bits_track_the_tile_the_writer_wrote`
  and the facade identity are inside the same split.
* the witness at the final tree: `intra [949, 25443, 1659, 1660, 1891,
  1785, 2448, ..]` -- 1785 `V_DCT` + 2448 `H_DCT` -- 8724 bytes, ffmpeg and
  `decode_stream` both sample-exact.
* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0
  ec-av1 warnings (the 21 `ec-opus` + 1 `ec-vorbis` warnings pre-date the
  lane).
* `EC_COMP_MISMATCH` is a STALE charter item: the flag exists in no source
  file in this repo any more (`grep -rn COMP_MISMATCH crates/` matches only
  older lane reports).
