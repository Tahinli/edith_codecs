//! Constant tables transcribed from Rec. ITU-T H.264.
//!
//! Every table keeps the specification's own row order and column meaning so it
//! can be read side by side with the document. Nothing here is derived at run
//! time from anything else: a table that is "obviously" a formula in one row
//! and an exception in the next is exactly where a clever decoder goes wrong.

/// One entry of a variable length code table: the symbol it codes, its length
/// in bits and the code itself, most significant bit first.
///
/// The two symbol fields carry whatever the table's columns are — for
/// `coeff_token` that is `(TrailingOnes, TotalCoeff)`, for the single-value
/// tables the value is in `a` and `b` is unused.
pub type Vlc = (u8, u8, u8, u16);

/// Table 9-5, `coeff_token`, column `0 <= nC < 2`.
pub const COEFF_TOKEN_NC0: &[Vlc] = &[
    // (TrailingOnes, TotalCoeff, length, code)
    (0, 0, 1, 0b1),
    (0, 1, 6, 0b000101),
    (1, 1, 2, 0b01),
    (0, 2, 8, 0b00000111),
    (1, 2, 6, 0b000100),
    (2, 2, 3, 0b001),
    (0, 3, 9, 0b000000111),
    (1, 3, 8, 0b00000110),
    (2, 3, 7, 0b0000101),
    (3, 3, 5, 0b00011),
    (0, 4, 10, 0b0000000111),
    (1, 4, 9, 0b000000110),
    (2, 4, 8, 0b00000101),
    (3, 4, 6, 0b000011),
    (0, 5, 11, 0b00000000111),
    (1, 5, 10, 0b0000000110),
    (2, 5, 9, 0b000000101),
    (3, 5, 7, 0b0000100),
    (0, 6, 13, 0b0000000001111),
    (1, 6, 11, 0b00000000110),
    (2, 6, 10, 0b0000000101),
    (3, 6, 8, 0b00000100),
    (0, 7, 13, 0b0000000001011),
    (1, 7, 13, 0b0000000001110),
    (2, 7, 11, 0b00000000101),
    (3, 7, 9, 0b000000100),
    (0, 8, 13, 0b0000000001000),
    (1, 8, 13, 0b0000000001010),
    (2, 8, 13, 0b0000000001101),
    (3, 8, 10, 0b0000000100),
    (0, 9, 14, 0b00000000001111),
    (1, 9, 14, 0b00000000001110),
    (2, 9, 13, 0b0000000001001),
    (3, 9, 11, 0b00000000100),
    (0, 10, 14, 0b00000000001011),
    (1, 10, 14, 0b00000000001010),
    (2, 10, 14, 0b00000000001101),
    (3, 10, 13, 0b0000000001100),
    (0, 11, 15, 0b000000000001111),
    (1, 11, 15, 0b000000000001110),
    (2, 11, 14, 0b00000000001001),
    (3, 11, 14, 0b00000000001100),
    (0, 12, 15, 0b000000000001011),
    (1, 12, 15, 0b000000000001010),
    (2, 12, 15, 0b000000000001101),
    (3, 12, 14, 0b00000000001000),
    (0, 13, 16, 0b0000000000001111),
    (1, 13, 15, 0b000000000000001),
    (2, 13, 15, 0b000000000001001),
    (3, 13, 15, 0b000000000001100),
    (0, 14, 16, 0b0000000000001011),
    (1, 14, 16, 0b0000000000001110),
    (2, 14, 16, 0b0000000000001101),
    (3, 14, 15, 0b000000000001000),
    (0, 15, 16, 0b0000000000000111),
    (1, 15, 16, 0b0000000000001010),
    (2, 15, 16, 0b0000000000001001),
    (3, 15, 16, 0b0000000000001100),
    (0, 16, 16, 0b0000000000000100),
    (1, 16, 16, 0b0000000000000110),
    (2, 16, 16, 0b0000000000000101),
    (3, 16, 16, 0b0000000000001000),
];

