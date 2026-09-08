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
  `coeff_bits_typed`): FIXED.
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
* `deferred: the BD arms for shipping `WIDE_TX_SET` at preset 0 -- see the
  gate section.
