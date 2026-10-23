//! A key-frame picture encoder: a planar 4:2:0 picture in, an AV1 stream out.
//!
//! This is the layer that turns the pieces below it — the intra predictors of
//! [`crate::intra`], the forward transform and quantizer of
//! [`crate::transform`], and the tile writer of [`crate::tile`] — into
//! something that takes a picture. Every block is 32x32 (its chroma 16x16),
//! which is the subset the tile writer codes, and each one picks its luma mode
//! from the seven non-directional ones by rate-distortion; chroma searches
//! its own `uv_mode` over the seven that read no further than the row above
//! and the column to the left, its transform type derived from that mode the
//! way the decoder derives it. Block sizes are what a loop above this would
//! choose between.
//!
//! The encoder carries its own reconstruction, because prediction reads it: a
//! block predicts from the reconstructed samples above and to its left, exactly
//! as the decoder will. That makes the reconstruction a claim about what a
//! decoder produces, and it is gated as one — sample for sample against ffmpeg.

use crate::decode::WithRef;
use ec_av1_syntax::sequence::{ColorConfig, OperatingPoint, SequenceHeader};
use ec_av1_syntax::{
    CdefParams, ChromaSamplePosition, FrameHeader, FrameType, LoopFilterParams,
    LoopRestorationParams, PRIMARY_REF_NONE, QuantizationParams, TileInfo, TxMode,
};
use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::TxbSet;
use crate::frame::frame_obu;
use crate::intra::{
    D67_PRED, DC_PRED, H_PRED, KEY_FRAME_MODES, PAETH_PRED, SMOOTH_H_PRED, SMOOTH_PRED,
    SMOOTH_V_PRED, V_PRED,
};
use crate::mc;
use crate::motion;
use crate::mvstack::{MiGrid, MiInfo, MvStack, NO_REF1, find_mv_stack, mv16};
use crate::obu::temporal_delimiter;
use crate::quant::ac_q;
use crate::sequence::sequence_header_obu;
use crate::tile::{
    BlockCoeffs, Coeff, INTRA_MODE_CTX, InterInfo, InterMode, Quadrant, Superblock, partition_bits,
};
#[cfg(test)]
use crate::transform::forward_and_quantize;
use crate::transform::{
    TxType, dequant_and_inverse_typed_wh, forward_and_quantize_typed,
    forward_and_quantize_typed_scaled,
};

/// The side of the larger of the two luma blocks this encoder codes, in
/// samples.
const BLOCK: usize = 32;

/// The side of the smaller one, which a 32x32 block may be split into four of.
const SUB: usize = 16;

/// The side of a superblock, which is what the partition tree starts from.
pub(crate) const SUPERBLOCK: usize = 64;

// lane-sb128 r1: the superblock size in samples the intra-reach rules
// (`has_top_right`/`has_bottom_left`) are answered against. libaom masks the
// block's mi position with `mi_size_wide[cm->seq_params->sb_size] - 1` and
// tests the "top row"/"rightmost column" of THAT superblock, so a 128x128
// superblock changes every answer inside it (the tables themselves are
// already laid out 128-relative -- see `Reach::table_stride`). [`SUPERBLOCK`]
// stays the encoder's own fixed 64.

/// Sets the superblock size [`Reach`] answers against (64 or 128).
pub(crate) fn set_reach_superblock_px(px: usize, fctx: &crate::decode::FrameCtx) {
    fctx.reach_sb_px.with(|c| c.set(px));
}

fn reach_sb_px(fctx: &crate::decode::FrameCtx) -> usize {
    fctx.reach_sb_px.with(|c| c.get())
}

/// [`reach_sb_px`] in 4x4 mode-info units -- libaom's `sb_mi_size` in
/// `has_top_right`/`has_bottom_left` (`mi_size_high[sb_size]`), which is what
/// [`Reach::of_tu_chroma`] masks the block's mi position with.
pub(crate) fn reach_sb_mi(fctx: &crate::decode::FrameCtx) -> usize {
    reach_sb_px(fctx) / 4
}

/// `scale_chroma_bsize(bsize, ss_x, ss_y)` (libaom `reconintra.c:1649`,
/// called from `av1_predict_intra_block` at `:1823`) in 4x4 mode-info units:
/// the block shape a CHROMA plane's reach is answered against. Only the
/// BLOCK_4X4 / BLOCK_4X8 / BLOCK_8X4 / BLOCK_4X16 / BLOCK_16X4 family is
/// remapped -- every other block size passes through, so at `ss (0,0)` and
/// for any block of 8 or more this is the block's own `(bw/4, bh/4)`.
pub(crate) fn chroma_mi_shape(bw: usize, bh: usize, ss_x: usize, ss_y: usize) -> (usize, usize) {
    match (bw, bh, ss_x, ss_y) {
        // BLOCK_4X4 -> BLOCK_8X8 / BLOCK_8X4 / BLOCK_4X8 / itself.
        (4, 4, 1, 1) => (2, 2),
        (4, 4, 1, 0) => (2, 1),
        (4, 4, 0, 1) => (1, 2),
        // BLOCK_4X8 -> BLOCK_8X8 / BLOCK_8X8 / BLOCK_4X8 / itself.
        (4, 8, 1, _) => (2, 2),
        (4, 8, 0, 1) => (1, 2),
        // BLOCK_8X4 -> BLOCK_8X8 / BLOCK_8X4 / BLOCK_8X8 / itself.
        (8, 4, 1, 0) => (2, 1),
        (8, 4, 1, 1) | (8, 4, 0, 1) => (2, 2),
        // BLOCK_4X16 -> BLOCK_8X16 / BLOCK_8X16 / BLOCK_4X16 / itself.
        (4, 16, 1, _) => (2, 4),
        (4, 16, 0, 1) => (1, 4),
        // BLOCK_16X4 -> BLOCK_16X8 / BLOCK_16X4 / BLOCK_16X8 / itself.
        (16, 4, 1, 0) => (4, 1),
        (16, 4, 1, 1) | (16, 4, 0, 1) => (4, 2),
        _ => (bw / 4, bh / 4),
    }
}

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
///
/// The fourth sweep (lane-av1lambda) ran the BD gates themselves rather than
/// those ladders, and 0.1 was far off the point: the rate term was carrying
/// roughly twice the weight it is worth. 640x384 gate, BD-rate vs libaom /
/// vs rav1e for 1080p film, 2160p film, screen capture:
///
/// | lambda | 1080p | 2160p | screen |
/// |--------|-------|-------|--------|
/// | 0.0125 | +78.0 / +31.3 | +90.8 / +46.5 | +57.2 / -0.3 |
/// | 0.025  | +71.5 / +28.4 | +87.0 / +46.5 | +50.4 / -3.8 |
/// | 0.0375 | +71.2 / +28.5 | +84.3 / +45.4 | +48.7 / -4.4 |
/// | 0.05   | +71.9 / +29.6 | +87.1 / +49.3 | +48.7 / -4.0 |
/// | 0.075  | +74.8 / +32.7 | +91.3 / +52.8 | +51.4 / -1.3 |
/// | 0.1    | +79.2 / +36.9 | +97.7 / +58.8 | +53.1 / -0.4 |
/// | 0.125  | +84.0 / +41.1 | +100.7 / +61.5 | +56.8 / +2.3 |
/// | 0.15   | +86.0 / +42.9 | +104.4 / +65.2 | +59.5 / +4.2 |
/// | 0.2    | +89.6 / +45.5 | +109.9 / +69.8 | +62.3 / +7.3 |
///
/// and on the native 1920x1024 crops, which is the table that decides:
///
/// | lambda | film 1080p | film 2160p | screen |
/// |--------|------------|------------|--------|
/// | 0.025  | +17.1 / -0.5 | +49.0 / +21.1 | +51.5 / -15.1 |
/// | 0.0375 | +16.3 / -1.0 | +47.5 / +19.7 | +50.7 / -15.1 |
/// | 0.05   | +17.0 / -0.3 | +47.0 / +19.2 | +51.6 / -14.2 |
/// | 0.1    | +18.1 / +0.9 | +47.1 / +19.0 | +58.3 / -9.8 |
///
/// 0.0375 was the best point on two clips of three there and cost the "2160p
/// film" row +0.4/+0.7, outside the +-0.3 band every other lever here is kept
/// inside, so 0.05 shipped. BOTH of those tables' "film" rows are the
/// `testsrc2` COLOUR BARS fixtures (see [`bd_rate_screen_native`]), ~90% of
/// whose cells have no intra cost -- no real film supported the point.
///
/// FIFTH SWEEP (lane-av1resweep, 2026-09-07), on the native gate's two REAL
/// film crops (film A 1080p, film B 2160p HDR) plus the capture, BD-rate vs
/// libaom / vs rav1e, everything else at the shipped defaults:
///
/// | lambda | bars 1080p | bars 2160p | film A | film B | screen |
/// |--------|------------|------------|--------|--------|--------|
/// | 0.02   | +17.5 / -0.2 | +49.7 / +21.7 | +65.8 / +35.0 | +91.2 / +55.4 | +53.4 / -14.1 |
/// | 0.0275 | +16.6 / -0.9 | +48.3 / +20.5 | +65.5 / +34.7 | +91.6 / +55.1 | +51.3 / -15.0 |
/// | 0.035  | +16.1 / -1.3 | +47.5 / +19.7 | +66.4 / +35.2 | +94.2 / +56.9 | +50.8 / -15.1 |
/// | 0.05   | +15.6 / -1.7 | +46.5 / +18.8 | +69.0 / +37.5 | +100.0 / +61.5 | +51.0 / -14.5 |
/// | 0.07   | +16.6 / -0.5 | +46.8 / +18.8 | +72.3 / +40.4 | +108.5 / +68.8 | +54.4 / -12.4 |
/// | 0.1    | +16.5 / -0.7 | +46.6 / +18.7 | +76.9 / +44.4 | +118.2 / +77.1 | +57.3 / -10.1 |
///
/// Real film wants roughly HALF the rate weight the bars asked for: the bowl
/// on both film rows bottoms between 0.02 and 0.0275 and is 5-11 points deep
/// against the shipped 0.05, while the bars rows walk the other way (they are
/// recorded, not decided on). 0.02 breaks the capture (+2.4 vs libaom), so
/// 0.0275 ships -- both films 3.5/8.4 points down vs libaom, the capture
/// +0.3/-0.5 (at the +-0.3 band, and better than baseline once the var-tx
/// defaults below are on: +50.1 / -15.4). It spends ~5% more encode wall.
const LAMBDA_SCALE: f64 = 0.0275;

/// The rate weight a HIDDEN frame (a pyramid ARF, coded but not shown) codes
/// under, as a multiple of [`LAMBDA_SCALE`] (lane-arfcen).
///
/// The ARF-vs-ARF census says our top ARF codes residual over HALF its area
/// (rav1e's, at the same display position, over 26-30%) and comes out 1.4-2.5x
/// its bytes at the same PSNR, while every leaf that predicts from it is
/// still 0.4 dB behind rav1e's -- i.e. the ARF's extra fidelity is bought at
/// a rate weight fitted on shown frames and does not propagate. This is the
/// one knob that prices a hidden frame's rate differently from its q, which
/// the pyramid's q offsets cannot do (they move lambda and step together).
const HIDDEN_LAMBDA_FACTOR: f64 = 1.0;

/// [`HIDDEN_LAMBDA_FACTOR`], swept by `EC_AV1_LAMBDA_HIDDEN` in any build.
fn hidden_lambda_factor() -> f64 {
    match std::env::var("EC_AV1_LAMBDA_HIDDEN")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(f) => f,
        None => HIDDEN_LAMBDA_FACTOR,
    }
}

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

/// How a key frame's lambda differs from an inter frame's: libaom weighs the
/// two apart (`rd_frame_type_factor`, key 128 against an inter frame's 144 out
/// of 128), because every later frame predicts from the key frame, so an error
/// left in it is paid for over the whole group. This encoder has no temporal
/// propagation model at all, and this factor is the cheapest stand-in for one.
/// Swept by `EC_AV1_LAMBDA_KEY` in a test build, like [`lambda_scale`].
///
/// MEASURED and NOT KEPT. 640x384 gate at [`LAMBDA_SCALE`] 0.05, BD vs
/// libaom / vs rav1e:
///
/// | k_key | 1080p | 2160p | screen |
/// |---|---|---|---|
/// | 1.0 | +71.9 / +29.6 | +87.1 / +49.3 | +48.7 / -4.0 |
/// | 0.85 | +71.5 / +29.4 | +87.0 / +48.3 | +49.7 / -3.9 |
/// | 0.7 | +71.3 / +28.9 | +87.2 / +48.6 | +49.4 / -4.1 |
/// | 0.5 | +71.4 / +28.9 | +88.7 / +49.6 | +50.1 / -3.4 |
///
/// The two films want the key frame coded finer and the screen capture wants
/// the opposite, by about the same amount, and at native the whole lever is
/// inside the +-0.3 band that decides nothing: k_key 0.7 reads +16.9 / -0.3,
/// +47.2 / +19.2, +51.4 / -14.7 against 1.0's +17.0 / -0.3, +47.0 / +19.2,
/// +51.6 / -14.2. A gate whose GOP is one key frame in twelve cannot see a
/// per-frame-type weight; the real lever here is temporal propagation
/// (libaom's `tpl`), which weighs each BLOCK by how much of it later frames
/// predict from, not each frame.
const KEY_LAMBDA_FACTOR: f64 = 1.0;