/// Table 9-5, `coeff_token`, column `2 <= nC < 4`.
pub const COEFF_TOKEN_NC2: &[Vlc] = &[
    (0, 0, 2, 0b11),
    (0, 1, 6, 0b001011),
    (1, 1, 2, 0b10),
    (0, 2, 6, 0b000111),
    (1, 2, 5, 0b00111),
    (2, 2, 3, 0b011),
    (0, 3, 7, 0b0000111),
    (1, 3, 6, 0b001010),
    (2, 3, 6, 0b001001),
    (3, 3, 4, 0b0101),
    (0, 4, 8, 0b00000111),
    (1, 4, 6, 0b000110),
    (2, 4, 6, 0b000101),
    (3, 4, 4, 0b0100),
    (0, 5, 8, 0b00000100),
    (1, 5, 7, 0b0000110),
    (2, 5, 7, 0b0000101),
    (3, 5, 5, 0b00110),
    (0, 6, 9, 0b000000111),
    (1, 6, 8, 0b00000110),
    (2, 6, 8, 0b00000101),
    (3, 6, 6, 0b001000),
    (0, 7, 11, 0b00000001111),
    (1, 7, 9, 0b000000110),
    (2, 7, 9, 0b000000101),
    (3, 7, 6, 0b000100),
    (0, 8, 11, 0b00000001011),
    (1, 8, 11, 0b00000001110),
    (2, 8, 11, 0b00000001101),
    (3, 8, 7, 0b0000100),
    (0, 9, 12, 0b000000001111),
    (1, 9, 11, 0b00000001010),
    (2, 9, 11, 0b00000001001),
    (3, 9, 9, 0b000000100),
    (0, 10, 12, 0b000000001011),
    (1, 10, 12, 0b000000001110),
    (2, 10, 12, 0b000000001101),
    (3, 10, 11, 0b00000001100),
    (0, 11, 12, 0b000000001000),
    (1, 11, 12, 0b000000001010),
    (2, 11, 12, 0b000000001001),
    (3, 11, 11, 0b00000001000),
    (0, 12, 13, 0b0000000001111),
    (1, 12, 13, 0b0000000001110),
    (2, 12, 13, 0b0000000001101),
    (3, 12, 12, 0b000000001100),
    (0, 13, 13, 0b0000000001011),
    (1, 13, 13, 0b0000000001010),
    (2, 13, 13, 0b0000000001001),
    (3, 13, 13, 0b0000000001100),
    (0, 14, 13, 0b0000000000111),
    (1, 14, 14, 0b00000000001011),
    (2, 14, 13, 0b0000000000110),
    (3, 14, 13, 0b0000000001000),
    (0, 15, 14, 0b00000000001001),
    (1, 15, 14, 0b00000000001000),
    (2, 15, 14, 0b00000000001010),
    (3, 15, 13, 0b0000000000001),
    (0, 16, 14, 0b00000000000111),
    (1, 16, 14, 0b00000000000110),
    (2, 16, 14, 0b00000000000101),
    (3, 16, 14, 0b00000000000100),
];

/// Table 9-5, `coeff_token`, column `4 <= nC < 8`.
pub const COEFF_TOKEN_NC4: &[Vlc] = &[
    (0, 0, 4, 0b1111),
    (0, 1, 6, 0b001111),
    (1, 1, 4, 0b1110),
    (0, 2, 6, 0b001011),
    (1, 2, 5, 0b01111),
    (2, 2, 4, 0b1101),
    (0, 3, 6, 0b001000),
    (1, 3, 5, 0b01100),
    (2, 3, 5, 0b01110),
    (3, 3, 4, 0b1100),
    (0, 4, 7, 0b0001111),
    (1, 4, 5, 0b01010),
    (2, 4, 5, 0b01011),
    (3, 4, 4, 0b1011),
    (0, 5, 7, 0b0001011),
    (1, 5, 5, 0b01000),
    (2, 5, 5, 0b01001),
    (3, 5, 4, 0b1010),
    (0, 6, 7, 0b0001001),
    (1, 6, 6, 0b001110),
    (2, 6, 6, 0b001101),
    (3, 6, 4, 0b1001),
    (0, 7, 7, 0b0001000),
    (1, 7, 6, 0b001010),
    (2, 7, 6, 0b001001),
    (3, 7, 4, 0b1000),
    (0, 8, 8, 0b00001111),
    (1, 8, 7, 0b0001110),
    (2, 8, 7, 0b0001101),
    (3, 8, 5, 0b01101),
    (0, 9, 8, 0b00001011),
    (1, 9, 8, 0b00001110),
    (2, 9, 7, 0b0001010),
    (3, 9, 6, 0b001100),
    (0, 10, 9, 0b000001111),
    (1, 10, 8, 0b00001010),
    (2, 10, 8, 0b00001101),
    (3, 10, 7, 0b0001100),
    (0, 11, 9, 0b000001011),
    (1, 11, 9, 0b000001110),
    (2, 11, 8, 0b00001001),
    (3, 11, 8, 0b00001100),
    (0, 12, 9, 0b000001000),
    (1, 12, 9, 0b000001010),
    (2, 12, 9, 0b000001101),
    (3, 12, 8, 0b00001000),
    (0, 13, 10, 0b0000001101),
    (1, 13, 9, 0b000000111),
    (2, 13, 9, 0b000001001),
    (3, 13, 9, 0b000001100),
    (0, 14, 10, 0b0000001001),
    (1, 14, 10, 0b0000001100),
    (2, 14, 10, 0b0000001011),
    (3, 14, 10, 0b0000001010),
    (0, 15, 10, 0b0000000101),
    (1, 15, 10, 0b0000001000),
    (2, 15, 10, 0b0000000111),
    (3, 15, 10, 0b0000000110),
    (0, 16, 10, 0b0000000001),
    (1, 16, 10, 0b0000000100),
    (2, 16, 10, 0b0000000011),
    (3, 16, 10, 0b0000000010),
];

