//! Inverse transforms (inverse DCT, inverse Walsh-Hadamard) and dequant
//! factor computation for VP8 (RFC 6386 §14).
//!
//! Every function here is a bit-exact transcription of the normative
//! reference implementation in libvpx (`vp8/common/idctllm.c` and
//! `vp8/common/quant_common.c`), which is also what RFC 6386 §§14.3–14.5
//! print as pseudocode. Bit-level fidelity of the rounding is a hard
//! requirement: arithmetic right shifts (`>>` on `i32` in Rust, as in C)
//! round toward negative infinity, and both implementations rely on that
//! floor behavior for negative operands.
//!
//! Fidelity details reproduced verbatim from the oracle:
//! - The 16-bit fixed-point constants `cospi8sqrt2minus1 = 20091` and
//!   `sinpi8sqrt2 = 35468` (§14.4). The `>> 16` multiplies carry **no**
//!   rounding constant; the `+4` before the final `>> 3` appears only in
//!   the second DCT pass.
//! - `short output[16]` intermediates: libvpx stores the first-pass
//!   results into a `short` array, truncating `i32` arithmetic to `i16`
//!   between passes. We reproduce those truncation points with `as i16`
//!   (wrapping two's-complement truncation, identical to the de facto C
//!   behavior) so results stay oracle-exact even for adversarial inputs.
//! - The inverse WHT rounds with `(x + 3) >> 3` (§14.3); the DC-only
//!   shortcut uses the same constant on `input[0]` alone.
//!
//! All functions are pure, allocation-free, and free of `unsafe`.

#![deny(unsafe_code)]

/// 16-bit fixed-point `sqrt(2) * cos(pi/8) - 1` (RFC 6386 §14.4). The
/// product `x * sqrt(2) * cos(pi/8)` is computed as `x + ((x * K) >> 16)`
/// because the constant exceeds 1.0 and would otherwise lose precision.
const COSPI8SQRT2MINUS1: i32 = 20091;

/// 16-bit fixed-point `sqrt(2) * sin(pi/8)` (RFC 6386 §14.4).
const SINPI8SQRT2: i32 = 35468;

/// DC quantizer lookup, `dc_qlookup[QINDEX_RANGE]` (RFC 6386 §14.1),
/// transcribed mechanically from libvpx `vp8/common/quant_common.c`.
const DC_QLOOKUP: [i16; 128] = [
    4, 5, 6, 7, 8, 9, 10, 10, 11, 12, 13, 14, 15, 16, 17, 17, 18, 19, 20, 20, 21, 21, 22, 22, 23,
    23, 24, 25, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 37, 38, 39, 40, 41, 42, 43, 44,
    45, 46, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 72, 73, 74, 75, 76, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 91,
    93, 95, 96, 98, 100, 101, 102, 104, 106, 108, 110, 112, 114, 116, 118, 122, 124, 126, 128, 130,
    132, 134, 136, 138, 140, 143, 145, 148, 151, 154, 157,
];

/// AC quantizer lookup, `ac_qlookup[QINDEX_RANGE]` (RFC 6386 §14.1),
/// transcribed mechanically from libvpx `vp8/common/quant_common.c`.
const AC_QLOOKUP: [i16; 128] = [
    4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52,
    53, 54, 55, 56, 57, 58, 60, 62, 64, 66, 68, 70, 72, 74, 76, 78, 80, 82, 84, 86, 88, 90, 92, 94,
    96, 98, 100, 102, 104, 106, 108, 110, 112, 114, 116, 119, 122, 125, 128, 131, 134, 137, 140,
    143, 146, 149, 152, 155, 158, 161, 164, 167, 170, 173, 177, 181, 185, 189, 193, 197, 201, 205,
    209, 213, 217, 221, 225, 229, 234, 239, 245, 249, 254, 259, 264, 269, 274, 279, 284,
];

/// Clamps a (possibly delta-adjusted) quantizer index to the valid
/// qindex range `[0, 127]` — dixie `clamp_q` / libvpx bounds in
/// `quant_common.c` (RFC 6386 §14.1).
#[inline]
fn clamp_q(q: i32) -> i32 {
    q.clamp(0, 127)
}

