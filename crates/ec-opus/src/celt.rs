//! The CELT (MDCT) layer of Opus — RFC 6716 Section 4.3.
//!
//! CELT codes a frame as a per-band energy envelope plus a unit-norm shape per
//! band, the shape being a PVQ codeword. Decoding is therefore: coarse and fine
//! energy, an implicit bit allocation that both sides must derive identically,
//! PVQ shapes with spreading/folding, denormalisation by the energy, and an
//! inverse MDCT with overlap-add, post-filter and de-emphasis.
//!
//! Two notes on where the numbers come from. RFC 6716 Section 6 makes the
//! reference decoder in its Appendix A normative — "should the description
//! contradict the source code of the reference implementation, the latter shall
//! take precedence" — and Section 4.3 defers several tables to it by name
//! (`e_prob_model`, `cache_caps50`, `tf_select_table`, `LOG2_FRAC_TABLE`,
//! band_allocation). Those constants are reproduced here from that appendix,
//! because a decoder that invents them does not decode Opus. Everything with a
//! closed form (the window, `eband` boundaries, PVQ combinatorics) is computed
//! or written from the prose instead.
//!
//! The MDCT is *not* a DFT: sizes here are 15*2^k (120, 240, 480, 960
//! coefficients), so the transform runs as a 15-point stage over
//! [`ec_dsp::Fft`] power-of-two sub-transforms — an `N/4`-point complex FFT
//! exactly as the reference does it, never an O(N^2) direct evaluation.

use ec_core::{Error, Result};
use ec_dsp::Fft;

use crate::range::{RangeDecoder, RangeEncoder};

/// Bands in the 48 kHz mode.
pub const NB_BANDS: usize = 21;
/// Band boundaries in units of 2.5 ms MDCT bins (Table 55).
pub(crate) const E_BANDS: [usize; NB_BANDS + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];
/// MDCT bins in a 2.5 ms frame; the window overlap is the same length.
pub(crate) const SHORT_MDCT: usize = 120;
/// Samples of overlap between consecutive frames.
pub(crate) const OVERLAP: usize = 120;
/// Longest post-filter period, and the history the comb filter needs.
#[allow(dead_code)]
const MAX_PERIOD: usize = 1024;
/// Shortest post-filter period.
const MIN_PERIOD: usize = 15;
/// Synthesis history kept per channel.
const DECODE_BUFFER: usize = 2048;
/// Allocation resolution: 1/8 bit.
pub(crate) const BITRES: u32 = 3;
/// Most fine-energy bits a band can get.
pub(crate) const MAX_FINE_BITS: i32 = 8;
pub(crate) const FINE_OFFSET: i32 = 21;
pub(crate) const QTHETA_OFFSET: i32 = 4;
pub(crate) const QTHETA_OFFSET_TWOPHASE: i32 = 16;

/// `logN400`: `log2(N)` per band in 1/8 bits, for the fine-energy offset.
pub(crate) const LOG_N: [i32; NB_BANDS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36,
];

/// Mean energy per band, subtracted before quantisation (`eMeans`, in log2).
pub(crate) const E_MEANS: [f32; NB_BANDS] = [
    6.4375, 6.25, 5.75, 5.3125, 5.0625, 4.8125, 4.5, 4.375, 4.875, 4.6875, 4.5625, 4.4375, 4.875,
    4.625, 4.3125, 4.5, 4.375, 4.625, 4.75, 4.4375, 3.75,
];

