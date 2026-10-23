//! Parametric Stereo reconstruction (ISO/IEC 14496-3 subpart 8.6.4).
//!
//! This is a port of FFmpeg's `aacps.c` and `aacpsdsp_template.c`, with the
//! same index conventions and the same closed-form tables (`aacps_tablegen.h`),
//! computed here in `f64`. The mono SBR QMF output is complex-valued: a QMF
//! sample is `Complex { re, im }`, and the reference stores it as `[re, im]`.
//!
//! Per frame: the low QMF bands go through a hybrid filterbank (bands 0..2 are
//! split into 6+2+2 subbands in 20-band mode, 12+8+4+4+4 in 34-band mode),
//! a decorrelator makes a second signal `d` from the mono signal `s`, and per
//! stereo band the IID/ICC (optionally IPD/OPD) parameters mix `(s, d)` into
//! the left and right signals. Envelope borders interpolate the mixing matrices
//! linearly. The hybrid synthesis folds the subbands back to QMF bands.

use crate::ps_data::{MAX_ENV, MAX_PAR, PsCommon};
use crate::ps_huff::{K_TO_I_20, K_TO_I_34};
use ec_dsp::Complex;
use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2};
use std::ops::Range;
use std::sync::OnceLock;

type Cx = Complex<f64>;

/// QMF time slots per SBR frame (`PS_QMF_TIME_SLOTS`).
const SLOTS: usize = 32;
/// Slots the SBR hands to PS per frame: the frame plus the 6-slot lookahead.
pub const IN_SLOTS: usize = 38;
/// QMF bands of the SBR synthesis.
pub const QMF_BANDS: usize = 64;
/// Lookahead of the hybrid filter, in slots.
const HIST: usize = 6;
/// Hybrid input window per low band: history plus the 38 new slots.
const IN_BUF_LEN: usize = HIST + IN_SLOTS;
/// Low QMF bands that the hybrid filterbank splits.
const LOW_BANDS: usize = 5;
/// Subband rows of the hybrid filterbank.
const MAX_SUB: usize = 91;
const MAX_DELAY: usize = 14;
const MAX_AP_DELAY: usize = 5;
const AP_LINKS: usize = 3;
const MAX_ALLPASS: usize = 50;

const NR_PAR_BANDS: [usize; 2] = [20, 34];
const NR_IPDOPD_BANDS: [usize; 2] = [11, 17];
const NR_BANDS: [usize; 2] = [71, 91];
const DECAY_CUTOFF: [i32; 2] = [10, 32];
const NR_ALLPASS_BANDS: [usize; 2] = [30, 50];
const SHORT_DELAY_BAND: [usize; 2] = [42, 62];
/// All-pass decay slope per band above the cutoff.
const DECAY_SLOPE: f64 = 0.05;
const PEAK_DECAY_FACTOR: f64 = 0.765_928_338_364_65;
const TRANSIENT_IMPACT: f64 = 1.5;
const A_SMOOTH: f64 = 0.25;
/// All-pass feedback coefficients (`a[m]`).
const AP_A: [f64; AP_LINKS] = [0.651_439_057_531_06, 0.564_718_122_007_76, 0.489_541_659_556_95];

/// Hybrid prototype filters (`g0_Q8`, `g0_Q12`, `g1_Q8`, `g2_Q4`, `g1_Q2`).
const G0_Q8: [f64; 7] = [0.007_460_829_498_12, 0.022_704_209_498_25, 0.045_468_659_304_73, 0.072_661_139_295_91, 0.098_851_085_752_64, 0.117_937_105_672_17, 0.125];
const G0_Q12: [f64; 7] = [0.040_811_799_246_92, 0.038_128_109_949_26, 0.051_449_081_356_99, 0.063_998_311_515_92, 0.074_283_138_011_06, 0.081_003_478_929_14, 0.083_333_333_333_33];
const G1_Q8: [f64; 7] = [0.015_656_756_001_22, 0.037_527_163_919_91, 0.054_178_913_787_82, 0.084_170_441_167_67, 0.103_073_441_580_36, 0.122_224_522_497_53, 0.125];
const G2_Q4: [f64; 7] = [-0.059_082_111_556_39, -0.048_714_983_749_46, 0.0, 0.077_787_239_158_51, 0.164_863_035_674_03, 0.232_798_566_629_96, 0.25];
const G1_Q2: [f64; 7] = [0.0, 0.018_994_875_260_49, 0.0, -0.072_931_391_675_38, 0.0, 0.305_966_305_451_68, 0.5];