/// Table 9-5, `coeff_token`, column `nC == -1`: the 4:2:0 chroma DC block,
/// which has only four coefficients.
pub const COEFF_TOKEN_CHROMA_DC: &[Vlc] = &[
    (0, 0, 2, 0b01),
    (0, 1, 6, 0b000111),
    (1, 1, 1, 0b1),
    (0, 2, 6, 0b000100),
    (1, 2, 6, 0b000110),
    (2, 2, 3, 0b001),
    (0, 3, 6, 0b000011),
    (1, 3, 7, 0b0000011),
    (2, 3, 7, 0b0000010),
    (3, 3, 6, 0b000101),
    (0, 4, 6, 0b000010),
    (1, 4, 8, 0b00000011),
    (2, 4, 8, 0b00000010),
    (3, 4, 7, 0b0000000),
];

/// Tables 9-7 and 9-8, `total_zeros` for 4x4 blocks, indexed by
/// `tzVlcIndex = TotalCoeff` minus one.
pub const TOTAL_ZEROS_4X4: [&[Vlc]; 15] = [
    // tzVlcIndex = 1
    &[
        (0, 0, 1, 0b1),
        (1, 0, 3, 0b011),
        (2, 0, 3, 0b010),
        (3, 0, 4, 0b0011),
        (4, 0, 4, 0b0010),
        (5, 0, 5, 0b00011),
        (6, 0, 5, 0b00010),
        (7, 0, 6, 0b000011),
        (8, 0, 6, 0b000010),
        (9, 0, 7, 0b0000011),
        (10, 0, 7, 0b0000010),
        (11, 0, 8, 0b00000011),
        (12, 0, 8, 0b00000010),
        (13, 0, 9, 0b000000011),
        (14, 0, 9, 0b000000010),
        (15, 0, 9, 0b000000001),
    ],
    // tzVlcIndex = 2
    &[
        (0, 0, 3, 0b111),
        (1, 0, 3, 0b110),
        (2, 0, 3, 0b101),
        (3, 0, 3, 0b100),
        (4, 0, 3, 0b011),
        (5, 0, 4, 0b0101),
        (6, 0, 4, 0b0100),
        (7, 0, 4, 0b0011),
        (8, 0, 4, 0b0010),
        (9, 0, 5, 0b00011),
        (10, 0, 5, 0b00010),
        (11, 0, 6, 0b000011),
        (12, 0, 6, 0b000010),
        (13, 0, 6, 0b000001),
        (14, 0, 6, 0b000000),
    ],
    // tzVlcIndex = 3
    &[
        (0, 0, 4, 0b0101),
        (1, 0, 3, 0b111),
        (2, 0, 3, 0b110),
        (3, 0, 3, 0b101),
        (4, 0, 4, 0b0100),
        (5, 0, 4, 0b0011),
        (6, 0, 3, 0b100),
        (7, 0, 3, 0b011),
        (8, 0, 4, 0b0010),
        (9, 0, 5, 0b00011),
        (10, 0, 5, 0b00010),
        (11, 0, 6, 0b000001),
        (12, 0, 5, 0b00001),
        (13, 0, 6, 0b000000),
    ],
    // tzVlcIndex = 4
    &[
        (0, 0, 5, 0b00011),
        (1, 0, 3, 0b111),
        (2, 0, 4, 0b0101),
        (3, 0, 4, 0b0100),
        (4, 0, 3, 0b110),
        (5, 0, 3, 0b101),
        (6, 0, 3, 0b100),
        (7, 0, 4, 0b0011),
        (8, 0, 3, 0b011),
        (9, 0, 4, 0b0010),
        (10, 0, 5, 0b00010),
        (11, 0, 5, 0b00001),
        (12, 0, 5, 0b00000),
    ],
    // tzVlcIndex = 5
    &[
        (0, 0, 4, 0b0101),
        (1, 0, 4, 0b0100),
        (2, 0, 4, 0b0011),
        (3, 0, 3, 0b111),
        (4, 0, 3, 0b110),
        (5, 0, 3, 0b101),
        (6, 0, 3, 0b100),
        (7, 0, 3, 0b011),
        (8, 0, 4, 0b0010),
        (9, 0, 5, 0b00001),
        (10, 0, 4, 0b0001),
        (11, 0, 5, 0b00000),
    ],
    // tzVlcIndex = 6
    &[
        (0, 0, 6, 0b000001),
        (1, 0, 5, 0b00001),
        (2, 0, 3, 0b111),
        (3, 0, 3, 0b110),
        (4, 0, 3, 0b101),
        (5, 0, 3, 0b100),
        (6, 0, 3, 0b011),
        (7, 0, 3, 0b010),
        (8, 0, 4, 0b0001),
        (9, 0, 3, 0b001),
        (10, 0, 6, 0b000000),
    ],
    // tzVlcIndex = 7
    &[
        (0, 0, 6, 0b000001),
        (1, 0, 5, 0b00001),
        (2, 0, 3, 0b101),
        (3, 0, 3, 0b100),
        (4, 0, 3, 0b011),
        (5, 0, 2, 0b11),
        (6, 0, 3, 0b010),
        (7, 0, 4, 0b0001),
        (8, 0, 3, 0b001),
        (9, 0, 6, 0b000000),
    ],
    // tzVlcIndex = 8
    &[
        (0, 0, 6, 0b000001),
        (1, 0, 4, 0b0001),
        (2, 0, 5, 0b00001),
        (3, 0, 3, 0b011),
        (4, 0, 2, 0b11),
        (5, 0, 2, 0b10),
        (6, 0, 3, 0b010),
        (7, 0, 3, 0b001),
        (8, 0, 6, 0b000000),
    ],
    // tzVlcIndex = 9
    &[
        (0, 0, 6, 0b000001),
        (1, 0, 6, 0b000000),
        (2, 0, 4, 0b0001),
        (3, 0, 2, 0b11),
        (4, 0, 2, 0b10),
        (5, 0, 3, 0b001),
        (6, 0, 2, 0b01),
        (7, 0, 5, 0b00001),
    ],
    // tzVlcIndex = 10
    &[
        (0, 0, 5, 0b00001),
        (1, 0, 5, 0b00000),
        (2, 0, 3, 0b001),
        (3, 0, 2, 0b11),
        (4, 0, 2, 0b10),
        (5, 0, 2, 0b01),
        (6, 0, 4, 0b0001),
    ],
    // tzVlcIndex = 11
    &[
        (0, 0, 4, 0b0000),
        (1, 0, 4, 0b0001),
        (2, 0, 3, 0b001),
        (3, 0, 3, 0b010),
        (4, 0, 1, 0b1),
        (5, 0, 3, 0b011),
    ],
    // tzVlcIndex = 12
    &[
        (0, 0, 4, 0b0000),
        (1, 0, 4, 0b0001),
        (2, 0, 2, 0b01),
        (3, 0, 1, 0b1),
        (4, 0, 3, 0b001),
    ],
    // tzVlcIndex = 13
    &[
        (0, 0, 3, 0b000),
        (1, 0, 3, 0b001),
        (2, 0, 1, 0b1),
        (3, 0, 2, 0b01),
    ],
    // tzVlcIndex = 14
    &[(0, 0, 2, 0b00), (1, 0, 2, 0b01), (2, 0, 1, 0b1)],
    // tzVlcIndex = 15
    &[(0, 0, 1, 0b0), (1, 0, 1, 0b1)],
];

