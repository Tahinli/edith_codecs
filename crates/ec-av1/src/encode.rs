//! A key-frame picture encoder: a planar 4:2:0 picture in, an AV1 stream out.
//!
//! This is the layer that turns the pieces below it — the intra predictors of
//! [`crate::intra`], the forward transform and quantizer of
//! [`crate::transform`], and the tile writer of [`crate::tile`] — into
//! something that takes a picture. Every block is 32x32 (its chroma 16x16),
//! which is the subset the tile writer codes, and each one picks its luma mode
//! from the seven non-directional ones by rate-distortion; chroma is predicted
//! DC, which is the mode the tile writer codes for it. Block sizes and
//! transform types are what a loop above this would choose between.
//!
//! The encoder carries its own reconstruction, because prediction reads it: a
//! block predicts from the reconstructed samples above and to its left, exactly
//! as the decoder will. That makes the reconstruction a claim about what a
//! decoder produces, and it is gated as one — sample for sample against ffmpeg.

use ec_av1_syntax::sequence::{ColorConfig, OperatingPoint, SequenceHeader};
use ec_av1_syntax::{
    ChromaSamplePosition, FrameHeader, FrameType, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
    TxMode,
};
use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::TxbSet;
use crate::frame::frame_obu;
use crate::intra::{D67_PRED, DC_PRED, KEY_FRAME_MODES, V_PRED};
use crate::obu::temporal_delimiter;
use crate::quant::ac_q;
use crate::sequence::sequence_header_obu;
use crate::tile::{
    BlockCoeffs, Coeff, INTRA_MODE_CTX, Quadrant, Superblock, partition_bits,
    sb_coeff_key_frame_tile,
};
use crate::transform::{dequant_and_inverse, forward_and_quantize};

/// The side of the larger of the two luma blocks this encoder codes, in
/// samples.
const BLOCK: usize = 32;

/// The side of the smaller one, which a 32x32 block may be split into four of.
const SUB: usize = 16;

/// The side of a superblock, which is what the partition tree starts from.
const SUPERBLOCK: usize = 64;

/// How heavily the mode search weighs rate against squared error, in units of
/// the quantizer's reconstruction step squared per bit.
///
/// Swept three times over his clips and three synthetic pictures
/// (`probe_lambda`, `probe_directional` and `probe_ladder`, and the tables in
/// the three lane reports). The first sweep, before the mode symbol was
/// costed, put the best point at 0.05; costing it moved the point to 0.1. The
/// third sweep, after the levels were costed through the writer's own CDFs
/// too, left it there: against 0.2 the ladders are worth -1.19% against -0.98%
/// on film and -0.20% against -0.14% on screen capture.
const LAMBDA_SCALE: f64 = 0.1;

/// [`LAMBDA_SCALE`], or whatever `EC_AV1_LAMBDA` names when the sweep that
/// picks it is the thing running. A release build has no such knob.
fn lambda_scale() -> f64 {
    let swept = std::env::var("EC_AV1_LAMBDA")
        .ok()
        .and_then(|v| v.parse::<f64>().ok());
    match swept {
        Some(scale) if cfg!(test) => scale,
        _ => LAMBDA_SCALE,
    }
}

/// Whether a 32x32 block may be split into four 16x16 ones when the trial says
/// four cost less. Set from the measurement in the lane report.
const SPLIT_BLOCKS: bool = true;

/// What [`encode_key_frame_with_modes`] codes with, which the sweep overrides
/// by calling [`encode_key_frame_inner`] both ways.
fn split_blocks() -> bool {
    SPLIT_BLOCKS
}

/// One 8-bit planar 4:2:0 picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The picture's width in luma samples.
    pub width: usize,
    /// Its height in luma samples.
    pub height: usize,
    /// The luma plane, `width * height` samples in raster order.
    pub y: Vec<u8>,
    /// The U plane, at half the width and half the height.
    pub u: Vec<u8>,
    /// The V plane, the same shape as U.
    pub v: Vec<u8>,
}

impl Picture {
    /// A mid-grey picture of the given size.
    #[must_use]
    pub fn grey(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            y: vec![128; width * height],
            u: vec![128; width * height / 4],
            v: vec![128; width * height / 4],
        }
    }

    fn check(&self) -> Result<()> {
        if self.width == 0
            || !self.width.is_multiple_of(BLOCK)
            || !self.height.is_multiple_of(BLOCK)
            || !self.height.is_multiple_of(BLOCK)
        {
            return Err(Error::unsupported(
                "AV1 encode",
                "the picture must be a whole number of 32x32 blocks in each direction",
            ));
        }
        let (luma, chroma) = (self.width * self.height, self.width * self.height / 4);
        if self.y.len() != luma || self.u.len() != chroma || self.v.len() != chroma {
            return Err(Error::unsupported(
                "AV1 encode",
                "each plane must carry one sample per position at 4:2:0",
            ));
        }
        Ok(())
    }
}

/// What one call to [`encode_key_frame`] produced.
#[derive(Clone, Debug)]
pub struct Encoded {
    /// The AV1 stream: a temporal delimiter, a sequence header OBU and a frame
    /// OBU carrying one tile.
    pub stream: Vec<u8>,
    /// What a decoder will produce from `stream` — the encoder's own
    /// reconstruction, which its prediction was built on.
    pub reconstruction: Picture,
    /// The luma intra mode each block was coded under, in the order the blocks
    /// are coded — raster order among the quadrants of each superblock, and
    /// among the four 16x16 blocks of a quadrant that was split.
    pub modes: Vec<u8>,
}

