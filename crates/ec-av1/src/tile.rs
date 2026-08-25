//! Tile payload writer (spec 5.11), for the one block coding the encoder can
//! produce today: a key frame whose every superblock is a single 64x64
//! DC-predicted block with no residual.
//!
//! That frame decodes to a flat mid-grey picture — every sample is the value a
//! DC prediction with no neighbours produces — which is what makes it a usable
//! gate: any desync between this writer and a real decoder shows up as a
//! decode failure or a sample that is not mid-grey, with no metric in the way.
//! It is the skeleton the block modes, transform sizes and coefficients hang
//! off as they arrive.

use ec_core::{Error, Result};

use crate::cdf;
use crate::msac::SymbolEncoder;

/// `PARTITION_NONE` (spec 6.10.4): the whole block, undivided.
const PARTITION_NONE: usize = 0;
/// `PARTITION_SPLIT` (spec 6.10.4): the block cut into four quadrants.
const PARTITION_SPLIT: usize = 3;

/// The partition types whose probability mass the split-or-horizontal flag of
/// a superblock hanging off the bottom of the frame gathers (spec 9.3,
/// `partition_gather_vert_alike`): the flag says split, so everything vertical
/// or split lands on it.
const VERT_ALIKE: [usize; 6] = [2, PARTITION_SPLIT, 4, 6, 7, 9];

/// The same for a superblock hanging off the right-hand edge
/// (`partition_gather_horz_alike`).
const HORZ_ALIKE: [usize; 6] = [1, PARTITION_SPLIT, 4, 5, 6, 8];

/// Mode-info units across a 32x32 block.
const BLOCK_MI: u32 = SB_MI / 2;
/// `DC_PRED` (spec 6.10.2), as both the luma and the chroma mode.
const DC_PRED: usize = 0;

/// `V_PRED` (spec 6.10.2), the first of the eight directional intra modes and
/// so the first mode that carries an angle delta.
const V_PRED: usize = 1;

/// `H_PRED`, which predicts every row from the column to the block's left.
#[cfg(test)]
const H_PRED: usize = 2;

/// The last directional mode, `D67_PRED`.
const D67_PRED: usize = 8;

/// The number of intra modes a key frame's luma block chooses from,
/// `INTRA_MODES` (spec 3).
const INTRA_MODES: usize = 13;

/// `Intra_Mode_Context` (spec 9.3): the five-way class each intra mode puts
/// its neighbours in when they pick the CDF for the next block's mode.
const INTRA_MODE_CTX: [usize; INTRA_MODES] = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0];

/// The symbol an angle delta of zero codes as: the alphabet runs from -3 to
/// +3, so `MAX_ANGLE_DELTA` is the middle of it.
const ANGLE_DELTA_ZERO: usize = 3;
/// Side of a superblock in 4x4 mode-info units when 128x128 superblocks are off.
const SB_MI: u32 = 16;

/// `NUM_BASE_LEVELS` (spec 3): levels above this carry a base-range tail.
const NUM_BASE_LEVELS: i32 = 2;
/// `COEFF_BASE_RANGE` (spec 3): how far the base-range tail reaches before the
/// Golomb tail takes over.
const COEFF_BASE_RANGE: i32 = 12;
/// `BR_CDF_SIZE - 1` (spec 3): the largest increment one base-range symbol
/// carries.
const BR_STEP: i32 = 3;
/// The largest level the base and base-range syntax carry between them.
/// Anything above it is written as a Golomb tail on top of this.
const MAX_BR_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;
/// The largest level this writer codes. The Golomb tail is a length in unary
/// followed by that many bits, and the spec's decoder reads at most twenty of
/// each, so a level a decoder cannot read back is refused rather than written.
const MAX_LEVEL: i32 = MAX_BR_LEVEL + (1 << 19);
/// The q-context band whose default CDFs [`crate::cdf`] carries.
const Q_CTX_2: std::ops::RangeInclusive<u8> = 61..=120;

/// Both writers here code whole superblocks only: a partial one forces the
/// partition syntax down the block tree, which they do not code yet.
/// The probability the CDF gives one symbol on its own.
fn element_prob(cdf: &[u16], element: usize) -> u16 {
    cdf[element] - if element > 0 { cdf[element - 1] } else { 0 }
}

/// The two-symbol CDF a partial superblock's partition flag is coded with: the
/// mass of the listed partition types becomes the probability of a split, and
/// the rest is the one partition the frame edge still allows.
fn gather(cdf: &[u16], elements: [usize; 6]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

fn check_superblocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(SB_MI)
        || !mi_rows.is_multiple_of(SB_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a key frame is written only for frames that are a whole number \
             of 64x64 superblocks",
        ));
    }
    Ok(())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// skipped 64x64 DC-predicted block.
///
/// `mi_cols` and `mi_rows` are the frame's dimensions in 4x4 mode-info units,
/// as the frame header carries them.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks:
/// a partial superblock forces the partition syntax down the block tree, which
/// this writer does not code yet.
pub fn flat_key_frame_tile(mi_cols: u32, mi_rows: u32) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);

    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        for c in 0..sb_cols {
            // decode_partition (spec 5.11.4). Every neighbour is a 64x64 block,
            // whose stored partition context has a zero bit at this block
            // size, so the context is 0 wherever the block sits.
            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // intra_frame_mode_info (spec 5.11.16). Segmentation, delta q,
            // delta lf, palette, filter intra and intrabc are all off in the
            // frame header, and a skipped block codes no CDEF index, so the
            // block is three symbols: the skip flag and the two modes.
            let skip_ctx = usize::from(r > 0) + usize::from(c > 0);
            enc.symbol_fixed(1, &cdf::SKIP[skip_ctx]);
            // Both neighbours are DC-predicted, and an unavailable neighbour
            // counts as DC too, so both mode contexts are 0.
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            // Chroma from luma is only offered up to 32x32, so the CFL-free
            // table is the one a 64x64 block reads.
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            // read_block_tx_size codes nothing while the frame's tx_mode is
            // TX_MODE_LARGEST, and a skipped block has no residual, so the
            // block ends here.
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// 64x64 DC-predicted block carrying one luma DC coefficient of `dc_level` and
/// no chroma residual.
///
/// `dc_level` is a quantised level, not a sample value: the decoder multiplies
/// it by the frame's DC quantiser and inverse-transforms it over the whole
/// block, so the picture it makes is a flat grey some distance either side of
/// the mid-grey a zero level gives. `base_q_idx` is the frame header's, and
/// picks the coefficient CDFs.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], which this is the one-level case of.
pub fn dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    dc_level: i32,
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let blocks = ((mi_cols / SB_MI) * (mi_rows / SB_MI)) as usize;
    dc_key_frame_tile_levels(mi_cols, mi_rows, base_q_idx, &vec![dc_level; blocks])
}

/// Writes the payload of a one-tile key frame carrying one luma DC coefficient
/// per superblock, `levels` giving them in the raster order the superblocks are
/// coded in.
///
/// Each superblock decodes to a flat block of its own grey, so a frame written
/// here is a grid of greys the caller chooses — which is what makes the sign
/// context observable: a block's sign context is read off its coded neighbours,
/// and a frame whose levels differ in sign exercises the three-way split that a
/// single-sign frame cannot reach.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks,
/// when `levels` does not carry exactly one level per superblock, when a level
/// is outside the range the base and base-range syntax carry (`-14..=14`
/// without zero), or when `base_q_idx` is outside the q-context band whose
/// default CDFs this crate carries.
pub fn dc_key_frame_tile_levels(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    check_levels(levels, (sb_cols * sb_rows) as usize, base_q_idx)?;

    // The sign of the DC each coded block left behind, for the two neighbours
    // the sign context is read from: one row of them above, and the block to
    // the left, which is dropped at the start of every superblock row the way a
    // decoder clears its left context there.
    let mut above: Vec<Option<bool>> = vec![None; sb_cols as usize];
    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        let mut left: Option<bool> = None;
        for c in 0..sb_cols {
            let dc_level = levels[(r * sb_cols + c) as usize];
            let negative = dc_level < 0;

            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // Nothing is skipped now, so every neighbour's skip flag is 0 and
            // the skip context stays 0 across the frame.
            enc.symbol_fixed(0, &cdf::SKIP[0]);
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            write_dc_coeffs(
                &mut enc,
                dc_level,
                dc_sign_ctx(above[c as usize], left),
                &cdf::TXB_SKIP_LUMA_64,
                &cdf::COEFF_BASE_EOB_LUMA_64_DC,
            );

            // Both chroma transform blocks are all-zero. Their planes carry no
            // coded coefficient anywhere in the frame, so the neighbour halves
            // of their context stay 0 and only the offset for a transform block
            // that covers its whole plane block is left: context 7.
            enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_32_NONE);
            enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_32_NONE);

            above[c as usize] = Some(negative);
            left = Some(negative);
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into four 32x32 DC-predicted blocks, each carrying one luma DC
/// coefficient and no chroma residual.
///
/// `levels` gives one level per 32x32 block in raster order across the frame,
/// so the grid is twice as wide and twice as tall as the superblock grid. The
/// blocks are coded in the z-order a superblock's split walks, but every
/// block's neighbours above and to its left are coded before it either way, so
/// the picture reads in raster order.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], with `levels` sized for the 32x32 grid.
pub fn split_dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    let (cols, rows) = (sb_cols * 2, sb_rows * 2);
    check_levels(levels, (cols * rows) as usize, base_q_idx)?;

    let mut above: Vec<Option<bool>> = vec![None; cols as usize];
    let mut left: Vec<Option<bool>> = vec![None; rows as usize];

    let mut enc = SymbolEncoder::new();
    for sb_r in 0..sb_rows {
        // A decoder clears its left context at the start of every superblock
        // row, and so does the partition context the 64x64 symbol reads.
        left.iter_mut().for_each(|l| *l = None);
        for sb_c in 0..sb_cols {
            // The partition context of a 64x64 block reads the bit its
            // neighbours' block size sets at this depth: a 32x32 neighbour sets
            // it, and an uncoded one leaves it clear, so the context is just
            // which neighbours exist. The 32x32 blocks below read a bit their
            // own size leaves clear, so their context is 0 throughout.
            // Every superblock here is split, so an existing neighbour always
            // sets that bit.
            let ctx = 2 * usize::from(sb_c > 0) + usize::from(sb_r > 0);
            enc.symbol_fixed(PARTITION_SPLIT, &cdf::PARTITION_W64[ctx]);

            for quadrant in 0..4 {
                let (r, c) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                let dc_level = levels[(r * cols + c) as usize];
                let negative = dc_level < 0;

                enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W32[0]);
                enc.symbol_fixed(0, &cdf::SKIP[0]);
                enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
                // Chroma from luma is offered up to 32x32, so the block reads
                // the wider table even though it does not take the mode.
                enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_CFL[DC_PRED]);

                write_dc_coeffs(
                    &mut enc,
                    dc_level,
                    dc_sign_ctx(above[c as usize], left[r as usize]),
                    &cdf::TXB_SKIP_LUMA_32,
                    &cdf::COEFF_BASE_EOB_LUMA_32[0],
                );
                enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_16[0]);
                enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_16[0]);

                above[c as usize] = Some(negative);
                left[r as usize] = Some(negative);
            }
        }
    }
    Ok(enc.finish())
}

/// One quantised coefficient of a transform block.
///
/// `row` and `col` are its position in the transform it belongs to — 32x32 for
/// luma, 16x16 for each chroma plane — not in the picture: a coefficient at
/// row 0 varies along the picture's width and one at column 0 along its
/// height, and the coefficient at the origin is the DC the block's average
/// sample value rides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coeff {
    /// The coefficient's row in the transform.
    pub row: u8,
    /// The coefficient's column in the transform.
    pub col: u8,
    /// Its quantised level, which the base and base-range syntax carry for
    /// magnitudes up to [`MAX_LEVEL`].
    pub level: i32,
}