/// Table 9-9 (a), `total_zeros` for the 4:2:0 chroma DC block.
pub const TOTAL_ZEROS_CHROMA_DC: [&[Vlc]; 3] = [
    &[
        (0, 0, 1, 0b1),
        (1, 0, 2, 0b01),
        (2, 0, 3, 0b001),
        (3, 0, 3, 0b000),
    ],
    &[(0, 0, 1, 0b1), (1, 0, 2, 0b01), (2, 0, 2, 0b00)],
    &[(0, 0, 1, 0b1), (1, 0, 1, 0b0)],
];

/// Table 9-10, `run_before`, indexed by `zerosLeft - 1` and capped at the
/// `zerosLeft > 6` column.
pub const RUN_BEFORE: [&[Vlc]; 7] = [
    &[(0, 0, 1, 0b1), (1, 0, 1, 0b0)],
    &[(0, 0, 1, 0b1), (1, 0, 2, 0b01), (2, 0, 2, 0b00)],
    &[
        (0, 0, 2, 0b11),
        (1, 0, 2, 0b10),
        (2, 0, 2, 0b01),
        (3, 0, 2, 0b00),
    ],
    &[
        (0, 0, 2, 0b11),
        (1, 0, 2, 0b10),
        (2, 0, 2, 0b01),
        (3, 0, 3, 0b001),
        (4, 0, 3, 0b000),
    ],
    &[
        (0, 0, 2, 0b11),
        (1, 0, 2, 0b10),
        (2, 0, 3, 0b011),
        (3, 0, 3, 0b010),
        (4, 0, 3, 0b001),
        (5, 0, 3, 0b000),
    ],
    &[
        (0, 0, 2, 0b11),
        (1, 0, 3, 0b000),
        (2, 0, 3, 0b001),
        (3, 0, 3, 0b011),
        (4, 0, 3, 0b010),
        (5, 0, 3, 0b101),
        (6, 0, 3, 0b100),
    ],
    // zerosLeft > 6: seven three-bit codes, then run_before = 7..=14 as
    // (run_before - 3) zeros followed by a one.
    &[
        (0, 0, 3, 0b111),
        (1, 0, 3, 0b110),
        (2, 0, 3, 0b101),
        (3, 0, 3, 0b100),
        (4, 0, 3, 0b011),
        (5, 0, 3, 0b010),
        (6, 0, 3, 0b001),
        (7, 0, 4, 0b0001),
        (8, 0, 5, 0b00001),
        (9, 0, 6, 0b000001),
        (10, 0, 7, 0b0000001),
        (11, 0, 8, 0b00000001),
        (12, 0, 9, 0b000000001),
        (13, 0, 10, 0b0000000001),
        (14, 0, 11, 0b00000000001),
    ],
];