/// The sequence and frame headers a picture of this size is coded under: one
/// tile, one transform size per block, no in-loop filtering, no CDF adaptation
/// and no tool the tile writer does not code.
///
/// # Errors
/// Returns an error when the picture is larger than the 16-bit frame size the
/// sequence header carries.
pub fn key_frame_headers(
    width: usize,
    height: usize,
    base_q_idx: u8,
) -> Result<(SequenceHeader, FrameHeader)> {
    let bits = |n: usize| -> Result<u32> {
        let n = u32::try_from(n).map_err(|_| too_large())?;
        if n == 0 || n > 1 << 16 {
            return Err(too_large());
        }
        Ok((32 - (n - 1).leading_zeros()).max(1))
    };
    let (frame_width_bits, frame_height_bits) = (bits(width)?, bits(height)?);
    let (w, h) = (width as u32, height as u32);
    let seq = SequenceHeader {
        seq_profile: 0,
        operating_points: vec![OperatingPoint {
            seq_level_idx: 8,
            ..OperatingPoint::default()
        }],
        frame_width_bits,
        frame_height_bits,
        max_frame_width: w,
        max_frame_height: h,
        use_128x128_superblock: false,
        enable_filter_intra: false,
        enable_intra_edge_filter: false,
        enable_order_hint: true,
        order_hint_bits: 7,
        enable_superres: false,
        enable_cdef: false,
        enable_restoration: false,
        seq_force_screen_content_tools: 0,
        seq_force_integer_mv: 0,
        color_config: ColorConfig {
            bit_depth: 8,
            mono_chrome: false,
            num_planes: 3,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
            chroma_sample_position: ChromaSamplePosition::Unknown,
            separate_uv_delta_q: false,
        },
        still_picture: false,
        reduced_still_picture_header: false,
        timing_info: None,
        decoder_model_info: None,
        initial_display_delay_present_flag: false,
        operating_point: 0,
        operating_point_idc: 0,
        frame_id_numbers_present_flag: false,
        delta_frame_id_length: 0,
        additional_frame_id_length: 0,
        enable_interintra_compound: false,
        enable_masked_compound: false,
        enable_warped_motion: false,
        enable_dual_filter: false,
        enable_jnt_comp: false,
        enable_ref_frame_mvs: false,
        film_grain_params_present: false,
    };
    let (mi_cols, mi_rows) = (w / 4, h / 4);
    let header = FrameHeader {
        frame_type: FrameType::Key,
        frame_is_intra: true,
        show_frame: true,
        error_resilient_mode: true,
        // The tile writer keeps the same CDF state the decoder does and
        // updates it in the same order, so the frame lets the decoder adapt.
        disable_cdf_update: false,
        // Nothing reads the state this frame leaves behind: a key frame always
        // starts from the defaults, and this encoder emits one frame.
        disable_frame_end_update_cdf: true,
        force_integer_mv: true,
        refresh_frame_flags: 0xFF,
        primary_ref_frame: PRIMARY_REF_NONE,
        frame_width: w,
        frame_height: h,
        upscaled_width: w,
        render_width: w,
        render_height: h,
        mi_cols,
        mi_rows,
        tile_info: TileInfo {
            uniform_spacing: true,
            cols: 1,
            rows: 1,
            mi_col_starts: vec![0, mi_cols],
            mi_row_starts: vec![0, mi_rows],
            tile_size_bytes: 1,
            ..TileInfo::default()
        },
        quantization: QuantizationParams {
            base_q_idx,
            ..QuantizationParams::default()
        },
        tx_mode: TxMode::Largest,
        ..FrameHeader::default()
    };
    Ok((seq, header))
}

fn too_large() -> Error {
    Error::unsupported(
        "AV1 encode",
        "the picture is larger than a frame size can carry",
    )
}

/// One plane of the picture being coded, and the reconstruction being built
/// beside it.
struct Plane<'a> {
    source: &'a [u8],
    reconstruction: Vec<u8>,
    width: usize,
    height: usize,
}

impl Plane<'_> {
    /// The reconstructed samples a block at `(x, y)` predicts from: the row
    /// above it, the column to its left and the sample between them, each
    /// missing where the block sits against an edge of the frame.
    ///
    /// `reach` says whether the samples above the block's right and below its
    /// left are decoded, which is what a directional mode reads into; where
    /// they are not, or where the frame ends first, the edge is shorter and
    /// the predictor repeats its last sample, exactly as the decoder's clamp
    /// to `aboveLimit` and `leftLimit` does (spec 7.11.2.2).
    fn edges(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<u8>) {
        let across = (x + if reach.above_right { 2 * side } else { side }).min(self.width);
        let down = (y + if reach.below_left { 2 * side } else { side }).min(self.height);
        let above =
            (y > 0).then(|| self.reconstruction[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > 0).then(|| {
            (y..down)
                .map(|row| self.reconstruction[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner = (x > 0 && y > 0).then(|| self.reconstruction[(y - 1) * self.width + x - 1]);
        (above, left, corner)
    }

    /// Codes one block under one mode without committing it: hands back the
    /// levels, the block the decoder would reconstruct, the squared error
    /// against the source and an estimate of what the levels cost in bits.
    fn trial(&self, at: At, mode: u8, base_q_idx: u8, deadzone: f64) -> Trial {
        let At {
            x, y, side, reach, ..
        } = at;
        let (above, left, corner) = self.edges(x, y, side, reach);
        let mut prediction = vec![0u8; side * side];
        crate::intra::predict(
            mode,
            above.as_deref(),
            left.as_deref(),
            corner,
            side,
            &mut prediction,
        );

        let mut residual = vec![0i32; side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        let levels = forward_and_quantize(&residual, side, 8, i32::from(base_q_idx), deadzone);
        let coded = dequant_and_inverse(&levels, side, 8, i32::from(base_q_idx));

        let mut reconstruction = vec![0u8; side * side];
        let mut sse = 0.0;
        for row in 0..side {
            for col in 0..side {
                let i = row * side + col;
                let sample = (i32::from(prediction[i]) + coded[i]).clamp(0, 255) as u8;
                reconstruction[i] = sample;
                let error = f64::from(
                    i32::from(self.source[(y + row) * self.width + x + col]) - i32::from(sample),
                );
                sse += error * error;
            }
        }
        // What the levels cost is priced through the same CDFs the tile writer
        // will code them with, so the search ranks modes -- and the partition
        // trial ranks trees -- by the bits they actually spend.
        let bits = if cfg!(test) && std::env::var_os("EC_AV1_ESTIMATE").is_some() {
            // What the search used before it could price a block exactly: a
            // level costs its magnitude's width plus a sign and a run. Kept
            // reachable from the sweep so the two rate terms can be compared
            // on one build.
            levels
                .iter()
                .filter(|&&level| level != 0)
                .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
                .sum()
        } else {
            crate::tile::coeff_bits(&levels, at.set)
        };
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// Writes a trial's reconstruction back into the plane.
    fn commit(&mut self, x: usize, y: usize, side: usize, trial: &Trial) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&trial.reconstruction[row * side..][..side]);
        }
    }

    /// Codes one block under a fixed mode, committing it, and hands back what
    /// it cost: its squared error and the bits its levels spend.
    fn code_block(
        &mut self,
        at: At,
        mode: u8,
        base_q_idx: u8,
        deadzone: f64,
    ) -> (Vec<Coeff>, f64, f64) {
        let trial = self.trial(at, mode, base_q_idx, deadzone);
        self.commit(at.x, at.y, at.side, &trial);
        (coeffs(&trial.levels, at.side), trial.sse, trial.bits)
    }

    /// The reconstructed samples of one square, so that a partition trial can
    /// be undone.
    fn snapshot(&self, x: usize, y: usize, side: usize) -> Vec<u8> {
        (y..y + side)
            .flat_map(|row| self.reconstruction[row * self.width + x..][..side].to_vec())
            .collect()
    }

    /// Puts a snapshot back.
    fn restore(&mut self, x: usize, y: usize, side: usize, samples: &[u8]) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&samples[row * side..][..side]);
        }
    }

    /// Codes one block under every mode the search offers and commits the one
    /// whose squared error and estimated rate come out cheapest.
    fn search_block(
        &mut self,
        at: At,
        search: &Search,
        mode_bits: &[f64; 13],
    ) -> (Vec<Coeff>, u8, f64) {
        let mut best: Option<(f64, u8, Trial)> = None;
        for &mode in search.modes {
            let trial = self.trial(at, mode, search.base_q_idx, search.deadzone);
            let cost = trial.sse + search.lambda * (trial.bits + mode_bits[usize::from(mode)]);
            if best
                .as_ref()
                .is_none_or(|(best_cost, _, _)| cost < *best_cost)
            {
                best = Some((cost, mode, trial));
            }
        }
        let (cost, mode, trial) = best.expect("the search offers at least one mode");
        self.commit(at.x, at.y, at.side, &trial);
        (coeffs(&trial.levels, at.side), mode, cost)
    }
}

