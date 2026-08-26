//! The CDFs a tile adapts as it is written.
//!
//! AV1's symbol decoder updates every non-literal CDF it reads (spec 8.3.2),
//! so an encoder that wants `disable_cdf_update` off has to keep the same
//! state and update it in the same order. What lives here is that state: one
//! owned copy of each table the tile writer touches, seeded from the defaults
//! in [`crate::cdf`].
//!
//! The tables are owned once, not once per use: two coefficient table sets
//! that share a table -- a 64x64 luma transform reads the 32x32 base-range
//! rows, and both luma sets read the same DC sign table -- share the *adapted*
//! table too, which is what the decoder does.

use crate::cdf;

/// Which of the four coefficient table sets a transform block is coded with.
#[derive(Clone, Copy)]
pub(crate) enum TxbSet {
    /// The 32x32 luma transform of a 32x32 block.
    Luma32,
    /// The 64x64 luma transform of a whole superblock, scanned as a 32x32.
    Luma64,
    /// The 16x16 transform of a chroma plane of a 32x32 block.
    Chroma16,
    /// The 32x32 transform of a chroma plane of a whole superblock.
    Chroma32,
}

/// The tables one transform block is coded with, borrowed from the state for
/// as long as that block is being written.
pub(crate) struct TxbTables<'a> {
    /// Side of the transform, in coefficients.
    pub side: usize,
    /// The all-zero flag, indexed by its own context.
    pub txb_skip: &'a mut [[u16; 3]],
    /// The end-of-block group, whose alphabet the transform size sets.
    pub eob_pt: &'a mut [u16],
    /// The top bit of the offset inside that group.
    pub eob_extra: &'a mut [[u16; 3]; 9],
    /// A coefficient's base level.
    pub base: &'a mut [[u16; 5]; 42],
    /// The base level of the last coefficient in the scan.
    pub base_eob: &'a mut [[u16; 4]; 4],
    /// The base-range tail above level two.
    pub br: &'a mut [[u16; 5]; 21],
    /// The sign of the DC.
    pub dc_sign: &'a mut [[u16; 3]; 3],
}

/// Every table a key frame's tile writer adapts.
pub(crate) struct Cdfs {
    /// The partition symbol of a 64x64 superblock.
    pub partition_w64: [[u16; 11]; 4],
    /// The partition symbol of a 32x32 block.
    pub partition_w32: [[u16; 11]; 4],
    /// Whether a block carries no residual at all.
    pub skip: [[u16; 3]; 3],
    /// The luma mode of a key frame's block, under the two neighbours' modes.
    pub kf_y_mode: [[[u16; 14]; 5]; 5],
    /// The chroma mode of a block too big to be offered chroma from luma.
    pub uv_mode_no_cfl: [[u16; 14]; 13],
    /// The chroma mode of a block that is offered it.
    pub uv_mode_cfl: [[u16; 15]; 13],
    /// The angle a directional mode is nudged by.
    pub angle_delta: [[u16; 8]; 8],
    /// The one-context all-zero flag of a 32x32 luma transform.
    pub txb_skip_luma_32: [[u16; 3]; 1],
    /// The same, for a 64x64 luma transform.
    pub txb_skip_luma_64: [[u16; 3]; 1],
    /// The all-zero flag of a 16x16 chroma transform.
    pub txb_skip_chroma_16: [[u16; 3]; 3],
    /// The all-zero flag of a 32x32 chroma transform.
    pub txb_skip_chroma_32: [[u16; 3]; 3],
    /// The end-of-block group of a luma transform of 1024 positions.
    pub eob_pt_1024_luma: [u16; 12],
    /// The same, for chroma.
    pub eob_pt_1024_chroma: [u16; 12],
    /// The end-of-block group of a 16x16 chroma transform.
    pub eob_pt_256_chroma: [u16; 10],
    /// The end-of-block offset bit, per table set.
    pub eob_extra_luma_32: [[u16; 3]; 9],
    /// The same, for a 64x64 luma transform.
    pub eob_extra_luma_64: [[u16; 3]; 9],
    /// The same, for a 16x16 chroma transform.
    pub eob_extra_chroma_16: [[u16; 3]; 9],
    /// The same, for a 32x32 chroma transform.
    pub eob_extra_chroma_32: [[u16; 3]; 9],
    /// The base level of a 32x32 luma coefficient.
    pub base_luma_32: [[u16; 5]; 42],
    /// The base level of a 64x64 luma coefficient.
    pub base_luma_64: [[u16; 5]; 42],
    /// The base level of a 16x16 chroma coefficient.
    pub base_chroma_16: [[u16; 5]; 42],
    /// The base level of a 32x32 chroma coefficient.
    pub base_chroma_32: [[u16; 5]; 42],
    /// The last coefficient's base level, per table set.
    pub base_eob_luma_32: [[u16; 4]; 4],
    /// The same, for a 64x64 luma transform.
    pub base_eob_luma_64: [[u16; 4]; 4],
    /// The same, for a 16x16 chroma transform.
    pub base_eob_chroma_16: [[u16; 4]; 4],
    /// The same, for a 32x32 chroma transform.
    pub base_eob_chroma_32: [[u16; 4]; 4],
    /// The base-range tail of a luma coefficient, which a 64x64 transform
    /// reads too because its index is clamped at the 32x32 size.
    pub br_luma_32: [[u16; 5]; 21],
    /// The base-range tail of a 16x16 chroma coefficient.
    pub br_chroma_16: [[u16; 5]; 21],
    /// The base-range tail of a 32x32 chroma coefficient.
    pub br_chroma_32: [[u16; 5]; 21],
    /// The DC sign of a luma coefficient, shared by both luma sets.
    pub dc_sign_luma: [[u16; 3]; 3],
    /// The DC sign of a chroma coefficient, shared by both chroma sets.
    pub dc_sign_chroma: [[u16; 3]; 3],
}