/// Table 9-4 (a), `coded_block_pattern` for `ChromaArrayType` 1 or 2, indexed
/// by `codeNum`: `(Intra_4x4 or Intra_8x8, Inter)`.
pub const CODED_BLOCK_PATTERN_CHROMA: [(u8, u8); 48] = [
    (47, 0),
    (31, 16),
    (15, 1),
    (0, 2),
    (23, 4),
    (27, 8),
    (29, 32),
    (30, 3),
    (7, 5),
    (11, 10),
    (13, 12),
    (14, 15),
    (39, 47),
    (43, 7),
    (45, 11),
    (46, 13),
    (16, 14),
    (3, 6),
    (5, 9),
    (10, 31),
    (12, 35),
    (19, 37),
    (21, 42),
    (26, 44),
    (28, 33),
    (35, 34),
    (37, 36),
    (42, 40),
    (44, 39),
    (1, 43),
    (2, 45),
    (4, 46),
    (8, 17),
    (17, 18),
    (18, 20),
    (20, 24),
    (24, 19),
    (6, 21),
    (9, 26),
    (22, 28),
    (25, 23),
    (32, 27),
    (33, 29),
    (34, 30),
    (36, 22),
    (40, 25),
    (38, 38),
    (41, 41),
];

/// Table 8-13, the 4x4 zig-zag (frame) inverse scan: coefficient index to
/// raster position `4 * y + x` inside the block.
pub const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// `(x, y)` of luma block `luma4x4BlkIdx` inside its macroblock, from the
/// inverse 4x4 luma block scanning process of clause 6.4.3.
pub const LUMA_4X4_BLOCK_XY: [(usize, usize); 16] = [
    (0, 0),
    (4, 0),
    (0, 4),
    (4, 4),
    (8, 0),
    (12, 0),
    (8, 4),
    (12, 4),
    (0, 8),
    (4, 8),
    (0, 12),
    (4, 12),
    (8, 8),
    (12, 8),
    (8, 12),
    (12, 12),
];

