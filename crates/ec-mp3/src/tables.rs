//! The constant tables Layer III needs besides the Huffman codes: scalefactor
//! band layouts, the pre-emphasis table, and the two lazily built power tables
//! requantisation runs on.
//!
//! The three MPEG-1 long-block layouts here were measured against a reference
//! decoder the same way the Huffman tables were (`scripts/mp3-tables/`, one
//! band attenuated at a time), which is also why they are stated as widths: a
//! width table cannot silently disagree with itself about where 576 ends.

use std::sync::OnceLock;

/// Widths of the 22 long scalefactor bands, by sample rate.
pub(crate) fn long_widths(sample_rate: u32) -> &'static [u16; 22] {
    match sample_rate {
        44100 => &[
            4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 10, 12, 16, 20, 24, 28, 34, 42, 50, 54, 76, 158,
        ],
        48000 => &[
            4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 10, 12, 16, 18, 22, 28, 34, 40, 46, 54, 54, 192,
        ],
        32000 => &[
            4, 4, 4, 4, 4, 4, 6, 6, 8, 10, 12, 16, 20, 24, 30, 38, 46, 56, 68, 84, 102, 26,
        ],
        22050 | 16000 | 11025 | 12000 => &[
            6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
        ],
        24000 => &[
            6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 18, 22, 26, 32, 38, 46, 54, 62, 70, 76, 36,
        ],
        // 8 kHz is the odd one out: five two-wide bands at the top rather than
        // one wide one.
        _ => &[
            12, 12, 12, 12, 12, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 76, 90, 2, 2, 2, 2, 2,
        ],
    }
}

/// Widths of the 13 short scalefactor bands (per window), by sample rate.
pub(crate) fn short_widths(sample_rate: u32) -> &'static [u16; 13] {
    match sample_rate {
        44100 => &[4, 4, 4, 4, 6, 8, 10, 12, 14, 18, 22, 30, 56],
        48000 => &[4, 4, 4, 4, 6, 6, 10, 12, 14, 16, 20, 26, 66],
        32000 => &[4, 4, 4, 4, 6, 8, 12, 16, 20, 26, 34, 42, 12],
        22050 => &[4, 4, 4, 6, 6, 8, 10, 14, 18, 26, 32, 42, 18],
        24000 => &[4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 32, 44, 12],
        // Measured: the MPEG-2.5 rates 11.025 and 12 kHz share the 16 kHz
        // layout rather than the LSF rate they halve.
        16000 | 11025 | 12000 => &[4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
        _ => &[8, 8, 8, 12, 16, 20, 24, 28, 36, 2, 2, 2, 26],
    }
}

/// Start offset of each long band, plus 576 as the last entry.
pub(crate) fn long_starts(sample_rate: u32) -> [u16; 23] {
    let mut out = [0u16; 23];
    let mut acc = 0;
    for (i, w) in long_widths(sample_rate).iter().enumerate() {
        out[i] = acc;
        acc += w;
    }
    out[22] = acc;
    out
}

/// Start offset of each short band within one window, plus 192.
pub(crate) fn short_starts(sample_rate: u32) -> [u16; 14] {
    let mut out = [0u16; 14];
    let mut acc = 0;
    for (i, w) in short_widths(sample_rate).iter().enumerate() {
        out[i] = acc;
        acc += w;
    }
    out[13] = acc;
    out
}

/// Additional high-frequency scalefactor applied when `preflag` is set.
pub(crate) const PRETAB: [u8; 22] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0,
];

/// `slen1`/`slen2` for the MPEG-1 `scalefac_compress` index.
pub(crate) const SLEN: [(u32, u32); 16] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (3, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 1),
    (3, 2),
    (3, 3),
    (4, 2),
    (4, 3),
];

