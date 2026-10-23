//! VLC and constant tables (ISO/IEC 13818-2 Annex B, clause 7).

/// Table B.14 / B.15 rows: (run, level, code, length without the sign bit).
pub(crate) const B14: [(u8, u8, u16, u8); 111] = [
    (0, 1, 3, 2),
    (0, 2, 4, 4),
    (0, 3, 5, 5),
    (0, 4, 6, 7),
    (0, 5, 38, 8),
    (0, 6, 33, 8),
    (0, 7, 10, 10),
    (0, 8, 29, 12),
    (0, 9, 24, 12),
    (0, 10, 19, 12),
    (0, 11, 16, 12),
    (0, 12, 26, 13),
    (0, 13, 25, 13),
    (0, 14, 24, 13),
    (0, 15, 23, 13),
    (0, 16, 31, 14),
    (0, 17, 30, 14),
    (0, 18, 29, 14),
    (0, 19, 28, 14),
    (0, 20, 27, 14),
    (0, 21, 26, 14),
    (0, 22, 25, 14),
    (0, 23, 24, 14),
    (0, 24, 23, 14),
    (0, 25, 22, 14),
    (0, 26, 21, 14),
    (0, 27, 20, 14),
    (0, 28, 19, 14),
    (0, 29, 18, 14),
    (0, 30, 17, 14),
    (0, 31, 16, 14),
    (0, 32, 24, 15),
    (0, 33, 23, 15),
    (0, 34, 22, 15),
    (0, 35, 21, 15),
    (0, 36, 20, 15),
    (0, 37, 19, 15),
    (0, 38, 18, 15),
    (0, 39, 17, 15),
    (0, 40, 16, 15),
    (1, 1, 3, 3),
    (1, 2, 6, 6),
    (1, 3, 37, 8),
    (1, 4, 12, 10),
    (1, 5, 27, 12),
    (1, 6, 22, 13),
    (1, 7, 21, 13),
    (1, 8, 31, 15),
    (1, 9, 30, 15),
    (1, 10, 29, 15),
    (1, 11, 28, 15),
    (1, 12, 27, 15),
    (1, 13, 26, 15),
    (1, 14, 25, 15),
    (1, 15, 19, 16),
    (1, 16, 18, 16),
    (1, 17, 17, 16),
    (1, 18, 16, 16),
    (2, 1, 5, 4),
    (2, 2, 4, 7),
    (2, 3, 11, 10),
    (2, 4, 20, 12),
    (2, 5, 20, 13),
    (3, 1, 7, 5),
    (3, 2, 36, 8),
    (3, 3, 28, 12),
    (3, 4, 19, 13),
    (4, 1, 6, 5),
    (4, 2, 15, 10),
    (4, 3, 18, 12),
    (5, 1, 7, 6),
    (5, 2, 9, 10),
    (5, 3, 18, 13),
    (6, 1, 5, 6),
    (6, 2, 30, 12),
    (6, 3, 20, 16),
    (7, 1, 4, 6),
    (7, 2, 21, 12),
    (8, 1, 7, 7),
    (8, 2, 17, 12),
    (9, 1, 5, 7),
    (9, 2, 17, 13),
    (10, 1, 39, 8),
    (10, 2, 16, 13),
    (11, 1, 35, 8),
    (11, 2, 26, 16),
    (12, 1, 34, 8),
    (12, 2, 25, 16),
    (13, 1, 32, 8),
    (13, 2, 24, 16),
    (14, 1, 14, 10),
    (14, 2, 23, 16),
    (15, 1, 13, 10),
    (15, 2, 22, 16),
    (16, 1, 8, 10),
    (16, 2, 21, 16),
    (17, 1, 31, 12),
    (18, 1, 26, 12),
    (19, 1, 25, 12),
    (20, 1, 23, 12),
    (21, 1, 22, 12),
    (22, 1, 31, 13),
    (23, 1, 30, 13),
    (24, 1, 29, 13),
    (25, 1, 28, 13),
    (26, 1, 27, 13),
    (27, 1, 31, 16),
    (28, 1, 30, 16),
    (29, 1, 29, 16),
    (30, 1, 28, 16),
    (31, 1, 27, 16),
];
// B14 esc=(1, 6) eob=(2, 2)
pub(crate) const B15: [(u8, u8, u16, u8); 111] = [
    (0, 1, 2, 2),
    (0, 2, 6, 3),
    (0, 3, 7, 4),
    (0, 4, 28, 5),
    (0, 5, 29, 5),
    (0, 6, 5, 6),
    (0, 7, 4, 6),
    (0, 8, 123, 7),
    (0, 9, 124, 7),
    (0, 10, 35, 8),
    (0, 11, 34, 8),
    (0, 12, 250, 8),
    (0, 13, 251, 8),
    (0, 14, 254, 8),
    (0, 15, 255, 8),
    (0, 16, 31, 14),
    (0, 17, 30, 14),
    (0, 18, 29, 14),
    (0, 19, 28, 14),
    (0, 20, 27, 14),
    (0, 21, 26, 14),
    (0, 22, 25, 14),
    (0, 23, 24, 14),
    (0, 24, 23, 14),
    (0, 25, 22, 14),
    (0, 26, 21, 14),
    (0, 27, 20, 14),
    (0, 28, 19, 14),
    (0, 29, 18, 14),
    (0, 30, 17, 14),
    (0, 31, 16, 14),
    (0, 32, 24, 15),
    (0, 33, 23, 15),
    (0, 34, 22, 15),
    (0, 35, 21, 15),
    (0, 36, 20, 15),
    (0, 37, 19, 15),
    (0, 38, 18, 15),
    (0, 39, 17, 15),
    (0, 40, 16, 15),
    (1, 1, 2, 3),
    (1, 2, 6, 5),
    (1, 3, 121, 7),
    (1, 4, 39, 8),
    (1, 5, 32, 8),
    (1, 6, 22, 13),
    (1, 7, 21, 13),
    (1, 8, 31, 15),
    (1, 9, 30, 15),
    (1, 10, 29, 15),
    (1, 11, 28, 15),
    (1, 12, 27, 15),
    (1, 13, 26, 15),
    (1, 14, 25, 15),
    (1, 15, 19, 16),
    (1, 16, 18, 16),
    (1, 17, 17, 16),
    (1, 18, 16, 16),
    (2, 1, 5, 5),
    (2, 2, 7, 7),
    (2, 3, 252, 8),
    (2, 4, 12, 10),
    (2, 5, 20, 13),
    (3, 1, 7, 5),
    (3, 2, 38, 8),
    (3, 3, 28, 12),
    (3, 4, 19, 13),
    (4, 1, 6, 6),
    (4, 2, 253, 8),
    (4, 3, 18, 12),
    (5, 1, 7, 6),
    (5, 2, 4, 9),
    (5, 3, 18, 13),
    (6, 1, 6, 7),
    (6, 2, 30, 12),
    (6, 3, 20, 16),
    (7, 1, 4, 7),
    (7, 2, 21, 12),
    (8, 1, 5, 7),
    (8, 2, 17, 12),
    (9, 1, 120, 7),
    (9, 2, 17, 13),
    (10, 1, 122, 7),
    (10, 2, 16, 13),
    (11, 1, 33, 8),
    (11, 2, 26, 16),
    (12, 1, 37, 8),
    (12, 2, 25, 16),
    (13, 1, 36, 8),
    (13, 2, 24, 16),
    (14, 1, 5, 9),
    (14, 2, 23, 16),
    (15, 1, 7, 9),
    (15, 2, 22, 16),
    (16, 1, 13, 10),
    (16, 2, 21, 16),
    (17, 1, 31, 12),
    (18, 1, 26, 12),
    (19, 1, 25, 12),
    (20, 1, 23, 12),
    (21, 1, 22, 12),
    (22, 1, 31, 13),
    (23, 1, 30, 13),
    (24, 1, 29, 13),
    (25, 1, 28, 13),
    (26, 1, 27, 13),
    (27, 1, 31, 16),
    (28, 1, 30, 16),
    (29, 1, 29, 16),
    (30, 1, 28, 16),
    (31, 1, 27, 16),
];
// B15 esc=(1, 6) eob=(6, 4)