/// Table 8-14, `v` for the inverse scaling of 4x4 blocks: three distinct
/// entries per `m = qP % 6`, positioned by [`norm_adjust_4x4`].
pub const NORM_ADJUST_V: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// `normAdjust4x4(m, i, j)` of clause 8.5.9: which of the three `v` columns
/// applies at position `(i, j)` of the block (`i` = row, `j` = column).
pub fn norm_adjust_4x4(m: usize, i: usize, j: usize) -> i32 {
    let column = match (i % 2, j % 2) {
        (0, 0) => 0,
        (1, 1) => 1,
        _ => 2,
    };
    NORM_ADJUST_V[m][column]
}

/// Table 8-15, `QPC` as a function of `qPI`, for `qPI >= 30`; below 30 the
/// mapping is the identity.
pub const QPC_FROM_QPI: [i32; 22] = [
    29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
];

/// `QPC` (Table 8-15) for a chroma quantisation index.
pub fn qpc_from_qpi(qpi: i32) -> i32 {
    if qpi < 30 {
        qpi
    } else {
        QPC_FROM_QPI[(qpi - 30).clamp(0, 21) as usize]
    }
}

/// Table 8-16, `alpha'` indexed by `indexA`.
pub const ALPHA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20,
    22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226,
    255, 255,
];

/// Table 8-16, `beta'` indexed by `indexB`.
pub const BETA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8, 8,
    9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
];