/// Where one block sits in its plane, and how far past its own edges its
/// prediction may read.
#[derive(Clone, Copy)]
struct At {
    x: usize,
    y: usize,
    side: usize,
    reach: Reach,
    /// Which of the tile writer's coefficient tables this block's levels are
    /// coded with, and so which the search prices them through.
    set: TxbSet,
}

/// Which of the samples past a block's own edges the decoder has decoded by
/// the time it predicts the block, and so which a directional mode may read.
///
/// The decoder derives these from its `BlockDecoded` flags, which are cleared
/// per superblock (`clear_block_decoded_flags`, spec 7.4). Rather than carry
/// that map, this reads the answer the same way libaom and rav1e do: for a
/// block whose transform covers it whole, whether the samples above its right
/// (or below its left) are decoded depends only on where the block sits inside
/// its 64x64 superblock, which is a pinned table per block size.
#[derive(Clone, Copy)]
struct Reach {
    above_right: bool,
    below_left: bool,
}

/// `has_tr_16x16` / `has_tr_32x32` (`recon_intra.rs` in rav1e, `has_tr_*` in
/// libaom): for a block that is neither in the superblock's top row nor its
/// rightmost column, whether the block above and to its right is coded before
/// it. Indexed by the block's position in the superblock, in blocks of its own
/// size, as `row * (16 >> log2 mi width) + col`, bit by bit from the low end.
const HAS_TOP_RIGHT: [&[u8]; 2] = [&[255, 85, 119, 85, 127, 85, 119, 85], &[95, 87]];

/// `has_bl_16x16` / `has_bl_32x32`: the same, for the block below and to the
/// left.
const HAS_BOTTOM_LEFT: [&[u8]; 2] = [&[84, 16, 84, 0, 84, 16, 84, 0], &[4, 4]];

impl Reach {
    /// What a block of `side` samples at `(x, y)` may read past its own edges,
    /// in a frame of `width` by `height`.
    fn of(side: usize, x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            above_right: y > 0 && x + side < width && Self::top_right(side, x, y),
            below_left: x > 0 && y + side < height && Self::bottom_left(side, x, y),
        }
    }

    /// Neither, which is all a mode that reads no further than its own edges
    /// needs.
    fn none() -> Self {
        Self {
            above_right: false,
            below_left: false,
        }
    }

    /// `has_top_right` for a block whose transform covers it whole: the top
    /// row of a superblock reads into the superblock above, which is decoded;
    /// the rightmost column would read into the superblock to the right, which
    /// is not; and everything between is the table's to answer.
    fn top_right(side: usize, x: usize, y: usize) -> bool {
        let (row, col, per_side) = Self::position(side, x, y);
        if row == 0 {
            return true;
        }
        if col + 1 == per_side {
            return false;
        }
        Self::bit(HAS_TOP_RIGHT[Self::table(side)], row * per_side + col)
    }

    /// `has_bottom_left` for such a block: the leftmost column of a superblock
    /// reads into the superblock to its left, which is decoded only as far
    /// down as its own bottom; the bottom row would read into the superblock
    /// below, which is not decoded at all; and the table answers the rest.
    fn bottom_left(side: usize, x: usize, y: usize) -> bool {
        let (row, col, per_side) = Self::position(side, x, y);
        if col == 0 {
            return row * side + side < SUPERBLOCK;
        }
        if row + 1 == per_side {
            return false;
        }
        Self::bit(HAS_BOTTOM_LEFT[Self::table(side)], row * per_side + col)
    }

    /// Where a block sits inside its superblock, in blocks of its own size,
    /// and how many of them a superblock is across.
    fn position(side: usize, x: usize, y: usize) -> (usize, usize, usize) {
        (
            (y % SUPERBLOCK) / side,
            (x % SUPERBLOCK) / side,
            SUPERBLOCK / side,
        )
    }

    /// Which of the two block sizes the encoder codes a table row is for.
    fn table(side: usize) -> usize {
        usize::from(side == BLOCK)
    }

    /// One bit of a table, counting from the low end of its first byte.
    fn bit(table: &[u8], index: usize) -> bool {
        (table[index / 8] >> (index % 8)) & 1 != 0
    }
}

/// What one symbol costs against a CDF, in bits.
pub(crate) fn symbol_bits(cdf: &[u16], symbol: usize) -> f64 {
    let low = if symbol == 0 { 0 } else { cdf[symbol - 1] };
    let probability = f64::from(cdf[symbol] - low) / 32768.0;
    -probability.log2()
}

