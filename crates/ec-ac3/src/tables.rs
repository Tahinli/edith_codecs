//! Constant tables from ATSC A/52:2012 — the ones a decoder cannot derive.
//!
//! Everything here is a transcription of a numbered table in the standard, kept
//! in one place so a reader can check it against the document without walking
//! the decoder: §5.4.1.4 (frame sizes), §7.2.3 (bit allocation), §7.3
//! (quantizers), §7.9.4 (transform window).
//!
//! The symmetric quantizer levels of Tables 7.19-7.23 are *not* transcribed:
//! every one of them is the uniform midtread set `(2k - (L-1))/L`, so
//! [`symmetric_level`] computes it and a test asserts the printed values.

/// Sample rates by `fscod` (Table 5.6); `0` marks the reserved code.
pub const SAMPLE_RATE: [u32; 4] = [48_000, 44_100, 32_000, 0];

/// Frame size in 16-bit words, `[frmsizecod][fscod]` (Table 5.18).
pub const FRAME_SIZE_WORDS: [[u16; 3]; 38] = [
    [64, 69, 96],
    [64, 70, 96],
    [80, 87, 120],
    [80, 88, 120],
    [96, 104, 144],
    [96, 105, 144],
    [112, 121, 168],
    [112, 122, 168],
    [128, 139, 192],
    [128, 140, 192],
    [160, 174, 240],
    [160, 175, 240],
    [192, 208, 288],
    [192, 209, 288],
    [224, 243, 336],
    [224, 244, 336],
    [256, 278, 384],
    [256, 279, 384],
    [320, 348, 480],
    [320, 349, 480],
    [384, 417, 576],
    [384, 418, 576],
    [448, 487, 672],
    [448, 488, 672],
    [512, 557, 768],
    [512, 558, 768],
    [640, 696, 960],
    [640, 697, 960],
    [768, 835, 1152],
    [768, 836, 1152],
    [896, 975, 1344],
    [896, 976, 1344],
    [1024, 1114, 1536],
    [1024, 1115, 1536],
    [1152, 1253, 1728],
    [1152, 1254, 1728],
    [1280, 1393, 1920],
    [1280, 1394, 1920],
];

/// Nominal bit rate in kbit/s by `frmsizecod >> 1` (Table 5.18).
pub const BIT_RATE_KBPS: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// Number of full-bandwidth channels by `acmod` (Table 5.8).
pub const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// First mantissa bin of each 1/6-octave band, `bndtab[]` (Table 7.12).
pub const BNDTAB: [usize; 51] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 31, 34, 37, 40, 43, 46, 49, 55, 61, 67, 73, 79, 85, 97, 109, 121, 133, 157, 181,
    205, 229, 253,
];

/// Bin number to band number, `masktab[]` (Table 7.13), derived from
/// [`BNDTAB`] — the standard prints both and says they duplicate each other.
pub const MASKTAB: [usize; 256] = {
    let mut t = [49usize; 256];
    let mut band = 0;
    while band < 50 {
        let mut bin = BNDTAB[band];
        while bin < BNDTAB[band + 1] {
            t[bin] = band;
            bin += 1;
        }
        band += 1;
    }
    t
};

/// Slow decay, `slowdec[]` (Table 7.6).
pub const SLOWDEC: [i32; 4] = [0x0f, 0x11, 0x13, 0x15];
/// Fast decay, `fastdec[]` (Table 7.7).
pub const FASTDEC: [i32; 4] = [0x3f, 0x53, 0x67, 0x7b];
/// Slow gain, `slowgain[]` (Table 7.8).
pub const SLOWGAIN: [i32; 4] = [0x540, 0x4d8, 0x478, 0x410];
/// dB per bit, `dbpbtab[]` (Table 7.9).
pub const DBPBTAB: [i32; 4] = [0x000, 0x700, 0x900, 0xb00];
/// Masking floor, `floortab[]` (Table 7.10). Entry 7 is `0xf800`, which is
/// -0x800 as the 16-bit signed value the masking arithmetic wants.
pub const FLOORTAB: [i32; 8] = [0x2f0, 0x2b0, 0x270, 0x230, 0x1f0, 0x170, 0x0f0, -0x800];
/// Fast gain, `fastgain[]` (Table 7.11).
pub const FASTGAIN: [i32; 8] = [0x080, 0x100, 0x180, 0x200, 0x280, 0x300, 0x380, 0x400];

/// Log-addition table, `latab[]` (Table 7.14).
pub const LATAB: [i32; 256] = [
    64, 63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 52, 51, 50, 49, 48, 47, 47, 46, 45, 44, 44,
    43, 42, 41, 41, 40, 39, 38, 38, 37, 36, 36, 35, 35, 34, 33, 33, 32, 32, 31, 30, 30, 29, 29, 28,
    28, 27, 27, 26, 26, 25, 25, 24, 24, 23, 23, 22, 22, 21, 21, 21, 20, 20, 19, 19, 19, 18, 18, 18,
    17, 17, 17, 16, 16, 16, 15, 15, 15, 14, 14, 14, 13, 13, 13, 13, 12, 12, 12, 12, 11, 11, 11, 11,
    10, 10, 10, 10, 10, 9, 9, 9, 9, 9, 8, 8, 8, 8, 8, 8, 7, 7, 7, 7, 7, 7, 6, 6, 6, 6, 6, 6, 6, 6,
    5, 5, 5, 5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0,
];

