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
use crate::mc;
use crate::motion;
use crate::mvstack::{MiGrid, MiInfo, MvStack, find_mv_stack};
use crate::obu::temporal_delimiter;
use crate::quant::ac_q;
use crate::sequence::sequence_header_obu;
use crate::tile::{
    BlockCoeffs, Coeff, INTRA_MODE_CTX, InterInfo, InterMode, Quadrant, Superblock, partition_bits,
    sb_coeff_inter_frame_tile, sb_coeff_key_frame_tile,
};
use crate::transform::{dequant_and_inverse, forward_and_quantize};

/// The side of the larger of the two luma blocks this encoder codes, in
/// samples.
const BLOCK: usize = 32;

/// The side of the smaller one, which a 32x32 block may be split into four of.
const SUB: usize = 16;

/// The side of a superblock, which is what the partition tree starts from.
pub(crate) const SUPERBLOCK: usize = 64;

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
pub(crate) fn split_blocks() -> bool {
    SPLIT_BLOCKS
}

/// One planar 4:2:0 picture. `y`/`u`/`v` hold one sample per position as
/// `u16` regardless of bit depth (`crate::decode`'s own `PlaneBuf` has used
/// `u16` throughout since the round-hbd-r1/r2 widen; this is the last
/// narrowing boundary that stayed `u8` -- widened lane-hbd r3 so a 10-bit
/// stream's decode output actually carries 10-bit precision instead of
/// silently losing its low 2 bits). The encoder only ever produces values in
/// `0..=255` (it is still 8-bit-only by design, see `encode.rs`'s r2
/// decision); `Plane::source` narrows right back to `u8` at its two
/// construction sites, a lossless round trip for that range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The picture's width in luma samples.
    pub width: usize,
    /// Its height in luma samples.
    pub height: usize,
    /// The luma plane, `width * height` samples in raster order.
    pub y: Vec<u16>,
    /// The U plane, at half the width and half the height.
    pub u: Vec<u16>,
    /// The V plane, the same shape as U.
    pub v: Vec<u16>,
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

    pub(crate) fn check(&self) -> Result<()> {
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

    /// What the public entry points require of the picture they are handed,
    /// before it is padded to the block grid: even, nonzero dimensions (4:2:0
    /// needs a whole chroma sample per two luma ones) and one sample per
    /// position on each plane.
    pub(crate) fn check_even(&self) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err(Error::unsupported(
                "AV1 encode",
                "the picture's width and height must each be even and nonzero",
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

    /// This picture, padded by edge replication to the next whole number of
    /// `align`-sample blocks in each direction — [`BLOCK`] for a lone key
    /// frame (the block coder's own requirement), [`SUPERBLOCK`] for a
    /// sequence (what the inter tile writer's partition needs on top of
    /// that). Identity (a plain clone, not a copy through the replication
    /// loop) when the picture is already that size — the multiple-of-32 (or
    /// -64) fast path is unchanged.
    pub(crate) fn padded_to(&self, align: usize) -> Picture {
        let padded_width = self.width.next_multiple_of(align);
        let padded_height = self.height.next_multiple_of(align);
        if padded_width == self.width && padded_height == self.height {
            return self.clone();
        }
        Picture {
            width: padded_width,
            height: padded_height,
            y: pad_plane(
                &self.y,
                self.width,
                self.height,
                padded_width,
                padded_height,
            ),
            u: pad_plane(
                &self.u,
                self.width / 2,
                self.height / 2,
                padded_width / 2,
                padded_height / 2,
            ),
            v: pad_plane(
                &self.v,
                self.width / 2,
                self.height / 2,
                padded_width / 2,
                padded_height / 2,
            ),
        }
    }
}

/// lane-hbd r2: the encoder's own reconstruction/source buffers stay `u8`
/// (8-bit only, unchanged this round -- widening the encoder is out of this
/// round's scope, see `lanes/hbd-r2.report.md`); this is the local box that
/// lets it keep calling [`crate::intra::predict`] now that shape is `u16`
/// in and out. A block-sized round-trip through `u16` is exact for 8-bit
/// content (every value fits both ways), so this changes no encoder output.
#[allow(clippy::too_many_arguments)]
fn intra_predict_u8(
    mode: u8,
    angle_delta: i32,
    above: Option<&[u8]>,
    left: Option<&[u8]>,
    corner: Option<u8>,
    bw: usize,
    bh: usize,
    enable_edge_filter: bool,
    smooth_neighbor: bool,
    dst: &mut [u8],
) {
    let above16: Option<Vec<u16>> = above.map(|s| s.iter().map(|&v| u16::from(v)).collect());
    let left16: Option<Vec<u16>> = left.map(|s| s.iter().map(|&v| u16::from(v)).collect());
    let mut dst16 = vec![0u16; dst.len()];
    crate::intra::predict(
        mode,
        angle_delta,
        above16.as_deref(),
        left16.as_deref(),
        corner.map(u16::from),
        bw,
        bh,
        enable_edge_filter,
        smooth_neighbor,
        &mut dst16,
    );
    for (d, &s) in dst.iter_mut().zip(dst16.iter()) {
        *d = s as u8;
    }
}

/// Pads one plane to `(padded_width, padded_height)` by repeating its last
/// row and column, so the block coder always sees a whole number of blocks.
/// [`crop_plane`] undoes this on the way back out.
fn pad_plane<T: Copy + Default>(
    source: &[T],
    width: usize,
    height: usize,
    padded_width: usize,
    padded_height: usize,
) -> Vec<T> {
    let mut out = vec![T::default(); padded_width * padded_height];
    for row in 0..padded_height {
        let src = source[row.min(height - 1) * width..][..width].as_ref();
        let dst = &mut out[row * padded_width..][..padded_width];
        dst[..width].copy_from_slice(src);
        dst[width..].fill(src[width - 1]);
    }
    out
}

/// The top-left `width` x `height` region of a plane that is `padded_width`
/// wide, which is the render-size crop [`Picture::padded`]'s replication
/// exists to let a decoder undo.
fn crop_plane<T: Copy>(source: &[T], padded_width: usize, width: usize, height: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        out.extend_from_slice(&source[row * padded_width..][..width]);
    }
    out
}

/// [`Encoded::reconstruction`] cropped to `(width, height)`: the render-size
/// region a decoder produces, which is the public contract for what a caller
/// sees back — the padded planes the encoder coded against never leave this
/// module. Identity when the reconstruction is already that size.
pub(crate) fn crop_encoded(encoded: &Encoded, width: usize, height: usize) -> Encoded {
    let reconstruction = &encoded.reconstruction;
    let cropped = if reconstruction.width == width && reconstruction.height == height {
        reconstruction.clone()
    } else {
        Picture {
            width,
            height,
            y: crop_plane(&reconstruction.y, reconstruction.width, width, height),
            u: crop_plane(
                &reconstruction.u,
                reconstruction.width / 2,
                width / 2,
                height / 2,
            ),
            v: crop_plane(
                &reconstruction.v,
                reconstruction.width / 2,
                width / 2,
                height / 2,
            ),
        }
    };
    Encoded {
        stream: encoded.stream.clone(),
        modes: encoded.modes.clone(),
        inter_block_share: encoded.inter_block_share,
        reconstruction: cropped,
        tile: encoded.tile.clone(),
        mi_cols: encoded.mi_cols,
        mi_rows: encoded.mi_rows,
        base_q_idx: encoded.base_q_idx,
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
    /// The fraction of this frame's 32x32 blocks that were coded inter (any
    /// of `NEARESTMV` skipped, `NEARESTMV` coded, or `NEWMV`). Always `0.0`
    /// for a key frame, which has no such choice — so a caller can tell a
    /// frame with no motion worth coding from one that never got the chance.
    pub inter_block_share: f64,
    /// The tile payload alone, undecorated by any OBU framing, and the frame
    /// header fields a decoder of it needs: `mi_cols`/`mi_rows`/`base_q_idx`.
    /// `crate::decode`'s tests are the only reader; a real decoder gets these
    /// off the wire by parsing `stream`'s OBUs instead.
    pub(crate) tile: Vec<u8>,
    pub(crate) mi_cols: u32,
    pub(crate) mi_rows: u32,
    pub(crate) base_q_idx: u8,
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
    key_frame_headers_colour(width, height, base_q_idx, unspecified_color_config())
}

/// [`ColorConfig`] with every CICP field "unspecified" (H.273 value 2) and
/// studio (limited) range: what this crate's sequence header carried before
/// [`crate::encoder`]'s facade could configure it, and what
/// [`key_frame_headers`] still asks for.
fn unspecified_color_config() -> ColorConfig {
    ColorConfig {
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
    }
}

/// [`key_frame_headers`] with the sequence header's colour config named
/// instead of hardcoded to "unspecified" -- spec 5.5.2's `color_primaries`,
/// `transfer_characteristics`, `matrix_coefficients` and `color_range`,
/// which is all a player picks a colour transform from and which
/// [`crate::encoder::Colour`] is the facade's own name for.
///
/// # Errors
/// The same as [`key_frame_headers`].
pub(crate) fn key_frame_headers_colour(
    width: usize,
    height: usize,
    base_q_idx: u8,
    color_config: ColorConfig,
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
        color_config,
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
    // spec `compute_image_size` (5.9.15): MiCols/MiRows come from the frame's
    // own (true, unpadded) width and height, not from any block-grid
    // alignment — a decoder derives these straight from `frame_width`/
    // `frame_height` below, so this must match it exactly.
    let (mi_cols, mi_rows) = (2 * ((w + 7) >> 3), 2 * ((h + 7) >> 3));
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
        // Forces `get_tx_set` (spec 5.11.48) to `TX_SET_INTRA_2` for every
        // intra transform below 32x32, not only 16x16 -- the set this crate
        // carries a table for at 8x8/16x16 (`INTRA_TX_TYPE_SET2_8`/`_16`).
        // This bit is part of the bitstream (spec 5.9.2's `reduced_tx_set`),
        // so a decoder honouring the header picks the same CDF the writer
        // used only if this is set here too, not only on the inter variant
        // below -- an r14 lane-av1-rect fix: a key frame's own header used to
        // default this to `false`, so `av1_get_ext_tx_set_type` picked the
        // 7-symbol `TX_SET_INTRA_1` table on the decoder side while the
        // writer coded against the 5-symbol `TX_SET_INTRA_2` one, diverging
        // the arithmetic coder's `rng` register from the very first 8x8-leaf
        // luma transform onward with no symbol value ever differing.
        reduced_tx_set: true,
        ..FrameHeader::default()
    };
    Ok((seq, header))
}

/// The frame header for a shown inter frame that predicts from the single
/// slot `last_slot` (every `ref_frame_idx` entry names it) and refreshes that
/// same slot with itself once coded. The sequence header is [`key_frame_headers`]'s,
/// reused verbatim: an inter frame never changes the sequence.
///
/// # Errors
/// Returns an error under the same conditions as [`key_frame_headers`].
pub fn inter_frame_headers(
    width: usize,
    height: usize,
    base_q_idx: u8,
    order_hint: u32,
    last_slot: u8,
) -> Result<(SequenceHeader, FrameHeader)> {
    let (seq, key) = key_frame_headers(width, height, base_q_idx)?;
    let header = FrameHeader {
        frame_type: FrameType::Inter,
        frame_is_intra: false,
        show_frame: true,
        error_resilient_mode: false,
        disable_cdf_update: false,
        disable_frame_end_update_cdf: true,
        force_integer_mv: false,
        order_hint,
        primary_ref_frame: 0,
        refresh_frame_flags: 1 << last_slot,
        ref_frame_idx: [last_slot; ec_av1_syntax::REFS_PER_FRAME],
        allow_high_precision_mv: false,
        interpolation_filter: ec_av1_syntax::InterpolationFilter::Eighttap,
        is_motion_mode_switchable: false,
        use_ref_frame_mvs: false,
        // Forces `get_tx_set` (spec 5.11.48) to the two-symbol
        // `TX_SET_INTER_3` for every inter transform below 32x32 -- the only
        // set this crate carries a table for at 16x16 (see
        // `crate::cdf::INTER_TX_TYPE_SET3_16`'s doc comment). A 32x32 inter
        // transform is unaffected (its `txSzSqrUp == TX_32X32` branch already
        // reads `TX_SET_INTER_3` regardless of this flag). An intra transform
        // IS affected below 16x16 too, not only at 16x16 (r11 lane-av1-rect:
        // the earlier claim here was wrong and desynced the 8x8-leaf straddle
        // path) -- with this flag set, `get_tx_set`'s intra branch returns
        // `TX_SET_INTRA_2` at every size up to 16x16, so `TX_8X8` reads
        // [`crate::cdf::INTRA_TX_TYPE_SET2_8`], not `SET1_8`.
        reduced_tx_set: true,
        ..key
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
    /// The frame's true, decodable extent in this plane's own units --
    /// `mi_cols`/`mi_rows` (spec `compute_image_size`) converted to samples,
    /// not `width`/`height` above, which is the padded coding-surface size.
    /// A decoder's `BlockDecoded` bookkeeping (spec 7.4) and its `mb_to_
    /// right_edge`/`mb_to_bottom_edge` clamps (libaom's
    /// `av1/common/reconintra.c` `build_intra_predictors`) never see past
    /// this bound, even though the padded reconstruction buffer holds real
    /// (if never-decoded-by-a-real-decoder) samples out there -- reading
    /// those instead of stopping here is what desyncs prediction at the true
    /// edge.
    true_width: usize,
    true_height: usize,
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
        // Even a block's *own* edge is clamped to the true frame bound, not
        // just a `reach` extension: libaom's `av1_predict_intra_block`
        // (`av1/common/reconintra.c`) derives `xr`/`yd` -- the distance from
        // this block's own right/bottom to the frame edge -- from
        // `mb_to_right_edge`/`mb_to_bottom_edge`, which are built from
        // `mi_cols`/`mi_rows` (the true, decodable extent), never from any
        // padded coding surface. `n_top_px`/`n_left_px` (how many *real*
        // samples the row/column above/left holds before the decoder starts
        // repeating the last one, same clamp as spec 7.11.2.2's `aboveLimit`/
        // `leftLimit`) is `min(side, xr + side)` even for a block whose own
        // extent straddles that edge legally (`has_half`) -- the transform
        // still covers the whole block, but prediction does not get to read
        // real reconstructed pixels past the true edge for its own row/column,
        // only for the padded tail it invents.
        let own_across = x + side.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + side.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width);
        let own_down = y + side.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + side.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height);
        // `across`/`down` can still fall at or below `x`/`y` for a sub-block
        // whose own origin already sits past the true edge -- only reachable
        // through a partition trial priced for comparison on a split this
        // frame's straddling quadrant will go on to refuse (see
        // `a_picture_off_the_block_grid_is_refused`), never through a block
        // this encoder actually commits. Such a block has no true-edge
        // samples above or left of it at all, same as `y == 0`/`x == 0`.
        let above = (y > 0 && across > x)
            .then(|| self.reconstruction[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > 0 && down > y).then(|| {
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
        intra_predict_u8(
            mode,
            0,
            above.as_deref(),
            left.as_deref(),
            corner,
            side,
            side,
            false,
            false,
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
        #[cfg(test)]
        let t = std::time::Instant::now();
        let levels = forward_and_quantize(&residual, side, 8, i32::from(base_q_idx), deadzone);
        let coded = dequant_and_inverse(&levels, side, 8, i32::from(base_q_idx));
        #[cfg(test)]
        stage_add(2, t.elapsed());

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
        #[cfg(test)]
        let t = std::time::Instant::now();
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
        #[cfg(test)]
        stage_add(3, t.elapsed());
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// [`Self::trial`] with the prediction already built, so that an inter
    /// block's motion-compensated prediction can go through the same
    /// residual/quantize/reconstruct path an intra block's does. `skip`
    /// takes the prediction as the reconstruction outright, coding no
    /// residual at all — what a `NEARESTMV` block that names no coefficients
    /// codes.
    #[allow(clippy::too_many_arguments)]
    fn code_from_prediction(
        &self,
        x: usize,
        y: usize,
        side: usize,
        prediction: &[u8],
        skip: bool,
        base_q_idx: u8,
        deadzone: f64,
        set: TxbSet,
    ) -> Trial {
        if skip {
            let mut sse = 0.0;
            for row in 0..side {
                for col in 0..side {
                    let error = f64::from(
                        i32::from(self.source[(y + row) * self.width + x + col])
                            - i32::from(prediction[row * side + col]),
                    );
                    sse += error * error;
                }
            }
            return Trial {
                levels: vec![0i32; side * side],
                reconstruction: prediction.to_vec(),
                sse,
                bits: 0.0,
            };
        }
        let mut residual = vec![0i32; side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        #[cfg(test)]
        let t = std::time::Instant::now();
        let levels = forward_and_quantize(&residual, side, 8, i32::from(base_q_idx), deadzone);
        let coded = dequant_and_inverse(&levels, side, 8, i32::from(base_q_idx));
        #[cfg(test)]
        stage_add(2, t.elapsed());
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
        #[cfg(test)]
        let t = std::time::Instant::now();
        let bits = if cfg!(test) && std::env::var_os("EC_AV1_ESTIMATE").is_some() {
            levels
                .iter()
                .filter(|&&level| level != 0)
                .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
                .sum()
        } else {
            crate::tile::coeff_bits(&levels, set)
        };
        #[cfg(test)]
        stage_add(3, t.elapsed());
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

    /// The source samples of one square, contiguous — what a motion search
    /// compares its candidates' predictions against ([`motion::search`]'s
    /// `source`), since [`Self::source`] itself is the whole plane, strided
    /// by [`Self::width`].
    fn source_block(&self, x: usize, y: usize, side: usize) -> Vec<u8> {
        (y..y + side)
            .flat_map(|row| self.source[row * self.width + x..][..side].to_vec())
            .collect()
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

    /// The SAD-plus-mode-cost score [`Self::search_block`] ranks candidates
    /// by, exposed so [`search_inter_block`]'s intra-candidate loop can
    /// prescreen by the identical rule rather than a second one that could
    /// drift from it.
    fn intra_scores(
        &self,
        at: At,
        modes: &[u8],
        mode_bits: &[f64; 13],
        lambda: f64,
    ) -> Vec<(f64, u8)> {
        let At {
            x, y, side, reach, ..
        } = at;
        let (above, left, corner) = self.edges(x, y, side, reach);
        modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                );
                let sad: f64 = (0..side * side)
                    .map(|i| {
                        let (row, col) = (i / side, i % side);
                        f64::from(
                            (i32::from(self.source[(y + row) * self.width + x + col])
                                - i32::from(prediction[i]))
                            .abs(),
                        )
                    })
                    .sum();
                (sad + lambda * mode_bits[usize::from(mode)], mode)
            })
            .collect()
    }

    /// Codes one block under every mode the search offers (or, when
    /// [`Search::top_k`] prunes it, the cheapest `top_k` of them by a SAD
    /// pre-pass plus DC) and commits the one whose squared error and
    /// estimated rate come out cheapest.
    ///
    /// The prediction is built once per mode regardless -- the full RD trial
    /// needs it too -- so ranking modes by SAD on that same prediction before
    /// running the expensive part (forward transform, quantize, reconstruct,
    /// `coeff_bits`) on only the survivors costs nothing extra to predict,
    /// only to sum.
    fn search_block(
        &mut self,
        at: At,
        search: &Search,
        mode_bits: &[f64; 13],
    ) -> (Vec<Coeff>, u8, f64) {
        let At {
            x, y, side, reach, ..
        } = at;
        let (above, left, corner) = self.edges(x, y, side, reach);
        let mut scored: Vec<(f64, u8, Vec<u8>)> = search
            .modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                );
                let sad: f64 = (0..side * side)
                    .map(|i| {
                        let (row, col) = (i / side, i % side);
                        f64::from(
                            (i32::from(self.source[(y + row) * self.width + x + col])
                                - i32::from(prediction[i]))
                            .abs(),
                        )
                    })
                    .sum();
                (
                    sad + search.lambda * mode_bits[usize::from(mode)],
                    mode,
                    prediction,
                )
            })
            .collect();
        if let Some(k) = search.top_k {
            if k < scored.len() {
                let dc_pos = scored.iter().position(|&(_, mode, _)| mode == DC_PRED);
                let dc_entry = dc_pos.map(|i| scored.remove(i));
                scored.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("scores are finite"));
                scored.truncate(if dc_entry.is_some() {
                    k.saturating_sub(1)
                } else {
                    k
                });
                if let Some(dc_entry) = dc_entry {
                    scored.push(dc_entry);
                }
            }
        }
        let mut best: Option<(f64, u8, Trial)> = None;
        for (_, mode, prediction) in scored {
            let trial = self.code_from_prediction(
                x,
                y,
                side,
                &prediction,
                false,
                search.base_q_idx,
                search.deadzone,
                at.set,
            );
            let cost = trial.sse + search.lambda * (trial.bits + mode_bits[usize::from(mode)]);
            if best
                .as_ref()
                .is_none_or(|(best_cost, _, _)| cost < *best_cost)
            {
                best = Some((cost, mode, trial));
            }
        }
        let (cost, mode, trial) = best.expect("the search offers at least one mode");
        self.commit(x, y, side, &trial);
        (coeffs(&trial.levels, side), mode, cost)
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
pub(crate) struct Reach {
    pub(crate) above_right: bool,
    pub(crate) below_left: bool,
}

/// `has_tr_16x16` / `has_tr_32x32` (`recon_intra.rs` in rav1e, `has_tr_*` in
/// libaom): for a block that is neither in the superblock's top row nor its
/// rightmost column, whether the block above and to its right is coded before
/// it. Indexed by the block's position in the superblock, in blocks of its own
/// size, as `row * (128 / side) + col` (`Reach::table_stride`) -- libaom's
/// 128, not this crate's 64 superblock -- bit by bit from the low end.
const HAS_TOP_RIGHT: [&[u8]; 3] = [
    &[255, 85, 119, 85, 127, 85, 119, 85],
    &[95, 87],
    // has_tr_8x8 (libaom av1/common/reconintra.c) -- table_stride(8) == 16
    // (128 / 8), so this row is 16 wide by 16 rows, 32 bytes.
    &[
        255, 255, 85, 85, 119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85, 255, 127, 85, 85,
        119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85,
    ],
];

/// `has_bl_16x16` / `has_bl_32x32`: the same, for the block below and to the
/// left.
const HAS_BOTTOM_LEFT: [&[u8]; 3] = [
    &[84, 16, 84, 0, 84, 16, 84, 0],
    &[4, 4],
    // has_bl_8x8 (libaom av1/common/reconintra.c)
    &[
        84, 85, 16, 17, 84, 85, 0, 1, 84, 85, 16, 17, 84, 85, 0, 0, 84, 85, 16, 17, 84, 85, 0, 1,
        84, 85, 16, 17, 84, 85, 0, 0,
    ],
];

/// `has_tr_32x16`/`has_tr_16x32` (libaom `reconintra.c`) -- lane-intradisp
/// r1: the two rect strips PARTITION_HORZ/VERT ever produce at the 32x32
/// level, transcribed verbatim (not derived): `[0]` is 32-wide, `[1]` is
/// 16-wide.
const HAS_TOP_RIGHT_RECT: [&[u8]; 2] = [&[15, 5, 7, 5], &[255, 119, 127, 119]];
/// `has_bl_32x16`/`has_bl_16x32`, same source, same order.
const HAS_BOTTOM_LEFT_RECT: [&[u8]; 2] = [&[78, 14, 78, 14], &[16, 0, 16, 0]];

impl Reach {
    /// What a block of `side` samples at `(x, y)` may read past its own edges,
    /// in a frame of `width` by `height`.
    pub(crate) fn of(side: usize, x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            above_right: y > 0 && x + side < width && Self::top_right(side, x, y),
            below_left: x > 0 && y + side < height && Self::bottom_left(side, x, y),
        }
    }

    /// [`Self::of`] for a true `bw`x`bh` rectangular block (lane-intradisp
    /// r1) -- `libaom`'s `has_top_right`/`has_bottom_left` index by the
    /// block's own width for the row stride and by its own height for the
    /// row/column position (`reconintra.c`'s `bw_in_mi_log2`/`bh_in_mi_log2`),
    /// unlike the square [`Self::of`] which uses one `side` for everything.
    pub(crate) fn of_rect(
        bw: usize,
        bh: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Self {
        Self {
            above_right: y > 0 && x + bw < width && Self::top_right_rect(bw, bh, x, y),
            below_left: x > 0 && y + bh < height && Self::bottom_left_rect(bw, bh, x, y),
        }
    }

    fn position_rect(bw: usize, bh: usize, x: usize, y: usize) -> (usize, usize, usize, usize) {
        (
            (y % SUPERBLOCK) / bh,
            (x % SUPERBLOCK) / bw,
            SUPERBLOCK / bh,
            SUPERBLOCK / bw,
        )
    }

    fn top_right_rect(bw: usize, bh: usize, x: usize, y: usize) -> bool {
        let (row, col, _per_row, per_col) = Self::position_rect(bw, bh, x, y);
        if row == 0 {
            return true;
        }
        if col + 1 == per_col {
            return false;
        }
        let table = HAS_TOP_RIGHT_RECT[usize::from(bw == 16)];
        Self::bit(table, (row * Self::table_stride(bw) + col) % (table.len() * 8))
    }

    fn bottom_left_rect(bw: usize, bh: usize, x: usize, y: usize) -> bool {
        let (row, col, per_row, _per_col) = Self::position_rect(bw, bh, x, y);
        if col == 0 {
            return row * bh + bh < SUPERBLOCK;
        }
        if row + 1 == per_row {
            return false;
        }
        let table = HAS_BOTTOM_LEFT_RECT[usize::from(bw == 16)];
        Self::bit(table, (row * Self::table_stride(bw) + col) % (table.len() * 8))
    }

    /// Neither, which is all a mode that reads no further than its own edges
    /// needs.
    pub(crate) fn none() -> Self {
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
        // corner-cut: `HAS_TOP_RIGHT`/`HAS_BOTTOM_LEFT` only carry dedicated
        // rows for side 8/16/32/64 (libaom's own `has_tr_8x8`/`has_tr_16x16`
        // families); a `TxMode::Select` TX4 transform unit reaches here with
        // side 4, whose real table (`has_tr_4x4`, 128 bytes) was never
        // transcribed. Clamping into the 16/32 table's own bit range keeps
        // this in-bounds and correct at every position that table already
        // covers periodically -- wrong only at the small remainder past its
        // length, a reach over-/under-estimate (extra or missing above-right
        // reference pixels), never a stream desync (Reach never gates a
        // symbol read). Ceiling: transcribe `has_tr_4x4`/`has_bl_4x4`
        // verbatim and give TX4 its own `HAS_TOP_RIGHT`/`HAS_BOTTOM_LEFT` row.
        let table = HAS_TOP_RIGHT[Self::table(side)];
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
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
        // corner-cut: see `top_right`'s note -- same clamp, same ceiling.
        let table = HAS_BOTTOM_LEFT[Self::table(side)];
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
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

    /// The row stride `HAS_TOP_RIGHT`/`HAS_BOTTOM_LEFT` index by: libaom pins
    /// these tables to its compile-time maximum superblock (128, spec
    /// `MAX_MIB_SIZE_LOG2` = 5 in 4-pixel units), not to whichever superblock
    /// size an encode actually uses, so a block's row in the bit index steps
    /// by 128 / `side` even though this crate only ever codes a 64
    /// superblock -- using `per_side` (`SUPERBLOCK` / `side`, 64-relative)
    /// here instead indexes the wrong bit for every block size but the one
    /// where 64 and 128 give the same stride.
    fn table_stride(side: usize) -> usize {
        128 / side
    }

    /// Which of the three block sizes the encoder codes a table row is for.
    fn table(side: usize) -> usize {
        if side == BLOCK {
            1
        } else if side == 8 {
            2
        } else {
            0
        }
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
    } else if side == 8 {
        // An 8x8 leaf under a straddling 16x16 (lane-av1-rect). r5 correction:
        // `TxbSet::Chroma4`'s doc now has the checked spec fact -- chroma is
        // `is_chroma_reference` at every BLOCK_8X8, so it is coded per leaf at
        // 4x4, not once at the parent's 8x8. No caller passes `side == 8` yet
        // (this branch is still unreached), so leaving `Chroma8` here would
        // silently wire the wrong table the moment one does; `Chroma4` is
        // what a real caller needs.
        (TxbSet::Luma8, TxbSet::Chroma4)
    } else {
        (TxbSet::Luma16, TxbSet::Chroma8)
    };
    let (luma_coeffs, mode, mut cost) = luma.search_block(
        At {
            x,
            y,
            side,
            reach: Reach::of(side, x, y, luma.true_width, luma.true_height),
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

/// Codes one leaf of a straddling quadrant as whichever costs least of intra
/// ([`code_square`], unchanged) or `NEARESTMV` against `reference` at
/// `stack.nearest_mv` -- the same NEARESTMV candidate [`search_inter_block`]
/// prices for a whole 32x32 block, just at this leaf's own `side` (16, for a
/// 16x16 leaf under a straddling 32x32 quadrant, or 8, for an 8x8 leaf under
/// a straddling 16x16 -- lane-av1inter8) and through [`TxbSet::Luma16Inter`]
/// or [`TxbSet::Luma8Inter`]. `NEWMV` (a motion search of its own) is not
/// attempted here: these leaves only ever fill the sliver of real content a
/// true edge lands in the middle of a larger block, not a leaf worth a full
/// search, so `NEARESTMV`-or-intra is this function's deliberate first cut
/// (see `write_inter_frame_leaf`'s doc comment).
#[allow(clippy::too_many_arguments)]
fn code_square_inter(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
) -> BlockCoeffs {
    let (intra_block, intra_cost) = code_square(luma, chroma, (x, y), side, search, mode_bits);
    let luma_set = if side == 8 {
        TxbSet::Luma8Inter
    } else {
        TxbSet::Luma16Inter
    };
    let chroma_set = if side == 8 {
        TxbSet::Chroma4
    } else {
        TxbSet::Chroma8
    };

    let skip_bits = |skip: bool| symbol_bits(&cdf::SKIP[0], usize::from(skip));
    let intra_inter_bits = |inter: bool| symbol_bits(&cdf::INTRA_INTER[0], usize::from(inter));
    let single_ref_bits = symbol_bits(&cdf::SINGLE_REF[0][0], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][2], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][3], 0);
    let mode_bits_inter = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1) // not NEWMV
        + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1) // not zero
        + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0); // NEARESTMV

    let mv = stack.nearest_mv;
    let ref_luma = (
        &reference.y,
        reference.width,
        luma.true_width,
        luma.true_height,
    );
    let ref_u = (
        &reference.u,
        reference.width / 2,
        chroma[0].true_width,
        chroma[0].true_height,
    );
    let ref_v = (
        &reference.v,
        reference.width / 2,
        chroma[1].true_width,
        chroma[1].true_height,
    );
    let luma_trial = mc_trial(
        luma,
        x,
        y,
        side,
        mv,
        true,
        ref_luma.0,
        ref_luma.1,
        ref_luma.2,
        ref_luma.3,
        false,
        search.base_q_idx,
        search.deadzone,
        luma_set,
    );
    let u = mc_trial(
        &chroma[0],
        x / 2,
        y / 2,
        side / 2,
        mv,
        false,
        ref_u.0,
        ref_u.1,
        ref_u.2,
        ref_u.3,
        false,
        search.base_q_idx,
        search.deadzone,
        chroma_set,
    );
    let v = mc_trial(
        &chroma[1],
        x / 2,
        y / 2,
        side / 2,
        mv,
        false,
        ref_v.0,
        ref_v.1,
        ref_v.2,
        ref_v.3,
        false,
        search.base_q_idx,
        search.deadzone,
        chroma_set,
    );
    let skip = luma_trial.levels.iter().all(|&l| l == 0)
        && u.levels.iter().all(|&l| l == 0)
        && v.levels.iter().all(|&l| l == 0);
    let inter_cost = luma_trial.sse
        + u.sse
        + v.sse
        + search.lambda
            * (skip_bits(skip)
                + intra_inter_bits(true)
                + single_ref_bits
                + mode_bits_inter
                + if skip {
                    0.0
                } else {
                    luma_trial.bits + u.bits + v.bits
                });

    // NEARESTMV for 16x16 leaves, gated on real cost. r1/r2 found the gate's
    // desync in `mvstack.rs`'s `find_mv_stack`: it lacked spec 7.10.2.4's
    // extended row/col scan, invisible at 8-mi-only geometry (the immediate
    // scan's own coverage already reached as far) but a real gap at 4-mi
    // leaf geometry. r3 ported the extended scan and, while proving it a
    // no-op at 8-mi (`extended_row_scan_is_a_no_op_at_8mi_geometry_*`,
    // `mvstack.rs`), found and fixed a second bug the extended scan's
    // coverage bookkeeping (`processed_rows`/`processed_cols`) exposed: an
    // intra neighbour is a coded cell too (libaom's `scan_row_mbmi`/
    // `scan_col_mbmi` advance coverage from any candidate's `bsize`,
    // regardless of whether it also casts a ref-frame-matching vote) --
    // `MiGrid` now records intra cells (`tile.rs`/`encode.rs`'s `else`
    // branches), which is what made 8-mi geometry a true no-op instead of
    // one that only held when every neighbour happened to be inter.
    if inter_cost < intra_cost {
        luma.commit(x, y, side, &luma_trial);
        chroma[0].commit(x / 2, y / 2, side / 2, &u);
        chroma[1].commit(x / 2, y / 2, side / 2, &v);
        BlockCoeffs {
            luma: coeffs(&luma_trial.levels, side),
            u: coeffs(&u.levels, side / 2),
            v: coeffs(&v.levels, side / 2),
            mode: DC_PRED as u8,
            skip,
            eight: None,
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv,
            }),
        }
    } else {
        intra_block
    }
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