/// The coefficients of one coded block: a 32x32 luma transform and, at 4:2:0,
/// the 16x16 transform each chroma plane covers the same area with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockCoeffs {
    /// The luma transform's coefficients.
    pub luma: Vec<Coeff>,
    /// The U plane's.
    pub u: Vec<Coeff>,
    /// The V plane's.
    pub v: Vec<Coeff>,
    /// The luma intra mode the block is predicted with, one of the thirteen
    /// modes a key frame codes (`DC_PRED` is zero, which is what
    /// [`Default`] and a plain coefficient list give). Chroma stays on
    /// `DC_PRED`.
    pub mode: u8,
}

impl From<Vec<Coeff>> for BlockCoeffs {
    /// A block that codes luma only.
    fn from(luma: Vec<Coeff>) -> Self {
        Self {
            luma,
            ..Self::default()
        }
    }
}

/// Side of the luma transform the coefficient writer codes.
const TX32: usize = 32;
/// Side of the chroma transform beside it at 4:2:0.
const TX16: usize = 16;

/// The shapes and default CDFs one plane's transform blocks are coded with.
/// The two differ in every coefficient table because the CDFs are indexed by
/// plane type and transform size, and in the end-of-block alphabet because a
/// 16x16 transform has a quarter of the positions to point at.
struct TxbCoding {
    /// Side of the transform, in coefficients.
    side: usize,
    /// The all-zero flag, indexed by its own context.
    txb_skip: &'static [[u16; 3]],
    /// The end-of-block group, whose alphabet the transform size sets.
    eob_pt: &'static [u16],
    /// The top bit of the offset inside that group.
    eob_extra: &'static [[u16; 3]; 9],
    /// A coefficient's base level.
    base: &'static [[u16; 5]; 42],
    /// The base level of the last coefficient in the scan.
    base_eob: &'static [[u16; 4]; 4],
    /// The base-range tail above level two.
    br: &'static [[u16; 5]; 21],
    /// The sign of the DC.
    dc_sign: &'static [[u16; 3]; 3],
}

/// The luma all-zero flag has one context here: the transform covers its whole
/// block, which fixes it at zero.
const TXB_SKIP_LUMA_32_CTX: [[u16; 3]; 1] = [cdf::TXB_SKIP_LUMA_32];

/// The 32x32 luma transform of a 32x32 block.
const LUMA_32: TxbCoding = TxbCoding {
    side: TX32,
    txb_skip: &TXB_SKIP_LUMA_32_CTX,
    eob_pt: &cdf::EOB_PT_1024_LUMA,
    eob_extra: &cdf::EOB_EXTRA_LUMA_32,
    base: &cdf::COEFF_BASE_LUMA_32,
    base_eob: &cdf::COEFF_BASE_EOB_LUMA_32,
    br: &cdf::COEFF_BR_LUMA_32,
    dc_sign: &cdf::DC_SIGN_LUMA,
};

/// The 16x16 transform of either chroma plane of that block at 4:2:0.
const CHROMA_16: TxbCoding = TxbCoding {
    side: TX16,
    txb_skip: &cdf::TXB_SKIP_CHROMA_16,
    eob_pt: &cdf::EOB_PT_256_CHROMA,
    eob_extra: &cdf::EOB_EXTRA_CHROMA_16,
    base: &cdf::COEFF_BASE_CHROMA_16,
    base_eob: &cdf::COEFF_BASE_EOB_CHROMA_16,
    br: &cdf::COEFF_BR_CHROMA_16,
    dc_sign: &cdf::DC_SIGN_CHROMA,
};

/// The luma all-zero flag of a 64x64 transform, whose one context is fixed the
/// way [`TXB_SKIP_LUMA_32_CTX`] is.
const TXB_SKIP_LUMA_64_CTX: [[u16; 3]; 1] = [cdf::TXB_SKIP_LUMA_64];

/// The 64x64 luma transform of a whole superblock. Only its top-left 32x32
/// carries coefficients, so it is scanned and its end-of-block position read
/// as a 32x32 transform is; what it does not share with [`LUMA_32`] is every
/// coefficient CDF, which is indexed by the transform size.
const LUMA_64: TxbCoding = TxbCoding {
    side: TX32,
    txb_skip: &TXB_SKIP_LUMA_64_CTX,
    eob_pt: &cdf::EOB_PT_1024_LUMA,
    eob_extra: &cdf::EOB_EXTRA_LUMA_64,
    base: &cdf::COEFF_BASE_LUMA_64,
    base_eob: &cdf::COEFF_BASE_EOB_LUMA_64,
    // The base-range tail is the one table a 64x64 transform does not have of
    // its own: the index is clamped at the 32x32 size, so it reads that row.
    br: &cdf::COEFF_BR_LUMA_32,
    dc_sign: &cdf::DC_SIGN_LUMA,
};

/// The 32x32 transform of either chroma plane of that superblock at 4:2:0.
const CHROMA_32: TxbCoding = TxbCoding {
    side: TX32,
    txb_skip: &cdf::TXB_SKIP_CHROMA_32,
    eob_pt: &cdf::EOB_PT_1024_CHROMA,
    eob_extra: &cdf::EOB_EXTRA_CHROMA_32,
    base: &cdf::COEFF_BASE_CHROMA_32,
    base_eob: &cdf::COEFF_BASE_EOB_CHROMA_32,
    br: &cdf::COEFF_BR_CHROMA_32,
    dc_sign: &cdf::DC_SIGN_CHROMA,
};

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC.
#[derive(Clone, Copy, Default)]
struct Neighbour {
    /// Whether the plane's transform block carried a coefficient.
    coded: bool,
    /// The sign of its DC, absent when the DC itself is zero.
    dc: Option<bool>,
}

/// Rejects a frame the 32x32 block grid cannot tile.
///
/// The frame need not be a whole number of superblocks — a superblock at the
/// right-hand or bottom edge may be half outside it — but every 32x32 block
/// that is coded has to be wholly inside, because a block that hangs over the
/// edge has to be coded as a rectangle this writer has no transform for.
fn check_blocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(BLOCK_MI)
        || !mi_rows.is_multiple_of(BLOCK_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame is written only for frames that are a \
             whole number of 32x32 blocks",
        ));
    }
    Ok(())
}

/// The coefficients of one superblock: either one 64x64 block covering it, or
/// the four 32x32 blocks it is split into.
///
/// A superblock at the right-hand or bottom edge of the frame may be half
/// outside it, and such a superblock cannot be left whole — the spec has no
/// partition that keeps a block outside the frame — so it must be
/// [`Split`](Superblock::Split), and carries only the quadrants that are
/// inside, in raster order among themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Superblock {
    /// One 64x64 block, whose luma transform is 64x64 — of which only the
    /// top-left 32x32 carries coefficients — and whose chroma transforms are
    /// 32x32 each at 4:2:0.
    Whole(BlockCoeffs),
    /// The 32x32 blocks the superblock is split into, each with a 32x32 luma
    /// transform and a 16x16 transform per chroma plane.
    Split(Vec<BlockCoeffs>),
}

/// Writes the payload of a one-tile key frame built from `superblocks`, each
/// either one 64x64 block or the 32x32 blocks it splits into.
///
/// `superblocks` gives one entry per superblock in raster order across the
/// frame. Every block is DC-predicted unless its [`BlockCoeffs::mode`] says
/// otherwise, and carries the coefficients its lists give; the coefficients
/// may sit anywhere in their transform, so a block is a picture rather than a
/// flat grey.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 32x32 blocks, when
/// `superblocks` does not carry exactly one entry per superblock or an entry
/// does not carry one block per quadrant inside the frame, when a superblock
/// that is half outside the frame is left whole, when a block names an intra
/// mode a key frame does not code, when a coefficient sits outside its
/// transform, repeats a position, carries a zero level or one wider than the
/// Golomb tail reaches, or when `base_q_idx` is outside the q-context band
/// whose default CDFs this crate carries.
pub fn sb_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    superblocks: &[Superblock],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    let (cols, rows) = (mi_cols / BLOCK_MI, mi_rows / BLOCK_MI);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    if superblocks.len() != (sb_cols * sb_rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one entry per superblock",
        ));
    }
    let bad_mode = superblocks
        .iter()
        .flat_map(|sb| match sb {
            Superblock::Whole(b) => std::slice::from_ref(b),
            Superblock::Split(b) => b.as_slice(),
        })
        .find(|b| usize::from(b.mode) >= INTRA_MODES);
    if let Some(bad) = bad_mode {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen a key frame codes",
                bad.mode
            ),
        ));
    }
    if !Q_CTX_2.contains(&base_q_idx) {
        return Err(Error::unsupported(
            "AV1 tile",
            "the coefficient CDFs of only one q context are known, so \
             base_q_idx must be 61..=120",
        ));
    }

    let split_planes = [&LUMA_32, &CHROMA_16, &CHROMA_16];
    let whole_planes = [&LUMA_64, &CHROMA_32, &CHROMA_32];
    let scans = [default_scan(TX32), default_scan(TX16)];
    // What the blocks above and to the left of the one being coded left
    // behind, per plane, one entry per 32x32 column and row. The left column
    // is reset at every superblock row because a tile starts each row with no
    // left neighbour.
    let mut above = vec![[Neighbour::default(); 3]; cols as usize];
    let mut left = vec![[Neighbour::default(); 3]; rows as usize];
    // The luma intra mode of those same neighbours, which picks the CDF the
    // next block's mode is coded with. A block outside the tile reads as
    // `DC_PRED`, which is what these start and are reset to.
    let mut above_mode = vec![DC_PRED; cols as usize];
    let mut left_mode = vec![DC_PRED; rows as usize];
    // Whether those neighbours were split, which is what the superblock's
    // partition symbol reads: the context is bit three of the neighbour's
    // entry in the spec's partition-context table, and of the sizes a
    // superblock can leave behind only a 32x32 block sets it. A neighbour
    // outside the tile reads as an unsplit one, which is what these start and
    // are reset to.
    let mut above_split = vec![false; cols as usize];
    let mut left_split = vec![false; rows as usize];

    let mut enc = SymbolEncoder::new();
    for sb_r in 0..sb_rows {
        left.iter_mut().for_each(|l| *l = Default::default());
        left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        left_split.iter_mut().for_each(|l| *l = false);
        for sb_c in 0..sb_cols {
            let (r0, c0) = (sb_r as usize * 2, sb_c as usize * 2);
            let ctx = 2 * usize::from(left_split[r0]) + usize::from(above_split[c0]);
            // A superblock whose bottom or right half is outside the frame
            // cannot be left unsplit, so the decoder reads a flag instead of
            // the partition symbol — and reads nothing at all when both halves
            // are outside, where the split is the only partition left.
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // The quadrants of this superblock that are inside the frame, as
            // positions in the 32x32 block grid.
            let quadrants: Vec<(usize, usize)> = (0..4)
                .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                .filter(|&(r, c)| r < rows && c < cols)
                .map(|(r, c)| (r as usize, c as usize))
                .collect();

            match &superblocks[(sb_r * sb_cols + sb_c) as usize] {
                Superblock::Whole(block) => {
                    if !has_cols || !has_rows {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a superblock that is half outside the frame cannot be left whole",
                        ));
                    }
                    enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[ctx]);
                    let mode = write_intra_mode(
                        &mut enc,
                        block,
                        above_mode[c0],
                        left_mode[r0],
                        // A 64x64 block is too big to be offered chroma from
                        // luma, so its chroma mode reads the table without it.
                        &cdf::UV_MODE_NO_CFL[usize::from(block.mode)],
                    );
                    let grids = [
                        level_grid(&block.luma, TX32)?,
                        level_grid(&block.u, TX32)?,
                        level_grid(&block.v, TX32)?,
                    ];
                    write_block_planes(
                        &mut enc,
                        &whole_planes,
                        &grids,
                        &[&scans[0], &scans[0], &scans[0]],
                        &above[c0],
                        &left[r0],
                    );
                    // The block covers two columns and two rows of the 32x32
                    // grid, so both of each read it as their neighbour.
                    for (r, c) in quadrants {
                        for plane in 0..3 {
                            let state = neighbour_state(&grids[plane]);
                            above[c][plane] = state;
                            left[r][plane] = state;
                        }
                        above_mode[c] = mode;
                        left_mode[r] = mode;
                        above_split[c] = false;
                        left_split[r] = false;
                    }
                }
                Superblock::Split(blocks) => {
                    match (has_cols, has_rows) {
                        (true, true) => enc.symbol_fixed(PARTITION_SPLIT, &cdf::PARTITION_W64[ctx]),
                        (true, false) => {
                            enc.symbol_fixed(1, &gather(&cdf::PARTITION_W64[ctx], VERT_ALIKE));
                        }
                        (false, true) => {
                            enc.symbol_fixed(1, &gather(&cdf::PARTITION_W64[ctx], HORZ_ALIKE));
                        }
                        (false, false) => {}
                    }
                    if blocks.len() != quadrants.len() {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a split superblock needs one block per quadrant inside the frame",
                        ));
                    }
                    for (block, (r, c)) in blocks.iter().zip(quadrants) {
                        enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W32[0]);
                        let mode = write_intra_mode(
                            &mut enc,
                            block,
                            above_mode[c],
                            left_mode[r],
                            &cdf::UV_MODE_CFL[usize::from(block.mode)],
                        );
                        let grids = [
                            level_grid(&block.luma, TX32)?,
                            level_grid(&block.u, TX16)?,
                            level_grid(&block.v, TX16)?,
                        ];
                        write_block_planes(
                            &mut enc,
                            &split_planes,
                            &grids,
                            &[&scans[0], &scans[1], &scans[1]],
                            &above[c],
                            &left[r],
                        );
                        for plane in 0..3 {
                            let state = neighbour_state(&grids[plane]);
                            above[c][plane] = state;
                            left[r][plane] = state;
                        }
                        above_mode[c] = mode;
                        left_mode[r] = mode;
                        above_split[c] = true;
                        left_split[r] = true;
                    }
                }
            }
        }
    }
    Ok(enc.finish())
}