/// [`KEY_LAMBDA_FACTOR`], or what `EC_AV1_LAMBDA_KEY` names in a test build.
fn key_lambda_factor() -> f64 {
    match std::env::var("EC_AV1_LAMBDA_KEY")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        Some(k) if cfg!(test) => k,
        _ => KEY_LAMBDA_FACTOR,
    }
}

// Every frame's predicted coefficient bits since the last
// `take_predicted_bits`, in coding order -- the encoder half of
// `predicted_coeff_bits_track_the_tile_the_writer_wrote`'s drift measurement.
#[cfg(test)]
thread_local! {
    static PREDICTED_BITS: std::cell::RefCell<Vec<f64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Records one frame's predicted coefficient bits.
#[cfg(test)]
fn record_predicted_bits(bits: f64) {
    PREDICTED_BITS.with(|p| p.borrow_mut().push(bits));
}

/// Takes (and clears) what the frames encoded on this thread were predicted to
/// spend on coefficients.
#[cfg(test)]
pub(crate) fn take_predicted_bits() -> Vec<f64> {
    PREDICTED_BITS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Whether a 16x16 inter leaf may split into four 8x8 ones of its own.
pub(crate) const SPLIT_INTER_8: bool = true;

/// [`SPLIT_INTER_8`], or what `EC_AV1_SPLIT_INTER8` names in a test build.
/// Whether a key frame codes `tx_mode == TxMode::Select` and searches each
/// block's transform depth (lane-av1tx). On by default; `EC_AV1_TX_SELECT=0`
/// turns it off for an A/B on one build.
/// Whether an INTER frame also carries `TxMode::Select`: an inter block's
/// residual as one transform over the whole block or as the four transforms
/// of half the side ([`commit_inter_luma`]), a searched `tx_depth` on every
/// intra block inside the inter frame. ON since lane-av1tx2, which found why
/// the streams desynced -- the writer counted the [`crate::decode::
/// TXFM_CTX_INIT`] band value of a neighbour outside the tile, where the
/// reader drops the term -- and measured the split search: BD-rate vs libaom
/// 233.5 -> 226.1 and 170.9 -> 167.4 on two clips, 212.8 -> 213.2 on the
/// third, for 13% encode wall. `EC_AV1_TX_SELECT_INTER=0` turns it off for an
/// A/B on one build.
fn tx_select_inter() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TX_SELECT_INTER")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::TX_SELECT_INTER))
}

/// SCREEN CONTENT ONLY, measured (12-frame `bd_rate_screen_native`, control
/// vs arm): the capture row goes +27.8/-26.0 -> +20.8/-30.1 -- seven points
/// against libaom -- while BOTH film rows LOSE (film A +21.7/-4.4 ->
/// +22.1/-4.0, film B +26.9/-0.6 -> +28.1/+0.7) and 2160p bars lose 0.8/1.1.
/// The keep rule wants both films down, so this takes the same content gate
/// palette, intrabc and the angle-delta refinement already stand behind: a
/// non-screen frame is byte-identical to the encoder before the lane.
///
/// The transform types an intra luma transform unit of `set` may be coded
/// with -- exactly the alphabet the writer's `tx_type` symbol names for that
/// set, so a searched type is always one the tile can express (lane-txset).
/// `DCT_DCT` first: it is the incumbent every other candidate has to beat.
///
/// Under this encoder's `reduced_tx_set` header bit every intra set at 16x16
/// and below is `TX_SET_INTRA_2` (spec `get_tx_set`), the five types
/// [`TxType::from_symbol`] names; 32x32 and 64x64 luma code no symbol at all,
/// and inter luma reads the two-type `TX_SET_INTER_3`, left at `DCT_DCT`
/// here.
pub(crate) fn tx_type_candidates(set: TxbSet, screen: bool) -> &'static [TxType] {
    const DCT: [TxType; 1] = [TxType::DctDct];
    const INTRA1: [TxType; 7] = [
        TxType::DctDct,
        TxType::AdstAdst,
        TxType::AdstDct,
        TxType::DctAdst,
        TxType::Idtx,
        TxType::VDct,
        TxType::HDct,
    ];
    const INTRA2: [TxType; 5] = [
        TxType::DctDct,
        TxType::AdstAdst,
        TxType::AdstDct,
        TxType::DctAdst,
        TxType::Idtx,
    ];
    match set {
        TxbSet::Luma16 | TxbSet::Luma8 | TxbSet::Luma4 if intra_tx_search_on(screen) => &INTRA2,
        // lane-txi: a `reduced_tx_set == 0` frame's intra 8x8/4x4 reads the
        // SEVEN-type `TX_SET_INTRA_1` (`decode::txbset_for`'s `Luma{8,4}Set1`;
        // 16x16 stays `TX_SET_INTRA_2`, there is no `Luma16Set1`). The two
        // extra members are the 1-D DCTs, and the order is the reader's own
        // map (`decode::tx_type_from_symbol`, 8-wide CDF).
        TxbSet::Luma8Set1 | TxbSet::Luma4Set1 if intra_tx_search_on(screen) => &INTRA1,
        _ => &DCT,
    }
}

/// The transform types an INTER luma transform unit of `set` may be coded
/// with (lane-txset2). Under this encoder's `reduced_tx_set` header bit every
/// inter luma set at 16x16 and below is the two-type `TX_SET_INTER_3`
/// (`IDTX`, `DCT_DCT`) and 32x32 is `EXT_TX_SET_DCT_IDTX` -- the same two
/// types, a symbol the writer already codes and never varied. 64x64 codes no
/// `tx_type` symbol at all, so it is not offered one.
/// SCREEN CONTENT ONLY, measured (12-frame `bd_rate_screen_native`): the
/// capture row goes +20.8/-30.1 -> +20.1/-30.4 while the two synthetic bars
/// rows lose TWENTY points (-1.0/-16.8 -> +23.1/+3.3 and +9.4/-12.7 ->
/// +31.2/+3.7) and film B 0.3 -- the `local-rd-on-references` class at its
/// loudest, and restricting the offer to 16x16 and below does not soften it
/// (`le16`: +22.9/+3.1, +31.6/+4.0). So this takes the same content gate the
/// intra search, palette and intrabc stand behind: a non-screen frame is
/// byte-identical to the encoder before the lane.
///
/// `EC_AV1_TXSET_INTER`: `0`/`off`, `screen` (screen frames only), `le16`
/// (16x16 and below, the sizes libaom's own census picks a non-`DCT_DCT`
/// inter type on -- it never takes `IDTX` at 32x32 on film B), anything else
/// on for every frame at every size.
pub(crate) fn inter_tx_type_candidates(set: TxbSet, screen: bool) -> &'static [TxType] {
    const DCT: [TxType; 1] = [TxType::DctDct];
    const INTER3: [TxType; 2] = [TxType::DctDct, TxType::Idtx];
    const INTER_WIDE: [TxType; 7] = [
        TxType::DctDct,
        TxType::AdstAdst,
        TxType::AdstDct,
        TxType::DctAdst,
        TxType::Idtx,
        TxType::VDct,
        TxType::HDct,
    ];
    let mode = inter_tx_type_search();
    let on = match mode {
        InterTxSearch::Off => false,
        InterTxSearch::Screen => screen,
        InterTxSearch::All | InterTxSearch::Le16 => true,
    };
    match set {
        TxbSet::Luma32Inter if on && mode != InterTxSearch::Le16 => &INTER3,
        TxbSet::Luma16Inter | TxbSet::Luma8Inter | TxbSet::Luma4Inter if on => &INTER3,
        // lane-txi: a `reduced_tx_set == 0` frame's inter 16x16 reads the
        // TWELVE-type `TX_SET_INTER_2` and its 8x8/4x4 `ALL16` (inter 32x32
        // stays the two-type `DCT_IDTX`). The seven offered here are the
        // members those two alphabets SHARE with the intra set -- the four
        // DTT combinations, `IDTX` and the two 1-D DCTs -- which is the
        // candidate list rav1e's speed 6 prunes to; the FLIPADST family and
        // the 1-D ADSTs are codable and left unoffered (deferred).
        TxbSet::Luma16InterSet1 | TxbSet::Luma8InterSet1 | TxbSet::Luma4InterSet1 if on => {
            &INTER_WIDE
        }
        _ => &DCT,
    }
}

/// lane-txi: does THIS frame code `reduced_tx_set = 0` (spec 5.9.2), i.e.
/// the wider `tx_type` alphabets at intra 8x8/4x4 and inter 16x16 and below?
/// `EC_AV1_TXSET_WIDE`: `0`/`off`, `screen` (screen frames only), anything
/// else every frame; unset takes [`crate::speed::WIDE_TX_SET`], screen-gated
/// like both halves of the type search itself.
pub(crate) fn wide_tx_set(screen: bool) -> bool {
    match WIDE_TX_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    static ENV: std::sync::LazyLock<Option<InterTxSearch>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TXSET_WIDE")
            .ok()
            .map(|v| match v.as_str() {
                "0" | "off" => InterTxSearch::Off,
                "screen" => InterTxSearch::Screen,
                _ => InterTxSearch::All,
            })
    });
    match ENV.unwrap_or(if crate::speed::at(&crate::speed::WIDE_TX_SET) {
        InterTxSearch::Screen
    } else {
        InterTxSearch::Off
    }) {
        InterTxSearch::Off => false,
        InterTxSearch::Screen => screen,
        _ => true,
    }
}

/// A PROCESS-GLOBAL [`wide_tx_set`] override that wins over both the
/// environment and the preset (`u8::MAX` = unset), the shape
/// [`set_inter_tx_search`] has: the lever is OFF at every preset, so the
/// witness that has to see the wider alphabets turns it on through this while
/// holding [`crate::speed::knob_write`].
static WIDE_TX_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`WIDE_TX_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_wide_tx_set(on: Option<bool>) {
    WIDE_TX_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The luma [`TxbSet`] the SEARCH and its pricer name for a frame that codes
/// `reduced_tx_set = 0`: the writer reads the bit once, off the frame's own
/// [`crate::cdf_state::Cdfs`], but the trial that prices a type has to read
/// the same alphabet or the two disagree on what a `tx_type` symbol costs.
pub(crate) fn luma_set_for(set: TxbSet, screen: bool) -> TxbSet {
    if wide_tx_set(screen) { set.wide() } else { set }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum InterTxSearch {
    Off,
    Screen,
    /// 16x16 and below only.
    Le16,
    All,
}

/// A PROCESS-GLOBAL [`inter_tx_type_search`] override that wins over both the
/// environment and the preset (`u8::MAX` = unset), the shape
/// [`set_deltaq_res`] has: the lever is OFF at every preset, so the witness
/// that has to see a non-`DCT_DCT` inter unit turns it on through this while
/// holding [`crate::speed::knob_write`].
static INTER_TX_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`INTER_TX_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_inter_tx_search(on: Option<bool>) {
    INTER_TX_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn inter_tx_type_search() -> InterTxSearch {
    match INTER_TX_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return InterTxSearch::Off,
        1 => return InterTxSearch::All,
        _ => {}
    }
    static ENV: std::sync::LazyLock<Option<InterTxSearch>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TXSET_INTER")
            .ok()
            .map(|v| match v.as_str() {
                "0" | "off" => InterTxSearch::Off,
                "screen" => InterTxSearch::Screen,
                "le16" => InterTxSearch::Le16,
                _ => InterTxSearch::All,
            })
    });
    ENV.unwrap_or_else(|| {
        if crate::speed::at(&crate::speed::TX_TYPE_SEARCH_INTER) {
            InterTxSearch::Screen
        } else {
            InterTxSearch::Off
        }
    })
}

