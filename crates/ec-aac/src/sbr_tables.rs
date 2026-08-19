//! SBR (HE-AAC v1) Huffman codebooks (ISO/IEC 14496-3 §4.6.18.3.6).
//!
//! Derived black box from a reference decoder's own bit accounting by
//! `scripts/aac-tables/sbrtables.py`: no source was consulted. The
//! decoder's "Expected to read N SBR bytes actually read M" complaint
//! reads codeword lengths off a controlled FIL payload, and its
//! `env_facs_q`/`noise_facs_q` range check reads the delta value each
//! codeword carries off the same payload. Every table here closed with
//! a Kraft sum of exactly 1 (asserted below).
#![allow(dead_code)]

// Not emitted (values incomplete or book did not close): ['ENV15_F', 'ENV15_T', 'ENV30_T', 'ENV30_F', 'ENVB15_F', 'ENVB15_T', 'ENVB30_T', 'ENVB30_F']

/// SBR Huffman book `NOISE_T`: (length, code, delta) by codeword.
pub(crate) static NOISE_T: [(u8, u32, i32); 63] = [
    (1, 0, 0),
    (2, 2, 1),
    (3, 6, -1),
    (4, 14, -2),
    (5, 30, 2),
    (6, 62, -3),
    (8, 252, 3),
    (8, 253, -4),
    (10, 1016, 4),
    (11, 2034, -5),
    (13, 8140, 5),
    (13, 8141, 11),
    (13, 8142, -31),
    (13, 8143, -30),
    (13, 8144, -29),
    (13, 8145, -28),
    (13, 8146, -27),
    (13, 8147, -26),
    (13, 8148, -25),
    (13, 8149, -24),
    (13, 8150, -23),
    (13, 8151, -22),
    (13, 8152, -21),
    (13, 8153, -20),
    (13, 8154, -19),
    (13, 8155, -18),
    (13, 8156, -17),
    (13, 8157, -16),
    (13, 8158, -15),
    (13, 8159, -14),
    (13, 8160, -13),
    (13, 8161, -12),
    (13, 8162, -11),
    (13, 8163, -10),
    (13, 8164, -9),
    (13, 8165, -8),
    (13, 8166, -7),
    (13, 8167, -6),
    (13, 8168, 6),
    (13, 8169, 7),
    (13, 8170, 8),
    (13, 8171, 9),
    (13, 8172, 10),
    (13, 8173, 12),
    (13, 8174, 13),
    (13, 8175, 14),
    (13, 8176, 15),
    (13, 8177, 16),
    (13, 8178, 17),
    (13, 8179, 18),
    (13, 8180, 19),
    (13, 8181, 20),
    (13, 8182, 21),
    (13, 8183, 22),
    (13, 8184, 23),
    (13, 8185, 24),
    (13, 8186, 25),
    (13, 8187, 26),
    (13, 8188, 27),
    (13, 8189, 28),
    (13, 8190, 29),
    (14, 16382, 30),
    (14, 16383, 31),
];

/// SBR Huffman book `NOISE_F`: (length, code, delta) by codeword.
pub(crate) static NOISE_F: [(u8, u32, i32); 63] = [
    (1, 0, 0),
    (2, 2, -1),
    (3, 6, 1),
    (4, 14, -2),
    (5, 30, 2),
    (6, 62, -3),
    (8, 252, 3),
    (8, 253, -4),
    (9, 508, 4),
    (9, 509, -5),
    (10, 1020, 5),
    (10, 1021, -6),
    (11, 2044, 6),
    (11, 2045, -7),
    (12, 4092, 7),
    (12, 4093, -8),
    (13, 8188, 8),
    (14, 16378, 9),
    (14, 16379, -9),
    (15, 32760, -10),
    (15, 32761, 10),
    (15, 32762, 11),
    (16, 65526, -11),
    (16, 65527, -12),
    (16, 65528, 12),
    (16, 65529, 13),
    (17, 131060, -13),
    (17, 131061, -15),
    (17, 131062, 14),
    (17, 131063, 15),
    (18, 262128, -14),
    (18, 262129, 18),
    (18, 262130, -18),
    (18, 262131, -24),
    (18, 262132, -19),
    (18, 262133, 16),
    (18, 262134, 17),
    (19, 524270, -22),
    (19, 524271, -21),
    (19, 524272, -16),
    (19, 524273, 20),
    (19, 524274, 21),
    (19, 524275, 22),
    (19, 524276, 25),
    (19, 524277, -23),
    (19, 524278, -20),
    (19, 524279, 24),
    (20, 1048560, -31),
    (20, 1048561, -30),
    (20, 1048562, -29),
    (20, 1048563, -28),
    (20, 1048564, -27),
    (20, 1048565, -26),
    (20, 1048566, -25),
    (20, 1048567, -17),
    (20, 1048568, 19),
    (20, 1048569, 23),
    (20, 1048570, 26),
    (20, 1048571, 27),
    (20, 1048572, 28),
    (20, 1048573, 29),
    (20, 1048574, 30),
    (20, 1048575, 31),
];

