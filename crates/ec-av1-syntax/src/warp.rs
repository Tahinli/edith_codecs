//! The shear validity of a global motion model (spec 7.11.3.6, 7.11.3.7).
//!
//! `VAWarpedMotionParamsAV1` carries an `invalid` flag alongside the warp
//! matrix, because a syntactically legal affine model can still describe a
//! shear the motion compensation filter cannot apply — in which case the block
//! falls back to translation. The check is pure arithmetic on the coded
//! parameters, so it belongs with the parse rather than with the decoder.

/// `DIV_LUT_PREC_BITS` (spec 3).
const DIV_LUT_PREC_BITS: u32 = 14;
/// `DIV_LUT_BITS` (spec 3).
const DIV_LUT_BITS: u32 = 8;
/// `WARP_PARAM_REDUCE_BITS` (spec 3).
const WARP_PARAM_REDUCE_BITS: u32 = 6;
/// `WARPEDMODEL_PREC_BITS` (spec 3).
const WARPEDMODEL_PREC_BITS: u32 = 16;

/// `Div_Lut` (spec 7.11.3.7), 257 entries of `DIV_LUT_PREC_BITS` precision.
static DIV_LUT: [i64; 257] = [
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768, 15709, 15650,
    15592, 15534, 15477, 15420, 15364, 15308, 15252, 15197, 15142, 15087, 15033, 14980, 14926,
    14873, 14821, 14769, 14717, 14665, 14614, 14564, 14513, 14463, 14413, 14364, 14315, 14266,
    14218, 14170, 14122, 14075, 14028, 13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662,
    13618, 13574, 13530, 13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107,
    13066, 13026, 12985, 12945, 12906, 12866, 12827, 12788, 12749, 12710, 12672, 12633, 12596,
    12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228, 12193, 12157, 12122,
    12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848, 11815, 11782, 11749, 11716, 11683,
    11651, 11619, 11586, 11555, 11523, 11491, 11460, 11429, 11398, 11367, 11336, 11305, 11275,
    11245, 11215, 11185, 11155, 11125, 11096, 11067, 11038, 11009, 10980, 10951, 10923, 10894,
    10866, 10838, 10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538,
    10512, 10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255, 10230, 10205,
    10180, 10156, 10131, 10107, 10082, 10058, 10034, 10010, 9986, 9963, 9939, 9916, 9892, 9869,
    9846, 9823, 9800, 9777, 9754, 9732, 9709, 9687, 9664, 9642, 9620, 9598, 9576, 9554, 9533, 9511,
    9489, 9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259, 9239, 9218, 9198, 9178,
    9158, 9138, 9118, 9098, 9079, 9059, 9039, 9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867,
    8849, 8830, 8812, 8793, 8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630, 8613, 8595, 8577,
    8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439, 8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306,
    8289, 8273, 8257, 8240, 8224, 8208, 8192,
];

/// `Round2(x, n)` (spec 4.7).
fn round2(x: i64, n: u32) -> i64 {
    if n == 0 {
        return x;
    }
    (x + (1 << (n - 1))) >> n
}

/// `Round2Signed(x, n)` (spec 4.7).
fn round2_signed(x: i64, n: u32) -> i64 {
    if x >= 0 { round2(x, n) } else { -round2(-x, n) }
}

/// `resolve_divisor(d)` (spec 7.11.3.7).
fn resolve_divisor(d: i64) -> (u32, i64) {
    let abs_d = d.unsigned_abs();
    if abs_d == 0 {
        // FloorLog2(0) is undefined; a zero divisor cannot produce a valid
        // shear, and the caller's validity check rejects the model anyway.
        return (DIV_LUT_PREC_BITS, 0);
    }
    let n = 63 - abs_d.leading_zeros();
    let e = abs_d as i64 - (1i64 << n);
    let f = if n > DIV_LUT_BITS {
        round2(e, n - DIV_LUT_BITS)
    } else {
        e << (DIV_LUT_BITS - n)
    };
    let factor = DIV_LUT[(f as usize).min(DIV_LUT.len() - 1)];
    (n + DIV_LUT_PREC_BITS, if d < 0 { -factor } else { factor })
}

/// `setup_shear(warpParams)` (spec 7.11.3.6): is this warp model usable?
///
/// True is `warpValid`; false is `VAWarpedMotionParamsAV1::invalid`.
pub(crate) fn warp_valid(params: &[i32; 6]) -> bool {
    let p: [i64; 6] = [
        params[0] as i64,
        params[1] as i64,
        params[2] as i64,
        params[3] as i64,
        params[4] as i64,
        params[5] as i64,
    ];
    let one = 1i64 << WARPEDMODEL_PREC_BITS;
    let alpha0 = (p[2] - one).clamp(-32768, 32767);
    let beta0 = p[3].clamp(-32768, 32767);
    let (div_shift, div_factor) = resolve_divisor(p[2]);
    let v = p[4] << WARPEDMODEL_PREC_BITS;
    let gamma0 = round2_signed(v * div_factor, div_shift).clamp(-32768, 32767);
    let w = p[3] * p[4];
    let delta0 = (p[5] - round2_signed(w * div_factor, div_shift) - one).clamp(-32768, 32767);

    let reduce = |x: i64| round2_signed(x, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;
    let (alpha, beta, gamma, delta) = (
        reduce(alpha0),
        reduce(beta0),
        reduce(gamma0),
        reduce(delta0),
    );

    4 * alpha.abs() + 7 * beta.abs() < one && 4 * gamma.abs() + 4 * delta.abs() < one
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_warp_is_valid() {
        let one = 1 << WARPEDMODEL_PREC_BITS;
        assert!(warp_valid(&[0, 0, one, 0, 0, one]));
    }

    #[test]
    fn an_extreme_shear_is_rejected() {
        let one = 1 << WARPEDMODEL_PREC_BITS;
        // beta of a quarter of unity alone breaks 7 * |beta| < 1 << 16.
        assert!(!warp_valid(&[0, 0, one, one / 4, 0, one]));
        // A degenerate zero scale cannot be sheared either.
        assert!(!warp_valid(&[0, 0, 0, 0, 0, one]));
    }

    #[test]
    fn div_lut_anchors_match_the_spec_table() {
        assert_eq!(DIV_LUT[0], 16384);
        assert_eq!(DIV_LUT[256], 8192);
        assert_eq!(DIV_LUT.len(), 257);
        // resolve_divisor(1 << 16) is an exact power of two: factor 16384.
        assert_eq!(resolve_divisor(1 << 16), (16 + 14, 16384));
        assert_eq!(resolve_divisor(-(1 << 16)), (30, -16384));
    }
}