/// What the tile writer spends to say a block is coded in each of the thirteen
/// modes, given the modes of the blocks above it and to its left: the luma mode
/// symbol against the CDF those two neighbours pick, the angle delta a
/// directional mode carries, and the chroma mode symbol, whose CDF the luma
/// mode itself indexes.
///
/// Without this the search is blind to what a mode costs to name, which is not
/// a rounding error: a directional mode on a picture that does not run that way
/// is a rate loss the squared error never sees.
/// Codes one square of the picture as a single block: searches the luma mode,
/// codes both chroma planes DC, and hands back the block and what it cost --
/// squared error plus lambda times the bits its symbols spend.
fn code_square(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
) -> (BlockCoeffs, f64) {
    let (luma_set, chroma_set) = if side == BLOCK {
        (TxbSet::Luma32, TxbSet::Chroma16)
    } else {
        (TxbSet::Luma16, TxbSet::Chroma8)
    };
    let (luma_coeffs, mode, mut cost) = luma.search_block(
        At {
            x,
            y,
            side,
            reach: Reach::of(side, x, y, luma.width, luma.height),
            set: luma_set,
        },
        search,
        mode_bits,
    );
    // Chroma is predicted DC because that is the mode the tile writer codes
    // for it; what it costs still counts towards the partition decision.
    let mut planes = [Vec::new(), Vec::new()];
    for (plane, coeffs) in chroma.iter_mut().zip(&mut planes) {
        let (levels, sse, bits) = plane.code_block(
            At {
                x: x / 2,
                y: y / 2,
                side: side / 2,
                reach: Reach::none(),
                set: chroma_set,
            },
            DC_PRED,
            search.base_q_idx,
            search.deadzone,
        );
        *coeffs = levels;
        cost += sse + search.lambda * bits;
    }
    let [u, v] = planes;
    (
        BlockCoeffs {
            u,
            v,
            luma: luma_coeffs,
            mode,
            ..BlockCoeffs::default()
        },
        cost,
    )
}

/// The reconstructed samples one 32x32 square covers in all three planes, so
/// that a partition trial can be undone.
fn snapshot(luma: &Plane, chroma: &[Plane; 2], (x, y): (usize, usize)) -> [Vec<u8>; 3] {
    [
        luma.snapshot(x, y, BLOCK),
        chroma[0].snapshot(x / 2, y / 2, BLOCK / 2),
        chroma[1].snapshot(x / 2, y / 2, BLOCK / 2),
    ]
}

/// Puts such a snapshot back.
fn restore(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    saved: &[Vec<u8>; 3],
) {
    luma.restore(x, y, BLOCK, &saved[0]);
    chroma[0].restore(x / 2, y / 2, BLOCK / 2, &saved[1]);
    chroma[1].restore(x / 2, y / 2, BLOCK / 2, &saved[2]);
}

fn mode_bits(above_mode: u8, left_mode: u8) -> [f64; 13] {
    let luma = &cdf::KF_Y_MODE[INTRA_MODE_CTX[usize::from(above_mode)]]
        [INTRA_MODE_CTX[usize::from(left_mode)]];
    std::array::from_fn(|mode| {
        let angle = if (usize::from(V_PRED)..=usize::from(D67_PRED)).contains(&mode) {
            symbol_bits(&cdf::ANGLE_DELTA[mode - usize::from(V_PRED)], 3)
        } else {
            0.0
        };
        symbol_bits(luma, mode) + angle + symbol_bits(&cdf::UV_MODE_CFL[mode], usize::from(DC_PRED))
    })
}

/// What the luma mode search picks between, and under what terms.
struct Search<'a> {
    base_q_idx: u8,
    deadzone: f64,
    lambda: f64,
    modes: &'a [u8],
}

/// One mode's coding of one block, before it is committed.
struct Trial {
    levels: Vec<i32>,
    reconstruction: Vec<u8>,
    sse: f64,
    bits: f64,
}

/// The non-zero levels of a block, as the tile writer takes them.
fn coeffs(levels: &[i32], side: usize) -> Vec<Coeff> {
    levels
        .iter()
        .enumerate()
        .filter(|&(_, &level)| level != 0)
        .map(|(i, &level)| Coeff {
            row: (i / side) as u8,
            col: (i % side) as u8,
            level,
        })
        .collect()
}

/// Encodes one picture as a key frame.
///
/// `base_q_idx` is the frame's quantizer index, and has to sit in the band
/// whose coefficient CDFs the tile writer carries (61..=120). `deadzone` is the
/// quantizer's rounding offset: 0.5 rounds to nearest, and smaller values trade
/// fidelity for rate.
///
/// # Errors
/// Returns an error when the picture is not a whole number of 32x32 blocks,
/// when its planes are not 4:2:0 of that size, or when the tile writer refuses
/// the quantizer index.
pub fn encode_key_frame(picture: &Picture, base_q_idx: u8, deadzone: f64) -> Result<Encoded> {
    encode_key_frame_with_modes(picture, base_q_idx, deadzone, &KEY_FRAME_MODES)
}

/// Encodes one picture as a key frame, choosing each block's luma mode from
/// `modes` alone.
///
/// This is what an ablation measures against: `&[DC_PRED]` is the encoder
/// before the mode search, [`crate::intra::NON_DIRECTIONAL`] is it before the
/// directional modes, and [`KEY_FRAME_MODES`] is what [`encode_key_frame`]
/// uses.
///
/// # Errors
/// The same as [`encode_key_frame`], and additionally when `modes` is empty or
/// names a mode [`crate::intra::predict`] does not predict.
pub fn encode_key_frame_with_modes(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
) -> Result<Encoded> {
    encode_key_frame_inner(picture, base_q_idx, deadzone, modes, split_blocks())
}