/// Hearing threshold, `hth[fscod][band]` (Table 7.15).
pub const HTH: [[i32; 50]; 3] = [
    [
        0x04d0, 0x04d0, 0x0440, 0x0400, 0x03e0, 0x03c0, 0x03b0, 0x03b0, 0x03a0, 0x03a0, 0x03a0,
        0x03a0, 0x03a0, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0370, 0x0370, 0x0360, 0x0360,
        0x0350, 0x0350, 0x0340, 0x0340, 0x0330, 0x0320, 0x0310, 0x0300, 0x02f0, 0x02f0, 0x02f0,
        0x02f0, 0x0300, 0x0310, 0x0340, 0x0390, 0x03e0, 0x0420, 0x0460, 0x0490, 0x04a0, 0x0460,
        0x0440, 0x0440, 0x0520, 0x0800, 0x0840, 0x0840,
    ],
    [
        0x04f0, 0x04f0, 0x0460, 0x0410, 0x03e0, 0x03d0, 0x03c0, 0x03b0, 0x03b0, 0x03a0, 0x03a0,
        0x03a0, 0x03a0, 0x03a0, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0380, 0x0370, 0x0370,
        0x0360, 0x0360, 0x0350, 0x0350, 0x0340, 0x0340, 0x0320, 0x0310, 0x0300, 0x02f0, 0x02f0,
        0x02f0, 0x02f0, 0x0300, 0x0320, 0x0350, 0x0390, 0x03e0, 0x0420, 0x0450, 0x04a0, 0x0490,
        0x0460, 0x0440, 0x0480, 0x0630, 0x0840, 0x0840,
    ],
    [
        0x0580, 0x0580, 0x04b0, 0x0450, 0x0420, 0x03f0, 0x03e0, 0x03d0, 0x03c0, 0x03b0, 0x03b0,
        0x03b0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x0390, 0x0390,
        0x0390, 0x0390, 0x0380, 0x0380, 0x0380, 0x0370, 0x0360, 0x0350, 0x0340, 0x0330, 0x0320,
        0x0310, 0x0300, 0x02f0, 0x02f0, 0x02f0, 0x0300, 0x0310, 0x0330, 0x0350, 0x03c0, 0x0410,
        0x0470, 0x04a0, 0x0460, 0x0440, 0x0450, 0x04e0,
    ],
];

/// Bit allocation pointer table, `baptab[]` (Table 7.16).
pub const BAPTAB: [u8; 64] = [
    0, 1, 1, 1, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9, 9, 9, 9,
    10, 10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 14, 14, 14, 14,
    15, 15, 15, 15, 15, 15, 15, 15, 15,
];