/// `dc_qlookup[clamp(q + delta)]` — Y-after-second / per-plane DC factor
/// lookup with delta adjustment (RFC 6386 §14.1 `dc_q`).
#[inline]
fn dc_factor(q: i32, delta: i32) -> i16 {
    DC_QLOOKUP[clamp_q(q + delta) as usize]
}

/// `ac_qlookup[clamp(q + delta)]` — AC factor lookup with delta
/// adjustment (RFC 6386 §14.1 `ac_q`).
#[inline]
fn ac_factor(q: i32, delta: i32) -> i16 {
    AC_QLOOKUP[clamp_q(q + delta) as usize]
}

/// Inverse Walsh-Hadamard transform of the 16 dequantized Y2 DC
/// coefficients into the 16 Y-subblock DC residuals
/// (RFC 6386 §14.3 `vp8_short_inv_walsh4x4_c`, oracle
/// `vp8_short_inv_walsh4x4_c` in `idctllm.c`).
///
/// `input` is the row-major 4x4 block of dequantized Y2 DCs; the returned
/// array holds the 16 Y-subblock DC values in the same row-major order
/// (the reference scatters `output[i]` to `mb_dqcoeff[i * 16]`, i.e. the
/// DC slot of the i-th Y subblock — the caller places them). Two passes
/// of the exact 4-point Hadamard butterfly, with `short` truncation of
/// the first-pass results and `(x + 3) >> 3` rounding in the second.
#[inline]
pub fn iwht4x4(input: &[i16; 16]) -> [i16; 16] {
    let mut tmp = [0i16; 16];

    // First pass: transform columns; results are stored transposed into
    // rows of `tmp` (ip walks input[i], input[4+i], input[8+i],
    // input[12+i]). libvpx stores into `short`: truncate to i16.
    for i in 0..4 {
        let a1 = input[i] as i32 + input[12 + i] as i32;
        let b1 = input[4 + i] as i32 + input[8 + i] as i32;
        let c1 = input[4 + i] as i32 - input[8 + i] as i32;
        let d1 = input[i] as i32 - input[12 + i] as i32;

        tmp[i] = (a1 + b1) as i16;
        tmp[4 + i] = (c1 + d1) as i16;
        tmp[8 + i] = (a1 - b1) as i16;
        tmp[12 + i] = (d1 - c1) as i16;
    }

    let mut output = [0i16; 16];

    // Second pass: transform rows in place, rounding with (x + 3) >> 3.
    for r in 0..4 {
        let i = 4 * r;
        let a1 = tmp[i] as i32 + tmp[i + 3] as i32;
        let b1 = tmp[i + 1] as i32 + tmp[i + 2] as i32;
        let c1 = tmp[i + 1] as i32 - tmp[i + 2] as i32;
        let d1 = tmp[i] as i32 - tmp[i + 3] as i32;

        let a2 = a1 + b1;
        let b2 = c1 + d1;
        let c2 = a1 - b1;
        let d2 = d1 - c1;

        output[i] = ((a2 + 3) >> 3) as i16;
        output[i + 1] = ((b2 + 3) >> 3) as i16;
        output[i + 2] = ((c2 + 3) >> 3) as i16;
        output[i + 3] = ((d2 + 3) >> 3) as i16;
    }

    output
}

/// One-nonzero-DC shortcut of the inverse WHT
/// (RFC 6386 §14.3 `vp8_short_inv_walsh4x4_1_c`, oracle
/// `vp8_short_inv_walsh4x4_1_c` in `idctllm.c`).
///
/// When only `input[0]` is non-zero, every Y-subblock DC equals
/// `(input[0] + 3) >> 3`. Bit-identical to `iwht4x4` for such inputs.
#[inline]
pub fn iwht4x4_dc(input: &[i16; 16]) -> [i16; 16] {
    let a1 = ((input[0] as i32 + 3) >> 3) as i16;
    [a1; 16]
}