impl Cdfs {
    /// The defaults a key frame starts from (spec 8.4, `init_coeff_cdfs` and
    /// `init_non_coeff_cdfs`), for the one quantizer context whose tables this
    /// crate carries.
    pub fn new() -> Cdfs {
        Cdfs {
            partition_w64: cdf::PARTITION_W64,
            partition_w32: cdf::PARTITION_W32,
            skip: cdf::SKIP,
            kf_y_mode: cdf::KF_Y_MODE,
            uv_mode_no_cfl: cdf::UV_MODE_NO_CFL,
            uv_mode_cfl: cdf::UV_MODE_CFL,
            angle_delta: cdf::ANGLE_DELTA,
            txb_skip_luma_32: [cdf::TXB_SKIP_LUMA_32],
            txb_skip_luma_64: [cdf::TXB_SKIP_LUMA_64],
            txb_skip_chroma_16: cdf::TXB_SKIP_CHROMA_16,
            txb_skip_chroma_32: cdf::TXB_SKIP_CHROMA_32,
            eob_pt_1024_luma: cdf::EOB_PT_1024_LUMA,
            eob_pt_1024_chroma: cdf::EOB_PT_1024_CHROMA,
            eob_pt_256_chroma: cdf::EOB_PT_256_CHROMA,
            eob_extra_luma_32: cdf::EOB_EXTRA_LUMA_32,
            eob_extra_luma_64: cdf::EOB_EXTRA_LUMA_64,
            eob_extra_chroma_16: cdf::EOB_EXTRA_CHROMA_16,
            eob_extra_chroma_32: cdf::EOB_EXTRA_CHROMA_32,
            base_luma_32: cdf::COEFF_BASE_LUMA_32,
            base_luma_64: cdf::COEFF_BASE_LUMA_64,
            base_chroma_16: cdf::COEFF_BASE_CHROMA_16,
            base_chroma_32: cdf::COEFF_BASE_CHROMA_32,
            base_eob_luma_32: cdf::COEFF_BASE_EOB_LUMA_32,
            base_eob_luma_64: cdf::COEFF_BASE_EOB_LUMA_64,
            base_eob_chroma_16: cdf::COEFF_BASE_EOB_CHROMA_16,
            base_eob_chroma_32: cdf::COEFF_BASE_EOB_CHROMA_32,
            br_luma_32: cdf::COEFF_BR_LUMA_32,
            br_chroma_16: cdf::COEFF_BR_CHROMA_16,
            br_chroma_32: cdf::COEFF_BR_CHROMA_32,
            dc_sign_luma: cdf::DC_SIGN_LUMA,
            dc_sign_chroma: cdf::DC_SIGN_CHROMA,
        }
    }