/// Writes everything a key frame's block carries before its coefficients: the
/// skip flag, its luma intra mode against the CDF its neighbours' modes pick,
/// the angle a directional mode is steered by, and its chroma mode. Hands back
/// the luma mode, which is what the blocks beside it read.
fn write_intra_mode(
    enc: &mut SymbolEncoder,
    block: &BlockCoeffs,
    above_mode: usize,
    left_mode: usize,
    uv_cdf: &[u16],
) -> usize {
    let mode = usize::from(block.mode);
    enc.symbol_fixed(0, &cdf::SKIP[0]);
    enc.symbol_fixed(
        mode,
        &cdf::KF_Y_MODE[INTRA_MODE_CTX[above_mode]][INTRA_MODE_CTX[left_mode]],
    );
    if (V_PRED..=D67_PRED).contains(&mode) {
        enc.symbol_fixed(ANGLE_DELTA_ZERO, &cdf::ANGLE_DELTA[mode - V_PRED]);
    }
    enc.symbol_fixed(DC_PRED, uv_cdf);
    mode
}

/// Writes the three transform blocks of one coded block, in the order a
/// decoder reads them.
fn write_block_planes(
    enc: &mut SymbolEncoder,
    planes: &[&TxbCoding; 3],
    grids: &[Vec<i32>; 3],
    scans: &[&Vec<u16>; 3],
    above: &[Neighbour; 3],
    left: &[Neighbour; 3],
) {
    for (plane, grid) in grids.iter().enumerate() {
        // Luma's transform covers its whole block, which fixes the all-zero
        // flag's context at zero; a chroma transform reads whether its
        // neighbours coded anything, on top of the offset the chroma tables
        // start at.
        let skip_ctx = if plane == 0 {
            0
        } else {
            usize::from(above[plane].coded) + usize::from(left[plane].coded)
        };
        write_coeffs(
            enc,
            planes[plane],
            grid,
            scans[plane],
            skip_ctx,
            dc_sign_ctx(above[plane].dc, left[plane].dc),
        );
    }
}

/// What a coded transform block leaves behind for the blocks that read it as a
/// neighbour.
fn neighbour_state(grid: &[i32]) -> Neighbour {
    Neighbour {
        coded: grid.iter().any(|&l| l != 0),
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into 32x32 blocks, each carrying the coefficients `blocks` gives it.
///
/// `blocks` gives one coefficient set per 32x32 block in raster order across
/// the frame. This is [`sb_coeff_key_frame_tile`] with every superblock split.
///
/// # Errors
/// As [`sb_coeff_key_frame_tile`], with `blocks` sized for the 32x32 grid.
pub fn split_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[BlockCoeffs],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    let (cols, rows) = (mi_cols / BLOCK_MI, mi_rows / BLOCK_MI);
    if blocks.len() != (cols * rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one coefficient set per 32x32 block",
        ));
    }
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let superblocks: Vec<Superblock> = (0..sb_rows)
        .flat_map(|sb_r| (0..sb_cols).map(move |sb_c| (sb_r, sb_c)))
        .map(|(sb_r, sb_c)| {
            Superblock::Split(
                (0..4)
                    .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                    .filter(|&(r, c)| r < rows && c < cols)
                    .map(|(r, c)| blocks[(r * cols + c) as usize].clone())
                    .collect(),
            )
        })
        .collect();
    sb_coeff_key_frame_tile(mi_cols, mi_rows, base_q_idx, &superblocks)
}

/// Lays one plane's coefficient list out over its transform, rejecting the
/// positions and levels the writer does not code.
fn level_grid(coeffs: &[Coeff], side: usize) -> Result<Vec<i32>> {
    let mut grid = vec![0i32; side * side];
    for coeff in coeffs {
        if usize::from(coeff.row) >= side || usize::from(coeff.col) >= side {
            return Err(Error::unsupported(
                "AV1 tile",
                "a coefficient sits outside the transform of its plane",
            ));
        }
        if coeff.level == 0 || coeff.level.abs() > MAX_LEVEL {
            return Err(Error::unsupported(
                "AV1 tile",
                format!(
                    "coefficients are written for levels -{MAX_LEVEL}..={MAX_LEVEL} without zero"
                ),
            ));
        }
        let pos = usize::from(coeff.row) * side + usize::from(coeff.col);
        if grid[pos] != 0 {
            return Err(Error::unsupported(
                "AV1 tile",
                "two coefficients of one block share a position",
            ));
        }
        grid[pos] = coeff.level;
    }
    Ok(grid)
}

/// The default scan of a square transform (spec 8.4.2's `Default_Scan_NxN`),
/// as raster positions in the order they are coded.
///
/// The table is a rule rather than hundreds of pinned numbers: the scan walks
/// the anti-diagonals of the transform outwards from the origin, each diagonal
/// from its top-right end on odd diagonals and from its bottom-left end on
/// even ones.
fn default_scan(side: usize) -> Vec<u16> {
    let mut scan = Vec::with_capacity(side * side);
    for d in 0..(2 * side - 1) {
        let lo = d.saturating_sub(side - 1);
        let hi = d.min(side - 1);
        let diagonal = (lo..=hi).map(|row| (row * side + (d - row)) as u16);
        if d % 2 == 0 {
            scan.extend(diagonal.rev());
        } else {
            scan.extend(diagonal);
        }
    }
    scan
}

/// coeffs() for one plane's transform block of any coefficients the base and
/// base-range syntax reach (spec 5.11.39).
///
/// A 32x32 or 16x16 transform is DCT-only here, so no transform type is coded;
/// and the levels the contexts below are read from are the levels of
/// coefficients later in the scan, which a decoder walking the scan backwards
/// already has.
fn write_coeffs(
    enc: &mut SymbolEncoder,
    coding: &TxbCoding,
    grid: &[i32],
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
) {
    let side = coding.side;
    let eob = scan
        .iter()
        .rposition(|&pos| grid[pos as usize] != 0)
        .map_or(0, |i| i + 1);
    enc.symbol_fixed(usize::from(eob == 0), &coding.txb_skip[skip_ctx]);
    if eob == 0 {
        return;
    }

    write_eob(enc, coding, eob);

    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos / side, pos % side);
        let level = grid[pos].abs();
        if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            enc.symbol_fixed(
                (level.min(NUM_BASE_LEVELS + 1) - 1) as usize,
                &coding.base_eob[ctx],
            );
        } else {
            let ctx = base_ctx(grid, side, row, col);
            enc.symbol_fixed(level.min(NUM_BASE_LEVELS + 1) as usize, &coding.base[ctx]);
        }
        if level > NUM_BASE_LEVELS {
            let ctx = br_ctx(grid, side, row, col);
            let mut remaining = level - (NUM_BASE_LEVELS + 1);
            let mut sent = 0;
            while sent < COEFF_BASE_RANGE {
                let k = remaining.min(BR_STEP);
                enc.symbol_fixed(k as usize, &coding.br[ctx]);
                if k < BR_STEP {
                    break;
                }
                remaining -= k;
                sent += BR_STEP;
            }
        }
    }

    // The signs come after the levels, in scan order, the DC's from a CDF and
    // the rest as raw bits (spec 5.11.39).
    for &pos in &scan[..eob] {
        let level = grid[pos as usize];
        if level == 0 {
            continue;
        }
        if pos == 0 {
            enc.symbol_fixed(usize::from(level < 0), &coding.dc_sign[sign_ctx]);
        } else {
            enc.literal(u32::from(level < 0), 1);
        }
        // A level the base and base-range syntax cannot reach carries the rest
        // of itself here, after its own sign (spec 5.11.39).
        if level.abs() > MAX_BR_LEVEL {
            write_golomb(enc, (level.abs() - MAX_BR_LEVEL - 1) as u32);
        }
    }
}

/// The end-of-block position (spec 5.11.39): which group of scan positions the
/// last coded coefficient falls in, then its offset inside that group — the
/// offset's top bit from a CDF and the rest as raw bits.
fn write_eob(enc: &mut SymbolEncoder, coding: &TxbCoding, eob: usize) {
    /// `Eob_Group_Start` (spec 5.11.39): the first scan position each group of
    /// end-of-block positions covers, indexed by the group's own number.
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    /// `Eob_Offset_Bits` (spec 5.11.39): how wide each group's offset is.
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let group = GROUP_START
        .iter()
        .rposition(|&start| start <= eob)
        .expect("the first group starts at zero");
    // The groups are numbered from one, and how many of them the transform
    // reaches is the size of its own end-of-block alphabet.
    enc.symbol_fixed(group - 1, coding.eob_pt);

    let bits = OFFSET_BITS[group];
    if bits > 0 {
        let offset = (eob - GROUP_START[group]) as u32;
        let top = (offset >> (bits - 1)) & 1;
        enc.symbol_fixed(top as usize, &coding.eob_extra[group - 3]);
        if bits > 1 {
            enc.literal(offset & ((1 << (bits - 1)) - 1), bits - 1);
        }
    }
}

/// Writes the remainder of a level the base and base-range syntax could not
/// reach (spec 5.11.40): the value plus one, as its bit length in unary
/// followed by the value's own bits, most significant first.
pub(crate) fn write_golomb(enc: &mut SymbolEncoder, value: u32) {
    let x = value + 1;
    let length = 32 - x.leading_zeros();
    for _ in 0..length - 1 {
        enc.literal(0, 1);
    }
    for i in (0..length).rev() {
        enc.literal((x >> i) & 1, 1);
    }
}

/// The context of the level of the last coded coefficient (spec 8.3.2): how
/// far into the scan it sits, in quarters and eighths of the transform.
fn eob_coeff_ctx(scan_idx: usize, area: usize) -> usize {
    match scan_idx {
        0 => 0,
        i if i <= area / 8 => 1,
        i if i <= area / 4 => 2,
        _ => 3,
    }
}

