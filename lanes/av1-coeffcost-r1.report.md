# lane-av1-coeffcost r1 — the mode search prices its coefficients exactly

The directional lane closed with the mode half of the rate term costed through
the writer's own CDFs and the level half still an estimate
(`2 + 2*log2(|level|+1)` per non-zero coefficient). This lane replaces that
estimate: a trial's levels are now priced through the very syntax and the very
tables `write_coeffs` will code them with.

## What landed

- `crates/ec-av1/src/msac.rs`: `SymbolEncoder` keeps a bit account.
  `symbol_fixed` prices *the interval narrowing it performs*, not the nominal
  probability of the table entry — the coder works its range through an
  eight-bit multiply and gives every symbol above the coded one a floor of
  `EC_MIN_PROB`, so the table entry is 8% off the truth on real coefficient
  data while the narrowing is exact. `bits()` and `reset_bits()` expose it.
- `crates/ec-av1/src/tile.rs`: `luma_32_coeff_bits()` — what one 32x32 luma
  transform block's levels cost, run through `write_coeffs` on a throwaway
  encoder: end-of-block group and offset, base levels, base range, DC sign,
  raw signs and the Golomb tail. The two neighbour-derived contexts (block
  skip, DC sign) are neutral because the search runs before the block's
  neighbours exist; every context read from the block's own levels is exact.
- `crates/ec-av1/src/encode.rs`: `Trial::bits` calls it. Chroma is not
  searched, so only the 32x32 luma table is asked for.
- `EC_AV1_LAMBDA` and `EC_AV1_ESTIMATE` sweep knobs, both `cfg!(test)`-only,
  so the two rate terms and any weight can be compared on one build.

## Gates

| Gate | Result |
| --- | --- |
| `the_priced_coefficient_bits_match_the_bytes_written` | three grids (lone DC, sparse ±small spread, levels past the base-range alphabet) priced within 2% of the bytes one encoder really spends |
| `the_bit_account_matches_the_bytes_written` (msac) | 20 000 skewed symbols, within 1% |
| `the_price_grows_with_the_levels` | empty < one coefficient < a bigger one; empty < a spread of 32; an empty block costs one skip symbol |
| full `ec-av1` suite | 78 passed, 0 failed, 3 ignored |
| `cargo clippy --release -p ec-av1 --all-targets` | 0 warnings |

## Mutation kills

| Mutation | Killed by |
| --- | --- |
| the price counts non-zero coefficients at 4 bits each | `the_price_grows_with_the_levels`, `the_priced_coefficient_bits_match_the_bytes_written` |
| the price is zero | those two, plus `the_search_beats_dc_alone` and `the_search_picks_the_direction_the_picture_runs` |
| the account adds one bit per symbol instead of pricing the narrowing | `the_priced_coefficient_bits_match_the_bytes_written` |

## What it is worth (`probe_ladder`, BD-rate against the merged estimate arm)

Three-point ladder (base_q_idx 110/90/70), luma PSNR. Negative is better.

| Picture | exact @ 0.1 | exact @ 0.2 | estimate @ 0.05 |
| --- | --- | --- | --- |
| Troy, 1080p film frame @600s, 640x352 | **-1.19%** | -0.98% | -0.08% |
| OBS screen capture 2026-08-25 16-27-54 @5s, 640x352 | **-0.20%** | -0.14% | +0.21% |
| test card 160x96 | -2.09% | -2.10% | +0.16% |
| diagonal 160x96 | -3.93% | -3.93% | +0.06% |
| stripes 160x96 | +0.00% | +0.00% | +0.51% |

`LAMBDA_SCALE` stays 0.1: it is the best point on both of his clips, and 0.2
buys nothing on the synthetics. `probe_directional` at 0.1 with the exact term
reports the six directional modes worth -0.48% on the film frame and +0.06% on
screen capture against the seven non-directional ones (-53.0% → -54.90% on the
synthetic diagonal), with 43 of 220 and 14 of 220 blocks directional.

Both clips are his own; no fixture-only claim in this table.

## Dispositions

- The block-skip and DC-sign contexts are neutral in the price while the writer
  reads them from neighbours: **accepted**. They cost at most one symbol per
  block and the search has no neighbour state to read at that point.
- Chroma trials are priced at zero bits: **accepted**. Chroma is coded DC-only
  and is not searched, so no ranking depends on it.
- Only the 32x32 luma table is priced: **accepted** for as long as the encoder
  emits split superblocks only. A whole-64x64 decision would need `LUMA_64`,
  and the table and the scan have to agree, so it takes an entry point of its
  own rather than a size argument.

## Not in this lane

- Block sizes below 32x32 and the partition decision (needs the luma-16 CDF
  tables and `EOB_PT_256_LUMA`).
- Transform types beyond `DCT_DCT`, and transform-size selection.
- CDF adaptation: every symbol is still written against the pinned tables, so
  the price is the price of a non-adaptive stream.
- Trellis or any rate-distortion optimisation of the levels themselves; this
  lane prices the levels the quantizer already chose, it does not change them.