/// Escape code of tables B.14 and B.15 (6 bits).
pub(crate) const ESCAPE: (u16, u8) = (1, 6);
/// End-of-block code of table B.14.
pub(crate) const EOB_B14: (u16, u8) = (2, 2);
/// End-of-block code of table B.15.
pub(crate) const EOB_B15: (u16, u8) = (6, 4);

/// Macroblock type flags (tables B.2 to B.4).
pub(crate) const MB_QUANT: u16 = 1;
pub(crate) const MB_FWD: u16 = 2;
pub(crate) const MB_BWD: u16 = 4;
pub(crate) const MB_PAT: u16 = 8;
pub(crate) const MB_INTRA: u16 = 16;

/// Table B.2: macroblock_type in I pictures, (code, length, flags).
pub(crate) const MBTYPE_I: &[(u16, u8, u16)] = &[(0b1, 1, MB_INTRA), (0b01, 2, MB_INTRA | MB_QUANT)];
/// Table B.3: macroblock_type in P pictures.
pub(crate) const MBTYPE_P: &[(u16, u8, u16)] = &[
    (0b1, 1, MB_FWD | MB_PAT),
    (0b01, 2, MB_PAT),
    (0b001, 3, MB_FWD),
    (0b00011, 5, MB_INTRA),
    (0b00010, 5, MB_FWD | MB_PAT | MB_QUANT),
    (0b00001, 5, MB_PAT | MB_QUANT),
    (0b000001, 6, MB_INTRA | MB_QUANT),
];
/// Table B.4: macroblock_type in B pictures.
pub(crate) const MBTYPE_B: &[(u16, u8, u16)] = &[
    (0b10, 2, MB_FWD | MB_BWD),
    (0b11, 2, MB_FWD | MB_BWD | MB_PAT),
    (0b010, 3, MB_BWD),
    (0b011, 3, MB_BWD | MB_PAT),
    (0b0010, 4, MB_FWD),
    (0b0011, 4, MB_FWD | MB_PAT),
    (0b00011, 5, MB_INTRA),
    (0b00010, 5, MB_FWD | MB_BWD | MB_PAT | MB_QUANT),
    (0b000011, 6, MB_FWD | MB_PAT | MB_QUANT),
    (0b000010, 6, MB_BWD | MB_PAT | MB_QUANT),
    (0b000001, 6, MB_INTRA | MB_QUANT),
];