/// Scalefactor band counts per partition for the MPEG-2 LSF scalefactor
/// scheme, indexed `[block number][0 = long, 1 = short, 2 = mixed]`.
pub(crate) const LSF_PARTITIONS: [[[u8; 4]; 3]; 6] = [
    [[6, 5, 5, 5], [9, 9, 9, 9], [6, 9, 9, 9]],
    [[6, 5, 7, 3], [9, 9, 12, 6], [6, 9, 12, 6]],
    [[11, 10, 0, 0], [18, 18, 0, 0], [15, 18, 0, 0]],
    [[7, 7, 7, 0], [12, 12, 12, 0], [6, 15, 12, 0]],
    [[6, 6, 6, 3], [12, 9, 9, 6], [6, 12, 9, 6]],
    [[8, 8, 5, 0], [15, 12, 9, 0], [6, 18, 9, 0]],
];

/// The largest quantised magnitude Layer III can code (15 plus 13 linbits).
pub(crate) const MAX_QUANT: usize = 8206;

/// `|is|^(4/3)`, the requantisation curve, tabulated once.
pub(crate) fn power43() -> &'static [f32; MAX_QUANT + 1] {
    static TABLE: OnceLock<Box<[f32; MAX_QUANT + 1]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0.0f32; MAX_QUANT + 1]);
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = (i as f64).powf(4.0 / 3.0) as f32;
        }
        table
    })
}

/// The four Layer III block windows: normal, start, short, stop.
pub(crate) fn windows() -> &'static [[f32; 36]; 4] {
    static WINDOWS: OnceLock<[[f32; 36]; 4]> = OnceLock::new();
    WINDOWS.get_or_init(|| {
        let mut w = [[0.0f32; 36]; 4];
        let sin36 = |i: usize| (std::f64::consts::PI / 36.0 * (i as f64 + 0.5)).sin() as f32;
        let sin12 = |i: usize| (std::f64::consts::PI / 12.0 * (i as f64 + 0.5)).sin() as f32;
        for (i, slot) in w[0].iter_mut().enumerate() {
            *slot = sin36(i);
        }
        for (i, slot) in w[1].iter_mut().enumerate() {
            *slot = match i {
                0..=17 => sin36(i),
                18..=23 => 1.0,
                24..=29 => sin12(i - 18),
                _ => 0.0,
            };
        }
        for (i, slot) in w[2].iter_mut().enumerate().take(12) {
            *slot = sin12(i);
        }
        for (i, slot) in w[3].iter_mut().enumerate() {
            *slot = match i {
                0..=5 => 0.0,
                6..=11 => sin12(i - 6),
                12..=17 => 1.0,
                _ => sin36(i),
            };
        }
        w
    })
}

/// Alias-reduction butterfly coefficients, `(cs, ca)` for the eight lines each
/// side of a subband boundary.
pub(crate) fn alias_coefficients() -> &'static [(f32, f32); 8] {
    static COEFFS: OnceLock<[(f32, f32); 8]> = OnceLock::new();
    COEFFS.get_or_init(|| {
        let ci = [
            -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
        ];
        let mut out = [(0.0f32, 0.0f32); 8];
        for (slot, c) in out.iter_mut().zip(ci) {
            let norm = (1.0f64 + c * c).sqrt();
            *slot = ((1.0 / norm) as f32, (c / norm) as f32);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_band_layout_spans_its_granule() {
        for rate in [44100, 48000, 32000, 24000, 22050, 16000, 12000, 11025, 8000] {
            let long: u16 = long_widths(rate).iter().sum();
            let short: u16 = short_widths(rate).iter().sum();
            assert_eq!(long, 576, "long bands at {rate} Hz");
            assert_eq!(short * 3, 576, "short bands at {rate} Hz");
            assert_eq!(long_starts(rate)[22], 576);
            assert_eq!(short_starts(rate)[13], 192);
        }
    }

    #[test]
    fn lsf_partitions_cover_their_band_counts() {
        for (block, entry) in LSF_PARTITIONS.iter().enumerate() {
            let sums: Vec<u32> = entry
                .iter()
                .map(|p| p.iter().map(|v| u32::from(*v)).sum())
                .collect();
            assert_eq!(sums, vec![21, 36, 33], "block number {block}");
        }
    }
}