/// The inter luma [`TxbSet`] a `tx`-sided transform unit reads, `None` for the
/// 64-point one (it codes no `tx_type` symbol).
/// lane-txw history, resolved by lane-intertx: the INTER half of the
/// widening was blocked by `refusal-from-own-desync` twice over. Half of it
/// was this writer coding a `V_DCT`/`H_DCT` unit as `TX_CLASS_2D`
/// (`tile::write_coeffs`, fixed by lane-txw, which unblocked the intra
/// half). The other half was the CHROMA inheritance: an inter block's
/// chroma transform type is the luma type reduced to the chroma size's set
/// (`decode::reduce_inherited_chroma_tx_type`), and (a) the encoder never
/// set `FrameCtx::reduced_tx_set_inter`, so [`recode_inter_chroma`] reduced
/// against a STALE allowance, and (b) `tile::write_block_planes` coded
/// every chroma plane as `DCT_DCT`, so an inherited 1-D type desynced the
/// chroma exactly as luma did. lane-intertx fixed both -- the frame sets
/// its own bit on the context (`encode_inter_frame`), the writer derives
/// the chroma type from this frame's own header bit
/// (`reduce_inherited_chroma_tx_type_flagged`) -- and the widened SEARCH
/// names these sets through [`luma_set_for`] at its call sites, so the
/// wider candidates are reachable and honestly priced. The intra half
/// never needed either (an intra block's chroma type comes off its own UV
/// mode, never off luma).
fn inter_luma_set(tx: usize) -> Option<TxbSet> {
    Some(match tx {
        4 => TxbSet::Luma4Inter,
        8 => TxbSet::Luma8Inter,
        16 => TxbSet::Luma16Inter,
        32 => TxbSet::Luma32Inter,
        _ => return None,
    })
}

fn tx_type_search() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TXSET")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::TX_TYPE_SEARCH))
}

/// Whether an intra luma unit of this frame searches its own type at all:
/// the preset lever ([`tx_type_search`]) under the screen content gate, which
/// `EC_AV1_TXSET=all` ([`intra_tx_all_frames`]) lifts and the process-global
/// [`set_intra_tx_search`] override wins over (the witness needs the search
/// on a frame the detector called non-screen).
fn intra_tx_search_on(screen: bool) -> bool {
    match INTRA_TX_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    (screen || intra_tx_all_frames()) && tx_type_search()
}

/// A PROCESS-GLOBAL [`intra_tx_search_on`] override (`u8::MAX` = unset), the
/// shape [`set_inter_tx_search`] has.
static INTRA_TX_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`INTRA_TX_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_intra_tx_search(on: Option<bool>) {
    INTRA_TX_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// lane-txi: `EC_AV1_TXSET=all` offers the five-type intra set on EVERY
/// frame -- the unconditional arm the gate measures -- instead of the screen
/// content gate [`tx_type_candidates`] ships behind. Any other value keeps
/// the gate (`0`/`off` still switches the search off entirely).
fn intra_tx_all_frames() -> bool {
    static ENV: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_TXSET").is_ok_and(|v| v == "all"));
    *ENV
}

fn tx_select() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TX_SELECT")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::TX_SELECT_KEY))
}

fn split_inter_8() -> bool {
    match std::env::var("EC_AV1_SPLIT_INTER8").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::SPLIT_8),
    }
}

/// Whether a 16x16 (or 8x8) leaf runs a motion search of its own beside the
/// NEARESTMV candidate it always had.
pub(crate) const LEAF_NEW_MV: bool = true;

/// Whether GOLDEN/ALTREF get a `NEWMV` search of their own. Measured on the
/// BD gate: with the early-out below at its swept margin, -1.7/-2.5/-0.0
/// points vs libaom and -1.6/-2.3/-0.0 vs rav1e for 4%/4%/0.3% more motion
/// searches (the census `bd_rate_vs_libaom_and_rav1e` prints).
pub(crate) const EXTRA_REF_NEW_MV: bool = true;

/// How much cheaper an extra reference's `NEARESTMV` has to be than LAST's
/// own best vector before that reference's `NEWMV` search is skipped: a
/// factor on the search cost scale, `1.0` meaning "as good or better".
/// Swept on the BD gate over 0.15/0.25/0.35/0.5/0.8/1.0/1.5 and off (`0.0`,
/// never skip): the whole BD gain survives down to 0.35 (+124.8/+150.4/+80.9
/// vs libaom, against +124.9/+150.3/+81.0 with no early-out at all) while the
/// extra searches drop from +27% to +4% of the base count -- and on screen
/// capture, where an extra reference's NEWMV wins nothing, it skips 99.7% of
/// them. Below 0.35 the gain starts eroding (0.15: +125.3/+151.7).
/// RE-SWEPT on lane-av1rejudge at [`LAMBDA_SCALE`] 0.05 with warp on: 0.25
/// reads +16.6/-0.6, +47.0/+19.1, +51.3/-14.3 and 0.5 reads +16.6/-0.7,
/// +47.0/+19.1, +51.3/-14.3 against the base's +16.7/-0.5, +47.0/+19.1,
/// +51.3/-14.3 -- one row 0.1-0.2 down, two flat, inside the measurement's
/// own noise. 0.35 stays; the lever that did move at the new rate weight is
/// [`LEAF_SECOND_NEW_MARGIN`], not this one.
const EXTRA_NEW_SKIP_MARGIN: f64 = 0.35;

/// [`EXTRA_NEW_SKIP_MARGIN`], swept by `EC_AV1_MV_SKIP_MARGIN`.
fn extra_new_skip_margin() -> f64 {
    match std::env::var("EC_AV1_MV_SKIP_MARGIN").ok() {
        Some(v) if cfg!(test) => v.parse().unwrap_or(EXTRA_NEW_SKIP_MARGIN),
        _ => EXTRA_NEW_SKIP_MARGIN,
    }
}

/// The margin on a LEAF's SECOND-reference motion search, the same early-out
/// shape [`EXTRA_NEW_SKIP_MARGIN`] gives a whole block's extra-reference
/// `NEWMV`: the search is skipped when that reference's own
/// `NEAREST_NEARESTMV` vector already prices this many times better than the
/// leaf's `LAST` search found. `0.0` never skips.
///
/// Swept on the BD gate over 0.15/0.35/0.6/0.8/1.2 and 0.0, reading BD vs
/// libaom (1080p/2160p/screen) against the extra `motion::search` calls at
/// 2160p (38195 with no second-reference search at all):
/// 0.15 +81.7/+97.4/+54.5 at 41286, 0.35 +81.4/+96.7 at 43093,
/// 0.6 +80.6/+95.5 at 44843, 0.8 +79.9/+94.6 at 47008, 1.2 +80.2/+94.2 at
/// 77882, never-skip +79.7/+94.1 at 81801. 0.8 takes the whole gain the
/// unskipped search buys (-3.1/-5.3 off the +83.0/+99.9 base) for a quarter
/// of its extra searches; past it the calls double for another -0.3/-0.5.
/// Screen capture is BYTE-IDENTICAL at every margin -- its leaf compound is
/// all `NEAREST_NEARESTMV` and no searched second vector ever wins there --
/// so the margin is pure wall on that content (+10% searches at 0.8).
///
/// RE-SWEPT on lane-av1rejudge after [`LAMBDA_SCALE`] moved 0.1 -> 0.05, on
/// the native gate with local warp on, BD vs libaom / vs rav1e:
///
/// | margin | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | 0.6 | +16.9 / -0.4 | +47.0 / +19.1 | +51.2 / -14.4 |
/// | 0.8 (was) | +16.7 / -0.5 | +47.0 / +19.1 | +51.3 / -14.3 |
/// | 1.2 | +16.2 / -1.1 | +46.6 / +18.8 | +51.3 / -14.4 |
/// | 1.6 | +16.4 / -0.8 | +46.6 / +18.8 | +51.3 / -14.4 |
/// | 0.0 (never skip) | +16.4 / -0.8 | +46.6 / +18.8 | +51.3 / -14.4 |
///
/// 1.2 is the floor of that bowl -- two rows 0.3-0.6 down on BOTH columns
/// with screen flat -- and it beats never-skipping, i.e. the early-out is
/// still worth having, just at a looser margin than the old rate weight
/// wanted. It costs about 10% encoder wall on the two film clips (36.6s vs
/// 32.6s at 2160p), which is the price of the extra second-reference
/// searches.
const LEAF_SECOND_NEW_MARGIN: f64 = 1.2;

/// [`LEAF_SECOND_NEW_MARGIN`], swept by `EC_AV1_LEAF_SECOND_MARGIN`; the
/// search itself is switched off by `EC_AV1_LEAF_SECOND_NEWMV=0`.
fn leaf_second_new_margin() -> f64 {
    match std::env::var("EC_AV1_LEAF_SECOND_MARGIN").ok() {
        Some(v) => v.parse().unwrap_or(LEAF_SECOND_NEW_MARGIN),
        None => LEAF_SECOND_NEW_MARGIN,
    }
}

/// Whether a leaf searches its SECOND reference at all (lane-av1comp4).
fn leaf_second_new_mv() -> bool {
    match std::env::var("EC_AV1_LEAF_SECOND_NEWMV").ok() {
        Some(v) => v != "0",
        None => crate::speed::at(&crate::speed::LEAF_SECOND),
    }
}

/// Whether a 16x16 or 8x8 inter LEAF is offered the compound candidates the
/// whole 32x32 block is (lane-av1comp3). The writer already codes a compound
/// block at any leaf size (`tile::write_inter_frame_leaf`/`..._leaf8` both
/// route through `write_compound_block`, which is sized by `bw4`/`bh4`); what
/// was missing was the encoder ever forming one below 32x32.
pub(crate) const LEAF_COMPOUND: bool = true;

/// [`LEAF_COMPOUND`], or what `EC_AV1_LEAF_COMPOUND` names in any build.
fn leaf_compound() -> bool {
    match std::env::var("EC_AV1_LEAF_COMPOUND").ok() {
        Some(v) => v != "0",
        None => crate::speed::at(&crate::speed::LEAF_COMPOUND),
    }
}

/// [`LEAF_NEW_MV`], or what `EC_AV1_LEAF_NEWMV` names in a test build.
fn leaf_new_mv() -> bool {
    match std::env::var("EC_AV1_LEAF_NEWMV").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => LEAF_NEW_MV,
    }
}

/// Whether an INTER frame's 32x32 block may be split into four 16x16 ones
/// when the trial says four cost less. Before lane-av1rd2 only a quadrant
/// the true frame edge cut through was ever split there; inside the frame
/// every block was coded whole, which left the partition decision the key
/// frame already makes unmade for eleven frames out of twelve.
pub(crate) const SPLIT_INTER_BLOCKS: bool = true;

/// [`SPLIT_INTER_BLOCKS`], or what `EC_AV1_SPLIT_INTER` names when the
/// measurement that keeps it is running (test builds only, like
/// [`lambda_scale`]).
fn split_inter_blocks() -> bool {
    match std::env::var("EC_AV1_SPLIT_INTER").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::SPLIT_INTER),
    }
}

/// Whether the inter search offers the whole superblock as ONE 64x64 block
/// (`PARTITION_NONE` at `BLOCK_64X64`), coded SKIP, against the four 32x32
/// quadrants it would otherwise always split into.
///
/// The census (`lanes/census-filmB.md`) is what points here: rav1e speed 6
/// codes 45.2% of its blocks at 64x64 -- 73.8% of its LEAF blocks, 99.2% of
/// them skip at 328 bytes a frame -- while every block this encoder wrote was
/// 32x32 or smaller, which put our mode+mv+partition+tx_size spend at 162,669
/// bits against rav1e's 61,988, about 46% of the film B gap.
///
/// lane-tx64: no longer skip-only. The root also prices a REAL residual --
/// one TX_64X64 luma transform (`transform.rs`'s forward network has always
/// reached 64 points; what was missing was the block coder and the writer
/// around it) and one TX_32X32 per chroma plane -- see [`b64_residual`],
/// which carries that arm's own gate table. Still refused at the 64 root:
/// compound references, a depth-1 4x32x32 var-tx tree, intra, and a
/// superblock the true frame edge cuts through.
///
/// Switched off by `EC_AV1_B64=0` in any build; `EC_AV1_B64RES=0` keeps the
/// root and restores the skip-only shape lane-b64 shipped.
pub(crate) const B64_ROOT: bool = true;

/// How many superblocks took the 64x64 root since the last
/// [`take_b64_root_hits`], so a gate reports how often it fires rather than
/// assuming it does (class `gate-blind-to-feature`).
static B64_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 64x64-root count since the last call, and zero it.
pub fn take_b64_root_hits() -> usize {
    B64_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether the 128 superblock root offers its `PARTITION_NONE` candidate at
/// all ([`search_root_128`]). On by default; `EC_AV1_B128=0` turns it off,
/// which is the attribution arm for a 128-superblock stream: it separates
/// what the SIZE costs from what the 128x128 BLOCK buys.
fn b128_root() -> bool {
    static ENV: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B128")
            .ok()
            .is_none_or(|v| v != "0")
    });
    *ENV
}

/// lane-b128r: whether the 128 root prices a REAL residual as well as the
/// skip arm -- four TX_64X64 luma units interleaved with a TX_32X32 chroma
/// pair per 64x64 mu chunk, var-tx depth 0 (the shape
/// [`crate::tile::write_inter_block_128`] codes and `decode_inter_block`
/// reads). Written, witnessed and byte-exact through both decoders, and
/// **off by default** (`EC_AV1_B128RES=1` turns it on) because it is INERT
/// on content: the RD takes it 0.7-1.9 times per frame against thousands of
/// whole 128 roots (lane-b128res census, native 12-frame gate, every clip),
/// and the BD effect is inside the noise on both gates -- long-GOP films
/// -0.1/+0.1 and all five 12-frame rows within 0.1 of the off default.
///
/// The historical reason for the parking is CLOSED: the bars 2160p q=90
/// frame 11 disagreement with ffmpeg (8 luma samples at the rows above a
/// 128-superblock horizontal edge) was the CDEF search pricing a preset per
/// 64x64 unit under a root that codes ONE literal for all four -- lane-
/// edge128's `crate::tile::cdef_unit_owner` collapses the four to the one
/// the decoder derives, and on this HEAD the same row re-measures three-way
/// EXACT (encoder reconstruction == ffmpeg == `decode_stream`, every frame,
/// every plane, both gates). The knob stays as the arm's gate, not as a
/// defect flag.
fn b128_residual() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B128RES")
            .ok()
            .map(|v| v != "0")
    });
    (ENV.unwrap_or(false) || FORCE_B128_RESIDUAL.load(std::sync::atomic::Ordering::Relaxed))
        && b128_root()
}
/// lane-rectres: whether the 128 root's RECTANGULAR pieces -- the two
/// HORZ/VERT halves and the four AB shapes' three pieces (two 64x64 squares
/// plus one half) -- price a REAL residual in the RD as well as the skip arm
/// ([`search_root_128_rect`] / [`search_root_128_ab`], written through
/// [`crate::tile::write_inter_block_128_rect`]). **Off by default**
/// (`EC_AV1_RECTRES=1` turns it on): OFF is today's behavior bit for bit --
/// no rect/AB piece can come out non-skip, so the writer's refusal is never
/// reached. The residual shape is the whole 128 block's, halved or quartered
/// per piece: one TX_64X64 luma unit plus a TX_32X32 chroma pair per 64x64
/// mu chunk, var-tx depth 0 (exactly what the decoder's mu-chunk loop reads
/// for any inter partition shape).
fn rectres() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_RECTRES")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(false) || FORCE_B128_RECTRES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test-only: take the rect/AB pieces' residual arm whatever it costs, so a
/// witness clip codes the shape rather than waiting for content on which the
/// RD picks it (the same role [`FORCE_B128_RESIDUAL`] plays at the NONE
/// root). Never read outside `#[cfg(test)]` builds.
static FORCE_B128_RECTRES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Arms [`FORCE_B128_RECTRES`] for the rest of the process.
#[cfg(test)]
pub(crate) fn force_b128_rectres(on: bool) {
    FORCE_B128_RECTRES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// How many rect/AB pieces the search left WITH a residual rather than
/// skipped, since the last [`take_b128_rectres_hits`] (class
/// `gate-blind-to-feature`).
static B128_RECTRES_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The rect/AB-piece-residual count since the last call, and zero it.
pub fn take_b128_rectres_hits() -> usize {
    B128_RECTRES_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// lane-b128r: whether the 128 root offers the COMPOUND candidates the 64
/// root already does (`NEAREST_NEARESTMV`, `GLOBAL_GLOBALMV`,
/// `NEW_NEARESTMV` over the LAST+GOLDEN / LAST+ALTREF pairs), at the block's
/// own 32x32-mi window. `EC_AV1_B128COMP=0` turns it off, which is this
/// arm's attribution control; the writer has always had the path
/// (`write_inter_block_128` calls `write_compound_block` for a block that
/// names a second reference).
fn b128_compound() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B128COMP")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(true) && b128_root()
}

/// How many 128 roots came out COMPOUND since the last
/// [`take_b128_compound_hits`] (class `gate-blind-to-feature`).
static B128_COMPOUND_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The compound-128 count since the last call, and zero it.
pub fn take_b128_compound_hits() -> usize {
    B128_COMPOUND_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// lane-b128hv: whether the 128 superblock root offers its `PARTITION_HORZ`
/// and `PARTITION_VERT` candidates -- two 128x64 / 64x128 inter blocks, the
/// same skip-only single-reference/compound machinery the NONE arm ships,
/// halved ([`search_root_128_rect`], `crate::tile::write_inter_block_128_rect`).
/// `EC_AV1_B128HV=0` is this arm's attribution control.
fn b128_rect() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_B128HV").ok().map(|v| v != "0"));
    ENV.unwrap_or(true) && b128_root()
}

/// lane-rectdq: whether the delta_q symbol group is priced on the root
/// candidates that really code it. `EC_AV1_RECTDQ=0` is this fix's
/// attribution control (the pre-lane pricing, where a 128 rect root and the
/// four-superblock arm were both priced as if the group were free).
fn rectdq() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_RECTDQ").ok().map(|v| v != "0"));
    ENV.unwrap_or(true)
}