    /// The tables one table set is coded with, borrowed out of the state.
    pub fn txb(&mut self, set: TxbSet) -> TxbTables<'_> {
        match set {
            TxbSet::Luma32 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_luma_32,
                eob_pt: &mut self.eob_pt_1024_luma,
                eob_extra: &mut self.eob_extra_luma_32,
                base: &mut self.base_luma_32,
                base_eob: &mut self.base_eob_luma_32,
                br: &mut self.br_luma_32,
                dc_sign: &mut self.dc_sign_luma,
            },
            TxbSet::Luma64 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_luma_64,
                eob_pt: &mut self.eob_pt_1024_luma,
                eob_extra: &mut self.eob_extra_luma_64,
                base: &mut self.base_luma_64,
                base_eob: &mut self.base_eob_luma_64,
                br: &mut self.br_luma_32,
                dc_sign: &mut self.dc_sign_luma,
            },
            TxbSet::Chroma16 => TxbTables {
                side: 16,
                txb_skip: &mut self.txb_skip_chroma_16,
                eob_pt: &mut self.eob_pt_256_chroma,
                eob_extra: &mut self.eob_extra_chroma_16,
                base: &mut self.base_chroma_16,
                base_eob: &mut self.base_eob_chroma_16,
                br: &mut self.br_chroma_16,
                dc_sign: &mut self.dc_sign_chroma,
            },
            TxbSet::Chroma32 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_chroma_32,
                eob_pt: &mut self.eob_pt_1024_chroma,
                eob_extra: &mut self.eob_extra_chroma_32,
                base: &mut self.base_chroma_32,
                base_eob: &mut self.base_eob_chroma_32,
                br: &mut self.br_chroma_32,
                dc_sign: &mut self.dc_sign_chroma,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64x64 luma transform reads the 32x32 base-range rows, so the two sets
    /// have to adapt the same table: a decoder keeps one copy, and an encoder
    /// that kept two would drift away from it the first time a level above two
    /// was coded.
    #[test]
    fn the_two_luma_sets_share_the_base_range_table() {
        let mut cdfs = Cdfs::new();
        cdfs.txb(TxbSet::Luma64).br[0][0] = 1;
        assert_eq!(cdfs.txb(TxbSet::Luma32).br[0][0], 1);
    }

    /// Both luma sets read one DC sign table, and both chroma sets another;
    /// what a luma block adapts must not reach the chroma one.
    #[test]
    fn the_sign_tables_are_shared_within_a_plane_type_and_not_across() {
        let mut cdfs = Cdfs::new();
        cdfs.txb(TxbSet::Luma32).dc_sign[1][0] = 1;
        cdfs.txb(TxbSet::Chroma16).dc_sign[2][0] = 2;
        assert_eq!(cdfs.txb(TxbSet::Luma64).dc_sign[1][0], 1);
        assert_eq!(cdfs.txb(TxbSet::Chroma32).dc_sign[2][0], 2);
        assert_ne!(cdfs.txb(TxbSet::Chroma32).dc_sign[1][0], 1);
        assert_ne!(cdfs.txb(TxbSet::Luma64).dc_sign[2][0], 2);
    }

    /// The end-of-block table a 1024-position transform reads is shared by both
    /// luma sets and separate from chroma's, which is the same hazard one table
    /// further along.
    #[test]
    fn the_luma_end_of_block_table_is_shared_and_chromas_is_not() {
        let mut cdfs = Cdfs::new();
        cdfs.txb(TxbSet::Luma32).eob_pt[0] = 7;
        assert_eq!(cdfs.txb(TxbSet::Luma64).eob_pt[0], 7);
        assert_ne!(cdfs.txb(TxbSet::Chroma32).eob_pt[0], 7);
    }

    /// Every set starts from the spec's defaults, so a tile that adapts nothing
    /// writes exactly what a non-adapting one does.
    #[test]
    fn the_state_starts_at_the_defaults() {
        let mut cdfs = Cdfs::new();
        assert_eq!(cdfs.txb(TxbSet::Luma32).base[3], cdf::COEFF_BASE_LUMA_32[3]);
        assert_eq!(
            cdfs.txb(TxbSet::Chroma16).eob_extra[2],
            cdf::EOB_EXTRA_CHROMA_16[2]
        );
        assert_eq!(cdfs.partition_w64[2], cdf::PARTITION_W64[2]);
    }
}