/// Keeps the DC candidate plus the cheapest `top_k` (or fewer, once DC is set
/// aside) of `scored` (as [`Plane::intra_scores`] built it), in `modes`' own
/// order -- the same DC-always-kept, cheapest-by-SAD rule [`Plane::search_block`]
/// applies, so [`search_inter_block`]'s intra-candidate loop prunes by one
/// rule shared with the key-frame path, not a second one that could diverge.
fn prune_by_sad(modes: &[u8], mut scored: Vec<(f64, u8)>, top_k: usize) -> Vec<u8> {
    let dc_pos = scored.iter().position(|&(_, mode)| mode == DC_PRED);
    let dc_entry = dc_pos.map(|i| scored.remove(i));
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("scores are finite"));
    scored.truncate(if dc_entry.is_some() {
        top_k.saturating_sub(1)
    } else {
        top_k
    });
    if let Some(dc_entry) = dc_entry {
        scored.push(dc_entry);
    }
    let keep: Vec<u8> = scored.into_iter().map(|(_, mode)| mode).collect();
    modes.iter().copied().filter(|m| keep.contains(m)).collect()
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
    /// How many of `modes` the full RD trial (forward transform, quantize,
    /// reconstruct, `coeff_bits`) actually runs on, ranked by a cheap SAD
    /// pre-pass on the prediction residual alone plus DC (always kept, since
    /// libaom's own intra pruning -- `av1/encoder/speed_features.c`'s
    /// `intra_pruning_with_hog` -- never prunes it away). `None` runs every
    /// mode through full RD, unchanged from before this lever.
    top_k: Option<usize>,
}

