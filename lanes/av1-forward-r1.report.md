# lane-av1-forward r1 — forward transform and quantizer for ec-av1

## What landed

`crates/ec-av1/src/transform.rs` gains the encoder half of the transform:

- `forward_transform_2d(residual, side) -> Vec<f64>` — an orthonormal DCT-II in
  double precision (rows, then columns), scaled to the decoder's fixed-point
  gain.
- `quantize(coeffs, side, bit_depth, q_idx, deadzone) -> Vec<i32>` — levels,
  with a deadzone as a rounding offset, dropping everything outside the coded
  top-left 32x32 of a 64-point transform.
- `forward_and_quantize` — the two composed.

AV1 specifies only the inverse transform. The gain the two ends have to agree
on was **measured, not assumed**: feeding one dequantized coefficient through
`inverse_transform_2d` reproduces its orthonormal basis function scaled by
`dq_denom(side) / 8`, at every size and every coefficient position. The
dequantizer has already divided by `dq_denom`, so the two cancel, and what the
encoder owes a level is size-independent:

    level = 8 * orthonormal(residual) / q

## Gates (all in `cargo test -p ec-av1`, 63 green)

| test | what it holds |
|---|---|
| `the_inverse_network_is_an_orthonormal_dct_over_eight` | the gain constant itself, per size and per position, against a basis computed independently of `dct_basis`; fit as well as scale, so mixing positions cannot pass |
| `a_fine_quantizer_roundtrips_a_residual_almost_exactly` | rmse < 1 sample at q_idx 10, sizes 4/8/16/32 |
| `the_roundtrip_error_is_the_quantizer_and_only_the_quantizer` | error rises monotonically with q and stays under one step — a wrong gain constant shows up as a floor a finer quantizer cannot get under |
| `a_64x64_transform_keeps_what_fits_in_its_coded_quarter` | band-limited 64x64 roundtrips to rmse < 1.5; white noise loses exactly the sqrt(3/4) of its magnitude that lives outside the coded quarter, and that loss does not move with the quantizer |
| `a_wider_deadzone_codes_fewer_coefficients` | monotone in both coefficient count and error |
| `negating_the_residual_negates_every_level` | no offset of its own |
| `a_flat_residual_is_a_dc_level_alone` | DC exactly `round(8 * 40 * side / dc_q)`, every AC zero, all five sizes |
| `a_quantized_picture_decodes_to_what_the_encoder_reconstructed` | end to end: a picture -> forward -> quantize -> tile -> **ffmpeg** -> sample-exact against the encoder's own reconstruction, and within a quarter step of the picture asked for (2.2 of 14 as written) |

## Mutation kills

| mutation | killed by |
|---|---|
| gain constant 8 -> 8*sqrt(2) (the value a coarser fit had suggested) | 5 tests |
| forward column pass transposed (`basis[u*side+i]` -> `basis[i*side+u]`) | 6 tests, including the ffmpeg end-to-end one |

The transpose is the reason the end-to-end tolerance is a quarter step rather
than a whole one: at a whole step the transposed forward measured 13.3 against
a bound of 14 and survived. An energy-preserving-but-wrong transform passes a
loose fidelity gate.

## Class named

**Constant fitted from a composite measurement.** The first pass took the gain
as `dq_denom/(8*sqrt(2))` from a roundtrip rmse that was flat across quantizers
— a composite number in which a wrong scale and a wrong basis are
indistinguishable. Probing the inverse with a *single* coefficient and
least-squares-fitting one scalar gave `dq_denom/8` immediately, with the fit
residual proving it was the whole story. Sweep: any constant in this crate that
was derived from a roundtrip rather than from an isolated probe.

## Not in this lane

- transform types other than DCT_DCT (ADST, identity, WHT) — the writer codes
  only DCT_DCT
- rectangular transform sizes
- quantizer matrices, segment-level q deltas
- the rate-distortion loop that would choose the deadzone and the levels; the
  deadzone is a parameter here, unswept