/// Mantissa bits per bap, `qntztab[]` (Table 7.18). Entries 1, 2 and 4 are the
/// *group* widths (5, 7 and 7 bits) — those baps are ungrouped by
/// [`crate::mantissa`], not read one mantissa at a time.
pub const QNTZTAB: [u32; 16] = [0, 5, 7, 3, 7, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// Quantizer levels for the symmetric baps 1-5 (Table 7.17).
pub const QUANT_LEVELS: [u32; 6] = [0, 3, 5, 7, 11, 15];

/// The `k`-th reconstruction level of a `levels`-level symmetric quantizer
/// (Tables 7.19-7.23): the uniform midtread set `(2k - (levels-1)) / levels`.
pub fn symmetric_level(levels: u32, k: u32) -> f32 {
    (2.0 * k as f32 - (levels as f32 - 1.0)) / levels as f32
}

/// Transform window `w[]`: 256 values used back to back to form the 512-point
/// window (Table 7.33).
// One printed value happens to sit within rounding distance of pi/4; it is a
// window sample, not that constant.
#[allow(clippy::approx_constant)]
pub const WINDOW: [f32; 256] = [
    0.00014, 0.00024, 0.00037, 0.00051, 0.00067, 0.00086, 0.00107, 0.00130, 0.00157, 0.00187,
    0.00220, 0.00256, 0.00297, 0.00341, 0.00390, 0.00443, 0.00501, 0.00564, 0.00632, 0.00706,
    0.00785, 0.00871, 0.00962, 0.01061, 0.01166, 0.01279, 0.01399, 0.01526, 0.01662, 0.01806,
    0.01959, 0.02121, 0.02292, 0.02472, 0.02662, 0.02863, 0.03073, 0.03294, 0.03527, 0.03770,
    0.04025, 0.04292, 0.04571, 0.04862, 0.05165, 0.05481, 0.05810, 0.06153, 0.06508, 0.06878,
    0.07261, 0.07658, 0.08069, 0.08495, 0.08935, 0.09389, 0.09859, 0.10343, 0.10842, 0.11356,
    0.11885, 0.12429, 0.12988, 0.13563, 0.14152, 0.14757, 0.15376, 0.16011, 0.16661, 0.17325,
    0.18005, 0.18699, 0.19407, 0.20130, 0.20867, 0.21618, 0.22382, 0.23161, 0.23952, 0.24757,
    0.25574, 0.26404, 0.27246, 0.28100, 0.28965, 0.29841, 0.30729, 0.31626, 0.32533, 0.33450,
    0.34376, 0.35311, 0.36253, 0.37204, 0.38161, 0.39126, 0.40096, 0.41072, 0.42054, 0.43040,
    0.44030, 0.45023, 0.46020, 0.47019, 0.48020, 0.49022, 0.50025, 0.51028, 0.52031, 0.53033,
    0.54033, 0.55031, 0.56026, 0.57019, 0.58007, 0.58991, 0.59970, 0.60944, 0.61912, 0.62873,
    0.63827, 0.64774, 0.65713, 0.66643, 0.67564, 0.68476, 0.69377, 0.70269, 0.71150, 0.72019,
    0.72877, 0.73723, 0.74557, 0.75378, 0.76186, 0.76981, 0.77762, 0.78530, 0.79283, 0.80022,
    0.80747, 0.81457, 0.82151, 0.82831, 0.83496, 0.84145, 0.84779, 0.85398, 0.86001, 0.86588,
    0.87160, 0.87716, 0.88257, 0.88782, 0.89291, 0.89785, 0.90264, 0.90728, 0.91176, 0.91610,
    0.92028, 0.92432, 0.92822, 0.93197, 0.93558, 0.93906, 0.94240, 0.94560, 0.94867, 0.95162,
    0.95444, 0.95713, 0.95971, 0.96217, 0.96451, 0.96674, 0.96887, 0.97089, 0.97281, 0.97463,
    0.97635, 0.97799, 0.97953, 0.98099, 0.98236, 0.98366, 0.98488, 0.98602, 0.98710, 0.98811,
    0.98905, 0.98994, 0.99076, 0.99153, 0.99225, 0.99291, 0.99353, 0.99411, 0.99464, 0.99513,
    0.99558, 0.99600, 0.99639, 0.99674, 0.99706, 0.99736, 0.99763, 0.99788, 0.99811, 0.99831,
    0.99850, 0.99867, 0.99882, 0.99895, 0.99908, 0.99919, 0.99929, 0.99938, 0.99946, 0.99953,
    0.99959, 0.99965, 0.99969, 0.99974, 0.99978, 0.99981, 0.99984, 0.99986, 0.99988, 0.99990,
    0.99992, 0.99993, 0.99994, 0.99995, 0.99996, 0.99997, 0.99998, 0.99998, 0.99998, 0.99999,
    0.99999, 0.99999, 0.99999, 1.00000, 1.00000, 1.00000, 1.00000, 1.00000, 1.00000, 1.00000,
    1.00000, 1.00000, 1.00000, 1.00000, 1.00000, 1.00000,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masktab_matches_band_boundaries() {
        // Table 7.13 spot values, then the invariant that every bin lands in
        // the band whose [start, next start) range contains it.
        assert_eq!(MASKTAB[0], 0);
        assert_eq!(MASKTAB[28], 28);
        assert_eq!(MASKTAB[30], 28);
        assert_eq!(MASKTAB[31], 29);
        assert_eq!(MASKTAB[252], 49);
        for band in 0..50 {
            for bin in BNDTAB[band]..BNDTAB[band + 1] {
                assert_eq!(MASKTAB[bin], band, "bin {bin}");
            }
        }
    }

    #[test]
    fn symmetric_quantizer_matches_printed_tables() {
        // Tables 7.19-7.23, as printed.
        assert!((symmetric_level(3, 0) + 2.0 / 3.0).abs() < 1e-6);
        assert!(symmetric_level(3, 1).abs() < 1e-6);
        assert!((symmetric_level(5, 3) - 2.0 / 5.0).abs() < 1e-6);
        assert!((symmetric_level(7, 6) - 6.0 / 7.0).abs() < 1e-6);
        assert!((symmetric_level(11, 0) + 10.0 / 11.0).abs() < 1e-6);
        assert!((symmetric_level(15, 14) - 14.0 / 15.0).abs() < 1e-6);
    }

    #[test]
    fn window_is_princen_bradley() {
        // §7.9.4: the 512-point sequence formed from w[] satisfies
        // w[n]^2 + w[511-n]^2 = 1. The printed table carries 5 decimals, which
        // is what bounds the error here.
        for n in 0..256 {
            let (a, b) = (WINDOW[n], WINDOW[255 - n]);
            assert!((a * a + b * b - 1.0).abs() < 2e-5, "n = {n}");
        }
    }
}