/// [`Search::top_k`]'s value: `None` keeps every mode's search unchanged
/// (the default, until a lever's quality gate justifies pruning by default);
/// `EC_AV1_PRUNE_K` sweeps it in a test build without touching callers.
fn prune_top_k() -> Option<usize> {
    #[cfg(test)]
    {
        if let Some(k) = TEST_TOP_K_OVERRIDE.with(|cell| cell.get()) {
            return k;
        }
    }
    let swept = std::env::var("EC_AV1_PRUNE_K")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());
    match swept {
        Some(k) if cfg!(test) => Some(k),
        _ => PRUNE_TOP_K,
    }
}

/// A per-thread override [`prune_top_k`] checks first, so one test can sweep
/// `top_k` in-process without mutating the environment (forbidden here --
/// `#![forbid(unsafe_code)]` -- and racy across parallel tests besides).
#[cfg(test)]
thread_local! {
    static TEST_TOP_K_OVERRIDE: std::cell::Cell<Option<Option<usize>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn set_test_top_k_override(value: Option<usize>) {
    TEST_TOP_K_OVERRIDE.with(|cell| cell.set(Some(value)));
}

/// The default: unpruned, thirteen full trials per block, same as before this
/// lever. Set from the sweep in the lane report if the quality gate clears it.
const PRUNE_TOP_K: Option<usize> = None;

/// [`Search::top_k`] for [`search_inter_block`]'s intra-candidate loop --
/// kept apart from [`prune_top_k`]/[`PRUNE_TOP_K`] because the two paths'
/// quality gates were measured separately and may land on different
/// defaults; `EC_AV1_PRUNE_K_INTER` sweeps it without touching
/// [`EC_AV1_PRUNE_K`] or the key-frame path.
fn prune_top_k_inter() -> Option<usize> {
    #[cfg(test)]
    {
        if let Some(k) = TEST_TOP_K_OVERRIDE_INTER.with(|cell| cell.get()) {
            return k;
        }
    }
    let swept = std::env::var("EC_AV1_PRUNE_K_INTER")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());
    match swept {
        Some(k) if cfg!(test) => Some(k),
        _ => INTER_PRUNE_TOP_K,
    }
}

/// A per-thread override [`prune_top_k_inter`] checks first, same reason as
/// [`TEST_TOP_K_OVERRIDE`].
#[cfg(test)]
thread_local! {
    static TEST_TOP_K_OVERRIDE_INTER: std::cell::Cell<Option<Option<usize>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn set_test_top_k_override_inter(value: Option<usize>) {
    TEST_TOP_K_OVERRIDE_INTER.with(|cell| cell.set(Some(value)));
}

/// The inter path's default `top_k`: `Some(3)`, from `prune_k_quality_sweep_inter`
/// (three real clips, q 60 and 150) -- every `K` in {3, 4, 6} cleared the
/// same rule the key-frame lever uses (every PSNR delta <0.05dB, bytes
/// within +1%; worst seen was -0.011dB / +0.147% bytes), so the cheapest,
/// `K=3`, ships as default. Unlike the key-frame path (still `None`), an
/// inter block's intra candidates rarely win against NEWMV/NEARESTMV, so
/// pruning them costs less quality here.
const INTER_PRUNE_TOP_K: Option<usize> = Some(3);

/// Per-thread nanosecond counters `stage_timing_breakdown_inter` (and
/// [`crate::mc::predict`], through [`stage_add`]) accumulate into, so an
/// inter frame's cost can be attributed to a bucket without changing what
/// the search actually does. Index: 0 = whole-call time inside
/// [`motion::search`] (includes bucket 1's time, run from inside it), 1 =
/// [`crate::mc::predict`] (every call, from the motion search's own
/// candidates and from a committed inter block's final prediction alike),
/// 2 = `forward_and_quantize` + `dequant_and_inverse`, 3 = `coeff_bits`.
#[cfg(test)]
thread_local! {
    static STAGE_NS: [std::cell::Cell<u64>; 4] = [
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
    ];
}

#[cfg(test)]
pub(crate) fn stage_add(bucket: usize, dur: std::time::Duration) {
    STAGE_NS.with(|c| c[bucket].set(c[bucket].get() + dur.as_nanos() as u64));
}

#[cfg(test)]
fn stage_reset() {
    STAGE_NS.with(|c| c.iter().for_each(|cell| cell.set(0)));
}

#[cfg(test)]
fn stage_read() -> [std::time::Duration; 4] {
    STAGE_NS.with(|c| {
        c.each_ref()
            .map(|cell| std::time::Duration::from_nanos(cell.get()))
    })
}

/// One mode's coding of one block, before it is committed.
#[derive(Clone)]
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
/// `base_q_idx` is the frame's quantizer index (0..=255); the tile writer
/// picks its coefficient CDFs from one of four q contexts by that index.
/// `deadzone` is the quantizer's rounding offset: 0.5 rounds to nearest, and
/// smaller values trade fidelity for rate.
///
/// The picture may be any even width and height: the encoder pads it by edge
/// replication to a whole number of 32x32 blocks internally, and the frame it
/// writes crops back to the picture's own size (`render_width`/
/// `render_height`), so [`Encoded::reconstruction`] is always the picture's
/// own size, never the padded one.
///
/// # Errors
/// Returns an error when the picture's width or height is zero or odd, or
/// when its planes are not 4:2:0 of that size.
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
    picture.check_even()?;
    let padded = picture.padded_to(BLOCK);
    let encoded = encode_key_frame_inner(
        &padded,
        base_q_idx,
        deadzone,
        modes,
        split_blocks(),
        (picture.width, picture.height),
        unspecified_color_config(),
    )?;
    Ok(crop_encoded(&encoded, picture.width, picture.height))
}