/// Table 8-17, `tC0` indexed by `indexA` and by `bS - 1`.
///
/// The `bS == 3` column (index 2) is the one an intra-only decoder uses, and it
/// is verified at every `indexA` the conformance suite and the quantiser sweep
/// reach (`tests/conformance.rs`). The `bS == 1` and `bS == 2` columns only
/// apply to inter macroblocks, so nothing in this release exercises them yet;
/// the inter release must re-check them the same way.
pub const TC0: [[i32; 3]; 52] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 1],
    [0, 1, 1],
    [0, 1, 1],
    [0, 1, 1],
    [0, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 2, 2],
    [1, 2, 2],
    [1, 2, 2],
    [2, 2, 2],
    [2, 2, 3],
    [2, 3, 3],
    [2, 3, 3],
    [2, 3, 4],
    [3, 3, 4],
    [3, 4, 4],
    [3, 4, 5],
    [4, 5, 6],
    [4, 5, 6],
    [4, 6, 7],
    [5, 7, 8],
    [6, 8, 9],
    [6, 8, 10],
    [7, 10, 11],
    [8, 11, 13],
    [9, 12, 14],
    [10, 13, 16],
    [11, 15, 18],
    [13, 17, 20],
    [14, 19, 23],
    [16, 21, 25],
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every VLC table has to be a prefix code, or the linear-scan decoder in
    /// `cavlc` could match two entries at once. This is the one property that
    /// catches a mistyped code that still looks plausible in isolation.
    fn assert_prefix_free(name: &str, table: &[Vlc]) {
        for (i, &(a1, b1, len1, code1)) in table.iter().enumerate() {
            assert!((1..=16).contains(&len1), "{name}: bad length {len1}");
            assert!(
                (code1 as u32) < (1u32 << len1),
                "{name}: code {code1:b} wider than its {len1} bits"
            );
            for &(a2, b2, len2, code2) in table.iter().skip(i + 1) {
                let (short, long, short_len) = if len1 <= len2 {
                    (code1 as u32, code2 as u32, len1)
                } else {
                    (code2 as u32, code1 as u32, len2)
                };
                let long_len = len1.max(len2);
                assert_ne!(
                    long >> (long_len - short_len),
                    short,
                    "{name}: ({a1},{b1}) and ({a2},{b2}) share a prefix"
                );
            }
        }
    }

    #[test]
    fn vlc_tables_are_prefix_codes() {
        assert_prefix_free("coeff_token nC<2", COEFF_TOKEN_NC0);
        assert_prefix_free("coeff_token 2<=nC<4", COEFF_TOKEN_NC2);
        assert_prefix_free("coeff_token 4<=nC<8", COEFF_TOKEN_NC4);
        assert_prefix_free("coeff_token chroma DC", COEFF_TOKEN_CHROMA_DC);
        for (i, table) in TOTAL_ZEROS_4X4.iter().enumerate() {
            assert_prefix_free(&format!("total_zeros tzVlcIndex {}", i + 1), table);
            assert_eq!(table.len(), 16 - i, "total_zeros row length");
        }
        for (i, table) in TOTAL_ZEROS_CHROMA_DC.iter().enumerate() {
            assert_prefix_free(&format!("chroma DC total_zeros {}", i + 1), table);
            assert_eq!(table.len(), 4 - i);
        }
        for (i, table) in RUN_BEFORE.iter().enumerate() {
            assert_prefix_free(&format!("run_before zerosLeft {}", i + 1), table);
        }
    }

    #[test]
    fn coeff_token_tables_cover_every_symbol() {
        for (name, table, max) in [
            ("nC<2", COEFF_TOKEN_NC0, 16u8),
            ("2<=nC<4", COEFF_TOKEN_NC2, 16),
            ("4<=nC<8", COEFF_TOKEN_NC4, 16),
            ("chroma DC", COEFF_TOKEN_CHROMA_DC, 4),
        ] {
            for total_coeff in 0..=max {
                for trailing_ones in 0..=3.min(total_coeff) {
                    let found = table
                        .iter()
                        .filter(|&&(t, c, _, _)| t == trailing_ones && c == total_coeff)
                        .count();
                    assert_eq!(
                        found, 1,
                        "{name}: ({trailing_ones}, {total_coeff}) appears {found} times"
                    );
                }
            }
            let expected = 1 + (1..=max as usize).map(|c| 1 + 3.min(c)).sum::<usize>();
            assert_eq!(table.len(), expected, "{name}: entry count");
        }
    }

    #[test]
    fn coded_block_pattern_is_a_permutation() {
        // Both columns of Table 9-4 are bijections onto 0..48: a typo that
        // duplicates one pattern necessarily drops another.
        for column in 0..2 {
            let mut seen = [false; 48];
            for entry in CODED_BLOCK_PATTERN_CHROMA {
                let v = if column == 0 { entry.0 } else { entry.1 } as usize;
                assert!(v < 48, "coded_block_pattern {v} out of range");
                assert!(!seen[v], "coded_block_pattern {v} appears twice");
                seen[v] = true;
            }
        }
    }

    #[test]
    fn zigzag_scan_visits_every_position_once() {
        let mut seen = [false; 16];
        for &p in &ZIGZAG_4X4 {
            assert!(!seen[p], "position {p} scanned twice");
            seen[p] = true;
        }
        // Corner anchors of the diagonal scan.
        assert_eq!(ZIGZAG_4X4[0], 0);
        assert_eq!(ZIGZAG_4X4[15], 15);
        assert_eq!(ZIGZAG_4X4[1], 1, "first step is horizontal");
        assert_eq!(ZIGZAG_4X4[2], 4);
    }

    #[test]
    fn quantisation_and_deblocking_tables() {
        assert_eq!(norm_adjust_4x4(0, 0, 0), 10);
        assert_eq!(norm_adjust_4x4(0, 1, 1), 16);
        assert_eq!(norm_adjust_4x4(0, 0, 1), 13);
        assert_eq!(norm_adjust_4x4(5, 3, 3), 29);
        assert_eq!(qpc_from_qpi(29), 29, "identity below 30");
        assert_eq!(qpc_from_qpi(30), 29);
        assert_eq!(qpc_from_qpi(39), 35);
        assert_eq!(qpc_from_qpi(51), 39);
        // alpha and beta are non-decreasing and start filtering at index 16.
        assert!(ALPHA.windows(2).all(|w| w[0] <= w[1]));
        assert!(BETA.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(ALPHA[15], 0);
        assert_eq!(ALPHA[16], 4);
        assert_eq!(BETA[16], 2);
        // tC0 grows with both bS and indexA.
        for row in TC0 {
            assert!(row[0] <= row[1] && row[1] <= row[2], "tC0 row {row:?}");
        }
        assert!(TC0.windows(2).all(|w| w[0][2] <= w[1][2]));
        assert_eq!(TC0[51], [16, 21, 25]);
        // tC0 for bS == 3 first becomes non-zero at indexA 17, exactly where
        // alpha and beta do.
        assert_eq!(TC0[16][2], 0);
        assert_eq!(TC0[17][2], 1);
    }
}