/// Full inverse DCT of one dequantized 4x4 coefficient block
/// (RFC 6386 §14.4; oracle `short_idct4x4llm_c` in `idctllm.c`).
///
/// `coeffs` is a row-major 4x4 block of **already dequantized**
/// coefficients (dequantization happens before the transform, RFC 6386
/// §14.5). Two passes of the 4-point inverse DCT with the §14.4
/// fixed-point constants; the `>> 16` multiplies carry no rounding
/// constant and the `(x + 4) >> 3` rounding is applied only in the
/// second pass. First-pass results are truncated to `i16` exactly where
/// the oracle stores into its `short output[16]` scratch.
///
/// Returns the row-major 4x4 residual. Not clamped, not added to the
/// prediction — see [`idct4x4_add`] for the combined form.
#[inline]
pub fn idct4x4(coeffs: &[i16; 16]) -> [i16; 16] {
    let mut tmp = [0i16; 16];

    // First pass: transform columns; results land transposed in rows of
    // `tmp` (ip walks coeffs[i], coeffs[4+i], coeffs[8+i], coeffs[12+i]).
    for i in 0..4 {
        let a1 = coeffs[i] as i32 + coeffs[8 + i] as i32;
        let b1 = coeffs[i] as i32 - coeffs[8 + i] as i32;

        let temp1 = (coeffs[4 + i] as i32 * SINPI8SQRT2) >> 16;
        let temp2 = coeffs[12 + i] as i32 + ((coeffs[12 + i] as i32 * COSPI8SQRT2MINUS1) >> 16);
        let c1 = temp1 - temp2;

        let temp1 = coeffs[4 + i] as i32 + ((coeffs[4 + i] as i32 * COSPI8SQRT2MINUS1) >> 16);
        let temp2 = (coeffs[12 + i] as i32 * SINPI8SQRT2) >> 16;
        let d1 = temp1 + temp2;

        // libvpx stores into `short`: truncate to i16.
        tmp[i] = (a1 + d1) as i16;
        tmp[12 + i] = (a1 - d1) as i16;
        tmp[4 + i] = (b1 + c1) as i16;
        tmp[8 + i] = (b1 - c1) as i16;
    }

    let mut output = [0i16; 16];

    // Second pass: transform rows, rounding with (x + 4) >> 3.
    for r in 0..4 {
        let i = 4 * r;
        let ip0 = tmp[i] as i32;
        let ip1 = tmp[i + 1] as i32;
        let ip2 = tmp[i + 2] as i32;
        let ip3 = tmp[i + 3] as i32;

        let a1 = ip0 + ip2;
        let b1 = ip0 - ip2;

        let temp1 = (ip1 * SINPI8SQRT2) >> 16;
        let temp2 = ip3 + ((ip3 * COSPI8SQRT2MINUS1) >> 16);
        let c1 = temp1 - temp2;

        let temp1 = ip1 + ((ip1 * COSPI8SQRT2MINUS1) >> 16);
        let temp2 = (ip3 * SINPI8SQRT2) >> 16;
        let d1 = temp1 + temp2;

        output[i] = ((a1 + d1 + 4) >> 3) as i16;
        output[i + 3] = ((a1 - d1 + 4) >> 3) as i16;
        output[i + 1] = ((b1 + c1 + 4) >> 3) as i16;
        output[i + 2] = ((b1 - c1 + 4) >> 3) as i16;
    }

    output
}

/// DC-only inverse DCT shortcut (RFC 6386 §14.4
/// `vp8_dc_only_idct_add_c` math; oracle `idctllm.c`): when position 0 is
/// the sole non-zero coefficient, every residual pixel is `(dc + 4) >> 3`.
///
/// Bit-identical to `idct4x4` on such inputs. Not clamped; the caller
/// adds and clamps (or uses [`idct4x4_add`]).
#[inline]
pub fn idct4x4_dc(dc: i16) -> [i16; 16] {
    let a1 = ((dc as i32 + 4) >> 3) as i16;
    [a1; 16]
}

/// Combined per-block reconstruction kernel (RFC 6386 §14.4/§14.5; oracle
/// `short_idct4x4llm_c` final loop in `idctllm.c`): inverse-transforms
/// `coeffs` and writes `clamp(predict + residual)` into `recon`.
///
/// `coeffs` must already be dequantized (dequantization is applied before
/// the transform; for intra Y macroblocks the DC positions already hold
/// the [`iwht4x4`] output). `predict` and `recon` are row-major 4x4
/// blocks of exactly 16 bytes each. Residuals are fully computed before
/// any output byte is written.
#[inline]
pub fn idct4x4_add(coeffs: &[i16; 16], predict: &[u8], recon: &mut [u8]) {
    let residual = idct4x4(coeffs);
    for i in 0..16 {
        let a = residual[i] as i32 + predict[i] as i32;
        recon[i] = a.clamp(0, 255) as u8;
    }
}