/// [`encode_key_frame_with_modes`] with the partition decision forced, which is
/// what the sweep that sets [`SPLIT_BLOCKS`] measures both ways. `picture` is
/// already padded to a whole number of 32x32 blocks; `render` is the real
/// (pre-pad) size the frame header tells a decoder to crop back to.
pub(crate) fn encode_key_frame_inner(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
    split_blocks: bool,
    render: (usize, usize),
    color_config: ColorConfig,
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
    // The header carries the frame's true (unpadded) size as `frame_width`/
    // `frame_height` -- what a decoder actually crops to -- with `mi_cols`/
    // `mi_rows` derived from that same true size (spec `compute_image_size`).
    // `render_width`/`render_height` come out equal to it too, so
    // `render_and_frame_size_different` is false and no render_size bits are
    // written, mirroring libaom/rav1e: the padded `picture` below is only the
    // internal coding surface, never what the header names.
    let (seq, mut header) = key_frame_headers_colour(render.0, render.1, base_q_idx, color_config)?;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;

    // Prediction reads no further than this, in each plane's own units --
    // see `Plane::true_width`/`true_height`.
    let (true_width, true_height) = (header.mi_cols as usize * 4, header.mi_rows as usize * 4);
    // lane-hbd r4: `Picture.y/u/v` widened to `Vec<u16>` for the decoder's
    // sake (DPB reference slots, 10-bit output); the encoder stays 8-bit by
    // design (see `intra_predict_u8`'s doc comment), so narrow the source
    // once here.
    let picture_y8: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let picture_u8: Vec<u8> = picture.u.iter().map(|&v| v as u8).collect();
    let picture_v8: Vec<u8> = picture.v.iter().map(|&v| v as u8).collect();
    let mut luma = Plane {
        source: &picture_y8,
        reconstruction: vec![128; picture_y8.len()],
        width: picture.width,
        height: picture.height,
        true_width,
        true_height,
    };
    let mut chroma = [
        Plane {
            source: &picture_u8,
            reconstruction: vec![128; picture_u8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
        },
        Plane {
            source: &picture_v8,
            reconstruction: vec![128; picture_v8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
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
        top_k: prune_top_k(),
    };

    // The block grid this frame is coded over: [`crate::tile::block_grid`]'s
    // ceiling of the header's own (true-size-derived) `mi_cols`/`mi_rows`,
    // which may be fewer columns/rows than the padded picture has room for
    // -- a block whose origin sits past the true bound is not coded at all,
    // matching what a decoder derives from `frame_width`/`frame_height` and
    // so never visits either.
    let (cols, rows) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (cols, rows) = (cols as usize, rows as usize);
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

                // spec `decode_partition`'s hasRows/hasCols recomputed at this
                // 32x32 block's own half (see `crate::tile::has_half`): the
                // true frame edge can fall inside a quadrant a superblock-level
                // check already let through. A quadrant that fails either may
                // not be left whole; a 16x16 sub-block that fails either (once
                // split) is a leaf this writer has no rectangular transform
                // for, so the search must not pick a split that would need one.
                let (has_cols32, has_rows32) = (
                    crate::tile::has_half(
                        col as u32 * crate::tile::BLOCK_MI,
                        crate::tile::BLOCK_MI,
                        header.mi_cols,
                    ),
                    crate::tile::has_half(
                        row as u32 * crate::tile::BLOCK_MI,
                        crate::tile::BLOCK_MI,
                        header.mi_rows,
                    ),
                );
                let whole_legal = has_cols32 && has_rows32;
                // A 16x16 sub-block's own hasCols/hasRows (spec
                // `decode_partition`, recomputed at this leaf's own half): one
                // axis false is a straddling leaf this writer codes as two
                // 8x8 leaves (`crate::tile::write_leaf8`, lane-av1-rect r7);
                // both axes false needs a rectangular transform this writer
                // has no size for at all, whether whole or split.
                let sub_half = |sr: usize, sc: usize| {
                    (
                        crate::tile::has_half(
                            sc as u32 * crate::tile::SUB_MI,
                            crate::tile::SUB_MI,
                            header.mi_cols,
                        ),
                        crate::tile::has_half(
                            sr as u32 * crate::tile::SUB_MI,
                            crate::tile::SUB_MI,
                            header.mi_rows,
                        ),
                    )
                };
                let both_axes_cut = (0..4)
                    .map(|i| (r0 + i / 2, c0 + i % 2))
                    .filter(|&(sr, sc)| {
                        (sr as u32) * crate::tile::SUB_MI < header.mi_rows
                            && (sc as u32) * crate::tile::SUB_MI < header.mi_cols
                    })
                    .any(|(sr, sc)| {
                        let (has_cols16, has_rows16) = sub_half(sr, sc);
                        !has_cols16 && !has_rows16
                    });
                if !whole_legal && both_axes_cut {
                    return Err(Error::unsupported(
                        "AV1 encode",
                        format!(
                            "the 32x32 block at ({col},{row}) straddles the true frame edge \
                             in a way that needs a rectangular (HORZ/VERT) transform this \
                             encoder does not code yet"
                        ),
                    ));
                }

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
                let mut split_modes = Vec::with_capacity(4);
                for sub in 0..4 {
                    let (sc, sr) = (c0 + sub % 2, r0 + sub / 2);
                    // Same filter as the writer's `sub_positions` (spec
                    // `decode_partition`'s `r >= MiRows || c >= MiCols` early
                    // return): a sub-block whose own origin sits past the
                    // true frame is never coded, so it must not be searched
                    // or pushed onto `split` either, or the writer's block
                    // count will not match what it is prepared to name.
                    if (sr as u32) * crate::tile::SUB_MI >= header.mi_rows
                        || (sc as u32) * crate::tile::SUB_MI >= header.mi_cols
                    {
                        continue;
                    }
                    let (has_cols16, has_rows16) = sub_half(sr, sc);
                    if has_cols16 && has_rows16 {
                        let (block, cost) = code_square(
                            &mut luma,
                            &mut chroma,
                            (x + (sub % 2) * SUB, y + (sub / 2) * SUB),
                            SUB,
                            &search,
                            &mode_bits(above_mode[sc], left_mode[sr]),
                        );
                        cost_split += cost;
                        split_modes.push(block.mode);
                        above_mode[sc] = block.mode;
                        left_mode[sr] = block.mode;
                        split.push(block);
                    } else {
                        // A straddling 16x16: two (or, at a true corner, one)
                        // 8x8 leaves, in the raster order `write_leaf8`'s
                        // caller expects. Both leaves cost against the SAME
                        // enclosing-slot mode context (see
                        // `crate::tile::write_leaf8`'s doc), and neither
                        // updates `above_mode`/`left_mode` at this SUB slot --
                        // the writer never does either, so the search must
                        // not diverge from what it will actually read next.
                        let leaf_mode_bits = mode_bits(above_mode[sc], left_mode[sr]);
                        let (x_sub, y_sub) = (x + (sub % 2) * SUB, y + (sub / 2) * SUB);
                        let mut leaves = Vec::with_capacity(2);
                        for i in 0..4 {
                            let leaf_x = x_sub + (i % 2) * 8;
                            let leaf_y = y_sub + (i / 2) * 8;
                            if leaf_x >= luma.true_width || leaf_y >= luma.true_height {
                                continue;
                            }
                            let (leaf, cost) = code_square(
                                &mut luma,
                                &mut chroma,
                                (leaf_x, leaf_y),
                                8,
                                &search,
                                &leaf_mode_bits,
                            );
                            cost_split += cost;
                            split_modes.push(leaf.mode);
                            leaves.push(leaf);
                        }
                        split.push(BlockCoeffs {
                            eight: Some(leaves),
                            ..BlockCoeffs::default()
                        });
                    }
                }

                // A split whose subs are all whole 16x16 is a real quality
                // alternative to `whole`, exactly as before. Through r14, a
                // split carrying an 8x8-leaf sub was only ever forced
                // (`!whole_legal`), never chosen on cost, because the leaf
                // path was not yet proven against a real decoder; r15 proved
                // it against ffmpeg, and lane-av1dec r5 taught crate::decode
                // the read path (`decode_leaf8`), so leaf8 can now win on
                // cost anywhere, same as any other split.
                if !whole_legal || (split_blocks && cost_split < cost_whole) {
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
        inter_block_share: 0.0,
        reconstruction: Picture {
            width: luma.width,
            height: luma.height,
            y: luma.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            u: u.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            v: v.reconstruction.iter().map(|&v| u16::from(v)).collect(),
        },
        tile,
        mi_cols: header.mi_cols,
        mi_rows: header.mi_rows,
        base_q_idx,
    })
}

/// `Y_MODE`'s size group (spec `Size_Group`) for a 32x32 block: the only
/// group an inter frame's intra branch codes (spec `inter_frame_mode_info`),
/// since every block this crate's inter tile writer codes is 32x32.
const SIZE_GROUP_32: usize = 3;

/// `Ref_Frame_List`'s `LAST_FRAME` (spec 3): the only reference
/// [`sb_coeff_inter_frame_tile`] ever names, and so the only one the MV
/// stack this encoder builds is ever asked to predict against.
const LAST_FRAME: i8 = 1;

/// What an inter frame's intra branch spends to name each of the thirteen
/// modes: `Y_MODE` by size group rather than `KF_Y_MODE` by neighbour
/// context, since an inter frame's intra blocks do not read their
/// neighbours' modes (spec `inter_frame_mode_info`, `sb_coeff_inter_frame_tile`'s
/// own doc comment). Unlike [`mode_bits`] this needs no per-block neighbour
/// state, so it is built once per frame.
fn inter_mode_bits() -> [f64; 13] {
    std::array::from_fn(|mode| {
        let angle = if (usize::from(V_PRED)..=usize::from(D67_PRED)).contains(&mode) {
            symbol_bits(&cdf::ANGLE_DELTA[mode - usize::from(V_PRED)], 3)
        } else {
            0.0
        };
        symbol_bits(&cdf::Y_MODE[SIZE_GROUP_32], mode)
            + angle
            + symbol_bits(&cdf::UV_MODE_CFL[mode], usize::from(DC_PRED))
    })
}

/// A 1/8-pel motion vector component, converted to the 1/16-pel offset
/// [`mc::predict`] takes, in the units of the plane it predicts into.
///
/// A luma sample of displacement is `mv/8`; in the 1/16-pel domain that is
/// `mv*2`. A 4:2:0 chroma sample is half a luma sample wide, so the same
/// physical displacement is `mv/16` chroma samples — `mv*1` in the 1/16-pel
/// domain. `luma` picks which.
fn mv_to_q4(pos: usize, mv_component: i32, luma: bool) -> i32 {
    (pos as i32) * 16 + mv_component * if luma { 2 } else { 1 }
}

/// `CLASS0_SIZE << (class + 2)`, mirroring `tile::mv_class_base` (private to
/// that module): the magnitude an `MV_CLASS_n` component's own bits start
/// counting from.
fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// The class a pre-offset magnitude `z` (`|diff| - 1`) falls in, mirroring
/// `tile::mv_class_of`.
fn mv_class_of(z: i32) -> usize {
    let mut class = 0;
    while class < 10 && mv_class_base(class + 1) <= z {
        class += 1;
    }
    class
}

/// What [`crate::tile::write_mv_component`] (private to that module) would
/// spend coding one non-zero motion vector component, against the same
/// static default CDFs the writer starts a frame from — the same
/// static-table approximation [`mode_bits`] costs the luma mode symbol
/// through, not the tile writer's own adapting state, which this encoder has
/// no way to read without duplicating it block for block.
///
/// Returns `None` when `diff` needs the eighth-pel precision this crate's
/// frames never carry (`allow_high_precision_mv` is always off), which is
/// what [`round_to_valid_mv`] exists to avoid ever happening.
fn mv_component_bits(diff: i32) -> Option<f64> {
    let mag = diff.unsigned_abs() as i32;
    let z = mag - 1;
    if z & 1 == 0 {
        return None;
    }
    let mut bits = symbol_bits(&cdf::MV_SIGN, usize::from(diff < 0));
    let class = mv_class_of(z);
    bits += symbol_bits(&cdf::MV_CLASS, class);
    let local = z - mv_class_base(class);
    if class == 0 {
        let bit = (local >> 3) & 1;
        let fr = (local >> 1) & 3;
        bits += symbol_bits(&cdf::MV_CLASS0_BIT, bit as usize);
        bits += symbol_bits(&cdf::MV_CLASS0_FR[bit as usize], fr as usize);
    } else {
        let d = local >> 3;
        let fr = (local >> 1) & 3;
        for i in 0..class {
            bits += symbol_bits(&cdf::MV_BIT[i], ((d >> i) & 1) as usize);
        }
        bits += symbol_bits(&cdf::MV_FR, fr as usize);
    }
    Some(bits)
}

/// What [`crate::tile::write_mv`] (private to that module) would spend
/// coding `mv` as a residual against `pred`: the joint symbol naming which
/// components differ, then each differing component. `None` under the same
/// condition [`mv_component_bits`] returns `None`.
fn mv_residual_bits(mv: (i32, i32), pred: (i32, i32)) -> Option<f64> {
    let diff = (mv.0 - pred.0, mv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0,
        (false, true) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    let mut bits = symbol_bits(&cdf::MV_JOINT, joint);
    for d in [diff.0, diff.1] {
        if d != 0 {
            bits += mv_component_bits(d)?;
        }
    }
    Some(bits)
}

/// Rounds `mv` so that its residual against `pred` is one
/// [`crate::tile::write_mv_component`] can actually code: each component's
/// difference is either zero or has an even magnitude (the eighth-pel bit
/// `allow_high_precision_mv` off always infers as one forces the coded
/// magnitude odd one step further in, which comes out even here since this
/// crate's motion search works in whole units of that same step). Rounding
/// down keeps every coded MV — and so every `nearest_mv` a later block's
/// stack votes with — built from an even displacement from `(0, 0)`, so the
/// invariant holds without this function seeing the whole stack.
fn round_to_valid_mv(mv: (i32, i32), pred: (i32, i32)) -> (i32, i32) {
    let round = |m: i32, p: i32| {
        let diff = m - p;
        let mag = diff.unsigned_abs() as i32;
        let even = mag - mag % 2;
        if even == 0 {
            p
        } else {
            p + even * diff.signum()
        }
    };
    (round(mv.0, pred.0), round(mv.1, pred.1))
}

/// Predicts one `side`-square block by motion compensation from `reference`
/// at `mv` (1/8-pel), then codes it through [`Plane::code_from_prediction`].
#[allow(clippy::too_many_arguments)]
fn mc_trial(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    mv: (i32, i32),
    luma: bool,
    reference: &[u16],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    skip: bool,
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet,
) -> Trial {
    let x_q4 = mv_to_q4(x, mv.1, luma);
    let y_q4 = mv_to_q4(y, mv.0, luma);
    // lane-hbd r4: `reference` (a DPB `Picture`'s plane) is `u16` now that
    // `Picture` is widened; `prediction`/the encoder stay `u8` (encoder is
    // 8-bit only this round, see `intra_predict_u8`'s doc comment).
    let mut prediction16 = vec![0u16; side * side];
    mc::predict(
        reference,
        stride,
        ref_width,
        ref_height,
        x_q4,
        y_q4,
        side,
        side,
        &mut prediction16,
    );
    let prediction: Vec<u8> = prediction16.iter().map(|&v| v as u8).collect();
    plane.code_from_prediction(x, y, side, &prediction, skip, base_q_idx, deadzone, set)
}

/// Codes one 32x32 block of an inter frame as whichever costs least of: each
/// intra mode [`Search::modes`] offers (predicted exactly as a key frame's
/// block is), `NEARESTMV`, and `NEWMV` searched from `reference` by
/// [`motion::search`] and seeded by `stack.pred_mv` — both inter candidates
/// coding a real residual (`skip: false`) whenever one prices out cheaper
/// than the all-skip candidate, now that `sb_coeff_inter_frame_tile`'s
/// missing inter `tx_type` symbol (the desync this function's candidates
/// used to route around; see `crate::cdf_state::TxbSet::Luma32Inter`) is
/// fixed.
///
/// The symbol costs this ranks candidates by use fixed contexts (0) for
/// `skip`, `intra_inter` and `single_ref` — the tile writer's actual
/// contexts for those come from its own neighbour bookkeeping
/// (`tile::Neighbours`), which is private to that module and not worth
/// duplicating for three one-or-two-bit symbols; `new_mv`/`ref_mv`/`zero_mv`/
/// `drl_mode` contexts come straight from `stack`, which is exact, because
/// [`find_mv_stack`] is public and this function is handed the same `MvStack`
/// the tile writer derives from the same grid state.
#[allow(clippy::too_many_arguments)]
fn search_inter_block(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
) -> BlockCoeffs {
    let (luma_set, chroma_set) = (TxbSet::Luma32, TxbSet::Chroma16);
    let reach = Reach::of(BLOCK, x, y, luma.true_width, luma.true_height);

    // skip / intra_inter symbol costs at a fixed context -- see this
    // function's doc comment.
    let skip_bits = |skip: bool| symbol_bits(&cdf::SKIP[0], usize::from(skip));
    let intra_inter_bits = |inter: bool| symbol_bits(&cdf::INTRA_INTER[0], usize::from(inter));
    let single_ref_bits = symbol_bits(&cdf::SINGLE_REF[0][0], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][2], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][3], 0);

    struct Candidate {
        cost: f64,
        luma: Trial,
        u: Trial,
        v: Trial,
        mode: u8,
        skip: bool,
        inter: Option<InterInfo>,
    }
    let mut best: Option<Candidate> = None;
    let mut consider = |candidate: Candidate| {
        if best.as_ref().is_none_or(|b| candidate.cost < b.cost) {
            best = Some(candidate);
        }
    };

    // The chroma trial an intra candidate prices does not depend on `mode`
    // at all -- both planes always code `DC_PRED` here (a coding choice this
    // search inherited, not this loop's to second-guess) at the same `reach:
    // Reach::none()` -- so it is the same call with the same inputs on every
    // one of the thirteen iterations below; run it once and reuse the result
    // rather than repeat an already-priced transform/quantize/entropy call
    // twelve times for nothing (bit-identical: the value each iteration
    // reads back is exactly what it used to recompute).
    let u_trial = chroma[0].trial(
        At {
            x: x / 2,
            y: y / 2,
            side: BLOCK / 2,
            reach: Reach::none(),
            set: chroma_set,
        },
        DC_PRED,
        search.base_q_idx,
        search.deadzone,
    );
    let v_trial = chroma[1].trial(
        At {
            x: x / 2,
            y: y / 2,
            side: BLOCK / 2,
            reach: Reach::none(),
            set: chroma_set,
        },
        DC_PRED,
        search.base_q_idx,
        search.deadzone,
    );

    let intra_modes: Vec<u8> = match search.top_k {
        Some(k) if k < search.modes.len() => {
            let scored = luma.intra_scores(
                At {
                    x,
                    y,
                    side: BLOCK,
                    reach,
                    set: luma_set,
                },
                search.modes,
                mode_bits,
                search.lambda,
            );
            prune_by_sad(search.modes, scored, k)
        }
        _ => search.modes.to_vec(),
    };
    for &mode in &intra_modes {
        let luma_trial = luma.trial(
            At {
                x,
                y,
                side: BLOCK,
                reach,
                set: luma_set,
            },
            mode,
            search.base_q_idx,
            search.deadzone,
        );
        let u = u_trial.clone();
        let v = v_trial.clone();
        let cost = luma_trial.sse
            + u.sse
            + v.sse
            + search.lambda
                * (luma_trial.bits
                    + u.bits
                    + v.bits
                    + mode_bits[usize::from(mode)]
                    + skip_bits(false)
                    + intra_inter_bits(false));
        consider(Candidate {
            cost,
            luma: luma_trial,
            u,
            v,
            mode,
            skip: false,
            inter: None,
        });
    }

    // The reference frame buffer a spec decoder holds is exactly the true
    // (unpadded) coded extent -- a superblock whose partition stops early at
    // `has_cols`/`has_rows` never codes syntax for the padding columns/rows
    // beyond it, so those samples are never part of the decoded picture.
    // Motion compensation clamps reads to that true extent (spec 7.11.3.4's
    // reference sample position clamp), not to this crate's own
    // padded-to-`SUPERBLOCK` coding surface -- passing the padded width/height
    // here would let a fractional-pel read past the true edge pick up this
    // encoder's own uncoded padding instead of the true edge's replicated
    // value a decoder computes.
    let ref_luma = (
        &reference.y,
        reference.width,
        luma.true_width,
        luma.true_height,
    );
    let ref_u = (
        &reference.u,
        reference.width / 2,
        chroma[0].true_width,
        chroma[0].true_height,
    );
    let ref_v = (
        &reference.v,
        reference.width / 2,
        chroma[1].true_width,
        chroma[1].true_height,
    );
    let mode_bits_inter = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1) // not NEWMV
            + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1) // not zero
            + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0); // NEARESTMV

    // The NEARESTMV candidate: `skip` is decided below from whether its
    // trial actually found a nonzero level, once its residual is priced
    // against the same CDFs the tile writer codes it with.
    {
        let mv = stack.nearest_mv;
        let luma_trial = mc_trial(
            luma,
            x,
            y,
            BLOCK,
            mv,
            true,
            ref_luma.0,
            ref_luma.1,
            ref_luma.2,
            ref_luma.3,
            false,
            search.base_q_idx,
            search.deadzone,
            luma_set,
        );
        let u = mc_trial(
            &chroma[0],
            x / 2,
            y / 2,
            BLOCK / 2,
            mv,
            false,
            ref_u.0,
            ref_u.1,
            ref_u.2,
            ref_u.3,
            false,
            search.base_q_idx,
            search.deadzone,
            chroma_set,
        );
        let v = mc_trial(
            &chroma[1],
            x / 2,
            y / 2,
            BLOCK / 2,
            mv,
            false,
            ref_v.0,
            ref_v.1,
            ref_v.2,
            ref_v.3,
            false,
            search.base_q_idx,
            search.deadzone,
            chroma_set,
        );
        let skip = luma_trial.levels.iter().all(|&l| l == 0)
            && u.levels.iter().all(|&l| l == 0)
            && v.levels.iter().all(|&l| l == 0);
        let cost = luma_trial.sse
            + u.sse
            + v.sse
            + search.lambda
                * (skip_bits(skip)
                    + intra_inter_bits(true)
                    + single_ref_bits
                    + mode_bits_inter
                    + if skip {
                        0.0
                    } else {
                        luma_trial.bits + u.bits + v.bits
                    });
        consider(Candidate {
            cost,
            luma: luma_trial,
            u,
            v,
            mode: DC_PRED,
            skip,
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv,
            }),
        });
    }

    let source_block = luma.source_block(x, y, BLOCK);
    // lane-hbd r4: `motion::search` is 8-bit only (its SAD/cost math is
    // integer-`u8`-scale); narrow the DPB reference plane locally.
    let ref_luma_8: Vec<u8> = ref_luma.0.iter().map(|&v| v as u8).collect();
    #[cfg(test)]
    let t = std::time::Instant::now();
    let found = motion::search(
        &ref_luma_8,
        ref_luma.1,
        ref_luma.2,
        ref_luma.3,
        &source_block,
        x,
        y,
        BLOCK,
        BLOCK,
        stack.pred_mv,
        search.lambda,
    );
    #[cfg(test)]
    stage_add(0, t.elapsed());
    let mv = round_to_valid_mv(found.mv, stack.pred_mv);
    if let Some(mv_bits) = mv_residual_bits(mv, stack.pred_mv) {
        let luma_trial = mc_trial(
            luma,
            x,
            y,
            BLOCK,
            mv,
            true,
            ref_luma.0,
            ref_luma.1,
            ref_luma.2,
            ref_luma.3,
            false,
            search.base_q_idx,
            search.deadzone,
            luma_set,
        );
        let u = mc_trial(
            &chroma[0],
            x / 2,
            y / 2,
            BLOCK / 2,
            mv,
            false,
            ref_u.0,
            ref_u.1,
            ref_u.2,
            ref_u.3,
            false,
            search.base_q_idx,
            search.deadzone,
            chroma_set,
        );
        let v = mc_trial(
            &chroma[1],
            x / 2,
            y / 2,
            BLOCK / 2,
            mv,
            false,
            ref_v.0,
            ref_v.1,
            ref_v.2,
            ref_v.3,
            false,
            search.base_q_idx,
            search.deadzone,
            chroma_set,
        );
        let drl_bits = if stack.entries.len() > 1 {
            symbol_bits(&cdf::DRL_MODE[stack.drl_ctx[0]], 0)
        } else {
            0.0
        };
        let skip = luma_trial.levels.iter().all(|&l| l == 0)
            && u.levels.iter().all(|&l| l == 0)
            && v.levels.iter().all(|&l| l == 0);
        let cost = luma_trial.sse
            + u.sse
            + v.sse
            + search.lambda
                * (skip_bits(skip)
                    + intra_inter_bits(true)
                    + single_ref_bits
                    + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                    + drl_bits
                    + mv_bits
                    + if skip {
                        0.0
                    } else {
                        luma_trial.bits + u.bits + v.bits
                    });
        consider(Candidate {
            cost,
            luma: luma_trial,
            u,
            v,
            mode: DC_PRED,
            skip,
            inter: Some(InterInfo {
                mode: InterMode::NewMv,
                mv,
            }),
        });
    }

    let best = best.expect("the search offers at least the intra modes");
    luma.commit(x, y, BLOCK, &best.luma);
    chroma[0].commit(x / 2, y / 2, BLOCK / 2, &best.u);
    chroma[1].commit(x / 2, y / 2, BLOCK / 2, &best.v);
    BlockCoeffs {
        luma: coeffs(&best.luma.levels, BLOCK),
        u: coeffs(&best.u.levels, BLOCK / 2),
        v: coeffs(&best.v.levels, BLOCK / 2),
        mode: best.mode,
        skip: best.skip,
        inter: best.inter,
        eight: None,
    }
}