/// `iid_par_dequant_default` followed by `iid_par_dequant_fine` (46 values).
const IID_DEQUANT: [f64; 46] = [
    0.056_234_132_519_03, 0.125_892_541_179_42, 0.199_526_231_496_89, 0.316_227_766_016_84,
    0.446_683_592_150_96, 0.630_957_344_480_19, 0.794_328_234_724_28, 1.0,
    1.258_925_411_794_17, 1.584_893_192_461_11, 2.238_721_138_568_34, 3.162_277_660_168_38,
    5.011_872_336_272_72, 7.943_282_347_242_84, 17.782_794_100_389_2,
    0.003_162_277_660_17, 0.005_623_413_251_90, 0.01, 0.017_782_794_100_39,
    0.031_622_776_601_68, 0.056_234_132_519_03, 0.079_432_823_472_43, 0.112_201_845_430_20,
    0.158_489_319_246_11, 0.223_872_113_856_83, 0.316_227_766_016_84, 0.398_107_170_553_50,
    0.501_187_233_627_27, 0.630_957_344_480_19, 0.794_328_234_724_28, 1.0,
    1.258_925_411_794_17, 1.584_893_192_461_11, 1.995_262_314_968_88, 2.511_886_431_509_58,
    3.162_277_660_168_38, 4.466_835_921_509_63, 6.309_573_444_801_93, 8.912_509_381_337_45,
    12.589_254_117_941_7, 17.782_794_100_389_2, 31.622_776_601_683_8, 56.234_132_519_034_9,
    100.0, 177.827_941_003_892, 316.227_766_016_837,
];
const ICC_INVQ: [f64; 8] = [1.0, 0.937, 0.841_18, 0.600_92, 0.367_64, 0.0, -0.589, -1.0];
const ACOS_ICC_INVQ: [f64; 8] = [0.0, 0.356_855_27, 0.571_334_66, 0.926_144_72, 1.194_326_3, PI / 2.0, 2.200_617_1, PI];
const IPDOPD_SIN: [f64; 8] = [0.0, FRAC_1_SQRT_2, 1.0, FRAC_1_SQRT_2, 0.0, -FRAC_1_SQRT_2, -1.0, -FRAC_1_SQRT_2];
const IPDOPD_COS: [f64; 8] = [1.0, FRAC_1_SQRT_2, 0.0, -FRAC_1_SQRT_2, -1.0, -FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2];
const F_CENTER_20: [f64; 10] = [-3.0, -1.0, 1.0, 3.0, 5.0, 7.0, 10.0, 14.0, 18.0, 22.0];
const F_CENTER_34: [f64; 32] = [
    2.0, 6.0, 10.0, 14.0, 18.0, 22.0, 26.0, 30.0, 34.0, -10.0, -6.0, -2.0, 51.0, 57.0, 15.0, 21.0,
    27.0, 33.0, 39.0, 45.0, 54.0, 66.0, 78.0, 42.0, 102.0, 66.0, 78.0, 90.0, 102.0, 114.0, 126.0, 90.0,
];
const FRACTIONAL_DELAY_LINKS: [f64; AP_LINKS] = [0.43, 0.75, 0.347];
const FRACTIONAL_DELAY_GAIN: f64 = 0.39;

/// The closed-form tables of `ps_tableinit()` (`aacps_tablegen.h`).
struct Tables {
    /// Phase smoothing, indexed by three 3-bit phase digits (`pd0*64+pd1*8+pd2`).
    pd_re: Vec<f64>,
    pd_im: Vec<f64>,
    /// IID/ICC to mixing matrix: `[iid][icc] = [h11, h12, h21, h22]`.
    ha: Vec<[[f64; 4]; 8]>,
    hb: Vec<[[f64; 4]; 8]>,
    /// Hybrid analysis filters: `[band][tap] = [re, im]`.
    f20_0_8: [[[f64; 2]; 8]; 8],
    f34_0_12: [[[f64; 2]; 8]; 12],
    f34_1_8: [[[f64; 2]; 8]; 8],
    f34_2_4: [[[f64; 2]; 8]; 4],
    /// Per mode and all-pass band: the link rotations and the overall phase.
    q_fract: [Vec<[[f64; 2]; AP_LINKS]>; 2],
    phi_fract: [Vec<[f64; 2]>; 2],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(build_tables)
}

fn make_filters<const N: usize>(proto: &[f64; 7]) -> [[[f64; 2]; 8]; N] {
    let mut out = [[[0.0f64; 2]; 8]; N];
    for (q, band) in out.iter_mut().enumerate() {
        for (n, tap) in band.iter_mut().enumerate().take(7) {
            let theta = 2.0 * PI * (q as f64 + 0.5) * (n as f64 - 6.0) / N as f64;
            tap[0] = proto[n] * theta.cos();
            tap[1] = proto[n] * -theta.sin();
        }
    }
    out
}

fn build_tables() -> Tables {
    let mut pd_re = vec![0.0; 512];
    let mut pd_im = vec![0.0; 512];
    for pd0 in 0..8 {
        for pd1 in 0..8 {
            for pd2 in 0..8 {
                let re = 0.25 * IPDOPD_COS[pd0] + 0.5 * IPDOPD_COS[pd1] + IPDOPD_COS[pd2];
                let im = 0.25 * IPDOPD_SIN[pd0] + 0.5 * IPDOPD_SIN[pd1] + IPDOPD_SIN[pd2];
                let mag = 1.0 / im.hypot(re);
                pd_re[pd0 * 64 + pd1 * 8 + pd2] = re * mag;
                pd_im[pd0 * 64 + pd1 * 8 + pd2] = im * mag;
            }
        }
    }

    let mut ha = vec![[[0.0f64; 4]; 8]; 46];
    let mut hb = vec![[[0.0f64; 4]; 8]; 46];
    for (iid, &c) in IID_DEQUANT.iter().enumerate() {
        let c1 = SQRT_2 / (1.0 + c * c).sqrt();
        let c2 = c * c1;
        for icc in 0..8 {
            let alpha = 0.5 * ACOS_ICC_INVQ[icc];
            let beta = alpha * (c1 - c2) * FRAC_1_SQRT_2;
            ha[iid][icc] = [
                c2 * (beta + alpha).cos(),
                c1 * (beta - alpha).cos(),
                c2 * (beta + alpha).sin(),
                c1 * (beta - alpha).sin(),
            ];

            let rho = ICC_INVQ[icc].max(0.05);
            let mut alpha = 0.5 * (2.0 * c * rho).atan2(c * c - 1.0);
            let mu0 = c + 1.0 / c;
            let mu = (1.0 + (4.0 * rho * rho - 4.0) / (mu0 * mu0)).sqrt();
            let gamma = (((1.0 - mu) / (1.0 + mu)).sqrt()).atan();
            if alpha < 0.0 {
                alpha += PI / 2.0;
            }
            let (ac, asn) = (alpha.cos(), alpha.sin());
            let (gc, gs) = (gamma.cos(), gamma.sin());
            hb[iid][icc] = [SQRT_2 * ac * gc, SQRT_2 * asn * gc, -SQRT_2 * asn * gs, SQRT_2 * ac * gs];
        }
    }

    let mut q_fract: [Vec<[[f64; 2]; AP_LINKS]>; 2] = [Vec::new(), Vec::new()];
    let mut phi_fract: [Vec<[f64; 2]>; 2] = [Vec::new(), Vec::new()];
    for mode in 0..2 {
        for k in 0..NR_ALLPASS_BANDS[mode] {
            let f_center = if mode == 0 {
                F_CENTER_20.get(k).map_or(k as f64 - 6.5, |f| f * 0.125)
            } else {
                F_CENTER_34.get(k).map_or(k as f64 - 26.5, |f| f / 24.0)
            };
            let mut q = [[0.0f64; 2]; AP_LINKS];
            for (m, link) in q.iter_mut().enumerate() {
                let theta = -PI * FRACTIONAL_DELAY_LINKS[m] * f_center;
                *link = [theta.cos(), theta.sin()];
            }
            q_fract[mode].push(q);
            let theta = -PI * FRACTIONAL_DELAY_GAIN * f_center;
            phi_fract[mode].push([theta.cos(), theta.sin()]);
        }
    }

    Tables {
        pd_re,
        pd_im,
        ha,
        hb,
        f20_0_8: make_filters::<8>(&G0_Q8),
        f34_0_12: make_filters::<12>(&G0_Q12),
        f34_1_8: make_filters::<8>(&G1_Q8),
        f34_2_4: make_filters::<4>(&G2_Q4),
        q_fract,
        phi_fract,
    }
}