/// Table B.1: macroblock_address_increment, (code, length, value 1..=33).
pub(crate) const MBA: &[(u16, u8, u16)] = &[
    (0b1, 1, 1),
    (0b011, 3, 2),
    (0b010, 3, 3),
    (0b0011, 4, 4),
    (0b0010, 4, 5),
    (0b00011, 5, 6),
    (0b00010, 5, 7),
    (0b0000111, 7, 8),
    (0b0000110, 7, 9),
    (0b00001011, 8, 10),
    (0b00001010, 8, 11),
    (0b00001001, 8, 12),
    (0b00001000, 8, 13),
    (0b00000111, 8, 14),
    (0b00000110, 8, 15),
    (0b0000010111, 10, 16),
    (0b0000010110, 10, 17),
    (0b0000010101, 10, 18),
    (0b0000010100, 10, 19),
    (0b0000010011, 10, 20),
    (0b0000010010, 10, 21),
    (0b00000100011, 11, 22),
    (0b00000100010, 11, 23),
    (0b00000100001, 11, 24),
    (0b00000100000, 11, 25),
    (0b00000011111, 11, 26),
    (0b00000011110, 11, 27),
    (0b00000011101, 11, 28),
    (0b00000011100, 11, 29),
    (0b00000011011, 11, 30),
    (0b00000011010, 11, 31),
    (0b00000011001, 11, 32),
    (0b00000011000, 11, 33),
    // macroblock_escape (adds 33) and the MPEG-1 stuffing code (adds nothing).
    (0b00000001000, 11, 34),
    (0b00000001111, 11, 35),
];