/// Encodes one picture as an inter frame predicting from `reference`'s
/// reconstruction, which the caller decoded (or, for the frame right after
/// the key frame, is the key frame's own reconstruction).
pub(crate) fn encode_inter_frame(
    picture: &Picture,
    reference: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    order_hint: u32,
    render: (usize, usize),
) -> Result<Encoded> {
    picture.check()?;
    if !picture.width.is_multiple_of(SUPERBLOCK) || !picture.height.is_multiple_of(SUPERBLOCK) {
        return Err(Error::unsupported(
            "AV1 encode",
            "an inter frame is coded only for a picture that is a whole number \
             of 64x64 superblocks -- the inter tile writer never splits a \
             superblock's partition below that",
        ));
    }
    if reference.width != picture.width || reference.height != picture.height {
        return Err(Error::unsupported(
            "AV1 encode",
            "the reference picture must be the same size as the one being coded",
        ));
    }

    // The single slot this crate's inter frames ever predict from or
    // refresh (`inter_frame_headers`'s contract): every frame both reads and
    // overwrites slot 0, so the frame just coded is always what the next one
    // predicts against.
    const LAST_SLOT: u8 = 0;
    // `render`, not `picture.width`/`picture.height`: `picture` here is
    // already padded to a whole number of superblocks (`encode_sequence`'s
    // `padded_to(SUPERBLOCK)`), and the header's `mi_cols`/`mi_rows` (and so
    // every true-edge clamp downstream) must come from the frame's real,
    // unpadded size, same as `key_frame_headers_colour` takes `render` and
    // not the padded picture in `encode_key_frame_inner`.
    let (seq, mut header) =
        inter_frame_headers(render.0, render.1, base_q_idx, order_hint, LAST_SLOT)?;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;

    let (true_width, true_height) = (header.mi_cols as usize * 4, header.mi_rows as usize * 4);
    // lane-hbd r4: encoder stays 8-bit by design; narrow the source once
    // here (see `encode_key_frame_inner`).
    let picture_y8: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let picture_u8: Vec<u8> = picture.u.iter().map(|&v| v as u8).collect();
    let picture_v8: Vec<u8> = picture.v.iter().map(|&v| v as u8).collect();
    let mut luma = Plane {
        source: &picture_y8,
        reconstruction: vec![128; picture_y8.len()],
        width: picture.width,
        height: picture.height,
        true_width,
        true_height,
    };
    let mut chroma = [
        Plane {
            source: &picture_u8,
            reconstruction: vec![128; picture_u8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
        },
        Plane {
            source: &picture_v8,
            reconstruction: vec![128; picture_v8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
        },
    ];

    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale() * step * step,
        modes: &KEY_FRAME_MODES,
        top_k: prune_top_k_inter(),
    };
    let mode_bits_table = inter_mode_bits();

    // The true grid ([`crate::tile::block_grid`]'s ceiling), not the padded
    // coding surface's: `blocks` carries one entry per 32x32 block the tile
    // writer will actually visit, same count it checks `blocks.len()`
    // against, which is fewer than the padded surface's own block count
    // whenever the true size is not a 64x64 multiple.
    let (cols, rows) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (cols, rows) = (cols as usize, rows as usize);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let (mi_cols, mi_rows) = (header.mi_cols as usize, header.mi_rows as usize);
    let mut grid = MiGrid::new(mi_cols, mi_rows);
    let mut blocks = vec![Quadrant::Whole(BlockCoeffs::default()); cols * rows];

    for sb_r in 0..sb_rows {
        for sb_c in 0..sb_cols {
            for quadrant in 0..4 {
                let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                // Same filter as the tile writer's: a quadrant whose own mi
                // origin is not inside the true frame is never coded.
                if r32 >= rows || c32 >= cols {
                    continue;
                }
                // spec `decode_partition`'s hasRows/hasCols recomputed at this
                // 32x32 block's own half (`crate::tile::has_half`), same as
                // the key frame search: the true frame edge can fall inside a
                // quadrant a superblock-level check already let through, and
                // such a quadrant cannot be left whole -- it must split into
                // the 16x16 leaves that are actually inside the true frame
                // (`crate::tile::sb_coeff_inter_frame_tile`'s `Quadrant::Split`
                // arm).
                let (has_cols32, has_rows32) = (
                    crate::tile::has_half(
                        c32 as u32 * crate::tile::BLOCK_MI,
                        crate::tile::BLOCK_MI,
                        header.mi_cols,
                    ),
                    crate::tile::has_half(
                        r32 as u32 * crate::tile::BLOCK_MI,
                        crate::tile::BLOCK_MI,
                        header.mi_rows,
                    ),
                );
                if has_cols32 && has_rows32 {
                    let (x, y) = (c32 * BLOCK, r32 * BLOCK);
                    let (mi_row, mi_col) = (r32 * 8, c32 * 8);
                    let stack =
                        find_mv_stack(&grid, mi_row, mi_col, 8, 8, LAST_FRAME, mi_cols, mi_rows);

                    let block = search_inter_block(
                        &mut luma,
                        &mut chroma,
                        (x, y),
                        &search,
                        &mode_bits_table,
                        reference,
                        &stack,
                    );

                    for dr in 0..8 {
                        for dc in 0..8 {
                            grid.set(
                                mi_row + dr,
                                mi_col + dc,
                                match block.inter {
                                    Some(info) => MiInfo {
                                        is_inter: true,
                                        ref_frame: LAST_FRAME,
                                        ref_frame1: None,
                                        mv1: None,
                                        mv: info.mv,
                                        is_new_mv: matches!(info.mode, InterMode::NewMv),
                                        size: 8,
                                        size_h: 8,
                                        is_global_mv0: false,
                                        is_global_mv1: false,
                                    },
                                    // Intra: no vote, but still a coded cell
                                    // -- mvstack's extended-scan coverage
                                    // (`processed_rows`/`processed_cols`)
                                    // must see it (module doc).
                                    None => MiInfo {
                                        is_inter: false,
                                        ref_frame: -1,
                                        ref_frame1: None,
                                        mv1: None,
                                        mv: (0, 0),
                                        is_new_mv: false,
                                        size: 8,
                                        size_h: 8,
                                        is_global_mv0: false,
                                        is_global_mv1: false,
                                    },
                                },
                            );
                        }
                    }
                    blocks[r32 * cols + c32] = Quadrant::Whole(block);
                } else {
                    // The sub-positions actually inside the true frame, same
                    // filter as `sb_coeff_inter_frame_tile`'s `sub_positions`.
                    let sub_positions: Vec<(usize, usize)> = (0..4)
                        .map(|i| (r32 * 2 + i / 2, c32 * 2 + i % 2))
                        .filter(|&(sr, sc)| {
                            (sr as u32) * crate::tile::SUB_MI < header.mi_rows
                                && (sc as u32) * crate::tile::SUB_MI < header.mi_cols
                        })
                        .collect();
                    // Each leaf searches intra against its own `NEARESTMV`
                    // candidate (`code_square_inter`), same real-inter choice
                    // `search_inter_block` makes for a whole 32x32 block, at
                    // this leaf's own 4x4-mi mv-stack window -- unless the
                    // leaf's own half itself straddles the true edge, which
                    // it codes as two (or one, at a true corner) 8x8 leaves
                    // (`crate::tile::write_leaf8`'s inter counterpart),
                    // mirroring the key frame search's straddling-16x16
                    // handling above (lane-av1inter8).
                    let mut leaves = Vec::with_capacity(sub_positions.len());
                    for (sr, sc) in sub_positions {
                        let (has_cols16, has_rows16) = (
                            crate::tile::has_half(
                                sc as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_cols,
                            ),
                            crate::tile::has_half(
                                sr as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_rows,
                            ),
                        );
                        if !has_cols16 && !has_rows16 {
                            return Err(Error::unsupported(
                                "AV1 encode",
                                format!(
                                    "the 16x16 block at ({sc},{sr}) straddles the true \
                                     frame edge in a way that needs a rectangular \
                                     (HORZ/VERT) transform this encoder does not code yet"
                                ),
                            ));
                        }
                        if has_cols16 && has_rows16 {
                            let (x, y) = (sc * SUB, sr * SUB);
                            let (mi_row, mi_col) = (sr * 4, sc * 4);
                            let stack = find_mv_stack(
                                &grid, mi_row, mi_col, 4, 4, LAST_FRAME, mi_cols, mi_rows,
                            );
                            let block = code_square_inter(
                                &mut luma,
                                &mut chroma,
                                (x, y),
                                SUB,
                                &search,
                                &mode_bits_table,
                                reference,
                                &stack,
                            );
                            for dr in 0..4 {
                                for dc in 0..4 {
                                    grid.set(
                                        mi_row + dr,
                                        mi_col + dc,
                                        match block.inter {
                                            Some(info) => MiInfo {
                                                is_inter: true,
                                                ref_frame: LAST_FRAME,
                                                ref_frame1: None,
                                                mv1: None,
                                                mv: info.mv,
                                                is_new_mv: matches!(info.mode, InterMode::NewMv),
                                                size: 4,
                                                size_h: 4,
                                                is_global_mv0: false,
                                                is_global_mv1: false,
                                            },
                                            None => MiInfo {
                                                is_inter: false,
                                                ref_frame: -1,
                                                ref_frame1: None,
                                                mv1: None,
                                                mv: (0, 0),
                                                is_new_mv: false,
                                                size: 4,
                                                size_h: 4,
                                                is_global_mv0: false,
                                                is_global_mv1: false,
                                            },
                                        },
                                    );
                                }
                            }
                            leaves.push(block);
                        } else {
                            let (x_sub, y_sub) = (sc * SUB, sr * SUB);
                            let (mi_row0, mi_col0) = (sr * 4, sc * 4);
                            let mut eight = Vec::with_capacity(2);
                            for i in 0..4 {
                                let leaf_x = x_sub + (i % 2) * 8;
                                let leaf_y = y_sub + (i / 2) * 8;
                                if leaf_x >= luma.true_width || leaf_y >= luma.true_height {
                                    continue;
                                }
                                let (mi_row, mi_col) =
                                    (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2);
                                let stack = find_mv_stack(
                                    &grid, mi_row, mi_col, 2, 2, LAST_FRAME, mi_cols, mi_rows,
                                );
                                let leaf = code_square_inter(
                                    &mut luma,
                                    &mut chroma,
                                    (leaf_x, leaf_y),
                                    8,
                                    &search,
                                    &mode_bits_table,
                                    reference,
                                    &stack,
                                );
                                for dr in 0..2 {
                                    for dc in 0..2 {
                                        grid.set(
                                            mi_row + dr,
                                            mi_col + dc,
                                            match leaf.inter {
                                                Some(info) => MiInfo {
                                                    is_inter: true,
                                                    ref_frame: LAST_FRAME,
                                                    ref_frame1: None,
                                                    mv1: None,
                                                    mv: info.mv,
                                                    is_new_mv: matches!(
                                                        info.mode,
                                                        InterMode::NewMv
                                                    ),
                                                    size: 2,
                                                    size_h: 2,
                                                    is_global_mv0: false,
                                                    is_global_mv1: false,
                                                },
                                                None => MiInfo {
                                                    is_inter: false,
                                                    ref_frame: -1,
                                                    ref_frame1: None,
                                                    mv1: None,
                                                    mv: (0, 0),
                                                    is_new_mv: false,
                                                    size: 2,
                                                    size_h: 2,
                                                    is_global_mv0: false,
                                                    is_global_mv1: false,
                                                },
                                            },
                                        );
                                    }
                                }
                                eight.push(leaf);
                            }
                            leaves.push(BlockCoeffs {
                                eight: Some(eight),
                                ..BlockCoeffs::default()
                            });
                        }
                    }
                    blocks[r32 * cols + c32] = Quadrant::Split(leaves);
                }
            }
        }
    }

    let modes = blocks
        .iter()
        .flat_map(Quadrant::blocks)
        .map(|b| b.mode)
        .collect::<Vec<_>>();
    let inter_block_share = blocks
        .iter()
        .flat_map(Quadrant::blocks)
        .filter(|b| b.inter.is_some())
        .count() as f64
        / modes.len() as f64;
    let tile = sb_coeff_inter_frame_tile(header.mi_cols, header.mi_rows, base_q_idx, &blocks)?;
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile)?);

    let [u, v] = chroma;
    Ok(Encoded {
        stream,
        modes,
        inter_block_share,
        reconstruction: Picture {
            width: luma.width,
            height: luma.height,
            y: luma.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            u: u.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            v: v.reconstruction.iter().map(|&v| u16::from(v)).collect(),
        },
        tile,
        mi_cols: header.mi_cols,
        mi_rows: header.mi_rows,
        base_q_idx,
    })
}

/// What [`encode_sequence`] produced: the concatenated AV1 stream -- a key
/// frame's temporal unit followed by one inter frame's temporal unit per
/// remaining picture, all sharing the key frame's sequence header -- and
/// each frame's own [`Encoded`], in coding order.
#[derive(Clone, Debug)]
pub struct EncodedSequence {
    /// The whole stream: `frames[0].stream` (a temporal delimiter, the
    /// sequence header and the key frame) followed by each later frame's
    /// `stream` (a temporal delimiter and that frame's OBU alone; it reuses
    /// the sequence header the key frame carried).
    pub stream: Vec<u8>,
    /// One entry per picture, in coding order.
    pub frames: Vec<Encoded>,
}