/// Maps a 10-band index to 20 bands (`map_idx_10_to_20`).
fn map_idx_10_to_20(dst: &mut [i8; MAX_PAR], par: &[i8; MAX_PAR], full: bool) {
    let mut b: isize = if full {
        9
    } else {
        dst[10] = 0;
        4
    };
    while b >= 0 {
        let v = par[b as usize];
        dst[2 * b as usize + 1] = v;
        dst[2 * b as usize] = v;
        b -= 1;
    }
}

/// Maps a 34-band index to 20 bands (`map_idx_34_to_20`).
fn map_idx_34_to_20(dst: &mut [i8; MAX_PAR], par: &[i8; MAX_PAR], full: bool) {
    let p = |i: usize| i32::from(par[i]);
    let half = |a: i32, b: i32| (a + b) / 2;
    dst[0] = ((2 * p(0) + p(1)) / 3) as i8;
    dst[1] = ((p(1) + 2 * p(2)) / 3) as i8;
    dst[2] = ((2 * p(3) + p(4)) / 3) as i8;
    dst[3] = ((p(4) + 2 * p(5)) / 3) as i8;
    dst[4] = half(p(6), p(7)) as i8;
    dst[5] = half(p(8), p(9)) as i8;
    dst[6] = par[10];
    dst[7] = par[11];
    dst[8] = half(p(12), p(13)) as i8;
    dst[9] = half(p(14), p(15)) as i8;
    dst[10] = par[16];
    if full {
        dst[11] = par[17];
        dst[12] = par[18];
        dst[13] = par[19];
        dst[14] = half(p(20), p(21)) as i8;
        dst[15] = half(p(22), p(23)) as i8;
        dst[16] = half(p(24), p(25)) as i8;
        dst[17] = half(p(26), p(27)) as i8;
        dst[18] = ((p(28) + p(29) + p(30) + p(31)) / 4) as i8;
        dst[19] = half(p(32), p(33)) as i8;
    }
}

/// Maps 34-band values to 20 bands in place (`map_val_34_to_20`).
fn map_val_34_to_20(par: &mut [f64; MAX_PAR]) {
    const THIRD: f64 = 0.333_333_33;
    let half = |a: f64, b: f64| (a + b) * 0.5;
    par[0] = (2.0 * par[0] + par[1]) * THIRD;
    par[1] = (par[1] + 2.0 * par[2]) * THIRD;
    par[2] = (2.0 * par[3] + par[4]) * THIRD;
    par[3] = (par[4] + 2.0 * par[5]) * THIRD;
    par[4] = half(par[6], par[7]);
    par[5] = half(par[8], par[9]);
    par[6] = par[10];
    par[7] = par[11];
    par[8] = half(par[12], par[13]);
    par[9] = half(par[14], par[15]);
    par[10] = par[16];
    par[11] = par[17];
    par[12] = par[18];
    par[13] = par[19];
    par[14] = half(par[20], par[21]);
    par[15] = half(par[22], par[23]);
    par[16] = half(par[24], par[25]);
    par[17] = half(par[26], par[27]);
    par[18] = (par[28] + par[29] + par[30] + par[31]) * 0.25;
    par[19] = half(par[32], par[33]);
}

/// Maps a 10-band index to 34 bands (`map_idx_10_to_34`).
fn map_idx_10_to_34(dst: &mut [i8; MAX_PAR], par: &[i8; MAX_PAR], full: bool) {
    if full {
        dst[28..34].fill(par[9]);
        dst[24..28].fill(par[8]);
        dst[20..24].fill(par[7]);
        dst[19] = par[6];
        dst[18] = par[6];
        dst[17] = par[5];
        dst[16] = par[5];
    } else {
        dst[16] = 0;
    }
    dst[15] = par[4];
    dst[14] = par[4];
    dst[13] = par[4];
    dst[12] = par[4];
    dst[11] = par[3];
    dst[10] = par[3];
    dst[9] = par[2];
    dst[8] = par[2];
    dst[7] = par[2];
    dst[6] = par[2];
    dst[5] = par[1];
    dst[4] = par[1];
    dst[3] = par[1];
    dst[2] = par[0];
    dst[1] = par[0];
    dst[0] = par[0];
}