/// Table B.9: coded_block_pattern, (code, length, cbp).
pub(crate) const CBP: &[(u16, u8, u16)] = &[
    (0b000000001, 9, 0),
    (0b01011, 5, 1),
    (0b01001, 5, 2),
    (0b001101, 6, 3),
    (0b1101, 4, 4),
    (0b0010111, 7, 5),
    (0b0010011, 7, 6),
    (0b00011111, 8, 7),
    (0b1100, 4, 8),
    (0b0010110, 7, 9),
    (0b0010010, 7, 10),
    (0b00011110, 8, 11),
    (0b10011, 5, 12),
    (0b00011011, 8, 13),
    (0b00010111, 8, 14),
    (0b00010011, 8, 15),
    (0b1011, 4, 16),
    (0b0010101, 7, 17),
    (0b0010001, 7, 18),
    (0b00011101, 8, 19),
    (0b10001, 5, 20),
    (0b00011001, 8, 21),
    (0b00010101, 8, 22),
    (0b00010001, 8, 23),
    (0b001111, 6, 24),
    (0b00001111, 8, 25),
    (0b00001101, 8, 26),
    (0b000000011, 9, 27),
    (0b01111, 5, 28),
    (0b00001011, 8, 29),
    (0b00000111, 8, 30),
    (0b000000111, 9, 31),
    (0b1010, 4, 32),
    (0b0010100, 7, 33),
    (0b0010000, 7, 34),
    (0b00011100, 8, 35),
    (0b001110, 6, 36),
    (0b00001110, 8, 37),
    (0b00001100, 8, 38),
    (0b000000010, 9, 39),
    (0b10000, 5, 40),
    (0b00011000, 8, 41),
    (0b00010100, 8, 42),
    (0b00010000, 8, 43),
    (0b01110, 5, 44),
    (0b00001010, 8, 45),
    (0b00000110, 8, 46),
    (0b000000110, 9, 47),
    (0b10010, 5, 48),
    (0b00011010, 8, 49),
    (0b00010110, 8, 50),
    (0b00010010, 8, 51),
    (0b01101, 5, 52),
    (0b00001001, 8, 53),
    (0b00000101, 8, 54),
    (0b000000101, 9, 55),
    (0b01100, 5, 56),
    (0b00001000, 8, 57),
    (0b00000100, 8, 58),
    (0b000000100, 9, 59),
    (0b111, 3, 60),
    (0b01010, 5, 61),
    (0b01000, 5, 62),
    (0b001100, 6, 63),
];

/// Table B.10: motion_code magnitude 0..=16, (code, length) without the sign bit.
pub(crate) const MOTION: [(u16, u8); 17] = [
    (0b1, 1),
    (0b01, 2),
    (0b001, 3),
    (0b0001, 4),
    (0b000011, 6),
    (0b0000101, 7),
    (0b0000100, 7),
    (0b0000011, 7),
    (0b000001011, 9),
    (0b000001010, 9),
    (0b000001001, 9),
    (0b0000010001, 10),
    (0b0000010000, 10),
    (0b0000001111, 10),
    (0b0000001110, 10),
    (0b0000001101, 10),
    (0b0000001100, 10),
];

/// Table B.12: dct_dc_size_luminance, (code, length) for sizes 0..=11.
pub(crate) const DC_LUMA: [(u16, u8); 12] = [
    (0b100, 3),
    (0b00, 2),
    (0b01, 2),
    (0b101, 3),
    (0b110, 3),
    (0b1110, 4),
    (0b11110, 5),
    (0b111110, 6),
    (0b1111110, 7),
    (0b11111110, 8),
    (0b111111110, 9),
    (0b111111111, 9),
];
/// Table B.13: dct_dc_size_chrominance.
pub(crate) const DC_CHROMA: [(u16, u8); 12] = [
    (0b00, 2),
    (0b01, 2),
    (0b10, 2),
    (0b110, 3),
    (0b1110, 4),
    (0b11110, 5),
    (0b111110, 6),
    (0b1111110, 7),
    (0b11111110, 8),
    (0b111111110, 9),
    (0b1111111110, 10),
    (0b1111111111, 10),
];