/// Encodes a sequence of pictures as a key frame followed by one inter frame
/// per remaining picture, each predicting from the previous frame's own
/// decoded reconstruction — never from the source, which is what a decoder
/// cannot see. Every picture must be the same even size (any even width and
/// height — each is padded to a whole number of 64x64 superblocks
/// internally, which is what lets an inter frame's reference always match
/// the frame being coded against it), and `pictures` must be non-empty.
///
/// # Errors
/// Returns an error when `pictures` is empty, when any picture besides the
/// first is not the same size as the first, or under the same conditions
/// [`encode_key_frame`] and the inter frame path do.
pub fn encode_sequence(
    pictures: &[Picture],
    base_q_idx: u8,
    deadzone: f64,
) -> Result<EncodedSequence> {
    let Some((first, rest)) = pictures.split_first() else {
        return Err(Error::unsupported(
            "AV1 encode",
            "a sequence needs at least one picture",
        ));
    };
    first.check_even()?;
    let render = (first.width, first.height);
    // The key frame and every inter frame after it are coded at the same
    // padded size, so the reference each inter frame predicts from -- the
    // previous frame's own uncropped reconstruction, at the size a decoder's
    // reference buffer actually holds -- always matches the frame being
    // coded against it.
    let key = encode_key_frame_inner(
        &first.padded_to(SUPERBLOCK),
        base_q_idx,
        deadzone,
        &KEY_FRAME_MODES,
        split_blocks(),
        render,
        unspecified_color_config(),
    )?;
    let mut stream = key.stream.clone();
    let mut reference = key.reconstruction.clone();
    let mut frames = vec![crop_encoded(&key, render.0, render.1)];
    for (i, picture) in rest.iter().enumerate() {
        picture.check_even()?;
        if (picture.width, picture.height) != render {
            return Err(Error::unsupported(
                "AV1 encode",
                "every picture in a sequence must be the same size as the first",
            ));
        }
        let order_hint = (i + 1) as u32;
        let inter = encode_inter_frame(
            &picture.padded_to(SUPERBLOCK),
            &reference,
            base_q_idx,
            deadzone,
            order_hint,
            render,
        )?;
        stream.extend_from_slice(&inter.stream);
        reference = inter.reconstruction.clone();
        frames.push(crop_encoded(&inter, render.0, render.1));
    }
    Ok(EncodedSequence { stream, frames })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intra::{D45_PRED, D135_PRED, H_PRED, KEY_FRAME_MODES, NON_DIRECTIONAL, V_PRED};
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
            y: out.stdout[..luma].iter().map(|&v| u16::from(v)).collect(),
            u: out.stdout[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
            v: out.stdout[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
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
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (x * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
            }
        }
        picture
    }

    fn psnr(a: &[u16], b: &[u16]) -> f64 {
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
        for &(width, height) in &[(64usize, 64usize), (96, 64), (160, 96), (32, 48)] {
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
        // One q index from each of the four coefficient-CDF contexts
        // (0..=20, 21..=60, 61..=120, 121..=255), on a single frame size.
        let (width, height) = (64usize, 64usize);
        let picture = test_card(width, height);
        for &q in &[15u8, 45, 100, 200] {
            let encoded = encode_key_frame(&picture, q, 0.5).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(decoded.y, encoded.reconstruction.y, "q={q}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "q={q}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "q={q}: V");
        }
    }

    /// The minimal repro that isolated the inter-residual desync: a key frame
    /// followed by one inter frame whose single superblock carries one
    /// `NEARESTMV` block (`mv == (0, 0)`) with exactly one nonzero luma
    /// coefficient, its three sibling blocks all `skip: true`. Before the
    /// `is_inter` transform-block fix this broke ffmpeg/dav1d ("Invalid data
    /// found when processing input") even though every symbol either block
    /// codes is otherwise proven (all-skip inter blocks decode clean, and the
    /// same coefficient syntax decodes clean on every intra key-frame gate).
    #[test]
    fn a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean: no ffmpeg"
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let picture = Picture::grey(width, height);
        let key = encode_key_frame(&picture, 100, 0.5).unwrap();

        let (seq, _) = key_frame_headers(width, height, 100).unwrap();
        let (_, inter_header) = inter_frame_headers(width, height, 100, 1, 0).unwrap();

        let residual_block = BlockCoeffs {
            luma: vec![Coeff {
                row: 0,
                col: 0,
                level: 4,
            }],
            u: Vec::new(),
            v: Vec::new(),
            mode: 0,
            skip: false,
            eight: None,
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv: (0, 0),
            }),
        };
        let skipped_block = BlockCoeffs {
            skip: true,
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv: (0, 0),
            }),
            ..BlockCoeffs::default()
        };
        let blocks = vec![
            Quadrant::Whole(residual_block),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block),
        ];
        let tile =
            sb_coeff_inter_frame_tile(inter_header.mi_cols, inter_header.mi_rows, 100, &blocks)
                .unwrap();

        // `key.stream` is already a temporal delimiter, the sequence header
        // and the key frame's own OBU (`encode_key_frame` built it from the
        // same `key_frame_headers(width, height, 100)` this test calls), so
        // the inter frame's OBU is appended straight onto it rather than
        // re-deriving the key frame's tile bytes.
        let mut stream = key.stream.clone();
        stream.extend_from_slice(&temporal_delimiter());
        stream.extend_from_slice(&frame_obu(&seq, &inter_header, &tile).unwrap());

        ffmpeg_decode_sequence(&stream, width, height, 2);
    }

    /// `ffprobe`'s reported `width,height` for one OBU stream -- the coded
    /// frame size an AV1 decoder allocates, not necessarily the render size
    /// (see [`a_frame_round_trips_at_its_own_size`]).
    fn ffprobe_size(stream: &[u8]) -> (u32, u32) {
        // Distinct per call, not per process: the test binary runs callers on
        // parallel threads, and a shared name lets one test's cleanup delete
        // the stream another test's ffprobe is still reading.
        static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ec-av1-probe-{}-{}.obu",
            std::process::id(),
            PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.trim().split(',');
        let width: u32 = fields.next().expect("ffprobe width").parse().unwrap();
        let height: u32 = fields.next().expect("ffprobe height").parse().unwrap();
        (width, height)
    }

    /// A key frame at an arbitrary even size round-trips through ffmpeg over
    /// its own render rectangle: [`Encoded::reconstruction`] is exactly the
    /// picture's own size, and what ffmpeg decodes over that same top-left
    /// region equals it sample for sample.
    ///
    /// This does not check that `ffprobe` reports the picture's own size,
    /// because it does not: `ffprobe`/ffmpeg's AV1 decoder reports the coded
    /// (block-padded) frame size, the same one libaom itself would code for
    /// a non-block-aligned picture. `render_width`/`render_height` (which
    /// this crate sets correctly, spec 5.9.6) is a display hint no AV1
    /// decoder is required to crop pixels by, and ffmpeg's does not --
    /// checked empirically against ffprobe here and against a real libaom
    /// encode separately. So the coded size, not the render size, is what
    /// `ffprobe` is asserted to report below.
    #[test]
    fn a_frame_round_trips_at_its_own_size() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_frame_round_trips_at_its_own_size: no ffmpeg");
            return;
        }
        // 854x480 is covered separately by
        // `an_854x480_picture_round_trips_through_its_padding`, which its own
        // doc comment explains was worth keeping as its own test.
        for &(width, height) in &[
            (1920usize, 1080usize),
            (640, 352),
            (1280, 720),
            (64, 56),
            (854, 480),
            (216, 96),
            (192, 120),
        ] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            assert_eq!(
                (encoded.reconstruction.width, encoded.reconstruction.height),
                (width, height),
                "{width}x{height}: reconstruction is cropped to the picture's own size"
            );
            assert_eq!(
                ffprobe_size(&encoded.stream),
                (width as u32, height as u32),
                "{width}x{height}: ffprobe reports the true (display) size, not the padded one"
            );
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(
                decoded.y, encoded.reconstruction.y,
                "{width}x{height}: luma"
            );
            assert_eq!(decoded.u, encoded.reconstruction.u, "{width}x{height}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "{width}x{height}: V");
        }
    }

    /// 854x480 (padded to 864x480, an edge-replication padded key frame) used
    /// to diverge from ffmpeg's decode at luma (row 161, col 369): a debug
    /// `aomdec` trace of `has_top_right`/`has_bottom_left` at that exact
    /// block (a 16x16 at mi (40, 88)) showed libaom indexing its pinned
    /// `has_bl_16x16` table at bit 18 (`row * 8 + col`, row=2 col=2), while
    /// `Reach::bottom_left` indexed the same table at bit 10 (`row * 4 +
    /// col`) -- different bits of an 8-byte table, one true, one false. The
    /// stride libaom indexes by is fixed at its compile-time maximum
    /// superblock (128px, `MAX_MIB_SIZE_LOG2` = 5), not the actual superblock
    /// size a stream uses, so the right stride for a 16x16 or 32x32 block is
    /// `128 / side`, not `SUPERBLOCK / side` (64-relative) -- `Reach::of`'s
    /// row/col position within the (correctly 64-relative) superblock stayed
    /// right, only the table's own row stride was wrong. Fixed in
    /// `Reach::table_stride`; not an arithmetic-coder desync (an
    /// EC_RNG-per-symbol trace against the same debug decoder found the
    /// bitstream byte-for-byte identical to libaom's across all 51188
    /// symbols of this frame before this bug was found).
    ///
    /// Since the frame header started writing the true (unpadded) display
    /// size into `frame_width`/`frame_height` rather than the padded coded
    /// size, ffmpeg crops to 854x480 on its own -- decoding at the padded
    /// 864x480 and cropping ourselves now reads past what ffmpeg actually
    /// emits (`av1_common_int.h`'s render/upscale path, mirrored by
    /// `av1_frame_size` on the decode side, crops the loop-filtered picture to
    /// the header's own size before output).
    #[test]
    fn an_854x480_picture_round_trips_through_its_padding() {
        if !have_ffmpeg() {
            return;
        }
        let (width, height) = (854usize, 480usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "854x480: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "854x480: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "854x480: V");
    }

    /// `Reach::top_right`/`bottom_left` against a from-scratch transcription
    /// of libaom's `has_top_right`/`has_bottom_left` (`av1/common/
    /// reconintra.c`), for every 16x16 and 32x32 block position across three
    /// superblocks square -- interior and every superblock edge (top row,
    /// left column, right column, bottom row) both sizes reach. Written to
    /// catch the class the `table_stride` bug was: an index stride silently
    /// wrong for one block size while looking plausible for the other.
    #[test]
    fn reach_matches_libaom_has_top_right_and_has_bottom_left() {
        // Transcribed from has_top_right/has_bottom_left's row_off==0,
        // col_off==0 (whole-transform) path, with MAX_MIB_SIZE_LOG2 = 5 (a
        // 128px reference grid) pinned as libaom pins it, independent of the
        // 64px superblock this crate actually codes.
        fn libaom_top_right(side: usize, x: usize, y: usize, width: usize, height: usize) -> bool {
            if y == 0 || x + side >= width {
                return false;
            }
            let (row, col, per_side) = (
                (y % SUPERBLOCK) / side,
                (x % SUPERBLOCK) / side,
                SUPERBLOCK / side,
            );
            if row == 0 {
                return true;
            }
            if col + 1 == per_side {
                return false;
            }
            let stride = 128 / side;
            let index = row * stride + col;
            let table = if side == BLOCK {
                [95u8, 87].to_vec()
            } else {
                vec![255, 85, 119, 85, 127, 85, 119, 85]
            };
            (table[index / 8] >> (index % 8)) & 1 != 0
        }

        fn libaom_bottom_left(side: usize, x: usize, y: usize, height: usize) -> bool {
            if x == 0 || y + side >= height {
                return false;
            }
            let (row, col, per_side) = (
                (y % SUPERBLOCK) / side,
                (x % SUPERBLOCK) / side,
                SUPERBLOCK / side,
            );
            if col == 0 {
                return row * side + side < SUPERBLOCK;
            }
            if row + 1 == per_side {
                return false;
            }
            let stride = 128 / side;
            let index = row * stride + col;
            let table = if side == BLOCK {
                [4u8, 4].to_vec()
            } else {
                vec![84, 16, 84, 0, 84, 16, 84, 0]
            };
            (table[index / 8] >> (index % 8)) & 1 != 0
        }

        let (width, height) = (SUPERBLOCK * 3, SUPERBLOCK * 3);
        for side in [16usize, BLOCK] {
            for y in (0..height).step_by(side) {
                for x in (0..width).step_by(side) {
                    let reach = Reach::of(side, x, y, width, height);
                    assert_eq!(
                        reach.above_right,
                        libaom_top_right(side, x, y, width, height),
                        "side={side} x={x} y={y}: above_right"
                    );
                    assert_eq!(
                        reach.below_left,
                        libaom_bottom_left(side, x, y, height),
                        "side={side} x={x} y={y}: below_left"
                    );
                }
            }
        }
    }

    /// An odd width or height is refused by name, not by however the padder
    /// or the block coder would happen to fail on it.
    #[test]
    fn odd_dimensions_are_refused() {
        for &(width, height) in &[(1921usize, 1080usize), (1920, 1081), (63, 63)] {
            let picture = Picture::grey(width, height);
            let err = encode_key_frame(&picture, 100, 0.5)
                .expect_err(&format!("{width}x{height} is odd and must be refused"));
            assert!(
                err.to_string().contains("even"),
                "{width}x{height}: error was {err}"
            );
        }
    }

    /// A sequence at a size that is not a multiple of the block grid decodes
    /// to the right frame count and the right size for every frame,
    /// including the inter frames whose reference is the previous frame's
    /// own (padded) reconstruction.
    #[test]
    fn sequence_round_trips_at_a_non_multiple_size() {
        if !have_ffmpeg() {
            eprintln!("SKIP sequence_round_trips_at_a_non_multiple_size: no ffmpeg");
            return;
        }
        let (width, height) = (160usize, 96usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), 3);
        for (i, frame) in encoded.frames.iter().enumerate() {
            assert_eq!(
                (frame.reconstruction.width, frame.reconstruction.height),
                (width, height),
                "frame {i}: reconstruction size"
            );
        }
        // See `a_frame_round_trips_at_its_own_size`: `ffprobe` reports the
        // true (display) size a sequence's frames share, not the padded one.
        assert_eq!(
            ffprobe_size(&encoded.stream),
            (width as u32, height as u32),
            "sequence: ffprobe reports the true (display) size"
        );
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        assert_eq!(decoded.len(), 3, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
        }
    }

    /// 1280x720 -- the export size a real edith timeline hits -- is exactly
    /// the "half straddle" class: 720 mod 32 == 16, so the true frame edge
    /// falls at exactly half of the last row of 32x32 blocks (`has_half`'s
    /// `pos + side_mi / 2 < bound` boundary), which is the case
    /// `sb_coeff_inter_frame_tile`'s `Quadrant::Split` now codes rather than
    /// refusing. A key frame alone already covered this size; this proves the
    /// inter tile writer's parity with it across a real multi-frame export.
    #[test]
    fn a_sequence_round_trips_at_the_exactly_half_straddle_size() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_sequence_round_trips_at_the_exactly_half_straddle_size: no ffmpeg");
            return;
        }
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), 3);
        for (i, frame) in encoded.frames.iter().enumerate() {
            assert_eq!(
                (frame.reconstruction.width, frame.reconstruction.height),
                (width, height),
                "frame {i}: reconstruction size"
            );
        }
        assert_eq!(
            ffprobe_size(&encoded.stream),
            (width as u32, height as u32),
            "ffprobe reports the true (display) 1280x720 size"
        );
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        assert_eq!(decoded.len(), 3, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
        }
    }

    /// 640x360 is the class the charter names: 360 mod 32 == 8, so a 16x16
    /// leaf's own half (`has_half` at the SUB level) itself straddles the
    /// true edge on one axis only. lane-av1-rect r7 wired the key frame
    /// writer's 8x8-leaf path (`crate::tile::write_leaf8`) live for exactly
    /// this case; lane-av1inter8 extends the same leaf-8 path to inter
    /// frames (`code_square_inter` at `side == 8`, `sb_coeff_inter_frame_tile`'s
    /// straddling-16x16 branch), so an inter frame no longer refuses this
    /// size either. This only proves the encoder does not error out -- see
    /// `a_640x360_half_straddle_sequence_round_trips_through_ffmpeg` for the
    /// decoder-clean proof (a 3-frame sequence, so the inter path is
    /// actually exercised).
    #[test]
    fn key_frame_and_inter_frame_both_encode_at_half_straddle_size() {
        let (width, height) = (640usize, 360usize);
        let picture = Picture::grey(width, height);
        encode_key_frame(&picture, 100, 0.5).expect("key frame now codes the mod-32==8 straddle");

        // `encode_sequence`'s second frame is the inter path: same size, now
        // wired the same way.
        let pictures = vec![picture.clone(), picture];
        encode_sequence(&pictures, 100, 0.5)
            .expect("inter frame now codes the mod-32==8 straddle too");
    }

    /// The r15 fix at production size: three 640x360 key frames (inter is
    /// still refused at this size, per the test above, so this stands in for
    /// the charter's "3-frame sequence" using three independent key frames of
    /// shifted content), each proven decoder-clean through ffmpeg rather than
    /// only encoder-clean.
    #[test]
    fn a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames: \
                 no ffmpeg"
            );
            return;
        }
        let (width, height) = (640usize, 360usize);
        for shift in [0i64, 7, 19] {
            let picture = panned_test_card(width, height, shift);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(decoded.y, encoded.reconstruction.y, "shift {shift}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "shift {shift}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "shift {shift}: V");
        }
    }

    /// The 640x360 straddle class, but through the inter path an actual
    /// 3-frame sequence exercises (lane-av1inter8's own ladder rung (b)):
    /// proves the leaf-8 inter stream this round wires up is not just
    /// encoder-clean (the test above) but decoder-clean against ffmpeg too.
    #[test]
    fn a_640x360_half_straddle_sequence_round_trips_through_ffmpeg() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_640x360_half_straddle_sequence_round_trips_through_ffmpeg: no ffmpeg"
            );
            return;
        }
        let (width, height) = (640usize, 360usize);
        let pictures: Vec<Picture> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), 4);
        if let Ok(path) = std::env::var("EC_AV1_DUMP") {
            std::fs::write(&path, &encoded.stream).expect("dump the raw stream");
        }
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 4);
        assert_eq!(decoded.len(), 4, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
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
            y: out.stdout[..luma].iter().map(|&v| u16::from(v)).collect(),
            u: out.stdout[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
            v: out.stdout[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
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
                        let encoded = encode_key_frame_inner(
                            &picture,
                            q,
                            0.5,
                            &KEY_FRAME_MODES,
                            split,
                            (picture.width, picture.height),
                            unspecified_color_config(),
                        )
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
            let blocks = encode_key_frame_inner(
                &picture,
                90,
                0.5,
                &KEY_FRAME_MODES,
                true,
                (picture.width, picture.height),
                unspecified_color_config(),
            )
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
    fn first_difference(ours: &[u16], theirs: &[u16], width: usize) -> Option<String> {
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

        // Bytes must not jump up across a q-context boundary: a wrong CDF
        // table for the far side would show up as a rate discontinuity here,
        // even though a coarser quantizer always codes no more than a finer
        // one on the same picture.
        for &(lo, hi) in &[(20u8, 21u8), (60, 61), (120, 121)] {
            let lo_bytes = encode_key_frame(&picture, lo, 0.5).unwrap().stream.len();
            let hi_bytes = encode_key_frame(&picture, hi, 0.5).unwrap().stream.len();
            assert!(
                hi_bytes <= lo_bytes,
                "q {lo}->{hi} crosses a context boundary: {lo_bytes} -> {hi_bytes} bytes"
            );
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
    /// panicking somewhere below. Most sizes off the 32x32 block grid encode
    /// fine now (see `a_frame_round_trips_at_its_own_size`) -- the true frame
    /// edge lands past the halfway point of whichever block it falls in, so
    /// `PARTITION_NONE` or a single gathered split flag still says everything
    /// the spec needs. lane-av1-rect r7 wires the 8x8-leaf path live, so a
    /// 16x16 leaf straddling on exactly one axis (e.g. 40x32, cols mod 32 ==
    /// 8) now encodes too (see `a_frame_round_trips_at_its_own_size`'s
    /// sibling below). Only a leaf whose true edge cuts *both* axes (e.g.
    /// 40x40, both dims mod 32 == 8) needs the rectangular transform neither
    /// writer has, so that class is still refused, by name rather than by
    /// corrupting the stream.
    #[test]
    fn a_picture_off_the_block_grid_is_refused() {
        let msg = encode_key_frame(&Picture::grey(40, 40), 100, 0.5)
            .expect_err("40x40 straddles both axes of a 16x16 leaf, needs a rectangular transform")
            .to_string();
        assert!(msg.contains("rectangular"), "refusal: {msg}");

        let mut short = Picture::grey(64, 64);
        short.u.truncate(10);
        assert!(encode_key_frame(&short, 100, 0.5).is_err());
    }

    /// 40x32: cols mod 32 == 8, so the true edge cuts a 16x16 leaf's own
    /// column half only (`has_half` false on cols, true on rows) -- exactly
    /// the single-axis straddle lane-av1-rect r7 wires through
    /// `crate::tile::write_leaf8`'s 8x8 leaves. Not yet decoder-clean:
    /// dav1d rejects the stream ("Invalid data found"). r7's falsification
    /// candidate (`partition_w8`'s ctx reading the enclosing 16x16 slot) is
    /// FALSIFIED by r8's rng trace against a debug `aomdec`: libaom's own
    /// `partition_plane_context` masks at `bsl = 0` for `BLOCK_8X8`, and every
    /// `partition_context_lookup` entry this writer ever produces (no 4x4
    /// splits exist) has bit 0 clear, so the ctx is provably 0 for every
    /// leaf regardless of neighbour state -- our writer already agreed with
    /// the decoder here (`ctx=0` both sides). r8 still ported the mi-precise
    /// tracking (`Neighbours::{above,left}_side_mi`, `partition_ctx_mi`) as a
    /// spec-accurate fix in its own right (it matters once 4x4 exists), and
    /// it does not regress the gate. r11 tell-for-tell traced two further,
    /// real bugs and fixed both: (1) `write_leaf8`'s intra-mode context read
    /// the *enclosing* 16x16 slot's stale `above_mode`/`left_mode` for both
    /// leaves, when the second leaf's true above (or left) neighbour is the
    /// first leaf itself -- fixed by threading the first leaf's mode into the
    /// second leaf's context, mi-precise, like `record_mi` already does for
    /// coefficients; (2) `TxbSet::Luma8`'s transform-type table was
    /// `INTRA_TX_TYPE_SET1_8` (`TX_SET_INTRA_1`, 7-way, correct only when
    /// `reduced_tx_set` is false), but this crate's key frames set
    /// `reduced_tx_set: true`, which per `get_tx_set` (spec 5.11.48) puts
    /// *every* intra size up to 16x16 -- not only 16x16 -- on `TX_SET_INTRA_2`
    /// (5-way); the wrong cardinality read enough of the stream as `TX_TYPE`
    /// symbol bits to desync everything after it. Fixed with a new
    /// `INTRA_TX_TYPE_SET2_8` (libaom's flat default for that exact
    /// eset/size slot). Both fixes are provably correct against libaom
    /// source and each closed part of the gap (leaf0's `eob` went from a
    /// desynced 36 to a near-miss 3, writer's real eob is 7), but a residual
    /// divergence remained in the `EOB_PT` symbol's decoded value despite an
    /// aligned bit cost. r12 traced that residual with a fresh dual-trace
    /// (do not reuse r11's committed logs, which predate its own fixes) and
    /// found two more real bugs, both now fixed: (3) `write_leaf8` hardcoded
    /// `cfl: false` for `write_intra_mode`'s chroma-mode CDF choice, but
    /// `is_cfl_allowed` (spec 5.11.5) is true for any block `<= 32x32` --
    /// every other `write_block` caller at 16x16 and up already passes
    /// `true`; the narrower `uv_mode_no_cfl` alphabet is one symbol short of
    /// `uv_mode_cfl`, so even an identical `DC_PRED` decision consumed a
    /// different amount of arithmetic range and derailed everything after
    /// it. (4) `record()`'s `above_mode`/`left_mode` write is a no-op at an
    /// 8x8 leaf's own side (`side / SUB == 0` for `SUB` = 16), and
    /// `write_leaf8` called only `record_mi` (the coefficient-context half),
    /// never updating those coarse arrays itself -- so a *second* straddling
    /// 16x16 quadrant stacked above or left of the first (never covered by
    /// r11's same-call `prev_leaf` override, which only threads state
    /// between the two leaves of one shared quadrant) read whatever stale
    /// mode sat in that slot before either leaf ran. Fixed by writing the
    /// leaf's own mode into `above_mode[c]`/`left_mode[r]` directly. Fixing
    /// (3) alone shrank the tell drift after leaf(0,8) from a decoder
    /// `eob_pt=3` (wrong) vs the writer's real `eob_pt=4` down to a `tell`
    /// offset of a single unit; fixing (3)+(4) together still leaves a
    /// residual divergence one leaf further in -- the decoder now reads
    /// leaf(2,8)'s own luma coefficient block as `txs_ctx=1` (a 32-sample
    /// transform) where the writer intends `TX_8X8` (`txs_ctx=2`), meaning
    /// something *before* this leaf's own `EC_TXBSKIP`/`EC_TXTYPE` still
    /// desyncs the stream by exactly the width of one earlier symbol -- not
    /// yet isolated to a single named symbol. r15 found the last bug: the
    /// header fix from r14 explained the rng skew fully, and the remaining
    /// desync (still past `mi_row=4`) was `write_leaf8`'s (4) fix itself --
    /// its `above_mode[c]`/`left_mode[r]` forced write ran *inside* the leaf
    /// loop, once per leaf, so the first leaf's write clobbered the true
    /// external neighbour mode the second leaf of the *same* straddling
    /// quadrant still needed for its non-adjacency axis (a column-straddle
    /// case: two leaves share one `outer_at` column index, so leaf 1's
    /// write to `left_mode[r]` overwrote the real western neighbour before
    /// leaf 2 read it). Fixed by moving the write out of `write_leaf8` and
    /// doing it once, after the whole leaf loop, from the last (bottom/
    /// right-most) leaf's mode. The stream now round-trips through ffmpeg.
    #[test]
    fn a_single_axis_straddle_round_trips_through_ffmpeg() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_single_axis_straddle_round_trips_through_ffmpeg: no ffmpeg");
            return;
        }
        let (width, height) = (40usize, 32usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        assert_eq!(
            (encoded.reconstruction.width, encoded.reconstruction.height),
            (width, height)
        );
        if let Ok(path) = std::env::var("EC_AV1_DUMP") {
            std::fs::write(&path, &encoded.stream).expect("dump the raw stream");
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "V");
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

    /// [`test_card`], panned `shift` samples to the right (wrapping), so a
    /// sequence of these is a translation a motion search can actually find
    /// — the content [`test_card`] itself draws, not a fresh pattern, so an
    /// inter frame's rate against the key frame's is measuring the same
    /// picture moving, not two different pictures.
    fn panned_test_card(width: usize, height: usize, shift: i64) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
                let gradient = sx * 200.0 / width as f64;
                let ripple = 30.0
                    * (sx * std::f64::consts::PI / 23.0).sin()
                    * (y as f64 * std::f64::consts::PI / 37.0).cos();
                let edge = if sx > (width * 3 / 7) as f64 && y > height / 3 {
                    40.0
                } else {
                    0.0
                };
                picture.y[y * width + x] =
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
            }
        }
        picture
    }

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
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
        let frame_bytes = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Picture {
                    width,
                    height,
                    y: out.stdout[base..base + luma].iter().map(|&v| u16::from(v)).collect(),
                    u: out.stdout[base + luma..base + luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes].iter().map(|&v| u16::from(v)).collect(),
                }
            })
            .collect()
    }

    /// The same claim [`ffmpeg_decodes_exactly_what_the_encoder_reconstructed`]
    /// makes for a key frame, extended down the reference chain: each inter
    /// frame's reconstruction predicts from the previous frame's own decoded
    /// reconstruction, so a single sample of drift anywhere would propagate
    /// into every frame after it -- which is why every frame is checked, not
    /// just the last one, and why this is an equality and not a tolerance.
    #[test]
    fn every_frame_of_a_sequence_decodes_to_what_the_encoder_reconstructed() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP every_frame_of_a_sequence_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        let (width, height) = (128usize, 64usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), 5);

        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 5);
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: V");
        }

        eprintln!("frame  bytes  luma PSNR (dB)  inter share");
        for (i, frame) in encoded.frames.iter().enumerate() {
            eprintln!(
                "{i:5}  {:5}  {:14.2}  {:11.2}",
                frame.stream.len(),
                psnr(&frame.reconstruction.y, &pictures[i].y),
                frame.inter_block_share
            );
        }
    }

    /// Low motion has to actually buy something: at least one inter frame of
    /// a panning sequence has to cost fewer bytes than the key frame that
    /// starts it, and the inter-block share it prints has to be non-zero --
    /// a search that never picks an inter mode would still pass a rate gate
    /// on a picture with no motion in it (the class this repo calls
    /// gate-blind-to-feature), so the share is printed even though it is not
    /// asserted on beyond being reachable at all.
    #[test]
    fn low_motion_makes_an_inter_frame_smaller_than_the_key_frame() {
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();

        let key_bytes = encoded.frames[0].stream.len();
        eprintln!("frame  bytes  inter share");
        for (i, frame) in encoded.frames.iter().enumerate() {
            eprintln!(
                "{i:5}  {:5}  {:11.2}",
                frame.stream.len(),
                frame.inter_block_share
            );
        }
        assert!(
            encoded.frames[1..]
                .iter()
                .any(|f| f.stream.len() < key_bytes),
            "no inter frame of a panning sequence beat the key frame's {key_bytes} bytes"
        );
        assert!(
            encoded.frames[1..]
                .iter()
                .any(|f| f.inter_block_share > 0.0),
            "no inter frame coded a single inter block -- the search never fired"
        );
    }

    /// A manual perf probe, not a gate: prints where a key frame's and an
    /// inter frame's time goes, coarse stage by coarse stage, so a perf lane
    /// measures before it optimizes rather than guessing. Run with
    /// `cargo test -p ec-av1 --release stage_timing_breakdown -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn stage_timing_breakdown() {
        use std::time::Instant;

        for &(width, height) in &[(1920usize, 1080usize), (3840, 2160)] {
            let picture = test_card(width, height);

            let t = Instant::now();
            let dc_only = encode_key_frame_with_modes(&picture, 100, 0.5, &[DC_PRED]).unwrap();
            let dc_only_t = t.elapsed();

            let t = Instant::now();
            let non_directional =
                encode_key_frame_with_modes(&picture, 100, 0.5, &NON_DIRECTIONAL).unwrap();
            let non_directional_t = t.elapsed();

            let t = Instant::now();
            let all_modes = encode_key_frame(&picture, 100, 0.5).unwrap();
            let all_modes_t = t.elapsed();

            eprintln!(
                "\n=== key frame {width}x{height} ===\n\
                 1 mode  (DC only):        {dc_only_t:>9.2?}  ({} bytes)\n\
                 7 modes (non-directional): {non_directional_t:>9.2?}  ({} bytes)\n\
                 13 modes (default search):  {all_modes_t:>9.2?}  ({} bytes)",
                dc_only.stream.len(),
                non_directional.stream.len(),
                all_modes.stream.len(),
            );

            // One inter frame predicting off the key frame's own
            // reconstruction, the (256, 256)-padded surface encode_sequence
            // itself would hand encode_inter_frame.
            let padded_ref = all_modes.reconstruction.padded_to(SUPERBLOCK);
            let padded_pic = picture.padded_to(SUPERBLOCK);
            let t = Instant::now();
            let inter =
                encode_inter_frame(&padded_pic, &padded_ref, 100, 0.5, 1, (width, height)).unwrap();
            let inter_t = t.elapsed();
            eprintln!(
                "inter frame vs its own key frame: {inter_t:>9.2?}  ({} bytes, inter share \
                 {:.2})",
                inter.stream.len(),
                inter.inter_block_share
            );
        }

        // Isolated per-call cost of the trial pipeline's own stages, at both
        // transform sizes the search runs, averaged over enough calls to
        // rise above `Instant::now`'s own noise.
        for side in [16usize, 32] {
            const N: u32 = 20_000;
            let residual: Vec<i32> = (0..side * side)
                .map(|i| ((i * 37 % 61) as i32) - 30)
                .collect();

            let t = Instant::now();
            let mut levels = Vec::new();
            for _ in 0..N {
                levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            }
            let forward_t = t.elapsed() / N;

            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(dequant_and_inverse(&levels, side, 8, 100));
            }
            let inverse_t = t.elapsed() / N;

            let set = if side == 32 {
                crate::cdf_state::TxbSet::Luma32
            } else {
                crate::cdf_state::TxbSet::Luma16
            };
            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(crate::tile::coeff_bits(&levels, set));
            }
            let entropy_t = t.elapsed() / N;

            let above = vec![128u8; side * 2];
            let left = vec![128u8; side * 2];
            let mut prediction = vec![0u8; side * side];
            let t = Instant::now();
            for _ in 0..N {
                intra_predict_u8(
                    D45_PRED,
                    0,
                    Some(&above),
                    Some(&left),
                    Some(128),
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                );
            }
            let predict_t = t.elapsed() / N;

            eprintln!(
                "\n=== per-call cost at {side}x{side}, averaged over {N} calls ===\n\
                 predict (1 directional mode): {predict_t:>9.2?}\n\
                 forward_and_quantize:         {forward_t:>9.2?}\n\
                 dequant_and_inverse:          {inverse_t:>9.2?}\n\
                 coeff_bits:                   {entropy_t:>9.2?}"
            );
        }
    }

    /// Breaks one inter frame's own cost into the four [`STAGE_NS`] buckets
    /// (motion search, [`crate::mc::predict`], transform+quantize,
    /// `coeff_bits`), plus what is left over once those are subtracted --
    /// the thirteen-mode intra-candidate loop `search_inter_block` still
    /// runs on every inter block (its own transform/quant/entropy calls are
    /// already inside buckets 2/3, so "left over" here is everything NOT
    /// timed: `crate::intra::predict` for those 13 modes, RD bookkeeping,
    /// symbol-cost lookups outside `coeff_bits`).
    ///
    /// `cargo test -p ec-av1 --release stage_timing_breakdown_inter -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn stage_timing_breakdown_inter() {
        use std::time::Instant;

        let (width, height) = (1280usize, 720);
        let picture = test_card(width, height);
        let key = encode_key_frame(&picture, 100, 0.5).unwrap();
        let padded_ref = key.reconstruction.padded_to(SUPERBLOCK);
        let padded_pic = picture.padded_to(SUPERBLOCK);

        stage_reset();
        let t = Instant::now();
        let inter =
            encode_inter_frame(&padded_pic, &padded_ref, 100, 0.5, 1, (width, height)).unwrap();
        let total_t = t.elapsed();
        let [motion_search, interpolation, transform_quant, entropy] = stage_read();
        let accounted = motion_search + transform_quant + entropy;
        // motion_search already contains interpolation's own time (every
        // candidate the search costs calls crate::mc::predict), so it is not
        // added again here -- interpolation is reported alongside as "of
        // which interpolation", not as a fifth additive bucket.
        let other = total_t.saturating_sub(accounted);

        eprintln!(
            "\n=== inter frame {width}x{height}, one frame ===\n\
             total:                          {total_t:>9.2?}  ({} bytes)\n\
             motion search (NEWMV):          {motion_search:>9.2?}  (of which interpolation \
             {interpolation:>9.2?})\n\
             transform + quantize:           {transform_quant:>9.2?}\n\
             coeff_bits (entropy pricing):   {entropy:>9.2?}\n\
             everything else (13-mode intra  {other:>9.2?}\n\
             candidate loop's own predict/bookkeeping, NEARESTMV's own \
             predict, RD glue):",
            inter.stream.len(),
        );
    }

    /// The quality gate [`Search::top_k`]'s lever lives or dies by: three real
    /// frames (a dark/flat one, a detailed one, a normal film one) at two
    /// `base_q_idx` values, encoded with the mode search unpruned (`top_k =
    /// None`, the baseline this same build produces without
    /// `EC_AV1_PRUNE_K` set) and with it pruned to each of a few `K`, bytes
    /// and luma PSNR compared side by side. Not a gate itself -- the numbers
    /// go in the lane report, which is where "ship as default" is decided --
    /// so it stays `#[ignore]`, run by hand with real clips on this machine.
    /// `cargo test -p ec-av1 --release prune_k_quality_sweep -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs real clips and ffmpeg; prints numbers for the lane report to judge"]
    fn prune_k_quality_sweep() {
        if !have_ffmpeg() {
            eprintln!("SKIP prune_k_quality_sweep: no ffmpeg");
            return;
        }
        let (width, height) = (640usize, 352usize);
        let frames: &[(&str, &str, &str)] = &[
            // (label, clip, seek) -- a near-black scene, a busy one, and an
            // ordinary film frame, each from a different real file.
            (
                "dark/flat",
                "/home/tahinli/Videos/he_is_not_the_only_one.mp4",
                "3",
            ),
            (
                "detailed",
                "/home/tahinli/Downloads/The.Hunger.Games.The.Ballad.Of.Songbirds.And.Snakes.2023.Bluray.2160p.AV1.HDR10.OPUS.7.1-UH.mkv",
                "1200",
            ),
            (
                "film",
                "/home/tahinli/Videos/Films/Troy.Director's.Cut.2004.Bluray.1080P.AV1.OPUS.5.1-DECK.mkv",
                "600",
            ),
        ];
        for &(label, clip, skip) in frames {
            if !std::path::Path::new(clip).exists() {
                eprintln!("SKIP {label}: {clip} not present on this machine");
                continue;
            }
            let picture = clip_frame(clip, skip, width, height);
            for &q in &[60u8, 150] {
                // SAFETY (not literally unsafe, just process-global): this is
                // a single-threaded #[ignore] probe, run by hand, never
                // alongside other tests that read Search::top_k.
                set_test_top_k_override(None);
                let baseline = encode_key_frame(&picture, q, 0.5).unwrap();
                let baseline_psnr = psnr(&baseline.reconstruction.y, &picture.y);
                eprint!(
                    "{label:9} q={q:3}  baseline {:6} bytes  {:6.2} dB",
                    baseline.stream.len(),
                    baseline_psnr
                );
                for &k in &[3usize, 4, 6] {
                    set_test_top_k_override(Some(k));
                    let pruned = encode_key_frame(&picture, q, 0.5).unwrap();
                    let pruned_psnr = psnr(&pruned.reconstruction.y, &picture.y);
                    eprint!(
                        "   K={k} {:6} bytes  {:6.2} dB ({:+.3} dB)",
                        pruned.stream.len(),
                        pruned_psnr,
                        pruned_psnr - baseline_psnr
                    );
                }
                eprintln!();
            }
        }
        set_test_top_k_override(None);
    }

    /// Two consecutive decoded frames from one clip, for an inter-frame
    /// sweep -- [`clip_frame`] alone only ever gives one.
    fn clip_frame_pair(clip: &str, skip: &str, width: usize, height: usize) -> [Picture; 2] {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip, "-frames:v", "2"])
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
        let frame_len = luma + 2 * chroma;
        assert_eq!(out.stdout.len(), frame_len * 2, "expected two 4:2:0 frames");
        std::array::from_fn(|i| {
            let bytes = &out.stdout[i * frame_len..][..frame_len];
            Picture {
                width,
                height,
                y: bytes[..luma].iter().map(|&v| u16::from(v)).collect(),
                u: bytes[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                v: bytes[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
            }
        })
    }

    /// The quality gate [`INTER_PRUNE_TOP_K`] lives or dies by: the same
    /// three real clips as [`prune_k_quality_sweep`], but coding a key frame
    /// then one inter frame off it (`search_inter_block`'s own intra
    /// candidates are what [`prune_by_sad`] prunes here, not the key-frame
    /// loop), at 1280x704 -- a whole number of 64x64 superblocks, so no
    /// padding step complicates the comparison. Prints bytes and luma PSNR
    /// for the inter frame alone, unpruned vs each `K`.
    /// `cargo test -p ec-av1 --release prune_k_quality_sweep_inter -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs real clips and ffmpeg; prints numbers for the lane report to judge"]
    fn prune_k_quality_sweep_inter() {
        if !have_ffmpeg() {
            eprintln!("SKIP prune_k_quality_sweep_inter: no ffmpeg");
            return;
        }
        let (width, height) = (1280usize, 704usize);
        let frames: &[(&str, &str, &str)] = &[
            (
                "dark/flat",
                "/home/tahinli/Videos/he_is_not_the_only_one.mp4",
                "3",
            ),
            (
                "detailed",
                "/home/tahinli/Downloads/The.Hunger.Games.The.Ballad.Of.Songbirds.And.Snakes.2023.Bluray.2160p.AV1.HDR10.OPUS.7.1-UH.mkv",
                "1200",
            ),
            (
                "film",
                "/home/tahinli/Videos/Films/Troy.Director's.Cut.2004.Bluray.1080P.AV1.OPUS.5.1-DECK.mkv",
                "600",
            ),
        ];
        for &(label, clip, skip) in frames {
            if !std::path::Path::new(clip).exists() {
                eprintln!("SKIP {label}: {clip} not present on this machine");
                continue;
            }
            let [key_source, inter_source] = clip_frame_pair(clip, skip, width, height);
            for &q in &[60u8, 150] {
                // SAFETY (not literally unsafe, just process-global): a
                // single-threaded #[ignore] probe, run by hand, never
                // alongside other tests that read Search::top_k.
                set_test_top_k_override_inter(None);
                let key = encode_key_frame(&key_source, q, 0.5).unwrap();
                let baseline = encode_inter_frame(
                    &inter_source,
                    &key.reconstruction,
                    q,
                    0.5,
                    1,
                    (width, height),
                )
                .unwrap();
                let baseline_psnr = psnr(&baseline.reconstruction.y, &inter_source.y);
                eprint!(
                    "{label:9} q={q:3}  baseline {:6} bytes  {:6.2} dB",
                    baseline.stream.len(),
                    baseline_psnr
                );
                for &k in &[3usize, 4, 6] {
                    set_test_top_k_override_inter(Some(k));
                    let pruned = encode_inter_frame(
                        &inter_source,
                        &key.reconstruction,
                        q,
                        0.5,
                        1,
                        (width, height),
                    )
                    .unwrap();
                    let pruned_psnr = psnr(&pruned.reconstruction.y, &inter_source.y);
                    eprint!(
                        "   K={k} {:6} bytes  {:6.2} dB ({:+.3} dB)",
                        pruned.stream.len(),
                        pruned_psnr,
                        pruned_psnr - baseline_psnr
                    );
                }
                eprintln!();
            }
        }
        set_test_top_k_override_inter(None);
    }

    /// `frames` consecutive pictures out of a real clip, scaled to a whole
    /// number of 32x32 blocks. Generalizes [`clip_frame`]/[`clip_frame_pair`]
    /// to an arbitrary count for the fixture regression gate below.
    fn clip_frames(
        clip: &str,
        skip: &str,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip])
            .args(["-frames:v", &frames.to_string()])
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
        let frame_len = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_len * frames,
            "expected {frames} 4:2:0 frames"
        );
        (0..frames)
            .map(|i| {
                let bytes = &out.stdout[i * frame_len..][..frame_len];
                Picture {
                    width,
                    height,
                    y: bytes[..luma].iter().map(|&v| u16::from(v)).collect(),
                    u: bytes[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                    v: bytes[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
                }
            })
            .collect()
    }

    /// The standing regression gate on real content: 12 frames of a real
    /// clip (`fixtures/video/h264-1080p-23.976-8bit.mp4`, decoded down to
    /// this crate's own input regardless of its own source codec), encoded
    /// as an AV1 GOP and checked against three numeric floors so a
    /// compatibility or quality regression fails the suite instead of
    /// waiting for a downstream repo's gate to catch it (as happened for a
    /// padded-size inter `FrameHeader` dav1d rejected, and a 1280x720 export
    /// refusal, both caught only that way).
    ///
    /// 640x384 is a whole number of 32x32 blocks (no straddle edge case;
    /// that geometry is already covered elsewhere), chosen only so encode
    /// time and gate numbers are not muddied by an edge-block class this
    /// test is not about.
    ///
    /// Measured 2026-08-27 at q=100: stream 69861 bytes, average luma PSNR
    /// 45.41 dB across the 12 decoded frames vs the source. Floor/ceiling
    /// below carry a margin off those measured numbers (PSNR floor =
    /// measured - 0.5 dB; byte ceiling = measured * 1.15).
    #[test]
    fn real_clip_encodes_within_its_quality_and_size_budget() {
        if !have_ffmpeg() {
            eprintln!("SKIP real_clip_encodes_within_its_quality_and_size_budget: no ffmpeg");
            return;
        }
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        if !clip.exists() {
            eprintln!(
                "SKIP real_clip_encodes_within_its_quality_and_size_budget: {} missing",
                clip.display()
            );
            return;
        }
        let (width, height, frame_count) = (640usize, 384usize, 12usize);
        let source = clip_frames(clip.to_str().unwrap(), "0", width, height, frame_count);
        let encoded = encode_sequence(&source, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), frame_count);

        // Gate 1: a real AV1 decoder (dav1d, via ffmpeg) accepts the whole
        // stream and hands back every frame -- a stream a downstream
        // decoder refuses is the exact class of regression this gate
        // exists for.
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frame_count);
        assert_eq!(decoded.len(), frame_count, "dav1d decoded every frame");

        // Gate 2: quality vs the source did not silently collapse.
        let mean_psnr: f64 = decoded
            .iter()
            .zip(&source)
            .map(|(d, s)| psnr(&d.y, &s.y))
            .sum::<f64>()
            / frame_count as f64;
        assert!(
            mean_psnr >= 44.91,
            "mean luma PSNR {mean_psnr:.2} dB fell below the 44.91 dB floor"
        );

        // Gate 3: the stream did not silently bloat.
        assert!(
            encoded.stream.len() <= 80_340,
            "stream grew to {} bytes, over the 80340-byte ceiling",
            encoded.stream.len()
        );
    }

    /// Sibling of `real_clip_encodes_within_its_quality_and_size_budget` at a
    /// straddle size: 640x384 is exact on both axes (32-aligned), so
    /// `Quadrant::Split` never fires there and the 16x16 `NEARESTMV` leaf
    /// path landed in e19bb47 goes unexercised by real, mixed intra/inter
    /// content. 704x400 has 400 mod 32 == 16 -- the same "exactly half
    /// straddle" class as `a_sequence_round_trips_at_the_exactly_half_
    /// straddle_size`'s 1280x720, but that test is a synthetic panned test
    /// card; this one is the real-clip gate, same three floors.
    ///
    /// Measured 2026-08-27 at q=100, 8 frames: stream 54525 bytes, average
    /// luma PSNR 45.48 dB across the 8 decoded frames vs the source.
    /// Floor/ceiling below carry the same margin as the sibling test (PSNR
    /// floor = measured - 0.5 dB; byte ceiling = measured * 1.15).
    #[test]
    fn real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size: \
                 no ffmpeg"
            );
            return;
        }
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        if !clip.exists() {
            eprintln!(
                "SKIP real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size: \
                 {} missing",
                clip.display()
            );
            return;
        }
        let (width, height, frame_count) = (704usize, 400usize, 8usize);
        let source = clip_frames(clip.to_str().unwrap(), "0", width, height, frame_count);
        let encoded = encode_sequence(&source, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), frame_count);

        // Gate 1: a real AV1 decoder (dav1d, via ffmpeg) accepts the whole
        // stream and hands back every frame.
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frame_count);
        assert_eq!(decoded.len(), frame_count, "dav1d decoded every frame");

        // Gate 2: quality vs the source did not silently collapse.
        let mean_psnr: f64 = decoded
            .iter()
            .zip(&source)
            .map(|(d, s)| psnr(&d.y, &s.y))
            .sum::<f64>()
            / frame_count as f64;
        assert!(
            mean_psnr >= 44.98,
            "mean luma PSNR {mean_psnr:.2} dB fell below the 44.98 dB floor"
        );

        // Gate 3: the stream did not silently bloat.
        assert!(
            encoded.stream.len() <= 62_703,
            "stream grew to {} bytes, over the 62703-byte ceiling",
            encoded.stream.len()
        );
    }

    /// The replica-shaped path: 24 frames, 1280x720, one key frame followed
    /// by inter frames -- the same GOP shape [`encode_sequence`] gives the
    /// facade -- timed end to end, so the edith export-time projection is a
    /// real number rather than a guess from the isolated key/inter numbers
    /// above. Run with and without `EC_AV1_PRUNE_K` set to compare.
    /// `cargo test -p ec-av1 --release sequence_bench_sanity -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn sequence_bench_sanity() {
        use std::time::Instant;
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..24)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let t = Instant::now();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let elapsed = t.elapsed();
        let total_bytes: usize = encoded.frames.iter().map(|f| f.stream.len()).sum();
        eprintln!(
            "24 frames @ 1280x720: {elapsed:>9.2?} total, {:>9.2?}/frame, {total_bytes} bytes",
            elapsed / 24
        );
    }

    /// The `base_q_idx` calibration sweep for `lane-av1-ratectl` -- feeds the
    /// `Quality`/`BytesPerFrame` rate-control surface in `encoder.rs`, not a
    /// gate itself. `cargo test -p ec-av1 --release calibration_sweep_base_q_idx
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "a calibration probe, not a gate"]
    fn calibration_sweep_base_q_idx() {
        if !have_ffmpeg() {
            eprintln!("SKIP calibration_sweep_base_q_idx: no ffmpeg");
            return;
        }
        let (width, height, frames) = (640usize, 384usize, 12usize);
        for clip in [
            "h264-1080p-23.976-8bit.mp4", // film-ish, 23.976fps
            "h264-1080p-60-8bit.mp4",     // higher motion, 60fps
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/video")
                .join(clip);
            if !path.exists() {
                eprintln!("SKIP {clip}: fixture missing");
                continue;
            }
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            eprintln!("--- {clip} ---");
            for q in [40u8, 70, 100, 130, 160, 190, 220, 240] {
                let encoded = encode_sequence(&source, q, 0.5).unwrap();
                let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frames);
                let mean_psnr: f64 = decoded
                    .iter()
                    .zip(&source)
                    .map(|(d, s)| psnr(&d.y, &s.y))
                    .sum::<f64>()
                    / frames as f64;
                let bytes_per_px = encoded.stream.len() as f64 / (width * height * frames) as f64;
                eprintln!(
                    "q={q:3}  bytes={:7}  bytes/px={bytes_per_px:.5}  psnr={mean_psnr:.2} dB",
                    encoded.stream.len()
                );
            }
        }
    }
}