/// Maps a 20-band index to 34 bands (`map_idx_20_to_34`).
fn map_idx_20_to_34(dst: &mut [i8; MAX_PAR], par: &[i8; MAX_PAR], full: bool) {
    let p = |i: usize| i32::from(par[i]);
    if full {
        dst[33] = par[19];
        dst[32] = par[19];
        dst[31] = par[18];
        dst[30] = par[18];
        dst[29] = par[18];
        dst[28] = par[18];
        dst[27] = par[17];
        dst[26] = par[17];
        dst[25] = par[16];
        dst[24] = par[16];
        dst[23] = par[15];
        dst[22] = par[15];
        dst[21] = par[14];
        dst[20] = par[14];
        dst[19] = par[13];
        dst[18] = par[12];
        dst[17] = par[11];
    }
    dst[16] = par[10];
    dst[15] = par[9];
    dst[14] = par[9];
    dst[13] = par[8];
    dst[12] = par[8];
    dst[11] = par[7];
    dst[10] = par[6];
    dst[9] = par[5];
    dst[8] = par[5];
    dst[7] = par[4];
    dst[6] = par[4];
    dst[5] = par[3];
    dst[4] = ((p(2) + p(3)) / 2) as i8;
    dst[3] = par[2];
    dst[2] = par[1];
    dst[1] = ((p(0) + p(1)) / 2) as i8;
    dst[0] = par[0];
}

/// Maps 20-band values to 34 bands in place (`map_val_20_to_34`).
fn map_val_20_to_34(par: &mut [f64; MAX_PAR]) {
    par[33] = par[19];
    par[32] = par[19];
    par[31] = par[18];
    par[30] = par[18];
    par[29] = par[18];
    par[28] = par[18];
    par[27] = par[17];
    par[26] = par[17];
    par[25] = par[16];
    par[24] = par[16];
    par[23] = par[15];
    par[22] = par[15];
    par[21] = par[14];
    par[20] = par[14];
    par[19] = par[13];
    par[18] = par[12];
    par[17] = par[11];
    par[16] = par[10];
    par[15] = par[9];
    par[14] = par[9];
    par[13] = par[8];
    par[12] = par[8];
    par[11] = par[7];
    par[10] = par[6];
    par[9] = par[5];
    par[8] = par[5];
    par[7] = par[4];
    par[6] = par[4];
    par[5] = par[3];
    par[4] = (par[2] + par[3]) * 0.5;
    par[3] = par[2];
    par[2] = par[1];
    par[1] = (par[0] + par[1]) * 0.5;
}

/// Remaps an index array onto the 34-band layout (`remap34`).
fn remap34(par: &[[i8; MAX_PAR]; MAX_ENV], num_par: usize, num_env: usize, full: bool) -> [[i8; MAX_PAR]; MAX_ENV] {
    let mut out = *par;
    if num_par == 20 || num_par == 11 {
        for e in 0..num_env.min(MAX_ENV) {
            map_idx_20_to_34(&mut out[e], &par[e], full);
        }
    } else if num_par == 10 || num_par == 5 {
        for e in 0..num_env.min(MAX_ENV) {
            map_idx_10_to_34(&mut out[e], &par[e], full);
        }
    }
    out
}

/// Remaps an index array onto the 20-band layout (`remap20`).
fn remap20(par: &[[i8; MAX_PAR]; MAX_ENV], num_par: usize, num_env: usize, full: bool) -> [[i8; MAX_PAR]; MAX_ENV] {
    let mut out = *par;
    if num_par == 34 || num_par == 17 {
        for e in 0..num_env.min(MAX_ENV) {
            map_idx_34_to_20(&mut out[e], &par[e], full);
        }
    } else if num_par == 10 || num_par == 5 {
        for e in 0..num_env.min(MAX_ENV) {
            map_idx_10_to_20(&mut out[e], &par[e], full);
        }
    }
    out
}

/// Lifts an SBR QMF sample (single precision) to the working precision.
fn lift(v: Complex<f32>) -> Cx {
    Cx { re: f64::from(v.re), im: f64::from(v.im) }
}

/// Lowers a working-precision QMF slab back to the SBR's single precision.
fn narrow(v: &[[Cx; QMF_BANDS]; SLOTS]) -> [[Complex<f32>; QMF_BANDS]; SLOTS] {
    let mut out = [[Complex { re: 0.0f32, im: 0.0f32 }; QMF_BANDS]; SLOTS];
    for (o, row) in out.iter_mut().zip(v.iter()) {
        for (dst, src) in o.iter_mut().zip(row.iter()) {
            *dst = Complex { re: src.re as f32, im: src.im as f32 };
        }
    }
    out
}

/// One output plane of the PS engine: `[slot][QMF band]`.
type QmfPlane = [[Complex<f32>; QMF_BANDS]; SLOTS];

