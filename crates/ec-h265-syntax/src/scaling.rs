//! Scaling lists: `scaling_list_data()` (7.3.4), the default lists of Tables 7-5
//! and 7-6, and the `ScalingFactor` each transform block is dequantised with (7.4.5).
//!
//! A list is stored as the coefficients a bitstream carries, `ScalingList[sizeId]
//! [matrixId][i]` in up-right diagonal order, plus the DC value of the 16x16 and
//! 32x32 lists. [`ScalingFactors`] turns those into a raster matrix per block size.
//!
//! The 32x32 chroma matrices (matrixId 1, 2, 4, 5) are not coded: they are only
//! used for 4:4:4, which this family does not decode, so they hold the defaults.

use ec_core::bitio::{BitReader, BitWriter};
use ec_core::error::{Error, Result};

use crate::ps::{Pps, Sps};

/// Table 7-6, intra 8x8 default, in up-right diagonal scan order (`i` = 0..63).
const DEFAULT_INTRA_8X8: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];

/// Table 7-6, inter 8x8 default, in up-right diagonal scan order (`i` = 0..63).
const DEFAULT_INTER_8X8: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];

/// The default list for `sizeId` (0 = 4x4 .. 3 = 32x32) and `matrixId`, as
/// Table 7-5 and Table 7-6 give it: flat 16 for 4x4, and for the larger sizes
/// the intra table for matrixId 0..2 (Y, Cb, Cr) and the inter table for 3..5.
fn default_coefficients(size_id: usize, matrix_id: usize) -> [u8; 64] {
    if size_id == 0 {
        [16; 64]
    } else if matrix_id < 3 {
        DEFAULT_INTRA_8X8
    } else {
        DEFAULT_INTER_8X8
    }
}

/// `matrixId` steps by 3 for 32x32 (only the luma and inter matrices are coded
/// there), and by 1 otherwise.
fn matrix_step(size_id: usize) -> usize {
    if size_id == 3 { 3 } else { 1 }
}

/// Number of coefficients coded for a list of `sizeId`: 16 for 4x4, else 64.
fn coded_count(size_id: usize) -> usize {
    if size_id == 0 { 16 } else { 64 }
}

/// One set of scaling lists, as a `scaling_list_data()` structure carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingList {
    /// `ScalingList[sizeId][matrixId][i]`, in up-right diagonal scan order. For
    /// sizeId 0 only the first 16 entries are meaningful.
    pub coefficients: [[[u8; 64]; 6]; 4],
    /// `scaling_list_dc_coef_minus8 + 8` for sizeId 2 and 3 (the DC of the
    /// upsampled matrix); 16 for sizeId 0 and 1, where there is none.
    pub dc: [[u8; 6]; 4],
}

impl Default for ScalingList {
    fn default() -> ScalingList {
        ScalingList::default_lists()
    }
}

impl ScalingList {
    /// The default lists of Tables 7-5 and 7-6, used when
    /// `scaling_list_enabled_flag` is set and no list data is sent.
    pub fn default_lists() -> ScalingList {
        let mut coefficients = [[[16u8; 64]; 6]; 4];
        for (size_id, sizes) in coefficients.iter_mut().enumerate() {
            for (matrix_id, matrix) in sizes.iter_mut().enumerate() {
                *matrix = default_coefficients(size_id, matrix_id);
            }
        }
        ScalingList {
            coefficients,
            dc: [[16; 6]; 4],
        }
    }