/// Static allocation table (Table 57), `[quality][band]`, 1/32 bit per bin.
pub(crate) const BAND_ALLOCATION: [[u8; NB_BANDS]; 11] = [
    [0; NB_BANDS],
    [
        90, 80, 75, 69, 63, 56, 49, 40, 34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    [
        110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32, 26, 20, 12, 0, 0, 0, 0, 0, 0,
    ],
    [
        118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23, 15, 4, 0, 0, 0, 0,
    ],
    [
        126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12, 1, 0, 0,
    ],
    [
        134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10, 1,
    ],
    [
        144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15, 1,
    ],
    [
        152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20, 1,
    ],
    [
        162, 155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30, 1,
    ],
    [
        172, 165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45,
        20,
    ],
    [
        200, 200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153,
        148, 129, 104,
    ],
];

/// Laplace parameters for coarse energy, `[LM][intra][2*band]` (`e_prob_model`).
pub(crate) const E_PROB_MODEL: [[[u8; 42]; 2]; 4] = [
    [
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    [
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    [
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    [
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];

/// Inter-frame prediction gain for coarse energy, per LM (`pred_coef`).
pub(crate) const PRED_COEF: [f32; 4] = [
    29440.0 / 32768.0,
    26112.0 / 32768.0,
    21248.0 / 32768.0,
    16384.0 / 32768.0,
];
/// Intra-frame (across bands) prediction gain, per LM (`beta_coef`).
pub(crate) const BETA_COEF: [f32; 4] = [
    30147.0 / 32768.0,
    22282.0 / 32768.0,
    12124.0 / 32768.0,
    6554.0 / 32768.0,
];
/// The same for an intra frame.
pub(crate) const BETA_INTRA: f32 = 4915.0 / 32768.0;

/// TF resolution adjustment, `[LM][4*transient + 2*tf_select + tf_change]`
/// (Tables 60-63).
pub(crate) const TF_SELECT: [[i32; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

/// Conservative `log2` in 1/8 bits, used to reserve the intensity parameter.
pub(crate) const LOG2_FRAC: [i32; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];

/// Index into [`CACHE_BITS`] per `[LM+1][band]` (`cache_index50`).
const CACHE_INDEX: [i16; 105] = [
    -1, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 41, 41, 41, 82, 82, 123, 164, 200, 222, 0, 0, 0, 0,
    0, 0, 0, 0, 41, 41, 41, 41, 123, 123, 123, 164, 164, 240, 266, 283, 295, 41, 41, 41, 41, 41,
    41, 41, 41, 123, 123, 123, 123, 240, 240, 240, 266, 266, 305, 318, 328, 336, 123, 123, 123,
    123, 123, 123, 123, 123, 240, 240, 240, 240, 305, 305, 305, 318, 318, 343, 351, 358, 364, 240,
    240, 240, 240, 240, 240, 240, 240, 305, 305, 305, 305, 343, 343, 343, 351, 351, 370, 376, 382,
    387,
];

/// Bits (minus one) to code `k` pulses in each band size (`cache_bits50`).
pub(crate) const CACHE_BITS: [u8; 392] = [
    40, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 40, 15, 23, 28, 31, 34, 36, 38, 39, 41, 42, 43, 44, 45, 46, 47,
    47, 49, 50, 51, 52, 53, 54, 55, 55, 57, 58, 59, 60, 61, 62, 63, 63, 65, 66, 67, 68, 69, 70, 71,
    71, 40, 20, 33, 41, 48, 53, 57, 61, 64, 66, 69, 71, 73, 75, 76, 78, 80, 82, 85, 87, 89, 91, 92,
    94, 96, 98, 101, 103, 105, 107, 108, 110, 112, 114, 117, 119, 121, 123, 124, 126, 128, 40, 23,
    39, 51, 60, 67, 73, 79, 83, 87, 91, 94, 97, 100, 102, 105, 107, 111, 115, 118, 121, 124, 126,
    129, 131, 135, 139, 142, 145, 148, 150, 153, 155, 159, 163, 166, 169, 172, 174, 177, 179, 35,
    28, 49, 65, 78, 89, 99, 107, 114, 120, 126, 132, 136, 141, 145, 149, 153, 159, 165, 171, 176,
    180, 185, 189, 192, 199, 205, 211, 216, 220, 225, 229, 232, 239, 245, 251, 21, 33, 58, 79, 97,
    112, 125, 137, 148, 157, 166, 174, 182, 189, 195, 201, 207, 217, 227, 235, 243, 251, 17, 35,
    63, 86, 106, 123, 139, 152, 165, 177, 187, 197, 206, 214, 222, 230, 237, 250, 25, 31, 55, 75,
    91, 105, 117, 128, 138, 146, 154, 161, 168, 174, 180, 185, 190, 200, 208, 215, 222, 229, 235,
    240, 245, 255, 16, 36, 65, 89, 110, 128, 144, 159, 173, 185, 196, 207, 217, 226, 234, 242, 250,
    11, 41, 74, 103, 128, 151, 172, 191, 209, 225, 241, 255, 9, 43, 79, 110, 138, 163, 186, 207,
    227, 246, 12, 39, 71, 99, 123, 144, 164, 182, 198, 214, 228, 241, 253, 9, 44, 81, 113, 142,
    168, 192, 214, 235, 255, 7, 49, 90, 127, 160, 191, 220, 247, 6, 51, 95, 134, 170, 203, 234, 7,
    47, 87, 123, 155, 184, 212, 237, 6, 52, 97, 137, 174, 208, 240, 5, 57, 106, 151, 192, 231, 5,
    59, 111, 158, 202, 243, 5, 55, 103, 147, 187, 224, 5, 60, 113, 161, 206, 248, 4, 65, 122, 175,
    224, 4, 67, 127, 182, 234,
];

/// Per-band allocation ceiling, `[2*LM + stereo][band]` (`cache_caps50`).
pub(crate) const CACHE_CAPS: [u8; 168] = [
    224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178, 168, 134,
    61, 37, 224, 224, 224, 224, 224, 224, 224, 224, 240, 240, 240, 240, 207, 207, 207, 198, 198,
    183, 144, 66, 40, 160, 160, 160, 160, 160, 160, 160, 160, 185, 185, 185, 185, 193, 193, 193,
    183, 183, 172, 138, 64, 38, 240, 240, 240, 240, 240, 240, 240, 240, 207, 207, 207, 207, 204,
    204, 204, 193, 193, 180, 143, 66, 40, 185, 185, 185, 185, 185, 185, 185, 185, 193, 193, 193,
    193, 193, 193, 193, 183, 183, 172, 138, 65, 39, 207, 207, 207, 207, 207, 207, 207, 207, 204,
    204, 204, 204, 201, 201, 201, 188, 188, 176, 141, 66, 40, 193, 193, 193, 193, 193, 193, 193,
    193, 193, 193, 193, 193, 194, 194, 194, 184, 184, 173, 139, 65, 39, 204, 204, 204, 204, 204,
    204, 204, 204, 201, 201, 201, 201, 198, 198, 198, 187, 187, 175, 140, 66, 40,
];

/// Hadamard interleave order for 2, 4, 8 and 16 blocks (`ordery_table`).
const ORDERY: [usize; 30] = [
    1, 0, //
    3, 0, 2, 1, //
    7, 0, 4, 3, 6, 1, 5, 2, //
    15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

pub(crate) const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
pub(crate) const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
const TAPSET_ICDF: [u8; 3] = [2, 1, 0];
pub(crate) const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

/// Post-filter tap sets (Section 4.3.7.1).
const POSTFILTER_GAINS: [[f32; 3]; 3] = [
    [0.306_640_63, 0.217_041_02, 0.129_638_67],
    [0.463_867_2, 0.268_066_4, 0.0],
    [0.799_804_7, 0.100_097_66, 0.0],
];

/// De-emphasis coefficient, `1/(1 - 0.85 z^-1)` at 48 kHz.
pub(crate) const PREEMPH: f32 = 0.850_006_1;
/// Internal signal scale; the output is divided by this.
pub(crate) const SIG_SCALE: f32 = 32768.0;

pub(crate) const SPREAD_NONE: usize = 0;
pub(crate) const SPREAD_NORMAL: usize = 2;
const SPREAD_AGGRESSIVE: usize = 3;

/// `ilog(x)`: index of the highest set bit plus one.
#[inline]
pub(crate) fn ilog(x: u32) -> i32 {
    (32 - x.leading_zeros()) as i32
}

/// Integer square root, `floor(sqrt(x))`.
fn isqrt32(x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    let mut r = (x as f64).sqrt() as u32;
    // Correct the float estimate; never trust it at the boundary.
    while r > 0 && (r as u64) * (r as u64) > x as u64 {
        r -= 1;
    }
    while ((r + 1) as u64) * ((r + 1) as u64) <= x as u64 {
        r += 1;
    }
    r
}

/// The CELT low-overlap window (Section 4.3.7), computed rather than tabulated.
pub(crate) fn overlap_window() -> Vec<f32> {
    (0..OVERLAP)
        .map(|i| {
            let inner = (core::f64::consts::FRAC_PI_2 * (i as f64 + 0.5) / OVERLAP as f64).sin();
            (core::f64::consts::FRAC_PI_2 * inner * inner).sin() as f32
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The N/4 FFT and the inverse MDCT
// ---------------------------------------------------------------------------

/// A complex FFT of size `15 * 2^k`, the only sizes CELT needs.
///
/// [`ec_dsp::Fft`] covers the `2^k` factor; the factor of 15 is one extra
/// decimation stage, so the whole transform stays `O(n log n)` without a
/// mixed-radix kernel in the DSP crate.
#[derive(Clone, Debug)]
pub(crate) struct Fft15 {
    n: usize,
    /// Sub-transform of length `n/15`.
    sub: Fft<f32>,
    /// `exp(2 pi i * k1 * n2 / n)` twiddles, split.
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    scratch_re: Vec<f32>,
    scratch_im: Vec<f32>,
}

impl Fft15 {
    pub(crate) fn new(n: usize) -> Fft15 {
        assert!(n.is_multiple_of(15) && (n / 15).is_power_of_two());
        let l = n / 15;
        let mut tw_re = vec![0.0; n];
        let mut tw_im = vec![0.0; n];
        for k1 in 0..15 {
            for n2 in 0..l {
                let a = 2.0 * core::f64::consts::PI * (k1 * n2) as f64 / n as f64;
                tw_re[k1 * l + n2] = a.cos() as f32;
                tw_im[k1 * l + n2] = a.sin() as f32;
            }
        }
        Fft15 {
            n,
            sub: Fft::new(l),
            tw_re,
            tw_im,
            scratch_re: vec![0.0; n],
            scratch_im: vec![0.0; n],
        }
    }

    /// Unscaled inverse transform: `X[k] = sum x[n] exp(+2 pi i n k / N)`.
    pub(crate) fn inverse(&mut self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        let l = n / 15;
        debug_assert_eq!(re.len(), n);
        // Stage 1: 15-point DFTs over n1 at stride L, twiddled on the way out.
        for n2 in 0..l {
            let mut xr = [0.0f32; 15];
            let mut xi = [0.0f32; 15];
            for n1 in 0..15 {
                xr[n1] = re[l * n1 + n2];
                xi[n1] = im[l * n1 + n2];
            }
            let (yr, yi) = dft15(&xr, &xi);
            for k1 in 0..15 {
                let (tr, ti) = (self.tw_re[k1 * l + n2], self.tw_im[k1 * l + n2]);
                self.scratch_re[k1 * l + n2] = yr[k1] * tr - yi[k1] * ti;
                self.scratch_im[k1 * l + n2] = yr[k1] * ti + yi[k1] * tr;
            }
        }
        // Stage 2: L-point transforms, one per k1, written out at stride 15.
        for k1 in 0..15 {
            let rr = &mut self.scratch_re[k1 * l..(k1 + 1) * l];
            let ii = &mut self.scratch_im[k1 * l..(k1 + 1) * l];
            // ec-dsp scales its inverse by 1/L; undo that to keep the
            // transform unscaled, which is what the MDCT expects.
            self.sub.inverse_split(rr, ii);
            for (k2, (&r, &i)) in rr.iter().zip(ii.iter()).enumerate() {
                re[k1 + 15 * k2] = r * l as f32;
                im[k1 + 15 * k2] = i * l as f32;
            }
        }
    }
}

/// A 15-point DFT as 5 radix-3 stages then 3 radix-5 stages (Cooley-Tukey on
/// 15 = 3*5), which costs about a third of the direct evaluation.
#[inline]
fn dft15(xr: &[f32; 15], xi: &[f32; 15]) -> ([f32; 15], [f32; 15]) {
    // exp(+2 pi i / 3) and the two fifth-root cosines/sines.
    const C3: f32 = -0.5;
    const S3: f32 = 0.866_025_4;
    const C5_1: f32 = 0.309_017;
    const S5_1: f32 = 0.951_056_5;
    const C5_2: f32 = -0.809_017;
    const S5_2: f32 = 0.587_785_25;
    // exp(+2 pi i * (n2*k1) / 15) for n2 in 0..5, k1 in 0..3.
    const TW_RE: [[f32; 3]; 5] = [
        [1.0, 1.0, 1.0],
        [1.0, 0.913_545_5, 0.669_130_6],
        [1.0, 0.669_130_6, -0.104_528_46],
        [1.0, 0.309_017, -0.809_017],
        [1.0, -0.104_528_46, -0.978_147_6],
    ];
    const TW_IM: [[f32; 3]; 5] = [
        [0.0, 0.0, 0.0],
        [0.0, 0.406_736_65, 0.743_144_8],
        [0.0, 0.743_144_8, 0.994_521_9],
        [0.0, 0.951_056_5, 0.587_785_25],
        [0.0, 0.994_521_9, -0.207_911_69],
    ];
    // Radix-3 over n1 (stride 5), one per n2.
    let mut ar = [[0.0f32; 3]; 5];
    let mut ai = [[0.0f32; 3]; 5];
    for n2 in 0..5 {
        let (r0, i0) = (xr[n2], xi[n2]);
        let (r1, i1) = (xr[5 + n2], xi[5 + n2]);
        let (r2, i2) = (xr[10 + n2], xi[10 + n2]);
        let (sr, si) = (r1 + r2, i1 + i2);
        let (dr, di) = (r1 - r2, i1 - i2);
        ar[n2][0] = r0 + sr;
        ai[n2][0] = i0 + si;
        let (tr, ti) = (r0 + C3 * sr, i0 + C3 * si);
        // Multiplying (dr, di) by i gives (-di, dr).
        ar[n2][1] = tr - S3 * di;
        ai[n2][1] = ti + S3 * dr;
        ar[n2][2] = tr + S3 * di;
        ai[n2][2] = ti - S3 * dr;
        // Twiddle by exp(+2 pi i n2 k1 / 15).
        for k1 in 1..3 {
            let (tr, ti) = (TW_RE[n2][k1], TW_IM[n2][k1]);
            let (r, i) = (ar[n2][k1], ai[n2][k1]);
            ar[n2][k1] = r * tr - i * ti;
            ai[n2][k1] = r * ti + i * tr;
        }
    }
    // Radix-5 over n2, one per k1; output index is k1 + 3*k2.
    let mut yr = [0.0f32; 15];
    let mut yi = [0.0f32; 15];
    for k1 in 0..3 {
        let (r0, i0) = (ar[0][k1], ai[0][k1]);
        let (r1, i1) = (ar[1][k1], ai[1][k1]);
        let (r2, i2) = (ar[2][k1], ai[2][k1]);
        let (r3, i3) = (ar[3][k1], ai[3][k1]);
        let (r4, i4) = (ar[4][k1], ai[4][k1]);
        let (s1r, s1i) = (r1 + r4, i1 + i4);
        let (d1r, d1i) = (r1 - r4, i1 - i4);
        let (s2r, s2i) = (r2 + r3, i2 + i3);
        let (d2r, d2i) = (r2 - r3, i2 - i3);
        yr[k1] = r0 + s1r + s2r;
        yi[k1] = i0 + s1i + s2i;
        let (m1r, m1i) = (r0 + C5_1 * s1r + C5_2 * s2r, i0 + C5_1 * s1i + C5_2 * s2i);
        let (n1r, n1i) = (S5_1 * d1r + S5_2 * d2r, S5_1 * d1i + S5_2 * d2i);
        let (m2r, m2i) = (r0 + C5_2 * s1r + C5_1 * s2r, i0 + C5_2 * s1i + C5_1 * s2i);
        let (n2r, n2i) = (S5_2 * d1r - S5_1 * d2r, S5_2 * d1i - S5_1 * d2i);
        yr[k1 + 3] = m1r - n1i;
        yi[k1 + 3] = m1i + n1r;
        yr[k1 + 12] = m1r + n1i;
        yi[k1 + 12] = m1i - n1r;
        yr[k1 + 6] = m2r - n2i;
        yi[k1 + 6] = m2i + n2r;
        yr[k1 + 9] = m2r + n2i;
        yi[k1 + 9] = m2i - n2r;
    }
    (yr, yi)
}

/// One inverse-MDCT plan: spectrum length `l = 15 * 2^k`, window `2*l`.
#[derive(Clone, Debug)]
struct ImdctPlan {
    /// Spectrum length.
    l: usize,
    fft: Fft15,
    /// `cos(2 pi (i + 1/8) / (2 l))` and its sine, for the pre/post rotation.
    rot_re: Vec<f32>,
    rot_im: Vec<f32>,
    fre: Vec<f32>,
    fim: Vec<f32>,
    out: Vec<f32>,
}

impl ImdctPlan {
    fn new(l: usize) -> ImdctPlan {
        let n = 2 * l;
        let quarter = l / 2;
        let mut rot_re = vec![0.0; quarter];
        let mut rot_im = vec![0.0; quarter];
        for i in 0..quarter {
            let a = 2.0 * core::f64::consts::PI * (i as f64 + 0.125) / n as f64;
            rot_re[i] = a.cos() as f32;
            rot_im[i] = a.sin() as f32;
        }
        ImdctPlan {
            l,
            fft: Fft15::new(quarter),
            rot_re,
            rot_im,
            fre: vec![0.0; quarter],
            fim: vec![0.0; quarter],
            out: vec![0.0; 2 * l],
        }
    }

    /// Transforms `l` coefficients read at `stride` into `l + OVERLAP` windowed
    /// time samples, added into `out` starting at `out_off`.
    ///
    /// The window is the low-overlap one: zero outside a `l + OVERLAP` span,
    /// which is why the output is that long and not `2*l`.
    fn inverse(
        &mut self,
        spectrum: &[f32],
        stride: usize,
        window: &[f32],
        out: &mut [f32],
        out_off: usize,
    ) {
        let l = self.l;
        let quarter = l / 2;
        // Pre-rotation: z[i] = -(x[l-1-2i] + j x[2i]) * exp(j 2pi (i+1/8)/2l).
        for i in 0..quarter {
            let a = spectrum[(2 * i) * stride];
            let b = spectrum[(l - 1 - 2 * i) * stride];
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            self.fre[i] = -(b * c - a * s);
            self.fim[i] = -(b * s + a * c);
        }
        self.fft.inverse(&mut self.fre, &mut self.fim);
        // Post-rotation by the same angles.
        for i in 0..quarter {
            let (re, im) = (self.fre[i], self.fim[i]);
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            self.fre[i] = re * c - im * s;
            self.fim[i] = im * c + re * s;
        }
        // De-shuffle into the time-domain half-window, then mirror for TDAC.
        // t[] holds the l samples that the window's two halves fold onto.
        let t = &mut self.out[..l];
        for i in 0..quarter {
            t[2 * i] = -self.fre[i];
            t[2 * i + 1] = self.fim[quarter - 1 - i];
        }
        // out is indexed from `out_off - (l - OVERLAP)/2` in the reference;
        // shift it so the first written sample lands on out_off.
        // The window is zero outside a span of `l + OVERLAP`, so that is all
        // that is written: `flat` samples straight through at each end, and
        // `OVERLAP/2` windowed samples mirrored onto the frame's own tails.
        let base = out_off;
        let flat = quarter - OVERLAP / 2;
        let mirror = quarter + OVERLAP / 2; // index of t[quarter-1] in `out`
        for i in 0..flat {
            out[base + mirror - 1 - i] = t[quarter - 1 - i];
        }
        for i in flat..quarter {
            let x = t[quarter - 1 - i];
            let w = i - flat;
            out[base + w] -= window[w] * x;
            out[base + mirror - 1 - i] += window[OVERLAP - 1 - w] * x;
        }
        for i in 0..flat {
            out[base + mirror + i] = t[quarter + i];
        }
        for i in flat..quarter {
            let x = t[quarter + i];
            let w = i - flat;
            out[base + l + OVERLAP - 1 - w] = window[w] * x;
            out[base + mirror + i] = window[OVERLAP - 1 - w] * x;
        }
    }
}

// ---------------------------------------------------------------------------
// PVQ
// ---------------------------------------------------------------------------

/// Builds the `U(n, .)` row used to count PVQ codewords, returning `V(n, k)`.
///
/// `V(n,k) = U(n,k) + U(n,k+1)` with `U` obeying
/// `u[n][k] = u[n-1][k] + u[n][k-1] + u[n-1][k-1]` (Section 4.3.4.2).
fn pvq_urow(n: usize, k: usize, u: &mut [u32]) -> u32 {
    u[0] = 0;
    u[1] = 1;
    for (j, slot) in u.iter_mut().enumerate().take(k + 2).skip(2) {
        *slot = (2 * j - 1) as u32;
    }
    for _ in 2..n {
        unext(&mut u[1..k + 2], 1);
    }
    u[k].wrapping_add(u[k + 1])
}

/// Steps a `U` row up one dimension in place.
pub(crate) fn unext(u: &mut [u32], mut ui0: u32) {
    for j in 1..u.len() {
        let ui1 = u[j].wrapping_add(u[j - 1]).wrapping_add(ui0);
        u[j - 1] = ui0;
        ui0 = ui1;
    }
    let last = u.len() - 1;
    u[last] = ui0;
}

/// Steps a `U` row down one dimension in place.
fn uprev(u: &mut [u32], mut ui0: u32) {
    for j in 1..u.len() {
        let ui1 = u[j].wrapping_sub(u[j - 1]).wrapping_sub(ui0);
        u[j - 1] = ui0;
        ui0 = ui1;
    }
    let last = u.len() - 1;
    u[last] = ui0;
}

/// Decodes one PVQ codeword of `k` pulses in `n` dimensions.
pub(crate) fn decode_pulses(
    dec: &mut RangeDecoder,
    n: usize,
    k: usize,
    y: &mut [i32],
    u: &mut [u32],
) {
    let v = pvq_urow(n, k, u);
    let mut i = dec.dec_uint(v.max(2));
    let mut k = k;
    for slot in y.iter_mut().take(n) {
        let p = u[k + 1];
        let neg = i >= p;
        if neg {
            i -= p;
        }
        let k0 = k;
        let mut p = u[k];
        while p > i {
            k -= 1;
            p = u[k];
        }
        i -= p;
        let mag = (k0 - k) as i32;
        *slot = if neg { -mag } else { mag };
        uprev(&mut u[..k + 2], 0);
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// Where a band's folding source lives: in the running normalised spectrum, or
/// in the scratch copy taken before that spectrum is rewritten in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lowband {
    None,
    Norm(usize),
    Scratch(usize),
}

impl Lowband {
    fn offset(self, n: usize) -> Lowband {
        match self {
            Lowband::None => Lowband::None,
            Lowband::Norm(l) => Lowband::Norm(l + n),
            Lowband::Scratch(o) => Lowband::Scratch(o + n),
        }
    }
}

/// Everything one band decode needs; a struct because the recursion changes
/// only two or three fields at a time.
#[derive(Clone, Copy, Debug)]
struct BandArgs {
    /// Band index.
    i: usize,
    /// Offset of the band in the coefficient buffer.
    x: usize,
    /// Offset of the second channel, when this is a stereo band.
    y: Option<usize>,
    /// Coefficients in the band.
    n: usize,
    /// Bits allocated, in 1/8th bits.
    b: i32,
    spread: usize,
    /// Short blocks the band is split over.
    blocks: usize,
    intensity: usize,
    tf_change: i32,
    lowband: Lowband,
    /// Recursion depth of the split.
    level: i32,
    lm: i32,
    /// Where to write the normalised band for later folding.
    lowband_out: Option<usize>,
    gain: f32,
    /// Which blocks may be folded into, one bit each.
    fill: u32,
}

/// The folding PRNG (`celt_lcg_rand`).
#[inline]
fn celt_lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(1664525).wrapping_add(1013904223)
}

/// Start of a band's pulse-cache row. `lm` is `-1` for a twice-split band.
pub(crate) fn cache_index(band: usize, lm: i32) -> usize {
    let row = (lm + 1) as usize;
    let idx = CACHE_INDEX[row * NB_BANDS + band];
    debug_assert!(idx >= 0, "no pulse cache for band {band} at LM {lm}");
    idx.max(0) as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AllocationResult {
    pub(crate) coded_bands: usize,
    pub(crate) intensity: usize,
    pub(crate) dual_stereo: bool,
    pub(crate) balance: i32,
}

enum AllocationMode<'a, 'b> {
    Decode(&'a mut RangeDecoder<'b>),
    Encode {
        enc: &'a mut RangeEncoder,
        intensity: usize,
        dual_stereo: bool,
        prev_coded_bands: usize,
    },
}

impl AllocationMode<'_, '_> {
    fn keep_band(&mut self, j: usize, band_bits: i32, band_width: i32, lm: usize) -> bool {
        match self {
            AllocationMode::Decode(dec) => dec.dec_bit_logp(1),
            AllocationMode::Encode {
                enc,
                prev_coded_bands,
                ..
            } => {
                let hysteresis = if j < *prev_coded_bands { 7 } else { 9 };
                let stay = band_bits > ((hysteresis * band_width) << lm << BITRES) >> 4;
                enc.enc_bit_logp(stay, 1);
                stay
            }
        }
    }

    fn code_intensity(&mut self, start: usize, coded_bands: usize, intensity_rsv: i32) -> usize {
        match self {
            AllocationMode::Decode(dec) => {
                if intensity_rsv > 0 {
                    start + dec.dec_uint((coded_bands + 1 - start) as u32) as usize
                } else {
                    0
                }
            }
            AllocationMode::Encode { enc, intensity, .. } => {
                if intensity_rsv > 0 {
                    *intensity = (*intensity).min(coded_bands);
                    enc.enc_uint(
                        (*intensity - start) as u32,
                        (coded_bands + 1 - start) as u32,
                    );
                    *intensity
                } else {
                    *intensity = 0;
                    0
                }
            }
        }
    }

    fn code_dual_stereo(&mut self, dual_stereo_rsv: i32) -> bool {
        match self {
            AllocationMode::Decode(dec) => {
                if dual_stereo_rsv > 0 {
                    dec.dec_bit_logp(1)
                } else {
                    false
                }
            }
            AllocationMode::Encode {
                enc, dual_stereo, ..
            } => {
                if dual_stereo_rsv > 0 {
                    enc.enc_bit_logp(*dual_stereo, 1);
                    *dual_stereo
                } else {
                    *dual_stereo = false;
                    false
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_allocation_decode(
    dec: &mut RangeDecoder,
    start: usize,
    end: usize,
    alloc_trim: i32,
    total: i32,
    c: usize,
    lm: usize,
    offsets: &[i32; NB_BANDS],
    caps: &[i32; NB_BANDS],
    pulses: &mut [i32; NB_BANDS],
    fine_quant: &mut [i32; NB_BANDS],
    fine_priority: &mut [i32; NB_BANDS],
) -> AllocationResult {
    compute_allocation_shared(
        AllocationMode::Decode(dec),
        start,
        end,
        alloc_trim,
        total,
        c,
        lm,
        offsets,
        caps,
        pulses,
        fine_quant,
        fine_priority,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_allocation_encode(
    enc: &mut RangeEncoder,
    start: usize,
    end: usize,
    alloc_trim: i32,
    total: i32,
    c: usize,
    lm: usize,
    offsets: &[i32; NB_BANDS],
    caps: &[i32; NB_BANDS],
    pulses: &mut [i32; NB_BANDS],
    fine_quant: &mut [i32; NB_BANDS],
    fine_priority: &mut [i32; NB_BANDS],
    intensity: usize,
    dual_stereo: bool,
    prev_coded_bands: usize,
) -> AllocationResult {
    compute_allocation_shared(
        AllocationMode::Encode {
            enc,
            intensity,
            dual_stereo,
            prev_coded_bands,
        },
        start,
        end,
        alloc_trim,
        total,
        c,
        lm,
        offsets,
        caps,
        pulses,
        fine_quant,
        fine_priority,
    )
}

#[allow(clippy::too_many_arguments)]
fn compute_allocation_shared(
    mut mode: AllocationMode<'_, '_>,
    start: usize,
    end: usize,
    alloc_trim: i32,
    total: i32,
    c: usize,
    lm: usize,
    offsets: &[i32; NB_BANDS],
    caps: &[i32; NB_BANDS],
    pulses: &mut [i32; NB_BANDS],
    fine_quant: &mut [i32; NB_BANDS],
    fine_priority: &mut [i32; NB_BANDS],
) -> AllocationResult {
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if c == 2 {
        intensity_rsv = LOG2_FRAC[end - start];
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }

    let mut thresh = [0i32; NB_BANDS];
    let mut trim_offset = [0i32; NB_BANDS];
    for j in start..end {
        let width = (E_BANDS[j + 1] - E_BANDS[j]) as i32;
        thresh[j] = ((c as i32) << BITRES).max((((3 * width) << lm) << BITRES) >> 4);
        trim_offset[j] = (c as i32
            * width
            * (alloc_trim - 5 - lm as i32)
            * (end - j - 1) as i32
            * (1 << (lm as u32 + BITRES)))
            >> 6;
        if (width << lm) == 1 {
            trim_offset[j] -= (c as i32) << BITRES;
        }
    }

    let mut lo = 1i32;
    let mut hi = BAND_ALLOCATION.len() as i32 - 1;
    while lo <= hi {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let width = (E_BANDS[j + 1] - E_BANDS[j]) as i32;
            let mut bitsj = (c as i32 * width * BAND_ALLOCATION[mid as usize][j] as i32) << lm >> 2;
            if bitsj > 0 {
                bitsj = 0.max(bitsj + trim_offset[j]);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(caps[j]);
            } else if bitsj >= (c as i32) << BITRES {
                psum += (c as i32) << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    let hi = lo;
    let lo = lo - 1;

    let mut bits1 = [0i32; NB_BANDS];
    let mut bits2 = [0i32; NB_BANDS];
    for j in start..end {
        let width = (E_BANDS[j + 1] - E_BANDS[j]) as i32;
        let mut b1 = (c as i32 * width * BAND_ALLOCATION[lo as usize][j] as i32) << lm >> 2;
        let mut b2 = if hi as usize >= BAND_ALLOCATION.len() {
            caps[j]
        } else {
            (c as i32 * width * BAND_ALLOCATION[hi as usize][j] as i32) << lm >> 2
        };
        if b1 > 0 {
            b1 = 0.max(b1 + trim_offset[j]);
        }
        if b2 > 0 {
            b2 = 0.max(b2 + trim_offset[j]);
        }
        if lo > 0 {
            b1 += offsets[j];
        }
        b2 += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits1[j] = b1;
        bits2[j] = 0.max(b2 - b1);
    }

    const ALLOC_STEPS: u32 = 6;
    let alloc_floor = (c as i32) << BITRES;
    let stereo = c > 1;
    let log_m = (lm as i32) << BITRES;

    let mut lo = 0i32;
    let mut hi = 1i32 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(caps[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }

    let mut psum = 0;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + ((lo * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(caps[j]);
        pulses[j] = tmp;
        psum += tmp;
    }

    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let percoeff = left / (E_BANDS[coded_bands] - E_BANDS[start]) as i32;
        left -= (E_BANDS[coded_bands] - E_BANDS[start]) as i32 * percoeff;
        let rem = 0.max(left - (E_BANDS[j] - E_BANDS[start]) as i32);
        let band_width = (E_BANDS[coded_bands] - E_BANDS[j]) as i32;
        let mut band_bits = pulses[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if mode.keep_band(j, band_bits, band_width, lm) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= pulses[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = LOG2_FRAC[j - start];
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            pulses[j] = alloc_floor;
        } else {
            pulses[j] = 0;
        }
        coded_bands -= 1;
    }

    let intensity = mode.code_intensity(start, coded_bands, intensity_rsv);
    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = mode.code_dual_stereo(dual_stereo_rsv);

    let mut left = total - psum;
    let percoeff = left / (E_BANDS[coded_bands] - E_BANDS[start]) as i32;
    left -= (E_BANDS[coded_bands] - E_BANDS[start]) as i32 * percoeff;
    for j in start..coded_bands {
        pulses[j] += percoeff * (E_BANDS[j + 1] - E_BANDS[j]) as i32;
    }
    for j in start..coded_bands {
        let tmp = left.min((E_BANDS[j + 1] - E_BANDS[j]) as i32);
        pulses[j] += tmp;
        left -= tmp;
    }

    let mut balance = 0i32;
    for j in start..coded_bands {
        let n0 = (E_BANDS[j + 1] - E_BANDS[j]) as i32;
        let n = n0 << lm;
        pulses[j] += balance;
        let mut excess;
        if n > 1 {
            excess = 0.max(pulses[j] - caps[j]);
            pulses[j] -= excess;
            let den = c as i32 + 0;
            let den = den * n + i32::from(c == 2 && n > 2 && !dual_stereo && j < intensity);
            let nc_log_n = den * (LOG_N[j] + log_m);
            let mut offset = (nc_log_n >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if pulses[j] + offset < (den * 2) << BITRES {
                offset += nc_log_n >> 2;
            } else if pulses[j] + offset < (den * 3) << BITRES {
                offset += nc_log_n >> 3;
            }
            let mut eb = 0.max((pulses[j] + offset + (den << (BITRES - 1))) / (den << BITRES));
            if c as i32 * eb > (pulses[j] >> BITRES) {
                eb = pulses[j] >> u32::from(stereo) >> BITRES;
            }
            eb = eb.min(MAX_FINE_BITS);
            fine_quant[j] = eb;
            fine_priority[j] = i32::from(eb * (den << BITRES) >= pulses[j] + offset);
            pulses[j] -= (c as i32 * eb) << BITRES;
        } else {
            excess = 0.max(pulses[j] - ((c as i32) << BITRES));
            pulses[j] -= excess;
            fine_quant[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine =
                (excess >> (u32::from(stereo) + BITRES)).min(MAX_FINE_BITS - fine_quant[j]);
            fine_quant[j] += extra_fine;
            let extra_bits = (extra_fine * c as i32) << BITRES;
            fine_priority[j] = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        balance = excess;
    }
    for j in coded_bands..end {
        fine_quant[j] = pulses[j] >> u32::from(stereo) >> BITRES;
        pulses[j] = 0;
        fine_priority[j] = i32::from(fine_quant[j] < 1);
    }

    AllocationResult {
        coded_bands,
        intensity,
        dual_stereo,
        balance,
    }
}

/// A CELT layer decoder, one per Opus stream.
#[derive(Clone, Debug)]
pub struct CeltDecoder {
    /// Channels this decoder outputs.
    channels: usize,
    /// Output decimation: 1 at 48 kHz, 2 at 24 kHz, and so on.
    downsample: usize,
    window: Vec<f32>,
    plans: Vec<ImdctPlan>,
    /// Synthesis history, `channels * DECODE_BUFFER`.
    decode_mem: Vec<f32>,
    /// MDCT overlap tail, `channels * OVERLAP`.
    overlap_mem: Vec<f32>,
    preemph_mem: [f32; 2],
    old_band_e: Vec<f32>,
    old_log_e: Vec<f32>,
    old_log_e2: Vec<f32>,
    background_log_e: Vec<f32>,
    postfilter_period: usize,
    postfilter_period_old: usize,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    /// The folding PRNG state, carried between frames.
    rng: u32,
    // Per-frame scratch, allocated once.
    x: Vec<f32>,
    freq: Vec<f32>,
    norm: Vec<f32>,
    lowband_scratch: Vec<f32>,
    hadamard_tmp: Vec<f32>,
    band_e: Vec<f32>,
    pulses: [i32; NB_BANDS],
    fine_quant: [i32; NB_BANDS],
    fine_priority: [i32; NB_BANDS],
    offsets: [i32; NB_BANDS],
    caps: [i32; NB_BANDS],
    tf_res: [i32; NB_BANDS],
    collapse_masks: [u8; 2 * NB_BANDS],
    iy: Vec<i32>,
    urow: Vec<u32>,
    mdct_buf: Vec<f32>,
}

impl CeltDecoder {
    /// A decoder producing `channels` channels at `48000/downsample` Hz.
    pub fn new(channels: usize, downsample: usize) -> CeltDecoder {
        assert!((1..=2).contains(&channels));
        let max_n = SHORT_MDCT * 8;
        CeltDecoder {
            channels,
            downsample,
            window: overlap_window(),
            plans: (0..4).map(|lm| ImdctPlan::new(SHORT_MDCT << lm)).collect(),
            decode_mem: vec![0.0; channels * DECODE_BUFFER],
            overlap_mem: vec![0.0; channels * OVERLAP],
            preemph_mem: [0.0; 2],
            old_band_e: vec![0.0; 2 * NB_BANDS],
            old_log_e: vec![-28.0; 2 * NB_BANDS],
            old_log_e2: vec![-28.0; 2 * NB_BANDS],
            background_log_e: vec![-28.0; 2 * NB_BANDS],
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            rng: 0,
            x: vec![0.0; 2 * max_n],
            freq: vec![0.0; 2 * max_n],
            norm: vec![0.0; 2 * max_n],
            lowband_scratch: vec![0.0; max_n],
            hadamard_tmp: vec![0.0; max_n],
            band_e: vec![0.0; 2 * NB_BANDS],
            pulses: [0; NB_BANDS],
            fine_quant: [0; NB_BANDS],
            fine_priority: [0; NB_BANDS],
            offsets: [0; NB_BANDS],
            caps: [0; NB_BANDS],
            tf_res: [0; NB_BANDS],
            collapse_masks: [0; 2 * NB_BANDS],
            iy: vec![0; max_n],
            urow: vec![0; 256],
            mdct_buf: vec![0.0; max_n + OVERLAP],
        }
    }

    /// Drops all inter-frame state (after a seek, or on a mode switch).
    pub fn reset(&mut self) {
        self.decode_mem.fill(0.0);
        self.overlap_mem.fill(0.0);
        self.preemph_mem = [0.0; 2];
        self.old_band_e.fill(0.0);
        self.old_log_e.fill(-28.0);
        self.old_log_e2.fill(-28.0);
        self.background_log_e.fill(-28.0);
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.rng = 0;
    }

    /// The range decoder state after the last frame — the test vectors' oracle.
    pub fn rng(&self) -> u32 {
        self.rng
    }

    /// Decodes one CELT frame.
    ///
    /// `frame_size` is in 48 kHz samples; `start`/`end` bound the coded bands
    /// (17..21 for the CELT half of a hybrid frame); `stream_channels` is what
    /// the bitstream codes, which may be 1 while the decoder outputs 2.
    /// `out` receives `channels * frame_size / downsample` interleaved samples.
    #[allow(clippy::too_many_arguments)]
    pub fn decode(
        &mut self,
        dec: &mut RangeDecoder,
        out: &mut [f32],
        frame_size: usize,
        start: usize,
        end: usize,
        stream_channels: usize,
    ) -> Result<()> {
        let lm = match frame_size {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::corrupt(format!(
                    "celt: frame size {frame_size} is not 120, 240, 480 or 960"
                )));
            }
        };
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let cc = self.channels;
        let c = stream_channels;
        let eff_end = end.min(NB_BANDS);
        let len = dec.len();
        let total_bits = (len * 8) as i32;

        // A mono stream fed to a stereo decoder inherits the louder history.
        if c == 1 {
            for i in 0..NB_BANDS {
                self.old_band_e[i] = self.old_band_e[i].max(self.old_band_e[NB_BANDS + i]);
            }
        }

        for ch in 0..c {
            self.x[ch * n..ch * n + m * E_BANDS[start]].fill(0.0);
            self.x[ch * n + m * E_BANDS[eff_end]..(ch + 1) * n].fill(0.0);
        }

        let mut tell = dec.tell() as i32;
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            dec.dec_bit_logp(15)
        } else {
            false
        };
        if silence {
            // Pretend the frame was fully consumed (Section 4.3).
            dec.skip_to_end();
            tell = total_bits;
        }

        let mut postfilter_gain = 0.0f32;
        let mut postfilter_pitch = 0usize;
        let mut postfilter_tapset = 0usize;
        if start == 0 && tell + 16 <= total_bits {
            if dec.dec_bit_logp(1) {
                let octave = dec.dec_uint(6) as usize;
                postfilter_pitch = (16 << octave) + dec.dec_bits(4 + octave as u32) as usize - 1;
                let qg = dec.dec_bits(3);
                if dec.tell() as i32 + 2 <= total_bits {
                    postfilter_tapset = dec.dec_icdf(&TAPSET_ICDF, 2);
                }
                postfilter_gain = 0.09375 * (qg + 1) as f32;
            }
            tell = dec.tell() as i32;
        }

        let transient = if lm > 0 && tell + 3 <= total_bits {
            let t = dec.dec_bit_logp(3);
            tell = dec.tell() as i32;
            t
        } else {
            false
        };
        let short_blocks = if transient { m } else { 0 };

        let intra = if tell + 3 <= total_bits {
            dec.dec_bit_logp(3)
        } else {
            false
        };
        self.unquant_coarse_energy(dec, start, end, intra, c, lm, len);
        self.tf_decode(dec, start, end, transient, lm, len);

        tell = dec.tell() as i32;
        let spread = if tell + 4 <= total_bits {
            dec.dec_icdf(&SPREAD_ICDF, 5)
        } else {
            SPREAD_NORMAL
        };

        // Per-band allocation ceiling.
        for i in 0..NB_BANDS {
            let bw = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            self.caps[i] =
                ((CACHE_CAPS[NB_BANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * bw as i32)
                    >> 2;
        }

        // Band boosts (dynalloc).
        let mut dynalloc_logp = 6u32;
        let mut total_bits_frac = total_bits << BITRES;
        let mut tell_frac = dec.tell_frac() as i32;
        self.offsets.fill(0);
        for i in start..end {
            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tell_frac + ((loop_logp as i32) << BITRES) < total_bits_frac
                && boost < self.caps[i]
            {
                let flag = dec.dec_bit_logp(loop_logp);
                tell_frac = dec.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits_frac -= quanta;
                loop_logp = 1;
            }
            self.offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = dynalloc_logp.saturating_sub(1).max(2);
            }
        }

        let alloc_trim = if tell_frac + (6 << BITRES) <= total_bits_frac {
            dec.dec_icdf(&TRIM_ICDF, 7) as i32
        } else {
            5
        };

        let mut bits = ((len as i32 * 8) << BITRES) - dec.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;

        let alloc = compute_allocation_decode(
            dec,
            start,
            end,
            alloc_trim,
            bits,
            c,
            lm,
            &self.offsets,
            &self.caps,
            &mut self.pulses,
            &mut self.fine_quant,
            &mut self.fine_priority,
        );
        let intensity = alloc.intensity;
        let dual_stereo = alloc.dual_stereo;
        let balance = alloc.balance;
        let coded_bands = alloc.coded_bands;

        self.unquant_fine_energy(dec, start, end, c);

        self.quant_all_bands(
            dec,
            start,
            end,
            short_blocks,
            spread,
            dual_stereo,
            intensity,
            (len as i32 * (8 << BITRES)) - anti_collapse_rsv,
            balance,
            lm,
            coded_bands,
            c,
            n,
        );

        let anti_collapse_on = if anti_collapse_rsv > 0 {
            dec.dec_bits(1) != 0
        } else {
            false
        };

        let bits_left = len as i32 * 8 - dec.tell() as i32;
        self.unquant_energy_finalise(dec, start, end, bits_left, c);

        if anti_collapse_on {
            self.anti_collapse(lm, c, n, start, end);
        }

        // log2 energy back to linear amplitude.
        for ch in 0..c {
            for (i, mean) in E_MEANS.iter().enumerate() {
                self.band_e[ch * NB_BANDS + i] = if i < start || i >= end {
                    0.0
                } else {
                    ((self.old_band_e[ch * NB_BANDS + i] + mean) as f64).exp2() as f32
                };
            }
        }
        if silence {
            self.band_e.fill(0.0);
            self.old_band_e.fill(-28.0);
        }

        self.denormalise_bands(eff_end, c, m, n);

        // Slide the synthesis history and run the inverse MDCT into it.
        for ch in 0..cc {
            let base = ch * DECODE_BUFFER;
            self.decode_mem
                .copy_within(base + n..base + DECODE_BUFFER, base);
        }
        for ch in 0..c {
            self.freq[ch * n..ch * n + m * E_BANDS[start]].fill(0.0);
            let bound = (m * E_BANDS[eff_end]).min(n / self.downsample);
            self.freq[ch * n + bound..(ch + 1) * n].fill(0.0);
        }
        if cc == 2 && c == 1 {
            let (a, b) = self.freq.split_at_mut(n);
            b[..n].copy_from_slice(&a[..n]);
        }
        if cc == 1 && c == 2 {
            for i in 0..n {
                self.freq[i] = 0.5 * (self.freq[i] + self.freq[n + i]);
            }
        }
        self.inverse_mdcts(short_blocks, lm, n, cc);

        // Post-filter, then de-emphasis into the caller's buffer.
        self.postfilter_period = self.postfilter_period.max(MIN_PERIOD);
        self.postfilter_period_old = self.postfilter_period_old.max(MIN_PERIOD);
        for ch in 0..cc {
            let base = ch * DECODE_BUFFER + DECODE_BUFFER - n;
            self.comb_filter(
                base,
                SHORT_MDCT.min(n),
                self.postfilter_period_old,
                self.postfilter_period,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                true,
            );
            if lm != 0 {
                self.comb_filter(
                    base + SHORT_MDCT,
                    n - SHORT_MDCT,
                    self.postfilter_period,
                    postfilter_pitch.max(MIN_PERIOD),
                    self.postfilter_gain,
                    postfilter_gain,
                    self.postfilter_tapset,
                    postfilter_tapset,
                    true,
                );
            }
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        // Energy history for the next frame's prediction and anti-collapse.
        if c == 1 {
            for i in 0..NB_BANDS {
                self.old_band_e[NB_BANDS + i] = self.old_band_e[i];
            }
        }
        if !transient {
            self.old_log_e2.copy_from_slice(&self.old_log_e);
            self.old_log_e.copy_from_slice(&self.old_band_e);
            for i in 0..2 * NB_BANDS {
                self.background_log_e[i] =
                    (self.background_log_e[i] + m as f32 * 0.001).min(self.old_band_e[i]);
            }
        } else {
            for i in 0..2 * NB_BANDS {
                self.old_log_e[i] = self.old_log_e[i].min(self.old_band_e[i]);
            }
        }
        for ch in 0..2 {
            for i in (0..start).chain(end..NB_BANDS) {
                self.old_band_e[ch * NB_BANDS + i] = 0.0;
                self.old_log_e[ch * NB_BANDS + i] = -28.0;
                self.old_log_e2[ch * NB_BANDS + i] = -28.0;
            }
        }
        self.rng = dec.range();

        self.deemphasis(out, n, cc);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn unquant_coarse_energy(
        &mut self,
        dec: &mut RangeDecoder,
        start: usize,
        end: usize,
        intra: bool,
        c: usize,
        lm: usize,
        len: usize,
    ) {
        let model = &E_PROB_MODEL[lm][usize::from(intra)];
        let (coef, beta) = if intra {
            (0.0, BETA_INTRA)
        } else {
            (PRED_COEF[lm], BETA_COEF[lm])
        };
        let budget = (len * 8) as i32;
        let mut prev = [0.0f32; 2];
        for i in start..end {
            for (ch, prev) in prev.iter_mut().enumerate().take(c) {
                let tell = dec.tell() as i32;
                let qi = if budget - tell >= 15 {
                    let pi = 2 * i.min(20);
                    laplace_decode(dec, (model[pi] as u32) << 7, (model[pi + 1] as i32) << 6)
                } else if budget - tell >= 2 {
                    let qi = dec.dec_icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                    (qi >> 1) ^ -(qi & 1)
                } else if budget - tell >= 1 {
                    -i32::from(dec.dec_bit_logp(1))
                } else {
                    -1
                };
                let q = qi as f32;
                let old = &mut self.old_band_e[i + ch * NB_BANDS];
                *old = old.max(-9.0);
                *old = coef * *old + *prev + q;
                *prev += q - beta * q;
            }
        }
    }

    fn unquant_fine_energy(&mut self, dec: &mut RangeDecoder, start: usize, end: usize, c: usize) {
        for i in start..end {
            if self.fine_quant[i] <= 0 {
                continue;
            }
            for ch in 0..c {
                let q2 = dec.dec_bits(self.fine_quant[i] as u32);
                let offset = (q2 as f32 + 0.5) / (1 << self.fine_quant[i]) as f32 - 0.5;
                self.old_band_e[i + ch * NB_BANDS] += offset;
            }
        }
    }

    fn unquant_energy_finalise(
        &mut self,
        dec: &mut RangeDecoder,
        start: usize,
        end: usize,
        mut bits_left: i32,
        c: usize,
    ) {
        for prio in 0..2 {
            for i in start..end {
                if bits_left < c as i32 {
                    break;
                }
                if self.fine_quant[i] >= MAX_FINE_BITS || self.fine_priority[i] != prio {
                    continue;
                }
                for ch in 0..c {
                    let q2 = dec.dec_bits(1);
                    let offset = (q2 as f32 - 0.5) / (1 << (self.fine_quant[i] + 1)) as f32;
                    self.old_band_e[i + ch * NB_BANDS] += offset;
                    bits_left -= 1;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tf_decode(
        &mut self,
        dec: &mut RangeDecoder,
        start: usize,
        end: usize,
        transient: bool,
        lm: usize,
        len: usize,
    ) {
        let mut budget = (len * 8) as i32;
        let mut tell = dec.tell() as i32;
        let mut logp: u32 = if transient { 2 } else { 4 };
        let tf_select_rsv = lm > 0 && tell + (logp as i32) < budget;
        budget -= i32::from(tf_select_rsv);
        let mut curr = 0i32;
        let mut changed = false;
        for i in start..end {
            if tell + logp as i32 <= budget {
                curr ^= i32::from(dec.dec_bit_logp(logp));
                tell = dec.tell() as i32;
                changed |= curr != 0;
            }
            self.tf_res[i] = curr;
            logp = if transient { 4 } else { 5 };
        }
        let t = usize::from(transient);
        let ch = usize::from(changed);
        let mut tf_select = 0usize;
        if tf_select_rsv && TF_SELECT[lm][4 * t + ch] != TF_SELECT[lm][4 * t + 2 + ch] {
            tf_select = usize::from(dec.dec_bit_logp(1));
        }
        for i in start..end {
            self.tf_res[i] = TF_SELECT[lm][4 * t + 2 * tf_select + self.tf_res[i] as usize];
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quant_all_bands(
        &mut self,
        dec: &mut RangeDecoder,
        start: usize,
        end: usize,
        short_blocks: usize,
        spread: usize,
        mut dual_stereo: bool,
        intensity: usize,
        total_bits: i32,
        mut balance: i32,
        lm: usize,
        coded_bands: usize,
        c: usize,
        n: usize,
    ) {
        let m = 1usize << lm;
        let blocks = if short_blocks != 0 { m } else { 1 };
        let norm_len = m * E_BANDS[NB_BANDS];
        let mut lowband_offset = 0usize;
        let mut update_lowband = true;
        self.collapse_masks.fill(0);

        for i in start..end {
            let n_band = m * E_BANDS[i + 1] - m * E_BANDS[i];
            let tell = dec.tell_frac() as i32;
            if i != start {
                balance -= tell;
            }
            let mut remaining_bits = total_bits - tell - 1;
            let b = if i < coded_bands {
                let curr_balance = balance / 3.min(coded_bands - i) as i32;
                0.max(16383.min((remaining_bits + 1).min(self.pulses[i] + curr_balance)))
            } else {
                0
            };

            if m * E_BANDS[i] >= m * E_BANDS[start] + n_band
                && (update_lowband || lowband_offset == 0)
            {
                lowband_offset = i;
            }

            let tf_change = self.tf_res[i];
            // Which already-decoded bands this one may fold from, and whether
            // they had any pulses at all (the collapse mask).
            let (mut x_cm, mut y_cm);
            let mut effective_lowband: Option<usize> = None;
            if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || blocks > 1 || tf_change < 0) {
                let low =
                    (m * E_BANDS[start]).max((m * E_BANDS[lowband_offset]).saturating_sub(n_band));
                effective_lowband = Some(low);
                let mut fold_start = lowband_offset;
                loop {
                    fold_start -= 1;
                    if m * E_BANDS[fold_start] <= low {
                        break;
                    }
                }
                let mut fold_end = lowband_offset - 1;
                loop {
                    fold_end += 1;
                    if m * E_BANDS[fold_end] >= low + n_band {
                        break;
                    }
                }
                x_cm = 0;
                y_cm = 0;
                let mut fi = fold_start;
                loop {
                    x_cm |= self.collapse_masks[fi * c] as u32;
                    y_cm |= self.collapse_masks[fi * c + c - 1] as u32;
                    fi += 1;
                    if fi >= fold_end {
                        break;
                    }
                }
            } else {
                x_cm = (1u32 << blocks) - 1;
                y_cm = x_cm;
            }

            if dual_stereo && i == intensity {
                // Switch off dual stereo to do intensity from here up.
                dual_stereo = false;
                for j in m * E_BANDS[start]..m * E_BANDS[i] {
                    self.norm[j] = 0.5 * (self.norm[j] + self.norm[norm_len + j]);
                }
            }

            let x_off = m * E_BANDS[i];
            let base = BandArgs {
                i,
                x: x_off,
                y: None,
                n: n_band,
                b,
                spread,
                blocks,
                intensity,
                tf_change,
                lowband: Lowband::None,
                level: 0,
                lm: lm as i32,
                lowband_out: Some(x_off),
                gain: 1.0,
                fill: 0,
            };
            if dual_stereo {
                x_cm = self.quant_band(
                    dec,
                    BandArgs {
                        b: b / 2,
                        lowband: effective_lowband.map_or(Lowband::None, Lowband::Norm),
                        fill: x_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
                y_cm = self.quant_band(
                    dec,
                    BandArgs {
                        x: n + x_off,
                        b: b / 2,
                        lowband: effective_lowband
                            .map_or(Lowband::None, |l| Lowband::Norm(norm_len + l)),
                        lowband_out: Some(norm_len + x_off),
                        fill: y_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
            } else {
                x_cm = self.quant_band(
                    dec,
                    BandArgs {
                        y: if c == 2 { Some(n + x_off) } else { None },
                        lowband: effective_lowband.map_or(Lowband::None, Lowband::Norm),
                        fill: x_cm | y_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
                y_cm = x_cm;
            }
            self.collapse_masks[i * c] = x_cm as u8;
            self.collapse_masks[i * c + c - 1] = y_cm as u8;
            balance += self.pulses[i] + tell;
            // Keep folding from this band only while it has a bit per sample.
            update_lowband = b > (n_band as i32) << BITRES;
        }
    }

    /// Decodes one band (Section 4.3.4), recursing for splits and for stereo.
    fn quant_band(&mut self, dec: &mut RangeDecoder, a: BandArgs, remaining_bits: &mut i32) -> u32 {
        let BandArgs {
            i,
            x,
            mut y,
            mut n,
            mut b,
            spread,
            mut blocks,
            intensity,
            mut tf_change,
            mut lowband,
            level,
            mut lm,
            lowband_out,
            gain,
            mut fill,
        } = a;
        let n0 = n;
        let mut n_b = n;
        let stereo = y.is_some();
        let mut split = stereo;
        let mut inv = false;
        let mut cm: u32 = 0;
        let mut time_divide = 0;
        let mut recombine = 0usize;
        let long_blocks = blocks == 1;

        n_b /= blocks;
        let mut n_b0 = n_b;
        let mut b0 = blocks;

        if n == 1 {
            // A single coefficient carries nothing but its sign.
            for offs in [Some(x), y].into_iter().flatten() {
                let mut sign = false;
                if *remaining_bits >= 1 << BITRES {
                    sign = dec.dec_bits(1) != 0;
                    *remaining_bits -= 1 << BITRES;
                }
                self.x[offs] = if sign { -1.0 } else { 1.0 };
            }
            if let Some(o) = lowband_out {
                self.norm[o] = self.x[x];
            }
            return 1;
        }

        if !stereo && level == 0 {
            if tf_change > 0 {
                recombine = tf_change as usize;
            }
            // The folding source is about to be rewritten in place, so take a
            // copy first — the band it belongs to is still needed as decoded.
            if lowband != Lowband::None
                && (recombine > 0 || (n_b & 1) == 0 && tf_change < 0 || b0 > 1)
            {
                for j in 0..n {
                    self.lowband_scratch[j] = self.lowband(lowband, j);
                }
                lowband = Lowband::Scratch(0);
            }
            for k in 0..recombine {
                const BIT_INTERLEAVE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
                haar1(&mut self.x[x..], n >> k, 1 << k);
                if let Some(l) = self.lowband_mut(lowband) {
                    haar1(l, n >> k, 1 << k);
                }
                fill = BIT_INTERLEAVE[(fill & 0xF) as usize] as u32
                    | (BIT_INTERLEAVE[(fill >> 4) as usize] as u32) << 2;
            }
            blocks >>= recombine;
            n_b <<= recombine;
            // Increasing the time resolution.
            while (n_b & 1) == 0 && tf_change < 0 {
                haar1(&mut self.x[x..], n_b, blocks);
                if let Some(l) = self.lowband_mut(lowband) {
                    haar1(l, n_b, blocks);
                }
                fill |= fill << blocks;
                blocks <<= 1;
                n_b >>= 1;
                time_divide += 1;
                tf_change += 1;
            }
            b0 = blocks;
            n_b0 = n_b;
            // Reorder the samples in time order instead of frequency order.
            if b0 > 1 {
                deinterleave_hadamard(
                    &mut self.x[x..x + n],
                    &mut self.hadamard_tmp,
                    n_b >> recombine,
                    b0 << recombine,
                    long_blocks,
                );
                if lowband != Lowband::None {
                    let mut tmp = core::mem::take(&mut self.hadamard_tmp);
                    if let Some(l) = self.lowband_mut(lowband) {
                        deinterleave_hadamard(
                            &mut l[..n],
                            tmp.as_mut_slice(),
                            n_b >> recombine,
                            b0 << recombine,
                            long_blocks,
                        );
                    }
                    self.hadamard_tmp = tmp;
                }
            }
        }

        // Split the band when one codebook would need more than 32 bits.
        let cache_start = cache_index(i, lm);
        let cache0 = CACHE_BITS[cache_start] as usize;
        if !stereo && lm != -1 && b > CACHE_BITS[cache_start + cache0] as i32 + 12 && n > 2 {
            n >>= 1;
            y = Some(x + n);
            split = true;
            lm -= 1;
            if blocks == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            blocks = (blocks + 1) >> 1;
        }

        let mut mid = 0.0f32;
        if split {
            let pulse_cap = LOG_N[i] + lm * (1 << BITRES);
            let offset = (pulse_cap >> 1)
                - if stereo && n == 2 {
                    QTHETA_OFFSET_TWOPHASE
                } else {
                    QTHETA_OFFSET
                };
            let mut qn = compute_qn(n as i32, b, offset, pulse_cap, stereo);
            if stereo && i >= intensity {
                qn = 1;
            }
            let tell = dec.tell_frac() as i32;
            let mut itheta: i32 = 0;
            if qn != 1 {
                if stereo && n > 2 {
                    // A step pdf: probability 3 up to itheta=qn/2, then 1.
                    let p0 = 3i32;
                    let x0 = qn / 2;
                    let ft = p0 * (x0 + 1) + x0;
                    let fs = dec.decode(ft as u32) as i32;
                    let xv = if fs < (x0 + 1) * p0 {
                        fs / p0
                    } else {
                        x0 + 1 + (fs - (x0 + 1) * p0)
                    };
                    let (fl, fh) = if xv <= x0 {
                        (p0 * xv, p0 * (xv + 1))
                    } else {
                        ((xv - 1 - x0) + (x0 + 1) * p0, (xv - x0) + (x0 + 1) * p0)
                    };
                    dec.update(fl as u32, fh as u32, ft as u32);
                    itheta = xv;
                } else if b0 > 1 || stereo {
                    itheta = dec.dec_uint((qn + 1) as u32) as i32;
                } else {
                    // Triangular pdf.
                    let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
                    let fm = dec.decode(ft as u32) as i32;
                    let (fl, fs);
                    if fm < (((qn >> 1) * ((qn >> 1) + 1)) >> 1) {
                        itheta = ((isqrt32(8 * fm as u32 + 1) - 1) >> 1) as i32;
                        fs = itheta + 1;
                        fl = (itheta * (itheta + 1)) >> 1;
                    } else {
                        itheta = (2 * (qn + 1) - isqrt32(8 * (ft - fm - 1) as u32 + 1) as i32) >> 1;
                        fs = qn + 1 - itheta;
                        fl = ft - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1);
                    }
                    dec.update(fl as u32, (fl + fs) as u32, ft as u32);
                }
                itheta = (itheta * 16384) / qn;
            } else if stereo {
                inv = if b > 2 << BITRES && *remaining_bits > 2 << BITRES {
                    dec.dec_bit_logp(2)
                } else {
                    false
                };
                itheta = 0;
            }
            let qalloc = dec.tell_frac() as i32 - tell;
            b -= qalloc;

            let orig_fill = fill;
            let (imid, iside, mut delta);
            if itheta == 0 {
                imid = 32767;
                iside = 0;
                fill &= (1 << blocks) - 1;
                delta = -16384;
            } else if itheta == 16384 {
                imid = 0;
                iside = 32767;
                fill &= ((1u32 << blocks) - 1) << blocks;
                delta = 16384;
            } else {
                imid = bitexact_cos(itheta as i16) as i32;
                iside = bitexact_cos((16384 - itheta) as i16) as i32;
                // The mid/side split that minimises the squared error.
                delta = frac_mul16(
                    ((n as i32 - 1) << 7) as i16,
                    bitexact_log2tan(iside, imid) as i16,
                );
            }
            mid = imid as f32 / 32768.0;
            let side = iside as f32 / 32768.0;

            if n == 2 && stereo {
                // With two coefficients the side needs only a sign.
                let mut mbits = b;
                let mut sbits = 0;
                if itheta != 0 && itheta != 16384 {
                    sbits = 1 << BITRES;
                }
                mbits -= sbits;
                let yy = y.unwrap();
                let swap = itheta > 8192;
                *remaining_bits -= qalloc + sbits;
                let (x2, y2) = if swap { (yy, x) } else { (x, yy) };
                let mut sign = 0i32;
                if sbits != 0 {
                    sign = dec.dec_bits(1) as i32;
                }
                let sign = 1 - 2 * sign;
                cm = self.quant_band(
                    dec,
                    BandArgs {
                        x: x2,
                        y: None,
                        n,
                        b: mbits,
                        blocks,
                        tf_change,
                        lowband,
                        lm,
                        fill: orig_fill,
                        ..a
                    },
                    remaining_bits,
                );
                self.x[y2] = -(sign as f32) * self.x[x2 + 1];
                self.x[y2 + 1] = sign as f32 * self.x[x2];
                let x0 = self.x[x] * mid;
                let x1 = self.x[x + 1] * mid;
                let y0 = self.x[yy] * side;
                let y1 = self.x[yy + 1] * side;
                self.x[x] = x0 - y0;
                self.x[yy] = x0 + y0;
                self.x[x + 1] = x1 - y1;
                self.x[yy + 1] = x1 + y1;
            } else {
                // "Normal" split.
                if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                    if itheta > 8192 {
                        // Rough approximation for pre-echo masking.
                        delta -= delta >> (4 - lm);
                    } else {
                        delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
                    }
                }
                let mut mbits = 0.max(b.min((b - delta) / 2));
                let mut sbits = b - mbits;
                *remaining_bits -= qalloc;
                let next_lowband2 = if lowband != Lowband::None && !stereo {
                    lowband.offset(n)
                } else {
                    Lowband::None
                };
                let next_lowband_out1 = if stereo { lowband_out } else { None };
                let next_level = if stereo { level } else { level + 1 };
                let mid_args = BandArgs {
                    x,
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    lowband,
                    level: next_level,
                    lm,
                    lowband_out: next_lowband_out1,
                    gain: if stereo { 1.0 } else { gain * mid },
                    fill,
                    ..a
                };
                let side_args = BandArgs {
                    x: y.unwrap(),
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    lowband: next_lowband2,
                    level: next_level,
                    lm,
                    lowband_out: None,
                    gain: gain * side,
                    fill: fill >> blocks,
                    ..a
                };
                // A mono split interleaves the two halves' collapse masks; a
                // stereo split keeps the side's mask in place.
                let shift = if stereo { 0 } else { b0 >> 1 };
                let mut rebalance = *remaining_bits;
                if mbits >= sbits {
                    cm = self.quant_band(
                        dec,
                        BandArgs {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                    rebalance = mbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 0 {
                        sbits += rebalance - (3 << BITRES);
                    }
                    cm |= self.quant_band(
                        dec,
                        BandArgs {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    ) << shift;
                } else {
                    cm = self.quant_band(
                        dec,
                        BandArgs {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    ) << shift;
                    rebalance = sbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 16384 {
                        mbits += rebalance - (3 << BITRES);
                    }
                    cm |= self.quant_band(
                        dec,
                        BandArgs {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                }
            }
        } else {
            // No split: straight PVQ, or folding when there are no pulses.
            let mut q = bits2pulses(i, lm, b);
            let mut curr_bits = pulses2bits(i, lm, q);
            *remaining_bits -= curr_bits;
            // Never bust the budget.
            while *remaining_bits < 0 && q > 0 {
                *remaining_bits += curr_bits;
                q -= 1;
                curr_bits = pulses2bits(i, lm, q);
                *remaining_bits -= curr_bits;
            }
            if q != 0 {
                let k = get_pulses(q) as usize;
                cm = self.alg_unquant(dec, x, n, k, spread, blocks, gain);
            } else {
                let cm_mask = (1u32 << blocks) - 1;
                fill &= cm_mask;
                if fill == 0 {
                    self.x[x..x + n].fill(0.0);
                } else if lowband == Lowband::None {
                    // Noise.
                    for j in 0..n {
                        self.rng = celt_lcg_rand(self.rng);
                        self.x[x + j] = ((self.rng as i32) >> 20) as f32;
                    }
                    cm = cm_mask;
                    renormalise(&mut self.x[x..x + n], gain);
                } else {
                    // Folded spectrum, about 48 dB below the normal level.
                    for j in 0..n {
                        self.rng = celt_lcg_rand(self.rng);
                        let tmp = if self.rng & 0x8000 != 0 {
                            1.0 / 256.0
                        } else {
                            -1.0 / 256.0
                        };
                        self.x[x + j] = self.lowband(lowband, j) + tmp;
                    }
                    cm = fill;
                    renormalise(&mut self.x[x..x + n], gain);
                }
            }
        }

        // Undo the reorganisations, and pass the band on for later folding.
        if stereo {
            if n != 2 {
                stereo_merge(&mut self.x, x, y.unwrap(), mid, n);
            }
            if inv {
                for j in 0..n {
                    self.x[y.unwrap() + j] = -self.x[y.unwrap() + j];
                }
            }
        } else if level == 0 {
            if b0 > 1 {
                interleave_hadamard(
                    &mut self.x[x..x + n0],
                    &mut self.hadamard_tmp,
                    n_b >> recombine,
                    b0 << recombine,
                    long_blocks,
                );
            }
            n_b = n_b0;
            let mut bb = b0;
            for _ in 0..time_divide {
                bb >>= 1;
                n_b <<= 1;
                cm |= cm >> bb;
                haar1(&mut self.x[x..], n_b, bb);
            }
            for k in 0..recombine {
                const BIT_DEINTERLEAVE: [u8; 16] = [
                    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0,
                    0xF3, 0xFC, 0xFF,
                ];
                cm = BIT_DEINTERLEAVE[(cm & 0xF) as usize] as u32;
                haar1(&mut self.x[x..], n0 >> k, 1 << k);
            }
            bb <<= recombine;
            // Scale for use as a folding source.
            if let Some(o) = lowband_out {
                let nrm = (n0 as f32).sqrt();
                for j in 0..n0 {
                    self.norm[o + j] = nrm * self.x[x + j];
                }
            }
            cm &= (1 << bb) - 1;
        }
        cm
    }

    /// Reads one sample of the folding source.
    fn lowband(&self, lb: Lowband, j: usize) -> f32 {
        match lb {
            Lowband::None => 0.0,
            Lowband::Norm(l) => self.norm[l + j],
            Lowband::Scratch(o) => self.lowband_scratch[o + j],
        }
    }

    /// The folding source as a mutable slice, for the in-place Haar passes.
    fn lowband_mut(&mut self, lb: Lowband) -> Option<&mut [f32]> {
        match lb {
            Lowband::None => None,
            Lowband::Norm(l) => Some(&mut self.norm[l..]),
            Lowband::Scratch(o) => Some(&mut self.lowband_scratch[o..]),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn alg_unquant(
        &mut self,
        dec: &mut RangeDecoder,
        x: usize,
        n: usize,
        k: usize,
        spread: usize,
        b_blocks: usize,
        gain: f32,
    ) -> u32 {
        if self.urow.len() < k + 2 {
            self.urow.resize(k + 2, 0);
        }
        decode_pulses(dec, n, k, &mut self.iy, &mut self.urow);
        let mut ryy = 0.0f32;
        for j in 0..n {
            ryy += (self.iy[j] * self.iy[j]) as f32;
        }
        let g = gain / ryy.sqrt();
        for j in 0..n {
            self.x[x + j] = g * self.iy[j] as f32;
        }
        exp_rotation(&mut self.x[x..x + n], n, -1, b_blocks, k, spread);
        // Collapse mask: which of the B short blocks got any pulse.
        if b_blocks <= 1 {
            return 1;
        }
        let n0 = n / b_blocks;
        let mut mask = 0u32;
        for bi in 0..b_blocks {
            for j in 0..n0 {
                mask |= u32::from(self.iy[bi * n0 + j] != 0) << bi;
            }
        }
        mask
    }

    fn anti_collapse(&mut self, lm: usize, c: usize, size: usize, start: usize, end: usize) {
        for i in start..end {
            let n0 = E_BANDS[i + 1] - E_BANDS[i];
            let depth = (1 + self.pulses[i]) / (n0 << lm) as i32;
            let thresh = 0.5 * (-0.125 * depth as f32).exp2();
            let sqrt_1 = 1.0 / ((n0 << lm) as f32).sqrt();
            for ch in 0..c {
                let mut prev1 = self.old_log_e[ch * NB_BANDS + i];
                let mut prev2 = self.old_log_e2[ch * NB_BANDS + i];
                if c == 1 {
                    prev1 = prev1.max(self.old_log_e[NB_BANDS + i]);
                    prev2 = prev2.max(self.old_log_e2[NB_BANDS + i]);
                }
                let ediff = (self.old_band_e[ch * NB_BANDS + i] - prev1.min(prev2)).max(0.0);
                let mut r = 2.0 * (-ediff).exp2();
                if lm == 3 {
                    r *= core::f32::consts::SQRT_2;
                }
                r = r.min(thresh) * sqrt_1;
                let base = ch * size + (E_BANDS[i] << lm);
                let mut renorm = false;
                for k in 0..(1usize << lm) {
                    if self.collapse_masks[i * c + ch] & (1 << k) == 0 {
                        for j in 0..n0 {
                            self.rng = celt_lcg_rand(self.rng);
                            self.x[base + (j << lm) + k] =
                                if self.rng & 0x8000 != 0 { r } else { -r };
                        }
                        renorm = true;
                    }
                }
                if renorm {
                    let len = n0 << lm;
                    renormalise(&mut self.x[base..base + len], 1.0);
                }
            }
        }
    }

    fn denormalise_bands(&mut self, eff_end: usize, c: usize, m: usize, n: usize) {
        for ch in 0..c {
            for i in 0..eff_end {
                let g = self.band_e[i + ch * NB_BANDS];
                for j in m * E_BANDS[i]..m * E_BANDS[i + 1] {
                    self.freq[ch * n + j] = self.x[ch * n + j] * g;
                }
            }
            for j in m * E_BANDS[eff_end]..n {
                self.freq[ch * n + j] = 0.0;
            }
        }
    }

    fn inverse_mdcts(&mut self, short_blocks: usize, lm: usize, n: usize, cc: usize) {
        let (n2, blocks, plan_lm) = if short_blocks != 0 {
            (SHORT_MDCT, short_blocks, 0)
        } else {
            (n, 1, lm)
        };
        for ch in 0..cc {
            self.mdct_buf[..n + OVERLAP].fill(0.0);
            for b in 0..blocks {
                let spec_off = ch * n + b;
                let plan = &mut self.plans[plan_lm];
                plan.inverse(
                    &self.freq[spec_off..],
                    blocks,
                    &self.window,
                    &mut self.mdct_buf,
                    n2 * b,
                );
            }
            let out_base = ch * DECODE_BUFFER + DECODE_BUFFER - n;
            let ov_base = ch * OVERLAP;
            for j in 0..OVERLAP {
                self.decode_mem[out_base + j] = self.mdct_buf[j] + self.overlap_mem[ov_base + j];
            }
            for j in OVERLAP..n {
                self.decode_mem[out_base + j] = self.mdct_buf[j];
            }
            for j in 0..OVERLAP {
                self.overlap_mem[ov_base + j] = self.mdct_buf[n + j];
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn comb_filter(
        &mut self,
        base: usize,
        n: usize,
        t0: usize,
        t1: usize,
        g0: f32,
        g1: f32,
        tapset0: usize,
        tapset1: usize,
        _in_place: bool,
    ) {
        if g0 == 0.0 && g1 == 0.0 {
            return;
        }
        let g00 = g0 * POSTFILTER_GAINS[tapset0][0];
        let g01 = g0 * POSTFILTER_GAINS[tapset0][1];
        let g02 = g0 * POSTFILTER_GAINS[tapset0][2];
        let g10 = g1 * POSTFILTER_GAINS[tapset1][0];
        let g11 = g1 * POSTFILTER_GAINS[tapset1][1];
        let g12 = g1 * POSTFILTER_GAINS[tapset1][2];
        // The overlap region cross-fades between the old and new pitch.
        let ov = OVERLAP.min(n);
        for i in 0..ov {
            let f = self.window[i] * self.window[i];
            let x = |k: usize| self.decode_mem[base + i - k];
            let v = self.decode_mem[base + i]
                + (1.0 - f) * g00 * x(t0)
                + (1.0 - f) * g01 * (x(t0 + 1) + x(t0.wrapping_sub(1)))
                + (1.0 - f) * g02 * (x(t0 + 2) + x(t0.wrapping_sub(2)))
                + f * g10 * x(t1)
                + f * g11 * (x(t1 + 1) + x(t1.wrapping_sub(1)))
                + f * g12 * (x(t1 + 2) + x(t1.wrapping_sub(2)));
            self.decode_mem[base + i] = v;
        }
        for i in ov..n {
            let x = |k: usize| self.decode_mem[base + i - k];
            let v = self.decode_mem[base + i]
                + g10 * x(t1)
                + g11 * (x(t1 + 1) + x(t1.wrapping_sub(1)))
                + g12 * (x(t1 + 2) + x(t1.wrapping_sub(2)));
            self.decode_mem[base + i] = v;
        }
    }

    fn deemphasis(&mut self, pcm: &mut [f32], n: usize, cc: usize) {
        for ch in 0..cc {
            let base = ch * DECODE_BUFFER + DECODE_BUFFER - n;
            let mut mem = self.preemph_mem[ch];
            let mut count = 0usize;
            let mut y = ch;
            for j in 0..n {
                let x = self.decode_mem[base + j];
                let tmp = x + mem;
                mem = PREEMPH * tmp;
                if count == 0 {
                    pcm[y] = tmp / SIG_SCALE;
                }
                count += 1;
                if count == self.downsample {
                    y += cc;
                    count = 0;
                }
            }
            self.preemph_mem[ch] = mem;
        }
    }
}

/// `ec_laplace_decode()`: the coarse-energy prediction error.
pub(crate) fn laplace_decode(dec: &mut RangeDecoder, fs0: u32, decay: i32) -> i32 {
    const MINP: u32 = 1;
    const NMIN: u32 = 16;
    let mut val = 0i32;
    let mut fs = fs0;
    let fm = dec.decode_bin(15);
    let mut fl = 0u32;
    if fm >= fs {
        val += 1;
        fl = fs;
        let ft = 32768 - MINP * (2 * NMIN) - fs0;
        fs = ((ft * (16384 - decay as u32)) >> 15) + MINP;
        while fs > MINP && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = (((fs - 2 * MINP) * decay as u32) >> 15) + MINP;
            val += 1;
        }
        if fs <= MINP {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di * MINP;
        }
        if fm < fl + fs {
            val = -val;
        } else {
            fl += fs;
        }
    }
    dec.update(fl, (fl + fs).min(32768), 32768);
    val
}

/// `bitexact_cos()`: a cosine that must round identically everywhere, because
/// the bit allocation depends on it.
pub(crate) fn bitexact_cos(x: i16) -> i16 {
    let tmp = (4096 + (x as i32) * (x as i32)) >> 13;
    let mut x2 = tmp as i16;
    x2 = (32767 - x2 as i32
        + frac_mul16(
            x2,
            (-7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2) as i16)) as i16,
        )) as i16;
    1 + x2
}

pub(crate) fn frac_mul16(a: i16, b: i16) -> i32 {
    (16384 + (a as i32) * (b as i32)) >> 15
}

pub(crate) fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ilog(icos as u32);
    let ls = ilog(isin as u32);
    let isin = (isin << (15 - ls)) as i16;
    let icos = (icos << (15 - lc)) as i16;
    (ls - lc) * (1 << 11) + frac_mul16(isin, (frac_mul16(isin, -2597) + 7932) as i16)
        - frac_mul16(icos, (frac_mul16(icos, -2597) + 7932) as i16)
}

pub(crate) fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b - pulse_cap - (4 << BITRES)).min((b + n2 * offset) / n2);
    qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let qn = EXP2_TABLE8[(qb & 0x7) as usize] >> (14 - (qb >> BITRES));
        (qn + 1) >> 1 << 1
    }
}

pub(crate) fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

pub(crate) fn bits2pulses(band: usize, lm: i32, bits: i32) -> i32 {
    let cache = &CACHE_BITS[cache_index(band, lm)..];
    let mut lo = 0i32;
    let mut hi = cache[0] as i32;
    let bits = bits - 1;
    for _ in 0..6 {
        let mid = (lo + hi + 1) >> 1;
        if cache[mid as usize] as i32 >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_cost = if lo == 0 {
        -1
    } else {
        cache[lo as usize] as i32
    };
    if bits - lo_cost <= cache[hi as usize] as i32 - bits {
        lo
    } else {
        hi
    }
}

pub(crate) fn pulses2bits(band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        return 0;
    }
    let cache = &CACHE_BITS[cache_index(band, lm)..];
    cache[pulses as usize] as i32 + 1
}

/// One level of the Haar transform, used for the time/frequency changes.
pub(crate) fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let n0 = n0 >> 1;
    const S: f32 = core::f32::consts::FRAC_1_SQRT_2;
    for i in 0..stride {
        for j in 0..n0 {
            let a = S * x[stride * 2 * j + i];
            let b = S * x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = a + b;
            x[stride * (2 * j + 1) + i] = a - b;
        }
    }
}

pub(crate) fn deinterleave_hadamard(
    x: &mut [f32],
    tmp: &mut [f32],
    n0: usize,
    stride: usize,
    hadamard: bool,
) {
    let n = n0 * stride;
    let tmp = &mut tmp[..n];
    if hadamard {
        let order = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[order[i] * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(tmp);
}

fn interleave_hadamard(x: &mut [f32], tmp: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let tmp = &mut tmp[..n];
    if hadamard {
        let order = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[order[i] * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(tmp);
}

/// Scales a vector to `gain` in the L2 sense.
fn renormalise(x: &mut [f32], gain: f32) {
    let mut e = 1e-15f32;
    for v in x.iter() {
        e += v * v;
    }
    let g = gain / e.sqrt();
    for v in x.iter_mut() {
        *v *= g;
    }
}

/// Recombines the mid/side pair back into left/right (`stereo_merge`).
fn stereo_merge(buf: &mut [f32], x: usize, y: usize, mid: f32, n: usize) {
    let mut xp = 0.0f32;
    let mut side = 0.0f32;
    for j in 0..n {
        xp += buf[x + j] * buf[y + j];
        side += buf[y + j] * buf[y + j];
    }
    xp *= mid;
    let mid2 = mid;
    let el = mid2 * mid2 + side - 2.0 * xp;
    let er = mid2 * mid2 + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        for j in 0..n {
            buf[y + j] = buf[x + j];
        }
        return;
    }
    let lgain = 1.0 / el.sqrt();
    let rgain = 1.0 / er.sqrt();
    for j in 0..n {
        let l = mid * buf[x + j];
        let r = buf[y + j];
        buf[x + j] = lgain * (l - r);
        buf[y + j] = rgain * (l + r);
    }
}

/// The spreading rotation (Section 4.3.4.3), applied in the decode direction.
pub(crate) fn exp_rotation(
    x: &mut [f32],
    len: usize,
    dir: i32,
    stride: usize,
    k: usize,
    spread: usize,
) {
    const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[spread - 1];
    let gain = len as f32 / (len + factor * k) as f32;
    let theta = 0.5 * gain * gain;
    let c = (core::f32::consts::FRAC_PI_2 * theta).cos();
    let s = (core::f32::consts::FRAC_PI_2 * (1.0 - theta)).cos();
    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let sub = len / stride;
    for i in 0..stride {
        let seg = &mut x[i * sub..(i + 1) * sub];
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(seg, sub, stride2, s, c);
            }
            exp_rotation1(seg, sub, 1, c, s);
        } else {
            exp_rotation1(seg, sub, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(seg, sub, stride2, s, -c);
            }
        }
    }
}

fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    for i in 0..len.saturating_sub(stride) {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
    if len > 2 * stride {
        for i in (0..=len - 2 * stride - 1).rev() {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 - s * x2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_power_complementary() {
        // Princen-Bradley: w[n]^2 + w[L-1-n]^2 == 1, which is what makes the
        // overlap-add reconstruct exactly.
        let w = overlap_window();
        for i in 0..OVERLAP {
            let s = w[i] * w[i] + w[OVERLAP - 1 - i] * w[OVERLAP - 1 - i];
            assert!((s - 1.0).abs() < 1e-6, "w[{i}]^2 + w[..]^2 = {s}");
        }
    }

    #[test]
    fn fft15_matches_a_direct_transform() {
        for &n in &[60usize, 120, 240, 480] {
            let mut re: Vec<f32> = (0..n).map(|i| ((i * 37 % 19) as f32) - 9.0).collect();
            let mut im: Vec<f32> = (0..n).map(|i| ((i * 11 % 7) as f32) - 3.0).collect();
            let (re0, im0) = (re.clone(), im.clone());
            let mut fft = Fft15::new(n);
            fft.inverse(&mut re, &mut im);
            for k in 0..n {
                let (mut sr, mut si) = (0.0f64, 0.0f64);
                for j in 0..n {
                    let a = 2.0 * core::f64::consts::PI * (j * k % n) as f64 / n as f64;
                    sr += re0[j] as f64 * a.cos() - im0[j] as f64 * a.sin();
                    si += re0[j] as f64 * a.sin() + im0[j] as f64 * a.cos();
                }
                assert!(
                    (sr - re[k] as f64).abs() < 1e-2 * n as f64,
                    "n={n} k={k} re {sr} vs {}",
                    re[k]
                );
                assert!(
                    (si - im[k] as f64).abs() < 1e-2 * n as f64,
                    "n={n} k={k} im"
                );
            }
        }
    }

    /// The MDCT of a `2L` window, straight from the definition — the slow
    /// reference the fast inverse is checked against.
    fn direct_mdct(x: &[f32], l: usize) -> Vec<f32> {
        (0..l)
            .map(|k| {
                let mut sum = 0.0f64;
                for (n, &v) in x.iter().enumerate().take(2 * l) {
                    let a = core::f64::consts::PI / l as f64
                        * (n as f64 + 0.5 + l as f64 / 2.0)
                        * (k as f64 + 0.5);
                    sum += v as f64 * a.cos();
                }
                sum as f32
            })
            .collect()
    }

    #[test]
    fn imdct_overlap_add_reconstructs_the_signal() {
        // Forward by definition, inverse through the FFT path, overlap-add at
        // hop L: the middle of the signal must come back. This pins down every
        // index, sign and window position in the inverse; the one free
        // parameter left is the overall gain, which the test reports.
        for &l in &[120usize, 240, 480] {
            let frames = 4;
            let total = (frames + 1) * l;
            let sig: Vec<f32> = (0..total)
                .map(|i| ((i * 7919 % 1000) as f32 / 500.0) - 1.0)
                .collect();
            // The 2L analysis window: zero, rise, ones, fall, zero.
            let w = overlap_window();
            let flat = (l - OVERLAP) / 2;
            let mut win = vec![0.0f32; 2 * l];
            for n in 0..l {
                win[n] = if n < flat {
                    0.0
                } else if n < flat + OVERLAP {
                    w[n - flat]
                } else {
                    1.0
                };
                win[2 * l - 1 - n] = win[n];
            }
            let mut plan = ImdctPlan::new(l);
            let mut out = vec![0.0f32; total + 2 * l];
            for t in 0..frames {
                let start = t * l;
                let mut block = vec![0.0f32; 2 * l];
                for n in 0..2 * l {
                    block[n] = win[n] * sig.get(start + n).copied().unwrap_or(0.0);
                }
                let spec = direct_mdct(&block, l);
                plan.inverse(&spec, 1, &w, &mut out, start + flat);
            }
            // Compare where two windows fully overlap.
            let lo = l + OVERLAP;
            let hi = frames * l - OVERLAP;
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for i in lo..hi {
                num += out[i] as f64 * sig[i] as f64;
                den += sig[i] as f64 * sig[i] as f64;
            }
            let gain = num / den;
            let mut err = 0.0f64;
            for i in lo..hi {
                let d = out[i] as f64 - gain * sig[i] as f64;
                err += d * d;
            }
            let rel = (err / den).sqrt();
            assert!(
                rel < 1e-3,
                "L={l}: overlap-add residual {rel} at gain {gain}"
            );
            // CELT puts the 2/N normalisation on the *forward* transform, so
            // this inverse is the plain sum and the loop above, whose forward
            // has no scaling at all, reconstructs at L/2.
            assert!(
                (gain - l as f64 / 2.0).abs() < 1e-3 * gain,
                "L={l}: gain {gain}, expected {}",
                l as f64 / 2.0
            );
        }
    }

    #[test]
    fn pvq_index_round_trips_within_v() {
        // V(N,K) from the recursion in Section 4.3.4.2, checked against the
        // closed forms the RFC gives for small N.
        let mut u = vec![0u32; 300];
        assert_eq!(pvq_urow(2, 1, &mut u), 4);
        assert_eq!(pvq_urow(2, 3, &mut u), 12);
        assert_eq!(pvq_urow(3, 2, &mut u), 18);
        assert_eq!(pvq_urow(4, 2, &mut u), 32);
        // V(N,K) = V(N-1,K) + V(N,K-1) + V(N-1,K-1).
        for n in 3..8usize {
            for k in 2..8usize {
                let mut a = vec![0u32; 300];
                let v = pvq_urow(n, k, &mut a);
                let v1 = pvq_urow(n - 1, k, &mut a);
                let v2 = pvq_urow(n, k - 1, &mut a);
                let v3 = pvq_urow(n - 1, k - 1, &mut a);
                assert_eq!(v, v1 + v2 + v3, "V({n},{k})");
            }
        }
    }
}
