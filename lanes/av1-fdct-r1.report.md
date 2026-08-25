# lane-av1-fdct r1 — dequantization and the inverse DCT

## What landed

`ec-av1` can now compute the residual a decoder will add to its prediction,
which is the piece an encoder needs before it can choose coefficients by
rate-distortion rather than by construction.

- `crates/ec-av1/src/quant.rs` — `dc_q`/`ac_q` (spec 7.12.2) over the vendored
  8/10/12-bit tables, `dq_denom` and the per-coefficient dequantization of spec
  7.12.3 steps a-f (the 24-bit mask, the divide toward zero, the clip).
- `crates/ec-av1/src/transform.rs` — `cos128`/`sin128`, `brev`, the butterfly
  `B` and Hadamard `H` of spec 7.13.2.1, the array permutation of 7.13.2.2, the
  whole inverse DCT stage list of 7.13.2.3 for `n = 2..6`, and the 2D driver of
  7.13.3 for square `DCT_DCT`. Written from the specification text, not
  transcribed from another encoder.

## Evidence

`cargo test --release -p ec-av1` — 55 passed, 0 failed.

The gates, in order of what they would catch:

| test | claim |
|---|---|
| `the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block` | eight coefficients, both signs, DC to the far corner; every one of 1024 samples equals what ffmpeg decodes |
| `..._for_a_64x64_block` | the same at 64x64, where the spec zeroes the untransmitted half before the row transform; all 4096 samples |
| `the_inverse_transform_reproduces_the_pinned_whole_superblock_residuals` | all 28 levels of `DECODED_AT_Q100`, the table pinned from a real decoder |
| `the_inverse_transform_reproduces_the_pinned_split_residuals` | all 28 levels of `SPLIT_RESIDUAL_AT_Q100` |
| `a_single_ac_coefficient_varies_along_its_own_axis` | a row basis is constant down its columns and is the transpose of the column basis, at every size |
| `a_dc_level_is_worth_half_as_much_each_time_the_transform_doubles` | holds the row shifts of the four sizes the frame syntax cannot reach |
| `negating_the_levels_negates_the_residual` | the dequantizer rounds toward zero, not down |

Mutation kills run by hand, both reverted:

- one `Cos128_Lookup` entry off by one (2896 -> 2897): 2 tests fail.
- the 8x8 row shift 1 -> 0: `a_dc_level_is_worth_half...` fails. Without that
  test the mutation survived, which is why it exists — the frame syntax only
  codes 32x32 and 64x64, so nothing else reaches the small sizes.

`cargo clippy --release -p ec-av1 --all-targets`: clean. `cargo fmt -p ec-av1`
applied.

## Not in this lane

- ADST, identity and WHT transforms, and the rectangular sizes. The encoder
  codes none of them; the DCT stage list is already general over `n = 2..6`.
- Quantization matrices (`using_qmatrix`), which the frame header never sets.
- Choosing coefficients with this: the residual model is here, the
  rate-distortion loop that would consume it is the next lane.