/// Classic zig-zag scan, scan position to raster index.
pub(crate) const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
/// Alternate scan (Figure 7-3), scan position to raster index.
pub(crate) const ALT_SCAN: [u8; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10, 17, 25, 32, 40, 48, 56, 57, 49, 41, 33, 26, 18, 3, 11, 4, 12, 19,
    27, 34, 42, 50, 58, 35, 43, 51, 59, 20, 28, 5, 13, 6, 14, 21, 29, 36, 44, 52, 60, 37, 45, 53,
    61, 22, 30, 7, 15, 23, 31, 38, 46, 54, 62, 39, 47, 55, 63,
];

/// Default intra quantiser matrix in zig-zag order (6.3.11).
pub(crate) const DEFAULT_INTRA: [u8; 64] = [
    8, 16, 16, 19, 16, 19, 22, 22, 22, 22, 22, 22, 26, 24, 26, 27, 27, 27, 26, 26, 26, 26, 27, 27,
    27, 29, 29, 29, 34, 34, 34, 29, 29, 29, 27, 27, 29, 29, 32, 32, 34, 34, 37, 38, 37, 35, 35, 34,
    35, 38, 38, 40, 40, 40, 48, 48, 46, 46, 56, 56, 58, 69, 69, 83,
];

/// Non-linear quantiser_scale (q_scale_type = 1), index quantiser_scale_code.
pub(crate) const NONLINEAR_QSCALE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64,
    72, 80, 88, 96, 104, 112,
];

/// A prefix-code lookup table indexed by the next `bits` bits of the stream.
pub(crate) struct Vlc {
    /// `len << 16 | value`; len 0 marks an invalid prefix.
    pub(crate) table: Vec<u32>,
    pub(crate) bits: u32,
}

impl Vlc {
    /// Build from `(code, length, value)` rows.
    pub(crate) fn build(rows: &[(u32, u32, u32)]) -> Vlc {
        let bits = rows.iter().map(|r| r.1).max().unwrap_or(1);
        let mut table = vec![0u32; 1 << bits];
        for &(code, len, value) in rows {
            let lo = (code << (bits - len)) as usize;
            let hi = lo + (1usize << (bits - len));
            for slot in &mut table[lo..hi] {
                *slot = (len << 16) | value;
            }
        }
        Vlc { table, bits }
    }

    fn small(rows: &[(u16, u8, u16)]) -> Vlc {
        let rows: Vec<_> = rows
            .iter()
            .map(|&(c, l, v)| (c as u32, l as u32, v as u32))
            .collect();
        Vlc::build(&rows)
    }

    fn pairs(rows: &[(u16, u8)]) -> Vlc {
        let rows: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, &(c, l))| (c as u32, l as u32, i as u32))
            .collect();
        Vlc::build(&rows)
    }

    fn coef(rows: &[(u8, u8, u16, u8); 111], esc: (u16, u8), eob: (u16, u8)) -> Vlc {
        // The sign bit follows the code and is read separately.
        let mut r: Vec<(u32, u32, u32)> = rows
            .iter()
            .map(|&(run, level, code, len)| {
                (code as u32, len as u32, ((run as u32) << 8) | level as u32)
            })
            .collect();
        r.push((esc.0 as u32, esc.1 as u32, COEF_ESC));
        r.push((eob.0 as u32, eob.1 as u32, COEF_EOB));
        Vlc::build(&r)
    }
}

/// Table values for end-of-block and escape.
pub(crate) const COEF_EOB: u32 = 0xFFFF;
pub(crate) const COEF_ESC: u32 = 0xFFFE;

/// All lookup tables, built once.
pub(crate) struct Tables {
    pub(crate) mba: Vlc,
    pub(crate) cbp: Vlc,
    pub(crate) mbtype: [Vlc; 3],
    pub(crate) motion: Vlc,
    pub(crate) dc_luma: Vlc,
    pub(crate) dc_chroma: Vlc,
    pub(crate) coef0: Vlc,
    pub(crate) coef1: Vlc,
}