    /// Parse `scaling_list_data()` (7.3.4) into the lists it produces: a matrix
    /// is either sent explicitly, copied from an earlier one (`pred_matrix_id_delta`),
    /// or the default when that delta is 0.
    pub fn parse(r: &mut BitReader) -> Result<ScalingList> {
        let mut list = ScalingList::default_lists();
        for size_id in 0..4 {
            for matrix_id in (0..6).step_by(matrix_step(size_id)) {
                if r.read_bit()? {
                    // scaling_list_pred_mode_flag = 1: the coefficients are sent.
                    let mut next: i32 = 8;
                    if size_id > 1 {
                        let dc = r.read_se()?.saturating_add(8);
                        if !(1..=255).contains(&dc) {
                            return Err(Error::corrupt(format!(
                                "HEVC scaling list: scaling_list_dc_coef_minus8 = {}",
                                dc - 8
                            )));
                        }
                        list.dc[size_id][matrix_id] = dc as u8;
                        next = dc;
                    }
                    for coef in list.coefficients[size_id][matrix_id]
                        .iter_mut()
                        .take(coded_count(size_id))
                    {
                        let delta = r.read_se()?;
                        if !(-128..=127).contains(&delta) {
                            return Err(Error::corrupt(format!(
                                "HEVC scaling list: scaling_list_delta_coef = {delta}"
                            )));
                        }
                        next = (next + delta + 256) % 256;
                        *coef = next as u8;
                    }
                } else {
                    // scaling_list_pred_mode_flag = 0: copy from refMatrixId, or
                    // keep the default already in place when the delta is 0.
                    let delta = r.read_ue()? as usize;
                    let step = matrix_step(size_id);
                    if delta > matrix_id / step {
                        return Err(Error::corrupt(format!(
                            "HEVC scaling list: scaling_list_pred_matrix_id_delta = {delta} \
                             at sizeId {size_id}, matrixId {matrix_id}"
                        )));
                    }
                    if delta != 0 {
                        let ref_id = matrix_id - delta * step;
                        list.coefficients[size_id][matrix_id] = list.coefficients[size_id][ref_id];
                        list.dc[size_id][matrix_id] = list.dc[size_id][ref_id];
                    }
                }
            }
        }
        Ok(list)
    }

    /// Write `scaling_list_data()` with every coded matrix sent explicitly
    /// (`scaling_list_pred_mode_flag` = 1). Parsing the result gives back `self`
    /// (for the coded matrices; see the module note on 32x32 chroma).
    pub fn write(&self, w: &mut BitWriter) {
        for size_id in 0..4 {
            for matrix_id in (0..6).step_by(matrix_step(size_id)) {
                w.write_bit(true);
                let mut next: i32 = 8;
                if size_id > 1 {
                    let dc = i32::from(self.dc[size_id][matrix_id]);
                    w.write_se(dc - 8);
                    next = dc;
                }
                for &coef in self.coefficients[size_id][matrix_id]
                    .iter()
                    .take(coded_count(size_id))
                {
                    let coef = i32::from(coef);
                    // The delta wraps modulo 256 into -128..=127, which the parser undoes.
                    let mut delta = (coef - next).rem_euclid(256);
                    if delta > 127 {
                        delta -= 256;
                    }
                    w.write_se(delta);
                    next = coef;
                }
            }
        }
    }
}