/// Symmetric-real filter bank of one hybrid band (`ps_hybrid_analysis_c`).
///
/// Output `i` is centred on input slot `i + 6`, reading slots `i..i + 13`.
fn analysis_bank<const N: usize>(inp: &[Cx; IN_BUF_LEN], filt: &[[[f64; 2]; 8]; N], out: &mut [[Cx; SLOTS]]) {
    for i in 0..SLOTS {
        let w = &inp[i..i + 13];
        let mut inre0 = [0.0f64; 6];
        let mut inre1 = [0.0f64; 6];
        let mut inim0 = [0.0f64; 6];
        let mut inim1 = [0.0f64; 6];
        for j in 0..6 {
            inre0[j] = w[j].re + w[12 - j].re;
            inre1[j] = w[j].im - w[12 - j].im;
            inim0[j] = w[j].im + w[12 - j].im;
            inim1[j] = w[j].re - w[12 - j].re;
        }
        for (n, row) in out.iter_mut().enumerate().take(N) {
            let f = &filt[n];
            let mut sum_re = f[6][0] * w[6].re;
            let mut sum_im = f[6][0] * w[6].im;
            for j in 0..6 {
                sum_re += f[j][0] * inre0[j] - f[j][1] * inre1[j];
                sum_im += f[j][0] * inim0[j] + f[j][1] * inim1[j];
            }
            row[i] = Cx { re: sum_re, im: sum_im };
        }
    }
}

/// Splits one low band into two subbands with a symmetric real filter
/// (`hybrid2_re`). `reverse` selects which of the two rows gets the sum.
fn hybrid2(inp: &[Cx; IN_BUF_LEN], base: usize, reverse: bool, out: &mut [[Cx; SLOTS]; MAX_SUB]) {
    let r = usize::from(reverse);
    for i in 0..SLOTS {
        let w = &inp[i..];
        let re_in = G1_Q2[6] * w[6].re;
        let im_in = G1_Q2[6] * w[6].im;
        let mut re_op = 0.0;
        let mut im_op = 0.0;
        for j in [0usize, 2, 4] {
            re_op += G1_Q2[j + 1] * (w[j + 1].re + w[11 - j].re);
            im_op += G1_Q2[j + 1] * (w[j + 1].im + w[11 - j].im);
        }
        out[base + r][i] = Cx { re: re_in + re_op, im: im_in + im_op };
        out[base + 1 - r][i] = Cx { re: re_in - re_op, im: im_in - im_op };
    }
}

/// Copies QMF bands `start..64` straight into subband rows `base + band`
/// (`hybrid_analysis_ileave`).
fn interleave(input: &[[Complex<f32>; QMF_BANDS]; IN_SLOTS], base: usize, start: usize, out: &mut [[Cx; SLOTS]; MAX_SUB]) {
    for band in start..QMF_BANDS {
        for j in 0..SLOTS {
            out[base + band][j] = lift(input[j][band]);
        }
    }
}

/// Folds subband rows back into QMF bands (`hybrid_synthesis`).
fn hybrid_synthesis(sub: &[[Cx; SLOTS]; MAX_SUB], is34: bool) -> [[Cx; QMF_BANDS]; SLOTS] {
    let mut out = [[Cx::default(); QMF_BANDS]; SLOTS];
    let sum = |n: usize, rows: Range<usize>| {
        let mut acc = Cx::default();
        for r in rows {
            acc.re += sub[r][n].re;
            acc.im += sub[r][n].im;
        }
        acc
    };
    for (n, o) in out.iter_mut().enumerate() {
        if is34 {
            o[0] = sum(n, 0..12);
            o[1] = sum(n, 12..20);
            o[2] = sum(n, 20..24);
            o[3] = sum(n, 24..28);
            o[4] = sum(n, 28..32);
            for band in 5..QMF_BANDS {
                o[band] = sub[27 + band][n];
            }
        } else {
            o[0] = sum(n, 0..6);
            o[1] = sum(n, 6..8);
            o[2] = sum(n, 8..10);
            for band in 3..QMF_BANDS {
                o[band] = sub[7 + band][n];
            }
        }
    }
    out
}

/// Shifts a decorrelator delay line by one frame and appends the new slots.
fn shift_delay(delay: &mut [Cx; SLOTS + MAX_DELAY], s: &[Cx; SLOTS]) {
    delay.copy_within(SLOTS..SLOTS + MAX_DELAY, 0);
    delay[MAX_DELAY..].copy_from_slice(s);
}

/// One all-pass decorrelator band (`ps_decorrelate_c`).
///
/// `delay[n]` is the input two slots back for the all-pass bands (the caller
/// passes the delay line from offset `MAX_DELAY - 2`).
fn allpass_decorrelate(
    out: &mut [Cx; SLOTS],
    delay: &[Cx],
    ap: &mut [[Cx; SLOTS + MAX_AP_DELAY]; AP_LINKS],
    phi: [f64; 2],
    q: &[[f64; 2]; AP_LINKS],
    tg: &[f64; SLOTS],
    g: f64,
) {
    let ag = [AP_A[0] * g, AP_A[1] * g, AP_A[2] * g];
    for n in 0..SLOTS {
        let d = delay[n];
        let mut in_re = d.re * phi[0] - d.im * phi[1];
        let mut in_im = d.re * phi[1] + d.im * phi[0];
        for m in 0..AP_LINKS {
            let a_re = ag[m] * in_re;
            let a_im = ag[m] * in_im;
            let link = ap[m][n + 2 - m];
            let apd_re = in_re;
            let apd_im = in_im;
            in_re = link.re * q[m][0] - link.im * q[m][1] - a_re;
            in_im = link.re * q[m][1] + link.im * q[m][0] - a_im;
            ap[m][n + MAX_AP_DELAY] = Cx {
                re: apd_re + ag[m] * in_re,
                im: apd_im + ag[m] * in_im,
            };
        }
        out[n] = Cx { re: tg[n] * in_re, im: tg[n] * in_im };
    }
}