pub(crate) fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| Tables {
        mba: Vlc::small(MBA),
        cbp: Vlc::small(CBP),
        mbtype: [
            Vlc::small(MBTYPE_I),
            Vlc::small(MBTYPE_P),
            Vlc::small(MBTYPE_B),
        ],
        motion: Vlc::pairs(&MOTION),
        dc_luma: Vlc::pairs(&DC_LUMA),
        dc_chroma: Vlc::pairs(&DC_CHROMA),
        coef0: Vlc::coef(&B14, ESCAPE, EOB_B14),
        coef1: Vlc::coef(&B15, ESCAPE, EOB_B15),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix_free(rows: &[(u32, u32)]) {
        for (i, a) in rows.iter().enumerate() {
            for (j, b) in rows.iter().enumerate() {
                if i != j && b.1 >= a.1 {
                    assert!(
                        (b.0 >> (b.1 - a.1)) != a.0,
                        "code {:b}/{} is a prefix of {:b}/{}",
                        a.0,
                        a.1,
                        b.0,
                        b.1
                    );
                }
            }
        }
    }

    fn check(rows: &[(u32, u32)], expect_full: bool) {
        prefix_free(rows);
        let kraft: f64 = rows.iter().map(|r| 2f64.powi(-(r.1 as i32))).sum();
        assert!(kraft <= 1.0 + 1e-12, "kraft {kraft}");
        if expect_full {
            assert!((kraft - 1.0).abs() < 1e-12, "kraft {kraft}");
        }
    }

    #[test]
    fn all_codes_are_prefix_free() {
        let t = |r: &[(u16, u8, u16)]| -> Vec<(u32, u32)> {
            r.iter().map(|x| (x.0 as u32, x.1 as u32)).collect()
        };
        check(&t(MBA), false);
        check(&t(CBP), false);
        check(&t(MBTYPE_I), false);
        check(&t(MBTYPE_P), false);
        check(&t(MBTYPE_B), false);
        let p = |r: &[(u16, u8)]| -> Vec<(u32, u32)> {
            r.iter().map(|x| (x.0 as u32, x.1 as u32)).collect()
        };
        check(&p(&DC_LUMA), false);
        check(&p(&DC_CHROMA), false);
        let mut m = p(&MOTION);
        // motion codes are followed by a sign bit except zero
        m.truncate(17);
        check(&m, false);
        let mut c14: Vec<(u32, u32)> = B14.iter().map(|x| (x.2 as u32, x.3 as u32)).collect();
        c14.push((ESCAPE.0 as u32, ESCAPE.1 as u32));
        c14.push((EOB_B14.0 as u32, EOB_B14.1 as u32));
        check(&c14, false);
        let mut c15: Vec<(u32, u32)> = B15.iter().map(|x| (x.2 as u32, x.3 as u32)).collect();
        c15.push((ESCAPE.0 as u32, ESCAPE.1 as u32));
        c15.push((EOB_B15.0 as u32, EOB_B15.1 as u32));
        check(&c15, false);
    }

    #[test]
    fn scans_are_permutations() {
        for s in [&ZIGZAG, &ALT_SCAN] {
            let mut seen = [false; 64];
            for &x in s.iter() {
                assert!(!seen[x as usize]);
                seen[x as usize] = true;
            }
        }
    }

    #[test]
    fn mba_and_cbp_are_complete_enough() {
        // Table B.9 has 64 entries (cbp 0 included), B.1 has 33 + escape + stuffing.
        assert_eq!(CBP.len(), 64);
        let mut seen = [false; 64];
        for r in CBP {
            assert!(!seen[r.2 as usize]);
            seen[r.2 as usize] = true;
        }
        let kraft: f64 = CBP.iter().map(|r| 2f64.powi(-(r.1 as i32))).sum();
        assert!(kraft > 0.99, "cbp kraft {kraft}");
    }
}