/// Dequantization factors for one macroblock, indexed by plane and
/// coefficient type (RFC 6386 §14.1; dixie `struct dequant_factors`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dequant {
    /// Y plane, DC coefficient (after inverse WHT for intra Y2 rows).
    pub y1_dc: i16,
    /// Y plane, AC coefficients.
    pub y1_ac: i16,
    /// Y2 plane (second-order) DC coefficient: `dc_qlookup[...] * 2`.
    pub y2_dc: i16,
    /// Y2 plane AC coefficient: `ac_qlookup[...] * 155 / 100`, floored at 8.
    pub y2_ac: i16,
    /// Chroma DC coefficient, capped at 132 after lookup.
    pub uv_dc: i16,
    /// Chroma AC coefficient.
    pub uv_ac: i16,
}

/// Computes the six dequantization factors for quantizer index `q` with
/// the per-frame delta updates (RFC 6386 §9.6 header fields applied per
/// §14.1; dixie `dequant_init`, oracle `vp8cx_init_quantizer` in
/// libvpx `decodframe.c` / `quant_common.c`).
///
/// `q` is the per-macroblock segment-adjusted base qindex (already
/// absolute or offset by the segment level per the segment header); the
/// delta arguments are the frame-header updates. Each lookup clamps its
/// `q + delta` sum to `[0, 127]`. Semantics transcribed exactly:
/// - `y1_dc = dc_qlookup[clamp(q + y1_dc_delta)]`,
///   `y1_ac = ac_qlookup[clamp(q)]` (no delta exists for Y AC),
/// - `y2_dc = dc_qlookup[clamp(q + y2_dc_delta)] * 2`,
/// - `y2_ac = ac_qlookup[clamp(q + y2_ac_delta)] * 155 / 100` (integer
///   division), floored at 8,
/// - `uv_dc = min(dc_qlookup[clamp(q + uv_dc_delta)], 132)` (cap applied
///   after the lookup),
/// - `uv_ac = ac_qlookup[clamp(q + uv_ac_delta)]`.
#[must_use]
pub fn dequant(
    q: i32,
    y1_dc_delta: i32,
    y2_dc_delta: i32,
    y2_ac_delta: i32,
    uv_dc_delta: i32,
    uv_ac_delta: i32,
) -> Dequant {
    Dequant {
        y1_dc: dc_factor(q, y1_dc_delta),
        y1_ac: ac_factor(q, 0),
        y2_dc: dc_factor(q, y2_dc_delta) * 2,
        y2_ac: {
            let v = (ac_factor(q, y2_ac_delta) as i32 * 155) / 100;
            (if v < 8 { 8 } else { v }) as i16
        },
        uv_dc: {
            let v = dc_factor(q, uv_dc_delta);
            if v > 132 { 132 } else { v }
        },
        uv_ac: ac_factor(q, uv_ac_delta),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-derived: impulse `input[0] = 8`, all else 0. First pass, the
    /// only touched column (0) reads (8,0,0,0): a1 = 8+0 = 8, b1 = 0,
    /// c1 = 0, d1 = 8, giving intermediate column 0 = [8,8,8,8]. Second
    /// pass, every row reads (8,0,0,0): a2 = b2 = c2 = d2 = 8, and
    /// (8 + 3) >> 3 = 1. Expected: all 16 outputs are 1.
    #[test]
    fn iwht_unit_impulse_yields_all_ones() {
        let mut input = [0i16; 16];
        input[0] = 8;
        assert_eq!(iwht4x4(&input), [1i16; 16]);
    }

    /// Hand-derived: impulse `input[5] = 8` (row 1, col 1). First pass,
    /// column 1 reads (0,8,0,0): a1 = 0, b1 = 8, c1 = 8, d1 = 0, giving
    /// intermediate column 1 = [8, 8, -8, -8]. Second pass, rows 0-1 read
    /// (0,8,0,0) → outputs (1, 1, -1, -1); rows 2-3 read (0,-8,0,0) →
    /// (-1, -1, 1, 1) — the Hadamard basis pattern for position (1,1).
    #[test]
    fn iwht_impulse_at_row1_col1_yields_hadamard_pattern() {
        let mut input = [0i16; 16];
        input[5] = 8;
        assert_eq!(
            iwht4x4(&input),
            [
                1, 1, -1, -1, //
                1, 1, -1, -1, //
                -1, -1, 1, 1, //
                -1, -1, 1, 1,
            ]
        );
    }

    /// Mixed-coefficient oracle: expected values computed with a Python
    /// model of the exact libvpx integer algorithm (see the model in
    /// `idct_mixed_block_matches_oracle` below — the WHT variant is the
    /// same structure with §14.3 butterflies and `(x + 3) >> 3`).
    #[test]
    fn iwht_mixed_block_matches_oracle() {
        let input = [
            40, -13, 0, 7, 222, 5, -40, 1, 0, 0, 17, -9, 300, -250, 88, 3,
        ];
        assert_eq!(
            iwht4x4(&input),
            [
                46, 30, 95, 110, //
                9, 42, -15, -55, //
                -3, -35, 37, 60, //
                -36, -27, -86, -92,
            ]
        );
    }

    /// The `_dc` shortcut must agree with the full transform whenever
    /// only `input[0]` is non-zero — across the whole practical range,
    /// including negatives and both i16 extremes (arithmetic shifts are
    /// floor rounding for negatives in both C and Rust).
    #[test]
    fn iwht_dc_shortcut_matches_full_transform() {
        for v in [
            0,
            1,
            -1,
            3,
            -3,
            8,
            -8,
            100,
            -100,
            1234,
            -4321,
            i16::MAX,
            i16::MIN,
        ] {
            let input = {
                let mut i = [0i16; 16];
                i[0] = v;
                i
            };
            assert_eq!(iwht4x4(&input), iwht4x4_dc(&input), "mismatch at dc={v}");
        }
    }

    /// §14.4 DC-only math: input [40, 0, ...] gives every pixel
    /// (40 + 4) >> 3 = 5.
    #[test]
    fn idct_dc_only_40_yields_five() {
        let coeffs = {
            let mut c = [0i16; 16];
            c[0] = 40;
            c
        };
        assert_eq!(idct4x4(&coeffs), [5i16; 16]);
        assert_eq!(idct4x4_dc(40), [5i16; 16]);
    }

    /// Mixed-coefficient oracle. The Python model below is a line-for-line
    /// transcription of libvpx `short_idct4x4llm_c` (same integer math,
    /// `short` truncation between passes via `s16`); running it on
    /// `coeffs` prints the expected block embedded in the assertion.
    ///
    /// ```python
    /// COS, SIN = 20091, 35468
    /// def s16(x):
    ///     x &= 0xFFFF
    ///     return x - 0x10000 if x >= 0x8000 else x
    /// def idct(inp):
    ///     out = [0] * 16
    ///     for i in range(4):  # pass 1: columns, transposed into rows
    ///         a1 = inp[i] + inp[8+i]; b1 = inp[i] - inp[8+i]
    ///         c1 = ((inp[4+i]*SIN) >> 16) - (inp[12+i] + ((inp[12+i]*COS) >> 16))
    ///         d1 = (inp[4+i] + ((inp[4+i]*COS) >> 16)) + ((inp[12+i]*SIN) >> 16)
    ///         out[i] = s16(a1 + d1); out[12+i] = s16(a1 - d1)
    ///         out[4+i] = s16(b1 + c1); out[8+i] = s16(b1 - c1)
    ///     res = [0] * 16
    ///     for i in range(4):  # pass 2: rows, rounding (x + 4) >> 3
    ///         ip = out[4*i:4*i+4]
    ///         a1 = ip[0] + ip[2]; b1 = ip[0] - ip[2]
    ///         c1 = ((ip[1]*SIN) >> 16) - (ip[3] + ((ip[3]*COS) >> 16))
    ///         d1 = (ip[1] + ((ip[1]*COS) >> 16)) + ((ip[3]*SIN) >> 16)
    ///         res[4*i+0] = s16((a1 + d1 + 4) >> 3)
    ///         res[4*i+3] = s16((a1 - d1 + 4) >> 3)
    ///         res[4*i+1] = s16((b1 + c1 + 4) >> 3)
    ///         res[4*i+2] = s16((b1 - c1 + 4) >> 3)
    ///     return res
    /// ```
    #[test]
    fn idct_mixed_block_matches_oracle() {
        let coeffs = [
            87, -43, 210, 5, -19, 0, 33, -77, 1234, -517, 9, 401, -88, 2100, -3, 55,
        ];
        assert_eq!(
            idct4x4(&coeffs),
            [
                305, 108, 139, 72, //
                -508, -228, -88, 303, //
                372, 85, -443, -640, //
                -48, -41, 345, 441,
            ]
        );
    }

    /// The DC-only shortcut must equal the full transform for any value
    /// at coefficient position 0 (with the rest zero), including the i16
    /// extremes where the intermediate still fits.
    #[test]
    fn idct_dc_shortcut_matches_full_transform() {
        for v in [
            0,
            1,
            -1,
            8,
            -8,
            40,
            -40,
            100,
            -100,
            4088,
            i16::MAX,
            i16::MIN,
        ] {
            let coeffs = {
                let mut c = [0i16; 16];
                c[0] = v;
                c
            };
            assert_eq!(idct4x4(&coeffs), idct4x4_dc(v), "mismatch at dc={v}");
        }
    }

    /// §14.5: `idct4x4_add` is `clamp255(predict + idct4x4(coeffs))`
    /// pixelwise. With a flat prediction of 100, every output byte must
    /// be the saturated sum; extreme residuals clamp to 0 and 255.
    #[test]
    fn idct_add_equals_clamped_predict_plus_residual() {
        let coeffs = [
            87, -43, 210, 5, -19, 0, 33, -77, 1234, -517, 9, 401, -88, 2100, -3, 55,
        ];
        let residual = idct4x4(&coeffs);
        let predict = [100u8; 16];
        let mut recon = [0u8; 16];
        idct4x4_add(&coeffs, &predict, &mut recon);
        for i in 0..16 {
            let expected = (residual[i] as i32 + 100).clamp(0, 255) as u8;
            assert_eq!(recon[i], expected, "pixel {i}");
        }

        // Saturation: DC 4088 → residual (4088 + 4) >> 3 = 511; 100 + 511
        // clamps to 255. Negative DC -32768 → residual (-32764) >> 3 =
        // -4096; clamps to 0.
        let mut hi = [0i16; 16];
        hi[0] = 4088;
        let mut lo = [0i16; 16];
        lo[0] = i16::MIN;
        let mut recon = [0u8; 16];
        idct4x4_add(&hi, &predict, &mut recon);
        assert_eq!(recon, [255u8; 16]);
        idct4x4_add(&lo, &predict, &mut recon);
        assert_eq!(recon, [0u8; 16]);
    }

    /// `idct4x4_add` over an existing recon block that already holds the
    /// prediction (the common libvpx dst/pred shape). The `&[u8]` /
    /// `&mut [u8]` signature statically excludes predict/recon aliasing.
    #[test]
    fn idct_add_updates_existing_recon_buffer() {
        let coeffs = {
            let mut c = [0i16; 16];
            c[0] = 40;
            c
        };
        let predict = [100u8; 16];
        let mut recon = [100u8; 16];
        idct4x4_add(&coeffs, &predict, &mut recon);
        assert_eq!(recon, [105u8; 16]);
    }

    /// §14.1 tables, transcribed from libvpx `quant_common.c`: exact
    /// length plus boundary and known-trap entries (the duplicated 10 at
    /// dc[6..7], the >89 acceleration at dc[95..96] and dc[111..112], and
    /// the ac[54..55] and ac[84..85] jumps).
    #[test]
    fn quant_lookup_tables_match_libvpx() {
        assert_eq!(DC_QLOOKUP.len(), 128);
        assert_eq!(AC_QLOOKUP.len(), 128);
        assert_eq!(&DC_QLOOKUP[..3], &[4, 5, 6]);
        assert_eq!(DC_QLOOKUP[127], 157);
        assert_eq!(AC_QLOOKUP[0], 4);
        assert_eq!(AC_QLOOKUP[127], 284);
        assert_eq!(&DC_QLOOKUP[6..=8], &[10, 10, 11]);
        assert_eq!(&DC_QLOOKUP[95..=97], &[89, 91, 93]);
        assert_eq!(&DC_QLOOKUP[111..=112], &[118, 122]);
        assert_eq!(&AC_QLOOKUP[53..=55], &[57, 58, 60]);
        assert_eq!(&AC_QLOOKUP[83..=85], &[116, 119, 122]);
    }

    /// §14.1 boundary qindices: q=0 gives the minimum factors (with the
    /// Y2 AC floor of 8 already active), q=127 gives the maxima including
    /// the chroma DC cap at 132.
    #[test]
    fn dequant_at_qindex_boundaries() {
        assert_eq!(
            dequant(0, 0, 0, 0, 0, 0),
            Dequant {
                y1_dc: 4,
                y1_ac: 4,
                y2_dc: 8,
                y2_ac: 8,
                uv_dc: 4,
                uv_ac: 4
            }
        );
        assert_eq!(
            dequant(127, 0, 0, 0, 0, 0),
            Dequant {
                y1_dc: 157,
                y1_ac: 284,
                y2_dc: 314,
                y2_ac: 440, // 284 * 155 / 100
                uv_dc: 132, // 157 capped at 132
                uv_ac: 284,
            }
        );
    }

    /// Delta-adjusted qindices clamp to [0, 127] before each lookup, for
    /// every delta channel (RFC 6386 §14.1 dixie `clamp_q`).
    #[test]
    fn dequant_delta_indices_clamp_to_qindex_range() {
        let lo = dequant(0, -1000, -1000, -1000, -1000, -1000);
        assert_eq!(lo.y1_dc, 4);
        assert_eq!(lo.y2_dc, 8); // 4 * 2
        assert_eq!(lo.y2_ac, 8); // floor
        assert_eq!(lo.uv_dc, 4);
        assert_eq!(lo.uv_ac, 4);

        let hi = dequant(127, 1000, 1000, 1000, 1000, 1000);
        assert_eq!(hi.y1_dc, 157);
        assert_eq!(hi.y2_dc, 314);
        assert_eq!(hi.y2_ac, 440);
        assert_eq!(hi.uv_dc, 132);
        assert_eq!(hi.uv_ac, 284);

        // A base q of 0 with positive deltas reaches mid-table: q=60 has
        // dc_qlookup[60] = 55, ac_qlookup[60] = 70.
        let mid = dequant(0, 60, 60, 60, 60, 60);
        assert_eq!(mid.y1_dc, 55);
        assert_eq!(mid.y1_ac, 4); // Y AC has no delta channel
        assert_eq!(mid.y2_dc, 110);
        assert_eq!(mid.y2_ac, 70 * 155 / 100); // 108
        assert_eq!(mid.uv_dc, 55);
        assert_eq!(mid.uv_ac, 70);
    }

    /// The Y2 AC floor: `x * 155 / 100 < 8` becomes 8. At q=0..4
    /// (ac = 4..8) the scaled values are 6, 7, 9, 10, 12 — only the first
    /// two floor up.
    #[test]
    fn dequant_y2_ac_floor_at_eight() {
        for q in 0..=4i32 {
            let ac = AC_QLOOKUP[q as usize] as i32;
            let scaled = ac * 155 / 100;
            assert_eq!(
                dequant(q, 0, 0, 0, 0, 0).y2_ac as i32,
                scaled.max(8),
                "q={q}"
            );
        }
        assert_eq!(dequant(0, 0, 0, 0, 0, 0).y2_ac, 8); // 4*155/100 = 6 → 8
        assert_eq!(dequant(1, 0, 0, 0, 0, 0).y2_ac, 8); // 5*155/100 = 7 → 8
        assert_eq!(dequant(2, 0, 0, 0, 0, 0).y2_ac, 9); // 6*155/100 = 9
    }

    /// libvpx computes the Y2 AC factor as `(x * 101581) >> 16` and its
    /// comment asserts bitwise equality with `x * 155 / 100` for all
    /// table entries; our transcription of dixie uses `* 155 / 100`, so
    /// sweep the whole qindex range and prove the two agree through the
    /// public API (with a matching delta for the Y2 AC channel).
    #[test]
    fn dequant_y2_ac_matches_libvpx_fixed_point_over_all_q() {
        for q in 0..=127i32 {
            for delta in [0, -q - 1, 127 - q] {
                let x = AC_QLOOKUP[clamp_q(q + delta) as usize] as i32;
                let expected = (((x * 101581) >> 16).max(8)) as i16;
                assert_eq!(
                    dequant(q, 0, 0, delta, 0, 0).y2_ac,
                    expected,
                    "q={q}, delta={delta}"
                );
            }
        }
    }
}