/// The per-stream PS reconstruction state (FFmpeg's `PSContext`, minus the
/// parsed parameters, which live in [`PsCommon`]).
pub struct PsEngine {
    /// Hybrid input per low band: 6 history slots, then the frame and lookahead.
    in_buf: [[Cx; IN_BUF_LEN]; LOW_BANDS],
    /// Decorrelator delay lines per subband (14 history + 32 new slots).
    delay: Vec<[Cx; SLOTS + MAX_DELAY]>,
    /// All-pass delay lines per band and link (5 history + 32 new slots).
    ap_delay: Vec<[[Cx; SLOTS + MAX_AP_DELAY]; AP_LINKS]>,
    peak_decay_nrg: [f64; MAX_PAR],
    power_smooth: [f64; MAX_PAR],
    peak_decay_diff_smooth: [f64; MAX_PAR],
    /// Mixing matrices `[mat][ipd][envelope][band]`, mat = H11, H12, H21, H22.
    h: [[[[f64; MAX_PAR]; MAX_ENV + 1]; 2]; 4],
    opd_hist: [usize; MAX_PAR],
    ipd_hist: [usize; MAX_PAR],
}

impl Default for PsEngine {
    fn default() -> PsEngine {
        PsEngine::new()
    }
}

impl PsEngine {
    /// A fresh engine with all delay lines and mixing matrices cleared.
    pub fn new() -> PsEngine {
        PsEngine {
            in_buf: [[Cx::default(); IN_BUF_LEN]; LOW_BANDS],
            delay: vec![[Cx::default(); SLOTS + MAX_DELAY]; MAX_SUB],
            ap_delay: vec![[[Cx::default(); SLOTS + MAX_AP_DELAY]; AP_LINKS]; MAX_ALLPASS],
            peak_decay_nrg: [0.0; MAX_PAR],
            power_smooth: [0.0; MAX_PAR],
            peak_decay_diff_smooth: [0.0; MAX_PAR],
            h: [[[[0.0; MAX_PAR]; MAX_ENV + 1]; 2]; 4],
            opd_hist: [0; MAX_PAR],
            ipd_hist: [0; MAX_PAR],
        }
    }

    /// Runs the stereo reconstruction over one SBR frame (`ff_ps_apply`).
    ///
    /// `input` is the SBR's 38-slot QMF buffer for the mono channel: the 32
    /// frame slots plus the 6 lookahead slots the hybrid filter reads ahead
    /// into. `top` is the first QMF band above the SBR range. Returns the left
    /// and right QMF slots for the frame's 32 slots.
    pub fn apply(
        &mut self,
        ps: &PsCommon,
        input: &[[Complex<f32>; QMF_BANDS]; IN_SLOTS],
        top: usize,
    ) -> (QmfPlane, QmfPlane) {
        let is34 = ps.is34bands;
        let m = usize::from(is34);
        let nr_bands = NR_BANDS[m];
        // Clear the delay state above the SBR range (`top += NR_BANDS - 64`).
        let top = (top as isize + nr_bands as isize - QMF_BANDS as isize).clamp(0, nr_bands as isize) as usize;
        for d in &mut self.delay[top..nr_bands] {
            *d = [Cx::default(); SLOTS + MAX_DELAY];
        }
        let n_allpass = NR_ALLPASS_BANDS[m];
        if top < n_allpass {
            for a in &mut self.ap_delay[top..n_allpass] {
                *a = [[Cx::default(); SLOTS + MAX_AP_DELAY]; AP_LINKS];
            }
        }

        let mut s = [[Cx::default(); SLOTS]; MAX_SUB];
        let mut d = [[Cx::default(); SLOTS]; MAX_SUB];
        self.hybrid_analysis(input, is34, &mut s);
        self.decorrelation(ps, &s, &mut d);
        self.stereo_processing(ps, &mut s, &mut d);
        let left = hybrid_synthesis(&s, is34);
        let right = hybrid_synthesis(&d, is34);
        (narrow(&left), narrow(&right))
    }

    /// Splits the low QMF bands into hybrid subbands (`hybrid_analysis`).
    fn hybrid_analysis(
        &mut self,
        input: &[[Complex<f32>; QMF_BANDS]; IN_SLOTS],
        is34: bool,
        out: &mut [[Cx; SLOTS]; MAX_SUB],
    ) {
        let t = tables();
        for (band, buf) in self.in_buf.iter_mut().enumerate() {
            for (j, slot) in input.iter().enumerate() {
                buf[j + HIST] = lift(slot[band]);
            }
        }
        {
            let buf = &self.in_buf;
            if is34 {
                analysis_bank(&buf[0], &t.f34_0_12, &mut out[0..12]);
                analysis_bank(&buf[1], &t.f34_1_8, &mut out[12..20]);
                analysis_bank(&buf[2], &t.f34_2_4, &mut out[20..24]);
                analysis_bank(&buf[3], &t.f34_2_4, &mut out[24..28]);
                analysis_bank(&buf[4], &t.f34_2_4, &mut out[28..32]);
                interleave(input, 27, 5, out);
            } else {
                let mut temp = [[Cx::default(); SLOTS]; 8];
                analysis_bank(&buf[0], &t.f20_0_8, &mut temp);
                for i in 0..SLOTS {
                    out[0][i] = temp[6][i];
                    out[1][i] = temp[7][i];
                    out[2][i] = temp[0][i];
                    out[3][i] = temp[1][i];
                    out[4][i] = Cx {
                        re: temp[2][i].re + temp[5][i].re,
                        im: temp[2][i].im + temp[5][i].im,
                    };
                    out[5][i] = Cx {
                        re: temp[3][i].re + temp[4][i].re,
                        im: temp[3][i].im + temp[4][i].im,
                    };
                }
                hybrid2(&buf[1], 6, true, out);
                hybrid2(&buf[2], 8, false, out);
                interleave(input, 7, 3, out);
            }
        }
        // Keep the last 6 slots of this frame as the next frame's history.
        for band in 0..LOW_BANDS {
            self.in_buf[band].copy_within(SLOTS..SLOTS + HIST, 0);
        }
    }