/// lane-rectdq: what `crate::tile::write_delta_q` spends to move
/// `CurrentQIndex` from `cur` to `target` at `1 << delta_q_res` steps of
/// `res` -- the same three pieces that writer codes, in its order: the
/// `delta_q_abs` symbol of `min(abs, 3)` against the DEFAULT table (the
/// writer's own `cdfs.delta_q`, which every tile arms fresh, adapts from
/// exactly this row), the escape literal pair when `abs >= 3`, and the sign
/// bit when `abs != 0`. Zero when the plan is disarmed (`res <= 0`).
pub(crate) fn delta_q_bits(target: i32, cur: i32, res: i32) -> f64 {
    if res <= 0 {
        return 0.0;
    }
    /// spec `DELTA_Q_SMALL`, as in the writer.
    const DELTA_Q_SMALL: usize = 3;
    let reduced = (target - cur) / res;
    let abs = reduced.unsigned_abs() as usize;
    // A pricing coder, not a sum of table entries: the escape's literal bits
    // are equiprobable SYMBOLS whose narrowing carries the `EC_MIN_PROB`
    // floor, so each really costs a little over one bit (class
    // `price-the-narrowing-not-the-table`). Read off the same
    // `SymbolEncoder::bits` account the writer's own coder keeps.
    let mut enc = crate::msac::SymbolEncoder::pricer();
    enc.reset_bits();
    enc.symbol_fixed(abs.min(DELTA_Q_SMALL), &cdf::DELTA_Q);
    if abs >= DELTA_Q_SMALL {
        let rem_bits = 31 - ((abs as u32) - 1).leading_zeros();
        enc.literal(rem_bits - 1, 3);
        enc.literal(abs as u32 - ((1u32 << rem_bits) + 1), rem_bits);
    }
    if abs != 0 {
        enc.literal(u32::from(reduced < 0), 1);
    }
    enc.bits()
}

/// [`delta_q_bits`] for the 128 superblock root at `(root_r, root_c)` (in 64
/// superblock units): the group every root shape but a SKIPPED
/// `PARTITION_NONE` root codes, since decode.rs' `whole_sb` asks for
/// `side == write_w == write_h == sb_px` and neither a rect half nor a 64
/// superblock under a 128 grid is that.
///
/// The target is the root's own cell of the `sb_q` grid, exactly the entry
/// `write_delta_q` indexes. `CurrentQIndex` is NOT knowable in the search --
/// the writer's `plan.cur` only advances at the roots that really code a
/// group, and which those are is decided by this very comparison -- so the
/// PREVIOUS 128 root's target in raster order stands in for it (the frame's
/// `base_q_idx` at the first root of the frame, which is also the writer's
/// own per-tile reset). Wrong only where a neighbouring root came out a
/// skipped NONE block, and then only by the difference of two adjacent tpl
/// cells.
fn delta_q_bits_at_root(
    sb_q: Option<&Vec<u8>>,
    sb_cols: usize,
    base_q_idx: u8,
    res: i32,
    (root_r, root_c): (usize, usize),
) -> f64 {
    if !rectdq() || res <= 0 {
        return 0.0;
    }
    let Some(q) = sb_q.filter(|q| !q.is_empty()) else {
        return 0.0;
    };
    let at = |r: usize, c: usize| i32::from(q[(r * sb_cols + c).min(q.len() - 1)]);
    let target = at(root_r, root_c);
    let cur = if root_c >= 2 {
        at(root_r, root_c - 2)
    } else if root_r >= 2 {
        // The last 128 root of the row above, i.e. the previous one in the
        // writer's raster walk.
        at(root_r - 2, (sb_cols.saturating_sub(1)) & !1)
    } else {
        i32::from(base_q_idx)
    };
    delta_q_bits(target, cur, res)
}

/// How many 128 roots the search left as two 128x64 (HORZ) / 64x128 (VERT)
/// halves since the last take (class `gate-blind-to-feature`).
static B128_HORZ_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The VERT twin of [`B128_HORZ_HITS`].
static B128_VERT_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The HORZ-128-root count since the last call, and zero it.
pub fn take_b128_horz_hits() -> usize {
    B128_HORZ_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The VERT-128-root count since the last call, and zero it.
pub fn take_b128_vert_hits() -> usize {
    B128_VERT_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Test-only: force the 128 root's rect partition whatever it costs -- 0 off,
/// 1 `PARTITION_HORZ`, 2 `PARTITION_VERT`. The witness needs it because
/// synthetic content is where the shape is cheapest to decode-check and the
/// RD there prefers one whole block; the gate census is the unforced count.
static FORCE_B128_RECT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Arms [`FORCE_B128_RECT`] for the rest of the process (0 / 1 HORZ / 2 VERT).
#[cfg(test)]
pub(crate) fn force_b128_rect(which: u8) {
    FORCE_B128_RECT.store(which, std::sync::atomic::Ordering::Relaxed);
}

/// lane-ab128: whether the 128 superblock root offers its four AB shapes --
/// `PARTITION_HORZ_A` (two 64x64 on top, one 128x64 below), `_HORZ_B` (one
/// 128x64 on top, two 64x64 below), `_VERT_A` (two 64x64 left, one 64x128
/// right) and `_VERT_B` (one 64x128 left, two 64x64 right). Every piece is
/// the same skip-only single-reference/compound block the NONE and HORZ/VERT
/// arms ship, at its own footprint ([`search_root_128_ab`],
/// `crate::tile::write_inter_block_128_rect`). `EC_AV1_B128AB=0` is this
/// arm's attribution control.
///
/// **Off by default** (`EC_AV1_B128AB=1` turns it on): measured on both gates
/// and it does not pay. The shapes fire 0-2 roots a frame against ~1000 coded
/// blocks (`EC_AV1_B128_CENSUS`), and every row is flat or 0.1 worse --
/// long-GOP film A +22.0/-8.5 against a +21.9/-8.6 control, film B +75.8/+0.8
/// against the same, 12-frame film A +18.1/-6.2 against +18.0/-6.3, film B
/// +23.1/-3.3 against +23.0/-3.5, screen byte-identical (lane-ab128). The
/// machinery is written, witnessed sample-exact through both decoders on all
/// four shapes, and kept for the arm that makes it pay: a non-skip piece (the
/// 64x64 pieces could carry the 64 root's residual, which is where libaom's
/// AB shapes earn their keep) or a cheaper partition price.
fn b128_ab() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_B128AB").ok().map(|v| v != "0"));
    ENV.unwrap_or(false) && b128_root()
}

/// How many 128 roots the search left as each AB shape since the last take,
/// in `PARTITION_HORZ_A`, `_HORZ_B`, `_VERT_A`, `_VERT_B` order (class
/// `gate-blind-to-feature`).
static B128_AB_HITS: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];

