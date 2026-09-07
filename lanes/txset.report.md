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
* Lever: `speed::TX_TYPE_SEARCH` (presets 0-6 on, 7-10 off), `EC_AV1_TXSET=0`
  forces it off and restores the pre-lane bytes.

## 4. Checks

| check | result |
|---|---|
| `transform::tests::a_typed_forward_round_trips_like_the_dct_one` (16 types) | PASS |
| `encoder::tests::an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` | PASS -- hits `[IDTX 12, DCT_DCT 15445, ADST_ADST 70, ADST_DCT 161, DCT_ADST 467]`, ours == ffmpeg every sample |
| `cargo test -p ec-av1 --lib --release round_trip` (19 tests) | PASS |
| `predicted_coeff_bits_track_the_tile_the_writer_wrote`, `tile_bytes_do_not_depend_on_the_thread_count --include-ignored`, `the_facade_codes_the_same_bytes_as_encode_sequence` | PASS |
| `cargo check -p ec-av1 --all-targets` | 0 errors, 0 warnings |