/// The up-right diagonal scan of a `blk` x `blk` block (6.5.3), as `(x, y)` pairs.
fn diag_scan(blk: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(blk * blk);
    let (mut x, mut y) = (0isize, 0isize);
    while out.len() < blk * blk {
        while y >= 0 {
            if (x as usize) < blk && (y as usize) < blk {
                out.push((x as usize, y as usize));
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }
    out
}

/// `ScalingFactor` for every block size and matrixId (7.4.5), each as a raster
/// matrix: entry `y * size + x` is `m[x][y]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingFactors {
    factors: [[Vec<u8>; 6]; 4],
}

impl ScalingFactors {
    /// Expand a list set into the factor matrices.
    pub fn new(list: &ScalingList) -> ScalingFactors {
        ScalingFactors {
            factors: std::array::from_fn(|size_id| {
                std::array::from_fn(|matrix_id| derive(list, size_id, matrix_id))
            }),
        }
    }

    /// The factors a picture is dequantised with (7.4.2.2, 7.4.3.3): `None` when
    /// `scaling_list_enabled_flag` is 0 (flat 16). Otherwise the PPS list when the
    /// PPS sends one, else the SPS list, else the defaults.
    pub fn for_picture(sps: &Sps, pps: &Pps) -> Option<ScalingFactors> {
        ScalingFactors::for_lists(
            sps.scaling_list_enabled,
            sps.scaling_list.as_ref(),
            pps.scaling_list.as_ref(),
        )
    }

    /// [`ScalingFactors::for_picture`] over the bare fields: `enabled` is
    /// `scaling_list_enabled_flag`, and the two lists are the explicit data of the
    /// SPS and the PPS when each sends one.
    pub fn for_lists(
        enabled: bool,
        sps_list: Option<&ScalingList>,
        pps_list: Option<&ScalingList>,
    ) -> Option<ScalingFactors> {
        if !enabled {
            return None;
        }
        Some(match pps_list.or(sps_list) {
            Some(list) => ScalingFactors::new(list),
            None => ScalingFactors::new(&ScalingList::default_lists()),
        })
    }

    /// The raster factor matrix for a transform block of side `1 << log2_size`
    /// (2..=5) and `matrix_id` (0..=2 intra, 3..=5 inter).
    pub fn for_block(&self, log2_size: u32, matrix_id: usize) -> &[u8] {
        let size_id = (log2_size as usize).saturating_sub(2).min(3);
        &self.factors[size_id][matrix_id.min(5)]
    }
}

/// Derive the raster `ScalingFactor` for one sizeId and matrixId (7.4.5).
fn derive(list: &ScalingList, size_id: usize, matrix_id: usize) -> Vec<u8> {
    let size = 4usize << size_id;
    let mut m = vec![16u8; size * size];
    // The coded block is 4x4 for sizeId 0 and 8x8 otherwise; for 16x16 and 32x32
    // each coded entry covers a 2x2 or 4x4 run of the matrix.
    let (blk, rep) = match size_id {
        0 => (4, 1),
        1 => (8, 1),
        2 => (8, 2),
        _ => (8, 4),
    };
    // 7-xx: with ChromaArrayType 3 the 32x32 chroma matrices (1, 2, 4, 5) are the 16x16 lists
    // of those matrices, expanded by four, with the 16x16 DC.
    let (src_size, coefs_id) = if size_id == 3 && !matrix_id.is_multiple_of(3) {
        (2, matrix_id)
    } else {
        (size_id, matrix_id)
    };
    let coefs = &list.coefficients[src_size][coefs_id];
    for (i, (sx, sy)) in diag_scan(blk).into_iter().enumerate() {
        let value = coefs[i];
        for j in 0..rep {
            for k in 0..rep {
                m[(sy * rep + j) * size + sx * rep + k] = value;
            }
        }
    }
    if size_id >= 2 {
        m[0] = list.dc[src_size][coefs_id];
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 7-6 as HM writes it: the same defaults in raster order (row = y).
    const HM_INTRA_8X8: [u8; 64] = [
        16, 16, 16, 16, 17, 18, 21, 24, 16, 16, 16, 16, 17, 19, 22, 25, 16, 16, 17, 18, 20, 22, 25,
        29, 16, 16, 18, 21, 24, 27, 31, 36, 17, 17, 20, 24, 30, 35, 41, 47, 18, 19, 22, 27, 35, 44,
        54, 65, 21, 22, 25, 31, 41, 54, 70, 88, 24, 25, 29, 36, 47, 65, 88, 115,
    ];
    const HM_INTER_8X8: [u8; 64] = [
        16, 16, 16, 16, 17, 18, 20, 24, 16, 16, 16, 17, 18, 20, 24, 25, 16, 16, 17, 18, 20, 24, 25,
        28, 16, 17, 18, 20, 24, 25, 28, 33, 17, 18, 20, 24, 25, 28, 33, 41, 18, 20, 24, 25, 28, 33,
        41, 54, 20, 24, 25, 28, 33, 41, 54, 71, 24, 25, 28, 33, 41, 54, 71, 91,
    ];

    /// Write `scaling_list_data()` where each matrix is described by `entry`:
    /// `Some((dc, coefs))` is sent explicitly, `None` is predicted with delta 0.
    /// `Err(delta)` is a prediction from a reference matrix `delta` steps back.
    enum Entry {
        Explicit(Option<i32>, Vec<i32>),
        Pred(usize),
    }

    fn write_data(w: &mut BitWriter, entry: impl Fn(usize, usize) -> Entry) {
        for size_id in 0..4 {
            for matrix_id in (0..6).step_by(matrix_step(size_id)) {
                match entry(size_id, matrix_id) {
                    Entry::Explicit(dc, coefs) => {
                        w.write_bit(true);
                        let mut next = 8;
                        if size_id > 1 {
                            let dc = dc.expect("explicit 16x16/32x32 lists carry a DC");
                            w.write_se(dc - 8);
                            next = dc;
                        }
                        for coef in coefs {
                            w.write_se((coef - next + 128).rem_euclid(256) - 128);
                            next = coef;
                        }
                    }
                    Entry::Pred(delta) => {
                        w.write_bit(false);
                        w.write_ue(delta as u32);
                    }
                }
            }
        }
    }

    fn parse_bits(w: BitWriter) -> Result<ScalingList> {
        let bytes = w.into_bytes();
        ScalingList::parse(&mut BitReader::new(&bytes))
    }

    #[test]
    fn default_tables_are_table_7_6_in_diagonal_order() {
        // The HM raster tables, read back along the diagonal scan, must give the
        // spec's diagonal-order tables: that cross-checks both against each other.
        for (raster, spec) in [
            (HM_INTRA_8X8, DEFAULT_INTRA_8X8),
            (HM_INTER_8X8, DEFAULT_INTER_8X8),
        ] {
            let scanned: Vec<u8> = diag_scan(8)
                .into_iter()
                .map(|(x, y)| raster[y * 8 + x])
                .collect();
            assert_eq!(scanned, spec.to_vec());
        }
    }

    #[test]
    fn default_lists_are_flat_at_4x4_and_table_7_6_above() {
        let list = ScalingList::default_lists();
        assert!(list.coefficients[0].iter().all(|m| m[..16] == [16; 16]));
        assert_eq!(list.coefficients[1][0], DEFAULT_INTRA_8X8);
        assert_eq!(list.coefficients[2][5], DEFAULT_INTER_8X8);
        assert_eq!(list.coefficients[3][3], DEFAULT_INTER_8X8);
        assert_eq!(list.dc, [[16; 6]; 4]);
    }

    #[test]
    fn default_factors_match_the_hm_raster_matrices() {
        let factors = ScalingFactors::new(&ScalingList::default_lists());
        assert_eq!(factors.for_block(3, 0), &HM_INTRA_8X8[..]);
        assert_eq!(factors.for_block(3, 4), &HM_INTER_8X8[..]);
        assert!(factors.for_block(2, 1).iter().all(|&m| m == 16));
    }

    #[test]
    fn default_list_round_trips_through_write_and_parse() {
        let list = ScalingList::default_lists();
        let mut w = BitWriter::with_capacity(64);
        list.write(&mut w);
        assert_eq!(parse_bits(w).unwrap(), list);
    }

    #[test]
    fn custom_list_round_trips_through_write_and_parse() {
        let mut list = ScalingList::default_lists();
        for size_id in 0..4 {
            for matrix_id in (0..6).step_by(matrix_step(size_id)) {
                for (i, coef) in list.coefficients[size_id][matrix_id]
                    .iter_mut()
                    .take(coded_count(size_id))
                    .enumerate()
                {
                    // Spread the values over the whole 1..=255 range, with wraps.
                    *coef = (1 + (i * 37 + size_id * 11 + matrix_id * 5) % 255) as u8;
                }
                if size_id > 1 {
                    list.dc[size_id][matrix_id] = (1 + (size_id * 50 + matrix_id) % 255) as u8;
                }
            }
        }
        let mut w = BitWriter::with_capacity(64);
        list.write(&mut w);
        assert_eq!(parse_bits(w).unwrap(), list);
    }

    #[test]
    fn prediction_copies_the_named_reference_matrix() {
        // sizeId 0: matrix 0 explicit (coefficient i = 10 + i), matrix 1 copies it
        // (delta 1), matrix 2 takes the default (delta 0). Everything else defaults.
        let explicit: Vec<i32> = (0..16).map(|i| 10 + i).collect();
        let explicit_u8: Vec<u8> = explicit.iter().map(|&c| c as u8).collect();
        let mut w = BitWriter::with_capacity(64);
        write_data(&mut w, |size_id, matrix_id| match (size_id, matrix_id) {
            (0, 0) => Entry::Explicit(None, explicit.clone()),
            (0, 1) => Entry::Pred(1),
            (0, 2) => Entry::Pred(0),
            _ => Entry::Pred(0),
        });
        let list = parse_bits(w).unwrap();
        assert_eq!(&list.coefficients[0][0][..16], &explicit_u8[..]);
        assert_eq!(list.coefficients[0][1], list.coefficients[0][0]);
        assert_eq!(&list.coefficients[0][2][..16], &[16u8; 16][..]);
    }

    #[test]
    fn prediction_copies_the_dc_too_and_the_32x32_step_is_three() {
        // sizeId 2 matrix 0 has DC 40; matrix 1 predicts from it and inherits the DC.
        // sizeId 3 matrix 3 predicts from matrix 0 (delta 1 at step 3), so it inherits
        // matrix 0's explicit list and DC.
        let coefs: Vec<i32> = (0..64).map(|i| 20 + (i % 30)).collect();
        let mut w = BitWriter::with_capacity(64);
        write_data(&mut w, |size_id, matrix_id| match (size_id, matrix_id) {
            (2, 0) => Entry::Explicit(Some(40), coefs.clone()),
            (2, 1) => Entry::Pred(1),
            (3, 0) => Entry::Explicit(Some(60), coefs.clone()),
            (3, 3) => Entry::Pred(1),
            _ => Entry::Pred(0),
        });
        let list = parse_bits(w).unwrap();
        assert_eq!(list.dc[2][0], 40);
        assert_eq!(list.dc[2][1], 40);
        assert_eq!(list.coefficients[2][1], list.coefficients[2][0]);
        assert_eq!(list.dc[3][0], 60);
        assert_eq!(list.dc[3][3], 60);
        assert_eq!(list.coefficients[3][3], list.coefficients[3][0]);
    }

    #[test]
    fn a_prediction_reaching_before_matrix_zero_is_refused() {
        // sizeId 0, matrixId 0 with delta 1: there is no matrix 0 - 1.
        let mut w = BitWriter::with_capacity(16);
        w.write_bit(false);
        w.write_ue(1);
        assert!(parse_bits(w).is_err());
    }

    #[test]
    fn dc_upsamples_the_16x16_factor_at_the_origin_only() {
        let mut list = ScalingList::default_lists();
        list.coefficients[2][0][0] = 30;
        list.dc[2][0] = 99;
        let factors = ScalingFactors::new(&list);
        let m = factors.for_block(4, 0);
        assert_eq!(m[0], 99);
        assert_eq!(m[1], 30);
        assert_eq!(m[16], 30);
        assert_eq!(m[17], 30);
    }

    #[test]
    fn pps_list_overrides_sps_list_and_flat_when_disabled() {
        assert!(ScalingFactors::for_lists(false, None, None).is_none());
        let defaults = ScalingFactors::new(&ScalingList::default_lists());
        assert_eq!(ScalingFactors::for_lists(true, None, None), Some(defaults));
        let mut sps_list = ScalingList::default_lists();
        sps_list.coefficients[1][0][0] = 50;
        assert_eq!(
            ScalingFactors::for_lists(true, Some(&sps_list), None),
            Some(ScalingFactors::new(&sps_list))
        );
        let mut pps_list = ScalingList::default_lists();
        pps_list.coefficients[1][0][0] = 70;
        assert_eq!(
            ScalingFactors::for_lists(true, Some(&sps_list), Some(&pps_list)),
            Some(ScalingFactors::new(&pps_list))
        );
    }
}