/// The context of a coefficient's level (spec 8.3.2): the magnitudes of the
/// five neighbours below and to the right of it — the ones a decoder walking
/// the scan backwards already has — plus a term for where in the transform it
/// sits. The DC reads context 0 whatever its neighbours carry.
fn base_ctx(grid: &[i32], side: usize, row: usize, col: usize) -> usize {
    if row == 0 && col == 0 {
        return 0;
    }
    let mag: i32 = [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)]
        .iter()
        .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs().min(3))
        .sum();
    let offset = cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize;
    (((mag + 1) >> 1).min(4) as usize) + offset
}

/// The context of a coefficient's base-range tail (spec 8.3.2): the magnitudes
/// of its three closest neighbours below and to the right, uncapped, and a
/// term separating the DC, the corner of the transform, and the rest.
fn br_ctx(grid: &[i32], side: usize, row: usize, col: usize) -> usize {
    let mag: i32 = [(1, 0), (0, 1), (1, 1)]
        .iter()
        .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs())
        .sum();
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        mag
    } else if row < 2 && col < 2 {
        mag + 7
    } else {
        mag + 14
    }
}

/// A level at a position that may fall off the transform, where the levels a
/// context reads are zero.
fn neighbour(grid: &[i32], side: usize, row: usize, col: usize) -> i32 {
    if row >= side || col >= side {
        0
    } else {
        grid[row * side + col]
    }
}

/// Shared by both DC writers: one level per block, none of them zero, and a
/// q index the coefficient CDFs are known for.
fn check_levels(levels: &[i32], blocks: usize, base_q_idx: u8) -> Result<()> {
    if levels.len() != blocks {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame needs one level per coded block",
        ));
    }
    if levels.iter().any(|&l| l == 0 || l.abs() > MAX_BR_LEVEL) {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame is written for levels -14..=14 without zero; \
             wider levels need the Golomb tail",
        ));
    }
    if !Q_CTX_2.contains(&base_q_idx) {
        return Err(Error::unsupported(
            "AV1 tile",
            "the coefficient CDFs of only one q context are known, so \
             base_q_idx must be 61..=120",
        ));
    }
    Ok(())
}

/// coeffs() for a luma transform block whose only coefficient is the DC (spec
/// 5.11.39). The block size is the transform size, so the all-zero flag's
/// context is 0; an end-of-block of one is token 0 of the position alphabet
/// with no extra bits; the transform sizes here are all DCT-only, so no
/// transform type is coded; and the DC's own neighbours are zero, so its
/// magnitude contexts are 0 throughout.
fn write_dc_coeffs(
    enc: &mut SymbolEncoder,
    dc_level: i32,
    sign_ctx: usize,
    txb_skip: &[u16],
    base_eob: &[u16],
) {
    let level = dc_level.abs();
    enc.symbol_fixed(0, txb_skip);
    enc.symbol_fixed(0, &cdf::EOB_PT_1024_LUMA);
    enc.symbol_fixed((level.min(NUM_BASE_LEVELS + 1) - 1) as usize, base_eob);
    if level > NUM_BASE_LEVELS {
        let mut remaining = level - (NUM_BASE_LEVELS + 1);
        let mut sent = 0;
        while sent < COEFF_BASE_RANGE {
            let k = remaining.min(BR_STEP);
            enc.symbol_fixed(k as usize, &cdf::COEFF_BR_LUMA_32[0]);
            if k < BR_STEP {
                break;
            }
            remaining -= k;
            sent += BR_STEP;
        }
    }
    // The signs come after the levels, DC first (spec 5.11.39).
    enc.symbol_fixed(usize::from(dc_level < 0), &cdf::DC_SIGN_LUMA[sign_ctx]);
}