/// The four per-shape AB-root counts since the last call, and zero them.
pub fn take_b128_ab_hits() -> [usize; 4] {
    std::array::from_fn(|i| B128_AB_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Test-only: force the 128 root's AB shape whatever it costs -- 0 off, then
/// 4..=7, the partition symbol itself ([`force_b128_rect`]'s twin; synthetic
/// content is where the shapes are cheapest to decode-check and the RD there
/// prefers one whole block).
static FORCE_B128_AB: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Arms [`FORCE_B128_AB`] for the rest of the process (0 off, else 4..=7).
#[cfg(test)]
pub(crate) fn force_b128_ab(which: u8) {
    FORCE_B128_AB.store(which, std::sync::atomic::Ordering::Relaxed);
}

/// lane-ab128: the three pieces of an AB shape at a 128 root, in the order
/// libaom's `decode_partition` codes them and decode.rs' `sb128_ab` arm
/// reads them (decode.rs ~35269) -- HORZ_A = TL, TR, bottom half; HORZ_B =
/// top half, BL, BR; VERT_A = TL, BL, right half; VERT_B = left half, TR, BR.
/// Each entry is the piece's own top-left in mode-info units and its
/// footprint in samples.
pub(crate) fn ab128_pieces(
    symbol: usize,
    (mi_row, mi_col): (usize, usize),
) -> [((usize, usize), (usize, usize)); 3] {
    const S: usize = SUPERBLOCK;
    let (r, c) = (mi_row, mi_col);
    let (r1, c1) = (mi_row + 16, mi_col + 16);
    match symbol {
        // PARTITION_HORZ_A
        4 => [((r, c), (S, S)), ((r, c1), (S, S)), ((r1, c), (S * 2, S))],
        // PARTITION_HORZ_B
        5 => [((r, c), (S * 2, S)), ((r1, c), (S, S)), ((r1, c1), (S, S))],
        // PARTITION_VERT_A
        6 => [((r, c), (S, S)), ((r1, c), (S, S)), ((r, c1), (S, S * 2))],
        // PARTITION_VERT_B
        _ => [((r, c), (S, S * 2)), ((r, c1), (S, S)), ((r1, c1), (S, S))],
    }
}

/// Test-only: take the 128 root's residual arm whatever it costs, so a
/// witness clip codes the shape rather than waiting for content on which the
/// RD picks it (the same role [`force_sb128`] plays for the superblock size).
/// Never read outside `#[cfg(test)]` builds.
static FORCE_B128_RESIDUAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test-only twin of [`FORCE_B128_RESIDUAL`] for the PARTITION decision: take
/// the 128 root whenever it produced a candidate at all, whatever its four
/// cells cost, so a witness codes the block it is about rather than waiting
/// for content on which the root also wins its own comparison.
static FORCE_B128_ROOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Arms [`FORCE_B128_RESIDUAL`] -- and with it [`FORCE_B128_ROOT`], since a
/// forced residual block only reaches the writer if its root is taken.
#[cfg(test)]
pub(crate) fn force_b128_residual(on: bool) {
    FORCE_B128_RESIDUAL.store(on, std::sync::atomic::Ordering::Relaxed);
    force_b128_root(on);
}

/// Arms [`FORCE_B128_ROOT`] for the rest of the process.
#[cfg(test)]
pub(crate) fn force_b128_root(on: bool) {
    FORCE_B128_ROOT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// How many 128 roots came out cheaper WITH a residual than skipped, since
/// the last [`take_b128_residual_hits`] (class `gate-blind-to-feature`).
static B128_RESIDUAL_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128-residual count since the last call, and zero it.
pub fn take_b128_residual_hits() -> usize {
    B128_RESIDUAL_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// How many 128x128 `PARTITION_NONE` roots the search took since the last
/// [`take_b128_none_hits`] (class `gate-blind-to-feature`): a flat BD result
/// is only attributable once the fire count is known.
static B128_NONE_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128x128-root count since the last call, and zero it.
pub fn take_b128_none_hits() -> usize {
    B128_NONE_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Rate-distortion optimised quantisation ([`crate::tile::rdoq`], lane-rdoq).
/// Preset-gated: it re-prices the block once per candidate level, so it buys
/// bits with wall (see `crate::speed::RDOQ` for the per-preset table).
///
/// `EC_AV1_RDOQ=0` in any build restores the plain deadzone quantiser --
/// which is what the byte pins are taken under when they name it.
fn rdoq_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_RDOQ").ok().map(|v| v != "0"));
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::RDOQ))
}

/// The rate weight [`crate::tile::rdoq`] trades bits for squared error at, in
/// units of one squared AC quantiser step -- the search's own
/// [`LAMBDA_SCALE`], so the quantiser and the mode search speak one currency.
///
/// The per-frame factors the search multiplies onto it ([`key_lambda_factor`],
/// [`hidden_lambda_factor`], both 1.0 as shipped) and the per-superblock
/// temporal map ([`tpl_lambda_factors`]) are NOT threaded down here:
/// corner-cut, ceiling = a superblock the temporal map weighs away from the
/// frame average has its coefficients optimised at the frame's weight. The
/// upgrade is one `lambda` field on the trial call sites, which is the whole
/// mode-search argument list.
///
/// SWEPT (lane-rdoq, 12-frame `bd_rate_screen_native`, BD vs libaom
/// `cpu-used 6` on the three real rows). The bowl is a factor of TWO above
/// the mode search's own weight, and it is the same multiplier on all three
/// rows:
///
/// | `EC_AV1_RDOQ_LAMBDA` | film A | film B | screen |
/// |---|---|---|---|
/// | 0.7 | +27.6 | +39.5 | +29.6 |
/// | 1.0 | +26.3 | +37.5 | +29.1 |
/// | 1.3 | +25.6 | +37.0 | +28.8 |
/// | 1.6 | +24.6 | +35.9 | +28.7 |
/// | 2.2 (shipped) | +24.4 | +35.5 | +28.1 |
/// | 3.2 | +24.7 | +36.1 | +29.4 |
///
/// A coefficient's rate is worth twice here what the mode search prices it
/// at, and the likeliest reason is that the price is taken against tables
/// that have not adapted yet ([`crate::tile::coeff_bits`]): a block coded
/// late in a frame whose `txb_skip`/`base` tables have narrowed really costs
/// less than the pricer says, and this pass is the one decision taken per
/// COEFFICIENT, so it feels that error most. The honest fix is an adapted
/// price, not a fitted multiplier; this is the fitted multiplier, and its
/// bowl is measured rather than assumed.
const RDOQ_LAMBDA_MULT: f64 = 2.2;

/// [`RDOQ_LAMBDA_MULT`], or what `EC_AV1_RDOQ_LAMBDA` names (the rate-term
/// calibration sweep).
fn rdoq_lambda() -> f64 {
    static MULT: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_RDOQ_LAMBDA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RDOQ_LAMBDA_MULT)
    });
    lambda_scale() * *MULT
}

/// [`B64_ROOT`], or what `EC_AV1_B64` names.
fn b64_root() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_B64").ok().map(|v| v != "0"));
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::B64_ROOT))
}

/// The KEY frame's own 64x64 root (lane-i64): the whole superblock searched
/// as ONE intra block -- every mode the 32x32 search offers, one TX_64X64
/// luma transform (`TxbSet::Luma64`, whose coded quarter is the 32x32 corner)
/// and one TX_32X32 per chroma plane -- and compared against the four 32x32
/// quadrants at the same lambda, exactly the shape the inter root
/// ([`B64_ROOT`]) takes on an inter frame. The writer has always been able to
/// code one (`crate::tile::sb_coeff_key_frame_tile`'s `Superblock::Whole`);
/// nothing produced one, so the key frame -- 21.6% of a real film stream --
/// coded every superblock split while libaom and rav1e leave 45-54% of theirs
/// whole.
///
/// No CfL (spec `is_cfl_allowed` stops at 32x32, and `search_chroma` gates on
/// it), no filter intra (`filter_intra_size_class` has no 64 row) and no
/// transform-depth split (the writer's `Superblock::Whole` arm takes a 32x32
/// level grid, so only depth 0 is representable) -- the root is refused
/// outright on an `allow_intrabc` frame, whose quadrant searches carry DV
/// state a discarded trial would desync.
///
/// Switched off by `EC_AV1_I64=0` in any build.
pub(crate) const I64_ROOT: bool = true;

/// How many key-frame superblocks were coded as one 64x64 intra block since
/// the last [`take_i64_root_hits`], so a gate reports the fire rate rather
/// than assuming it (class `gate-blind-to-feature`).
/// Index 1 is how many were OFFERED the trial at all, so a gate reads a
/// SHARE and not a bare count, and index 2 how many of the whole ones sat on
/// a superblock the true frame edge CUTS THROUGH (lane-corner) -- the
/// `gate-blind-to-feature` guard for the edge path, which no
/// superblock-aligned clip fires at all.
static I64_HITS: [std::sync::atomic::AtomicUsize; 3] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

/// The key-frame `(whole, offered, edge)` 64x64-intra-root counts since the
/// last call, and zero them.
pub fn take_i64_root_hits() -> [usize; 3] {
    std::array::from_fn(|i| I64_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// [`I64_ROOT`], or what `EC_AV1_I64` names.
fn i64_root() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_I64").ok().map(|v| v != "0"));
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::I64_ROOT))
}

/// Whether the 64x64 root also prices a NON-skip candidate (lane-tx64): the
/// winner's prediction through one TX_64X64 luma transform and two TX_32X32
/// chroma ones. Off by `EC_AV1_B64RES=0`, which restores the skip-only root
/// lane-b64 shipped, so the two can be measured against each other on the
/// same build.
///
/// MEASURED on the native gate (two arms of one build, baseline reproduced to
/// the tenth): both film rows improve on both columns -- film A
/// +43.0/+15.0 -> +42.9/+14.8, film B +64.7/+34.0 -> +63.3/+31.5 -- and
/// SCREEN CAPTURE loses +33.4/-23.8 -> +34.3/-23.4, outside the keep rule's
/// +-0.3 screen clause. So the caller gates this off on a screen frame
/// (`allow_screen_content_tools`), the same content gate the coding pyramid
/// takes: a desktop capture's 64x64 superblocks are flat runs whose skip arm
/// already codes them for nothing, and a residual there only spends bits the
/// intra/palette tools would have spent better.
///
/// CENSUS on film B (12 frames, q120, `EC_AV1_BITCENSUS=1` +
/// `examples/syntax_census.rs`, both streams read by OUR decoder), off vs on:
///
/// | | skip-only | + residual |
/// |---|---|---|
/// | 64x64 share, all frames | 36.4% | 38.3% |
/// | 64x64 share, leaf frames | 53.3% | 56.0% |
/// | skip area, all frames | 80.4% | 78.8% |
/// | skip area, leaf frames | 90.2% | 88.5% |
/// | stream bytes | 34,366 | 34,295 |
/// | encoder wall | 13.4s | 9.6s |
///
/// `base_luma_64` enters the leaf frames' top tables at 5.7%, i.e. the
/// TX_64X64 coefficients are really being coded. The root wins MORE often
/// with a residual to offer (rav1e speed 6's own 64x64 share on this clip is
/// 45.2%), which is also why the arm is 28% FASTER despite costing three more
/// transforms per superblock: a root that wins is four quadrant searches that
/// never run.
fn b64_residual() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_B64RES").ok().map(|v| v != "0"));
    ENV.unwrap_or(true) && b64_root()
}

/// How many 64x64 roots came out cheaper with a residual than skipped, since
/// the last [`take_b64_residual_hits`] -- so a gate reports the fire rate
/// rather than assuming one (class `gate-blind-to-feature`).
static B64_RESIDUAL_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The non-skip 64x64-root count since the last call, and zero it.
pub fn take_b64_residual_hits() -> usize {
    B64_RESIDUAL_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether the 64x64 root also prices COMPOUND candidates (lane-b64b): the
/// `LAST` + extra-reference pairs the 32x32 level already offers, at the
/// three modes a root can form without a second 64x64 motion search --
/// `NEAREST_NEARESTMV`, `GLOBAL_GLOBALMV` and `NEW_NEARESTMV` (whose NEW half
/// is the vector `search_skip_64`'s own single-reference search already
/// found). `NEAREST_NEWMV`/`NEW_NEWMV` need the SECOND reference's own
/// `NEWMV` vector, which only a second search per superblock produces -- the
/// wall this root exists to save -- so they are not offered here.
///
/// Off by `EC_AV1_B64COMP=0`, so both arms measure on one build.
fn b64_compound() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B64COMP")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(true) && b64_root()
}

/// How many 64x64 roots took a COMPOUND candidate since the last
/// [`take_b64_compound_hits`] (class `gate-blind-to-feature`).
static B64_COMPOUND_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The compound 64x64-root count since the last call, and zero it.
pub fn take_b64_compound_hits() -> usize {
    B64_COMPOUND_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether a 64x64 root that codes a residual may SPLIT its luma transform
/// (lane-b64b): four TX_32X32 units instead of the one TX_64X64, decided by
/// [`commit_inter_luma`] like every other inter block's var-tx, and coded by
/// the writer's `Whole64` arm as `txfm_split = 1`. It exists because
/// TX_64X64 zeroes everything outside its 32x32 corner, so a root whose
/// residual has detail in the other three quadrants cannot code it at all.
///
/// Off by `EC_AV1_B64VARTX=0`.
fn b64_var_tx() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B64VARTX")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(true) && b64_root()
}