/// [`encode_key_frame_with_modes`] with the partition decision forced, which is
/// what the sweep that sets [`SPLIT_BLOCKS`] measures both ways.
fn encode_key_frame_inner(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
    split_blocks: bool,
) -> Result<Encoded> {
    picture.check()?;
    if modes.is_empty() {
        return Err(Error::unsupported(
            "AV1 encode",
            "a mode search needs at least one mode to choose from",
        ));
    }
    if let Some(bad) = modes.iter().find(|m| !KEY_FRAME_MODES.contains(m)) {
        return Err(Error::unsupported(
            "AV1 encode",
            format!("intra mode {bad} is not one the encoder predicts"),
        ));
    }
    let (seq, header) = key_frame_headers(picture.width, picture.height, base_q_idx)?;

    let mut luma = Plane {
        source: &picture.y,
        reconstruction: vec![128; picture.y.len()],
        width: picture.width,
        height: picture.height,
    };
    let mut chroma = [
        Plane {
            source: &picture.u,
            reconstruction: vec![128; picture.u.len()],
            width: picture.width / 2,
            height: picture.height / 2,
        },
        Plane {
            source: &picture.v,
            reconstruction: vec![128; picture.v.len()],
            width: picture.width / 2,
            height: picture.height / 2,
        },
    ];

    // The search trades a bit for the squared error it saves, in the units the
    // reconstruction is measured in: one step of the quantizer, squared.
    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale() * step * step,
        modes,
    };

    let (cols, rows) = (picture.width / BLOCK, picture.height / BLOCK);
    // The luma mode of the block above and of the block to the left, which is
    // what picks the CDF the next block's mode is coded against -- the same
    // bookkeeping the tile writer keeps, so that the search is costing the
    // symbol the writer will actually write.
    // The bookkeeping is kept on the 16x16 grid the tile writer keeps it on,
    // because a 32x32 block may be split into four 16x16 ones.
    let mut above_mode = vec![DC_PRED; cols * 2];
    let mut left_mode = vec![DC_PRED; rows * 2];
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let mut superblocks = Vec::with_capacity(sb_cols * sb_rows);
    let mut modes = Vec::with_capacity(cols * rows);
    for sb_row in 0..sb_rows {
        for sb_col in 0..sb_cols {
            // The quadrants of a superblock are coded in the order the decoder
            // walks them, which for a 64x64 split into 32x32 blocks is raster
            // order among the quadrants that are inside the frame.
            let mut blocks = Vec::with_capacity(4);
            for quadrant in 0..4 {
                let (col, row) = (sb_col * 2 + quadrant % 2, sb_row * 2 + quadrant / 2);
                if col >= cols || row >= rows {
                    continue;
                }
                let (x, y) = (col * BLOCK, row * BLOCK);
                let (c0, r0) = (col * 2, row * 2);
                let base = snapshot(&luma, &chroma, (x, y));

                // What the whole 32x32 costs, including the partition symbol
                // that says it is not split.
                let (whole, mut cost_whole) = code_square(
                    &mut luma,
                    &mut chroma,
                    (x, y),
                    BLOCK,
                    &search,
                    &mode_bits(above_mode[c0], left_mode[r0]),
                );
                cost_whole += search.lambda * partition_bits(BLOCK, false);
                let after_whole = snapshot(&luma, &chroma, (x, y));

                // What four 16x16 blocks cost instead, each searched against
                // the reconstruction the ones before it left.
                restore(&mut luma, &mut chroma, (x, y), &base);
                let mut split = Vec::with_capacity(4);
                let mut cost_split = search.lambda
                    * (partition_bits(BLOCK, true) + 4.0 * partition_bits(SUB, false));
                let mut split_modes = [DC_PRED; 4];
                for (sub, sub_mode) in split_modes.iter_mut().enumerate() {
                    let (sc, sr) = (c0 + sub % 2, r0 + sub / 2);
                    let (block, cost) = code_square(
                        &mut luma,
                        &mut chroma,
                        (x + (sub % 2) * SUB, y + (sub / 2) * SUB),
                        SUB,
                        &search,
                        &mode_bits(above_mode[sc], left_mode[sr]),
                    );
                    cost_split += cost;
                    *sub_mode = block.mode;
                    above_mode[sc] = block.mode;
                    left_mode[sr] = block.mode;
                    split.push(block);
                }

                if split_blocks && cost_split < cost_whole {
                    modes.extend_from_slice(&split_modes);
                    blocks.push(Quadrant::Split(split));
                } else {
                    restore(&mut luma, &mut chroma, (x, y), &after_whole);
                    for cell in 0..2 {
                        above_mode[c0 + cell] = whole.mode;
                        left_mode[r0 + cell] = whole.mode;
                    }
                    modes.push(whole.mode);
                    blocks.push(Quadrant::Whole(whole));
                }
            }
            superblocks.push(Superblock::Split(blocks));
        }
    }

    let tile = sb_coeff_key_frame_tile(header.mi_cols, header.mi_rows, base_q_idx, &superblocks)?;
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq)?);
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile)?);

    let [u, v] = chroma;
    Ok(Encoded {
        stream,
        modes,
        reconstruction: Picture {
            width: luma.width,
            height: luma.height,
            y: luma.reconstruction,
            u: u.reconstruction,
            v: v.reconstruction,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intra::{D135_PRED, D45_PRED, H_PRED, KEY_FRAME_MODES, NON_DIRECTIONAL, V_PRED};
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the three planes.
    fn ffmpeg_decode(stream: &[u8], width: usize, height: usize) -> Picture {
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
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        }
    }

    /// A picture with something of everything in it: a gradient, an edge, a
    /// ripple and a block of flat colour, none of them aligned to the block
    /// grid.
    fn test_card(width: usize, height: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let gradient = x as f64 * 200.0 / width as f64;
                let ripple = 30.0
                    * (x as f64 * std::f64::consts::PI / 23.0).sin()
                    * (y as f64 * std::f64::consts::PI / 37.0).cos();
                let edge = if x > width * 3 / 7 && y > height / 3 {
                    40.0
                } else {
                    0.0
                };
                picture.y[y * width + x] =
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u8;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (x * 60 / (width / 2))) as u8;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u8;
            }
        }
        picture
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let squared: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum();
        if squared == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0 * 255.0 * a.len() as f64 / squared).log10()
    }

    /// The claim the whole encoder rests on: what a decoder produces is what
    /// the encoder said it would, sample for sample, on every plane.
    ///
    /// Prediction reads the reconstruction, so a single sample of drift
    /// anywhere would spread into every block below and to the right of it —
    /// which is why this is an equality and not a tolerance.
    #[test]
    fn ffmpeg_decodes_exactly_what_the_encoder_reconstructed() {
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_decodes_exactly_what_the_encoder_reconstructed: no ffmpeg");
            return;
        }
        for &(width, height) in &[(64usize, 64usize), (96, 64), (160, 96)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(
                decoded.y, encoded.reconstruction.y,
                "{width}x{height}: luma"
            );
            assert_eq!(decoded.u, encoded.reconstruction.u, "{width}x{height}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "{width}x{height}: V");
        }
    }

    /// One frame of a real clip, scaled to a whole number of 32x32 blocks.
    fn clip_frame(clip: &str, skip: &str, width: usize, height: usize) -> Picture {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip, "-frames:v", "1"])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        }
    }

    /// Prints what the mode search saves over DC prediction alone, for the
    /// synthetic pictures and for whatever clips `EC_AV1_CLIPS` names, so the
    /// weight the search puts on rate can be swept.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_lambda() {
        let mut pictures = vec![
            ("test card".to_string(), test_card(160, 96)),
            ("stripes".to_string(), stripes(160, 96, true)),
        ];
        if let Ok(clips) = std::env::var("EC_AV1_CLIPS") {
            for entry in clips.split(':').filter(|e| !e.is_empty()) {
                let (path, skip) = entry.split_once('@').unwrap_or((entry, "0"));
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                pictures.push((name, clip_frame(path, skip, 640, 352)));
            }
        }
        for (name, picture) in pictures {
            let dc = ladder(&picture, &[DC_PRED]);
            let searched = ladder(&picture, &NON_DIRECTIONAL);
            println!(
                "{name}: {:+.1}% rate against DC alone, {:.0}B/{:.2}dB against {:.0}B/{:.2}dB at the middle point",
                bd_rate(&dc, &searched) * 100.0,
                10f64.powf(searched[1].1),
                searched[1].0,
                10f64.powf(dc[1].1),
                dc[1].0,
            );
        }
    }

    /// Prints the whole mode-search ladder, so two builds of the search can be
    /// compared against each other rather than each against its own baseline.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_ladder() {
        for (name, picture) in sweep_pictures() {
            let all = ladder(&picture, &KEY_FRAME_MODES);
            let points = all
                .iter()
                .map(|&(db, log_bytes)| format!("{:.0}@{db:.3}", 10f64.powf(log_bytes)))
                .collect::<Vec<_>>()
                .join(" ");
            println!("ladder {name}: {points}");
        }
    }

    /// Splitting a 32x32 block into four 16x16 ones, measured both ways over
    /// the sweep pictures and whatever clips `EC_AV1_CLIPS` names. This is what
    /// sets [`SPLIT_BLOCKS`]; the table it prints is in the lane report.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_split() {
        for (name, picture) in sweep_pictures() {
            let ladder = |split: bool| {
                let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
                    .iter()
                    .map(|&q| {
                        let encoded =
                            encode_key_frame_inner(&picture, q, 0.5, &KEY_FRAME_MODES, split)
                                .unwrap();
                        (
                            psnr(&encoded.reconstruction.y, picture.y.as_slice()),
                            (encoded.stream.len() as f64).log10(),
                        )
                    })
                    .collect();
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                points
            };
            let (whole, split) = (ladder(false), ladder(true));
            let blocks = encode_key_frame_inner(&picture, 90, 0.5, &KEY_FRAME_MODES, true)
                .unwrap()
                .modes
                .len();
            let quadrants = (picture.width / BLOCK) * (picture.height / BLOCK);
            println!(
                "split {name}: {:+.2}% rate, {blocks} blocks for {quadrants} quadrants at q90",
                bd_rate(&whole, &split) * 100.0
            );
        }
    }

    /// A striped picture, running one way or the other. The stripes are what
    /// separates a vertical predictor from a horizontal one: a mode search
    /// reading a transposed edge would pick the wrong one of the pair, which no
    /// symmetric picture would show.
    fn stripes(width: usize, height: usize, vertical: bool) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let along = if vertical { x } else { y };
                picture.y[y * width + x] = if (along / 4) % 2 == 0 { 40 } else { 210 };
            }
        }
        picture
    }

    /// Where two planes first disagree, and by how much: a mismatch reported as
    /// a position says which block and which sample of it went wrong, which a
    /// pair of thousand-sample arrays does not.
    fn first_difference(ours: &[u8], theirs: &[u8], width: usize) -> Option<String> {
        let i = ours.iter().zip(theirs).position(|(a, b)| a != b)?;
        let differ = ours.iter().zip(theirs).filter(|(a, b)| a != b).count();
        Some(format!(
            "{differ} samples differ, first at ({}, {}): ours {} theirs {}",
            i % width,
            i / width,
            ours[i],
            theirs[i]
        ))
    }

    /// Every mode the encoder offers has to predict what the decoder predicts,
    /// not just the ones a particular picture happens to choose: each is forced
    /// over a whole picture and the reconstruction gated against ffmpeg.
    #[test]
    fn every_mode_decodes_to_what_the_encoder_predicted() {
        if !have_ffmpeg() {
            eprintln!("SKIP every_mode_decodes_to_what_the_encoder_predicted: no ffmpeg");
            return;
        }
        // 128 wide is a whole number of superblocks and 160 is not: the last
        // superblock of a 160-wide row is half a one, whose blocks have no
        // above-right samples inside the frame at all.
        for (width, height) in [(128, 96), (160, 96)] {
            let picture = test_card(width, height);
            for mode in KEY_FRAME_MODES {
                let encoded = encode_key_frame_with_modes(&picture, 100, 0.5, &[mode]).unwrap();
                let decoded = ffmpeg_decode(&encoded.stream, width, height);
                for (plane, ours, theirs, stride) in [
                    ("luma", &encoded.reconstruction.y, &decoded.y, width),
                    ("U", &encoded.reconstruction.u, &decoded.u, width / 2),
                    ("V", &encoded.reconstruction.v, &decoded.v, width / 2),
                ] {
                    assert!(
                        first_difference(ours, theirs, stride).is_none(),
                        "{width}x{height} mode {mode}, {plane}: {}",
                        first_difference(ours, theirs, stride).unwrap()
                    );
                }
                assert!(
                    encoded.modes.iter().all(|&m| m == mode),
                    "mode {mode}: the encoder coded something else"
                );
            }
        }
    }

    /// The search has to follow the picture: vertical stripes are cheapest
    /// predicted from the row above, horizontal ones from the column to the
    /// left. Reading the edges the other way round would swap the two answers
    /// while leaving every fidelity gate intact.
    #[test]
    fn the_search_picks_the_direction_the_picture_runs() {
        for (vertical, want) in [(true, V_PRED), (false, H_PRED)] {
            let picture = stripes(128, 96, vertical);
            // Only the pair, because on stripes a third mode predicts exactly
            // what the right one of the pair does -- PAETH reads the corner and
            // the left column, which a striped picture makes equal -- and the
            // tie then goes to whichever is cheaper to name, which is not what
            // this gate is about.
            let encoded =
                encode_key_frame_with_modes(&picture, 100, 0.5, &[V_PRED, H_PRED]).unwrap();
            // The first block of the picture has neither neighbour, so it
            // cannot tell the modes apart; every other one can.
            let picked = encoded.modes[1..].iter().filter(|&&m| m == want).count();
            assert!(
                picked * 2 > encoded.modes.len() - 1,
                "vertical={vertical}: only {picked} of {} blocks picked mode {want}, modes {:?}",
                encoded.modes.len() - 1,
                encoded.modes
            );
        }
    }

    /// What the search is for: the same picture, at the same quantizer, coded
    /// smaller and more faithfully than DC alone can manage.
    /// Rate saved at matched fidelity, over a three-point ladder: the trapezoid
    /// between the two rate-distortion curves in log-rate against PSNR, as a
    /// fraction of the reference's rate. Negative means the second curve costs
    /// less for the same picture.
    fn bd_rate(reference: &[(f64, f64)], other: &[(f64, f64)]) -> f64 {
        let low = reference[0].0.max(other[0].0);
        let high = reference[reference.len() - 1]
            .0
            .min(other[other.len() - 1].0);
        assert!(high > low, "the two ladders have to overlap in PSNR");
        let log_rate_at = |curve: &[(f64, f64)], psnr: f64| {
            let i = curve
                .windows(2)
                .position(|w| psnr >= w[0].0 && psnr <= w[1].0)
                .unwrap_or(0);
            let (x0, y0) = curve[i];
            let (x1, y1) = curve[i + 1];
            y0 + (y1 - y0) * (psnr - x0) / (x1 - x0)
        };
        let steps = 64;
        let mut area = 0.0;
        for step in 0..steps {
            let psnr = low + (high - low) * (f64::from(step) + 0.5) / f64::from(steps);
            area += log_rate_at(other, psnr) - log_rate_at(reference, psnr);
        }
        10f64.powf(area / f64::from(steps)) - 1.0
    }

    /// A ladder of (luma PSNR, log10 bytes) for one mode set, ordered by
    /// fidelity.
    fn ladder(picture: &Picture, modes: &[u8]) -> Vec<(f64, f64)> {
        let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
            .iter()
            .map(|&q| {
                let encoded = encode_key_frame_with_modes(picture, q, 0.5, modes).unwrap();
                (
                    psnr(&encoded.reconstruction.y, picture.y.as_slice()),
                    (encoded.stream.len() as f64).log10(),
                )
            })
            .collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        points
    }

    /// What the search is for: the same pictures cost less to code at the same
    /// fidelity than DC prediction alone can manage. A picture that runs one
    /// way saves far more than a busy one, which is the shape a working
    /// directional pair has.
    #[test]
    fn the_search_beats_dc_alone() {
        for (name, picture, want) in [
            ("test card", test_card(160, 96), -0.05),
            ("stripes", stripes(160, 96, true), -0.40),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &[DC_PRED]),
                &ladder(&picture, &NON_DIRECTIONAL),
            );
            assert!(
                saved < want,
                "{name}: the search saved {:.1}% of the rate, wanted at least {:.1}%",
                saved * 100.0,
                -want * 100.0
            );
        }
    }

    /// The pictures a sweep is measured over: two synthetic ones, plus a frame
    /// from each clip named in `EC_AV1_CLIPS` as `path@skip`, colon separated.
    fn sweep_pictures() -> Vec<(String, Picture)> {
        let mut pictures = vec![
            ("test card".to_string(), test_card(160, 96)),
            ("stripes".to_string(), stripes(160, 96, true)),
            ("diagonal".to_string(), diagonal(160, 96, true)),
        ];
        if let Ok(clips) = std::env::var("EC_AV1_CLIPS") {
            for entry in clips.split(':').filter(|e| !e.is_empty()) {
                let (path, skip) = entry.split_once('@').unwrap_or((entry, "0"));
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                pictures.push((name, clip_frame(path, skip, 640, 352)));
            }
        }
        pictures
    }

    /// What the six directional modes are worth, per picture, at whatever
    /// `LAMBDA_SCALE` currently is: the rate they save over the seven that read
    /// no further, and how often the search picks one.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_directional() {
        for (name, picture) in sweep_pictures() {
            let dc = ladder(&picture, &[DC_PRED]);
            let flat = ladder(&picture, &NON_DIRECTIONAL);
            let all = ladder(&picture, &KEY_FRAME_MODES);
            let encoded = encode_key_frame(&picture, 90, 0.5).unwrap();
            let directional = encoded
                .modes
                .iter()
                .filter(|&&m| (3..=8).contains(&m))
                .count();
            // A picture whose ladders sit at different fidelities altogether
            // has no BD-rate to report -- see the flat-sample-window class.
            let overlap = |a: &[(f64, f64)], b: &[(f64, f64)]| {
                a[0].0.max(b[0].0) < a[a.len() - 1].0.min(b[b.len() - 1].0)
            };
            let against = |a: &[(f64, f64)], b: &[(f64, f64)]| {
                if overlap(a, b) {
                    format!("{:+.2}%", bd_rate(a, b) * 100.0)
                } else {
                    "no overlap".to_string()
                }
            };
            println!(
                "{name}: {} against the seven, {} against DC alone, {directional} of {} blocks directional",
                against(&flat, &all),
                against(&dc, &all),
                encoded.modes.len(),
            );
        }
    }

    /// A picture whose stripes run along a diagonal, one way or the other.
    /// This is to the directional modes what [`stripes`] is to the vertical and
    /// horizontal pair: a predictor that walked the edge in the wrong direction
    /// would answer the two pictures the same way round.
    fn diagonal(width: usize, height: usize, down_right: bool) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let along = if down_right { x + height - y } else { x + y };
                picture.y[y * width + x] = if (along / 6) % 2 == 0 { 40 } else { 210 };
            }
        }
        picture
    }

    /// The mode picked by the most blocks of a picture, ignoring the first
    /// block, which has no neighbours to tell the modes apart with.
    fn favourite_mode(picture: &Picture) -> (u8, usize, usize) {
        let encoded = encode_key_frame(picture, 100, 0.5).unwrap();
        let blocks = &encoded.modes[1..];
        let mut counts = [0usize; 13];
        for &mode in blocks {
            counts[usize::from(mode)] += 1;
        }
        let (mode, count) = counts
            .iter()
            .enumerate()
            .max_by_key(|&(_, count)| *count)
            .expect("thirteen modes");
        (mode as u8, *count, blocks.len())
    }

    /// The search has to follow a diagonal the way it runs: stripes down and to
    /// the right are cheapest predicted at 135 degrees, stripes down and to the
    /// left at 45. A walk that stepped the wrong way along the edge, or read
    /// the above row where it should read the left column, would swap these.
    #[test]
    fn the_search_picks_the_diagonal_the_picture_runs() {
        for (down_right, want) in [(true, D135_PRED), (false, D45_PRED)] {
            let picture = diagonal(160, 96, down_right);
            let (mode, count, blocks) = favourite_mode(&picture);
            assert_eq!(
                mode, want,
                "down_right={down_right}: {count} of {blocks} blocks picked mode {mode}"
            );
        }
    }

    /// What the directional modes are for: a picture that runs along a diagonal
    /// costs far less to code with them than the seven that read only their own
    /// edges can manage, and a picture that runs no particular way costs no
    /// more. The second half is the one that bites: a mode set the search
    /// cannot price is a mode set that loses rate on content that does not want
    /// it, which is what costing the mode symbol is for.
    #[test]
    fn the_diagonals_beat_the_modes_that_read_no_further() {
        for (name, picture, want) in [
            ("down-right", diagonal(160, 96, true), -0.20),
            ("down-left", diagonal(160, 96, false), -0.20),
            ("test card", test_card(160, 96), 0.01),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &NON_DIRECTIONAL),
                &ladder(&picture, &KEY_FRAME_MODES),
            );
            assert!(
                saved < want,
                "{name}: the directional modes saved {:.1}% of the rate, wanted better than {:.1}%",
                saved * 100.0,
                -want * 100.0
            );
        }
    }

    /// A mode the encoder cannot predict must be refused rather than coded as
    /// something else, and a search with nothing to choose from likewise.
    #[test]
    fn a_mode_the_encoder_cannot_predict_is_refused() {
        let picture = test_card(64, 64);
        let message = encode_key_frame_with_modes(&picture, 100, 0.5, &[13])
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 13"),
            "the refusal must name the mode, got {message}"
        );
        assert!(encode_key_frame_with_modes(&picture, 100, 0.5, &[]).is_err());
    }

    /// The picture that comes back is the picture that went in, to within the
    /// quantizer. A prediction that read the wrong neighbours, or a block
    /// written into the wrong place, would still decode to what the encoder
    /// reconstructed — it is this gate that says the reconstruction is of the
    /// right picture.
    #[test]
    fn the_encoded_picture_is_the_one_that_went_in() {
        let picture = test_card(160, 96);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        let luma = psnr(&encoded.reconstruction.y, &picture.y);
        assert!(luma > 36.0, "luma PSNR {luma} at q 100");
        for (plane, (got, want)) in [
            (encoded.reconstruction.u.as_slice(), picture.u.as_slice()),
            (encoded.reconstruction.v.as_slice(), picture.v.as_slice()),
        ]
        .iter()
        .enumerate()
        {
            let chroma = psnr(got, want);
            assert!(chroma > 40.0, "chroma plane {plane} PSNR {chroma} at q 100");
        }
    }

    /// A finer quantizer costs bits and buys fidelity, and a wider deadzone
    /// does the opposite. Both are monotone, which is what a rate-distortion
    /// loop above this will assume.
    #[test]
    fn fidelity_and_rate_move_with_the_quantizer() {
        let picture = test_card(128, 128);
        let mut previous: Option<(usize, f64)> = None;
        for &q in &[70u8, 90, 110] {
            let encoded = encode_key_frame(&picture, q, 0.5).unwrap();
            let quality = psnr(&encoded.reconstruction.y, &picture.y);
            if let Some((bytes, better)) = previous {
                assert!(
                    encoded.stream.len() < bytes,
                    "q {q}: {} bytes",
                    encoded.stream.len()
                );
                assert!(quality < better, "q {q}: PSNR {quality}");
            }
            previous = Some((encoded.stream.len(), quality));
        }

        let mut previous = None;
        for &deadzone in &[0.5f64, 0.3, 0.15] {
            let encoded = encode_key_frame(&picture, 100, deadzone).unwrap();
            let quality = psnr(&encoded.reconstruction.y, &picture.y);
            if let Some((bytes, better)) = previous {
                assert!(
                    encoded.stream.len() < bytes,
                    "deadzone {deadzone}: {} bytes",
                    encoded.stream.len()
                );
                assert!(quality < better, "deadzone {deadzone}: PSNR {quality}");
            }
            previous = Some((encoded.stream.len(), quality));
        }
    }

    /// A flat picture is a flat stream: every block predicts its neighbours'
    /// average, which is the picture's own value, and codes nothing.
    #[test]
    fn a_flat_picture_costs_almost_nothing() {
        let mut picture = Picture::grey(128, 128);
        picture.y.fill(97);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        // The first block has no neighbour and predicts 128, so it carries a
        // DC; every block after it predicts 97 and carries nothing.
        assert!(
            encoded.stream.len() < 100,
            "{} bytes for a flat picture",
            encoded.stream.len()
        );
        for (i, &s) in encoded.reconstruction.y.iter().enumerate().skip(32 * 128) {
            assert_eq!(s, 97, "sample {i} of a flat picture");
        }
    }

    /// The sizes the encoder refuses, refused for a reason rather than by
    /// panicking somewhere below.
    #[test]
    fn a_picture_off_the_block_grid_is_refused() {
        assert!(encode_key_frame(&Picture::grey(40, 32), 100, 0.5).is_err());
        assert!(encode_key_frame(&Picture::grey(32, 48), 100, 0.5).is_err());
        let mut short = Picture::grey(64, 64);
        short.u.truncate(10);
        assert!(encode_key_frame(&short, 100, 0.5).is_err());
        // The tile writer carries the CDFs of one q context only.
        assert!(encode_key_frame(&Picture::grey(64, 64), 40, 0.5).is_err());
    }

    /// A frame of real video, rather than a picture built to be easy.
    ///
    /// `EC_AV1_CLIP` names the clip and `EC_AV1_CLIP_SKIP` how far into it to
    /// seek; ffmpeg decodes one frame, crops it to the block grid, and the
    /// encoder's reconstruction has to survive the same equality gate as the
    /// synthetic pictures — real video reaches contexts a test card does not.
    #[test]
    fn a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed() {
        let Ok(clip) = std::env::var("EC_AV1_CLIP") else {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: \
                 set EC_AV1_CLIP to a clip"
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        let skip = std::env::var("EC_AV1_CLIP_SKIP").unwrap_or_else(|_| "0".into());
        // A 4:2:0 frame cropped to whole 32x32 blocks, in a size that keeps the
        // test quick while still spanning many superblocks.
        let (width, height) = (640usize, 352usize);
        let picture = clip_frame(&clip, &skip, width, height);

        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "V");
        eprintln!(
            "{} bytes, luma PSNR {:.2} dB",
            encoded.stream.len(),
            psnr(&decoded.y, &picture.y)
        );
    }
}