/// `Dc_Sign_Contexts` (spec 8.3.2): every 4x4 unit above and left of the block
/// votes the sign of the DC its own block carries — plus one for a positive
/// one, minus one for a negative one, nothing for a unit with no coded
/// coefficient — and the sum picks one of three contexts. Every block here is a
/// whole superblock carrying one DC, so all sixteen units of a neighbour vote
/// together and the sum can only lean up, lean down, or cancel.
fn dc_sign_ctx(above: Option<bool>, left: Option<bool>) -> usize {
    let vote = |n: Option<bool>| match n {
        None => 0i32,
        Some(true) => -1,
        Some(false) => 1,
    };
    match (vote(above) + vote(left)).signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_obu;
    use crate::obu::temporal_delimiter;
    use crate::sequence::sequence_header_obu;
    use ec_av1_syntax::sequence::SequenceHeader;
    use ec_av1_syntax::{
        FrameHeader, FrameType, LoopFilterParams, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
        TxMode,
    };
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// A 64x64 sequence with every tool this writer does not code turned off:
    /// 64x64 superblocks, no CDEF, no loop restoration, no superres, no filter
    /// intra, and no screen content tools (which is what keeps intra block copy
    /// and palette out of the block syntax).
    fn sequence_64() -> SequenceHeader {
        let mut seq = crate::sequence::tests::sample_1080p();
        seq.frame_width_bits = 7;
        seq.frame_height_bits = 7;
        seq.max_frame_width = 64;
        seq.max_frame_height = 64;
        seq.use_128x128_superblock = false;
        seq.enable_filter_intra = false;
        seq.enable_cdef = false;
        seq.enable_restoration = false;
        seq.enable_superres = false;
        seq.seq_force_screen_content_tools = 0;
        seq.seq_force_integer_mv = 0;
        seq
    }

    /// The key frame the tile above belongs to: one tile, quantised so nothing
    /// is lossless, one transform size per block, no in-loop filtering, and no
    /// CDF adaptation (the writer codes against the defaults).
    fn flat_key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            frame_is_intra: true,
            show_frame: true,
            error_resilient_mode: true,
            disable_cdf_update: true,
            allow_screen_content_tools: false,
            force_integer_mv: true,
            refresh_frame_flags: 0xFF,
            primary_ref_frame: PRIMARY_REF_NONE,
            frame_width: 64,
            frame_height: 64,
            upscaled_width: 64,
            render_width: 64,
            render_height: 64,
            mi_cols: 16,
            mi_rows: 16,
            tile_info: TileInfo {
                uniform_spacing: true,
                cols: 1,
                rows: 1,
                cols_log2: 0,
                rows_log2: 0,
                mi_col_starts: vec![0, 16],
                mi_row_starts: vec![0, 16],
                context_update_tile_id: 0,
                tile_size_bytes: 1,
            },
            quantization: QuantizationParams {
                base_q_idx: 100,
                ..QuantizationParams::default()
            },
            loop_filter: LoopFilterParams::default(),
            tx_mode: TxMode::Largest,
            reduced_tx_set: false,
            ..FrameHeader::default()
        }
    }

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the planes.
    fn ffmpeg_decode(stream: &[u8], w: usize, h: usize) -> Vec<u8> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg failed to start");
        child
            .stdin
            .take()
            .expect("ffmpeg stdin")
            .write_all(stream)
            .expect("writing the stream to ffmpeg");
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout.len(),
            w * h * 3 / 2,
            "expected one 4:2:0 frame, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn flat_key_frame_decodes_to_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP flat_key_frame_decodes_to_mid_grey: no ffmpeg on PATH");
            return;
        }
        let seq = sequence_64();
        let header = flat_key_frame();
        let tile = flat_key_frame_tile(header.mi_cols, header.mi_rows).unwrap();

        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());

        let planes = ffmpeg_decode(&stream, 64, 64);
        // A DC prediction with no neighbour to average is the middle of the
        // range, and a skipped block adds no residual, so every sample of every
        // plane is 128 — one wrong symbol anywhere in the tile shows up here.
        for (i, &s) in planes.iter().enumerate() {
            assert_eq!(s, 128, "sample {i} of the decoded frame");
        }
    }

    /// The one q-context whose CDFs this crate carries; the decoded table
    /// below is pinned at this quantiser and nowhere else.
    const Q_IDX: u8 = 100;

    /// Encode a 64x64 key frame carrying `dc_level` and hand back its planes.
    fn decode_dc_frame(dc_level: i32) -> Vec<u8> {
        let seq = sequence_64();
        let mut header = flat_key_frame();
        header.quantization.base_q_idx = Q_IDX;
        let tile = dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, dc_level).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, 64, 64)
    }

    /// What a reference decoder makes of each DC level at `base_q_idx` 100:
    /// the level times the DC quantiser, inverse-transformed over a whole
    /// 64x64 block — which spreads one coefficient over 4096 samples, so a
    /// level moves the picture by a sample or two, not by tens. These numbers
    /// are pinned from the decoder rather than derived: this crate has no
    /// inverse transform of its own yet to derive them with. What the test
    /// asserts around them — flat planes, untouched chroma, monotone in the
    /// level, and the sign going the right way — is derived, and it is what a
    /// desync in the coefficient syntax breaks first.
    const DECODED_AT_Q100: [(i32, u8); 28] = [
        (1, 128),
        (2, 128),
        (3, 129),
        (4, 129),
        (5, 129),
        (6, 129),
        (7, 129),
        (8, 129),
        (9, 130),
        (10, 130),
        (11, 130),
        (12, 130),
        (13, 130),
        (14, 131),
        (-1, 128),
        (-2, 128),
        (-3, 128),
        (-4, 127),
        (-5, 127),
        (-6, 127),
        (-7, 127),
        (-8, 127),
        (-9, 126),
        (-10, 126),
        (-11, 126),
        (-12, 126),
        (-13, 126),
        (-14, 126),
    ];

    #[test]
    fn a_dc_coefficient_moves_the_whole_block_off_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_dc_coefficient_moves_the_whole_block_off_mid_grey: no ffmpeg");
            return;
        }
        let mut seen = Vec::new();
        for (level, want) in DECODED_AT_Q100 {
            let planes = decode_dc_frame(level);
            let (luma, chroma) = planes.split_at(64 * 64);
            for (i, &sample) in luma.iter().enumerate() {
                assert_eq!(sample, want, "luma sample {i} at dc level {level}");
            }
            // The chroma transform blocks are all-zero, so both planes stay at
            // the prediction. A desync in the luma coefficient syntax lands
            // here first: the chroma flags are the next symbols after it.
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} at dc level {level}");
            }
            seen.push((level, want));
        }
        // The picture moves off mid-grey the way the level says: up for a
        // positive level, down for a negative one, and never back towards it
        // as the level grows.
        for w in seen.windows(2) {
            let ((prev_level, prev), (level, value)) = (w[0], w[1]);
            if prev_level.signum() != level.signum() {
                continue;
            }
            if level > 0 {
                assert!(
                    value >= prev,
                    "level {level} decoded below level {prev_level}"
                );
                assert!(value >= 128, "a positive level darkened the picture");
            } else {
                assert!(
                    value <= prev,
                    "level {level} decoded above level {prev_level}"
                );
                assert!(value <= 128, "a negative level brightened the picture");
            }
        }
        assert!(
            DECODED_AT_Q100.iter().any(|&(_, v)| v > 128)
                && DECODED_AT_Q100.iter().any(|&(_, v)| v < 128),
            "the pinned table has to move the picture both ways"
        );
    }

    /// Encode a grid of 64x64 superblocks, each carrying its own DC level, and
    /// hand back the decoded planes.
    fn decode_level_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
        let (w, h) = (64 * sb_cols, 64 * sb_rows);
        let (seq, header) = frame_of(w, h);
        let tile = dc_key_frame_tile_levels(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, w as usize, h as usize)
    }

    /// A sequence and key frame header for a `w` by `h` frame of whole
    /// superblocks.
    fn frame_of(w: u32, h: u32) -> (SequenceHeader, FrameHeader) {
        let mut seq = sequence_64();
        seq.max_frame_width = w;
        seq.max_frame_height = h;
        let mut header = flat_key_frame();
        header.frame_width = w;
        header.frame_height = h;
        header.upscaled_width = w;
        header.render_width = w;
        header.render_height = h;
        header.mi_cols = w / 4;
        header.mi_rows = h / 4;
        header.tile_info.mi_col_starts = vec![0, header.mi_cols];
        header.tile_info.mi_row_starts = vec![0, header.mi_rows];
        header.quantization.base_q_idx = Q_IDX;
        (seq, header)
    }

    fn decoded_value(level: i32) -> u8 {
        DECODED_AT_Q100
            .iter()
            .find(|&&(l, _)| l == level)
            .map(|&(_, v)| v)
            .expect("the level is in the pinned table")
    }

    /// `dc_predict` (spec 7.11.2) for the flat case: a block whose neighbours
    /// are themselves flat predicts their average, and predicts mid-grey with
    /// no neighbour at all. Every block here is a whole 64x64 superblock, so
    /// the above row and the left column weigh the same.
    fn dc_prediction(above: Option<u8>, left: Option<u8>) -> u8 {
        match (above, left) {
            (None, None) => 128,
            (Some(a), None) => a,
            (None, Some(l)) => l,
            (Some(a), Some(l)) => ((u32::from(a) * 64 + u32::from(l) * 64 + 64) >> 7) as u8,
        }
    }

    /// What a DC level adds to the prediction: the pinned table is that sum
    /// against a mid-grey prediction, and the residual does not depend on what
    /// it is added to.
    fn dc_residual(level: i32) -> i32 {
        i32::from(decoded_value(level)) - 128
    }

    /// Every block reads its DC sign context off the coded blocks above and to
    /// its left, and the three ways that can land — no coded neighbour, the
    /// neighbours leaning one way, the neighbours cancelling — are only
    /// reachable in a frame whose levels differ in sign. Getting the context
    /// wrong desyncs the arithmetic decoder, so the check is that every block
    /// still decodes to the grey its own level asks for.
    #[test]
    fn each_superblock_decodes_to_the_grey_its_own_level_asks_for() {
        if !have_ffmpeg() {
            eprintln!("SKIP each_superblock_decodes_to_the_grey_its_own_level_asks_for: no ffmpeg");
            return;
        }
        // Read in raster order the two grids put a positive, a negative and a
        // cancelling sign context in front of the bottom-right block, and a
        // leaning-down one in front of the second grid's right and bottom
        // blocks.
        for levels in [[14, 3, -3, -14], [-14, -3, 3, 14]] {
            let planes = decode_level_grid(&levels, 2, 2);
            let (luma, chroma) = planes.split_at(128 * 128);
            // The blocks are not independent: a DC prediction reads the
            // reconstructed neighbours, so each block's grey is its
            // neighbours' average plus its own residual.
            let mut recon = [0u8; 4];
            for (block, level) in levels.iter().enumerate() {
                let (br, bc) = (block / 2, block % 2);
                let above = (br > 0).then(|| recon[block - 2]);
                let left = (bc > 0).then(|| recon[block - 1]);
                let want = (i32::from(dc_prediction(above, left)) + dc_residual(*level))
                    .clamp(0, 255) as u8;
                recon[block] = want;
                for y in 0..64 {
                    for x in 0..64 {
                        let i = (br * 64 + y) * 128 + bc * 64 + x;
                        assert_eq!(
                            luma[i], want,
                            "luma at ({x}, {y}) of the block carrying level {level} in {levels:?}"
                        );
                    }
                }
            }
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} of {levels:?}");
            }
        }
    }

    /// What a DC level adds to a 32x32 block's prediction at `base_q_idx` 100,
    /// pinned from the decoder the way [`DECODED_AT_Q100`] is. A 32x32
    /// transform spreads its DC over a quarter of the samples a 64x64 one
    /// does, so the same level moves the picture further: level 14 is five
    /// sample values here against three there.
    const SPLIT_RESIDUAL_AT_Q100: [(i32, i32); 28] = [
        (1, 0),
        (2, 1),
        (3, 1),
        (4, 1),
        (5, 2),
        (6, 2),
        (7, 3),
        (8, 3),
        (9, 3),
        (10, 4),
        (11, 4),
        (12, 4),
        (13, 5),
        (14, 5),
        (-1, 0),
        (-2, -1),
        (-3, -1),
        (-4, -1),
        (-5, -2),
        (-6, -2),
        (-7, -2),
        (-8, -3),
        (-9, -3),
        (-10, -4),
        (-11, -4),
        (-12, -4),
        (-13, -5),
        (-14, -5),
    ];

    fn split_residual(level: i32) -> i32 {
        SPLIT_RESIDUAL_AT_Q100
            .iter()
            .find(|&&(l, _)| l == level)
            .map(|&(_, r)| r)
            .expect("the level is in the pinned table")
    }

    /// Splitting a superblock puts three more syntax elements in front of every
    /// block — the split partition itself, the 32x32 partition below it and a
    /// chroma mode from the wider table CFL-capable blocks read — and moves the
    /// coefficients onto the 32x32 CDFs. Getting any of them wrong desyncs the
    /// arithmetic decoder, and the check is that all sixteen blocks still land
    /// on the grey their own level and their neighbours ask for.
    #[test]
    fn a_split_superblock_decodes_each_quadrant_on_its_own_level() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_split_superblock_decodes_each_quadrant_on_its_own_level: no ffmpeg");
            return;
        }
        // Four by four 32x32 blocks over two by two superblocks, so the walk
        // crosses superblock boundaries in both directions, and signs that
        // alternate so the DC sign context lands on all three of its values.
        let levels: [i32; 16] = [14, -3, 5, -5, -7, 7, -14, 3, 2, -2, 9, -9, -11, 11, -1, 1];
        let planes = decode_split_grid(&levels, 2, 2);
        let (luma, chroma) = planes.split_at(128 * 128);

        let mut recon = [0i32; 16];
        for (block, level) in levels.iter().enumerate() {
            let (br, bc) = (block / 4, block % 4);
            let above = (br > 0).then(|| recon[block - 4] as u8);
            let left = (bc > 0).then(|| recon[block - 1] as u8);
            let want =
                (i32::from(dc_prediction(above, left)) + split_residual(*level)).clamp(0, 255);
            recon[block] = want;
            for y in 0..32 {
                for x in 0..32 {
                    let i = (br * 32 + y) * 128 + bc * 32 + x;
                    assert_eq!(
                        i32::from(luma[i]),
                        want,
                        "luma at ({x}, {y}) of the block carrying level {level} at row {br}, \
                         column {bc}"
                    );
                }
            }
        }
        for (i, &sample) in chroma.iter().enumerate() {
            assert_eq!(sample, 128, "chroma sample {i}");
        }
    }

    /// Encode a grid of 32x32 blocks, each carrying its own DC level.
    fn decode_split_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
        let (w, h) = (64 * sb_cols, 64 * sb_rows);
        let (seq, header) = frame_of(w, h);
        let tile = split_dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, w as usize, h as usize)
    }

    #[test]
    fn a_level_grid_that_does_not_cover_the_frame_is_refused() {
        assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3]).is_err());
        assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3, 0]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 3, 3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 0, 3]).is_err());
    }

    #[test]
    fn dc_levels_the_base_syntax_cannot_carry_are_refused() {
        assert!(dc_key_frame_tile(16, 16, 100, 0).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, 15).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, -15).is_err());
        assert!(dc_key_frame_tile(16, 16, 40, 3).is_err());
    }

    #[test]
    fn partial_superblocks_are_refused() {
        assert!(flat_key_frame_tile(16, 20).is_err());
        assert!(flat_key_frame_tile(0, 16).is_err());
    }

    /// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
    /// hands back the luma plane as rows of samples.
    fn decode_coeff_quadrants(blocks: &[Vec<Coeff>]) -> Vec<Vec<u8>> {
        let blocks: Vec<BlockCoeffs> = blocks.iter().cloned().map(BlockCoeffs::from).collect();
        let [luma, u, v] = decode_coeff_planes(&blocks);
        for (name, plane) in [("U", &u), ("V", &v)] {
            for (y, row) in plane.iter().enumerate() {
                for (x, &s) in row.iter().enumerate() {
                    assert_eq!(
                        s, 128,
                        "{name} sample ({x},{y}): no block codes a chroma coefficient, and a \
                         DC prediction with no coded neighbour is mid-grey"
                    );
                }
            }
        }
        luma
    }

    /// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
    /// hands back its three planes as rows of samples: 64 rows of luma and, at
    /// 4:2:0, 32 rows of each chroma plane.
    fn decode_coeff_planes(blocks: &[BlockCoeffs]) -> [Vec<Vec<u8>>; 3] {
        let (seq, header) = frame_of(64, 64);
        let tile =
            split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, 64, 64);
        let (luma, chroma) = planes.split_at(64 * 64);
        let (u, v) = chroma.split_at(32 * 32);
        [
            luma.chunks_exact(64).map(<[u8]>::to_vec).collect(),
            u.chunks_exact(32).map(<[u8]>::to_vec).collect(),
            v.chunks_exact(32).map(<[u8]>::to_vec).collect(),
        ]
    }

    /// The top-left 32x32 block of a decoded frame, which is the only one whose
    /// prediction has no neighbour to lean on.
    fn top_left_block(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
        rows[..32].iter().map(|r| r[..32].to_vec()).collect()
    }

    /// A transform's basis functions all average to zero except the DC's, so a
    /// block carrying no DC decodes to samples that average to its prediction —
    /// mid-grey for the block with no neighbours. Rounding of the inverse
    /// transform moves the average by less than a sample.
    fn assert_mean_is_mid_grey(block: &[Vec<u8>]) {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        let mean = f64::from(sum) / (32.0 * 32.0);
        assert!(
            (mean - 128.0).abs() <= 1.0,
            "a block with no DC coefficient averages to its mid-grey prediction, got {mean}"
        );
    }

    /// A coefficient at row 0, column 1 selects the basis function that is flat
    /// down the block and a half-cycle of a cosine across it. Nothing about
    /// that shape is pinned here: every row of the block must be the same row,
    /// the samples must fall from left to right and actually move, and the
    /// block must still average to its prediction. A desync in the scan, the
    /// end-of-block position or the level contexts breaks one of those.
    #[test]
    fn a_coefficient_off_the_origin_selects_its_own_basis_function() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_coefficient_off_the_origin_selects_its_own_basis_function: no ffmpeg"
            );
            return;
        }
        let blocks = vec![
            vec![Coeff {
                row: 0,
                col: 1,
                level: 12,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let rows = decode_coeff_quadrants(&blocks);
        let block = top_left_block(&rows);

        for (y, row) in block.iter().enumerate() {
            assert_eq!(row, &block[0], "row {y} of a horizontal basis function");
        }
        assert!(
            block[0][0] > block[0][31],
            "a positive coefficient leans the block towards its left edge: {:?}",
            block[0]
        );
        for x in 1..32 {
            assert!(
                block[0][x] <= block[0][x - 1],
                "the half-cycle falls from left to right, broke at column {x}: {:?}",
                block[0]
            );
        }
        assert_mean_is_mid_grey(&block);

        // The same coefficient transposed selects the basis function that is
        // flat across the block and a half-cycle down it.
        let blocks = vec![
            vec![Coeff {
                row: 1,
                col: 0,
                level: 12,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let rows = decode_coeff_quadrants(&blocks);
        let block = top_left_block(&rows);
        for (y, row) in block.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(s, row[0], "sample ({y},{x}) of a vertical basis function");
            }
        }
        assert!(
            block[0][0] > block[31][0],
            "a positive coefficient leans the block up"
        );
        for y in 1..32 {
            assert!(
                block[y][0] <= block[y - 1][0],
                "the half-cycle falls from top to bottom, broke at row {y}"
            );
        }
        assert_mean_is_mid_grey(&block);
    }

    /// A coefficient late in the scan pushes the end-of-block position into a
    /// group that carries an offset — the far corner of a 32x32 transform is
    /// the last of the eleven groups, whose offset is nine bits wide, one of
    /// them from a CDF and the rest raw. Every coefficient before it is coded
    /// too, so this also walks the base and base-range contexts over a block
    /// with neighbours that carry levels rather than zeros.
    #[test]
    fn a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group");
            return;
        }
        let block = vec![
            Coeff {
                row: 31,
                col: 31,
                level: 5,
            },
            Coeff {
                row: 5,
                col: 7,
                level: -9,
            },
            Coeff {
                row: 1,
                col: 1,
                level: 14,
            },
            Coeff {
                row: 0,
                col: 2,
                level: -3,
            },
            Coeff {
                row: 2,
                col: 0,
                level: 2,
            },
            Coeff {
                row: 1,
                col: 2,
                level: -1,
            },
        ];
        let rows = decode_coeff_quadrants(&[block, Vec::new(), Vec::new(), Vec::new()]);
        let block = top_left_block(&rows);
        assert_mean_is_mid_grey(&block);
        assert!(
            block.iter().flatten().any(|&s| s != block[0][0]),
            "six coefficients do not decode to a flat block"
        );
        // The blocks that carry nothing are flat: their prediction is whatever
        // their neighbours left, and they add no residual to it.
        for (name, ys, xs) in [("top right", 0..32, 32..64), ("bottom left", 32..64, 0..32)] {
            let first = rows[ys.start][xs.start];
            for y in ys {
                for x in xs.clone() {
                    assert_eq!(rows[y][x], first, "the {name} block carries no coefficient");
                }
            }
        }
    }

    /// Neighbouring blocks that each carry coefficients read their DC sign
    /// context, their prediction and their own contexts off each other, so a
    /// frame whose four blocks all carry mixed-sign coefficients exercises what
    /// a single coded block cannot. The check is that it decodes at all — a
    /// desync anywhere fails the decode or the frame size — and that the block
    /// with no neighbours still averages to mid-grey.
    #[test]
    fn every_quadrant_carries_its_own_coefficients() {
        if !have_ffmpeg() {
            eprintln!("SKIP every_quadrant_carries_its_own_coefficients: no ffmpeg");
            return;
        }
        let blocks = vec![
            vec![
                Coeff {
                    row: 0,
                    col: 1,
                    level: 7,
                },
                Coeff {
                    row: 3,
                    col: 4,
                    level: -14,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: -6,
                },
                Coeff {
                    row: 2,
                    col: 2,
                    level: 9,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: 6,
                },
                Coeff {
                    row: 1,
                    col: 0,
                    level: -2,
                },
                Coeff {
                    row: 8,
                    col: 9,
                    level: 3,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: 11,
                },
                Coeff {
                    row: 16,
                    col: 1,
                    level: -4,
                },
            ],
        ];
        let rows = decode_coeff_quadrants(&blocks);
        assert_mean_is_mid_grey(&top_left_block(&rows));
        for (name, ys, xs) in [
            ("top right", 0..32, 32..64),
            ("bottom left", 32..64, 0..32),
            ("bottom right", 32..64, 32..64),
        ] {
            let block: Vec<Vec<u8>> = rows[ys].iter().map(|r| r[xs.clone()].to_vec()).collect();
            assert!(
                block.iter().flatten().any(|&s| s != block[0][0]),
                "the {name} block carries coefficients, so it is not flat"
            );
        }
    }

    /// The top-left 16x16 block of a decoded chroma plane, which is the only
    /// one whose prediction has no neighbour to lean on.
    fn top_left_chroma(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
        rows[..16].iter().map(|r| r[..16].to_vec()).collect()
    }

    /// The mean of a decoded block, which every basis function but the DC's
    /// leaves at the block's prediction.
    fn mean(block: &[Vec<u8>]) -> f64 {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        let count = block.iter().map(Vec::len).sum::<usize>() as f64;
        f64::from(sum) / count
    }

    /// A chroma plane's coefficients ride their own transform, their own CDFs
    /// and their own end-of-block alphabet, so a chroma basis function is the
    /// gate on all three: U carries the coefficient that is flat down the block
    /// and a half-cycle across it, V the one turned a quarter turn, and neither
    /// may reach the other plane or luma.
    #[test]
    fn each_chroma_plane_codes_its_own_basis_function() {
        if !have_ffmpeg() {
            eprintln!("SKIP each_chroma_plane_codes_its_own_basis_function: no ffmpeg");
            return;
        }
        let mut blocks = vec![BlockCoeffs::default(); 4];
        blocks[0].u = vec![Coeff {
            row: 0,
            col: 1,
            level: 12,
        }];
        blocks[0].v = vec![Coeff {
            row: 1,
            col: 0,
            level: 12,
        }];
        let [luma, u, v] = decode_coeff_planes(&blocks);

        for (y, row) in luma.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(
                    s, 128,
                    "luma sample ({x},{y}): no block codes a luma coefficient, so the picture \
                     stays at its mid-grey prediction"
                );
            }
        }

        let u_block = top_left_chroma(&u);
        for (y, row) in u_block.iter().enumerate() {
            assert_eq!(
                row, &u_block[0],
                "row {y} of a horizontal chroma basis function"
            );
        }
        assert!(
            u_block[0][0] > u_block[0][15],
            "a positive coefficient leans the U block towards its left edge: {:?}",
            u_block[0]
        );

        let v_block = top_left_chroma(&v);
        for (x, &top) in v_block[0].iter().enumerate() {
            assert_eq!(
                top, v_block[0][0],
                "column {x} of a vertical chroma basis function"
            );
            let column: Vec<u8> = v_block.iter().map(|r| r[x]).collect();
            assert!(
                column[0] > column[15],
                "a positive coefficient leans the V column towards its top edge: {column:?}"
            );
        }

        for (name, block) in [("U", &u_block), ("V", &v_block)] {
            let mean = mean(block);
            assert!(
                (mean - 128.0).abs() <= 1.0,
                "the {name} block carries no DC, so it averages to its mid-grey prediction, \
                 got {mean}"
            );
        }
    }

    /// Every block coding chroma puts the chroma all-zero flag on the contexts
    /// a coded neighbour above and to the left select, and the DC sign on the
    /// contexts their signs select. A frame whose four blocks all carry chroma
    /// DCs of mixed sign walks those contexts; the picture that comes back must
    /// still lean the way each block's own DC asks.
    #[test]
    fn chroma_dc_signs_read_their_neighbours() {
        if !have_ffmpeg() {
            eprintln!("SKIP chroma_dc_signs_read_their_neighbours: no ffmpeg");
            return;
        }
        let dc = |level| {
            vec![Coeff {
                row: 0,
                col: 0,
                level,
            }]
        };
        let signs = [8, -8, -8, 8];
        let blocks: Vec<BlockCoeffs> = signs
            .iter()
            .map(|&level| BlockCoeffs {
                luma: Vec::new(),
                u: dc(level),
                v: dc(-level),
                ..BlockCoeffs::default()
            })
            .collect();
        let [luma, u, v] = decode_coeff_planes(&blocks);

        for (y, row) in luma.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(s, 128, "luma sample ({x},{y}) with no luma coefficient");
            }
        }

        // A DC prediction leans on the neighbours a block has, so each block is
        // measured against the prediction it was given rather than mid-grey:
        // the only block with no neighbour is the first, and the rest are read
        // as a step away from the block above them and to their left.
        for (i, &level) in signs.iter().enumerate() {
            let (r, c) = (i / 2, i % 2);
            let block = |plane: &[Vec<u8>]| -> Vec<Vec<u8>> {
                plane[r * 16..r * 16 + 16]
                    .iter()
                    .map(|row| row[c * 16..c * 16 + 16].to_vec())
                    .collect()
            };
            for (name, plane, want) in [("U", &u, level), ("V", &v, -level)] {
                let block = block(plane);
                let mean = mean(&block);
                for (y, row) in block.iter().enumerate() {
                    assert_eq!(
                        row.iter().collect::<std::collections::HashSet<_>>().len(),
                        1,
                        "row {y} of a DC-only {name} block is flat"
                    );
                }
                if i == 0 {
                    assert!(
                        (want > 0) == (mean > 128.0),
                        "{name} block {i} carries a DC of {want}, so it must lean off its \
                         mid-grey prediction that way, got {mean}"
                    );
                }
            }
        }
        assert_ne!(u, v, "the two chroma planes carry opposite DCs");
    }

    /// The coefficient writer refuses what it cannot code rather than writing a
    /// stream a decoder walks off the end of.
    /// The 32x32 block at the given block row and column of a decoded plane.
    fn block_at(rows: &[Vec<u8>], block_row: usize, block_col: usize) -> Vec<Vec<u8>> {
        rows[block_row * 32..block_row * 32 + 32]
            .iter()
            .map(|r| r[block_col * 32..block_col * 32 + 32].to_vec())
            .collect()
    }

    /// Encodes a key frame of the given size, whose 32x32 blocks carry
    /// `blocks` in raster order, and hands back its luma plane as rows of
    /// samples.
    fn decode_luma_at(width: usize, height: usize, blocks: &[BlockCoeffs]) -> Vec<Vec<u8>> {
        let (seq, header) = frame_of(width as u32, height as u32);
        let tile =
            split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, width, height);
        planes[..width * height]
            .chunks_exact(width)
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Every row of a block is the same row, and that row falls from left to
    /// right — the shape of a coefficient at row 0, column 1.
    fn assert_falls_across(block: &[Vec<u8>], name: &str) {
        for row in block {
            assert_eq!(
                row, &block[0],
                "{name}: every row of the block is the same row"
            );
        }
        assert!(
            block[0][0] > block[0][31],
            "{name}: the row falls from left to right, got {} then {}",
            block[0][0],
            block[0][31]
        );
    }

    /// Every column of a block is constant across it and falls down it — the
    /// shape of a coefficient at row 1, column 0.
    fn assert_falls_down(block: &[Vec<u8>], name: &str) {
        for (y, row) in block.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == row[0]),
                "{name}: row {y} of the block is one constant"
            );
        }
        assert!(
            block[0][0] > block[31][0],
            "{name}: the column falls down the block, got {} then {}",
            block[0][0],
            block[31][0]
        );
    }

    /// The mean sample of a decoded block.
    fn mean_of(block: &[Vec<u8>]) -> f64 {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        f64::from(sum) / (block.len() * block[0].len()) as f64
    }

    /// The top-left block of a frame whose only coefficient is a luma DC of
    /// `level`.
    fn decode_dc_level_block(level: i32) -> Vec<Vec<u8>> {
        let blocks = vec![
            vec![Coeff {
                row: 0,
                col: 0,
                level,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        top_left_block(&decode_coeff_quadrants(&blocks))
    }

    /// `read_golomb` (spec 5.11.40), written from the specification's
    /// pseudocode, and the level its caller builds from it: a level above the
    /// base-range tail is fifteen plus what the tail carries.
    fn read_golomb_level(dec: &mut crate::msac::tests::SymbolDecoder) -> u32 {
        let mut length = 0;
        while dec.literal(1) == 0 {
            length += 1;
            assert!(
                length < 20,
                "the tail's length prefix runs past twenty bits"
            );
        }
        let mut x = 1;
        for _ in 0..length {
            x = (x << 1) | dec.literal(1);
        }
        (MAX_BR_LEVEL + 1) as u32 + (x - 1)
    }

    /// The tail says how much of the level the base and base-range syntax could
    /// not carry, and the spec's own reader must hand back the level that was
    /// asked for — not one either side of it. A tail written a bit long, a bit
    /// short, or against the wrong base fails this at the first level it
    /// reaches.
    #[test]
    fn the_golomb_tail_reads_back_as_the_level_it_was_written_for() {
        for level in (MAX_BR_LEVEL + 1..=MAX_BR_LEVEL + 300)
            .chain([MAX_LEVEL - 1, MAX_LEVEL])
            .map(|l| l as u32)
        {
            let mut enc = SymbolEncoder::new();
            write_golomb(&mut enc, level - (MAX_BR_LEVEL + 1) as u32);
            // The tail is raw bits, so it needs no padding beyond what the
            // encoder's own flush writes.
            let data = enc.finish();
            let mut dec = crate::msac::tests::SymbolDecoder::new(&data);
            assert_eq!(
                read_golomb_level(&mut dec),
                level,
                "the tail of level {level}"
            );
        }
    }

    /// The base and base-range syntax reach level fourteen between them;
    /// anything above that carries the rest of itself as a Golomb tail, written
    /// after its own sign. A DC level moves the whole block off its mid-grey
    /// prediction by an amount that grows with the level, so a run of levels
    /// either side of the tail's threshold — spaced wide enough that the
    /// quantiser separates them — must decode to a run of steadily
    /// brighter blocks — and the negatives of the same levels to steadily
    /// darker ones. A tail written with the wrong length, in the wrong place in
    /// the syntax, or off by one desyncs the decoder outright.
    #[test]
    fn levels_above_the_base_range_tail_carry_a_golomb_tail() {
        if !have_ffmpeg() {
            eprintln!("SKIP levels_above_the_base_range_tail_carry_a_golomb_tail: no ffmpeg");
            return;
        }
        let mut brighter = Vec::new();
        let mut darker = Vec::new();
        for level in [10, 14, 18, 31, 60, 400] {
            brighter.push((level, mean_of(&decode_dc_level_block(level))));
            darker.push((level, mean_of(&decode_dc_level_block(-level))));
        }
        for pair in brighter.windows(2) {
            let [(low, dim), (high, bright)] = [pair[0], pair[1]];
            assert!(
                bright > dim,
                "level {high} decodes brighter than level {low}, got {bright} against {dim}"
            );
        }
        for pair in darker.windows(2) {
            let [(low, bright), (high, dim)] = [pair[0], pair[1]];
            assert!(
                dim < bright,
                "level -{high} decodes darker than level -{low}, got {dim} against {bright}"
            );
        }
        let (_, mid) = brighter[0];
        assert!(
            mid > 128.0 && darker[0].1 < 128.0,
            "a positive level brightens its block and a negative one darkens it, got {mid} and {}",
            darker[0].1
        );
    }

    /// A frame need not be a whole number of superblocks. A 96x64 frame is a
    /// superblock and a half across: the second superblock has no right-hand
    /// half, so its partition is a gathered flag rather than the full symbol,
    /// and the two blocks that would sit in that half are never coded at all.
    /// Each of the six blocks that do exist carries one of two basis functions,
    /// alternating, so a block written into the wrong place or a flag the
    /// decoder reads as a different number of bits shows up as a block with the
    /// other block's shape — or as a decode failure.
    fn decode_luma_sb(width: usize, height: usize, sbs: &[Superblock]) -> Vec<Vec<u8>> {
        let (seq, header) = frame_of(width as u32, height as u32);
        let tile = sb_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, sbs).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, width, height);
        planes[..width * height]
            .chunks_exact(width)
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// A superblock left whole carries one 64x64 transform, whose lowest
    /// horizontal basis function falls across the whole sixty-four samples in
    /// one stroke. Were the same superblock split, each half would restart the
    /// gradient at its own left edge and the picture would rise again in the
    /// middle, so the walk across the row is what tells the two apart — and
    /// the split superblock beside it proves the two partitions still agree on
    /// where the next one begins.
    #[test]
    fn a_whole_superblock_covers_all_four_of_its_quadrants() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_whole_superblock_covers_all_four_of_its_quadrants: no ffmpeg");
            return;
        }
        let across = BlockCoeffs::from(vec![Coeff {
            row: 0,
            col: 1,
            level: 20,
        }]);
        let down = BlockCoeffs::from(vec![Coeff {
            row: 1,
            col: 0,
            level: 20,
        }]);
        let sbs = [
            Superblock::Whole(across),
            Superblock::Split(vec![down.clone(), down.clone(), down.clone(), down]),
        ];
        let rows = decode_luma_sb(128, 64, &sbs);

        for (y, row) in rows.iter().enumerate() {
            assert_eq!(
                &row[..64],
                &rows[0][..64],
                "row {y} of the whole superblock repeats its first row"
            );
        }
        let first = &rows[0][..64];
        assert!(
            first.windows(2).all(|w| w[0] >= w[1]),
            "the whole superblock falls across its sixty-four samples without \
             restarting: {first:?}"
        );
        assert!(
            first[0] > first[63],
            "the whole superblock falls from left to right, got {} then {}",
            first[0],
            first[63]
        );
        for block_row in 0..2 {
            for block_col in 2..4 {
                let block = block_at(&rows, block_row, block_col);
                assert_falls_down(&block, &format!("quadrant ({block_row},{block_col})"));
            }
        }
    }

    /// A superblock half outside the frame has no partition that keeps a block
    /// outside it, so it cannot be left whole.
    #[test]
    fn a_whole_superblock_at_the_frame_edge_is_refused() {
        let block = BlockCoeffs::from(vec![Coeff {
            row: 0,
            col: 0,
            level: 4,
        }]);
        let sbs = [
            Superblock::Split(vec![
                block.clone(),
                block.clone(),
                block.clone(),
                block.clone(),
            ]),
            Superblock::Whole(block.clone()),
        ];
        // Ninety-six samples across is three 32x32 blocks, so the second
        // superblock hangs half outside the frame.
        let err = sb_coeff_key_frame_tile(24, 16, Q_IDX, &sbs).unwrap_err();
        assert!(
            format!("{err}").contains("half outside the frame"),
            "got {err}"
        );
    }

    #[test]
    fn a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block: no \
                 ffmpeg"
            );
            return;
        }
        let blocks: Vec<BlockCoeffs> = (0..6)
            .map(|i| {
                let (row, col) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
                BlockCoeffs::from(vec![Coeff {
                    row,
                    col,
                    level: 12,
                }])
            })
            .collect();
        let rows = decode_luma_at(96, 64, &blocks);
        for block_row in 0..2 {
            for block_col in 0..3 {
                let name = format!("block ({block_row},{block_col})");
                let block = block_at(&rows, block_row, block_col);
                if (block_row * 3 + block_col) % 2 == 0 {
                    assert_falls_across(&block, &name);
                } else {
                    assert_falls_down(&block, &name);
                }
            }
        }
    }

    /// A frame whose blocks do not tile it is refused rather than written as a
    /// stream a decoder walks off the end of.
    #[test]
    fn a_frame_that_is_not_a_whole_number_of_blocks_is_refused() {
        let (_, header) = frame_of(80, 64);
        let blocks = vec![BlockCoeffs::default(); 4];
        let err = split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, &blocks)
            .expect_err("80 is not a whole number of 32x32 blocks");
        assert!(
            format!("{err}").contains("32x32"),
            "the refusal names the block size, got {err}"
        );
    }

    /// A directional intra mode says where its prediction comes from, and with
    /// no residual of its own a block must be exactly that prediction. The
    /// top-left block is given a basis function that is flat across it and
    /// falls down it; the block to its right is coded `H_PRED`, so every one of
    /// its rows must be the constant its left neighbour's rightmost column
    /// hands it, and the block below is coded `V_PRED`, so every one of its
    /// columns must be the constant the row above it ends on — a single flat
    /// value for the whole block. Nothing here pins the transform's shape or
    /// the mode's CDF: a mode written with the wrong context, an angle delta
    /// left out or a mode index off by one desyncs the symbol decoder and the
    /// two empty blocks stop mirroring their neighbours.
    #[test]
    fn directional_modes_predict_from_the_neighbours_they_name() {
        if !have_ffmpeg() {
            eprintln!("SKIP directional_modes_predict_from_the_neighbours_they_name: no ffmpeg");
            return;
        }
        let gradient = BlockCoeffs {
            luma: vec![Coeff {
                row: 1,
                col: 0,
                level: 12,
            }],
            ..BlockCoeffs::default()
        };
        let blocks = [
            gradient,
            BlockCoeffs {
                mode: H_PRED as u8,
                ..BlockCoeffs::default()
            },
            BlockCoeffs {
                mode: V_PRED as u8,
                ..BlockCoeffs::default()
            },
            BlockCoeffs::default(),
        ];
        let [luma, ..] = decode_coeff_planes(&blocks);

        let source = block_at(&luma, 0, 0);
        for (r, row) in source.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == row[0]),
                "the basis function at row 1, column 0 is flat across the block, row {r} is not:                  {row:?}"
            );
        }
        let profile: Vec<u8> = source.iter().map(|r| r[0]).collect();
        assert!(
            profile.windows(2).all(|w| w[0] >= w[1]) && profile[0] > profile[31],
            "the block must fall from top to bottom, got {profile:?}"
        );

        let horizontal = block_at(&luma, 0, 1);
        for (r, row) in horizontal.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == profile[r]),
                "an H_PRED block repeats the column to its left, row {r} should be all {} but is                  {row:?}",
                profile[r]
            );
        }

        let vertical = block_at(&luma, 1, 0);
        let bottom = profile[31];
        assert!(
            vertical.iter().flatten().all(|&s| s == bottom),
            "a V_PRED block repeats the row above it, which is all {bottom} here, got rows \
             {:?} and {:?}",
            vertical[0],
            vertical[31]
        );
    }

    /// The mode index is checked before anything is written.
    #[test]
    fn an_intra_mode_outside_the_key_frame_set_is_refused() {
        let mut blocks = vec![BlockCoeffs::default(); 4];
        blocks[2].mode = INTRA_MODES as u8;
        let message = split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 13"),
            "the refusal must name the mode, got {message}"
        );
    }

    #[test]
    fn coefficients_the_writer_cannot_code_are_refused() {
        let empty = || vec![BlockCoeffs::default(); 4];
        let cases: [(&str, Vec<BlockCoeffs>); 6] = [
            ("one set short", vec![BlockCoeffs::default(); 3]),
            ("off the transform", {
                let mut b = empty();
                b[0].luma = vec![Coeff {
                    row: 32,
                    col: 0,
                    level: 1,
                }];
                b
            }),
            ("a zero level", {
                let mut b = empty();
                b[1].luma = vec![Coeff {
                    row: 0,
                    col: 0,
                    level: 0,
                }];
                b
            }),
            ("a level past the Golomb tail's reach", {
                let mut b = empty();
                b[2].luma = vec![Coeff {
                    row: 0,
                    col: 0,
                    level: MAX_LEVEL + 1,
                }];
                b
            }),
            ("two coefficients at one position", {
                let mut b = empty();
                b[3].luma = vec![
                    Coeff {
                        row: 4,
                        col: 4,
                        level: 1,
                    },
                    Coeff {
                        row: 4,
                        col: 4,
                        level: 2,
                    },
                ];
                b
            }),
            ("off the chroma transform", {
                let mut b = empty();
                b[0].u = vec![Coeff {
                    row: 0,
                    col: 16,
                    level: 1,
                }];
                b
            }),
        ];
        for (name, blocks) in cases {
            assert!(
                split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks).is_err(),
                "{name} must be refused"
            );
        }
        assert!(
            split_coeff_key_frame_tile(16, 16, 200, &empty()).is_err(),
            "a q index outside the band whose CDFs are known must be refused"
        );
    }

    /// The default scan is written as the rule that generates it rather than a
    /// thousand pinned numbers, so the rule is checked against what a scan must
    /// be: every position exactly once, the origin first, and never a position
    /// before one it sits diagonally behind — which is what lets a decoder read
    /// a coefficient's context off the coefficients it has already decoded.
    #[test]
    fn the_default_scan_walks_every_position_outwards() {
        for side in [TX16, TX32] {
            let area = side * side;
            let scan = default_scan(side);
            assert_eq!(scan.len(), area);
            let mut seen = vec![false; area];
            for &pos in &scan {
                assert!(
                    !seen[pos as usize],
                    "position {pos} is scanned twice in {side}x{side}"
                );
                seen[pos as usize] = true;
            }
            assert_eq!(scan[0], 0, "the scan starts at the DC");

            let mut order = vec![0usize; area];
            for (i, &pos) in scan.iter().enumerate() {
                order[pos as usize] = i;
            }
            for row in 0..side {
                for col in 0..side {
                    for (dr, dc) in [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)] {
                        let (nr, nc) = (row + dr, col + dc);
                        if nr < side && nc < side {
                            assert!(
                                order[nr * side + nc] > order[row * side + col],
                                "({nr},{nc}) is a context neighbour of ({row},{col}), so it must \
                                 be scanned after it"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The whole point of the inverse transform living in the encoder is that
    /// it predicts the decoder without asking one. The residual a DC-only
    /// 64x64 transform produces has already been pinned from a real decoder in
    /// [`DECODED_AT_Q100`], so that table is the gate: every level in it, both
    /// signs, has to come back out of `dequant_and_inverse` exactly.
    #[test]
    fn the_inverse_transform_reproduces_the_pinned_whole_superblock_residuals() {
        for (level, _) in DECODED_AT_Q100 {
            let mut levels = vec![0i32; 64 * 64];
            levels[0] = level;
            let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
            let want = dc_residual(level);
            assert!(
                residual.iter().all(|&r| r == want),
                "level {level}: want a flat {want}, got {}..{} (first {})",
                residual.iter().min().unwrap(),
                residual.iter().max().unwrap(),
                residual[0]
            );
        }
    }

    /// The same gate at the other transform size the encoder codes: a split
    /// superblock's 32x32 blocks, whose residuals are pinned in
    /// [`SPLIT_RESIDUAL_AT_Q100`]. The two sizes divide the dequantized
    /// coefficient by different denominators, so a size-blind dequantizer
    /// passes one table and fails the other.
    #[test]
    fn the_inverse_transform_reproduces_the_pinned_split_residuals() {
        for (level, want) in SPLIT_RESIDUAL_AT_Q100 {
            let mut levels = vec![0i32; 32 * 32];
            levels[0] = level;
            let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
            assert!(
                residual.iter().all(|&r| r == want),
                "level {level}: want a flat {want}, got {}..{} (first {})",
                residual.iter().min().unwrap(),
                residual.iter().max().unwrap(),
                residual[0]
            );
        }
    }

    /// A DC-only transform is flat, which is exactly the case that cannot tell
    /// a transposed or mis-permuted butterfly network from a correct one. An AC
    /// coefficient in the first row must vary along the row and stay constant
    /// down each column, and the one in the first column the other way round;
    /// a transposed network swaps the two.
    #[test]
    fn a_single_ac_coefficient_varies_along_its_own_axis() {
        for side in [4, 8, 16, 32, 64] {
            let mut horizontal = vec![0i32; side * side];
            horizontal[1] = 100;
            let h = crate::transform::dequant_and_inverse(&horizontal, side, 8, i32::from(Q_IDX));
            let mut vertical = vec![0i32; side * side];
            vertical[side] = 100;
            let v = crate::transform::dequant_and_inverse(&vertical, side, 8, i32::from(Q_IDX));
            for row in 0..side {
                for col in 0..side {
                    assert_eq!(
                        h[row * side + col],
                        h[col],
                        "{side}: a first-row coefficient is constant down column {col}"
                    );
                    assert_eq!(
                        v[row * side + col],
                        v[row * side],
                        "{side}: a first-column coefficient is constant along row {row}"
                    );
                }
            }
            // The two are each other's transpose. Only to within a rounding
            // step: the spec rounds halves up rather than away from zero, so a
            // basis function and its transpose can land a count apart where
            // the row and column passes round opposite ways.
            assert!(h[0] != h[side - 1], "{side}: the horizontal basis varies");
            for row in 0..side {
                for col in 0..side {
                    let (a, b) = (h[row * side + col], v[col * side + row]);
                    assert!(
                        (a - b).abs() <= 1,
                        "{side}: transpose at ({row},{col}): {a} vs {b}"
                    );
                }
            }
        }
    }

    /// Negating every level negates every residual, to within the one count
    /// the spec's round-halves-up leaves behind. A dequantizer that rounded a
    /// negative coefficient away from zero instead of toward it would shift a
    /// whole basis function, not a count of it, so the bound is what makes
    /// this a test rather than a restatement.
    #[test]
    fn negating_the_levels_negates_the_residual() {
        for side in [32, 64] {
            for level in [1, 2, 3, 5, 9, 14, 40, 100] {
                let mut levels = vec![0i32; side * side];
                levels[0] = level;
                levels[1] = -level;
                levels[side + 1] = level * 2;
                let pos = crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
                let negated: Vec<i32> = levels.iter().map(|&l| -l).collect();
                let neg =
                    crate::transform::dequant_and_inverse(&negated, side, 8, i32::from(Q_IDX));
                let worst = pos
                    .iter()
                    .zip(&neg)
                    .map(|(&p, &n)| (p + n).abs())
                    .max()
                    .expect("the block is not empty");
                assert!(worst <= 1, "side {side} level {level}: off by {worst}");
            }
        }
    }

    /// The pinned tables above are DC-only, and a flat residual cannot tell a
    /// correct butterfly network from one that is merely correct at DC. This
    /// is the same claim against a real decoder and with real AC content: the
    /// top-left block of a key frame predicts a flat mid-grey with no
    /// neighbours to read, so what ffmpeg shows there is exactly 128 plus the
    /// residual the encoder's own inverse transform computes. Every sample has
    /// to agree, not most of them.
    #[test]
    fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block: no ffmpeg"
            );
            return;
        }
        // A DC, two low-frequency terms of each orientation, a diagonal and a
        // far corner, with both signs among them.
        let coeffs = [
            (0u8, 0u8, 9i32),
            (0, 1, -14),
            (1, 0, 7),
            (0, 3, 5),
            (3, 0, -3),
            (2, 2, 11),
            (7, 5, -6),
            (31, 31, 4),
        ];
        let block: BlockCoeffs = coeffs
            .iter()
            .map(|&(row, col, level)| Coeff { row, col, level })
            .collect::<Vec<_>>()
            .into();
        let rows = decode_luma_at(
            64,
            64,
            &[
                block,
                BlockCoeffs::default(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
            ],
        );

        let mut levels = vec![0i32; 32 * 32];
        for &(row, col, level) in &coeffs {
            levels[usize::from(row) * 32 + usize::from(col)] = level;
        }
        let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
        for row in 0..32 {
            for col in 0..32 {
                let want = (128 + residual[row * 32 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                    rows[row][col]
                );
            }
        }
        // The block is not flat, so the agreement is about the transform and
        // not about a prediction both sides got trivially right.
        let (lo, hi) = (
            rows[..32].iter().flat_map(|r| &r[..32]).min().unwrap(),
            rows[..32].iter().flat_map(|r| &r[..32]).max().unwrap(),
        );
        assert!(hi - lo > 4, "the block carries real detail, got {lo}..{hi}");
    }

    /// The same against the other transform size, the 64x64 one a whole
    /// superblock carries. Only its top-left 32x32 can hold coefficients, and
    /// the spec zeroes the rest before the row transform — a 64-point network
    /// that read the missing half as anything else would disagree here.
    #[test]
    fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block: no ffmpeg"
            );
            return;
        }
        let coeffs = [
            (0u8, 0u8, 12i32),
            (0, 2, -9),
            (2, 0, 6),
            (1, 1, -5),
            (5, 9, 8),
            (31, 0, -4),
            (0, 31, 3),
        ];
        let block: BlockCoeffs = coeffs
            .iter()
            .map(|&(row, col, level)| Coeff { row, col, level })
            .collect::<Vec<_>>()
            .into();
        let rows = decode_luma_sb(64, 64, &[Superblock::Whole(block)]);

        let mut levels = vec![0i32; 64 * 64];
        for &(row, col, level) in &coeffs {
            levels[usize::from(row) * 64 + usize::from(col)] = level;
        }
        let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
        for row in 0..64 {
            for col in 0..64 {
                let want = (128 + residual[row * 64 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                    rows[row][col]
                );
            }
        }
    }

    /// The AV1 transforms are normalized so that a quantized level means the
    /// same thing at every size: doubling the transform's side halves what a
    /// DC level is worth, which is the job the per-size row shift and the
    /// dequantizer's denominator split between them. The frame syntax only
    /// codes 32x32 and 64x64 transforms, so this is what holds the four
    /// smaller sizes' shifts to the spec's table — a shift wrong by one at any
    /// size doubles or halves that size's step alone.
    #[test]
    fn a_dc_level_is_worth_half_as_much_each_time_the_transform_doubles() {
        let mut previous: Option<i32> = None;
        for side in [4usize, 8, 16, 32, 64] {
            let mut levels = vec![0i32; side * side];
            levels[0] = 100;
            let residual =
                crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
            let dc = residual[0];
            assert!(
                residual.iter().all(|&r| r == dc),
                "side {side}: a DC coefficient reconstructs flat"
            );
            if let Some(previous) = previous {
                assert!(
                    (dc * 2 - previous).abs() <= 1,
                    "side {side}: {dc} is not half of the previous size's {previous}"
                );
            }
            previous = Some(dc);
        }
    }

    /// The encoder's own round trip, through a real bitstream.
    ///
    /// A picture is chosen, its residual against the DC prediction of 128 is
    /// forward-transformed and quantized, the levels are written into a tile,
    /// and ffmpeg decodes it. Two things have to hold: what comes back is
    /// exactly what the encoder's own inverse said it would be — sample for
    /// sample, which is what lets a rate-distortion loop trust its
    /// reconstruction — and it is within a quantizer step of the picture that
    /// was asked for, which is what makes the forward transform an encoder
    /// rather than a scrambler.
    #[test]
    fn a_quantized_picture_decodes_to_what_the_encoder_reconstructed() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_quantized_picture_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        // A gradient, a ripple and a corner patch: low frequencies, a mid one
        // and an edge, none of them a basis function of the transform.
        let mut residual = vec![0i32; 32 * 32];
        for row in 0..32usize {
            for col in 0..32usize {
                let gradient = row as f64 * 2.0 - 32.0;
                let ripple = 25.0
                    * (col as f64 * std::f64::consts::PI / 6.0).sin()
                    * (row as f64 * std::f64::consts::PI / 11.0).cos();
                let patch = if row >= 24 && col >= 20 { -40.0 } else { 0.0 };
                residual[row * 32 + col] = (gradient + ripple + patch).round() as i32;
            }
        }

        let levels =
            crate::transform::forward_and_quantize(&residual, 32, 8, i32::from(Q_IDX), 0.5);
        assert!(
            levels.iter().any(|&l| l != 0),
            "the picture quantized away entirely"
        );
        let coded: Vec<Coeff> = levels
            .iter()
            .enumerate()
            .filter(|&(_, &level)| level != 0)
            .map(|(i, &level)| Coeff {
                row: (i / 32) as u8,
                col: (i % 32) as u8,
                level,
            })
            .collect();

        let rows = decode_luma_at(
            64,
            64,
            &[
                coded.into(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
            ],
        );
        let reconstruction =
            crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));

        let mut squared = 0.0f64;
        for row in 0..32 {
            for col in 0..32 {
                let want = (128 + reconstruction[row * 32 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "sample ({row},{col}): the decoder and the encoder disagree"
                );
                let error = f64::from(rows[row][col]) - f64::from(128 + residual[row * 32 + col]);
                squared += error * error;
            }
        }
        let rmse = (squared / 1024.0).sqrt();
        // A quantizer step is q/8 in residual units. Rounding to nearest
        // spreads the error over a step, and a quarter of one is a bound this
        // picture clears with room to spare (2.2 of 14 as written) while a
        // forward transform that is merely energy-preserving does not: a
        // transposed column pass measures 13.3 here.
        let step = f64::from(crate::quant::ac_q(8, i32::from(Q_IDX))) / 8.0;
        assert!(
            rmse < step / 4.0,
            "the decoded picture is {rmse} off, a step being {step}"
        );
    }
}