    /// The all-pass decorrelator and the transient ducker (`decorrelation`).
    fn decorrelation(&mut self, ps: &PsCommon, s: &[[Cx; SLOTS]; MAX_SUB], out: &mut [[Cx; SLOTS]; MAX_SUB]) {
        let t = tables();
        let is34 = ps.is34bands;
        let m = usize::from(is34);
        let k_to_i: &[u8] = if is34 { &K_TO_I_34 } else { &K_TO_I_20 };

        if is34 != ps.is34bands_old {
            self.peak_decay_nrg = [0.0; MAX_PAR];
            self.power_smooth = [0.0; MAX_PAR];
            self.peak_decay_diff_smooth = [0.0; MAX_PAR];
            for d in self.delay.iter_mut() {
                *d = [Cx::default(); SLOTS + MAX_DELAY];
            }
            for a in self.ap_delay.iter_mut() {
                *a = [[Cx::default(); SLOTS + MAX_AP_DELAY]; AP_LINKS];
            }
        }

        // Band power per parameter band, over the subbands that map to it.
        let mut power = [[0.0f64; SLOTS]; MAX_PAR];
        for k in 0..NR_BANDS[m] {
            let i = usize::from(k_to_i[k]);
            for n in 0..SLOTS {
                power[i][n] += s[k][n].re * s[k][n].re + s[k][n].im * s[k][n].im;
            }
        }

        // Transient detector: a per-band gain that ducks the decorrelated part.
        let mut transient = [[0.0f64; SLOTS]; MAX_PAR];
        for i in 0..NR_PAR_BANDS[m] {
            for n in 0..SLOTS {
                let decayed = PEAK_DECAY_FACTOR * self.peak_decay_nrg[i];
                self.peak_decay_nrg[i] = decayed.max(power[i][n]);
                self.power_smooth[i] += A_SMOOTH * (power[i][n] - self.power_smooth[i]);
                self.peak_decay_diff_smooth[i] +=
                    A_SMOOTH * (self.peak_decay_nrg[i] - power[i][n] - self.peak_decay_diff_smooth[i]);
                let denom = TRANSIENT_IMPACT * self.peak_decay_diff_smooth[i];
                transient[i][n] = if denom > self.power_smooth[i] {
                    self.power_smooth[i] / denom
                } else {
                    1.0
                };
            }
        }

        for k in 0..NR_ALLPASS_BANDS[m] {
            let b = usize::from(k_to_i[k]);
            let g = (1.0 - DECAY_SLOPE * f64::from(k as i32 - DECAY_CUTOFF[m])).clamp(0.0, 1.0);
            shift_delay(&mut self.delay[k], &s[k]);
            for link in self.ap_delay[k].iter_mut() {
                link.copy_within(SLOTS..SLOTS + MAX_AP_DELAY, 0);
            }
            allpass_decorrelate(
                &mut out[k],
                &self.delay[k][MAX_DELAY - 2..],
                &mut self.ap_delay[k],
                t.phi_fract[m][k],
                &t.q_fract[m][k],
                &transient[b],
                g,
            );
        }
        for k in NR_ALLPASS_BANDS[m]..SHORT_DELAY_BAND[m] {
            let b = usize::from(k_to_i[k]);
            shift_delay(&mut self.delay[k], &s[k]);
            for n in 0..SLOTS {
                let dv = self.delay[k][n];
                out[k][n] = Cx { re: dv.re * transient[b][n], im: dv.im * transient[b][n] };
            }
        }
        for k in SHORT_DELAY_BAND[m]..NR_BANDS[m] {
            let b = usize::from(k_to_i[k]);
            shift_delay(&mut self.delay[k], &s[k]);
            for n in 0..SLOTS {
                let dv = self.delay[k][MAX_DELAY - 1 + n];
                out[k][n] = Cx { re: dv.re * transient[b][n], im: dv.im * transient[b][n] };
            }
        }
    }