/// How many 64x64 roots split their luma transform since the last
/// [`take_b64_var_tx_hits`].
static B64_VAR_TX_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The split-luma 64x64-root count since the last call, and zero it.
pub fn take_b64_var_tx_hits() -> usize {
    B64_VAR_TX_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether a superblock the true frame edge CUTS THROUGH may be left whole
/// (lane-b64b). AV1 asks only that the block's own half is inside
/// (`has_cols`/`has_rows`, spec `decode_partition`), and the writer already
/// codes that case; what was missing was a trial whose SSE is scored over the
/// INSIDE part alone ([`Plane::block_sse`], which clips since lane-sse)
/// instead of over the padding the encoder
/// invented past the edge. Both gate films are superblock-aligned, so this
/// only reaches the user's own arbitrary export sizes.
///
/// Off by `EC_AV1_B64EDGE=0`.
fn b64_edge() -> bool {
    edge_root_enabled() && b64_root()
}

/// What `EC_AV1_B64EDGE` names on its own, without the inter root's own
/// preset switch -- the key frame's root ([`I64_ROOT`]) reads the same flag
/// but is gated by [`i64_root`].
fn edge_root_enabled() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B64EDGE")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(true)
}

/// How many 64x64 roots the frame edge cut through since the last
/// [`take_b64_edge_hits`] (class `gate-blind-to-feature`: both gate films are
/// superblock-aligned, so nothing but a synthetic size fires this).
static B64_EDGE_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The edge 64x64-root count since the last call, and zero it.
pub fn take_b64_edge_hits() -> usize {
    B64_EDGE_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether the encoder searches loop restoration at all (spec 7.17): one
/// Wiener filter per 64x64 luma restoration unit, picked from
/// [`crate::filter_search::WIENER_CANDIDATES`]. Swept by `EC_AV1_LR` in a
/// test build, like [`lambda_scale`].
pub(crate) const RESTORATION: bool = true;

/// [`RESTORATION`], or what `EC_AV1_LR` names in a test build.
fn restoration_enabled() -> bool {
    match std::env::var("EC_AV1_LR").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::RESTORATION),
    }
}

/// libaom's `partition_search_breakout` shape: how many non-zero luma
/// coefficients per 256 luma samples a block may code and still be judged
/// well enough predicted that the split below it is not worth trying. `0`
/// disables the breakout (only the existing all-skip prune applies), which
/// is the default until the sweep below justifies otherwise.
///
/// Swept on the BD gate (`EC_AV1_SPLIT_BREAKOUT`, test builds only), which
/// is where this lane's wall would have gone: 1 cuts the three clips'
/// encode wall 6.3/6.0/6.2 s -> 3.6/3.2/4.4 s but costs +18.4/+24.0/+7.9
/// BD-rate points against libaom; 2 costs +23.4/+29.4/+18.8 and 4 costs
/// +31.7/+40.6/+27.8, for barely more speed. The split trial is where this
/// encoder's inter quality lives, so `0` ships and the knob stays for a
/// future explicit speed preset.
pub(crate) const SPLIT_BREAKOUT_COEFFS: usize = 0;

/// libaom's `partition_search_breakout_rate_thr` shape, in this encoder's own
/// currency: a 32x32 inter block whose whole-block RD cost per pixel comes
/// out under `threshold * lambda` is not offered the split at all, and a
/// 16x16 leaf under it is not offered the 8x8 split. `0.0` offers both
/// always, as before this lever.
///
/// The census is what points here: the split trial runs on nearly every
/// non-skip block (15132 leaf searches per ladder point on the 1080p clip)
/// and wins 7.6% of the time on film, 3.0% on screen -- yet the coefficient
/// -count breakout ([`SPLIT_BREAKOUT_COEFFS`]) that prunes it costs 18-31 BD
/// points, so the gate has to read the block's own RD cost, not its levels.
///
/// Swept on the BD gate over 0.125/0.25/0.5/1.0 (vs libaom, against a base of
/// +121.1/+146.1/+79.3): 0.125 is +121.2/+146.4/+79.1 (+0.1/+0.3/-0.2) for
/// -10.0%/-5.7% instructions (1080p/screen, `perf stat` ABAB x2) and
/// -7.7/-8.6/-4.5% wall; 0.25 already costs +0.7/+0.6/+0.6, 0.5 costs
/// +3.5/+4.7/+1.8 for -15% wall, and 1.0 costs +19/+27/+6. So 0.125 ships --
/// the largest threshold whose BD stays inside the +0.3 keep rule on every
/// clip.
/// RE-SWEPT on lane-av1rejudge at [`LAMBDA_SCALE`] 0.05 with warp on (base
/// +16.7/-0.5, +47.0/+19.1, +51.3/-14.3): 0.0625 is +16.9/-0.4, +47.0/+19.1,
/// +51.3/-14.2 (the 1080p film 0.2 worse for a finer search) and 0.25 is
/// +16.6/-0.6, +46.9/+19.1, +51.6/-14.5 (screen 0.3 worse vs libaom, 3s off
/// the screen wall). Neither clears the keep rule; 0.125 stays.
pub(crate) const SPLIT_RD_THRESHOLD: f64 = 0.125;

/// [`SPLIT_RD_THRESHOLD`], swept by `EC_AV1_SPLIT_RD` in any build.
fn split_rd_threshold() -> f64 {
    static ENV: std::sync::LazyLock<Option<f64>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_SPLIT_RD")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::SPLIT_RD))
}

/// Whether `cost` over a `side` x `side` block is cheap enough per pixel that
/// [`split_rd_threshold`] withholds the split trial.
fn split_rd_breakout(cost: f64, side: usize, lambda: f64) -> bool {
    split_rd_breakout_at(split_rd_threshold(), cost, side, lambda)
}

/// [`split_rd_breakout`] at an explicit threshold -- what the 64x64 root's own
/// early-out reads.
fn split_rd_breakout_at(t: f64, cost: f64, side: usize, lambda: f64) -> bool {
    t > 0.0 && cost < t * lambda * (side * side) as f64
}

/// The threshold the 64x64 root's early-out uses: [`split_rd_threshold`]
/// CLAMPED at the shipped [`SPLIT_RD_THRESHOLD`] (lane-tx64).
///
/// The 32x32 level is allowed to loosen its breakout with the speed preset
/// (`crate::speed::SPLIT_RD` steps to 0.5 at preset 6), but lane-b64 measured
/// what that does one size up: at 0.5 a whole superblock is taken at 64x64
/// without its four quadrants ever being searched, and film B goes
/// +91.5/+53.4 -> +114.6/+71.5. A 64x64 block is four times the area, so the
/// same per-pixel threshold withholds four times as much search -- the
/// preset's step is priced for the 32 level and does not carry up here.
fn b64_breakout_threshold() -> f64 {
    split_rd_threshold().min(SPLIT_RD_THRESHOLD)
}

/// [`SPLIT_BREAKOUT_COEFFS`], swept by `EC_AV1_SPLIT_BREAKOUT` in a test
/// build.
fn split_breakout_coeffs() -> usize {
    match std::env::var("EC_AV1_SPLIT_BREAKOUT").ok() {
        Some(v) if cfg!(test) => v.parse().unwrap_or(SPLIT_BREAKOUT_COEFFS),
        _ => crate::speed::at(&crate::speed::SPLIT_BREAKOUT),
    }
}

/// How often each partition decision was taken since the last
/// [`take_partition_hits`], so a gate can report how often a new partition
/// fires rather than assume it does (gate-blind-to-feature). Index 0 is an
/// inter 32x32 block left whole, 1 one split into four 16x16 leaves; 2 is
/// such a leaf left whole, 3 one split into four 8x8 leaves of its own.
static PARTITION_HITS: [std::sync::atomic::AtomicUsize; 4] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

/// Records one such decision.
fn partition_hit(which: usize) {
    PARTITION_HITS[which].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Takes (and clears) the counts.
#[cfg(test)]
pub(crate) fn take_partition_hits() -> [usize; 4] {
    [0, 1, 2, 3].map(|i| PARTITION_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Whether the trial-set census (`EC_AV1_CENSUS=1`) is on: how many full
/// rate-distortion trials the block search actually runs, by kind, and where
/// the winner sat in the cheap SAD ranking the pruning levers rank by. Off by
/// default, and a measurement build rather than a speed one -- the census
/// runs the SAD pre-pass the unpruned search otherwise skips.
pub fn census_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| matches!(std::env::var("EC_AV1_CENSUS").as_deref(), Ok("1")))
}

/// What each [`CENSUS`] counter counts.
pub const CENSUS_KINDS: [&str; 11] = [
    "intra blocks searched (key frame + inter leaves)",
    "intra luma mode trials",
    "chroma searches",
    "chroma mode trials (each = 2 plane trials)",
    "tx-depth trials (transform units, depth >= 1)",
    "inter 32x32 blocks searched",
    "inter block intra-mode trials",
    "inter mv candidates (each = 3 mc trials)",
    "inter leaves searched (intra vs NEARESTMV/NEWMV)",
    "  of those, intra won",
    "  of those, the NEARESTMV trial coded no residual",
];

/// The census counters themselves, indexed by [`CENSUS_KINDS`].
static CENSUS: [std::sync::atomic::AtomicUsize; 11] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 11];

/// Where the RD winner sat in the SAD ranking, for the luma intra search and
/// the chroma mode search -- what says whether a top-N prune would have kept
/// the block the search actually chose.
static LUMA_RANK: [std::sync::atomic::AtomicUsize; 13] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 13];
static CHROMA_RANK: [std::sync::atomic::AtomicUsize; 7] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 7];