/// SBR Huffman book `NOISEB_T`: (length, code, delta) by codeword.
pub(crate) static NOISEB_T: [(u8, u32, i32); 25] = [
    (1, 0, 0),
    (2, 2, -1),
    (3, 6, 1),
    (5, 28, -2),
    (6, 58, 2),
    (8, 236, -12),
    (8, 237, -11),
    (8, 238, -10),
    (8, 239, -9),
    (8, 240, -8),
    (8, 241, -7),
    (8, 242, -6),
    (8, 243, -5),
    (8, 244, -4),
    (8, 245, -3),
    (8, 246, 3),
    (8, 247, 4),
    (8, 248, 5),
    (8, 249, 6),
    (8, 250, 7),
    (8, 251, 8),
    (8, 252, 9),
    (8, 253, 10),
    (8, 254, 11),
    (8, 255, 12),
];

/// SBR Huffman book `NOISEB_F`: (length, code, delta) by codeword.
pub(crate) static NOISEB_F: [(u8, u32, i32); 25] = [
    (1, 0, 0),
    (2, 2, -1),
    (3, 6, 1),
    (4, 14, -2),
    (5, 30, 2),
    (6, 62, 3),
    (7, 126, -3),
    (8, 254, -4),
    (9, 510, 4),
    (11, 2044, -5),
    (12, 4090, 5),
    (13, 8182, 6),
    (13, 8183, -12),
    (13, 8184, -11),
    (13, 8185, -10),
    (13, 8186, -9),
    (13, 8187, -8),
    (14, 16376, -7),
    (14, 16377, -6),
    (14, 16378, 7),
    (14, 16379, 8),
    (14, 16380, 9),
    (14, 16381, 10),
    (14, 16382, 11),
    (14, 16383, 12),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn kraft_is_one(codes: &[(u8, u32, i32)]) {
        let sum: f64 = codes.iter().map(|&(l, _, _)| 2f64.powi(-(l as i32))).sum();
        assert!((sum - 1.0).abs() < 1e-9, "Kraft sum {sum}");
    }

    fn is_prefix_free(codes: &[(u8, u32, i32)]) {
        for (i, &(li, ci, _)) in codes.iter().enumerate() {
            for &(lj, cj, _) in codes.iter().skip(i + 1) {
                let (short, long) = if li <= lj { (li, lj) } else { (lj, li) };
                let (sc, lc) = if li <= lj { (ci, cj) } else { (cj, ci) };
                assert_ne!(sc, lc >> (long - short), "{ci:?} prefixes {cj:?}");
            }
        }
    }

    #[test]
    fn noise_t_is_a_complete_prefix_code() {
        kraft_is_one(&NOISE_T);
        is_prefix_free(&NOISE_T);
    }

    #[test]
    fn noise_f_is_a_complete_prefix_code() {
        kraft_is_one(&NOISE_F);
        is_prefix_free(&NOISE_F);
    }

    #[test]
    fn noiseb_t_is_a_complete_prefix_code() {
        kraft_is_one(&NOISEB_T);
        is_prefix_free(&NOISEB_T);
    }

    #[test]
    fn noiseb_f_is_a_complete_prefix_code() {
        kraft_is_one(&NOISEB_F);
        is_prefix_free(&NOISEB_F);
    }
}