    /// Mixing matrices per envelope, then the mix of `(s, d)` into the stereo
    /// pair in place (`stereo_processing`). On return `l` holds the left and
    /// `r` the right subband signal.
    fn stereo_processing(
        &mut self,
        ps: &PsCommon,
        l: &mut [[Cx; SLOTS]; MAX_SUB],
        r: &mut [[Cx; SLOTS]; MAX_SUB],
    ) {
        let t = tables();
        let is34 = ps.is34bands;
        let m = usize::from(is34);
        let k_to_i: &[u8] = if is34 { &K_TO_I_34 } else { &K_TO_I_20 };
        let lut = if ps.icc_mode < 3 { &t.ha } else { &t.hb };

        // The previous frame's last envelope becomes this frame's envelope 0.
        if ps.num_env_old != 0 {
            for mat in 0..4 {
                for ipd in 0..2 {
                    self.h[mat][ipd][0] = self.h[mat][ipd][ps.num_env_old];
                }
            }
        }

        let num_env = ps.num_env;
        let (iid, icc, ipd, opd) = if is34 {
            if !ps.is34bands_old {
                for mat in 0..4 {
                    for ipd_row in 0..2 {
                        map_val_20_to_34(&mut self.h[mat][ipd_row][0]);
                    }
                }
                self.reset_ipdopd_hist();
            }
            (
                remap34(&ps.iid_par, ps.nr_iid_par, num_env, true),
                remap34(&ps.icc_par, ps.nr_icc_par, num_env, true),
                remap34(&ps.ipd_par, ps.nr_ipdopd_par, num_env, false),
                remap34(&ps.opd_par, ps.nr_ipdopd_par, num_env, false),
            )
        } else {
            if ps.is34bands_old {
                for mat in 0..4 {
                    for ipd_row in 0..2 {
                        map_val_34_to_20(&mut self.h[mat][ipd_row][0]);
                    }
                }
                self.reset_ipdopd_hist();
            }
            (
                remap20(&ps.iid_par, ps.nr_iid_par, num_env, true),
                remap20(&ps.icc_par, ps.nr_icc_par, num_env, true),
                remap20(&ps.ipd_par, ps.nr_ipdopd_par, num_env, false),
                remap20(&ps.opd_par, ps.nr_ipdopd_par, num_env, false),
            )
        };

        let quant = i32::from(ps.iid_quant) * 23;
        // Mixing matrices for each envelope and parameter band.
        for e in 0..num_env {
            for b in 0..NR_PAR_BANDS[m] {
                let idx = (i32::from(iid[e][b]) + 7 + quant).clamp(0, 45) as usize;
                let icc_idx = usize::from(icc[e][b].clamp(0, 7) as u8);
                let row = lut[idx][icc_idx];
                let mut mix = row;
                if ps.enable_ipdopd && b < NR_IPDOPD_BANDS[m] {
                    let opd_idx = (self.opd_hist[b] * 8 + usize::from(opd[e][b].clamp(0, 7) as u8)).min(511);
                    let ipd_idx = (self.ipd_hist[b] * 8 + usize::from(ipd[e][b].clamp(0, 7) as u8)).min(511);
                    let (opd_re, opd_im) = (t.pd_re[opd_idx], t.pd_im[opd_idx]);
                    let (ipd_re, ipd_im) = (t.pd_re[ipd_idx], t.pd_im[ipd_idx]);
                    self.opd_hist[b] = opd_idx & 0x3F;
                    self.ipd_hist[b] = ipd_idx & 0x3F;
                    let ipd_adj_re = opd_re * ipd_re + opd_im * ipd_im;
                    let ipd_adj_im = opd_im * ipd_re - opd_re * ipd_im;
                    let [h11, h12, h21, h22] = mix;
                    self.h[0][1][e + 1][b] = h11 * opd_im;
                    self.h[1][1][e + 1][b] = h12 * ipd_adj_im;
                    self.h[2][1][e + 1][b] = h21 * opd_im;
                    self.h[3][1][e + 1][b] = h22 * ipd_adj_im;
                    mix = [h11 * opd_re, h12 * ipd_adj_re, h21 * opd_re, h22 * ipd_adj_re];
                }
                for (h_mat, v) in self.h.iter_mut().zip(mix) {
                    h_mat[0][e + 1][b] = v;
                }
            }

            // Interpolate the matrices across this envelope's slots.
            let start = ps.border_position[e];
            let stop = ps.border_position[e + 1];
            let count = (stop - start).max(0) as usize;
            let width = 1.0 / if count == 0 { 1.0 } else { count as f64 };
            let first = (start + 1).max(0) as usize;
            for k in 0..NR_BANDS[m] {
                if count == 0 {
                    break;
                }
                let b = usize::from(k_to_i[k]);
                let mut h0 = [0.0f64; 4];
                let mut hs0 = [0.0f64; 4];
                let mut h1 = [0.0f64; 4];
                let mut hs1 = [0.0f64; 4];
                let flip = (is34 && (9..=13).contains(&k)) || (!is34 && k <= 1);
                for mat in 0..4 {
                    h0[mat] = self.h[mat][0][e][b];
                    hs0[mat] = (self.h[mat][0][e + 1][b] - h0[mat]) * width;
                    if ps.enable_ipdopd {
                        let sign = if flip { -1.0 } else { 1.0 };
                        h1[mat] = sign * self.h[mat][1][e][b];
                        hs1[mat] = (self.h[mat][1][e + 1][b] - h1[mat]) * width;
                    }
                }
                for n in 0..count {
                    let slot = first + n;
                    if slot >= SLOTS {
                        break;
                    }
                    for mat in 0..4 {
                        h0[mat] += hs0[mat];
                        h1[mat] += hs1[mat];
                    }
                    let lv = l[k][slot];
                    let rv = r[k][slot];
                    if ps.enable_ipdopd {
                        l[k][slot] = Cx {
                            re: h0[0] * lv.re + h0[2] * rv.re - h1[0] * lv.im - h1[2] * rv.im,
                            im: h0[0] * lv.im + h0[2] * rv.im + h1[0] * lv.re + h1[2] * rv.re,
                        };
                        r[k][slot] = Cx {
                            re: h0[1] * lv.re + h0[3] * rv.re - h1[1] * lv.im - h1[3] * rv.im,
                            im: h0[1] * lv.im + h0[3] * rv.im + h1[1] * lv.re + h1[3] * rv.re,
                        };
                    } else {
                        l[k][slot] = Cx {
                            re: h0[0] * lv.re + h0[2] * rv.re,
                            im: h0[0] * lv.im + h0[2] * rv.im,
                        };
                        r[k][slot] = Cx {
                            re: h0[1] * lv.re + h0[3] * rv.re,
                            im: h0[1] * lv.im + h0[3] * rv.im,
                        };
                    }
                }
            }
        }
    }

    fn reset_ipdopd_hist(&mut self) {
        self.opd_hist = [0; MAX_PAR];
        self.ipd_hist = [0; MAX_PAR];
    }
}