fn census_add(kind: usize, n: usize) {
    if census_on() {
        CENSUS[kind].fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Reads (and clears) the census, so a driver can attribute it to its own encode.
pub fn take_census() -> ([usize; 11], [usize; 13], [usize; 7]) {
    let take = |c: &std::sync::atomic::AtomicUsize| c.swap(0, std::sync::atomic::Ordering::Relaxed);
    (
        std::array::from_fn(|i| take(&CENSUS[i])),
        std::array::from_fn(|i| take(&LUMA_RANK[i])),
        std::array::from_fn(|i| take(&CHROMA_RANK[i])),
    )
}

/// The rank of `winner` in `scored` ordered by its cheap score, cheapest
/// first -- the census's one shared rule, so the luma and chroma histograms
/// mean the same thing.
fn sad_rank(scored: &[(f64, u8)], winner: u8) -> usize {
    scored
        .iter()
        .filter(|&&(score, mode)| {
            mode != winner
                && score
                    < scored
                        .iter()
                        .find(|&&(_, m)| m == winner)
                        .map_or(f64::INFINITY, |&(s, _)| s)
        })
        .count()
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
    /// The U plane: half the width, and the frame's own chroma height -- a
    /// quarter of the luma samples at 4:2:0, half at 4:2:2, all of them at
    /// 4:4:4. A consumer that only knows the luma size reads the chroma
    /// format off this sample count (see `decode::ref_chroma_shape`).
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
    fctx: &crate::decode::FrameCtx,
) {
    // Three heap allocations per call became three stack buffers: this runs
    // once per mode per trial, and an edge is at most `bw + bh` samples while
    // a block is at most `SUPERBLOCK * SUPERBLOCK` (lane-i64: the key frame's
    // 64x64 intra root predicts through here too, and reads up to `2 * bw`
    // samples above).
    let mut above_buf = [0u16; 2 * SUPERBLOCK];
    let mut left_buf = [0u16; 2 * SUPERBLOCK];
    // lane-av1speed2: a `BLOCK * BLOCK` scratch is 2 KB zeroed on every mode
    // trial of every block, and a 4x4 block writes 32 bytes of it. The
    // predictor fills every sample it is handed, so the zeros are dead
    // either way -- size the scratch to the block instead of to the largest
    // block there is.

    let widen = |src: &[u8], buf: &mut [u16; 2 * SUPERBLOCK]| {
        for (d, &v) in buf[..src.len()].iter_mut().zip(src) {
            *d = u16::from(v);
        }
    };
    if let Some(s) = above {
        widen(s, &mut above_buf);
    }
    if let Some(s) = left {
        widen(s, &mut left_buf);
    }
    let above16 = above.map(|s| &above_buf[..s.len()]);
    let left16 = left.map(|s| &left_buf[..s.len()]);
    let mut run = |dst16: &mut [u16]| {
        crate::intra::predict(
            mode,
            angle_delta,
            above16,
            left16,
            corner.map(u16::from),
            bw,
            bh,
            enable_edge_filter,
            smooth_neighbor,
            dst16,
            fctx,
        );
        for (d, &s) in dst.iter_mut().zip(dst16.iter()) {
            *d = s as u8;
        }
    };
    let n = bw * bh;
    if n <= 64 {
        run(&mut [0u16; 64][..n]);
    } else if n <= 256 {
        run(&mut [0u16; 256][..n]);
    } else if n <= BLOCK * BLOCK {
        run(&mut [0u16; BLOCK * BLOCK][..n]);
    } else {
        run(&mut [0u16; SUPERBLOCK * SUPERBLOCK][..n]);
    }
}

/// [`intra_predict_u8`] for recursive filter intra (spec 7.11.2.3): the same
/// `u8` box around [`crate::intra::predict_filter_intra`], whose `mode` is one
/// of the five `FILTER_INTRA_MODES` rather than an ordinary intra mode.
fn filter_intra_predict_u8(
    mode: usize,
    above: Option<&[u8]>,
    left: Option<&[u8]>,
    corner: Option<u8>,
    side: usize,
    dst: &mut [u8],
    fctx: &crate::decode::FrameCtx,
) {
    let mut above_buf = [0u16; 2 * BLOCK];
    let mut left_buf = [0u16; 2 * BLOCK];
    let widen = |src: &[u8], buf: &mut [u16; 2 * BLOCK]| {
        for (d, &v) in buf[..src.len()].iter_mut().zip(src) {
            *d = u16::from(v);
        }
    };
    if let Some(a) = above {
        widen(a, &mut above_buf);
    }
    if let Some(l) = left {
        widen(l, &mut left_buf);
    }
    let mut dst16 = [0u16; BLOCK * BLOCK];
    let n = side * side;
    crate::intra::predict_filter_intra(
        mode,
        above.map(|a| &above_buf[..a.len()]),
        left.map(|l| &left_buf[..l.len()]),
        corner.map(u16::from),
        side,
        side,
        &mut dst16[..n],
        fctx,
    );
    for (d, &v) in dst.iter_mut().zip(dst16[..n].iter()) {
        *d = v as u8;
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
///
/// The chroma extent is `ROUND_POWER_OF_TWO(size, 1)` -- `(size + 1) / 2`,
/// libaom's `av1_common_plane_width` (`av1/common/av1_common_int.h`) -- so an
/// ODD declared luma size keeps the chroma column a decoder keeps (a 65-wide
/// 4:2:0 frame has 33, not 32). Every even size is unaffected: `(2k+1)/2 ==
/// k`, so this is a no-op for every caller `encode_key_frame`'s even-size
/// check lets through.
pub(crate) fn crop_encoded(encoded: &Encoded, width: usize, height: usize) -> Encoded {
    let reconstruction = &encoded.reconstruction;
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let cropped = if reconstruction.width == width && reconstruction.height == height {
        reconstruction.clone()
    } else {
        Picture {
            width,
            height,
            y: crop_plane(&reconstruction.y, reconstruction.width, width, height),
            u: crop_plane(&reconstruction.u, reconstruction.width / 2, cw, ch),
            v: crop_plane(&reconstruction.v, reconstruction.width / 2, cw, ch),
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
        tx_select: encoded.tx_select,
        switchable_motion_mode: encoded.switchable_motion_mode,
        screen: encoded.screen,
        reduced_tx_set: encoded.reduced_tx_set,
        start_cdfs: encoded.start_cdfs.clone(),
        next_cdfs: encoded.next_cdfs.clone(),
        loop_filter: encoded.loop_filter,
        loop_restoration: encoded.loop_restoration,
        allow_intrabc: encoded.allow_intrabc,
        delta: encoded.delta,
        cdef: encoded.cdef,
    }
}

/// A `Cdfs` inside a `Debug` type: the tables are tens of kilobytes of
/// numbers nobody wants printed, and [`Encoded`] derives `Debug`.
#[derive(Clone)]
pub(crate) struct CdfSnapshot(pub(crate) crate::cdf_state::Cdfs);

impl std::fmt::Debug for CdfSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CdfSnapshot")
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
    /// This frame header's `tx_mode == TxMode::Select` ([`tx_select`], and
    /// [`tx_select_inter`] on an inter frame) — a decoder of `tile` needs it,
    /// and reading it off `stream`'s header is what `decode_stream` does. A
    /// test that decodes `tile` directly must pass this one along, or it reads
    /// a `tx_depth` frame as a `TxMode::Largest` one and desyncs at the first
    /// block.
    pub(crate) tx_select: bool,
    /// This frame header's `is_motion_mode_switchable` (spec 5.9.2) -- the
    /// bit that makes every eligible single-reference inter block carry a
    /// `motion_mode` symbol. Same contract as `tx_select` above: a test
    /// decoding `tile` directly must pass it along or it desyncs at the
    /// first eligible block.
    pub(crate) switchable_motion_mode: bool,
    /// The CDF state this frame's tile writer started from -- the defaults
    /// for a key frame and for the first inter frame, and the previous
    /// frame's stored tables after that. A test that decodes `tile` directly
    /// must feed the decoder this, exactly as `decode_stream` feeds it what
    /// the frame's `primary_ref_frame` slot holds (same class as
    /// `tx_select`: a header bit a raw tile decode cannot guess).
    ///
    /// BOXED (lane-av1stacksweep), and this is the whole point: the payload is
    /// a 15232-byte `Cdfs`, and `Encoded` is returned BY VALUE from a
    /// four-deep `Result<Encoded>` constructor chain, so an inline copy of two
    /// of them made `Encoded` 30728 bytes -- 99.2% of this struct was these
    /// two fields, and it was the largest by-value return in the crate.
    /// `DpbSlot::cdfs` was boxed for the same reason in lane-av1enchang; this
    /// is the other copy of the same table. The VALUES are untouched, so the
    /// bitstream is byte-identical; `crate::stack_budget` gates the result.
    pub(crate) start_cdfs: Box<CdfSnapshot>,
    /// What this frame stores into the slots it refreshes (spec 7.20,
    /// `crate::stream::stored_cdfs_for`): its end-of-tile tables with the
    /// counts reset when `disable_frame_end_update_cdf` is off, and what it
    /// started from when it is on.
    pub(crate) next_cdfs: Box<CdfSnapshot>,
    /// This frame header's chosen deblocking parameters
    /// ([`crate::filter_search::pick_deblock`]) — `reconstruction` is the
    /// picture the decoder produces UNDER them, so a test that decodes
    /// `tile` directly must pass these along, exactly as it must `tx_select`
    /// (class: test asserts against a stale header).
    pub(crate) loop_filter: LoopFilterParams,
    /// This frame header's chosen CDEF parameters, threaded for the same
    /// reason as `loop_filter`.
    pub(crate) cdef: CdefParams,
    /// This frame header's `allow_screen_content_tools`
    /// ([`screen_content`]) -- every intra block of the tile carries the two
    /// palette-mode symbols under it, so a raw tile decode that guessed
    /// `false` would desync at the first block (same class as `tx_select`).
    pub(crate) screen: bool,
    /// This frame header's own `reduced_tx_set` (spec 5.9.2) -- the bit that
    /// picks the inter `tx_type` alphabets AND, on an inter block, the
    /// allowance the chroma transform's inherited type reduces against
    /// ([`crate::decode::reduce_inherited_chroma_tx_type`]). A raw tile
    /// decode that guesses `true` here mis-derives every inherited chroma
    /// type on a `reduced_tx_set = 0` frame (lane-intertx; same class as
    /// `tx_select`/`screen` above).
    pub(crate) reduced_tx_set: bool,
    /// This frame header's chosen loop restoration parameters, threaded for
    /// the same reason as `loop_filter` -- and doubly so: the per-unit
    /// filters themselves are coded in `tile`, so a decode of it under a
    /// stale `RESTORE_NONE` header desyncs at the first superblock.
    pub(crate) loop_restoration: LoopRestorationParams,
    /// This frame header's `allow_intrabc` -- every intra block of the tile
    /// carries a `use_intrabc` symbol under it, so a raw tile decode that
    /// guessed `false` would desync at the first block (same class as
    /// `screen`/`tx_select`).
    pub(crate) allow_intrabc: bool,
    /// This frame header's `delta_q_params`/`delta_lf_params` (spec
    /// 5.9.17/5.9.18) -- with `delta_q_present` every superblock's first
    /// coded block carries a `delta_qindex` symbol group, so a raw tile
    /// decode that guessed the defaults desyncs at the first superblock
    /// (same class as `screen`/`tx_select`).
    pub(crate) delta: ec_av1_syntax::DeltaParams,
}

thread_local! {
    /// The share of 16x16 source blocks [`intrabc_worth_it`] found a repeat
    /// for on the last key frame encoded on this thread -- read by the gates,
    /// which print it beside the block counts (class `gate-blind-to-feature`).
    static IBC_SHARE: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// [`IBC_SHARE`] for the last key frame encoded on this thread.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intrabc_source_share() -> f64 {
    IBC_SHARE.with(std::cell::Cell::get)
}

thread_local! {
    /// This SEQUENCE's `seq_force_screen_content_tools`: whether the sequence
    /// header offers the per-frame `allow_screen_content_tools` bit at all.
    /// Armed once by the key frame ([`encode_key_frame_inner`], from
    /// [`screen_content`]) and read by every header builder after it, so that
    /// the inter frames of a screen sequence lay their headers out the same
    /// way the sequence header the key frame wrote says they do -- a per-call
    /// parameter would have to thread through `inter_frame_headers`' four
    /// public entry points and every hand-built-stream test that calls them.
    /// A sequence with no screen content leaves this false and writes exactly
    /// the bits it wrote before this lane.
    static SEQ_SCREEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// Overrides [`screen_content`] for the calling thread, which is what the
    /// intra-MODE ablations use: on a two-colour synthetic picture the palette
    /// codes every block losslessly, so both arms of a mode ablation reach
    /// infinite PSNR and the ablation measures nothing at all (the
    /// `fixture-proves-the-symbol-not-the-signal` class). An environment
    /// variable cannot do this -- the tests run in one process, in parallel.
    static SCREEN_FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    /// What the last [`encode_sequence_with_ctx`] call on this thread coded
    /// under, AFTER the content gate: the pyramid, or `None` for a stream
    /// that stayed flat. Read by the BD gates, which print the requested and
    /// the effective pyramid side by side -- a screen clip requests one and
    /// codes flat, and that difference is the gate (class
    /// `gate-blind-to-feature`).
    static LAST_SEQ_PYRAMID: std::cell::Cell<Option<crate::encoder::Pyramid>> =
        const { std::cell::Cell::new(None) };
}

/// The pyramid the last sequence coded on this thread was EFFECTIVELY under.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn last_sequence_pyramid() -> Option<crate::encoder::Pyramid> {
    LAST_SEQ_PYRAMID.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn force_screen(value: Option<bool>) {
    SCREEN_FORCE.with(|c| c.set(value));
}

// lane-fintra: [`force_screen`] for the filter-intra candidate, so a MODE
// ablation can measure the mode search alone. Setting it also clears the
// sequence header's own `enable_filter_intra` bit, so the stream the arm
// writes is exactly the one this encoder wrote before the lane.
#[cfg(test)]
thread_local! {
    static FILTER_INTRA_FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn force_filter_intra(value: Option<bool>) {
    FILTER_INTRA_FORCE.with(|c| c.set(value));
}

pub(crate) fn arm_seq_screen(on: bool) {
    SEQ_SCREEN.with(|c| c.set(on));
}

fn seq_screen() -> bool {
    SEQ_SCREEN.with(std::cell::Cell::get)
}

/// How many distinct colours a 16x16 luma block may hold and still count as
/// screen content ([`screen_content`]), `EC_AV1_SCREEN_COLORS`. libaom's own
/// `av1_set_screen_content_options` bound is 4, which separates nothing on
/// lossily coded (so noisy) material: `probe_screen_detect` sweeps N against
/// the variance floor below on the five gate clips at BOTH the native crop
/// and the 640x384 gate's scale, and 16 colours / var > 16 / an eighth of
/// the frame is the widest-margin split of those ten rows -- every
/// non-screen row at most 9.6% of blocks (bars 3.6/2.3 native, 9.6/9.5
/// scaled; film A 0.6/0.3, film B 0.5/0.9) against the OBS capture's 23.6%
/// native and 17.9% scaled.
fn screen_colors() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_SCREEN_COLORS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16)
    })
}

/// The reciprocal of the frame area those blocks must cover, in tenths --
/// libaom's own `counts * blk_h * blk_w * 10 > width * height` is a tenth;
/// 8 (an eighth) is the midpoint of the two populations
/// `probe_screen_detect` measures at the colour bound and variance floor
/// around it. `EC_AV1_SCREEN_PCT`.
fn screen_pct() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_SCREEN_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
    })
}

/// The per-pixel variance a few-colour block must clear before it counts as
/// screen content, `EC_AV1_SCREEN_VAR`. libaom keeps the same term (its
/// `counts_2`/`var_thresh` in `av1_set_screen_content_options`); without it
/// a smooth dark film frame is all few-colour blocks and reads as a desktop
/// capture -- which is exactly what this detector did until 2026-09-07, when
/// it called BOTH real films screen content on 48/48 frames, priced every
/// coefficient against the default CDFs and ran the palette search over
/// film. `probe_screen_detect`: at 16 colours the floor takes both films
/// from 46.2/52.5% of blocks to 0.6/0.5% and the capture only from 29.9% to
/// 23.6%.
fn screen_var() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_SCREEN_VAR")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16)
    })
}

/// How many frames [`screen_content`] said yes/no to, so a gate can print
/// whether the detector fired at all (class `gate-blind-to-feature`).
pub(crate) static SCREEN_FRAMES: [std::sync::atomic::AtomicUsize; 2] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 2];

/// `av1_set_screen_content_options` (av1/encoder/encoder.c), luma only: count
/// the distinct colours of every 16x16 block and turn the screen-content
/// tools on when the blocks holding between 2 and [`screen_colors`] of them
/// AND clearing [`screen_var`] cover more than a [`screen_pct`]th of the
/// frame. `width` is the plane's
/// stride, `true_*` the frame's real (decodable) extent.
fn screen_content(y: &[u8], width: usize, true_width: usize, true_height: usize) -> bool {
    if let Some(forced) = SCREEN_FORCE.with(std::cell::Cell::get) {
        SCREEN_FRAMES[usize::from(forced)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return forced;
    }
    if let Ok(v) = std::env::var("EC_AV1_SCREEN") {
        let on = v != "0";
        SCREEN_FRAMES[usize::from(on)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return on;
    }
    let (blk, limit, floor) = (16usize, screen_colors(), screen_var());
    let mut counted = 0usize;
    for by in (0..true_height).step_by(blk) {
        for bx in (0..true_width).step_by(blk) {
            let (h, w) = ((true_height - by).min(blk), (true_width - bx).min(blk));
            let mut seen = [false; 256];
            let (mut colors, mut sum, mut sq) = (0usize, 0u64, 0u64);
            for row in 0..h {
                for col in 0..w {
                    let v = y[(by + row) * width + bx + col];
                    sum += u64::from(v);
                    sq += u64::from(v) * u64::from(v);
                    if !seen[usize::from(v)] {
                        seen[usize::from(v)] = true;
                        colors += 1;
                    }
                }
            }
            // Per-pixel variance over the block's real (edge-clipped) extent.
            let n = (h * w) as u64;
            let var = (sq - sum * sum / n) / n;
            if colors > 1 && colors <= limit && var > floor {
                counted += 1;
            }
        }
    }
    let on = counted * blk * blk * screen_pct() > true_width * true_height;
    SCREEN_FRAMES[usize::from(on)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    on
}

/// Whether this SOURCE picture reads as screen content, for a caller that has
/// to decide something BEFORE the frame is coded: [`crate::encoder::Av1Encoder`]
/// gates its coding pyramid on this (a desktop capture keeps the flat
/// one-in-one-out structure the BD gate measured it on, camera material takes
/// the mini-GOP). The key frame runs the same detector again when it builds
/// its header -- that is the run the [`SCREEN_FRAMES`] histogram is meant to
/// count, so this probe undoes its own tick and leaves the gate prints
/// reading one detection per coded frame.
pub(crate) fn picture_is_screen(picture: &Picture) -> bool {
    let y: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let on = screen_content(&y, picture.width, picture.width, picture.height);
    SCREEN_FRAMES[usize::from(on)].fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    on
}

/// The sequence and frame headers a picture of this size is coded under: one
/// tile, one transform size per block, no in-loop filter parameters yet
/// (`crate::encode::pick_and_apply_filters` fills the deblocking levels and
/// CDEF strengths in once the tile is coded), no CDF adaptation
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
/// `color_config` also carries the chroma shape, and that is what a coverage
/// witness needs: nothing in this crate's ENCODER codes a shape other than
/// 4:2:0, but a sequence header can still be written at any
/// `subsampling_x`/`subsampling_y` the spec allows, which is how the 4:4:0
/// header witness is built. It builds HEADERS only -- pair the pair it
/// returns with a tile from [`encode_key_frame`] (4:2:0) or
/// [`encode_key_frame_at_size`].
///
/// # Errors
/// The same as [`key_frame_headers`].
pub fn key_frame_headers_colour(
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
        use_128x128_superblock: sb128_on(),
        // lane-fintra: the encoder's intra search offers the five recursive
        // filter-intra modes on eligible blocks, so the sequence bit is set
        // whenever that search is on (`EC_AV1_FILTER_INTRA=0` switches both
        // off and restores the streams this encoder wrote before the lane).
        enable_filter_intra: filter_intra_on(),
        enable_intra_edge_filter: false,
        enable_order_hint: true,
        order_hint_bits: 7,
        enable_superres: false,
        // spec 5.9.19: with the sequence bit set every frame header carries
        // `cdef_params`. This encoder writes `cdef_bits == 0` -- one strength
        // pair for the whole frame, chosen by `crate::filter_search` -- so no
        // `cdef_idx` literal is coded in the tile and a frame that wants no
        // CDEF simply carries zero strengths.
        enable_cdef: true,
        enable_restoration: true,
        // SELECT (2) when this sequence's key frame detected screen content
        // ([`screen_content`]): the per-frame `allow_screen_content_tools` bit
        // is then coded in every frame header, and only the frames that want
        // the palette syntax set it. 0 -- the forced-off form, no bit at all
        // -- otherwise, which is what every non-screen stream this encoder
        // writes still carries.
        seq_force_screen_content_tools: if seq_screen() {
            ec_av1_syntax::sequence::SELECT_SCREEN_CONTENT_TOOLS
        } else {
            0
        },
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
        // lane-av1obmc2: the sequence bit `allow_warped_motion` is gated on
        // (spec 5.5.2). Set only under the warp knob, so the DEFAULT sequence
        // header is byte-identical to the streams before this lane.
        enable_warped_motion: warp_on(),
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

/// The DPB slot `GOLDEN_FRAME` names in every frame this encoder writes: the
/// key frame refreshes all eight slots, each inter frame only slot 0, so slot
/// 1 still holds the key frame however long the GOP runs.
pub const GOLDEN_SLOT: u8 = 1;

/// lane-refs: the frame-level `interpolation_filter` (spec 6.8.9) every inter
/// frame this encoder writes carries. `EC_AV1_INTERP=smooth|sharp` codes the
/// EIGHTTAP_SMOOTH / EIGHTTAP_SHARP kernel instead of the default REGULAR --
/// one filter for the whole frame, which is what libaom does on this content
/// too (`lanes/obmc3.report.md` read `switchable_interp 0/0` out of its own
/// stream, so a per-BLOCK filter search has no bits to win). The kernel
/// reaches the search, the trials and the recon through
/// `FrameCtx::interp_filter`, so the stream a decoder reads and the
/// prediction the search priced are the same one.
/// A PROCESS-GLOBAL [`frame_interp_filter`] override that wins over the
/// environment (`u8::MAX` = unset), the shape [`set_inter_tx_search`] has:
/// the witness that has to see a non-REGULAR kernel coded and decoded turns
/// it on through this while holding [`crate::speed::knob_write`].
static INTERP_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`INTERP_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_frame_interp(filter: Option<ec_av1_syntax::InterpolationFilter>) {
    INTERP_OVERRIDE.store(
        filter.map_or(u8::MAX, |f| f as u8),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn frame_interp_filter() -> ec_av1_syntax::InterpolationFilter {
    use ec_av1_syntax::InterpolationFilter as F;
    match INTERP_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return F::Eighttap,
        1 => return F::EighttapSmooth,
        2 => return F::EighttapSharp,
        _ => {}
    }
    static ENV: std::sync::LazyLock<Option<F>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_INTERP")
            .ok()
            .map(|v| match v.as_str() {
                "smooth" => F::EighttapSmooth,
                "sharp" => F::EighttapSharp,
                _ => F::Eighttap,
            })
    });
    ENV.unwrap_or(F::Eighttap)
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
    inter_frame_headers_slots(
        width, height, base_q_idx, order_hint, last_slot, last_slot, last_slot,
    )
}

/// [`inter_frame_headers`] with the three slots named apart: the one read as
/// `LAST_FRAME`, the one this frame refreshes, and the one read as
/// `ALTREF_FRAME`. The encoder alternates the refreshed slot between 0 and 2
/// so that the frame two back is still intact in the slot this one is about
/// to overwrite -- read before write, spec 7.20's decode order.
///
/// # Errors
/// As [`inter_frame_headers`].
pub fn inter_frame_headers_slots(
    width: usize,
    height: usize,
    base_q_idx: u8,
    order_hint: u32,
    last_slot: u8,
    self_slot: u8,
    altref_slot: u8,
) -> Result<(SequenceHeader, FrameHeader)> {
    let (seq, key) = key_frame_headers(width, height, base_q_idx)?;
    let header = FrameHeader {
        frame_type: FrameType::Inter,
        frame_is_intra: false,
        show_frame: true,
        error_resilient_mode: false,
        disable_cdf_update: false,
        // The tile writer hands its end-of-tile tables to the next frame
        // (`encode_sequence_with_ctx`), so the frame stores them the way the
        // decoder will load them (spec 7.20).
        disable_frame_end_update_cdf: false,
        force_integer_mv: false,
        order_hint,
        primary_ref_frame: 0,
        refresh_frame_flags: 1 << self_slot,
        // Every reference but `GOLDEN_FRAME` (index 3 of `ref_frame_idx`)
        // names the slot this frame both predicts from and refreshes;
        // GOLDEN names slot 1, which the key frame wrote when it refreshed
        // all eight and which no inter frame ever overwrites, so it holds
        // the key frame's own picture for the whole GOP.
        ref_frame_idx: {
            let mut idx = [last_slot; ec_av1_syntax::REFS_PER_FRAME];
            idx[3] = GOLDEN_SLOT;
            idx[6] = altref_slot;
            idx
        },
        // lane-hpmv: libaom's own rule (`av1_pick_and_set_high_precision_mv`:
        // `use_hp = qindex < HIGH_PRECISION_MV_QTHRESH`), with this crate's
        // threshold in [`HP_MV_QTHRESH`]. `force_integer_mv` is false in
        // every frame this constructor builds, so no further guard is needed.
        allow_high_precision_mv: high_precision_mv_at(base_q_idx),
        // A FIXED frame-level filter, so no block codes an `interp_filter`
        // symbol. lane-obmc3 instrumented whether that costs anything: a
        // libaom `cpu-used 6` crf 41 stream of the long-GOP film B window
        // (45941 B, the same point `lanes/libcen.report.md` matched) read
        // back through `EC_AV1_BITCENSUS=1 syntax_census` codes
        // `switchable_interp 0/0` in EVERY frame -- libaom picks a fixed
        // frame filter here too, so there are ZERO per-block filter bits at
        // stake on this content and the per-block switchable search was not
        // built (the charter's premise that libaom codes switchable interp
        // per block on film B is refuted by its own stream).
        interpolation_filter: ec_av1_syntax::InterpolationFilter::Eighttap,
        // lane-av1obmc: with this bit set every single-reference inter block
        // libaom's `motion_mode_allowed` accepts carries a `motion_mode`
        // symbol (`crate::tile::write_motion_mode`); `allow_warped_motion`
        // stays false, so the alphabet is the 2-symbol `obmc_cdf`. It is tied
        // to the OBMC knob rather than always on: with every block choosing
        // SIMPLE_TRANSLATION the symbol is pure cost (+0.07% / +0.02% on the
        // gate clip), so the DEFAULT path writes no motion_mode syntax at all
        // and stays byte-identical to the streams before this lane.
        // lane-av1obmc2: the warp knob joins that disjunction, and turns on
        // `allow_warped_motion` besides -- which is what makes an eligible
        // block read the 3-symbol `motion_mode_cdf` alphabet instead of the
        // 2-symbol `obmc_cdf` one (libaom `motion_mode_allowed`).
        is_motion_mode_switchable: crate::envflags::env_flag!("EC_AV1_OBMC") || warp_on(),
        allow_warped_motion: warp_on(),
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

mod coef;
pub use coef::*;

mod split;
pub(crate) use split::*;

mod inter_search;
pub use inter_search::*;

mod frame;
pub use frame::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod more_tests;
