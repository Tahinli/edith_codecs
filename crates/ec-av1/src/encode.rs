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
use crate::transform::{
    TxType, dequant_and_inverse_typed_wh, forward_and_quantize,
    forward_and_quantize_typed,
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
    match std::env::var("EC_AV1_LAMBDA_KEY").ok().and_then(|v| v.parse::<f64>().ok()) {
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

/// [`B64_ROOT`], or what `EC_AV1_B64` names.
fn b64_root() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B64").ok().map(|v| v != "0")
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::B64_ROOT))
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
fn b64_residual() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_B64RES").ok().map(|v| v != "0")
    });
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
        crate::envflags::var("EC_AV1_SPLIT_RD").ok().and_then(|v| v.parse().ok())
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
    dst: &mut [u8], fctx: &crate::decode::FrameCtx,
) {
    // Three heap allocations per call became three stack buffers: this runs
    // once per mode per trial, and an edge is at most `bw + bh` samples while
    // a block is at most `BLOCK * BLOCK`. Same samples either way.
    let mut above_buf = [0u16; 2 * BLOCK];
    let mut left_buf = [0u16; 2 * BLOCK];
    // lane-av1speed2: a `BLOCK * BLOCK` scratch is 2 KB zeroed on every mode
    // trial of every block, and a 4x4 block writes 32 bytes of it. The
    // predictor fills every sample it is handed, so the zeros are dead
    // either way -- size the scratch to the block instead of to the largest
    // block there is.

    let widen = |src: &[u8], buf: &mut [u16; 2 * BLOCK]| {
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
            dst16, fctx,
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
    } else {
        run(&mut [0u16; BLOCK * BLOCK][..n]);
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
    dst: &mut [u8], fctx: &crate::decode::FrameCtx,
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
        &mut dst16[..n], fctx,
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
        tx_select: encoded.tx_select,
        switchable_motion_mode: encoded.switchable_motion_mode,
        screen: encoded.screen,
        start_cdfs: encoded.start_cdfs.clone(),
        next_cdfs: encoded.next_cdfs.clone(),
        loop_filter: encoded.loop_filter,
        loop_restoration: encoded.loop_restoration,
        allow_intrabc: encoded.allow_intrabc,
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
    pub(crate) start_cdfs: CdfSnapshot,
    /// What this frame stores into the slots it refreshes (spec 7.20,
    /// `crate::stream::stored_cdfs_for`): its end-of-tile tables with the
    /// counts reset when `disable_frame_end_update_cdf` is off, and what it
    /// started from when it is on.
    pub(crate) next_cdfs: CdfSnapshot,
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
    inter_frame_headers_slots(width, height, base_q_idx, order_hint, last_slot, last_slot, last_slot)
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
        allow_high_precision_mv: false,
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
        is_motion_mode_switchable: crate::envflags::env_flag!("EC_AV1_OBMC")
            || warp_on(),
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

/// One plane of the picture being coded, and the reconstruction being built
/// beside it.
/// What one committed 4x4 cell left behind for the coefficient contexts of
/// the blocks that read it as a neighbour -- tile.rs `neighbour_state`, which
/// is the same state decode.rs records.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CoefCtx {
    /// spec `cul_level`: the sum of the unit's coded magnitudes, clamped to 7.
    level: u8,
    /// The DC sign this cell votes with: `-1` negative, `1` positive, `0` when
    /// the DC itself was zero (tile.rs `dc_vote`).
    dc: i8,
}

/// One plane's coefficient contexts, committed in CODING ORDER: the search
/// prices partition TREES, so the neighbour state a candidate reads has to be
/// whatever the blocks coded before it left -- the parent's committed
/// neighbours for a tree's first child, the just-committed sibling for the
/// later ones. Keeping the map as a full per-4x4-cell grid rather than the
/// writer's two running bands is what makes that fall out: a trial commits
/// over its own rectangle ([`Plane::commit`]) and a losing trial is undone
/// with the pixels ([`Plane::restore`]), so what a candidate reads above and
/// to its left is exactly what the tile writer's `Neighbours` bands will hold
/// when it gets there.
///
/// Before this map the pricer took both contexts as zero, which is exact for
/// a luma transform covering its whole block and the largest remaining row of
/// the pricer census everywhere else (all-zero chroma under-priced 41-81%).
#[derive(Default)]
struct CoefCtxMap {
    cells: Vec<CoefCtx>,
    stride: usize,
    /// Whether the transforms being priced right now are SMALLER than the
    /// block they sit in -- a `TxMode::Select` depth split
    /// ([`Plane::code_tx_depth`]) or an inter var-tx split
    /// ([`commit_inter_luma`]). That is the one thing the (x, y, side) of a
    /// trial cannot tell, and it is what decides whether luma reads context 0
    /// or `SKIP_CONTEXTS` (tile.rs `write_luma_tus`).
    tu_split: bool,
}

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
    /// This plane's own tile's pixel bounds, in this plane's units -- the
    /// encoder side of the decoder's `PlaneBuf::set_tile_origin`: intra
    /// prediction never reaches across a tile edge even though the
    /// neighbouring tile's real samples sit right there in the shared
    /// reconstruction. `(0, 0, width, height)` is the single-tile case and
    /// leaves every edge exactly as it was before tiles existed.
    tile_x0: usize,
    tile_y0: usize,
    tile_x1: usize,
    tile_y1: usize,
    /// [`CoefCtxMap`]: what the blocks already committed left for the
    /// coefficient contexts of the ones still being priced. Sized on the
    /// first commit, so a plane literal does not have to know its own extent
    /// twice.
    ctx: CoefCtxMap,
}

impl Plane<'_> {
    /// Clips this plane's edge reads to one tile ([`Self::tile_x0`]),
    /// called at every superblock of the frame-raster search with the rect
    /// of the tile that superblock belongs to.
    fn set_tile(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        self.tile_x0 = x0;
        self.tile_y0 = y0;
        self.tile_x1 = x1.min(self.width);
        self.tile_y1 = y1.min(self.height);
    }
    /// The reconstructed samples a block at `(x, y)` predicts from: the row
    /// above it, the column to its left and the sample between them, each
    /// missing where the block sits against an edge of the frame.
    ///
    /// `reach` says whether the samples above the block's right and below its
    /// left are decoded, which is what a directional mode reads into; where
    /// they are not, or where the frame ends first, the edge is shorter and
    /// the predictor repeats its last sample, exactly as the decoder's clamp
    /// to `aboveLimit` and `leftLimit` does (spec 7.11.2.2).
    /// lane-av1speed2: the two edges used to be a `Vec` each -- a malloc and
    /// a free per mode per block, on the search's hottest path -- while an
    /// edge is at most `2 * side` samples, which is `2 * BLOCK`. Same
    /// samples, on the caller's stack.
    fn edges_into<'a>(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
        above_buf: &'a mut [u8; 2 * BLOCK],
        left_buf: &'a mut [u8; 2 * BLOCK],
    ) -> (Option<&'a [u8]>, Option<&'a [u8]>, Option<u8>) {
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
        let across = across.min(self.tile_x1);
        let down = down.min(self.tile_y1);
        let above_n = (y > self.tile_y0 && across > x).then(|| across - x);
        if let Some(n) = above_n {
            above_buf[..n]
                .copy_from_slice(&self.reconstruction[(y - 1) * self.width + x..][..n]);
        }
        let left_n = (x > self.tile_x0 && down > y).then(|| down - y);
        if let Some(n) = left_n {
            for (d, row) in left_buf[..n].iter_mut().zip(y..down) {
                *d = self.reconstruction[row * self.width + x - 1];
            }
        }
        let corner = (x > self.tile_x0 && y > self.tile_y0)
            .then(|| self.reconstruction[(y - 1) * self.width + x - 1]);
        (
            above_n.map(|n| &above_buf[..n]),
            left_n.map(|n| &left_buf[..n]),
            corner,
        )
    }

    /// Codes one block under one mode without committing it: hands back the
    /// levels, the block the decoder would reconstruct, the squared error
    /// against the source and an estimate of what the levels cost in bits.
    fn trial(&self, at: At, mode: u8, angle_delta: i32, base_q_idx: u8, deadzone: f64, fctx: &crate::decode::FrameCtx) -> Trial {
        self.trial_typed(at, mode, angle_delta, base_q_idx, deadzone, TxType::DctDct, fctx)
    }

    /// [`Self::trial`] under a named transform type -- what an intra chroma
    /// block gets, since chroma codes no `tx_type` symbol and the decoder
    /// derives the type from the chroma mode itself (`Intra_Mode_To_Tx_Type`,
    /// spec 9.3, [`crate::decode::default_intra_tx_type`]). Luma keeps
    /// `DCT_DCT`, which is the one type this writer's `tx_type` symbol names.
    #[allow(clippy::too_many_arguments)]
    /// Sum of absolute differences between the source block at `(x, y)` and a
    /// `side x side` prediction, as the whole number it is.
    ///
    /// Bit-identical to the `f64` accumulation it replaces: every partial sum
    /// is an integer below `255 * 64 * 64` (under 2^21), which `f64` holds
    /// exactly, so no add here rounds and the order of the adds cannot change
    /// the result. Dropping the per-pixel `i / side` and `i % side` -- an
    /// integer division per sample -- is what lets the row walk vectorize.
    fn block_sad(&self, x: usize, y: usize, side: usize, prediction: &[u8]) -> f64 {
        let mut sad = 0u32;
        for row in 0..side {
            let source = &self.source[(y + row) * self.width + x..][..side];
            for (&s, &p) in source.iter().zip(&prediction[row * side..][..side]) {
                sad += u32::from(s.abs_diff(p));
            }
        }
        f64::from(sad)
    }

    /// Squared error between the source block at `(x, y)` and a reconstruction
    /// of it. Exact for the same reason [`Self::block_sad`] is: the largest
    /// total is `255^2 * 64 * 64`, under 2^28.
    fn block_sse(&self, x: usize, y: usize, side: usize, reconstruction: &[u8]) -> f64 {
        let mut sse = 0u64;
        for row in 0..side {
            let source = &self.source[(y + row) * self.width + x..][..side];
            for (&s, &r) in source.iter().zip(&reconstruction[row * side..][..side]) {
                let d = u32::from(s.abs_diff(r));
                sse += u64::from(d * d);
            }
        }
        sse as f64
    }

    fn trial_typed(
        &self,
        at: At,
        mode: u8,
        // This block's `angle_delta_y`/`angle_delta_uv` (spec
        // `read_intra_angle_info`, -3..=3), zero for every non-directional
        // mode -- the predictor's own `angle_delta` argument.
        angle_delta: i32,
        base_q_idx: u8,
        deadzone: f64,
        tx_type: TxType,
        fctx: &crate::decode::FrameCtx,
    ) -> Trial {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * BLOCK], [0u8; 2 * BLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        // Per-candidate scratch on the search's hottest path, so both buffers
        // live on the stack -- `side` is at most `BLOCK` -- rather than
        // costing a malloc/free pair per trial (`mc_trial` already does this).
        let mut prediction = [0u8; BLOCK * BLOCK];
        let prediction = &mut prediction[..side * side];
        intra_predict_u8(
            mode,
            angle_delta,
            above,
            left,
            corner,
            side,
            side,
            false,
            false,
            prediction, fctx,
        );

        let mut residual = [0i32; BLOCK * BLOCK];
        let residual = &mut residual[..side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        #[cfg(test)]
        let t = stage_start();
        let levels =
            forward_and_quantize_typed(residual, side, 8, i32::from(base_q_idx), deadzone, tx_type);
        // lane-av1speed2: 60% of the transform units of an inter stream carry
        // no coefficient at all, and the square entry point re-inflates that
        // case into `side * side` zeros (one allocate-and-memset per trial)
        // only for the sum below to add them to the prediction. The `_wh`
        // entry hands back the empty all-zero marker instead, and a zero
        // residual reconstructs to the prediction itself -- `clamp(p + 0)` is
        // `p` for a `u8`, so the bytes are the same.
        let coded = dequant_and_inverse_typed_wh(
            &levels, side, side, 8, i32::from(base_q_idx), 0, 0, tx_type,
        );
        #[cfg(test)]
        stage_since(2, t);

        let reconstruction: Vec<u8> = if coded.is_empty() {
            prediction.to_vec()
        } else {
            prediction
                .iter()
                .zip(&coded)
                .map(|(&p, &c)| (i32::from(p) + c).clamp(0, 255) as u8)
                .collect()
        };
        let sse = self.block_sse(x, y, side, &reconstruction);
        // What the levels cost is priced through the same CDFs the tile writer
        // will code them with, so the search ranks modes -- and the partition
        // trial ranks trees -- by the bits they actually spend.
        #[cfg(test)]
        let t = stage_start();
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
            {
                let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, at.set);
                crate::tile::coeff_bits(
                    &levels,
                    at.set,
                    crate::decode::q_ctx_of(base_q_idx),
                    skip_ctx,
                    sign_ctx,
                )
            }
        };
        #[cfg(test)]
        stage_since(3, t);
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
            let sse = self.block_sse(x, y, side, prediction);
            return Trial {
                levels: vec![0i32; side * side],
                reconstruction: prediction.to_vec(),
                sse,
                bits: 0.0,
            };
        }
        // A 64x64 root's residual is four times the scratch every other
        // block coder here sizes for [`BLOCK`]; only that one case pays for
        // a heap buffer, so the 4x4..32x32 path keeps its stack array.
        let mut small = [0i32; BLOCK * BLOCK];
        let mut big = if side > BLOCK {
            vec![0i32; side * side]
        } else {
            Vec::new()
        };
        let residual: &mut [i32] = if side > BLOCK {
            &mut big
        } else {
            &mut small[..side * side]
        };
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        #[cfg(test)]
        let t = stage_start();
        let levels = forward_and_quantize(residual, side, 8, i32::from(base_q_idx), deadzone);
        // The same all-zero shortcut [`Self::trial_typed`] takes.
        let coded = dequant_and_inverse_typed_wh(
            &levels,
            side,
            side,
            8,
            i32::from(base_q_idx),
            0,
            0,
            TxType::DctDct,
        );
        #[cfg(test)]
        stage_since(2, t);
        let reconstruction: Vec<u8> = if coded.is_empty() {
            prediction.to_vec()
        } else {
            prediction
                .iter()
                .zip(&coded)
                .map(|(&p, &c)| (i32::from(p) + c).clamp(0, 255) as u8)
                .collect()
        };
        let sse = self.block_sse(x, y, side, &reconstruction);
        // A 64-point transform codes only its top-left 32x32 corner (spec
        // 5.11.39's zero-out; `TxbSet::Luma64`'s tables are 32-sided and the
        // key frame writer takes a 32x32 level grid for one). The inverse
        // above reads the whole 64x64 array, but the price below and the
        // levels the writer takes are that corner.
        let levels = if side > BLOCK {
            coded_corner(&levels, side, BLOCK)
        } else {
            levels
        };
        #[cfg(test)]
        let t = stage_start();
        let bits = if cfg!(test) && std::env::var_os("EC_AV1_ESTIMATE").is_some() {
            levels
                .iter()
                .filter(|&&level| level != 0)
                .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
                .sum()
        } else {
            {
                let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, set);
                crate::tile::coeff_bits(
                    &levels,
                    set,
                    crate::decode::q_ctx_of(base_q_idx),
                    skip_ctx,
                    sign_ctx,
                )
            }
        };
        #[cfg(test)]
        stage_since(3, t);
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// Codes this block's luma at transform depth `depth`: the
    /// `(side >> depth)^2` transform units in raster order, each predicted
    /// from the reconstruction the units before it left behind -- exactly
    /// what the decoder's multi-transform-unit branch does (decode.rs
    /// `decode_block`, `tu_reach`) -- and each transformed, quantised and
    /// reconstructed on its own. Commits the result and hands back the
    /// block-coordinate levels, the block's squared error and what its
    /// coefficients cost.
    #[allow(clippy::too_many_arguments)]
    fn code_tx_depth(
        &mut self,
        at: At,
        mode: u8,
        angle_delta: i32,
        depth: usize,
        // This block's `filter_intra_mode` when it is coded with one, in
        // which case every transform unit predicts recursively off the
        // reconstruction the units before it left -- what the decoder does
        // per unit (decode.rs `decode_block`, `push_intra_rect`'s
        // `filter_intra`), not once over the whole block.
        filter_intra: Option<u8>,
        search: &Search, fctx: &crate::decode::FrameCtx,
    ) -> (Vec<i32>, f64, f64) {
        let At { x, y, side, reach, .. } = at;
        let tx = side >> depth;
        let n = 1usize << depth;
        let set = match tx {
            32 => TxbSet::Luma32,
            16 => TxbSet::Luma16,
            8 => TxbSet::Luma8,
            _ => TxbSet::Luma4,
        };
        let mut levels = vec![0i32; side * side];
        census_add(4, n * n);
        // A unit smaller than its block reads the neighbour magnitude table
        // for its `txb_skip` context, not context 0 ([`Plane::coef_ctx`]).
        self.ctx.tu_split = n > 1;
        let (mut sse, mut bits) = (0.0, 0.0);
        for tu_row in 0..n {
            for tu_col in 0..n {
                let (col_off, row_off) = (tu_col * tx, tu_row * tx);
                let tu = At {
                    x: x + col_off,
                    y: y + row_off,
                    side: tx,
                    reach: Reach::of_tu(side, side, col_off, row_off, tx, tx, reach),
                    set,
                };
                let trial = match filter_intra {
                    Some(fi) => {
                        let (mut above_buf, mut left_buf) = ([0u8; 2 * BLOCK], [0u8; 2 * BLOCK]);
                        let (above, left, corner) =
                            self.edges_into(tu.x, tu.y, tx, tu.reach, &mut above_buf, &mut left_buf);
                        let mut prediction = vec![0u8; tx * tx];
                        filter_intra_predict_u8(
                            usize::from(fi),
                            above,
                            left,
                            corner,
                            tx,
                            &mut prediction,
                            fctx,
                        );
                        self.code_from_prediction(
                            tu.x,
                            tu.y,
                            tx,
                            &prediction,
                            false,
                            search.base_q_idx,
                            search.deadzone,
                            set,
                        )
                    }
                    None => {
                        self.trial(tu, mode, angle_delta, search.base_q_idx, search.deadzone, fctx)
                    }
                };
                sse += trial.sse;
                bits += trial.bits;
                self.commit(tu.x, tu.y, tx, &trial);
                for row in 0..tx {
                    levels[(row_off + row) * side + col_off..][..tx]
                        .copy_from_slice(&trial.levels[row * tx..][..tx]);
                }
            }
        }
        self.ctx.tu_split = false;
        (levels, sse, bits)
    }

    /// Writes a trial's reconstruction back into the plane, and the
    /// coefficient context it leaves for its neighbours ([`CoefCtxMap`]).
    fn commit(&mut self, x: usize, y: usize, side: usize, trial: &Trial) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&trial.reconstruction[row * side..][..side]);
        }
        self.commit_ctx(x, y, side, &trial.levels);
    }

    /// Publishes one committed transform's own context over every 4x4 cell it
    /// covers -- tile.rs `neighbour_state` + `Neighbours::record_planes`,
    /// which is what the writer will read when it reaches the next block.
    fn commit_ctx(&mut self, x: usize, y: usize, side: usize, levels: &[i32]) {
        if self.ctx.cells.is_empty() {
            self.ctx.stride = self.width.div_ceil(4);
            self.ctx.cells = vec![CoefCtx::default(); self.ctx.stride * self.height.div_ceil(4)];
        }
        let cul = levels.iter().map(|l| l.unsigned_abs()).sum::<u32>().min(7) as u8;
        let state = CoefCtx {
            level: cul,
            dc: match levels[0].signum() {
                0 => 0,
                -1 => -1,
                _ => 1,
            },
        };
        let n = (side / 4).max(1);
        // Only up to the frame's TRUE extent: the writer's own
        // `Neighbours::record_planes` clamps to `blocks_wide`/`blocks_high`
        // (spec `av1_set_entropy_contexts`, off the true `mi_cols`/`mi_rows`)
        // and leaves the cells past it uncoded even mid-block, so a
        // straddling block publishes context for its inside part only.
        let rows = n.min(self.true_height.saturating_sub(y).div_ceil(4));
        let cols = n.min(self.true_width.saturating_sub(x).div_ceil(4));
        for row in 0..rows {
            let start = (y / 4 + row) * self.ctx.stride + x / 4;
            let end = (start + cols).min(self.ctx.cells.len());
            if start < end {
                self.ctx.cells[start..end].fill(state);
            }
        }
    }

    /// A block committed without any residual at all ([`ibc_commit`]): the
    /// cells it covers coded nothing, and leaving an earlier trial's state
    /// there would price its neighbours off a block that was never coded.
    fn commit_ctx_zero(&mut self, x: usize, y: usize, side: usize) {
        self.commit_ctx(x, y, side, &[0i32]);
    }

    /// The `txb_skip` and `dc_sign` contexts a transform at `(x, y)` of this
    /// plane reads, off what the blocks before it in coding order committed
    /// ([`CoefCtxMap`]) -- the writer's own derivation: chroma reads whether
    /// the above/left neighbours coded anything (tile.rs
    /// `write_block_planes`), luma reads context 0 when its transform covers
    /// its whole block and decode.rs `SKIP_CONTEXTS` off the neighbour
    /// magnitude tiers when it does not (tile.rs `write_luma_tus` /
    /// `Neighbours::luma_skip_ctx`), and both planes read the DC sign vote of
    /// the same neighbours (tile.rs `dc_sign_ctx`).
    ///
    /// A neighbour outside this plane's own TILE does not exist, exactly as
    /// the writer's per-tile `Neighbours` bands start blank.
    ///
    /// CLOSED (lane-av1straddle): the decisions this price changes used to
    /// make `decode_stream_round_trips_an_odd_size_gop` and
    /// `an_odd_size_gop_round_trips_bit_exact_against_the_encoder_reconstruction`
    /// fail at 216x96. Never this map: the writer coded the phantom
    /// transform units of a right-edge-straddling 32x32 (tile.rs
    /// `write_luma_tus`, now clipped to the frame's true `mi_cols`/`mi_rows`
    /// like decode.rs and libaom), which libdav1d and libaom both refused
    /// outright while a decision change was what first walked into it.
    fn coef_ctx(&self, x: usize, y: usize, side: usize, set: TxbSet) -> (usize, usize) {
        if self.ctx.cells.is_empty() {
            return (0, 0);
        }
        let n = (side / 4).max(1);
        let (cx, cy) = (x / 4, y / 4);
        let (mut top, mut left) = (0u8, 0u8);
        let mut vote = 0i32;
        if y > self.tile_y0 && cy > 0 {
            for cell in &self.ctx.cells[(cy - 1) * self.ctx.stride + cx..][..n] {
                top |= cell.level;
                vote += i32::from(cell.dc);
            }
        }
        if x > self.tile_x0 && cx > 0 {
            for row in 0..n {
                let cell = self.ctx.cells[(cy + row) * self.ctx.stride + cx - 1];
                left |= cell.level;
                vote += i32::from(cell.dc);
            }
        }
        let skip_ctx = if set.is_chroma() {
            usize::from(top != 0) + usize::from(left != 0)
        } else if self.ctx.tu_split {
            crate::decode::SKIP_CONTEXTS[usize::from(top).min(4)][usize::from(left).min(4)]
        } else {
            0
        };
        let sign_ctx = match vote.signum() {
            0 => 0,
            -1 => 1,
            _ => 2,
        };
        (skip_ctx, sign_ctx)
    }

    /// The source samples of one square, contiguous — what a motion search
    /// compares its candidates' predictions against ([`motion::search`]'s
    /// `source`), since [`Self::source`] itself is the whole plane, strided
    /// by [`Self::width`].
    fn source_block(&self, x: usize, y: usize, side: usize) -> Vec<u8> {
        rows_of(self.source, self.width, x, y, side)
    }

    /// The reconstructed samples of one square, so that a partition trial can
    /// be undone.
    /// The reconstructed samples of one square AND the coefficient contexts
    /// its cells hold, so that a partition trial can be undone whole: a
    /// losing trial that left its own nz/DC state behind would price the
    /// blocks after it off a block the tile writer never codes.
    fn snapshot(&self, x: usize, y: usize, side: usize) -> (Vec<u8>, Vec<CoefCtx>) {
        (
            rows_of(&self.reconstruction, self.width, x, y, side),
            self.ctx_rows(x, y, side),
        )
    }

    fn ctx_rows(&self, x: usize, y: usize, side: usize) -> Vec<CoefCtx> {
        if self.ctx.cells.is_empty() {
            return Vec::new();
        }
        let n = (side / 4).max(1);
        (0..n)
            .flat_map(|row| {
                let start = (y / 4 + row) * self.ctx.stride + x / 4;
                self.ctx.cells[start..(start + n).min(self.ctx.cells.len())].to_vec()
            })
            .collect()
    }

    /// Puts a snapshot back.
    fn restore(&mut self, x: usize, y: usize, side: usize, saved: &(Vec<u8>, Vec<CoefCtx>)) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&saved.0[row * side..][..side]);
        }
        if saved.1.is_empty() {
            return;
        }
        let n = (side / 4).max(1);
        for row in 0..n {
            let start = (y / 4 + row) * self.ctx.stride + x / 4;
            let end = (start + n).min(self.ctx.cells.len());
            if start < end {
                self.ctx.cells[start..end].copy_from_slice(&saved.1[row * n..][..end - start]);
            }
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
        lambda: f64, fctx: &crate::decode::FrameCtx,
    ) -> Vec<(f64, u8)> {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * BLOCK], [0u8; 2 * BLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction, fctx,
                );
                let sad = self.block_sad(x, y, side, &prediction);
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
        mode_bits: &[f64; 13], fctx: &crate::decode::FrameCtx,
    ) -> (Vec<Coeff>, u8, i32, f64) {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * BLOCK], [0u8; 2 * BLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        // Nothing ranks by the SAD pre-pass unless `top_k` prunes by it (or
        // the census reports it), so summing it under the default unpruned
        // search is `side * side` absolute differences per mode spent on a
        // number no one reads.
        let want_sad = search.top_k.is_some() || census_on();
        let mut scored: Vec<(f64, u8, Vec<u8>)> = search
            .modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction, fctx,
                );
                let sad = if want_sad {
                    self.block_sad(x, y, side, &prediction)
                } else {
                    0.0
                };
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
        let ranking: Vec<(f64, u8)> = if census_on() {
            scored.iter().map(|&(score, mode, _)| (score, mode)).collect()
        } else {
            Vec::new()
        };
        census_add(0, 1);
        census_add(1, scored.len());
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
        let (mut cost, mode, mut trial) = best.expect("the search offers at least one mode");
        // `angle_delta_y` (spec `read_intra_angle_info`): the eight
        // directional modes each carry a -3..=3 delta, three degrees a step,
        // which the writer has always coded as ZERO because nothing searched
        // it. Refined on the winning mode only -- libaom's own ordering, and
        // six extra trials on the directional blocks instead of on every
        // block times every mode. The edges are the ones the base angle
        // already read (`enable_intra_edge_filter` is off in this encoder's
        // sequence header, so no delta changes which samples exist), and the
        // predictor is the decoder's, `angle_delta` argument and all.
        let mut angle_delta = 0i32;
        // Not on a screen frame: measured on the 5-row native gate, the
        // refinement is worth -0.6/-0.6 on the 2160p film row and flat on the
        // 1080p one, but +0.7/+0.3 the WRONG way on the screen capture --
        // synthetic edges land on the base angles, so every delta there buys
        // a symbol and no prediction. Same `search.screen` gate palette and
        // intrabc already stand behind.
        if angle_delta_on() && !search.screen && (V_PRED..=D67_PRED).contains(&mode) {
            let row = &cdf::ANGLE_DELTA[usize::from(mode - V_PRED)];
            let zero_bits = symbol_bits(row, crate::tile::ANGLE_DELTA_ZERO);
            let mut prediction = vec![0u8; side * side];
            for delta in [-3i32, -2, -1, 1, 2, 3] {
                intra_predict_u8(
                    mode,
                    delta,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction, fctx,
                );
                let candidate = self.code_from_prediction(
                    x,
                    y,
                    side,
                    &prediction,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    at.set,
                );
                let bits = symbol_bits(row, (crate::tile::ANGLE_DELTA_ZERO as i32 + delta) as usize)
                    - zero_bits;
                let cost_d = candidate.sse
                    + search.lambda * (candidate.bits + mode_bits[usize::from(mode)] + bits);
                if cost_d < cost {
                    cost = cost_d;
                    angle_delta = delta;
                    trial = candidate;
                }
            }
            ANGLE_DELTA_HITS[(crate::tile::ANGLE_DELTA_ZERO as i32 + angle_delta) as usize]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if census_on() {
            LUMA_RANK[sad_rank(&ranking, mode).min(12)]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.commit(x, y, side, &trial);
        (coeffs(&trial.levels, side), mode, angle_delta, cost)
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
const HAS_TOP_RIGHT: [&[u8]; 4] = [
    &[255, 85, 119, 85, 127, 85, 119, 85],
    &[95, 87],
    // has_tr_8x8 (libaom av1/common/reconintra.c) -- table_stride(8) == 16
    // (128 / 8), so this row is 16 wide by 16 rows, 32 bytes.
    &[
        255, 255, 85, 85, 119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85, 255, 127, 85, 85,
        119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85,
    ],

    // has_tr_4x4 (libaom av1/common/reconintra.c, transcribed verbatim) --
    // table_stride(4) == 32 (128 / 4), so 32 bits per row, 32 rows, 128
    // bytes. Sub-8x8 leaves (lane-sub8) are the only blocks that reach here
    // with side 4 outside a TX4 split; without their own row they clamped
    // into the 8x8 table and read above-right samples the decoder has not
    // written yet (all-zero pixels in the bottom-right triangle of a
    // directional 4x4 leaf).
    &[
        255, 255, 255, 255, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127,
        85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 255, 127, 255, 127, 85, 85, 85, 85,
        119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127, 85, 85, 85, 85, 119, 119, 119, 119,
        85, 85, 85, 85, 255, 255, 255, 127, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85,
        127, 127, 127, 127, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 255, 127, 255, 127,
        85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127, 85, 85, 85, 85,
        119, 119, 119, 119, 85, 85, 85, 85,
    ],
];

/// `has_bl_16x16` / `has_bl_32x32`: the same, for the block below and to the
/// left.
const HAS_BOTTOM_LEFT: [&[u8]; 4] = [
    &[84, 16, 84, 0, 84, 16, 84, 0],
    &[4, 4],
    // has_bl_8x8 (libaom av1/common/reconintra.c)
    &[
        84, 85, 16, 17, 84, 85, 0, 1, 84, 85, 16, 17, 84, 85, 0, 0, 84, 85, 16, 17, 84, 85, 0, 1,
        84, 85, 16, 17, 84, 85, 0, 0,
    ],

    // has_bl_4x4 (same source, same order).
    &[
        84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17,
        84, 85, 85, 85, 0, 0, 1, 0, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84,
        85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 0, 0, 0, 84, 85, 85, 85, 16, 17, 17, 17, 84,
        85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 0, 1, 0, 84, 85,
        85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85,
        85, 85, 0, 0, 0, 0,
    ],
];

/// `has_tr_vert_*` (libaom `reconintra.c`, `has_tr_vert_tables`), indexed by
/// [`Reach::table`] exactly like [`HAS_TOP_RIGHT`]: inside a
/// `PARTITION_VERT_A`/`_B` the square sub-blocks are visited TL, BL, TR, BR
/// rather than in raster order, so the bottom-left square's top-right
/// neighbour is the partition's own right-hand rectangle -- not yet decoded.
/// libaom switches tables on the partition type (`get_has_tr_table`); the
/// ordinary table says 1 where this one says 0 (32x32 at row 1 col 0:
/// `95` bit 4 = 1 vs `15` bit 4 = 0), which is exactly the case
/// lane-part32 r5 caught reading undecoded samples.
const HAS_TOP_RIGHT_VERT: [&[u8]; 3] = [
    // has_tr_vert_16x16
    &[255, 0, 119, 0, 127, 0, 119, 0],
    // has_tr_vert_32x32
    &[15, 7],
    // has_tr_vert_8x8
    &[
        255, 255, 0, 0, 119, 119, 0, 0, 127, 127, 0, 0, 119, 119, 0, 0, 255, 127, 0, 0, 119, 119,
        0, 0, 127, 127, 0, 0, 119, 119, 0, 0,
    ],
];

/// [`HAS_TOP_RIGHT_VERT`]'s sibling, `has_bl_vert_tables`.
const HAS_BOTTOM_LEFT_VERT: [&[u8]; 3] = [
    // has_bl_vert_16x16
    &[254, 16, 254, 0, 254, 16, 254, 0],
    // has_bl_vert_32x32
    &[14, 14],
    // has_bl_vert_8x8
    &[
        254, 255, 16, 17, 254, 255, 0, 1, 254, 255, 16, 17, 254, 255, 0, 0, 254, 255, 16, 17, 254,
        255, 0, 1, 254, 255, 16, 17, 254, 255, 0, 0,
    ],
];

thread_local! {
    /// Set for the duration of a `PARTITION_VERT_A`/`_B`'s own sub-blocks
    /// (see [`Reach::vert_ab_partition`]) -- libaom carries the partition
    /// type down into `has_top_right`/`has_bottom_left` as an argument, and
    /// threading one bool through this crate's already twenty-argument
    /// `decode_block` would touch every call site instead of the two arms
    /// that need it.
    static VERT_AB_PARTITION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restores [`VERT_AB_PARTITION`] to what it was when the guard was made.
pub(crate) struct VertAbGuard(bool);

impl Drop for VertAbGuard {
    fn drop(&mut self) {
        VERT_AB_PARTITION.with(|c| c.set(self.0));
    }
}

/// The `has_tr_*` / `has_bl_*` tables for every rectangular shape this
/// decoder codes (libaom `reconintra.c`, generated from the oracle's source
/// so no byte is retyped). Each is indexed by
/// `(blk_row_in_sb << (5 - log2(bw / 4))) + blk_col_in_sb`, i.e. row stride
/// `128 / bw` bits ([`Reach::table_stride`]) on libaom's 128-pixel
/// superblock grid; on this crate's 64-pixel superblock the highest index a
/// shape can reach stays inside its own table, so no wrap is involved.
///
/// lane-tx4x8 r3: the previous two-row `HAS_TOP_RIGHT_RECT`/
/// `HAS_BOTTOM_LEFT_RECT` pair held 32x16/16x32 only and a `_` arm routed
/// every other shape -- 16x8 strips among them -- to the 32x16 row, wrong at
/// 10 of the 21 reachable superblock positions for above-right and 7 for
/// below-left.
/// `has_tr_4x8` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 32 bits (`128 / 4`).
const HAS_TR_4X8: [u8; 64] = [255, 255, 255, 255, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 127, 255, 127, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 255, 255, 127, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 127, 255, 127, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119];
/// `has_bl_4x8` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 32 bits (`128 / 4`).
const HAS_BL_4X8: [u8; 64] = [16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 1, 0, 16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 0, 0, 16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 1, 0, 16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 0, 0];
/// `has_tr_8x4` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 16 bits (`128 / 8`).
const HAS_TR_8X4: [u8; 64] = [255, 255, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0, 127, 127, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0, 255, 127, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0, 127, 127, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0];
/// `has_bl_8x4` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 16 bits (`128 / 8`).
const HAS_BL_8X4: [u8; 64] = [254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 1, 254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 0, 254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 1, 254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 0];
/// `has_tr_8x16` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 16 bits (`128 / 8`).
const HAS_TR_8X16: [u8; 16] = [255, 255, 119, 119, 127, 127, 119, 119, 255, 127, 119, 119, 127, 127, 119, 119];
/// `has_bl_8x16` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 16 bits (`128 / 8`).
const HAS_BL_8X16: [u8; 16] = [16, 17, 0, 1, 16, 17, 0, 0, 16, 17, 0, 1, 16, 17, 0, 0];
/// `has_tr_16x8` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 8 bits (`128 / 16`).
const HAS_TR_16X8: [u8; 16] = [255, 0, 85, 0, 119, 0, 85, 0, 127, 0, 85, 0, 119, 0, 85, 0];
/// `has_bl_16x8` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 8 bits (`128 / 16`).
const HAS_BL_16X8: [u8; 16] = [254, 84, 254, 16, 254, 84, 254, 0, 254, 84, 254, 16, 254, 84, 254, 0];
/// `has_tr_16x32` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 8 bits (`128 / 16`).
const HAS_TR_16X32: [u8; 4] = [255, 119, 127, 119];
/// `has_bl_16x32` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 8 bits (`128 / 16`).
const HAS_BL_16X32: [u8; 4] = [16, 0, 16, 0];
/// `has_tr_32x16` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 4 bits (`128 / 32`).
const HAS_TR_32X16: [u8; 4] = [15, 5, 7, 5];
/// `has_bl_32x16` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 4 bits (`128 / 32`).
const HAS_BL_32X16: [u8; 4] = [78, 14, 78, 14];
/// `has_tr_32x64` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 4 bits (`128 / 32`).
const HAS_TR_32X64: [u8; 1] = [127];
/// `has_bl_32x64` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 4 bits (`128 / 32`).
const HAS_BL_32X64: [u8; 1] = [0];
/// `has_tr_64x32` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 2 bits (`128 / 64`).
const HAS_TR_64X32: [u8; 1] = [19];
/// `has_bl_64x32` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 2 bits (`128 / 64`).
const HAS_BL_64X32: [u8; 1] = [34];
/// `has_tr_4x16` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 32 bits (`128 / 4`).
const HAS_TR_4X16: [u8; 32] = [255, 255, 255, 255, 127, 127, 127, 127, 255, 127, 255, 127, 127, 127, 127, 127, 255, 255, 255, 127, 127, 127, 127, 127, 255, 127, 255, 127, 127, 127, 127, 127];
/// `has_bl_4x16` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 32 bits (`128 / 4`).
const HAS_BL_4X16: [u8; 32] = [0, 1, 1, 1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0];
/// `has_tr_16x4` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 8 bits (`128 / 16`).
const HAS_TR_16X4: [u8; 32] = [255, 0, 0, 0, 85, 0, 0, 0, 119, 0, 0, 0, 85, 0, 0, 0, 127, 0, 0, 0, 85, 0, 0, 0, 119, 0, 0, 0, 85, 0, 0, 0];
/// `has_bl_16x4` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 8 bits (`128 / 16`).
const HAS_BL_16X4: [u8; 32] = [254, 254, 254, 84, 254, 254, 254, 16, 254, 254, 254, 84, 254, 254, 254, 0, 254, 254, 254, 84, 254, 254, 254, 16, 254, 254, 254, 84, 254, 254, 254, 0];
/// `has_tr_8x32` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 16 bits (`128 / 8`).
const HAS_TR_8X32: [u8; 8] = [255, 255, 127, 127, 255, 127, 127, 127];
/// `has_bl_8x32` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 16 bits (`128 / 8`).
const HAS_BL_8X32: [u8; 8] = [0, 1, 0, 0, 0, 1, 0, 0];
/// `has_tr_32x8` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 4 bits (`128 / 32`).
const HAS_TR_32X8: [u8; 8] = [15, 0, 5, 0, 7, 0, 5, 0];
/// `has_bl_32x8` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 4 bits (`128 / 32`).
const HAS_BL_32X8: [u8; 8] = [238, 78, 238, 14, 238, 78, 238, 14];
/// `has_tr_16x64` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 8 bits (`128 / 16`).
const HAS_TR_16X64: [u8; 2] = [255, 127];
/// `has_bl_16x64` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 8 bits (`128 / 16`).
const HAS_BL_16X64: [u8; 2] = [0, 0];
/// `has_tr_64x16` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 2 bits (`128 / 64`).
const HAS_TR_64X16: [u8; 2] = [3, 1];
/// `has_bl_64x16` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 2 bits (`128 / 64`).
const HAS_BL_64X16: [u8; 2] = [42, 42];

/// The `has_tr_*`/`has_bl_*` row for one rectangular block shape.
fn rect_reach_tables(bw: usize, bh: usize) -> (&'static [u8], &'static [u8]) {
    match (bw, bh) {
        (4, 8) => (&HAS_TR_4X8, &HAS_BL_4X8),
        (8, 4) => (&HAS_TR_8X4, &HAS_BL_8X4),
        (8, 16) => (&HAS_TR_8X16, &HAS_BL_8X16),
        (16, 8) => (&HAS_TR_16X8, &HAS_BL_16X8),
        (16, 32) => (&HAS_TR_16X32, &HAS_BL_16X32),
        (32, 16) => (&HAS_TR_32X16, &HAS_BL_32X16),
        (32, 64) => (&HAS_TR_32X64, &HAS_BL_32X64),
        (64, 32) => (&HAS_TR_64X32, &HAS_BL_64X32),
        (4, 16) => (&HAS_TR_4X16, &HAS_BL_4X16),
        (16, 4) => (&HAS_TR_16X4, &HAS_BL_16X4),
        (8, 32) => (&HAS_TR_8X32, &HAS_BL_8X32),
        (32, 8) => (&HAS_TR_32X8, &HAS_BL_32X8),
        (16, 64) => (&HAS_TR_16X64, &HAS_BL_16X64),
        (64, 16) => (&HAS_TR_64X16, &HAS_BL_64X16),
        _ => unreachable!("no libaom has_tr/has_bl table for a {bw}x{bh} block"),
    }
}

impl Reach {
    /// `has_top_right`/`has_bottom_left` (libaom `reconintra.c`) for a
    /// transform unit at `(col_off, row_off)` INSIDE a `bw`x`bh` block whose
    /// own answer is `block` -- a different rule from the standalone-block
    /// tables, because a unit's neighbours are mostly its own block's
    /// already-reconstructed units:
    ///  * top-right: available while the unit to the right is still inside
    ///    the block (`col_off + tx < bw`); otherwise only the block's top row
    ///    of units reaches past it, through the block-level answer
    ///    (libaom's `row_off > 0` branch returns exactly
    ///    `col_off + txw < plane_bw`).
    ///  * bottom-left: a unit outside the block's left column has none
    ///    (libaom returns 0 for `col_off > 0`); inside the block
    ///    (`row_off + tx < bh`) they are the left unit's, already
    ///    reconstructed; the bottom row falls back to the block-level answer.
    pub(crate) fn of_tu(
        bw: usize,
        bh: usize,
        col_off: usize,
        row_off: usize,
        // The unit's own width and height in pixels; equal for a square
        // transform, different for the rect unit a 1:4 strip's first split
        // produces (lane-rectsplitx r1).
        tx_w: usize,
        tx_h: usize,
        block: Self,
    ) -> Self {
        // A block wider than 64 is coded in 64x64 "mu" chunks in raster
        // order, so both libaom predicates special-case it (lane-sb128c r10);
        // without this the last unit column of every chunk of a 128x64 block
        // predicts from a chunk libaom has not decoded yet.
        let wide = bw > 64;
        Self {
            above_right: if row_off > 0 {
                // `has_top_right`'s `row_off > 0` branch: the offset is taken
                // modulo the 64-wide chunk, except for the one unit whose
                // top-right corner is the centre of a 128x128 block.
                if wide {
                    (row_off == 64 && col_off + tx_w == 64) || (col_off % 64) + tx_w < 64
                } else {
                    col_off + tx_w < bw
                }
            } else if col_off + tx_w < bw {
                true
            } else {
                block.above_right
            },
            below_left: if wide && col_off > 0 && col_off % 64 == 0 {
                // `has_bottom_left`: at the left edge of a right-hand chunk
                // the bottom-left pixels are in the already-coded left chunk,
                // provided they stay inside it.
                (row_off % 64) + tx_h < bh.min(64)
            } else if col_off > 0 {
                false
            } else if row_off + tx_h < bh {
                true
            } else {
                block.below_left
            },
        }
    }

    /// What a block of `side` samples at `(x, y)` may read past its own edges,
    /// in a frame of `width` by `height`.
    pub(crate) fn of(side: usize, x: usize, y: usize, width: usize, height: usize, fctx: &crate::decode::FrameCtx) -> Self {
        Self {
            above_right: y > 0 && x + side < width && Self::top_right(side, x, y, fctx),
            below_left: x > 0 && y + side < height && Self::bottom_left(side, x, y, fctx),
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
        height: usize, fctx: &crate::decode::FrameCtx,
    ) -> Self {
        Self {
            above_right: y > 0 && x + bw < width && Self::top_right_rect(bw, bh, x, y, fctx),
            below_left: x > 0 && y + bh < height && Self::bottom_left_rect(bw, bh, x, y, fctx),
        }
    }

    /// [`Self::of`] for one transform unit of a square block whose transform
    /// is smaller than itself (libaom `has_top_right`/`has_bottom_left` with
    /// non-zero `row_off`/`col_off`): interior units answer from the block's
    /// own geometry, and only a unit on the block's top row (resp. left
    /// column) reaches the table -- which is the BLOCK's table read at the
    /// BLOCK's position, never a table row for the transform's size. Letting
    /// the unit stand in for a block of its own size agrees with libaom for
    /// the ordinary tables, but the `has_tr_vert_*`/`has_bl_vert_*` pair has
    /// no 4x4 row at all (`has_tr_vert_tables[BLOCK_4X4]` is NULL), so a TX4
    /// unit inside an 8x8 square of a `PARTITION_VERT_A`/`_B` panicked on a
    /// real `--enable-tx-size-search=1` stream (lane-ab16 r2).
     /// libaom `has_top_right` at block granularity (`row_off`/`col_off` 0,
    /// the transform covering the whole block, luma): everything before the
    /// table lookup is that function's own early-exit ladder.
    fn top_right_rect(bw: usize, bh: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let sb_mi: usize = reach_sb_px(fctx) / 4;
        let (mi_row, mi_col) = (y / 4, x / 4);
        let bw_log2 = (bw / 4).trailing_zeros() as usize;
        let bh_log2 = (bh / 4).trailing_zeros() as usize;
        let blk_row = (mi_row & (sb_mi - 1)) >> bh_log2;
        let blk_col = (mi_col & (sb_mi - 1)) >> bw_log2;
        // Top row of the superblock: the top-right pixels are in the (already
        // decoded) superblock above.
        if blk_row == 0 {
            return true;
        }
        // Rightmost column: they fall in the superblock to the right.
        if ((blk_col + 1) << bw_log2) >= sb_mi {
            return false;
        }
        let table = rect_reach_tables(bw, bh).0;
        Self::bit(table, Self::rect_table_index(bw_log2, blk_row, blk_col))
    }

    /// libaom `has_top_right`'s own index arithmetic:
    /// `(blk_row << (MAX_MIB_SIZE_LOG2 - bw_in_mi_log2)) + blk_col`, and
    /// `MAX_MIB_SIZE_LOG2` is 5 (`enums.h`: libaom's tables are laid out for a
    /// 128x128 superblock = 32 mi), NOT this crate's 64x64 = 16. lane-rectx r5:
    /// r4 wrote `4 - bw_log2`, halving the row stride -- every table row but
    /// the first then read the wrong byte (16x8 wrong at 80 mi positions in a
    /// 64-wide SB, 8x16 at 16, 32x16 at 64, a regression against main).
    /// [`tests::rect_reach_tables_are_indexed_with_a_32_mi_row_stride`] pins it.
    fn rect_table_index(bw_log2: usize, blk_row: usize, blk_col: usize) -> usize {
        (blk_row << (5 - bw_log2)) + blk_col
    }

    /// libaom `has_bottom_left`, same granularity as [`Self::top_right_rect`].
    fn bottom_left_rect(bw: usize, bh: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let sb_mi: usize = reach_sb_px(fctx) / 4;
        let (mi_row, mi_col) = (y / 4, x / 4);
        let bw_log2 = (bw / 4).trailing_zeros() as usize;
        let bh_log2 = (bh / 4).trailing_zeros() as usize;
        let blk_row = (mi_row & (sb_mi - 1)) >> bh_log2;
        let blk_col = (mi_col & (sb_mi - 1)) >> bw_log2;
        // Leftmost column of the superblock: the bottom-left pixels are in
        // the left superblock iff they stay inside its height.
        if blk_col == 0 {
            return (blk_row << bh_log2) + (bh / 4) < sb_mi;
        }
        // Bottom row (and not the leftmost column): they fall in the
        // superblock below, which is not decoded yet.
        if ((blk_row + 1) << bh_log2) >= sb_mi {
            return false;
        }
        let table = rect_reach_tables(bw, bh).1;
        Self::bit(table, Self::rect_table_index(bw_log2, blk_row, blk_col))
    }

    /// Marks every [`Reach::of`] made until the returned guard drops as being
    /// inside a `PARTITION_VERT_A`/`_B` (libaom's `get_has_tr_table` /
    /// `get_has_bl_table` partition argument). Rectangular sub-blocks are
    /// unaffected: libaom's own comment says vertical rectangles keep their
    /// non-vert table, which is what [`Reach::of_rect`] already reads.
    /// Whether a `PARTITION_VERT_A`/`_B`'s sub-blocks are being decoded
    /// right now ([`Self::vert_ab_partition`]'s guard is alive), so a caller
    /// can count the sub-block work that partition creates (lane-ab16).
    pub(crate) fn in_vert_ab() -> bool {
        VERT_AB_PARTITION.with(std::cell::Cell::get)
    }

    pub(crate) fn vert_ab_partition() -> VertAbGuard {
        VertAbGuard(VERT_AB_PARTITION.with(|c| c.replace(true)))
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
    fn top_right(side: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let (row, col, per_side) = Self::position(side, x, y, fctx);
        if row == 0 {
            return true;
        }
        if col + 1 == per_side {
            return false;
        }
        // lane-sub8 r4: side 4 now has its own `has_tr_4x4`/`has_bl_4x4` row
        // (the clamp corner-cut that used to live here is gone); sides
        // 16/32/64 share row 0 exactly as libaom's tables repeat.
        // corner-cut: libaom transcribes no vert 4x4 row, so a side-4 block
        // inside a `PARTITION_VERT_A`/`_B` falls back to the ordinary table
        // -- a reach over-estimate (extra above-right reference pixels),
        // never a stream desync. Ceiling: transcribe `has_tr_vert_4x4` /
        // `has_bl_vert_4x4` and extend the VERT tables to four rows.
        // NB: named `table_row`, not `row` -- `row` above is the block's
        // position inside the superblock and is what the bit index below
        // steps by (a merge that shadowed it desynced every block size).
        let table_row = Self::table(side);
        let table = if VERT_AB_PARTITION.with(std::cell::Cell::get) && table_row < HAS_TOP_RIGHT_VERT.len() {
            HAS_TOP_RIGHT_VERT[table_row]
        } else {
            HAS_TOP_RIGHT[table_row]
        };
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
    }

    /// `has_bottom_left` for such a block: the leftmost column of a superblock
    /// reads into the superblock to its left, which is decoded only as far
    /// down as its own bottom; the bottom row would read into the superblock
    /// below, which is not decoded at all; and the table answers the rest.
    fn bottom_left(side: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let (row, col, per_side) = Self::position(side, x, y, fctx);
        if col == 0 {
            return row * side + side < reach_sb_px(fctx);
        }
        if row + 1 == per_side {
            return false;
        }
        // lane-sub8 r4: side 4 now has its own `has_tr_4x4`/`has_bl_4x4` row
        // (the clamp corner-cut that used to live here is gone); sides
        // 16/32/64 share row 0 exactly as libaom's tables repeat.
        // corner-cut: libaom transcribes no vert 4x4 row, so a side-4 block
        // inside a `PARTITION_VERT_A`/`_B` falls back to the ordinary table
        // -- a reach over-estimate (extra above-right reference pixels),
        // never a stream desync. Ceiling: transcribe `has_tr_vert_4x4` /
        // `has_bl_vert_4x4` and extend the VERT tables to four rows.
        // NB: named `table_row`, not `row` -- `row` above is the block's
        // position inside the superblock and is what the bit index below
        // steps by (a merge that shadowed it desynced every block size).
        let table_row = Self::table(side);
        let table = if VERT_AB_PARTITION.with(std::cell::Cell::get) && table_row < HAS_BOTTOM_LEFT_VERT.len() {
            HAS_BOTTOM_LEFT_VERT[table_row]
        } else {
            HAS_BOTTOM_LEFT[table_row]
        };
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
    }

    /// Where a block sits inside its superblock, in blocks of its own size,
    /// and how many of them a superblock is across.
    fn position(side: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> (usize, usize, usize) {
        let sb = reach_sb_px(fctx);
        ((y % sb) / side, (x % sb) / side, sb / side)
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
        } else if side == 4 {
            3
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
/// Whether local warped motion is on: the `WARPED_CAUSAL` candidate in the
/// search, the frame header's `allow_warped_motion` and the sequence's
/// `enable_warped_motion`, all three together. ON by default since
/// lane-av1rejudge re-measured it at [`LAMBDA_SCALE`] 0.05 (the table at
/// [`warp_prediction`]); `EC_AV1_WARP=0` turns the tool -- and the two header
/// bits -- back off for an A/B on one build.
pub(crate) fn warp_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_WARP").ok().map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::WARP))
}

/// The smallest LEAF an OBMC candidate is offered for, so the census can
/// attribute the tool's effect by footprint (`EC_AV1_OBMC_MIN`, default 8 --
/// every leaf; 16 or 32 turn the smaller leaves off). Only read when
/// `EC_AV1_OBMC` is on, and never by the 32x32 whole-block candidate.
///
/// MEASURED at native, `EC_AV1_OBMC=1` throughout, BD vs libaom / vs rav1e
/// (12 frames, four quantizers, the standing keep table):
///
/// | min side | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | OBMC off | +18.0 / +0.7 | +47.3 / +19.2 | +59.4 / -10.0 |
/// | 32 (32x32 only) | +18.4 / +1.1 | +47.3 / +19.2 | +59.1 / -10.0 |
/// | 16 | +18.4 / +1.1 | +47.3 / +19.2 | +59.3 / -10.0 |
/// | 8 (every leaf) | +18.7 / +1.4 | +47.3 / +19.2 | +59.3 / -10.0 |
///
/// So the leaves are not the thing that was holding OBMC back: at 8x8 the
/// candidate WINS 9704 of the 11462 leaves that code the symbol on the 1080p
/// film -- 85% of them, far past libaom's own rate -- and costs another 0.3
/// BD points on top of the 0.4 the 32x32 candidate already costs. A tool
/// that wins the local RD on five blocks in six and loses ladder bytes is
/// the `local-RD-on-references` class again, not a missing footprint.
///
/// RE-MEASURED on lane-av1lambda at [`LAMBDA_SCALE`] 0.05 (min side 8):
/// +16.7 / -0.5, +47.1 / +19.2, +51.8 / -14.2 against the same build with
/// the knob off (+17.0 / -0.3, +47.0 / +19.2, +51.6 / -14.2). The 0.7-point
/// loss on the 1080p film became a 0.3-point win and the other two rows went
/// 0.1-0.2 the other way, so OBMC is now neutral rather than a loss -- half
/// of the "local win, ladder loss" it was charged with was the rate weight.
/// It still does not meet the keep rule (two down, one flat, both columns).
/// The tool stays OFF by default; the upgrade path is the price, not the size
/// (the 640x384 arm reads +78.9/+36.4, +94.9/+56.3, +54.2/+0.8 at min side 8
/// against +78.8/+36.1, +95.1/+56.4, +54.5/+1.1 off -- two rows down, the
/// 1080p film up, i.e. the same disagreement between the two gates).
///
/// RE-JUDGED again on lane-av1rejudge against the defaults this lane kept
/// (local warp on, [`LEAF_SECOND_NEW_MARGIN`] 1.2 -- native +16.2/-1.1,
/// +46.6/+18.8, +51.3/-14.4): OBMC at min side 8 on top of that base reads
/// +16.2/-1.0, +46.6/+18.8, +51.3/-14.4, i.e. three rows flat and the 1080p
/// rav1e column 0.1 WORSE, so the tool buys nothing once the second-reference
/// margin is loosened. On warp alone it looked like a keep (+16.3/-0.9 at
/// min 8, +16.6/-0.7 at min 16 against warp's +16.7/-0.5) -- that gain and
/// the margin's gain are the same bytes. It stays OFF.
fn obmc_min_side() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_OBMC_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)
    })
}

/// What the `motion_mode`/`obmc` symbol costs (spec 5.11.24), off the tables
/// this frame's writer really starts from (`tile::obmc_symbol_bits`) -- the
/// same frame-CDF price the coefficients of a non-screen frame are estimated
/// with, not the static default table.
/// The same price for whichever alphabet the writer will really use for this
/// block: `warp_alphabet` is `motion_mode_allowed`'s 3-symbol choice
/// (`allow_warped_motion` and at least one warp sample), otherwise the
/// 2-symbol `obmc` one. Both sides of every motion-mode comparison in the
/// search pay it, so the alphabet cancels out of a comparison and only its
/// per-value spread decides.
fn motion_mode_bits(write_w: usize, write_h: usize, motion: u8, warp_alphabet: bool) -> f64 {
    let Some(row) = crate::decode::motion_mode_cdf_row(write_w, write_h) else {
        return 0.0;
    };
    match warp_alphabet {
        true => crate::tile::motion_mode_symbol_bits(row, motion),
        false => crate::tile::obmc_symbol_bits(row, motion == 1),
    }
}

/// MEASURED 2026-09-06 and NOT KEPT ON BY DEFAULT (`EC_AV1_WARP=1` turns the
/// candidate, the header's `allow_warped_motion` and the sequence's
/// `enable_warped_motion` on together). BD vs libaom / vs rav1e, the standing
/// native keep table, against the same build with the knob off:
///
/// | clip | warp on | off |
/// |---|---|---|
/// | film 1080p | +18.7 / +1.6 | +18.0 / +0.7 |
/// | film 2160p | +47.3 / +19.2 | +47.3 / +19.2 |
/// | screen | +59.2 / -9.9 | +59.4 / -10.0 |
///
/// One row flat, one 0.2 down against libaom and 0.1 up against rav1e, and
/// the 1080p film 0.7/0.9 UP -- the keep rule wants two down and one flat, so
/// it stays off. The 640x384 arm reads +78.7/+36.5, +95.1/+56.4, +54.7/+1.2
/// against +78.8/+36.1, +95.1/+56.4, +54.5/+1.1. The census says the tool
/// fires: 7.0% / 2.7% / 2.4% of the blocks that code a motion_mode symbol
/// take WARPED_CAUSAL at native (197+91+2441 blocks on the 1080p film), so
/// this is a measured loss, not an inert knob. Same shape as the OBMC
/// candidate above it (`obmc_min_side`): a local RD win that does not convert
/// into ladder bytes.
///
/// RE-MEASURED on lane-av1lambda, after [`LAMBDA_SCALE`] moved 0.1 -> 0.05,
/// because a tool whose gain is local and whose cost is the rate it adds is
/// exactly what a halved rate weight re-judges. The verdict FLIPS SIGN:
///
/// | clip | warp on | off (both at lambda 0.05) |
/// |---|---|---|
/// | film 1080p | +16.7 / -0.5 | +17.0 / -0.3 |
/// | film 2160p | +47.0 / +19.1 | +47.0 / +19.2 |
/// | screen | +51.3 / -14.3 | +51.6 / -14.2 |
///
/// two rows down against libaom (0.3 each) with the third flat, and the rav1e
/// column inside 0.1 everywhere -- which reads as a keep rather than the
/// 0.7/0.9 loss it was.
///
/// KEPT ON BY DEFAULT on lane-av1rejudge, which reproduced that table
/// exactly (base +17.0/-0.3, +47.0/+19.2, +51.6/-14.2; warp +16.7/-0.5,
/// +47.0/+19.1, +51.3/-14.3 -- two rows down on both columns, one flat) and
/// ran the conformance shape the lambda lane deferred: every warp stream
/// decodes three-way exact (our decoder and ffmpeg, `EC_COMP_MISMATCH=1`
/// clean), the native gate runs clean under `EC_AV1_TILES=2:2` (warp inside
/// a 16-tile frame, sample walk and all), and tile bytes still do not depend
/// on the thread count. The tool fires on 5.4% / 3.3% / 3.3% of the blocks
/// that code a motion_mode symbol. `EC_AV1_WARP=0` ([`warp_on`]) turns it,
/// `allow_warped_motion` and `enable_warped_motion` back off together.
///
/// The WARPED_CAUSAL prediction of one square single-reference block: the
/// decoder's own warp-sample walk (`decode::find_samples` +
/// `warp::select_samples`), least-squares model (`warp::find_projection`) and
/// affine predictor (`warp::warp_affine`), driven off the encoder's own mi
/// grid -- never a second implementation of any of the three. `None` when the
/// block would code no motion_mode symbol at all, when it has no warp sample
/// (the writer then codes the 2-symbol alphabet, which has no WARPED value),
/// or when the projection is not filter-representable (the decoder would then
/// predict translationally, so the mode is pure cost).
#[allow(clippy::too_many_arguments)]
fn warp_prediction(
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    ref_frame: i8,
    ref_planes: [(&[u16], usize, usize, usize); 3],
    fctx: &crate::decode::FrameCtx,
) -> Option<[Vec<u8>; 3]> {
    let n4 = side / 4;
    if !crate::decode::has_overlappable_neighbour(grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows) {
        return None;
    }
    let mut samples = crate::decode::find_samples(
        grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows, ref_frame, fctx,
    );
    if samples.is_empty() {
        return None;
    }
    if samples.len() > 1 {
        crate::warp::select_samples(mv, &mut samples, side as i32, side as i32);
    }
    let params = crate::warp::find_projection(
        &samples,
        side as i32,
        side as i32,
        mv.1,
        mv.0,
        mi_row as i32,
        mi_col as i32,
    )?;
    let mut pred = [
        vec![0u16; side * side],
        vec![0u16; side * side / 4],
        vec![0u16; side * side / 4],
    ];
    // The translational prediction first, exactly as the decoder builds it,
    // then the affine one over the planes whose OWN block is at least 8x8
    // (libaom `av1_init_warp_params` bails below that, so an 8x8 luma block's
    // 4x4 chroma stays translational).
    for (i, r) in ref_planes.into_iter().enumerate() {
        let luma_plane = i == 0;
        let (px, py) = if luma_plane { (x, y) } else { (x / 2, y / 2) };
        let s = if luma_plane { side } else { side / 2 };
        mc::predict(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(px, mv.1, luma_plane),
            mv_to_q4(py, mv.0, luma_plane),
            s,
            s,
            &mut pred[i],
            fctx,
        );
        if crate::decode::warp_plane_allowed(s, s) {
            let (sub, cside) = (usize::from(!luma_plane) as i32, s as i32);
            crate::warp::warp_affine(
                &params,
                r.0,
                r.2 as i32,
                r.3 as i32,
                r.1 as i32,
                &mut pred[i],
                px as i32,
                py as i32,
                cside,
                cside,
                cside,
                sub,
                sub,
                fctx,
            );
        }
    }
    let [py_buf, pu_buf, pv_buf] = pred;
    let u8s = |b: &[u16]| b.iter().map(|&v| v as u8).collect::<Vec<u8>>();
    Some([u8s(&py_buf), u8s(&pu_buf), u8s(&pv_buf)])
}

pub(crate) fn symbol_bits(cdf: &[u16], symbol: usize) -> f64 {
    let low = if symbol == 0 { 0 } else { cdf[symbol - 1] };
    let width = cdf[symbol] - low;
    // lane-av1speed: a CDF interval is a 15-bit width, so there are only
    // 32769 prices a symbol can carry; the `log2` behind each is computed
    // once instead of per call (libm's `__log2_fma` was 5% of the encoder's
    // profile -- this function is called for every symbol of every candidate
    // the RD search prices). Each entry is exactly the expression this used
    // to evaluate, so every price is bit-identical.
    static PRICES: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    PRICES.get_or_init(|| {
        (0..=32768u32)
            .map(|w| -(f64::from(w) / 32768.0).log2())
            .collect()
    })[usize::from(width)]
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
/// How many halvings of a `side`-square block's transform the `tx_depth`
/// symbol can name (libaom's per-size depth cap, `cdf::TX_SIZE_CAT0`'s two
/// symbols against `CAT1`..`CAT3`'s three), floored at the 4x4 transform.
fn max_tx_depth(side: usize) -> usize {
    match side {
        8 => 1,
        16 | 32 => 2,
        _ => 0,
    }
}

/// What the `tx_depth` symbol itself costs, off the size category's own CDF.
/// Priced at context row 0 -- the writer picks the real row from its
/// neighbours' transform sizes, which the search has no view of; the term is
/// the same for every depth of one block either way, so it only shifts the
/// depth-0-versus-split margin, never the ordering between two splits.
fn tx_depth_bits(side: usize, depth: usize) -> f64 {
    match side {
        8 => symbol_bits(&cdf::TX_SIZE_CAT0[0], depth),
        16 => symbol_bits(&cdf::TX_SIZE_CAT1[0], depth),
        32 => symbol_bits(&cdf::TX_SIZE_CAT2[0], depth),
        _ => symbol_bits(&cdf::TX_SIZE_CAT3[0], depth),
    }
}

/// How many blocks resolved to each `tx_depth`, the fire count that says
/// whether the depth search below reaches its candidates at all.
static TX_DEPTH_HITS: [std::sync::atomic::AtomicUsize; 3] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

/// Reads [`TX_DEPTH_HITS`] and zeroes it, so a gate can attribute the counts
/// to its own encode.
#[cfg(test)]
pub(crate) fn take_tx_depth_hits() -> [usize; 3] {
    std::array::from_fn(|d| TX_DEPTH_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many inter blocks resolved to each var-tx depth (0 = one transform
/// over the whole block, 1 = its four halves), the fire count a gate prints
/// to see the split search reach its candidates at all. Indexed
/// `(block is 32x32) * 4 + (block is compound) * 2 + depth`, so one census
/// says which of the four block classes the split reaches -- the 32x32 root
/// and the compound blocks are this lane's two new ones.
static INTER_TX_SPLIT_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];

/// Whether a 32x32 inter winner searches its own var-tx depth (the four
/// 16x16 units against the flat transform) instead of coding depth 0 blind.
/// ON by default; `EC_AV1_TX32_DEPTH=0` turns it off.
///
/// Rejected three times on the bars rows of the native gate (flat to +0.3).
/// lane-av1resweep re-judged it on the two REAL film crops at
/// [`LAMBDA_SCALE`] 0.05, where it is the largest single lever in this file
/// (BD vs libaom / vs rav1e, shipped -> `EC_AV1_TX32_DEPTH=1`):
///
/// | clip | shipped | tx32 depth | + [`compound_var_tx`] |
/// |---|---|---|---|
/// | bars 1080p | +15.6 / -1.7 | +15.7 / -1.5 | +15.6 / -1.6 |
/// | bars 2160p | +46.5 / +18.8 | +46.5 / +18.8 | +46.6 / +18.8 |
/// | film A | +69.0 / +37.5 | +67.9 / +36.3 | +65.5 / +34.7 |
/// | film B | +100.0 / +61.5 | +96.6 / +58.4 | +94.5 / +56.9 |
/// | screen | +51.0 / -14.5 | +50.7 / -14.4 | +50.2 / -14.6 |
///
/// The bars rows are ~90% zero-intra-cost cells, so the split never had
/// residual to reach there -- the tool was being judged on content that
/// cannot pay for it (class `gate-blind-to-feature`). `EC_AV1_TX32_DEPTH`.
fn tx32_depth_search() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TX32_DEPTH").ok().map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::TX32_DEPTH))
}

/// Whether a COMPOUND inter winner is offered the var-tx split at all: its
/// trial units have to be predicted from BOTH references
/// ([`mc_trial_compound`]) or the split would re-predict them by single-
/// reference translation, which is not what the decoder reconstructs.
/// ON by default (the table on [`tx32_depth_search`]: on real film it is
/// worth a further -2.4/-1.6 on film A and -2.1/-1.5 on film B on top of the
/// 32x32 depth search); `EC_AV1_COMP_VARTX=0` turns it off.
/// `EC_AV1_COMP_VARTX`.
fn compound_var_tx() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_COMP_VARTX").ok().map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::COMPOUND_VAR_TX))
}

/// How many 32x32-and-below inter blocks each reference frame won, indexed by
/// `ref_frame` (`crate::mvstack::LAST_FRAME` = 1 .. `ALTREF_FRAME` = 7), the
/// fire count a gate prints to see an extra reference reach a block at all
/// ([[gate-blind-to-feature]]: a pyramid whose backward `ALTREF` is never
/// chosen is a pyramid that costs bytes and buys nothing).
static REF_FRAME_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];

/// Reads [`REF_FRAME_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_ref_frame_hits() -> [usize; 8] {
    std::array::from_fn(|r| REF_FRAME_HITS[r].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Reads [`INTER_TX_SPLIT_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_inter_tx_split_hits() -> [usize; 8] {
    std::array::from_fn(|d| INTER_TX_SPLIT_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// What the `txfm_split` symbols of one inter block cost at `depth`, priced
/// off the neighbour-free context row the way [`tx_depth_bits`] is: depth 0
/// is one flag, depth 1 is a split root plus one unsplit flag per child
/// (decode.rs `read_var_tx_size` stops symbol-free only at `MAX_VARTX_DEPTH`
/// or a 4x4 sub-size).
fn txfm_split_bits(side: usize, depth: usize) -> f64 {
    let max_tx = side.min(64);
    let ctx = |tx: usize| {
        crate::decode::txfm_partition_ctx_rect(
            crate::decode::TXFM_CTX_INIT,
            crate::decode::TXFM_CTX_INIT,
            side,
            tx,
            tx,
        )
    };
    if depth == 0 {
        return symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx)], 0);
    }
    symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx)], 1)
        + 4.0 * symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx / 2)], 0)
}

/// The inter block's own var-tx decision, once its motion vector is settled:
/// its residual as ONE transform over the whole block, or as the four
/// transforms of half the side the tree's first halving names. The prediction
/// is the same motion compensation either way, so the split trial
/// re-transforms the identical residual in four pieces -- it wins where four
/// small transforms carry a locally varying residual cheaper than one big
/// one. Commits the winner into `luma` and hands back its block-coordinate
/// levels, the depth the writer codes and the cost the winner adds to (or
/// takes off) the caller's already-computed flat cost.
///
/// Only for a block wholly inside the frame: past the true edge the reader's
/// var-tx recursion skips the units that fall outside
/// (decode.rs `read_var_tx_size`'s `max_w_mi`/`max_h_mi` guard), which would
/// leave the residual units this writes unpaired.
#[allow(clippy::too_many_arguments)]
fn commit_inter_luma(
    luma: &mut Plane,
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    reference: (&[u16], usize, usize, usize),
    search: &Search,
    flat: &Trial,
    skip: bool,
    // A COMPOUND winner's second half: the vector and reference planes the
    // trial units are blended from ([`mc_trial_compound`], the decoder's own
    // `comp_group_idx == 0` average). `None` is the single-reference block.
    second: Option<((i32, i32), (&[u16], usize, usize, usize))>,
    fctx: &crate::decode::FrameCtx,
) -> (Vec<i32>, u8, f64) {
    let eligible = tx_select()
        && tx_select_inter()
        && !skip
        && side >= 16
        && (second.is_none() || compound_var_tx())
        && (side < BLOCK || tx32_depth_search())
        && x + side <= luma.true_width
        && y + side <= luma.true_height;
    if !eligible {
        luma.commit(x, y, side, flat);
        return (flat.levels.clone(), 0, 0.0);
    }
    let tx = side / 2;
    let set = if tx >= 16 { TxbSet::Luma16Inter } else { TxbSet::Luma8Inter };
    let mut levels = vec![0i32; side * side];
    let (mut sse, mut bits) = (0.0, 0.0);
    let mut units = Vec::with_capacity(4);
    // The split units are smaller than their block, so their `txb_skip`
    // context is the neighbour magnitude table ([`Plane::coef_ctx`]) -- and
    // each unit's own commit publishes what the next one reads, coding order.
    luma.ctx.tu_split = true;
    for tu_row in 0..2 {
        for tu_col in 0..2 {
            let (tu_x, tu_y) = (x + tu_col * tx, y + tu_row * tx);
            let trial = match second {
                Some((mv1, ref1)) => mc_trial_compound(
                    luma, tu_x, tu_y, tx, (mv, mv1), true, reference, ref1,
                    search.base_q_idx, search.deadzone, set, fctx,
                ),
                None => mc_trial(
                    luma, tu_x, tu_y, tx, mv, true, reference.0, reference.1, reference.2,
                    reference.3, false, search.base_q_idx, search.deadzone, set, fctx,
                ),
            };
            sse += trial.sse;
            bits += trial.bits;
            for row in 0..tx {
                levels[(tu_row * tx + row) * side + tu_col * tx..][..tx]
                    .copy_from_slice(&trial.levels[row * tx..][..tx]);
            }
            units.push((tu_x, tu_y, trial));
        }
    }
    luma.ctx.tu_split = false;
    let split_cost = sse + search.lambda * (bits + txfm_split_bits(side, 1));
    let flat_cost = flat.sse + search.lambda * (flat.bits + txfm_split_bits(side, 0));
    let class = usize::from(side == BLOCK) * 4 + usize::from(second.is_some()) * 2;
    if split_cost < flat_cost {
        for (tu_x, tu_y, trial) in &units {
            luma.commit(*tu_x, *tu_y, tx, trial);
        }
        INTER_TX_SPLIT_HITS[class + 1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (levels, 1, split_cost - flat_cost);
    }
    INTER_TX_SPLIT_HITS[class].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    luma.commit(x, y, side, flat);
    (flat.levels.clone(), 0, 0.0)
}

/// The largest distinct-colour count a block may hold and still be offered a
/// palette candidate at all: past it the k-means quantisation costs more
/// error than the map saves rate, and every trial is wasted wall.
///
/// Swept on the gate's own screen capture (lane-av1pal2, the
/// `unswept-decision-constants` class): 32 (the value this lane inherited)
/// finds 515 palette blocks and scores +66.4%/+10.3% vs libaom/rav1e, 64
/// finds 1264 and scores +57.1%/+3.2%, 128 finds 1102 and scores
/// +56.9%/+2.3%, and 256 is byte-identical to 128 -- above 128 the bound
/// binds on nothing this content holds. 128 ships: best BD against both
/// references and no wall cost at all (6.1s, the same as at 32, because the
/// blocks it newly admits are ones the RD then keeps).
/// `EC_AV1_PAL_MAXCOLORS`.
fn palette_max_colors() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_PAL_MAXCOLORS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(128)
    })
}

/// The palettes worth trying for one block's luma, in the shape libaom's
/// `av1_k_means` picks them: a block holding at most `PALETTE_MAX_SIZE`
/// distinct colours takes them ALL (the prediction is then exact and the
/// residual empty), and a busier one is quantised to 8 and to 4 colours by a
/// one-dimensional Lloyd iteration seeded evenly across its range -- both
/// offered, because which wins is an RD question the caller answers.
/// `block` is `side*side` source samples, row-major.
fn palette_candidates(block: &[u8]) -> Vec<crate::tile::PaletteY> {
    let mut histogram = [0u32; 256];
    for &v in block {
        histogram[usize::from(v)] += 1;
    }
    let present: Vec<u16> = (0..256u16).filter(|&v| histogram[usize::from(v)] > 0).collect();
    if present.len() < 2 {
        return Vec::new();
    }
    let exact = |colors: &[u16]| -> crate::tile::PaletteY {
        let mut base = [0u16; 8];
        base[..colors.len()].copy_from_slice(colors);
        let map = block
            .iter()
            .map(|&v| {
                colors
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, &c)| (i32::from(c) - i32::from(v)).abs())
                    .map(|(i, _)| i as u8)
                    .expect("a palette holds at least two colours")
            })
            .collect();
        crate::tile::PaletteY { size: colors.len() as u8, colors: base, map }
    };
    if present.len() <= 8 {
        return vec![exact(&present)];
    }
    if present.len() > palette_max_colors() {
        return Vec::new();
    }
    let (lo, hi) = (
        i32::from(present[0]),
        i32::from(present[present.len() - 1]),
    );
    let mut out = Vec::with_capacity(2);
    for n in [8usize, 4] {
        // Even seeding across the block's own range, then four Lloyd passes
        // over the histogram (256 bins, not `side*side` pixels).
        let mut centres: Vec<f64> = (0..n)
            .map(|i| f64::from(lo) + f64::from(hi - lo) * (i as f64 + 0.5) / n as f64)
            .collect();
        for _ in 0..4 {
            let mut sums = vec![0f64; n];
            let mut counts = vec![0u32; n];
            for (v, &count) in histogram.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                let k = (0..n)
                    .min_by(|&a, &b| {
                        (centres[a] - v as f64)
                            .abs()
                            .total_cmp(&(centres[b] - v as f64).abs())
                    })
                    .expect("n >= 2");
                sums[k] += (v as u32 * count) as f64;
                counts[k] += count;
            }
            for k in 0..n {
                if counts[k] > 0 {
                    centres[k] = sums[k] / f64::from(counts[k]);
                }
            }
        }
        let mut colors: Vec<u16> = centres
            .iter()
            .map(|&c| c.round().clamp(0.0, 255.0) as u16)
            .collect();
        colors.sort_unstable();
        colors.dedup();
        if colors.len() >= 2 {
            out.push(exact(&colors));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
// ---------------------------------------------------------------------------
// lane-av1ibc: intra block copy (spec 5.11.13 `use_intrabc`, 7.11.4 DV
// validity). A key frame that detected screen content and finds enough exact
// repeats in its own source sets `allow_intrabc`, which the spec forces every
// in-loop filter OFF for (frame.rs:194/210/252/272) -- so the bit only earns
// its place when the copied blocks pay back the deblock/CDEF/LR the frame
// gives up. `EC_AV1_INTRABC=0` turns the whole arm off.
// ---------------------------------------------------------------------------

/// Whether intra block copy is offered at all. **Off by default** --
/// `EC_AV1_INTRABC=1` turns it on. The syntax is complete and proven against
/// ffmpeg (`a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly`),
/// but on the gate's real screen capture it MEASURED WORSE: setting
/// `allow_intrabc` costs the key frame its deblocking, CDEF and loop
/// restoration (spec 5.9.11/5.9.19/5.9.20) and the copied blocks do not pay
/// that back -- screen BD +59.1/+5.1 against the +54.5/+1.1 base (lane-av1ibc,
/// `probe_intrabc_key_frame` has the per-key-frame table). Also off on a frame
/// that detected no screen content.
fn intrabc_enabled() -> bool {
    if let Some(forced) = INTRABC_FORCE.with(std::cell::Cell::get) {
        return forced;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("EC_AV1_INTRABC").ok().as_deref() == Some("1"))
}

thread_local! {
    /// Overrides [`intrabc_enabled`] for the calling thread -- the ablation
    /// probe runs both arms in one process, and the suite is one process, so
    /// an environment variable cannot separate them (same reason
    /// [`SCREEN_FORCE`] exists).
    static INTRABC_FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn force_intrabc(value: Option<bool>) {
    INTRABC_FORCE.with(|c| c.set(value));
}

/// How many of a screen frame's 16x16 blocks must have an exact repeat
/// somewhere else in the source before `allow_intrabc` is worth the in-loop
/// filters it costs, in percent (`EC_AV1_INTRABC_PCT`). Unswept-decision-
/// constant class: `probe_intrabc_threshold` measures the arm at several
/// values.
fn intrabc_pct() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_INTRABC_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
    })
}

/// The pixel grid intrabc source positions are hashed on
/// (`EC_AV1_INTRABC_STEP`): every DV this encoder writes is a multiple of it,
/// which keeps the block-hash table (and so the search) linear in the frame
/// at a `step`-th of the density. It must stay EVEN -- a chroma plane copies
/// at `dv / 2` full pel, so an odd DV would land half-pel and need the
/// bilinear interpolation decode.rs applies but this encoder does not.
fn intrabc_step() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let n: usize = std::env::var("EC_AV1_INTRABC_STEP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        if n % 2 == 0 { n.max(2) } else { 4 }
    })
}

/// The side of the luma square the block-hash table is keyed on.
const IBC_HASH: usize = 16;

/// libaom `INTRABC_DELAY_PIXELS / 64` -- how many 64x64 superblocks behind
/// the current one the source's bottom-right corner must already be.
const INTRABC_DELAY_SB64: i32 = 4;

/// FNV-1a over one `IBC_HASH`-square of `plane` at `(x, y)`.
fn ibc_hash(plane: &[u8], width: usize, x: usize, y: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for row in 0..IBC_HASH {
        let base = (y + row) * width + x;
        for &v in &plane[base..base + IBC_HASH] {
            h ^= u64::from(v);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Every `intrabc_step()`-aligned position of `plane` inside
/// `(x0, y0)..(x1, y1)`, keyed by [`ibc_hash`].
fn ibc_index(
    table: &mut std::collections::HashMap<u64, Vec<(u32, u32)>>,
    plane: &[u8],
    width: usize,
    (x0, y0): (usize, usize),
    (x1, y1): (usize, usize),
) {
    let step = intrabc_step();
    if x1 < x0 + IBC_HASH || y1 < y0 + IBC_HASH {
        return;
    }
    let mut y = y0.next_multiple_of(step);
    while y + IBC_HASH <= y1 {
        let mut x = x0.next_multiple_of(step);
        while x + IBC_HASH <= x1 {
            table
                .entry(ibc_hash(plane, width, x, y))
                .or_default()
                .push((x as u32, y as u32));
            x += step;
        }
        y += step;
    }
}

/// Whether `screen`-detected source `y` repeats itself enough to be worth
/// `allow_intrabc` -- the share of its 16-aligned 16x16 blocks whose exact
/// content appears at some other position, against [`intrabc_pct`]. Returns
/// the share as well, for the gate's own print.
fn intrabc_worth_it(y: &[u8], width: usize, true_width: usize, true_height: usize) -> (bool, f64) {
    if true_width < 4 * 64 || true_height < 2 * 64 {
        return (false, 0.0);
    }
    let mut table = std::collections::HashMap::new();
    ibc_index(&mut table, y, width, (0, 0), (true_width, true_height));
    let (mut total, mut matched) = (0usize, 0usize);
    let mut by = 0;
    while by + IBC_HASH <= true_height {
        let mut bx = 0;
        while bx + IBC_HASH <= true_width {
            total += 1;
            if let Some(hits) = table.get(&ibc_hash(y, width, bx, by)) {
                if hits.iter().any(|&(hx, hy)| (hx as usize, hy as usize) != (bx, by)) {
                    matched += 1;
                }
            }
            bx += IBC_HASH;
        }
        by += IBC_HASH;
    }
    let share = if total == 0 { 0.0 } else { matched as f64 / total as f64 };
    (share * 100.0 >= intrabc_pct() as f64, share)
}

/// One tile's intrabc search state: a block-hash table over the part of the
/// tile's own reconstruction that is already final (whole superblock rows --
/// the in-loop filters are off on this frame, so a coded superblock row never
/// changes again), plus the last DV chosen, which the RD price uses as its
/// predictor estimate.
struct Ibc {
    /// Keyed on the SOURCE, exactly as libaom's `av1_intrabc_hash` is: the
    /// source repeats exactly where a lossily coded reconstruction only
    /// nearly does, so an exact-match table over the reconstruction finds
    /// almost nothing. The candidate is then SCORED against the
    /// reconstruction ([`ibc_sse`]), which is what a decoder will copy.
    table: std::collections::HashMap<u64, Vec<(u32, u32)>>,
    /// The tile's own luma rect, in samples.
    tile: (usize, usize, usize, usize),
    /// The DV the previous intrabc block of this tile took -- the *estimate*
    /// of decode.rs `read_intrabc_dv`'s predictor the RD price is taken
    /// against. The writer computes the true predictor off its own mi grid
    /// (`crate::tile::intrabc_dv_pred`); a wrong estimate here can only cost
    /// a slightly mispriced candidate, never a wrong bitstream.
    last_dv: Option<(i32, i32)>,
    /// How many blocks took intrabc, and how many hash lookups found at least
    /// one valid candidate -- the gate's own fire counts.
    hits: usize,
    lookups: usize,
    found: usize,
}

impl Ibc {
    fn new(tile: (usize, usize, usize, usize), source: &[u8], width: usize) -> Self {
        let mut table = std::collections::HashMap::new();
        ibc_index(&mut table, source, width, (tile.0, tile.1), (tile.2, tile.3));
        Self {
            table,
            tile,
            last_dv: None,
            hits: 0,
            lookups: 0,
            found: 0,
        }
    }

    /// libaom `av1_is_dv_valid` for a `w`x`h` block at `(x, y)` under `dv`
    /// (1/8 pel), with this lane's own extra restriction on top: the source
    /// must lie entirely in a COMPLETED superblock row, which is exactly the
    /// region [`Self::fill_to`] indexes and a strict subset of what the spec
    /// allows. corner-cut, ceiling named: same-superblock-row sources (the
    /// wavefront's own diagonal) are legal and not searched; the upgrade path
    /// is to index the current row's finished superblocks too.
    fn dv_valid(&self, x: usize, y: usize, w: usize, h: usize, dv: (i32, i32)) -> bool {
        let (dr, dc) = (dv.0 / 8, dv.1 / 8);
        if dv.0 % 8 != 0 || dv.1 % 8 != 0 || dr % 2 != 0 || dc % 2 != 0 {
            return false;
        }
        let (tx0, ty0, tx1, ty1) = (
            self.tile.0 as i32,
            self.tile.1 as i32,
            self.tile.2 as i32,
            self.tile.3 as i32,
        );
        let (sx, sy) = (x as i32 + dc, y as i32 + dr);
        let (w, h) = (w as i32, h as i32);
        if sx < tx0 || sy < ty0 || sx + w > tx1 || sy + h > ty1 {
            return false;
        }
        let active_sb_row = (y as i32 - ty0) >> 6;
        let active_sb_col = (x as i32 - tx0) >> 6;
        let src_sb_row = (sy + h - 1 - ty0) >> 6;
        let src_sb_col = (sx + w - 1 - tx0) >> 6;
        // This lane's own restriction (see the doc comment).
        if src_sb_row >= active_sb_row {
            return false;
        }
        let total = ((tx1 - tx0 - 1) >> 6) + 1;
        if src_sb_row * total + src_sb_col >= active_sb_row * total + active_sb_col - INTRABC_DELAY_SB64
        {
            return false;
        }
        // libaom's wavefront: only the top-left cone of the frame is legal.
        let wf = (1 + INTRABC_DELAY_SB64) * (active_sb_row - src_sb_row);
        src_sb_col < active_sb_col - INTRABC_DELAY_SB64 + wf
    }
}

/// How many block searches ran, how many found at least one VALID candidate,
/// and how many won on rate-distortion -- the hash hit rate a gate prints
/// (class `gate-blind-to-feature`: "the search ran" and "the search found
/// something" and "the block took it" are three different claims).
static IBC_SEARCH: [std::sync::atomic::AtomicUsize; 3] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

impl Drop for Ibc {
    fn drop(&mut self) {
        for (slot, n) in [self.lookups, self.found, self.hits].into_iter().enumerate() {
            IBC_SEARCH[slot].fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Reads [`IBC_SEARCH`] and zeroes it, so a gate can attribute the counts to
/// its own encode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_intrabc_search() -> [usize; 3] {
    std::array::from_fn(|i| IBC_SEARCH[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// [`crate::tile::write_dv_component`]'s static price -- the same shape as
/// [`mv_component_bits`] without the `mv_fr` symbol, which `force_integer_mv`
/// infers rather than codes.
fn dv_component_bits(diff: i32) -> f64 {
    let z = diff.unsigned_abs() as i32 - 1;
    let mut bits = symbol_bits(&cdf::MV_SIGN, usize::from(diff < 0));
    let class = mv_class_of(z);
    bits += symbol_bits(&cdf::MV_CLASS, class);
    let local = z - mv_class_base(class);
    if class == 0 {
        bits += symbol_bits(&cdf::MV_CLASS0_BIT, ((local >> 3) & 1) as usize);
    } else {
        let d = local >> 3;
        for i in 0..class {
            bits += symbol_bits(&cdf::MV_BIT[i], ((d >> i) & 1) as usize);
        }
    }
    bits
}

/// What one block vector costs against `pred`, plus the `skip` and
/// `use_intrabc` flags the block carries and the ordinary intra block does
/// not pay in this currency.
fn dv_bits(dv: (i32, i32), pred: (i32, i32)) -> f64 {
    let diff = (dv.0 - pred.0, dv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0,
        (false, true) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    let mut bits = symbol_bits(&cdf::MV_JOINT, joint) + 2.0;
    if diff.0 != 0 {
        bits += dv_component_bits(diff.0);
    }
    if diff.1 != 0 {
        bits += dv_component_bits(diff.1);
    }
    bits
}

/// The squared error of copying the `side`-square block at `(sx, sy)` of each
/// plane's reconstruction onto the source block at `(x, y)`, luma and both
/// chroma planes together -- what an intrabc block's whole (residual-free)
/// distortion is.
fn ibc_sse(
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    (sx, sy): (usize, usize),
    side: usize,
) -> f64 {
    let mut sse = 0.0f64;
    for row in 0..side {
        let src = &luma.source[(y + row) * luma.width + x..][..side];
        let rec = &luma.reconstruction[(sy + row) * luma.width + sx..][..side];
        for (&s, &r) in src.iter().zip(rec) {
            let d = f64::from(i32::from(s) - i32::from(r));
            sse += d * d;
        }
    }
    let half = side / 2;
    for plane in chroma {
        for row in 0..half {
            let src = &plane.source[(y / 2 + row) * plane.width + x / 2..][..half];
            let rec = &plane.reconstruction[(sy / 2 + row) * plane.width + sx / 2..][..half];
            for (&s, &r) in src.iter().zip(rec) {
                let d = f64::from(i32::from(s) - i32::from(r));
                sse += d * d;
            }
        }
    }
    sse
}

/// Copies the `side`-square block at `(sx, sy)` of each plane's
/// reconstruction onto `(x, y)` -- an intrabc block's whole reconstruction,
/// since it carries no residual (`skip`). Full-pel in both planes by
/// construction ([`intrabc_step`] keeps every DV even), which is what makes
/// this a plain copy where decode.rs runs the bilinear kernel over the same
/// samples.
fn ibc_commit(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    (sx, sy): (usize, usize),
    side: usize,
) {
    for row in 0..side {
        let from = (sy + row) * luma.width + sx;
        let to = (y + row) * luma.width + x;
        let copied: Vec<u8> = luma.reconstruction[from..from + side].to_vec();
        luma.reconstruction[to..to + side].copy_from_slice(&copied);
    }
    let half = side / 2;
    for plane in chroma {
        for row in 0..half {
            let from = (sy / 2 + row) * plane.width + sx / 2;
            let to = (y / 2 + row) * plane.width + x / 2;
            let copied: Vec<u8> = plane.reconstruction[from..from + half].to_vec();
            plane.reconstruction[to..to + half].copy_from_slice(&copied);
        }
        plane.commit_ctx_zero(x / 2, y / 2, half);
    }
    luma.commit_ctx_zero(x, y, side);
}

/// Searches this block's own DV: every hash-table position whose 16x16 luma
/// square equals the block's source square, plus the previous block's DV as a
/// local fallback, scored by [`ibc_sse`] against the DV's own price. Returns
/// the winning `(dv, cost)`, or `None` when nothing valid was found.
fn ibc_search(
    ibc: &mut Ibc,
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    lambda: f64,
) -> Option<((i32, i32), f64, (usize, usize))> {
    let pred = ibc.last_dv.unwrap_or((0, -(64 + 256) * 8));
    let mut best: Option<((i32, i32), f64, (usize, usize))> = None;
    let consider = |ibc: &Ibc, sx: usize, sy: usize, best: &mut Option<((i32, i32), f64, (usize, usize))>| {
        let dv = (
            (sy as i32 - y as i32) * 8,
            (sx as i32 - x as i32) * 8,
        );
        if !ibc.dv_valid(x, y, side, side, dv) {
            return;
        }
        let cost = ibc_sse(luma, chroma, (x, y), (sx, sy), side) + lambda * dv_bits(dv, pred);
        if best.as_ref().is_none_or(|b| cost < b.1) {
            *best = Some((dv, cost, (sx, sy)));
        }
    };
    ibc.lookups += 1;
    let key = ibc_hash(luma.source, luma.width, x, y);
    // The table is filled in raster order, so the entries BEFORE this block's
    // own position are exactly the already-coded ones -- and the nearest of
    // them costs the fewest DV bits. Walking backwards from the block's own
    // slot is what makes the search look up-and-left; taking the newest
    // entries outright only ever finds candidates below and to the right,
    // every one of which `dv_valid` rejects.
    let hits: Vec<(u32, u32)> = ibc
        .table
        .get(&key)
        .map(|h| {
            let end = h.partition_point(|&(hx, hy)| (hy, hx) < (y as u32, x as u32));
            h[..end].iter().rev().take(32).copied().collect()
        })
        .unwrap_or_default();
    for (hx, hy) in hits {
        consider(&*ibc, hx as usize, hy as usize, &mut best);
    }
    if let Some(dv) = ibc.last_dv {
        let (sx, sy) = (x as i32 + dv.1 / 8, y as i32 + dv.0 / 8);
        if sx >= 0 && sy >= 0 {
            consider(&*ibc, sx as usize, sy as usize, &mut best);
        }
    }
    if best.is_some() {
        ibc.found += 1;
    }
    best
}

fn code_square(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
    // The frame header's `tx_mode == TxMode::Select`: the block then searches
    // its own transform depth ([`Plane::code_tx_depth`]) instead of coding one
    // transform over its whole side.
    tx_select: bool,
    // This tile's intra block-copy state ([`Ibc`]), `None` on a frame whose
    // header did not set `allow_intrabc`.
    ibc: Option<&mut Ibc>, fctx: &crate::decode::FrameCtx,
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
    let at = At {
        x,
        y,
        side,
        reach: Reach::of(side, x, y, luma.true_width, luma.true_height, fctx),
        set: luma_set,
    };
    let (mut luma_coeffs, mode, angle_delta, mut cost) =
        luma.search_block(at, search, mode_bits, fctx);
    // The depth search runs on the mode the whole-block transform picked --
    // libaom's own ordering, and the reason it costs one extra pass per depth
    // rather than one per (mode, depth) pair.
    let mut tx_depth = 0u8;
    if tx_select && max_tx_depth(side) > 0 {
        let mut best = (
            cost + search.lambda * tx_depth_bits(side, 0),
            0usize,
            luma.snapshot(x, y, side),
            luma_coeffs,
        );
        for depth in 1..=max_tx_depth(side) {
            let (levels, sse, bits) = luma.code_tx_depth(at, mode, angle_delta, depth, None, search, fctx);
            let cost_d = sse
                + search.lambda
                    * (bits + mode_bits[usize::from(mode)] + tx_depth_bits(side, depth));
            if cost_d < best.0 {
                best = (cost_d, depth, luma.snapshot(x, y, side), coeffs(&levels, side));
            }
        }
        luma.restore(x, y, side, &best.2);
        cost = best.0;
        tx_depth = best.1 as u8;
        luma_coeffs = best.3;
        TX_DEPTH_HITS[best.1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    // The palette candidate is priced against the mode the luma search (and
    // its transform-depth search) already settled on, so a block only takes
    // one when it beats the best ordinary intra coding of the same pixels.
    // A palette block is `DC_PRED` with `tx_depth` 0 by construction: its
    // prediction is the colour map, not the DC of its neighbours.
    let mut mode = mode;
    let mut angle_delta = angle_delta;
    let mut palette = None;
    if search.screen && crate::decode::palette_bsize_ctx(side).is_some() {
        let source: Vec<u8> = (y..y + side)
            .flat_map(|row| luma.source[row * luma.width + x..][..side].to_vec())
            .collect();
        let kept = luma.snapshot(x, y, side);
        for pal in palette_candidates(&source) {
            let n = usize::from(pal.size);
            let prediction: Vec<u8> = pal
                .map
                .iter()
                .map(|&i| pal.colors[usize::from(i).min(n - 1)] as u8)
                .collect();
            let trial = luma.code_from_prediction(
                x,
                y,
                side,
                &prediction,
                false,
                search.base_q_idx,
                search.deadzone,
                luma_set,
            );
            let cost_p = trial.sse
                + search.lambda
                    * (trial.bits
                        + mode_bits[DC_PRED as usize]
                        + crate::tile::palette_bits(&pal, side)
                        + if tx_select { tx_depth_bits(side, 0) } else { 0.0 });
            if cost_p < cost {
                cost = cost_p;
                mode = DC_PRED as u8;
                angle_delta = 0;
                tx_depth = 0;
                luma_coeffs = coeffs(&trial.levels, side);
                luma.commit(x, y, side, &trial);
                palette = Some(pal);
            }
        }
        if palette.is_none() {
            luma.restore(x, y, side, &kept);
        }
    }

    // The five recursive filter-intra modes (spec 5.11.14), priced against
    // whatever the mode, transform-depth and palette searches settled on --
    // the same "one candidate loop after the incumbent is known" shape the
    // palette above uses. A block that takes one is `DC_PRED` with no palette
    // by construction (`av1_filter_intra_allowed`), and its luma transform
    // types are coded from `fimode_to_intradir`'s row (`crate::tile::tx_row`).
    //
    // lane-fitu: on a SCREEN frame the candidate is priced at every transform
    // depth the ordinary intra search runs, each unit predicting recursively
    // off the units before it ([`Plane::code_tx_depth`]'s `filter_intra`
    // arm), so a block whose ordinary winner wanted a split transform is
    // compared against a filter arm that may split too.
    //
    // Measured (native BD table, lane-fitu): searching the depths on EVERY
    // frame moves screen -1.0/-0.4 and the bars rows -0.4/-0.7 but leaves
    // film A flat and film B +0.1/+0.5 WORSE -- the extra filter blocks are a
    // locally cheaper reconstruction that the film rows go on to predict from
    // (class `local-rd-on-references`) -- at +30..35% encoder wall. Screen
    // content is where it pays, and a non-screen frame keeps the depth-0-only
    // arm, byte for byte.
    let mut filter_intra: Option<u8> = None;
    if filter_intra_on()
        && palette.is_none()
        && let Some(class) = crate::decode::filter_intra_size_class(side)
    {
        let row = &cdf::FILTER_INTRA[class];
        // With the sequence bit on, EVERY `DC_PRED` block without a luma
        // palette carries the flag, so the incumbent pays its zero before the
        // comparison -- otherwise the candidate is charged a bit the block
        // spends either way (class `price-the-narrowing-not-the-table`).
        if mode == DC_PRED {
            cost += search.lambda * symbol_bits(row, 0);
        }
        let one_bits = symbol_bits(row, 1);
        let kept = luma.snapshot(x, y, side);
        let max_depth = if tx_select && search.screen { max_tx_depth(side) } else { 0 };
        let mut best: Option<(f64, u8, usize, (Vec<u8>, Vec<CoefCtx>), Vec<Coeff>)> = None;
        for fi in 0..5u8 {
            for depth in 0..=max_depth {
                let (levels, sse, bits) =
                    luma.code_tx_depth(at, DC_PRED, 0, depth, Some(fi), search, fctx);
                let cost_f = sse
                    + search.lambda
                        * (bits
                            + mode_bits[DC_PRED as usize]
                            + one_bits
                            + symbol_bits(&cdf::FILTER_INTRA_MODE, usize::from(fi))
                            + if tx_select { tx_depth_bits(side, depth) } else { 0.0 });
                if cost_f < cost && best.as_ref().is_none_or(|b| cost_f < b.0) {
                    best = Some((
                        cost_f,
                        fi,
                        depth,
                        luma.snapshot(x, y, side),
                        coeffs(&levels, side),
                    ));
                }
            }
        }
        if let Some((cost_f, fi, depth, snapshot, levels)) = best {
            cost = cost_f;
            mode = DC_PRED;
            angle_delta = 0;
            tx_depth = depth as u8;
            luma_coeffs = levels;
            luma.restore(x, y, side, &snapshot);
            filter_intra = Some(fi);
        } else {
            luma.restore(x, y, side, &kept);
        }
    }

    // Chroma searches its own mode over [`CHROMA_MODES`], one symbol for both
    // planes; what it costs still counts towards the partition decision.
    let ([u, v], uv_mode, chroma_cost, palette_uv, cfl_alphas) =
        search_chroma(chroma, luma, (x, y), side, chroma_set, search, mode, fctx);
    cost += chroma_cost;

    // The intrabc arm is priced against the WHOLE block -- luma and both
    // chroma planes, residual-free -- so it is compared after the chroma
    // search has added its own cost, not against the luma decision alone.
    if let Some(ibc) = ibc {
        if let Some((dv, cost_ibc, src)) =
            ibc_search(ibc, luma, chroma, (x, y), side, search.lambda)
        {
            if cost_ibc < cost {
                ibc_commit(luma, chroma, (x, y), src, side);
                ibc.last_dv = Some(dv);
                ibc.hits += 1;
                return (
                    BlockCoeffs {
                        skip: true,
                        dv: Some(dv),
                        ..BlockCoeffs::default()
                    },
                    cost_ibc,
                );
            }
        }
    }
    (
        BlockCoeffs {
            u,
            v,
            luma: luma_coeffs,
            mode,
            uv_mode,
            tx_depth,
            palette,
            palette_uv,
            filter_intra,
            cfl_alphas,
            angle_delta_y: angle_delta as i8,
            ..BlockCoeffs::default()
        },
        cost,
    )
}

/// The chroma modes the search offers, and the only ones it may offer: each
/// predicts from the row directly above and the column directly to the left
/// and nothing further, so none of them needs the above-right/below-left
/// `Reach` a chroma block is coded with `Reach::none()` for. The eight
/// directional modes do read past that, and `UV_CFL_PRED` needs the luma
/// reconstruction and its own alpha syntax; both are left out here.
const CHROMA_MODES: [u8; 7] = [
    DC_PRED,
    V_PRED,
    H_PRED,
    SMOOTH_PRED,
    SMOOTH_V_PRED,
    SMOOTH_H_PRED,
    PAETH_PRED,
];

/// The thirteen intra mode names, for the chroma fire count's print.
#[cfg(test)]
const UV_MODE_NAMES: [&str; 14] = [
    "DC", "V", "H", "D45", "D135", "D113", "D157", "D203", "D67", "SMOOTH", "SMOOTH_V",
    "SMOOTH_H", "PAETH", "CFL",
];

/// How many blocks each chroma mode has won, indexed by the mode itself --
/// the fire count that says whether the search below reaches its candidates
/// at all (the `gate-blind-to-feature` class), printed once per gate run.
static UV_MODE_HITS: [std::sync::atomic::AtomicUsize; 14] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 14];

/// How many blocks each CfL alpha magnitude (`1..=16`, indexed `mag - 1`) has
/// won, summed over both chroma planes -- the alpha histogram the gate prints
/// beside the CfL share (`gate-blind-to-feature`: a tool that fires at one
/// magnitude only is a search that never left its first candidate).
static CFL_ALPHA_HITS: [std::sync::atomic::AtomicUsize; 17] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 17];

/// Reads [`CFL_ALPHA_HITS`] and zeroes it. Index 0 is "this plane took alpha
/// zero on a CfL block", `1..=16` the magnitudes.
#[cfg(test)]
pub(crate) fn take_cfl_alpha_hits() -> [usize; 17] {
    std::array::from_fn(|m| CFL_ALPHA_HITS[m].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many directional luma blocks won at each `angle_delta_y`, indexed
/// `delta + 3` -- the histogram that says whether the refinement below ever
/// leaves zero (`gate-blind-to-feature`).
static ANGLE_DELTA_HITS: [std::sync::atomic::AtomicUsize; 7] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 7];

/// Reads [`ANGLE_DELTA_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_angle_delta_hits() -> [usize; 7] {
    std::array::from_fn(|d| ANGLE_DELTA_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Whether the luma search refines a directional winner's `angle_delta_y`
/// (`EC_AV1_ANGLE=0` switches it off, the lane's own on/off pair).
fn angle_delta_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_ANGLE").ok().map(|v| v != "0")
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::ANGLE))
}

/// Whether the chroma search offers `UV_CFL_PRED` at all (`EC_AV1_CFL=0`
/// switches it off, which is how the lane measured its own on/off pair).
fn cfl_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_CFL").ok().map(|v| v != "0")
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::CFL))
}

/// Whether the intra search offers the five recursive filter-intra modes at
/// all, and with them whether the sequence header sets `enable_filter_intra`
/// (`EC_AV1_FILTER_INTRA=0` switches both off, which is how the lane measured
/// its own on/off pair -- and how every byte pin written before it still
/// reproduces). It is also a speed lever: on at preset 0, off above it.
pub(crate) fn filter_intra_on() -> bool {
    #[cfg(test)]
    if let Some(forced) = FILTER_INTRA_FORCE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_FILTER_INTRA").ok().map(|v| v != "0")
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::FILTER_INTRA))
}

/// Reads [`UV_MODE_HITS`] and zeroes it, so a gate can attribute the counts
/// to its own encode.
#[cfg(test)]
pub(crate) fn take_uv_mode_hits() -> [usize; 14] {
    std::array::from_fn(|m| UV_MODE_HITS[m].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Searches both chroma planes over [`CHROMA_MODES`] under one shared mode --
/// `uv_mode` is a single symbol covering U and V -- and commits the cheapest.
/// Returns each plane's levels, the mode, and what the pair cost (squared
/// error plus lambda times bits, the same currency the luma search speaks).
///
/// The chroma symbol itself was already priced at `DC_PRED` by [`mode_bits`],
/// which is what the luma mode decision was taken against, so what a
/// candidate owes here is only the *difference* from that -- plus, for `V`/`H`
/// (directional, spec `read_intra_angle_info`), the zero `angle_delta_uv` the
/// writer then has to code.
#[allow(clippy::too_many_arguments)]
/// How many of [`CHROMA_MODES`] the chroma search runs a full trial pair on,
/// ranked by the SAD of their predictions summed over both planes (`DC_PRED`
/// always kept, the same rule [`prune_by_sad`] applies to luma). `None`
/// trials all seven, as before this lever.
///
/// The census (`EC_AV1_CENSUS=1`) is what justifies the lever: chroma was
/// the search's largest trial population by far -- seven modes times two
/// planes for every intra block, against 3.7 luma trials -- and the RD
/// winner sat at SAD rank 0 for 67-77% of blocks and inside the top three
/// for 88%. `EC_AV1_CHROMA_K` sweeps it in any build.
fn chroma_top_k() -> Option<usize> {
    static ENV: std::sync::LazyLock<Option<Option<usize>>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_CHROMA_K")
            .ok()
            .map(|v| v.parse::<usize>().ok().filter(|&k| k > 0))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::CHROMA_K))
}

/// [`chroma_top_k`]'s default. `None`: no `K` cleared the keep rule on the BD
/// gate. Swept over 2/3/4/5 (vs libaom, base +121.1/+146.1/+79.3):
/// K=2 is +122.3/+147.5/+79.8 for -7.0/-8.3/-7.9% wall, K=3 +121.4/+146.8/
/// +80.6 for -5.4/-5.2/-4.6%, K=4 +120.5/+145.2/+79.3 (BD-neutral or better)
/// for only -3.4% wall and -2.8% instructions, K=5 +122.0/+145.1/+80.0. The
/// BD swings by about a point in either direction as near-tie chroma modes
/// flip, which is the measurement's own sensitivity, not a trend: nothing
/// here buys >=5% wall at <=+0.3 BD, so the default stays unpruned.
/// RE-MEASURED on lane-av1rejudge at [`LAMBDA_SCALE`] 0.05 with warp on:
/// K=4 is +17.1/-0.1, +47.1/+19.2, +51.6/-13.8 against the base's
/// +16.7/-0.5, +47.0/+19.1, +51.3/-14.3 -- every row worse. Still `None`.
pub(crate) const CHROMA_TOP_K: Option<usize> = None;

fn search_chroma(
    chroma: &mut [Plane; 2],
    // The luma plane's RECONSTRUCTION under this block, already committed by
    // [`Plane::search_block`] -- the signal a `UV_CFL_PRED` candidate scales.
    luma: &Plane,
    (x, y): (usize, usize),
    side: usize,
    set: TxbSet,
    search: &Search,
    luma_mode: u8,
    fctx: &crate::decode::FrameCtx,
) -> (
    [Vec<Coeff>; 2],
    u8,
    f64,
    Option<crate::tile::PaletteUv>,
    Option<(i32, i32)>,
) {
    let at = At {
        x: x / 2,
        y: y / 2,
        side: side / 2,
        reach: Reach::none(),
        set,
    };
    // The census's cheap proxy for a chroma mode: the SAD of its prediction
    // summed over both planes, ranked by the same rule the luma histogram
    // uses. Only built under `EC_AV1_CENSUS`.
    // The SAD of each mode's prediction, summed over both planes -- the
    // census's ranking rule, and what [`chroma_top_k`] prunes by.
    let sad_scores = |chroma: &[Plane; 2]| -> Vec<(f64, u8)> {
        let u = chroma[0].intra_scores(at, &CHROMA_MODES, &[0.0; 13], 0.0, fctx);
        let v = chroma[1].intra_scores(at, &CHROMA_MODES, &[0.0; 13], 0.0, fctx);
        u.iter()
            .zip(v.iter())
            .map(|(&(us, mode), &(vs, _))| (us + vs, mode))
            .collect()
    };
    let modes: Vec<u8> = match chroma_top_k() {
        Some(k) if k < CHROMA_MODES.len() => prune_by_sad(&CHROMA_MODES, sad_scores(chroma), k),
        _ => CHROMA_MODES.to_vec(),
    };
    let uv_cdf = &cdf::UV_MODE_CFL[usize::from(luma_mode)];
    let dc_bits = symbol_bits(uv_cdf, usize::from(DC_PRED));
    let ranking: Vec<(f64, u8)> = if census_on() {
        sad_scores(chroma)
    } else {
        Vec::new()
    };
    census_add(2, 1);
    census_add(3, modes.len());
    let mut best: Option<(f64, u8, Trial, Trial)> = None;
    for mode in modes {
        let mut bits = symbol_bits(uv_cdf, usize::from(mode)) - dc_bits;
        if (V_PRED..=D67_PRED).contains(&mode) {
            bits += symbol_bits(&cdf::ANGLE_DELTA[usize::from(mode - V_PRED)], 3);
        }
        let tx_type = crate::decode::default_intra_tx_type(mode);
        let u = chroma[0].trial_typed(at, mode, 0, search.base_q_idx, search.deadzone, tx_type, fctx);
        let v = chroma[1].trial_typed(at, mode, 0, search.base_q_idx, search.deadzone, tx_type, fctx);
        let cost = u.sse + v.sse + search.lambda * (bits + u.bits + v.bits);
        if best.as_ref().is_none_or(|(b, ..)| cost < *b) {
            best = Some((cost, mode, u, v));
        }
    }
    let (mut cost, mut mode, mut u, mut v) = best.expect("CHROMA_MODES is not empty");
    // The chroma palette arm (spec 5.11.46's plane-1 half): one k-means over
    // U -- the same 1-D Lloyd [`palette_candidates`] runs for luma, which is
    // what keeps `u_colors` ascending and distinct the way
    // `read_palette_colors_uv`'s merge/delta reader needs -- and V taken as
    // each cluster's own mean, since the colour-index map is SHARED by the
    // two planes. Priced against the best ordinary chroma mode above, so a
    // block only takes one when it beats it; a palette block's chroma mode is
    // `UV_DC_PRED` by construction.
    let mut palette_uv = None;
    if search.screen && crate::decode::palette_bsize_ctx(side).is_some() {
        debug_assert_eq!(crate::tile::palette_uv_side(side), at.side);
        let plane_source = |p: &Plane| -> Vec<u8> {
            (at.y..at.y + at.side)
                .flat_map(|row| p.source[row * p.width + at.x..][..at.side].to_vec())
                .collect()
        };
        let (u_source, v_source) = (plane_source(&chroma[0]), plane_source(&chroma[1]));
        for pal in palette_candidates(&u_source) {
            let n = usize::from(pal.size);
            // V's base colour per cluster: the mean of the samples the shared
            // map assigns to it (an empty cluster cannot happen -- every
            // colour `palette_candidates` returns is some pixel's nearest).
            let mut sums = [0u32; 8];
            let mut counts = [0u32; 8];
            for (&i, &sample) in pal.map.iter().zip(v_source.iter()) {
                let k = usize::from(i).min(n - 1);
                sums[k] += u32::from(sample);
                counts[k] += 1;
            }
            let mut v_colors = [0u16; 8];
            for k in 0..n {
                v_colors[k] = if counts[k] > 0 {
                    ((sums[k] + counts[k] / 2) / counts[k]) as u16
                } else {
                    0
                };
            }
            let candidate = crate::tile::PaletteUv {
                size: pal.size,
                u_colors: pal.colors,
                v_colors,
                map: pal.map,
            };
            let predict = |colors: &[u16; 8]| -> Vec<u8> {
                candidate
                    .map
                    .iter()
                    .map(|&i| colors[usize::from(i).min(n - 1)] as u8)
                    .collect()
            };
            let (pu, pv) = (predict(&candidate.u_colors), predict(&candidate.v_colors));
            let u_trial = chroma[0].code_from_prediction(
                at.x, at.y, at.side, &pu, false, search.base_q_idx, search.deadzone, set,
            );
            let v_trial = chroma[1].code_from_prediction(
                at.x, at.y, at.side, &pv, false, search.base_q_idx, search.deadzone, set,
            );
            let cost_p = u_trial.sse
                + v_trial.sse
                + search.lambda
                    * (u_trial.bits
                        + v_trial.bits
                        + crate::tile::palette_uv_bits(&candidate, side));
            if cost_p < cost {
                cost = cost_p;
                mode = DC_PRED;
                u = u_trial;
                v = v_trial;
                palette_uv = Some(candidate);
            }
        }
    }
    // The chroma-from-luma arm (spec 5.11.45, libaom `cfl_rd_pick_alpha`):
    // `UV_CFL_PRED` predicts as `DC_PRED` (`get_uv_mode`, spec 9.3) and then
    // nudges every sample by `alpha_q3` times the block's subsampled,
    // average-subtracted luma reconstruction. Offered only where
    // `is_cfl_allowed` (spec 5.11.5) does -- a luma block at most 32x32 --
    // and priced against the best ordinary chroma mode above, palette
    // included, so a block only takes it when it beats both.
    let mut cfl_alphas = None;
    if side <= 32 && cfl_on() {
        // The AC signal, through the decoder's own
        // `cfl_luma_subsampling_420_lbd_c` + `subtract_average_c` body.
        let ac = crate::decode::cfl_ac_q3_at(x, y, side, side, |lx, ly| {
            i32::from(luma.reconstruction[ly * luma.width + lx])
        });
        let dc_prediction = |plane: &Plane| -> Vec<u8> {
            let (mut above_buf, mut left_buf) = ([0u8; 2 * BLOCK], [0u8; 2 * BLOCK]);
            let (above, left, corner) =
                plane.edges_into(at.x, at.y, at.side, at.reach, &mut above_buf, &mut left_buf);
            let mut prediction = vec![0u8; at.side * at.side];
            intra_predict_u8(
                DC_PRED, 0, above, left, corner, at.side, at.side, false, false, &mut prediction,
                fctx,
            );
            prediction
        };
        let bases = [dc_prediction(&chroma[0]), dc_prediction(&chroma[1])];
        // `av1_cfl_predict_block` (cfl.c): clip the nudged prediction, then
        // add the residual -- which is what [`Plane::code_from_prediction`]
        // does with what this returns.
        let apply = |base: &[u8], alpha: i32| -> Vec<u8> {
            base.iter()
                .zip(ac.iter())
                .map(|(&b, &a)| {
                    (i32::from(b) + crate::decode::cfl_scaled(alpha, a)).clamp(0, 255) as u8
                })
                .collect()
        };
        // The least-squares alpha, then its two neighbours: minimising
        // `sum (source - base - alpha*ac/64)^2` over a continuous alpha is one
        // dot-product ratio, and the quantised optimum is one of the two
        // integers around it (the clamp and the Q6 rounding are what the SSE
        // check below re-decides). libaom sweeps all sixteen magnitudes under
        // full RD; three predictions per plane buy nearly the same alpha for a
        // thirtieth of the search.
        let pick_alpha = |plane: &Plane, base: &[u8]| -> i32 {
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for row in 0..at.side {
                for col in 0..at.side {
                    let i = row * at.side + col;
                    let d = f64::from(plane.source[(at.y + row) * plane.width + at.x + col])
                        - f64::from(base[i]);
                    num += d * f64::from(ac[i]);
                    den += f64::from(ac[i]) * f64::from(ac[i]);
                }
            }
            if den == 0.0 {
                return 0;
            }
            let ideal = 64.0 * num / den;
            let mut best = (plane.block_sse(at.x, at.y, at.side, base), 0i32);
            for alpha in [ideal.floor() as i64, ideal.ceil() as i64] {
                let alpha = alpha.clamp(-16, 16) as i32;
                if alpha == 0 {
                    continue;
                }
                let sse = plane.block_sse(at.x, at.y, at.side, &apply(base, alpha));
                if sse < best.0 {
                    best = (sse, alpha);
                }
            }
            best.1
        };
        let picked = [
            pick_alpha(&chroma[0], &bases[0]),
            pick_alpha(&chroma[1], &bases[1]),
        ];
        if picked != [0, 0] {
            let cfl_bits = symbol_bits(uv_cdf, crate::tile::UV_CFL_PRED) - dc_bits;
            let trial = |plane_idx: usize, alpha: i32| -> Trial {
                let prediction = if alpha == 0 {
                    bases[plane_idx].clone()
                } else {
                    apply(&bases[plane_idx], alpha)
                };
                chroma[plane_idx].code_from_prediction(
                    at.x,
                    at.y,
                    at.side,
                    &prediction,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    set,
                )
            };
            // At least one alpha must be nonzero: `cfl_alpha_signs` has no
            // (ZERO, ZERO) joint value ([`crate::tile::cfl_joint_sign`]).
            let combos: [(i32, i32); 3] = [
                (picked[0], picked[1]),
                (picked[0], 0),
                (0, picked[1]),
            ];
            for &(alpha_u, alpha_v) in &combos {
                if (alpha_u, alpha_v) == (0, 0) {
                    continue;
                }
                let cu = trial(0, alpha_u);
                let cv = trial(1, alpha_v);
                let bits =
                    cfl_bits + crate::tile::cfl_alpha_bits(alpha_u, alpha_v) + cu.bits + cv.bits;
                let cost_cfl = cu.sse + cv.sse + search.lambda * bits;
                if cost_cfl < cost {
                    cost = cost_cfl;
                    mode = crate::tile::UV_CFL_PRED as u8;
                    u = cu;
                    v = cv;
                    palette_uv = None;
                    cfl_alphas = Some((alpha_u, alpha_v));
                }
            }
        }
    }
    if let Some((alpha_u, alpha_v)) = cfl_alphas {
        for alpha in [alpha_u, alpha_v] {
            CFL_ALPHA_HITS[alpha.unsigned_abs() as usize]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if census_on() {
        CHROMA_RANK[sad_rank(&ranking, mode).min(6)]
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    UV_MODE_HITS[usize::from(mode)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    chroma[0].commit(at.x, at.y, at.side, &u);
    chroma[1].commit(at.x, at.y, at.side, &v);
    (
        [coeffs(&u.levels, at.side), coeffs(&v.levels, at.side)],
        mode,
        cost,
        palette_uv,
        cfl_alphas,
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
/// The compound MV stack of each `LAST` + extra-reference pair at one leaf,
/// as [`code_square_inter`] wants them: empty on a frame that codes no
/// `reference_select`, or when the leaf compound candidates are switched off.
/// One helper because THREE leaf call sites need it -- the split quadrants
/// and, since lane-av1comp4, both straddling-edge sites, which used to pass
/// `&[]` and so coded no compound block anywhere along the frame's true edge.
#[allow(clippy::too_many_arguments)]
fn leaf_compound_stacks<'a>(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    reference_select: bool,
    golden: Option<&'a Picture>,
    altref: Option<&'a Picture>,
) -> Vec<(i8, &'a Picture, crate::mvstack::CompoundMvStack)> {
    if !(reference_select && leaf_compound()) {
        return Vec::new();
    }
    [
        (crate::mvstack::GOLDEN_FRAME, golden),
        (crate::mvstack::ALTREF_FRAME, altref),
    ]
    .into_iter()
    .filter_map(|(r, pic)| {
        pic.map(|p| {
            (
                r,
                p,
                crate::mvstack::find_mv_stack_compound(
                    grid, mi_row, mi_col, bw4, bh4,
                    (crate::mvstack::LAST_FRAME, r), mi_cols, mi_rows,
                    grid.sign_bias_table(), &[(0, 0); 7], None,
                ),
            )
        })
    })
    .collect()
}

/// The OBMC prediction of one square single-reference block: this block's own
/// translation prediction (`mc::predict`) with every bordering above/left
/// neighbour blended into the overlap strips by the DECODER's own plan and
/// blend ([`crate::decode::obmc_plan`] / [`crate::decode::obmc_run`]), so what
/// the search prices is what a decoder reconstructs -- never a second
/// implementation of the blend. `None` when the block has no overlappable
/// neighbour at all (the writer would then code no `motion_mode` symbol) or
/// when the plan cannot be built (a neighbour naming a reference this encode
/// does not hold).
#[allow(clippy::too_many_arguments)]
fn obmc_prediction(
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    refs: &[Option<&Picture>; 8],
    // This block's OWN reference planes, luma then both chroma, each as
    // (samples, stride, true width, true height).
    ref_planes: [(&[u16], usize, usize, usize); 3],
    frame_width: usize,
    fctx: &crate::decode::FrameCtx,
) -> Option<[Vec<u8>; 3]> {
    let n4 = side / 4;
    if !crate::decode::has_overlappable_neighbour(grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows) {
        return None;
    }
    let refpix = crate::decode::RefPix::ready(*refs);
    let plan = crate::decode::obmc_plan(
        grid,
        &[],
        &[],
        mi_row,
        mi_col,
        n4,
        n4,
        mi_rows,
        mi_cols,
        side,
        side,
        side,
        side / 2,
        x,
        y,
        x / 2,
        y / 2,
        &refpix,
        Some(mc::InterpFilterKind::Regular),
        frame_width,
    )
    .ok()?;
    let mut pred = [
        vec![0u16; side * side],
        vec![0u16; side * side / 4],
        vec![0u16; side * side / 4],
    ];
    for (i, r) in ref_planes.into_iter().enumerate() {
        let luma_plane = i == 0;
        let (px, py) = if luma_plane { (x, y) } else { (x / 2, y / 2) };
        let s = if luma_plane { side } else { side / 2 };
        mc::predict(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(px, mv.1, luma_plane),
            mv_to_q4(py, mv.0, luma_plane),
            s,
            s,
            &mut pred[i],
            fctx,
        );
    }
    let [mut py_buf, mut pu_buf, mut pv_buf] = pred;
    crate::decode::obmc_run(&plan, &refpix, &mut py_buf, &mut pu_buf, &mut pv_buf, fctx);
    let u8s = |b: &[u16]| b.iter().map(|&v| v as u8).collect::<Vec<u8>>();
    Some([u8s(&py_buf), u8s(&pu_buf), u8s(&pv_buf)])
}

fn code_square_inter(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
    // The COMPOUND stack of each `LAST` + extra-reference pair, built at THIS
    // leaf's own `bw4`/`bh4` (empty on a frame that does not code
    // `reference_select`, and at the straddling call sites).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    // lane-av1obmc2: the mi grid this leaf's neighbours were published into
    // (`record_mi`, in the writer's own coding order) and this leaf's place in
    // it -- what the OBMC candidate's eligibility walk and neighbour plan
    // read, the same state the tile writer reads when it codes the leaf's
    // `motion_mode` symbol.
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    // Every picture an OBMC neighbour can name: a leaf only searches LAST,
    // but the neighbours it blends may be GOLDEN/ALTREF blocks.
    refs: &[Option<&Picture>; 8],
    fctx: &crate::decode::FrameCtx,
) -> (BlockCoeffs, f64) {
    let (intra_block, intra_cost) =
        code_square(
            luma,
            chroma,
            (x, y),
            side,
            search,
            mode_bits,
            tx_select() && tx_select_inter(),
            None,
            fctx,
        );
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
        luma_set, fctx,
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
        chroma_set, fctx,
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
        chroma_set, fctx,
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
    // A motion search of this leaf's own (lane-av1rd2): before it, a leaf
    // only ever took the stack's NEARESTMV candidate or went intra, so a
    // 32x32 block splitting because its four quarters MOVE differently could
    // not code the movement it split for. Same candidate `search_inter_block`
    // prices for a whole block -- `motion::search` from `pred_mv`, rounded to
    // a vector the residual syntax can name -- at this leaf's own side.
    let mut best: (f64, Option<(Trial, Trial, Trial, bool, InterInfo)>) = (inter_cost, None);
    // The `NEARESTMV` candidate `inter_cost` prices, named once so the OBMC
    // re-price below can hand it back as the winner's own mode info.
    let nearest_info = InterInfo {
        ref1: None,
        mv1: (0, 0),
        ref_frame: crate::mvstack::LAST_FRAME,
        mode: InterMode::NearestMv,
        mv,
        ref_mv_idx: 0,
    };
    // What the current SINGLE-REFERENCE winner pays in mode/reference/mv
    // syntax, apart from its residual: the OBMC candidate re-prices the same
    // syntax against a different prediction, so both sides of that comparison
    // carry it.
    let mut single_syntax = intra_inter_bits(true) + single_ref_bits + mode_bits_inter;
    // This leaf's own `NEWMV` vector, kept whether or not it won the
    // single-reference comparison: it seeds the compound half-new candidate
    // below, exactly as `search_inter_block` seeds its own from the searches
    // it already ran.
    let mut searched_new: Option<(i32, i32)> = None;
    // What that search cost, kept for the second reference's own early-out
    // below (`None` when this leaf ran no search at all).
    let mut searched_new_cost: Option<f64> = None;
    let source_block = luma.source_block(x, y, side);
    if leaf_new_mv() {
        let (seeds, seed_n) = mv_seeds(stack);
        let found = motion::search(
            ref_luma.0,
            ref_luma.1,
            ref_luma.2,
            ref_luma.3,
            &source_block,
            x,
            y,
            side,
            side,
            stack.pred_mv,
            &seeds[..seed_n],
            search.lambda, fctx,
            1,
        );
        let new_mv = round_to_valid_mv(found.mv, stack.pred_mv);
        searched_new_cost = Some(found.cost);
        if let Some(mv_bits) = mv_residual_bits(new_mv, stack.pred_mv) {
            searched_new = Some(new_mv);
            // Same reuse as `search_inter_block`: a NEWMV equal to the
            // stack's NEARESTMV is the same three trials, already run above.
            let (luma_new, u_new, v_new) = if new_mv == mv {
                (luma_trial.clone(), u.clone(), v.clone())
            } else {
                (
                    mc_trial(
                        luma, x, y, side, new_mv, true, ref_luma.0, ref_luma.1, ref_luma.2,
                        ref_luma.3, false, search.base_q_idx, search.deadzone, luma_set, fctx,
                    ),
                    mc_trial(
                        &chroma[0], x / 2, y / 2, side / 2, new_mv, false, ref_u.0, ref_u.1,
                        ref_u.2, ref_u.3, false, search.base_q_idx, search.deadzone, chroma_set,
                        fctx,
                    ),
                    mc_trial(
                        &chroma[1], x / 2, y / 2, side / 2, new_mv, false, ref_v.0, ref_v.1,
                        ref_v.2, ref_v.3, false, search.base_q_idx, search.deadzone, chroma_set,
                        fctx,
                    ),
                )
            };
            let skip_new = luma_new.levels.iter().all(|&l| l == 0)
                && u_new.levels.iter().all(|&l| l == 0)
                && v_new.levels.iter().all(|&l| l == 0);
            let drl_bits = if stack.entries.len() > 1 {
                symbol_bits(&cdf::DRL_MODE[stack.drl_ctx[0]], 0)
            } else {
                0.0
            };
            let cost_new = luma_new.sse
                + u_new.sse
                + v_new.sse
                + search.lambda
                    * (skip_bits(skip_new)
                        + intra_inter_bits(true)
                        + single_ref_bits
                        + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                        + drl_bits
                        + mv_bits
                        + if skip_new {
                            0.0
                        } else {
                            luma_new.bits + u_new.bits + v_new.bits
                        });
            if cost_new < best.0 {
                single_syntax = intra_inter_bits(true)
                    + single_ref_bits
                    + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                    + drl_bits
                    + mv_bits;
                best = (
                    cost_new,
                    Some((
                        luma_new,
                        u_new,
                        v_new,
                        skip_new,
                        InterInfo {
                            ref1: None,
                            mv1: (0, 0),
                            ref_frame: crate::mvstack::LAST_FRAME,
                            mode: InterMode::NewMv,
                            mv: new_mv,
                            ref_mv_idx: 0,
                        },
                    )),
                );
            }
        }
    }

    // The COMPOUND candidates at this leaf (lane-av1comp3), the same set
    // `search_inter_block` prices for a whole 32x32 block:
    // `NEAREST_NEARESTMV`, `GLOBAL_GLOBALMV`, `NEW_NEARESTMV` off the vector
    // this leaf already searched, and -- since lane-av1comp4 gave the leaf a
    // SECOND-reference search of its own -- `NEAREST_NEWMV` and `NEW_NEWMV`
    // too. Priced with the same static-CDF approximation, plus the
    // compound-only syntax the writer emits.
    for (ref1, g, cstack) in compound.iter().filter(|_| leaf_compound()) {
        let (ref1, g) = (*ref1, *g);
        let uni =
            (crate::mvstack::BWDREF_FRAME..=crate::mvstack::ALTREF_FRAME).contains(&ref1);
        let pair_bits = symbol_bits(&cdf::COMP_MODE[0], 1)
            + if uni {
                symbol_bits(&cdf::COMP_REF_TYPE[0], 1)
                    + symbol_bits(&cdf::COMP_REF[0][0], 0)
                    + symbol_bits(&cdf::COMP_REF[0][1], 0)
                    + symbol_bits(&cdf::COMP_BWDREF[0][0], 1)
            } else {
                symbol_bits(&cdf::COMP_REF_TYPE[0], 0)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][0], 0)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][1], 1)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][2], 1)
            };
        let mode_ctx =
            cdf::COMPOUND_MODE_CTX_MAP[cstack.ref_mv_ctx >> 1][cstack.new_mv_ctx.min(4)];
        let mode_bits_of =
            |mode: usize| pair_bits + symbol_bits(&cdf::INTER_COMPOUND_MODE[mode_ctx], mode);
        let mut ccands: Vec<(((i32, i32), (i32, i32)), f64, InterInfo)> = vec![
            (
                cstack.nearest_mv,
                mode_bits_of(0),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearestNearestMv,
                    mv: cstack.nearest_mv.0,
                    mv1: cstack.nearest_mv.1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
            (
                ((0, 0), (0, 0)),
                mode_bits_of(6),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::GlobalGlobalMv,
                    mv: (0, 0),
                    mv1: (0, 0),
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
        ];
        // Every compound MV residual here is priced against compound stack
        // entry 0, which is what `assign_compound_mv` predicts from at
        // `ref_mv_idx = 0` -- so it is also what the second-reference search
        // below seeds from and rounds to, and no candidate it finds can then
        // fail `mv_residual_bits`.
        let cbase = cstack
            .entries
            .first()
            .map_or(cstack.nearest_mv, |e| (e.mv0, e.mv1));
        // This leaf's SECOND-reference motion search (lane-av1comp4). Without
        // it a leaf could only name a stack vector for the second half, and
        // screen capture showed the consequence: every leaf compound block
        // there was `NEAREST_NEARESTMV`. Skipped, like `search_inter_block`'s
        // extra-reference `NEWMV`, when this reference's own
        // `NEAREST_NEARESTMV` vector already prices `margin` times better
        // than the `LAST` search of this same leaf did -- one candidate
        // evaluation standing in for a whole search.
        let margin = leaf_second_new_margin();
        let mut second_new: Option<(i32, i32)> = None;
        if leaf_second_new_mv() {
            let skip = margin > 0.0
                && searched_new_cost.is_some_and(|last| {
                    motion::cost_at(
                        &g.y, g.width, luma.true_width, luma.true_height, &source_block, x, y,
                        side, side, cstack.nearest_mv.1, cbase.1, search.lambda, fctx,
                    ) * margin
                        <= last
                });
            if !skip {
                let mut seeds = [(0, 0); 4];
                let mut seed_n = 1;
                for e in cstack.entries.iter().take(3) {
                    seeds[seed_n] = e.mv1;
                    seed_n += 1;
                }
                let found = motion::search(
                    &g.y, g.width, luma.true_width, luma.true_height, &source_block, x, y, side,
                    side, cbase.1, &seeds[..seed_n], search.lambda, fctx, ref_distance(ref1),
                );
                second_new = Some(round_to_valid_mv(found.mv, cbase.1));
            }
        }
        if let Some(new_mv) = searched_new {
            if let Some(b0) = mv_residual_bits(new_mv, cbase.0) {
                ccands.push((
                    (new_mv, cstack.nearest_mv.1),
                    mode_bits_of(3) + b0,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NewNearestMv,
                        mv: new_mv,
                        mv1: cstack.nearest_mv.1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
            }
        }
        // `NEAREST_NEWMV` and `NEW_NEWMV`: the NEW halves are the two
        // searches this leaf ran, each residual priced against `cbase`.
        if let Some(mv1) = second_new {
            if let Some(b1) = mv_residual_bits(mv1, cbase.1) {
                ccands.push((
                    (cstack.nearest_mv.0, mv1),
                    mode_bits_of(2) + b1,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NearestNewMv,
                        mv: cstack.nearest_mv.0,
                        mv1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
                if let Some((new_mv, b0)) =
                    searched_new.and_then(|m| mv_residual_bits(m, cbase.0).map(|b| (m, b)))
                {
                    ccands.push((
                        (new_mv, mv1),
                        mode_bits_of(7) + b0 + b1,
                        InterInfo {
                            ref_frame: crate::mvstack::LAST_FRAME,
                            mode: InterMode::NewNewMv,
                            mv: new_mv,
                            mv1,
                            ref1: Some(ref1),
                            ref_mv_idx: 0,
                        },
                    ));
                }
            }
        }
        ccands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        ccands.dedup_by_key(|c| c.0);
        let g_luma = (g.y.as_slice(), g.width, luma.true_width, luma.true_height);
        let g_u = (
            g.u.as_slice(),
            g.width / 2,
            chroma[0].true_width,
            chroma[0].true_height,
        );
        let g_v = (
            g.v.as_slice(),
            g.width / 2,
            chroma[1].true_width,
            chroma[1].true_height,
        );
        for (mvs, bits, info) in ccands {
            let luma_c = mc_trial_compound(
                luma, x, y, side, mvs, true,
                (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3), g_luma,
                search.base_q_idx, search.deadzone, luma_set, fctx,
            );
            let u_c = mc_trial_compound(
                &chroma[0], x / 2, y / 2, side / 2, mvs, false,
                (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3), g_u,
                search.base_q_idx, search.deadzone, chroma_set, fctx,
            );
            let v_c = mc_trial_compound(
                &chroma[1], x / 2, y / 2, side / 2, mvs, false,
                (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3), g_v,
                search.base_q_idx, search.deadzone, chroma_set, fctx,
            );
            let skip_c = luma_c.levels.iter().all(|&l| l == 0)
                && u_c.levels.iter().all(|&l| l == 0)
                && v_c.levels.iter().all(|&l| l == 0);
            let cost_c = luma_c.sse
                + u_c.sse
                + v_c.sse
                + search.lambda
                    * (skip_bits(skip_c)
                        + intra_inter_bits(true)
                        + bits
                        + if skip_c { 0.0 } else { luma_c.bits + u_c.bits + v_c.bits });
            if cost_c < best.0 {
                best = (cost_c, Some((luma_c, u_c, v_c, skip_c, info)));
            }
        }
    }

    // lane-av1obmc2: the OBMC candidate at a 16x16 / 8x8 LEAF (spec
    // `motion_mode == OBMC_CAUSAL`), offered to this leaf's single-reference
    // winner -- libaom's `motion_mode_allowed` refuses a compound block, and
    // the writer codes no symbol for one. The prediction is the decoder's own
    // plan and blend over the neighbours this leaf's `grid` already carries
    // (published by `record_mi` in the writer's coding order), so the
    // reconstruction priced here is the one a decoder rebuilds. lane-av1obmc
    // offered this only at 32x32, which is not where libaom cashes its OBMC
    // gain.
    let mut motion_won = 0u8;
    let single = match &best.1 {
        None => Some(nearest_info),
        Some((_, _, _, _, info)) if info.ref1.is_none() => Some(*info),
        Some(_) => None,
    };
    if let Some(info) = single.filter(|i| {
        (crate::envflags::env_flag!("EC_AV1_OBMC") || warp_on())
            && i.ref_frame == crate::mvstack::LAST_FRAME
    }) {
        let planes = [
            (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
            (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
            (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
        ];
        // Which alphabet this block's writer will code the symbol against
        // (`tile::write_motion_mode`, libaom `motion_mode_allowed`): the
        // 3-symbol one only under `allow_warped_motion` and at least one warp
        // sample. Mirrored here so the candidates are priced against the
        // table the tile really narrows.
        let warp_alphabet = warp_on()
            && crate::decode::num_proj_ref(
                grid,
                mi_row,
                mi_col,
                side / 4,
                side / 4,
                mi_cols,
                mi_rows,
                info.ref_frame,
                fctx,
            ) >= 1;
        let mm = |m: u8| motion_mode_bits(side, side, m, warp_alphabet);
        let mut candidates: Vec<(u8, [Vec<u8>; 3])> = Vec::new();
        if crate::envflags::env_flag!("EC_AV1_OBMC") && side >= obmc_min_side() {
            if let Some(pred) = obmc_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                side,
                info.mv,
                refs,
                planes,
                reference.width,
                fctx,
            ) {
                candidates.push((1, pred));
            }
        }
        if warp_alphabet {
            if let Some(pred) = warp_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                side,
                info.mv,
                info.ref_frame,
                planes,
                fctx,
            ) {
                candidates.push((2, pred));
            }
        }
        for (motion, pred) in candidates {
            let luma_o = luma.code_from_prediction(
                x, y, side, &pred[0], false, search.base_q_idx, search.deadzone, luma_set,
            );
            let u_o = chroma[0].code_from_prediction(
                x / 2, y / 2, side / 2, &pred[1], false, search.base_q_idx, search.deadzone,
                chroma_set,
            );
            let v_o = chroma[1].code_from_prediction(
                x / 2, y / 2, side / 2, &pred[2], false, search.base_q_idx, search.deadzone,
                chroma_set,
            );
            let skip_o = luma_o.levels.iter().all(|&l| l == 0)
                && u_o.levels.iter().all(|&l| l == 0)
                && v_o.levels.iter().all(|&l| l == 0);
            let cost_o = luma_o.sse
                + u_o.sse
                + v_o.sse
                + search.lambda
                    * (skip_bits(skip_o)
                        + single_syntax
                        + mm(motion)
                        + if skip_o { 0.0 } else { luma_o.bits + u_o.bits + v_o.bits });
            // The incumbent pays the motion_mode symbol IT would code, so
            // both sides of the comparison carry that syntax.
            if cost_o < best.0 + search.lambda * mm(motion_won) {
                motion_won = motion;
                best = (cost_o, Some((luma_o, u_o, v_o, skip_o, info)));
            }
        }
    }

    if let (cost, Some((luma_new, u_new, v_new, skip_new, info))) = best {
        if cost < intra_cost {
            // A compound winner commits the flat trial it was priced from:
            // `commit_inter_luma`'s var-tx trial set is single-reference
            // (`mc_trial`), so a compound leaf codes `tx_depth = 0`.
            // lane-av1comp4 BUILT that upgrade (a `commit_inter_luma`
            // predicting each half-side trial from BOTH references, verified
            // byte-identical to this when its split is disabled) and MEASURED
            // it a loss: vs libaom +83.5/+99.7/+54.9 at no margin against the
            // +83.0/+99.9/+54.5 here, and +83.3/+100.2/+54.9, +83.8/+99.5/
            // +54.9, +83.3/+100.0/+54.9 at split margins 0.02/0.05/0.15. A
            // compound block's residual is small enough that the four extra
            // per-unit txb_skip/eob symbols cost more ladder bytes than this
            // encoder's static-CDF estimate prices them at, so the local RD
            // win does not convert. lane-av1txbits REBUILT it on top of a
            // search that prices against the tables the frame is really
            // written with (`tile::arm_pricing_cdfs`, the estimate error on
            // these very blocks measured down from +31% to +3%) and measured
            // it neutral-to-worse again: +78.8/+95.0/+54.5 and +36.5/+56.3/
            // +1.4 against the +78.8/+95.1/+54.5 and +36.1/+56.4/+1.4 of the
            // same build without it. The static-CDF estimate was NOT what
            // held the compound var-tx trial back. The flat trial stays.
            // An OBMC winner commits the flat trial too: `commit_inter_luma`
            // re-predicts each var-tx unit by plain translation, which is not
            // the prediction this candidate was priced from.
            // lane-av1txdepth REBUILT the compound trial set at LAMBDA_SCALE
            // 0.05 (both rejections above were measured at 0.1, the weight
            // the lambda lane showed was 2x too heavy): a compound winner is
            // offered the same var-tx split, its units predicted from BOTH
            // references (`mc_trial_compound`). RE-MEASURED native: film
            // 1080p +16.0/-1.1, film 2160p +46.6/+18.8, screen +51.4/-14.4
            // against the +16.2/-1.1, +46.6/+18.8, +51.3/-14.4 with it off --
            // one row 0.2 down, two flat, short of the two-down-one-flat keep
            // rule, so it stays behind `EC_AV1_COMP_VARTX` (default OFF)
            // rather than change the default on a single 0.2 row. It fires:
            // 37.0% / 11.1% / 32.9% of the compound blocks split. 640x384 for
            // the record: +70.5/+84.0/+48.9 and +28.9/+46.2/-3.7 against
            // +70.4/+85.3/+48.5 and +28.4/+47.4/-4.0 -- the 4K film row is
            // 1.3/1.2 down there, which is where the upgrade path is if this
            // is picked up again.
            let second = info.ref1.filter(|_| motion_won == 0).and_then(|r| {
                refs[r as usize].map(|g| {
                    (
                        info.mv1,
                        (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                    )
                })
            });
            let (levels, tx_depth, dcost) = if motion_won != 0
                || (info.ref1.is_some() && second.is_none())
            {
                luma.commit(x, y, side, &luma_new);
                (luma_new.levels.clone(), 0, 0.0)
            } else {
                commit_inter_luma(
                    luma, (x, y), side, info.mv,
                    (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                    search, &luma_new, skip_new, second, fctx,
                )
            };
            chroma[0].commit(x / 2, y / 2, side / 2, &u_new);
            chroma[1].commit(x / 2, y / 2, side / 2, &v_new);
            return (
                BlockCoeffs {
                    angle_delta_y: 0,
                    cfl_alphas: None,
                    filter_intra: None,
                    luma: coeffs(&levels, side),
                    u: coeffs(&u_new.levels, side / 2),
                    v: coeffs(&v_new.levels, side / 2),
                    mode: DC_PRED as u8,
                    uv_mode: DC_PRED,
                    skip: skip_new,
                    eight: None,
                    dv: None,
                    palette: None,
            palette_uv: None,
                    tx_depth,
                    inter: Some(info),
                    motion_mode: motion_won,
                },
                cost + dcost,
            );
        }
    }

    census_add(8, 1);
    if skip {
        census_add(10, 1);
    }
    if inter_cost >= intra_cost {
        census_add(9, 1);
    }
    if inter_cost < intra_cost {
        let (levels, tx_depth, dcost) = commit_inter_luma(
            luma, (x, y), side, mv, (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3), search, &luma_trial, skip, None, fctx,
        );
        chroma[0].commit(x / 2, y / 2, side / 2, &u);
        chroma[1].commit(x / 2, y / 2, side / 2, &v);
        (BlockCoeffs {
            angle_delta_y: 0,
            cfl_alphas: None,
            filter_intra: None,
            luma: coeffs(&levels, side),
            u: coeffs(&u.levels, side / 2),
            v: coeffs(&v.levels, side / 2),
            mode: DC_PRED as u8,
            uv_mode: DC_PRED,
            skip,
            eight: None,
            dv: None,
            palette: None,
            palette_uv: None,
            tx_depth,
            motion_mode: 0,
            inter: Some(InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv,
                ref_mv_idx: 0,
            }),
        }, inter_cost + dcost)
    } else {
        (intra_block, intra_cost)
    }
}

/// Publishes one coded inter-frame block into the `mi` grid the next block's
/// MV stack reads, over the `size` x `size` 4x4 units it covers. An intra
/// block casts no vote but is still a coded cell (see `code_square_inter`'s
/// note on `processed_rows`/`processed_cols`).
fn record_mi(grid: &mut MiGrid, mi_row: usize, mi_col: usize, size: u8, inter: Option<InterInfo>) {
    let info = match inter {
        Some(info) => MiInfo {
            is_inter: true,
            ref_frame: info.ref_frame,
            // A COMPOUND block votes with BOTH of its references and both of
            // its vectors, exactly as the tile writer publishes it: recording
            // a compound block as a single-reference one here left the
            // encoder's own grid disagreeing with the writer's from the first
            // compound block on, so every later stack it searched against was
            // not the stack the decoder derives.
            ref_frame1: info.ref1.unwrap_or(NO_REF1),
            mv1: mv16(info.mv1),
            mv: mv16(info.mv),
            // decode.rs' own `is_new_mv`: `NEWMV` on the single-reference
            // side, compound modes 2/3/7 on the other.
            is_new_mv: matches!(
                info.mode,
                InterMode::NewMv
                    | InterMode::NewNewMv
                    | InterMode::NearestNewMv
                    | InterMode::NewNearestMv
            ),
            size,
            size_h: size,
            is_global_mv0: false,
            is_global_mv1: false,
        },
        None => MiInfo {
            is_inter: false,
            ref_frame: -1,
            ref_frame1: NO_REF1,
            mv1: (0, 0),
            mv: (0, 0),
            is_new_mv: false,
            size,
            size_h: size,
            is_global_mv0: false,
            is_global_mv1: false,
        },
    };
    for dr in 0..usize::from(size) {
        for dc in 0..usize::from(size) {
            grid.set(mi_row + dr, mi_col + dc, info);
        }
    }
}

/// The reconstructed samples one 32x32 square covers in all three planes, so
/// that a partition trial can be undone.
/// One square of a plane, row-major, the inverse of [`Plane::restore`].
///
/// Same bytes as the `flat_map(|row| ..to_vec()).collect()` this replaces,
/// which allocated and freed a `Vec` per row of every block it copied --
/// `Plane::snapshot` runs once per partition trial.
fn rows_of(plane: &[u8], width: usize, x: usize, y: usize, side: usize) -> Vec<u8> {
    let mut out = vec![0u8; side * side];
    for row in 0..side {
        out[row * side..][..side].copy_from_slice(&plane[(y + row) * width + x..][..side]);
    }
    out
}

fn snapshot(
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    side: usize,
) -> [(Vec<u8>, Vec<CoefCtx>); 3] {
    [
        luma.snapshot(x, y, side),
        chroma[0].snapshot(x / 2, y / 2, side / 2),
        chroma[1].snapshot(x / 2, y / 2, side / 2),
    ]
}

/// Puts such a snapshot back.
fn restore(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    saved: &[(Vec<u8>, Vec<CoefCtx>); 3],
) {
    luma.restore(x, y, side, &saved[0]);
    chroma[0].restore(x / 2, y / 2, side / 2, &saved[1]);
    chroma[1].restore(x / 2, y / 2, side / 2, &saved[2]);
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
#[derive(Clone, Copy)]
struct Search<'a> {
    /// This frame's `allow_screen_content_tools` ([`screen_content`]): the
    /// luma search then offers a palette candidate beside the intra modes.
    screen: bool,
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
        _ => crate::speed::at(&crate::speed::PRUNE_K),
    }
}

// A per-thread override [`prune_top_k`] checks first, so one test can sweep
// `top_k` in-process without mutating the environment (forbidden here --
// `#![forbid(unsafe_code)]` -- and racy across parallel tests besides).
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
pub(crate) const PRUNE_TOP_K: Option<usize> = None;

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
        _ => crate::speed::at(&crate::speed::PRUNE_K_INTER),
    }
}

// A per-thread override [`prune_top_k_inter`] checks first, same reason as
// [`TEST_TOP_K_OVERRIDE`].
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
pub(crate) const INTER_PRUNE_TOP_K: Option<usize> = Some(3);

// Per-thread nanosecond counters `stage_timing_breakdown_inter` (and
// [`crate::mc::predict`], through [`stage_add`]) accumulate into, so an
// inter frame's cost can be attributed to a bucket without changing what
// the search actually does. Index: 0 = whole-call time inside
// [`motion::search`] (includes bucket 1's time, run from inside it), 1 =
// [`crate::mc::predict`] (every call, from the motion search's own
// candidates and from a committed inter block's final prediction alike),
// 2 = `forward_and_quantize` + `dequant_and_inverse`, 3 = `coeff_bits`.
#[cfg(test)]
thread_local! {
    static STAGE_NS: [std::cell::Cell<u64>; 4] = [
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
        std::cell::Cell::new(0),
    ];
}

/// lane-av1cap: the trial timers are `#[cfg(test)]`, and the BD gate and
/// every encoder test are `cfg(test)` builds -- so two `Instant::now()` ran
/// per transform trial and per `mc::predict` call whether or not anyone was
/// reading the breakdown (the vdso clock was 1.7% of the encoder's profile).
/// The clock is now read only when [`crate::par::stage_times`] is on, which
/// is what prints the breakdown in the first place.
#[cfg(test)]
pub(crate) fn stage_start() -> Option<std::time::Instant> {
    crate::par::stage_times().then(std::time::Instant::now)
}

#[cfg(test)]
pub(crate) fn stage_since(bucket: usize, t: Option<std::time::Instant>) {
    if let Some(t) = t {
        stage_add(bucket, t.elapsed());
    }
}

#[cfg(test)]
pub(crate) fn stage_add(bucket: usize, dur: std::time::Duration) {
    STAGE_NS.with(|c| c[bucket].set(c[bucket].get() + dur.as_nanos() as u64));
}

#[cfg(test)]
fn stage_reset() {
    crate::par::set_stage_times(true);
    STAGE_NS.with(|c| c.iter().for_each(|cell| cell.set(0)));
}

#[cfg(test)]
fn stage_read() -> [std::time::Duration; 4] {
    crate::par::set_stage_times(false);
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

/// The top-left `to` x `to` corner of a `from`-sided level grid -- the part
/// of a 64-point transform that is actually coded (everything outside it is
/// zeroed by [`crate::transform::quantize`] already, so this only reshapes).
fn coded_corner(levels: &[i32], from: usize, to: usize) -> Vec<i32> {
    let mut out = vec![0i32; to * to];
    for row in 0..to {
        out[row * to..][..to].copy_from_slice(&levels[row * from..][..to]);
    }
    out
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

thread_local! {
    /// The tile grid the next frame is coded with, as `(TileColsLog2,
    /// TileRowsLog2)` -- armed by [`crate::encoder::Av1Encoder`] from its
    /// [`crate::encoder::EncoderConfig`], the same way `crate::tile`'s
    /// `arm_cdef_idx`/`arm_lr`/`arm_sign_bias` arm the writers. `(0, 0)` is
    /// one tile per frame, what every caller that never arms anything gets
    /// and what every stream this crate wrote before tiles existed carries.
    static TILE_LOG2: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

/// Arms this thread's next frame encodes with a tile grid (log2 counts).
pub(crate) fn arm_tiles(cols_log2: u32, rows_log2: u32) {
    TILE_LOG2.with(|c| c.set((cols_log2, rows_log2)));
}

fn armed_tiles() -> (u32, u32) {
    let armed = TILE_LOG2.with(std::cell::Cell::get);
    if armed != (0, 0) {
        return armed;
    }
    // `EC_AV1_TILES=<cols_log2>[:<rows_log2>]` in a TEST build, so the BD gate
    // (which codes through `encode_sequence`, not the facade) can measure what
    // a tile grid costs without a knob of its own -- same shape as
    // `EC_AV1_LR`/`EC_AV1_PYRAMID`.
    match std::env::var("EC_AV1_TILES").ok() {
        Some(v) if cfg!(test) => {
            let mut f = v.split(':');
            let cols = f.next().and_then(|c| c.parse().ok()).unwrap_or(0);
            let rows = f.next().and_then(|r| r.parse().ok()).unwrap_or(0);
            (cols, rows)
        }
        _ => armed,
    }
}

/// Codes a frame's `tiles` tiles, each entirely on its own (see
/// [`crate::tile::TileRect`]), across [`crate::par::tile_threads`] workers of
/// the crate's own pool, and hands back their payloads in tile order plus
/// whatever `code` returned for tile `store` -- the CDF state
/// `context_update_tile_id` names. The result cannot depend on the thread
/// count: each job writes one tile from the frame's own starting tables into
/// its own slot.
fn write_tiles<F>(tiles: usize, store: usize, code: F) -> Result<(Vec<Vec<u8>>, Option<crate::cdf_state::Cdfs>)>
where
    F: Fn(usize) -> Result<(Vec<u8>, Option<crate::cdf_state::Cdfs>)> + Sync + Send,
{
    let _t = crate::par::timer(crate::par::S_TILE_WRITE);
    type Slot = Option<Result<(Vec<u8>, Option<crate::cdf_state::Cdfs>)>>;
    let threads = crate::par::tile_threads().min(tiles);
    let mut slots: Vec<Slot> = Vec::new();
    if threads <= 1 {
        for index in 0..tiles {
            slots.push(Some(code(index)));
        }
    } else {
        let done: std::sync::Mutex<Vec<Slot>> = std::sync::Mutex::new((0..tiles).map(|_| None).collect());
        {
            let batch = crate::par::Batch::new("ec-av1-tile");
            for (from, to) in crate::par::bands(tiles, threads) {
                let done = &done;
                let code = &code;
                batch.submit(move || {
                    for index in from..to {
                        let coded = code(index);
                        done.lock().expect("tile slots")[index] = Some(coded);
                    }
                });
            }
        }
        slots = done.into_inner().expect("tile slots");
    }
    let mut out = Vec::with_capacity(tiles);
    let mut stored = None;
    for (index, slot) in slots.into_iter().enumerate() {
        let (bytes, cdfs) = slot.expect("every tile was coded")?;
        if index == store {
            stored = cdfs;
        }
        out.push(bytes);
    }
    Ok((out, stored))
}

/// Runs `job(tile_index, ctx)` once per tile, on `EC_AV1_TILE_THREADS`
/// workers, and returns the results in tile order. The counterpart of
/// [`write_tiles`] for the RD/motion SEARCH: a tile's job owns every piece of
/// state the search mutates (its own reconstruction planes, its own `MiGrid`,
/// its own neighbour-mode bands and its own [`crate::decode::FrameCtx`]), and
/// availability is already tile-relative (`clip_planes_to_tile`,
/// `MiGrid::set_tile_bounds`), so what a tile decides cannot depend on which
/// other tiles have run -- gated by
/// `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count`, which now
/// covers the search as well as the write.
///
/// The `FrameCtx` copy is made on the calling thread (`&FrameCtx` is `!Sync`
/// by design) and moved into the worker, exactly as [`crate::par::run_bands`]
/// does it for the decoder's filter bands.
fn search_tiles<T, F>(tiles: usize, fctx: &crate::decode::FrameCtx, job: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(usize, &crate::decode::FrameCtx) -> Result<T> + Sync,
{
    let _t = crate::par::timer(crate::par::S_TILE_SEARCH);
    let threads = crate::par::tile_threads().min(tiles);
    if threads <= 1 {
        return (0..tiles).map(|index| job(index, fctx)).collect();
    }
    type Slot<T> = Option<Result<T>>;
    let done: std::sync::Mutex<Vec<Slot<T>>> =
        std::sync::Mutex::new((0..tiles).map(|_| None).collect());
    {
        let batch = crate::par::Batch::new("ec-av1-tile-search");
        for (from, to) in crate::par::bands(tiles, threads) {
            let done = &done;
            let job = &job;
            let ctx = crate::decode::filter_ctx_copy(fctx);
            batch.submit(move || {
                for index in from..to {
                    let searched = job(index, &ctx);
                    done.lock().expect("tile slots")[index] = Some(searched);
                }
            });
        }
    }
    done.into_inner()
        .expect("tile slots")
        .into_iter()
        .map(|slot| slot.expect("every tile was searched"))
        .collect()
}

/// One tile job's own copy of a plane: the frame's source, a blank
/// reconstruction of the frame's size, and the frame's own bounds -- narrowed
/// to the tile by `clip_planes_to_tile` right after. Blank rather than a copy
/// of the frame's reconstruction because a tile's prediction never reads a
/// sample outside its own rectangle, and every sample inside it is written
/// before it is read.
fn fresh_plane(source: &[u8], width: usize, height: usize, true_width: usize, true_height: usize) -> Plane<'_> {
    Plane {
        source,
        reconstruction: vec![128; source.len()],
        width,
        height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
        ctx: CoefCtxMap::default(),
    }
}

/// Copies the rectangle `[x0, x1) x [y0, y1)` of a tile job's own plane into
/// the frame's plane, through the [`crate::par::Shared`] every job holds.
///
/// corner-cut: the disjointness that makes the aliasing sound is the caller's
/// -- `x0..x1`/`y0..y1` are the job's own `TileRect`, and a frame's tile
/// rectangles partition it -- checked end-to-end by
/// `tile_bytes_do_not_depend_on_the_thread_count` rather than by the borrow
/// checker, exactly the contract `Shared`'s own doc names. Upgrade path is
/// row-range `split_at_mut` plumbing through `Plane`, which would have to
/// thread an origin offset through every prediction and transform call site
/// for the same bytes.
#[allow(unsafe_code)]
fn copy_rect(
    dst: crate::par::Shared<'_, Vec<u8>>,
    src: &[u8],
    stride: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) {
    // SAFETY: this job touches only its own tile rectangle (see above).
    let dst = unsafe { dst.get() };
    for y in y0..y1 {
        let (a, b) = (y * stride + x0, y * stride + x1);
        dst[a..b].copy_from_slice(&src[a..b]);
    }
}

/// The frame header's `tile_info` (spec 5.9.15) for `layout`:
/// `context_update_tile_id` names the largest tile, whose end-of-tile CDF
/// tables the frame stores (spec 7.20), which is what
/// [`crate::tile::TileLayout::largest_tile`] picks and what the tile loops
/// below publish.
fn tile_info_of(layout: &crate::tile::TileLayout, tile_size_bytes: u32) -> ec_av1_syntax::TileInfo {
    ec_av1_syntax::TileInfo {
        uniform_spacing: true,
        cols: layout.cols(),
        rows: layout.rows(),
        cols_log2: layout.cols_log2,
        rows_log2: layout.rows_log2,
        mi_col_starts: layout.mi_col_starts(),
        mi_row_starts: layout.mi_row_starts(),
        context_update_tile_id: layout.largest_tile() as u32,
        tile_size_bytes,
    }
}

/// Clips the three planes' intra edge reads to `rect`, the tile the
/// superblock about to be searched belongs to (the encoder side of the
/// decoder's `PlaneBuf::set_tile_origin`). The search itself stays in frame
/// raster order -- which is a linear extension of every tile's own coding
/// order, so a block still only ever predicts from samples already
/// reconstructed -- and only availability is tile-relative.
fn clip_planes_to_tile(
    luma: &mut Plane<'_>,
    chroma: &mut [Plane<'_>; 2],
    rect: crate::tile::TileRect,
) {
    let (x0, y0) = (rect.mi_col0 as usize * 4, rect.mi_row0 as usize * 4);
    let (x1, y1) = (rect.mi_col1 as usize * 4, rect.mi_row1 as usize * 4);
    luma.set_tile(x0, y0, x1, y1);
    for plane in chroma.iter_mut() {
        plane.set_tile(x0 / 2, y0 / 2, x1.div_ceil(2), y1.div_ceil(2));
    }
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
/// Codes one picture as a whole key frame, at `base_q_idx`, and returns its
/// stream. The per-frame decode state this needs lives for exactly this call.
///
/// # Errors
/// Returns an error under the same conditions
/// [`encode_key_frame_with_modes`] does.
pub fn encode_key_frame(picture: &Picture, base_q_idx: u8, deadzone: f64) -> Result<Encoded> {
    encode_key_frame_with_ctx(picture, base_q_idx, deadzone, &crate::decode::FrameCtx::new())
}

/// [`encode_key_frame`] with the per-block intra modes given rather than
/// searched.
///
/// # Errors
/// Returns an error when `picture`'s dimensions are odd or zero, when `modes`
/// is empty, or when a block's coefficients do not fit the coded frame.
pub fn encode_key_frame_with_modes(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
) -> Result<Encoded> {
    encode_key_frame_with_modes_with_ctx(
        picture,
        base_q_idx,
        deadzone,
        modes,
        &crate::decode::FrameCtx::new(),
    )
}

/// Codes `pictures` as one key frame followed by inter frames.
///
/// Camera material is coded under [`crate::encoder::Pyramid::default`] since
/// lane-av1pyrdef -- mini-GOPs of 4 with a hidden `ALTREF` and a
/// `show_existing_frame` header, so `stream` is in CODING order while
/// `frames` stays in display order (`EncodedSequence::coding_order` is the
/// map). Screen content codes flat, decided once per sequence by the same
/// detector the streaming facade's content gate uses, and `EC_AV1_PYRAMID=0`
/// puts every stream back on the flat path for an A/B.
///
/// # Errors
/// Returns an error under the same conditions [`encode_key_frame`] and the
/// inter frame path do.
pub fn encode_sequence(
    pictures: &[Picture],
    base_q_idx: u8,
    deadzone: f64,
) -> Result<EncodedSequence> {
    encode_sequence_with_ctx(pictures, base_q_idx, deadzone, &crate::decode::FrameCtx::new())
}

pub(crate) fn encode_key_frame_with_ctx(picture: &Picture, base_q_idx: u8, deadzone: f64, fctx: &crate::decode::FrameCtx) -> Result<Encoded> {
    encode_key_frame_with_modes_with_ctx(picture, base_q_idx, deadzone, &KEY_FRAME_MODES, fctx)
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
pub(crate) fn encode_key_frame_with_modes_with_ctx(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8], fctx: &crate::decode::FrameCtx,
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
        unspecified_color_config(), fctx,
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
    color_config: ColorConfig, fctx: &crate::decode::FrameCtx,
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
    // Screen-content detection runs on the SOURCE luma, before any header is
    // built: it decides this sequence's `seq_force_screen_content_tools` (and
    // so the layout of every frame header after it, [`SEQ_SCREEN`]) as well as
    // this frame's own `allow_screen_content_tools`.
    let screen = screen_content(
        &picture.y.iter().map(|&v| v as u8).collect::<Vec<u8>>(),
        picture.width,
        (picture.width).min(render.0),
        (picture.height).min(render.1),
    );
    arm_seq_screen(screen);
    // A key frame's writer really does start from the defaults, and a search
    // worker may be carrying an inter frame's armed tables (lane-av1txbits):
    // disarm, or a key frame's bits would depend on which thread searched it.
    crate::tile::arm_pricing_cdfs(None, false);
    // spec 5.9.2 `allow_intrabc`, screen-detected key frames only: the bit
    // forces deblocking, CDEF and loop restoration OFF for this frame
    // (frame.rs:194/210/252/272), so it is only set when the source repeats
    // itself enough for the copied blocks to pay that back
    // ([`intrabc_worth_it`]).
    // Single-tile frames only: decode.rs keeps ONE frame-scoped intrabc mi
    // grid across every tile while this writer builds one per tile, so a
    // multi-tile allow_intrabc frame would predict its DVs off a different
    // grid than the decoder does. corner-cut, ceiling named: the upgrade
    // path is a frame-scoped grid shared by the tile writers.
    let (allow_intrabc, ibc_share) = if screen && intrabc_enabled() && armed_tiles() == (0, 0) {
        intrabc_worth_it(
            &picture.y.iter().map(|&v| v as u8).collect::<Vec<u8>>(),
            picture.width,
            picture.width.min(render.0),
            picture.height.min(render.1),
        )
    } else {
        (false, 0.0)
    };
    IBC_SHARE.with(|c| c.set(ibc_share));
    let (seq, mut header) = key_frame_headers_colour(render.0, render.1, base_q_idx, color_config)?;
    header.allow_screen_content_tools = screen;
    header.allow_intrabc = allow_intrabc;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;
    // `TxMode::Select`: every block codes a `tx_depth` symbol and may split
    // its luma transform below its own side (`crate::tile::write_luma_select`).
    // Set here rather than in `key_frame_headers_colour` so the header
    // builders' other callers -- the hand-built tile writers, which code no
    // depth symbol -- keep the `TxMode::Largest` stream they are written for.
    let tx_select = tx_select();
    if tx_select {
        header.tx_mode = TxMode::Select;
    }
    let (tile_cols_log2, tile_rows_log2) = armed_tiles();

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
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: picture.width,
        tile_y1: picture.height,
        ctx: CoefCtxMap::default(),
    };
    let mut chroma = [
        Plane {
            source: &picture_u8,
            reconstruction: vec![128; picture_u8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
        Plane {
            source: &picture_v8,
            reconstruction: vec![128; picture_v8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
    ];

    // The search trades a bit for the squared error it saves, in the units the
    // reconstruction is measured in: one step of the quantizer, squared.
    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale() * key_lambda_factor() * step * step,
        modes,
        top_k: prune_top_k(),
        screen,
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
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    // This frame's tile grid (one tile unless the facade armed more).
    let layout =
        crate::tile::TileLayout::new(header.mi_cols, header.mi_rows, tile_cols_log2, tile_rows_log2);
    header.tile_info = tile_info_of(&layout, 1);
    // The filter search re-decodes this frame's tiles; the decoder derives
    // every tile's own rect from this same `tile_info` (its `mi_col_starts`/
    // `mi_row_starts`), which is what makes the writer's rects and the
    // reader's the same rects.
    let header_tile_info = header.tile_info.clone();
    // lane-av1tsearch: the search runs once per TILE (`search_tiles`), not
    // once over the whole frame. Each tile job builds its own reconstruction
    // planes and its own `above_mode`/`left_mode` bands, which is also what
    // the tile writer reads -- a tile resets its neighbour contexts (spec
    // 5.11.1), so a block at a tile's left edge must cost its mode against
    // the default context, not against whatever the tile to its west left in
    // that row. At one tile the bands are the frame's, exactly as before.
    let (frame_width, frame_height) = (picture.width, picture.height);
    let luma_out = crate::par::Shared::new(&mut luma.reconstruction);
    let [cb, cr] = &mut chroma;
    let chroma_out = [
        crate::par::Shared::new(&mut cb.reconstruction),
        crate::par::Shared::new(&mut cr.reconstruction),
    ];
    let search_tile = |index: usize,
                       fctx: &crate::decode::FrameCtx|
     -> Result<Vec<(usize, Superblock, Vec<u8>)>> {
        crate::tile::arm_pricing_cdfs(None, false);
        let rect = layout.rect(index);
        let mut luma = fresh_plane(&picture_y8, frame_width, frame_height, true_width, true_height);
        let mut chroma = [
            fresh_plane(&picture_u8, frame_width / 2, frame_height / 2, true_width / 2, true_height / 2),
            fresh_plane(&picture_v8, frame_width / 2, frame_height / 2, true_width / 2, true_height / 2),
        ];
        clip_planes_to_tile(&mut luma, &mut chroma, rect);
        let mut above_mode = vec![DC_PRED; cols * 2];
        let mut left_mode = vec![DC_PRED; rows * 2];
        let mut ibc = allow_intrabc.then(|| {
            Ibc::new(
                (
                    rect.mi_col0 as usize * 4,
                    rect.mi_row0 as usize * 4,
                    (rect.mi_col1 as usize * 4).min(true_width),
                    (rect.mi_row1 as usize * 4).min(true_height),
                ),
                &picture_y8,
                frame_width,
            )
        });
        let mut coded: Vec<(usize, Superblock, Vec<u8>)> = Vec::new();
    for sb_row in rect.sb_row0 as usize..rect.sb_row1 as usize {
        for sb_col in rect.sb_col0 as usize..rect.sb_col1 as usize {
            let mut modes = Vec::with_capacity(4);
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
                let base = snapshot(&luma, &chroma, (x, y), BLOCK);

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
                // `decode_partition`, recomputed at this leaf's own half):
                // either axis false is a straddling leaf this writer codes as
                // the 8x8 leaves that are inside it
                // (`crate::tile::write_leaf8`, lane-av1-rect r7). Both axes
                // false is the same split with the partition symbol inferred
                // rather than coded -- no rectangular transform is needed
                // (lane-av1rect).
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
                // What the whole 32x32 costs, including the partition symbol
                // that says it is not split.
                let (whole, mut cost_whole) = code_square(
                    &mut luma,
                    &mut chroma,
                    (x, y),
                    BLOCK,
                    &search,
                    &mode_bits(above_mode[c0], left_mode[r0]),
                    tx_select,
                    ibc.as_mut(), fctx,
                );
                cost_whole += search.lambda * partition_bits(BLOCK, false);
                let after_whole = snapshot(&luma, &chroma, (x, y), BLOCK);

                // What four 16x16 blocks cost instead, each searched against
                // the reconstruction the ones before it left.
                restore(&mut luma, &mut chroma, (x, y), BLOCK, &base);
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
                            tx_select,
                            ibc.as_mut(), fctx,
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
                                tx_select,
                                None, fctx,
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
                    restore(&mut luma, &mut chroma, (x, y), BLOCK, &after_whole);
                    for cell in 0..2 {
                        above_mode[c0 + cell] = whole.mode;
                        left_mode[r0 + cell] = whole.mode;
                    }
                    modes.push(whole.mode);
                    blocks.push(Quadrant::Whole(whole));
                }
            }
            coded.push((sb_row * sb_cols + sb_col, Superblock::Split(blocks), modes));
        }
    }
        let (x0, y0) = (rect.mi_col0 as usize * 4, rect.mi_row0 as usize * 4);
        let (x1, y1) = (rect.mi_col1 as usize * 4, rect.mi_row1 as usize * 4);
        copy_rect(luma_out, &luma.reconstruction, frame_width, x0, y0, x1, y1);
        for (&out, tile) in chroma_out.iter().zip(&chroma) {
            copy_rect(
                out,
                &tile.reconstruction,
                frame_width / 2,
                x0 / 2,
                y0 / 2,
                x1.div_ceil(2),
                y1.div_ceil(2),
            );
        }
        Ok(coded)
    };
    let searched = search_tiles(layout.count(), fctx, search_tile)?;
    // Back into frame raster order: the tile writer indexes `superblocks` by
    // the frame's own superblock index, and `modes` is the coding order the
    // ablation drivers read.
    let mut in_order: Vec<Option<(Superblock, Vec<u8>)>> = (0..sb_cols * sb_rows).map(|_| None).collect();
    for (at, superblock, block_modes) in searched.into_iter().flatten() {
        in_order[at] = Some((superblock, block_modes));
    }
    let mut superblocks = Vec::with_capacity(sb_cols * sb_rows);
    let mut modes = Vec::with_capacity(cols * rows);
    for slot in in_order {
        let (superblock, block_modes) = slot.expect("every superblock belongs to a tile");
        superblocks.push(superblock);
        modes.extend_from_slice(&block_modes);
    }

    #[cfg(test)]
    record_predicted_bits(crate::tile::predicted_coeff_bits_sb(&superblocks, base_q_idx));
    // A key frame always starts from the defaults (`primary_ref_frame` is
    // `PRIMARY_REF_NONE`), and its header keeps `disable_frame_end_update_cdf`
    // set, so what it stores into the slots it refreshes is exactly those
    // defaults again (spec 7.20's `started_from` arm) -- the first inter
    // frame after it therefore starts from the defaults too.
    let (mi_cols, mi_rows) = (header.mi_cols, header.mi_rows);
    let cdfs = crate::cdf_state::Cdfs::new(crate::tile::q_ctx_of(base_q_idx));
    let start_cdfs = CdfSnapshot(cdfs);
    // The luma restoration-unit grid this frame's `lr_params` implies
    // (`av1_lr_count_units` rounds to nearest, so a short last unit is
    // swallowed by the one before it), for the tile writer's own per-unit
    // walk.
    let lr_horz = crate::restoration::count_units(header.frame_width, 64) as u32;
    let lr_vert = crate::restoration::count_units(header.frame_height, 64) as u32;
    // Every tile is coded from this frame's OWN starting tables and leaves
    // its own end state behind (spec 5.11.1: each tile resets the CDFs, the
    // loop-restoration reference and its neighbour contexts), so the tiles
    // are independent of each other and of the order they are written in.
    let code_tiles = |bits: u8,
                      sb_cols: usize,
                      grid: &[u8],
                      units: &[Option<crate::restoration::WienerInfo>]|
     -> Result<Vec<Vec<u8>>> {
        let (out, _) = write_tiles(layout.count(), usize::MAX, |index| {
            crate::tile::arm_cdef_idx(bits, sb_cols, grid.to_vec());
            crate::tile::arm_lr(64, lr_horz, lr_vert, units.to_vec());
            let mut cdfs = start_cdfs.0.clone();
            crate::tile::arm_screen(screen);
            crate::tile::arm_filter_intra(filter_intra_on());
            crate::tile::arm_intrabc(allow_intrabc);
            crate::tile::arm_pricing_cdfs(None, false);
            let bytes = crate::tile::sb_coeff_key_frame_tile_cdfs(
                mi_cols,
                mi_rows,
                base_q_idx,
                &superblocks,
                tx_select,
                &mut cdfs,
                layout.rect(index),
            )?;
            Ok((bytes, None))
        })?;
        Ok(out)
    };
    let mut tiles = code_tiles(0, sb_cols, &[], &[])?;
    // spec 7.14 deblocking: pick this frame's `loop_filter_level` by handing
    // the tile just coded back to the decoder under each candidate (see
    // `crate::filter_search`), and keep the filtered picture that wins as
    // the reconstruction -- what a decoder outputs, and what the next
    // frame predicts from.
    pick_and_apply_filters(
        &mut header,
        &mut luma,
        &mut chroma,
        [&picture_y8, &picture_u8, &picture_v8],
        // spec 5.9.11/5.9.19/5.9.20: `allow_intrabc` forces deblocking, CDEF
        // and loop restoration off and codes none of their parameters
        // (frame.rs:194/210/252/272) -- there is no filter to search for, and
        // the encoder's reconstruction is already what a decoder produces.
        !allow_intrabc,
        &mut tiles,
        search.lambda,
        fctx,
        // Every re-code starts from this frame's OWN starting tables, never
        // from where the first write left them: the second tile is the same
        // symbol sequence plus new ones, so it must adapt from the same
        // point the decoder will.
        |bits, sb_cols, grid, units| code_tiles(bits, sb_cols, grid, units),
        |lf, cdef, h, tiles: &[Vec<u8>], lr| {
            let payloads: Vec<&[u8]> = tiles.iter().map(Vec::as_slice).collect();
            crate::decode::decode_key_frame_tiles_lr(
                &payloads,
                &header_tile_info,
                h.mi_cols,
                h.mi_rows,
                base_q_idx,
                h.frame_width,
                h.frame_height,
                seq.enable_filter_intra,
                cdef,
                lf,
                tx_select,
                h.reduced_tx_set,
                h.allow_screen_content_tools,
                h.allow_intrabc,
                lr,
                fctx,
            )
        },
    )?;
    let tile_size_bytes = crate::frame::tile_size_bytes_for(&tiles);
    header.tile_info.tile_size_bytes = tile_size_bytes;
    let tile = crate::frame::tile_group_payload(&tiles, tile_size_bytes);
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
        tx_select,
        // A key frame codes no inter block, so no motion_mode symbol.
        switchable_motion_mode: false,
        screen,
        next_cdfs: start_cdfs.clone(),
        start_cdfs,
        loop_filter: header.loop_filter,
        cdef: header.cdef,
        loop_restoration: header.loop_restoration,
        allow_intrabc,
    })
}

/// Chooses this frame's deblocking levels and CDEF strengths, writes them
/// into `header` and
/// replaces the frame's own region of the encoder's reconstruction with the
/// filtered picture the decoder produced under them
/// ([`crate::filter_search::pick_deblock`]).
///
/// `search == false` leaves the header at level 0 (deblocking off) and the
/// reconstruction untouched: the caller's stream is one this crate's own
/// decoder cannot read back, so there is nothing to search against.
///
/// # Errors
/// The decoder's own error, when it refuses the tile the encoder just wrote
/// -- that is an encoder defect, not a filter one, so it is propagated
/// rather than swallowed into "no deblocking".
fn pick_and_apply_filters(
    header: &mut FrameHeader,
    luma: &mut Plane<'_>,
    chroma: &mut [Plane<'_>; 2],
    source: [&[u8]; 3],
    search: bool,
    // The coded tiles, one payload per tile of the frame's grid, replaced in
    // place when the search chooses a per-64x64 `cdef_idx` list
    // (`cdef_bits > 0`): those literals live in the tile payloads, so every
    // tile is written a second time under `recode` and the winner
    // re-decoded from them.
    tiles: &mut Vec<Vec<u8>>,
    lambda: f64,
    fctx: &crate::decode::FrameCtx,
    recode: impl Fn(
        u8,
        usize,
        &[u8],
        &[Option<crate::restoration::WienerInfo>],
    ) -> Result<Vec<Vec<u8>>>,
    decode: impl Fn(
        &LoopFilterParams,
        &CdefParams,
        &FrameHeader,
        &[Vec<u8>],
        &LoopRestorationParams,
    ) -> Result<Picture>,
) -> Result<()> {
    if !search {
        return Ok(());
    }
    let (fw, fh) = (header.frame_width as usize, header.frame_height as usize);
    let hdr = header.clone();
    // libaom `av1_pick_filter_level`'s sibling `av1_cdef_search`:
    // `cdef_damping = 3 + (base_qindex >> 6)`, never searched.
    let damping = 3 + (header.quantization.base_q_idx >> 6);
    let (cols32, rows32) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (sb_cols, sb_rows) = (cols32.div_ceil(2) as usize, rows32.div_ceil(2) as usize);
    let none_lr = LoopRestorationParams::default();
    // Every candidate scores the same coded tile: decode it once and re-run
    // the filters alone for the rest (`crate::decode::FilterReplay`, 24% of
    // the encoder's profile before this). The per-64x64 error the CDEF
    // preset search reads comes off the REPLAYED picture, which is the same
    // picture a decode produces -- that is what the replay is pinned on.
    crate::decode::clear_filter_replay();
    // lane-av1fpar: the per-tile search is over, so every core is idle from
    // here to the end of the frame -- band each candidate's deblock/CDEF
    // replay, the capture decode's own filter chain and the loop-restoration
    // passes across the same workers the search used.
    let _bands = crate::par::override_filter_threads(crate::par::tile_threads());
    let ft = crate::par::timer(crate::par::S_FILTER);
    let search_result = crate::filter_search::pick_filters(
        |lf, cdef| {
            if let Some(picture) = crate::decode::replay_filters(lf, cdef, fctx) {
                return Ok(picture);
            }
            crate::decode::arm_filter_replay();
            let _t = crate::par::timer(crate::par::S_FIRST);
            decode(lf, cdef, &hdr, tiles, &none_lr)
        },
        source,
        luma.width,
        (fw, fh),
        damping,
        (sb_cols, sb_rows),
        lambda,
    );
    drop(ft);
    let (lf, cdef, idx_grid, picture) = search_result?;
    header.loop_filter = lf;
    header.cdef = cdef;
    // spec 7.17: loop restoration. Its per-unit filters live in the TILE, not
    // in the frame header, so a re-decode under candidate parameters cannot
    // search them the way deblocking and CDEF are searched above. What the
    // decoder hands back instead is its own filter chain's intermediates
    // (`crate::decode::FrameCtx::capture_stages`), and the search runs on
    // those, through the decoder's own Wiener kernel.
    //
    // The capture rides the decode this stage was already going to do: the
    // re-decode of the `cdef_idx` tile when there is one, and otherwise one
    // decode of the tile as it stands. That decode's header still says
    // `RESTORE_NONE`, so the decoder takes no pre-CDEF snapshot and the
    // post-CDEF picture stands in for it at LR's 3-row stripe borders --
    // an approximation inside the SEARCH only; the reconstruction this
    // function ends up splicing is always a real decode of the real tile.
    let lr_on = restoration_enabled();
    // The `cdef_idx` literals live in the tile payload, so a frame that
    // chose a per-64x64 preset list codes its tiles a second time.
    if !idx_grid.is_empty() {
        *tiles = recode(cdef.bits, sb_cols, &idx_grid, &[])?;
    }
    // lane-av1cap: that re-coded tile used to be DECODED here as well --
    // 10% of a 4K frame -- purely to reach the post-CDEF plane the loop
    // restoration search reads (`crate::decode::FrameCtx::capture_stages`)
    // and the picture this function splices. Both come out of ONE more
    // replay of the winning parameters on the same captured pre-filter
    // planes every candidate was scored on: the same two filter kernels, on
    // the same input, in the same order, so the same bytes. Re-coding the
    // tile does not move the reconstruction the replay holds -- `cdef_idx`
    // is an equiprobable literal, so it adapts no CDF and changes no
    // residual.
    let replayed = crate::decode::replay_final(&lf, &cdef, &idx_grid, lr_on, fctx);
    #[cfg(test)]
    let replay_was_used = replayed.is_some();
    let (mut filtered, mut stages) = match replayed {
        Some((picture, stages)) => (picture, stages),
        // Nothing captured (the shapes [`crate::decode::FilterReplay`]
        // refuses): every candidate was a full decode, so the winner's own
        // picture stands, and the stages still need a capturing decode.
        None => {
            fctx.capture_stages.set(lr_on);
            let picture = if idx_grid.is_empty() && !lr_on {
                picture
            } else {
                let _t = crate::par::timer(crate::par::S_CAPTURE);
                decode(&lf, &cdef, &hdr, tiles, &none_lr)?
            };
            fctx.capture_stages.set(false);
            (picture, crate::decode::take_filter_stages(fctx))
        }
    };
    #[cfg(test)]
    if crate::decode::verify_final_replay() && replay_was_used {
        fctx.capture_stages.set(lr_on);
        let want = decode(&lf, &cdef, &hdr, tiles, &none_lr)?;
        fctx.capture_stages.set(false);
        let want_stages = crate::decode::take_filter_stages(fctx);
        assert!(
            (want.width, want.height) == (filtered.width, filtered.height)
                && want.y == filtered.y
                && want.u == filtered.u
                && want.v == filtered.v,
            "the final filter replay is not the capture decode it stands in for",
        );
        // Only the frame's own cropped region: the plane is padded to the
        // superblock grid and the columns past the mi extent are written by
        // nothing (the decoder's plane pool leaves whatever the last frame
        // put there), while the restoration search reads strictly inside
        // `fw x fh` -- `restoration::lr_sample` clamps its column to
        // `plane_w - 1` and its row to the frame's, and
        // `filter_search::pick_restoration`'s own SSE walk never leaves the
        // crop either.
        let region = |a: &[u16], b: &[u16], stride: usize, w: usize, h: usize, what: &str| {
            for row in 0..h {
                let (x, y) = (&a[row * stride..][..w], &b[row * stride..][..w]);
                assert!(
                    x == y,
                    "{what}: the replayed filter stages are not the ones the capture \
                     decode returns (row {row} of {h}, width {w}, stride {stride})",
                );
            }
        };
        match (&want_stages, &stages) {
            (Some(a), Some(b)) => {
                assert_eq!(a.stride, b.stride, "the captured plane strides moved");
                let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
                for (i, (w, h)) in [(fw, fh), (cw, ch), (cw, ch)].into_iter().enumerate() {
                    region(&a.deblocked[i], &b.deblocked[i], a.stride[i], w, h, "deblocked");
                    region(&a.cdefed[i], &b.cdefed[i], a.stride[i], w, h, "cdefed");
                }
            }
            (None, None) => {}
            _ => panic!("one path captured filter stages and the other did not"),
        }
    }
    // lane-av1cap2: the capture stays live past the restoration search --
    // `crate::decode::replay_restoration` filters the same post-CDEF plane
    // rather than decoding the re-coded tile. Cleared at the end instead.
    let mut lr = LoopRestorationParams::default();
    if lr_on {
        let mut want = LoopRestorationParams {
            loop_restoration_size: [64, 64, 64],
            uses_lr: true,
            ..LoopRestorationParams::default()
        };
        want.frame_restoration_type[0] = ec_av1_syntax::RestorationType::Wiener;
        let lrt = crate::par::timer(crate::par::S_LR);
        let units = match stages.take() {
            Some(stages) => crate::filter_search::pick_restoration(
                &stages,
                source[0],
                luma.width,
                (fw, fh),
                &want,
                lambda,
                fctx,
            ),
            None => Vec::new(),
        };
        // A frame no unit restores keeps `RESTORE_NONE` on every plane
        // rather than paying ~1 bit per 64x64 for a tile full of
        // `restore_wiener = 0` symbols (the BD gate saw exactly that on the
        // clip whose units never take a filter).
        drop(lrt);
        if units.iter().any(Option::is_some) {
            let coded = recode(cdef.bits, sb_cols, &idx_grid, &units)?;
            let _t = crate::par::timer(crate::par::S_CAPTURE);
            // lane-av1cap2: that re-coded tile used to be DECODED here to
            // reach the restored reference frame -- 5.6% of a 4K frame,
            // for a filter that reads nothing of the tile but the per-unit
            // Wiener filters this stage just chose. Run the decoder's own
            // restoration kernel on the replay's post-CDEF plane instead
            // (`crate::decode::replay_restoration`), with the real
            // post-deblock stripe borders.
            match crate::decode::replay_restoration(&want, &units, fctx) {
                Some(y) => {
                    #[cfg(test)]
                    let margin = fctx
                        .last_frame_wide_margin
                        .with(|m| m.borrow().as_ref().map(|p| p.y.clone()));
                    #[cfg(test)]
                    if crate::decode::verify_final_replay() {
                        let want_pic = decode(&lf, &cdef, &hdr, &coded, &want)?;
                        assert!(
                            (want_pic.width, want_pic.height) == (filtered.width, filtered.height)
                                && want_pic.u == filtered.u
                                && want_pic.v == filtered.v,
                            "the restoration replay moved a chroma plane or the picture size",
                        );
                        assert!(
                            want_pic.y == y,
                            "the restoration replay is not the decode of the re-coded tile",
                        );
                        assert!(
                            fctx.last_frame_wide_margin
                                .with(|m| m.borrow().as_ref().map(|p| p.y.clone()))
                                == margin,
                            "the restoration replay's wide-margin luma is not the decode's",
                        );
                    }
                    filtered.y = y;
                }
                // Nothing captured: the winner came out of a full decode,
                // so this one does too.
                None => filtered = decode(&lf, &cdef, &hdr, &coded, &want)?,
            }
            *tiles = coded;
            lr = want;
        }
    }
    crate::decode::clear_filter_replay();
    header.loop_restoration = lr;
    let dec_cw = filtered.width.div_ceil(2);
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
    crate::filter_search::splice(&mut luma.reconstruction, luma.width, &filtered.y, filtered.width, fw, fh);
    crate::filter_search::splice(&mut chroma[0].reconstruction, chroma[0].width, &filtered.u, dec_cw, cw, ch);
    crate::filter_search::splice(&mut chroma[1].reconstruction, chroma[1].width, &filtered.v, dec_cw, cw, ch);
    Ok(())
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

/// Whether an extra reference (GOLDEN/ALTREF) is offered a `NEWMV` of its
/// own -- a second motion search per block against that reference's picture,
/// priced off that reference's own stack. `EC_AV1_MV_EXTRA_NEW=0` turns it
/// off in a test build (what the lane measured both arms with).
fn extra_ref_new_mv() -> bool {
    match std::env::var("EC_AV1_MV_EXTRA_NEW").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::EXTRA_REF_NEW),
    }
}

/// The seven `ref_frame_idx` order hints of a frame coded on this crate's own
/// flat slot plan (`inter_frame_headers_slots`): every reference but
/// `GOLDEN_FRAME` and `ALTREF_FRAME` reads the frame just coded,
/// `GOLDEN_FRAME` reads the GOP's key frame, and `ALTREF_FRAME` reads the
/// frame two back when there is one (otherwise it, too, names the key frame's
/// slot). Hints are 7-bit, the only `order_hint_bits` this encoder writes.
pub(crate) fn flat_order_hints(order_hint: u32, key_hint: u32, has_altref: bool) -> [u32; 7] {
    let last = order_hint.wrapping_sub(1) & 0x7f;
    let alt = if has_altref {
        order_hint.wrapping_sub(2) & 0x7f
    } else {
        key_hint
    };
    [last, last, last, key_hint, last, last, alt]
}

/// Whether an inter frame carries `reference_select` (spec 5.9.22): the
/// header bit that lets each block choose between a single and a compound
/// reference. Off by default until compound prediction is measured to pay;
/// `EC_AV1_COMPOUND=1` arms it in a test build (which is what the BD gate
/// runs as), and every block still codes SINGLE, so the only cost is one
/// `comp_mode` symbol per inter block.
pub(crate) const REFERENCE_SELECT: bool = true;

/// [`REFERENCE_SELECT`], with the test-build environment override.
pub(crate) fn reference_select() -> bool {
    match std::env::var("EC_AV1_COMPOUND").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::COMPOUND),
    }
}

/// [`tile::write_drl_idx`]'s cost against one stack: one `drl_mode` symbol
/// per entry past `start`, `1` to advance and `0` to stop, at most two.
fn drl_bits_for(stack: &MvStack, start: usize, target: usize) -> f64 {
    let mut bits = 0.0;
    let mut idx = start;
    while idx < start + 2 && stack.entries.len() > idx + 1 {
        let advance = idx < target;
        bits += symbol_bits(&cdf::DRL_MODE[stack.drl_ctx[idx]], usize::from(advance));
        if !advance {
            break;
        }
        idx += 1;
    }
    bits
}

/// The cheapest `(bits, drl index)` the `NEWMV` syntax can name `mv` with off
/// `stack`: each DRL entry is a different `PredMv` (spec 7.10.2.10
/// `assign_mv`), so the same vector costs a different residual from each.
fn best_new_mv_syntax(stack: &MvStack, mv: (i32, i32)) -> Option<(f64, u8)> {
    let mut best: Option<(f64, u8)> = None;
    for idx in 0..3usize {
        if idx > 0 && stack.entries.len() <= idx {
            break;
        }
        let base = stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv);
        if let Some(b) = mv_residual_bits(mv, base) {
            let total = b + drl_bits_for(stack, 0, idx);
            if best.is_none_or(|(t, _)| total < t) {
                best = Some((total, idx as u8));
            }
        }
    }
    best
}

/// The starting points [`motion::search`] gets besides `stack.pred_mv`: the
/// zero vector and the stack's own candidates. A neighbour block's vector is
/// where this block's motion most often already is, which is what pays for
/// the extra starting points (measured on the BD gate: -0.9/-0.8/-0.0 points
/// vs libaom for 0.2 s off a 6 s ladder).
fn mv_seeds(stack: &MvStack) -> ([(i32, i32); 4], usize) {
    let mut seeds = [(0, 0); 4];
    let mut n = 1;
    for e in stack.entries.iter().take(3) {
        seeds[n] = e.mv;
        n += 1;
    }
    (seeds, n)
}

/// What [`crate::tile::write_mv`] (private to that module) would spend
/// coding `mv` as a residual against `pred`: the joint symbol naming which
/// components differ, then each differing component. `None` under the same
/// condition [`mv_component_bits`] returns `None`.
/// How many frames back `ref_frame` sits from the frame being coded, off the
/// order hints [`crate::tile::arm_order_hints`] armed for it. `LAST_FRAME` is
/// 1 in this encoder's GOP; `GOLDEN`/`ALTREF` can be much further, which is
/// what [`crate::motion::search`] scales its starting step by.
fn ref_distance(ref_frame: i8) -> u32 {
    let (bits, order_hint, hints) = crate::tile::order_hints();
    let i = (ref_frame - crate::mvstack::LAST_FRAME).clamp(0, 6) as usize;
    let d = crate::motion_field::get_relative_dist(bits, order_hint, hints[i])
        .unsigned_abs()
        .max(1);
    if crate::envflags::env_flag!("EC_TRACE_DIST") {
        eprintln!("EC_DIST ref={ref_frame} bits={bits} oh={order_hint} hint={} d={d}", hints[i]);
    }
    d
}

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
    set: TxbSet, fctx: &crate::decode::FrameCtx,
) -> Trial {
    let x_q4 = mv_to_q4(x, mv.1, luma);
    let y_q4 = mv_to_q4(y, mv.0, luma);
    // lane-hbd r4: `reference` (a DPB `Picture`'s plane) is `u16` now that
    // `Picture` is widened; `prediction`/the encoder stay `u8` (encoder is
    // 8-bit only this round, see `intra_predict_u8`'s doc comment).
    // lane-av1speed: both buffers are per-candidate scratch on the hottest
    // path in the encoder, so they live on the stack -- `side` is at most
    // `BLOCK` (32) for luma and half that for chroma. Two heap allocations
    // per motion-compensated trial otherwise.
    let mut prediction16 = [0u16; BLOCK * BLOCK];
    let prediction16 = &mut prediction16[..side * side];
    mc::predict(
        reference,
        stride,
        ref_width,
        ref_height,
        x_q4,
        y_q4,
        side,
        side,
        prediction16, fctx,
    );
    let mut prediction = [0u8; BLOCK * BLOCK];
    let prediction = &mut prediction[..side * side];
    for (dst, &src) in prediction.iter_mut().zip(prediction16.iter()) {
        *dst = src as u8;
    }
    plane.code_from_prediction(x, y, side, prediction, skip, base_q_idx, deadzone, set)
}

/// [`mc_trial`]'s compound counterpart (spec 7.11.3.1's compound path): both
/// references predict into the `CONV_BUF` intermediate domain
/// ([`mc::predict_compound_intermediate`]) and are blended by
/// [`mc::combine_compound`] at the simple-average split `(8, 8)` -- exactly
/// what a decoder computes for a `comp_group_idx == 0`, `compound_idx == 1`
/// block, so the encoder's reconstruction is the decoder's.
#[allow(clippy::too_many_arguments)]
fn mc_trial_compound(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    (mv0, mv1): ((i32, i32), (i32, i32)),
    luma: bool,
    ref0: (&[u16], usize, usize, usize),
    ref1: (&[u16], usize, usize, usize),
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet, fctx: &crate::decode::FrameCtx,
) -> Trial {
    let mut inter0 = [0i32; BLOCK * BLOCK];
    let mut inter1 = [0i32; BLOCK * BLOCK];
    let (inter0, inter1) = (&mut inter0[..side * side], &mut inter1[..side * side]);
    for (mv, r, dst) in [(mv0, ref0, &mut *inter0), (mv1, ref1, &mut *inter1)] {
        mc::predict_compound_intermediate(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(x, mv.1, luma),
            mv_to_q4(y, mv.0, luma),
            mc::REF_NO_SCALE,
            side,
            side,
            mc::InterpFilterKind::Regular,
            mc::InterpFilterKind::Regular,
            dst,
        );
    }
    let mut blended16 = [0u16; BLOCK * BLOCK];
    let blended16 = &mut blended16[..side * side];
    mc::combine_compound(inter0, inter1, 8, 8, blended16, fctx);
    let mut prediction = [0u8; BLOCK * BLOCK];
    let prediction = &mut prediction[..side * side];
    for (dst, &src) in prediction.iter_mut().zip(blended16.iter()) {
        *dst = src as u8;
    }
    plane.code_from_prediction(x, y, side, prediction, false, base_q_idx, deadzone, set)
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
/// One prediction of `side` x `side` samples from `reference`, as `u8` --
/// [`mc_trial`]'s prediction half, on the heap because a 64x64 luma block is
/// four times the stack scratch that function sizes for [`BLOCK`].
fn predict_u8(
    reference: &[u16],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    luma: bool,
    fctx: &crate::decode::FrameCtx,
) -> Vec<u8> {
    let mut prediction = vec![0u16; side * side];
    mc::predict(
        reference,
        stride,
        ref_width,
        ref_height,
        mv_to_q4(x, mv.1, luma),
        mv_to_q4(y, mv.0, luma),
        side,
        side,
        &mut prediction,
        fctx,
    );
    prediction.iter().map(|&v| v as u8).collect()
}

/// The 64x64 `PARTITION_NONE` candidate for a whole superblock ([`B64_ROOT`]):
/// single-reference LAST at `NEARESTMV`/`GLOBALMV`/`NEARMV`/`NEWMV`, coded
/// SKIP, priced through the same `symbol_bits` path every other candidate
/// uses. The winner's prediction is committed to the planes (it IS the
/// reconstruction -- a skip block has no residual); the caller restores them
/// if the four quadrants come out cheaper.
///
/// `None` when no candidate could be coded at all (a `NEWMV` residual the
/// writer cannot name is simply not offered, and the other modes always are,
/// so this only happens if every candidate is refused).
fn search_skip_64(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    search: &Search,
    // Whether the non-skip arm is priced at all ([`b64_residual`], and off on
    // screen content -- see that function's doc).
    residual: bool,
    reference: &Picture,
    stack: &MvStack,
    fctx: &crate::decode::FrameCtx,
) -> Option<(f64, BlockCoeffs)> {
    let side = SUPERBLOCK;
    let not_new = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1);
    let not_zero = symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1);
    let ref_bits = symbol_bits(&cdf::SINGLE_REF[0][0], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][2], 0)
        + symbol_bits(&cdf::SINGLE_REF[0][3], 0);
    // What every candidate pays whatever its vector: `skip` coded 1, then
    // `is_inter` coded 1, then the LAST chain above.
    let fixed = symbol_bits(&cdf::SKIP[0], 1) + symbol_bits(&cdf::INTRA_INTER[0], 1) + ref_bits;
    let info_of = |mode: InterMode, mv: (i32, i32), ref_mv_idx: u8| InterInfo {
        ref1: None,
        mv1: (0, 0),
        ref_frame: crate::mvstack::LAST_FRAME,
        mode,
        mv,
        ref_mv_idx,
    };
    let mut cands: Vec<(f64, InterInfo)> = vec![
        (
            not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0),
            info_of(InterMode::NearestMv, stack.nearest_mv, 0),
        ),
        (
            not_new + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 0),
            info_of(InterMode::GlobalMv, (0, 0), 0),
        ),
    ];
    let near_bits = not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 1);
    for idx in 1..=2usize {
        if let Some(e) = stack.entries.get(idx) {
            cands.push((
                near_bits + drl_bits_for(stack, 1, idx),
                info_of(InterMode::NearMv, e.mv, idx as u8),
            ));
        }
    }
    if leaf_new_mv() {
        let source_block = luma.source_block(x, y, side);
        let (seeds, seed_n) = mv_seeds(stack);
        let found = motion::search(
            &reference.y,
            reference.width,
            luma.true_width,
            luma.true_height,
            &source_block,
            x,
            y,
            side,
            side,
            stack.pred_mv,
            &seeds[..seed_n],
            search.lambda,
            fctx,
            1,
        );
        let mv = round_to_valid_mv(found.mv, stack.pred_mv);
        if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(stack, mv) {
            cands.push((
                symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0) + bits,
                info_of(InterMode::NewMv, mv, ref_mv_idx),
            ));
        }
    }

    let mut best: Option<(f64, f64, InterInfo, [Trial; 3])> = None;
    for (bits, info) in cands {
        let trials = [
            (0usize, x, y, side, true),
            (1, x / 2, y / 2, side / 2, false),
            (2, x / 2, y / 2, side / 2, false),
        ]
        .map(|(plane, px, py, s, is_luma)| {
            let (samples, stride, w, h) = match plane {
                0 => (&reference.y, reference.width, luma.true_width, luma.true_height),
                1 => (
                    &reference.u,
                    reference.width / 2,
                    chroma[0].true_width,
                    chroma[0].true_height,
                ),
                _ => (
                    &reference.v,
                    reference.width / 2,
                    chroma[1].true_width,
                    chroma[1].true_height,
                ),
            };
            let prediction = predict_u8(samples, stride, w, h, (px, py), s, info.mv, is_luma, fctx);
            let target: &Plane = if plane == 0 { luma } else { &chroma[plane - 1] };
            // `skip` true: the trial is the prediction itself plus its SSE,
            // so no transform of any size is involved (the reason a 64x64
            // root can be coded at all before a forward TX_64X64 exists).
            target.code_from_prediction(
                px,
                py,
                s,
                &prediction,
                true,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Luma32Inter,
            )
        });
        let cost = trials[0].sse
            + trials[1].sse
            + trials[2].sse
            + search.lambda * (fixed + bits);
        if best.as_ref().is_none_or(|b| cost < b.0) {
            best = Some((cost, bits, info, trials));
        }
    }
    let (skip_cost, bits, info, skip_trials) = best?;

    // The winner's prediction coded with a REAL residual (lane-tx64): one
    // TX_64X64 luma transform -- `TxbSet::Luma64`, whose coded quarter is the
    // 32x32 corner `code_from_prediction` hands back -- and one TX_32X32 per
    // chroma plane, since a 64x64 root's chroma block IS 32x32. Only the mv
    // the skip arm already chose is re-coded: the prediction with the lowest
    // SSE is also the cheapest residual to code, and a second 64x64 motion
    // search per superblock is the wall this root was taken to SAVE.
    //
    // Priced against the skip arm through the same `symbol_bits` path, so the
    // two numbers are comparable: `fixed` charged `skip` coded 1, and this
    // arm charges 0 instead and adds its three planes' coefficient bits.
    let mut chosen = (skip_cost, true, skip_trials);
    if residual {
        let sets = [TxbSet::Luma64, TxbSet::Chroma32, TxbSet::Chroma32];
        let residual_trials = [0usize, 1, 2].map(|plane| {
            let (target, px, py, s) = if plane == 0 {
                (&*luma, x, y, side)
            } else {
                (&chroma[plane - 1], x / 2, y / 2, side / 2)
            };
            target.code_from_prediction(
                px,
                py,
                s,
                &chosen.2[plane].reconstruction,
                false,
                search.base_q_idx,
                search.deadzone,
                sets[plane],
            )
        });
        let unskip = symbol_bits(&cdf::SKIP[0], 0) - symbol_bits(&cdf::SKIP[0], 1);
        let coeff_bits: f64 = residual_trials.iter().map(|t| t.bits).sum();
        let cost = residual_trials.iter().map(|t| t.sse).sum::<f64>()
            + search.lambda * (fixed + bits + unskip + coeff_bits);
        if cost < skip_cost {
            B64_RESIDUAL_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            chosen = (cost, false, residual_trials);
        }
    }

    let (cost, skip, trials) = chosen;
    luma.commit(x, y, side, &trials[0]);
    chroma[0].commit(x / 2, y / 2, side / 2, &trials[1]);
    chroma[1].commit(x / 2, y / 2, side / 2, &trials[2]);
    Some((
        cost,
        BlockCoeffs {
            skip,
            inter: Some(info),
            // Both grids are 32-sided: the luma one is the 64-point
            // transform's coded corner, the chroma ones are whole TX_32X32.
            luma: if skip { Vec::new() } else { coeffs(&trials[0].levels, BLOCK) },
            u: if skip { Vec::new() } else { coeffs(&trials[1].levels, side / 2) },
            v: if skip { Vec::new() } else { coeffs(&trials[2].levels, side / 2) },
            ..BlockCoeffs::default()
        },
    ))
}

fn search_inter_block(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
    // `GOLDEN_FRAME`: the key frame's own reconstruction, held in DPB slot 1
    // for the whole GOP, with the stack this block derives against it. Only
    // its two search-free modes are priced (`NEARESTMV`, `GLOBALMV`) -- a
    // second motion search would double the wall of the stage that already
    // owns most of it.
    extra: &[(i8, &Picture, &MvStack)],
    // The COMPOUND stack of each `LAST_FRAME` + extra-reference pair, built by
    // the caller off the same MI grid the tile writer will (empty unless the
    // frame carries `reference_select`).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    // lane-av1obmc: the mi grid this search's neighbours were published into
    // (`record_mi`) and this block's own place in it -- what the OBMC
    // candidate's eligibility walk and neighbour plan read, the same state
    // the tile writer will read when it codes the `motion_mode` symbol.
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> (BlockCoeffs, f64) {
    // `luma_set` is the INTRA candidates' table; the two inter candidates
    // below price their coefficients through `TxbSet::Luma32Inter`, which is
    // what `sb_coeff_inter_frame_tile` codes an inter block with (it carries
    // the inter `tx_type` symbol `Luma32` has no table for -- lane-av1rd1).
    let (luma_set, chroma_set) = (TxbSet::Luma32, TxbSet::Chroma16);
    let reach = Reach::of(BLOCK, x, y, luma.true_width, luma.true_height, fctx);

    // skip / intra_inter symbol costs at a fixed context -- see this
    // function's doc comment.
    let skip_bits = |skip: bool| symbol_bits(&cdf::SKIP[0], usize::from(skip));
    let intra_inter_bits = |inter: bool| symbol_bits(&cdf::INTRA_INTER[0], usize::from(inter));
    // `write_single_ref`'s own tree, at the fixed context this pricer uses
    // for every neighbour-derived symbol: LAST is p1=0,p3=0,p4=0 and GOLDEN
    // p1=0,p3=1,p5=1.
    let single_ref_bits = |ref_frame: i8| {
        symbol_bits(&cdf::SINGLE_REF[0][0], 0)
            + if ref_frame == crate::mvstack::GOLDEN_FRAME {
                symbol_bits(&cdf::SINGLE_REF[0][2], 1) + symbol_bits(&cdf::SINGLE_REF[0][4], 1)
            } else {
                symbol_bits(&cdf::SINGLE_REF[0][2], 0) + symbol_bits(&cdf::SINGLE_REF[0][3], 0)
            }
    };

    struct Candidate {
        cost: f64,
        luma: Trial,
        u: Trial,
        v: Trial,
        mode: u8,
        skip: bool,
        inter: Option<InterInfo>,
        /// The mode/mv syntax this candidate pays for, apart from its
        /// residual -- what the OBMC candidate below has to re-pay when it
        /// re-prices the winner's prediction.
        mode_bits: f64,
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
        0,
        search.base_q_idx,
        search.deadzone, fctx,
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
        0,
        search.base_q_idx,
        search.deadzone, fctx,
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
                search.lambda, fctx,
            );
            prune_by_sad(search.modes, scored, k)
        }
        _ => search.modes.to_vec(),
    };
    census_add(5, 1);
    census_add(6, intra_modes.len());
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
            0,
            search.base_q_idx,
            search.deadzone, fctx,
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
            mode_bits: mode_bits[usize::from(mode)],
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
    // Every single-reference mode the writer can code (spec 5.11.24's symbol
    // chain, mirrored by `tile::write_inter_mode`), priced with the static
    // CDFs at this block's own stack contexts. A candidate is a motion
    // vector plus the cheapest syntax that reaches it, so two modes landing
    // on the same vector share one set of trials: the added modes cost
    // mode-bit arithmetic, not another motion compensation.
    let not_new = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1);
    let not_zero = symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1);
    // `tile::write_drl_idx`'s cost: one `drl_mode` symbol per stack entry
    // past `start`, `1` to advance and `0` to stop, at most two. A `target`
    // the loop cannot reach is never offered below -- every candidate's
    // index is one this same loop walks to.
    let drl_bits = |start: usize, target: usize| drl_bits_for(stack, start, target);
    let last_ref_bits = single_ref_bits(crate::mvstack::LAST_FRAME);
    let mut cands: Vec<((i32, i32), f64, InterInfo)> = vec![
        (
            stack.nearest_mv,
            last_ref_bits + not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: stack.nearest_mv,
                ref_mv_idx: 0,
            },
        ),
        // GLOBALMV is the identity model's zero vector -- this encoder writes
        // no `gm_params` -- and the cheapest syntax of the four on the static
        // content where that vector is also the right one.
        (
            (0, 0),
            last_ref_bits + not_new + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::GlobalMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            },
        ),
    ];
    let near_bits =
        last_ref_bits + not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 1);
    for idx in 1..=2usize {
        if let Some(e) = stack.entries.get(idx) {
            cands.push((
                e.mv,
                near_bits + drl_bits(1, idx),
                InterInfo {
                    ref1: None,
                    mv1: (0, 0),
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearMv,
                    mv: e.mv,
                    ref_mv_idx: idx as u8,
                },
            ));
        }
    }

    let source_block = luma.source_block(x, y, BLOCK);
    let (seeds, seed_n) = mv_seeds(stack);
    #[cfg(test)]
    let t = stage_start();
    let found = motion::search(
        ref_luma.0,
        ref_luma.1,
        ref_luma.2,
        ref_luma.3,
        &source_block,
        x,
        y,
        BLOCK,
        BLOCK,
        stack.pred_mv,
        &seeds[..seed_n],
        search.lambda, fctx,
        1,
    );
    #[cfg(test)]
    stage_since(0, t);
    let mv = round_to_valid_mv(found.mv, stack.pred_mv);
    // Each DRL entry is a different `PredMv` (spec 7.10.2.10 `assign_mv`), so
    // the same vector costs a different residual from each: take the index
    // whose residual plus DRL bits is cheapest, not entry 0 by default.
    if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(stack, mv) {
        cands.push((
            mv,
            last_ref_bits + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0) + bits,
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NewMv,
                mv,
                ref_mv_idx,
            },
        ));
    }

    // Each extra reference's own `NEWMV` vector, for the compound
    // `NEW_NEWMV` candidate below to pair with `LAST`'s.
    let mut extra_new_mvs: Vec<(i8, (i32, i32))> = Vec::new();
    for &(ref_frame, g, gstack) in extra {
        let g_ref_bits = single_ref_bits(ref_frame);
        let g_not_new = symbol_bits(&cdf::NEW_MV[gstack.new_mv_ctx], 1);
        cands.push((
            gstack.nearest_mv,
            g_ref_bits
                + g_not_new
                + symbol_bits(&cdf::ZERO_MV[gstack.zero_mv_ctx], 1)
                + symbol_bits(&cdf::REF_MV[gstack.ref_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame,
                mode: InterMode::NearestMv,
                mv: gstack.nearest_mv,
                ref_mv_idx: 0,
            },
        ));
        cands.push((
            (0, 0),
            g_ref_bits + g_not_new + symbol_bits(&cdf::ZERO_MV[gstack.zero_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame,
                mode: InterMode::GlobalMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            },
        ));
        // A motion search of this reference's own: without it, GOLDEN/ALTREF
        // can only be coded at a vector some neighbour already found, which
        // is why they carried a third of the inter blocks at their two
        // search-free modes alone.
        // The search is skipped when this reference's own NEARESTMV already
        // prices out `margin` times better than LAST's best vector does --
        // one candidate evaluation (`motion::cost_at`) standing in for a
        // whole search of ~50.
        let margin = extra_new_skip_margin();
        let skip_search = extra_ref_new_mv()
            && margin > 0.0
            && motion::cost_at(
                &g.y,
                g.width,
                luma.true_width,
                luma.true_height,
                &source_block,
                x,
                y,
                BLOCK,
                BLOCK,
                gstack.nearest_mv,
                gstack.pred_mv,
                search.lambda,
                fctx,
            ) * margin
                <= found.cost;
        if extra_ref_new_mv() && !skip_search {
            let (gseeds, gn) = mv_seeds(gstack);
            #[cfg(test)]
            let t = stage_start();
            let gfound = motion::search(
                &g.y,
                g.width,
                luma.true_width,
                luma.true_height,
                &source_block,
                x,
                y,
                BLOCK,
                BLOCK,
                gstack.pred_mv,
                &gseeds[..gn],
                search.lambda, fctx,
                ref_distance(ref_frame),
            );
            #[cfg(test)]
            stage_since(0, t);
            let gmv = round_to_valid_mv(gfound.mv, gstack.pred_mv);
            extra_new_mvs.push((ref_frame, gmv));
            if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(gstack, gmv) {
                cands.push((
                    gmv,
                    g_ref_bits + symbol_bits(&cdf::NEW_MV[gstack.new_mv_ctx], 0) + bits,
                    InterInfo {
                        ref1: None,
                        mv1: (0, 0),
                        ref_frame,
                        mode: InterMode::NewMv,
                        mv: gmv,
                        ref_mv_idx,
                    },
                ));
            }
        }
    }

    // The compound candidates (spec 5.11.24's `INTER_COMPOUND_MODES`), one
    // trial set each: both references predict the block and the two
    // predictions are averaged, which is what pays on content where neither
    // reference alone is right. Priced with the same static-CDF approximation
    // the single-reference candidates use, plus the compound-only symbols
    // (`comp_mode`, the reference-pair tree, `comp_group_idx`/`compound_idx`).
    // The two compound-mode arms, each measured alone on the BD gate before
    // it could default on. `NEAREST_NEWMV`/`NEW_NEARESTMV` keep (-1.0/-0.8
    // vs libaom, screen byte-identical) and ship on; `NEAR_NEARMV` does not
    // (+0.5/-0.2/flat) and stays behind `EC_AV1_COMP_NEARNEAR`.
    // RE-MEASURED on lane-av1rejudge at LAMBDA_SCALE 0.05 with warp on:
    // +16.6/-0.7, +47.0/+19.1, +51.3/-14.3 against the base's +16.7/-0.5,
    // +47.0/+19.1, +51.3/-14.3 -- one row 0.1/0.2 down, two flat, short of
    // the keep rule. Still behind the knob.
    let near_near = crate::envflags::env_flag!("EC_AV1_COMP_NEARNEAR");
    let half_new = true;
    for (ref1, g, cstack) in compound {
        let (ref1, g) = (*ref1, *g);
        let uni = (crate::mvstack::BWDREF_FRAME..=crate::mvstack::ALTREF_FRAME)
            .contains(&ref1);
        let pair_bits = symbol_bits(&cdf::COMP_MODE[0], 1)
            + if uni {
                // LAST + a backward reference: `comp_ref_type` = bidirectional,
                // `comp_ref` LAST, `comp_bwdref` ALTREF.
                symbol_bits(&cdf::COMP_REF_TYPE[0], 1)
                    + symbol_bits(&cdf::COMP_REF[0][0], 0)
                    + symbol_bits(&cdf::COMP_REF[0][1], 0)
                    + symbol_bits(&cdf::COMP_BWDREF[0][0], 1)
            } else {
                // LAST + GOLDEN: unidirectional, `uni_comp_ref` p0/p1/p2.
                symbol_bits(&cdf::COMP_REF_TYPE[0], 0)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][0], 0)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][1], 1)
                    + symbol_bits(&cdf::UNI_COMP_REF[0][2], 1)
            };
        let mode_ctx =
            cdf::COMPOUND_MODE_CTX_MAP[cstack.ref_mv_ctx >> 1][cstack.new_mv_ctx.min(4)];
        let mode_bits_of =
            |mode: usize| pair_bits + symbol_bits(&cdf::INTER_COMPOUND_MODE[mode_ctx], mode);
        let mut ccands: Vec<(((i32, i32), (i32, i32)), f64, InterInfo)> = vec![
            (
                cstack.nearest_mv,
                mode_bits_of(0),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearestNearestMv,
                    mv: cstack.nearest_mv.0,
                    mv1: cstack.nearest_mv.1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
            (
                ((0, 0), (0, 0)),
                mode_bits_of(6),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::GlobalGlobalMv,
                    mv: (0, 0),
                    mv1: (0, 0),
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
        ];
        // `NEAR_NEARMV`: both halves off compound stack entry 1 (the
        // decoder's `ref_mv_idx = 0`), which costs one DRL symbol whenever
        // the stack is deep enough for that walk to read one.
        if let Some(e) = cstack.entries.get(1).filter(|_| near_near) {
            let drl = if cstack.entries.len() > 2 {
                symbol_bits(&cdf::DRL_MODE[cstack.drl_ctx[1]], 0)
            } else {
                0.0
            };
            ccands.push((
                (e.mv0, e.mv1),
                mode_bits_of(1) + drl,
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearNearMv,
                    mv: e.mv0,
                    mv1: e.mv1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ));
        }
        // The half-new modes and `NEW_NEWMV`, all seeded from the two
        // single-reference searches this block already ran -- no joint search
        // of its own. Every residual is priced against stack entry 0, which
        // is what `assign_compound_mv` predicts from at `ref_mv_idx = 0`.
        let cbase = cstack
            .entries
            .first()
            .map_or(cstack.nearest_mv, |e| (e.mv0, e.mv1));
        if let Some(b0) = mv_residual_bits(mv, cbase.0).filter(|_| half_new) {
            ccands.push((
                (mv, cstack.nearest_mv.1),
                mode_bits_of(3) + b0,
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NewNearestMv,
                    mv,
                    mv1: cstack.nearest_mv.1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ));
        }
        if let Some(&(_, mv1)) = extra_new_mvs.iter().find(|(r, _)| *r == ref1) {
            if let Some(b1) = mv_residual_bits(mv1, cbase.1).filter(|_| half_new) {
                ccands.push((
                    (cstack.nearest_mv.0, mv1),
                    mode_bits_of(2) + b1,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NearestNewMv,
                        mv: cstack.nearest_mv.0,
                        mv1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
            }
            let base = cbase;
            if let (Some(b0), Some(b1)) = (
                mv_residual_bits(mv, base.0),
                mv_residual_bits(mv1, base.1),
            ) {
                ccands.push((
                    (mv, mv1),
                    mode_bits_of(7) + b0 + b1,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NewNewMv,
                        mv,
                        mv1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
            }
        }
        ccands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        ccands.dedup_by_key(|c| c.0);
        let g_luma = (g.y.as_slice(), g.width, luma.true_width, luma.true_height);
        let g_u = (g.u.as_slice(), g.width / 2, chroma[0].true_width, chroma[0].true_height);
        let g_v = (g.v.as_slice(), g.width / 2, chroma[1].true_width, chroma[1].true_height);
        for (mvs, bits, info) in ccands {
            let luma_trial = mc_trial_compound(
                luma, x, y, BLOCK, mvs, true,
                (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3), g_luma,
                search.base_q_idx, search.deadzone, TxbSet::Luma32Inter, fctx,
            );
            let u = mc_trial_compound(
                &chroma[0], x / 2, y / 2, BLOCK / 2, mvs, false,
                (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3), g_u,
                search.base_q_idx, search.deadzone, chroma_set, fctx,
            );
            let v = mc_trial_compound(
                &chroma[1], x / 2, y / 2, BLOCK / 2, mvs, false,
                (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3), g_v,
                search.base_q_idx, search.deadzone, chroma_set, fctx,
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
                        + bits
                        + if skip { 0.0 } else { luma_trial.bits + u.bits + v.bits });
            consider(Candidate {
                cost,
                luma: luma_trial,
                u,
                v,
                mode: DC_PRED,
                skip,
                mode_bits: bits,
                inter: Some(info),
            });
        }
    }

    // One trial set per distinct (reference, vector) pair: sort, cheapest
    // syntax first, and drop the rest of each pair's run.
    cands.sort_by(|a, b| {
        (a.2.ref_frame, a.0)
            .cmp(&(b.2.ref_frame, b.0))
            .then(a.1.total_cmp(&b.1))
    });
    cands.dedup_by_key(|c| (c.2.ref_frame, c.0));
    census_add(7, cands.len());
    {
        for (mv, mode_bits_inter, info) in cands {
            let (ref_luma, ref_u, ref_v) = match extra
                .iter()
                .find(|(r, _, _)| *r == info.ref_frame)
            {
                Some(&(_, g, _)) => (
                    (&g.y, g.width, luma.true_width, luma.true_height),
                    (&g.u, g.width / 2, chroma[0].true_width, chroma[0].true_height),
                    (&g.v, g.width / 2, chroma[1].true_width, chroma[1].true_height),
                ),
                None => (ref_luma, ref_u, ref_v),
            };
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
                TxbSet::Luma32Inter, fctx,
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
                chroma_set, fctx,
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
                chroma_set, fctx,
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
                mode_bits: mode_bits_inter,
                inter: Some(info),
            });
        }
    }

    let best = best.expect("the search offers at least the intra modes");
    // lane-av1obmc: the OBMC candidate (spec `motion_mode == OBMC_CAUSAL`),
    // offered to the SINGLE-REFERENCE winner only -- libaom's
    // `motion_mode_allowed` fails `is_motion_variation_allowed_compound` on a
    // compound block, so the writer codes no symbol for one. The prediction
    // is built by the DECODER's own plan and blend
    // ([`crate::decode::obmc_plan`]/[`crate::decode::obmc_run`],
    // `av1_build_obmc_inter_prediction`) off the committed neighbours' mvs
    // this search's `grid` already carries, so what is priced here is what a
    // decoder reconstructs -- never a second implementation of the blend.
    //
    // MEASURED 2026-09-06 and NOT KEPT ON BY DEFAULT (`EC_AV1_OBMC=1` turns it
    // on). Native crop BD, the standing recipe, with it on against the same
    // build with it off: film 1080p +18.2/+0.9 against +18.0/+0.7, film 2160p
    // +47.3/+19.2 against +47.3/+19.2, screen +59.1/-10.0 against
    // +59.4/-10.0. That is one row down (screen -0.3 vs libaom), one flat and
    // one UP -- the keep rule wants two down and one flat, so it stays off.
    // The 640x384 gate reads the same shape louder: +79.7/+95.5/+54.2 and
    // +37.2/+56.8/+0.8 against +78.8/+95.1/+54.5 and +36.1/+56.4/+1.1. Two
    // things bound it and are the upgrade path: only the 32x32 winner is
    // offered OBMC (the 16x16/8x8 leaves in `code_square_inter` are not, and
    // that is where libaom cashes most of its OBMC gain), and the symbol/
    // residual price here is the static-CDF estimate.
    let mut best = best;
    let mut motion_won = 0u8;
    if let Some(info) = best.inter.filter(|i| {
        i.ref1.is_none()
            && (crate::envflags::env_flag!("EC_AV1_OBMC")
                || warp_on())
    }) {
        let mut refs: [Option<&Picture>; 8] = [None; 8];
        refs[LAST_FRAME as usize] = Some(reference);
        for &(r, p, _) in extra {
            refs[r as usize] = Some(p);
        }
        // This block's OWN reference planes: the single-reference winner may
        // be one of the extra references, not LAST.
        let planes: [(&[u16], usize, usize, usize); 3] =
            match extra.iter().find(|(r, _, _)| *r == info.ref_frame) {
                Some(&(_, g, _)) => [
                    (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                    (g.u.as_slice(), g.width / 2, chroma[0].true_width, chroma[0].true_height),
                    (g.v.as_slice(), g.width / 2, chroma[1].true_width, chroma[1].true_height),
                ],
                None => [
                    (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                    (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                    (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                ],
            };
        // The alphabet `tile::write_motion_mode` will code this block's
        // symbol against, mirrored term for term (libaom
        // `motion_mode_allowed`).
        let warp_alphabet = warp_on()
            && crate::decode::num_proj_ref(
                grid,
                mi_row,
                mi_col,
                8,
                8,
                mi_cols,
                mi_rows,
                info.ref_frame,
                fctx,
            ) >= 1;
        let mm = |m: u8| motion_mode_bits(BLOCK, BLOCK, m, warp_alphabet);
        let mut candidates: Vec<(u8, [Vec<u8>; 3])> = Vec::new();
        if crate::envflags::env_flag!("EC_AV1_OBMC") {
            if let Some(pred) = obmc_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                BLOCK,
                info.mv,
                &refs,
                planes,
                reference.width,
                fctx,
            ) {
                candidates.push((1, pred));
            }
        }
        if warp_alphabet {
            if let Some(pred) = warp_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                BLOCK,
                info.mv,
                info.ref_frame,
                planes,
                fctx,
            ) {
                candidates.push((2, pred));
            }
        }
        for (motion, pred) in candidates {
            let luma_trial = luma.code_from_prediction(
                x, y, BLOCK, &pred[0], false, search.base_q_idx, search.deadzone,
                TxbSet::Luma32Inter,
            );
            let u = chroma[0].code_from_prediction(
                x / 2, y / 2, BLOCK / 2, &pred[1], false, search.base_q_idx, search.deadzone,
                chroma_set,
            );
            let v = chroma[1].code_from_prediction(
                x / 2, y / 2, BLOCK / 2, &pred[2], false, search.base_q_idx, search.deadzone,
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
                        + best.mode_bits
                        + mm(motion)
                        + if skip { 0.0 } else { luma_trial.bits + u.bits + v.bits });
            if cost < best.cost + search.lambda * mm(motion_won) {
                motion_won = motion;
                best = Candidate {
                    cost,
                    luma: luma_trial,
                    u,
                    v,
                    mode: DC_PRED,
                    skip,
                    mode_bits: best.mode_bits,
                    inter: best.inter,
                };
            }
        }
    }
    // The transform-depth search of a 32x32 winner (`EC_AV1_TX32_DEPTH`,
    // default OFF): the four 16x16 units of `commit_inter_luma` against the
    // flat trial, exactly as a 16x16 leaf is offered. lane-av1txbits measured
    // it flat-to-worse at LAMBDA_SCALE 0.1; lane-av1txdepth REBUILT it and
    // RE-MEASURED it at 0.05, where the rate weight is half of what rejected
    // it, and it is worse again on the deciding native table: film 1080p
    // +16.6/-0.5, film 2160p +46.6/+18.8, screen +50.9/-14.3 against the
    // +16.2/-1.1, +46.6/+18.8, +51.3/-14.4 of the same build with it off --
    // 1080p up 0.4/0.6, one row flat, screen 0.4 down against libaom and 0.1
    // up against rav1e. (640x384 reads it slightly the other way, +70.0/
    // +85.0/+48.4 and +28.3/+47.2/-3.6 against +70.4/+85.3/+48.5 and
    // +28.4/+47.4/-4.0 -- the native table decides.)
    //
    // NOT blindness: the split wins 62.3% of the 1080p film's 32x32 blocks,
    // 70.9% of the 4K film's and 53.5% of the screen's. NOT mis-pricing
    // either, and the census names the row: the split units' own set
    // `Luma16Inter` is priced within 1.4% of what it is written at in every
    // bucket from two non-zeros up, and its ZERO bucket (335 units) is priced
    // +30.1% ABOVE the written bits -- an error that biases AGAINST splitting,
    // so a perfect pricer would split even more, and more splitting is what
    // measured worse. The four extra txb_skip/eob symbols simply cost more
    // ladder bytes than the local RD saves. The flat 32x32 transform stays.
    // lane-av1txdepth REBUILT that search at LAMBDA_SCALE 0.05 (the rejection
    // above was measured at 0.1), behind `EC_AV1_TX32_DEPTH`, and gave the
    // compound winner the same offer behind `EC_AV1_COMP_VARTX`. An OBMC/warp
    // winner still commits flat: `commit_inter_luma` re-predicts each unit by
    // plain translation, which is not the prediction it was priced from.
    let (levels, tx_depth, dcost) = match best.inter.filter(|_| motion_won == 0) {
        Some(info) => {
            let own = match extra.iter().find(|(r, _, _)| *r == info.ref_frame) {
                Some(&(_, g, _)) => (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                None => (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
            };
            let second = info.ref1.and_then(|r| {
                compound.iter().find(|(c, _, _)| *c == r).map(|&(_, g, _)| {
                    (
                        info.mv1,
                        (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                    )
                })
            });
            match info.ref1.is_some() && second.is_none() {
                true => {
                    luma.commit(x, y, BLOCK, &best.luma);
                    (best.luma.levels.clone(), 0, 0.0)
                }
                false => commit_inter_luma(
                    luma, (x, y), BLOCK, info.mv, own, search, &best.luma, best.skip, second,
                    fctx,
                ),
            }
        }
        None => {
            luma.commit(x, y, BLOCK, &best.luma);
            (best.luma.levels.clone(), 0, 0.0)
        }
    };
    chroma[0].commit(x / 2, y / 2, BLOCK / 2, &best.u);
    chroma[1].commit(x / 2, y / 2, BLOCK / 2, &best.v);
    (
        BlockCoeffs {
            angle_delta_y: 0,
            cfl_alphas: None,
            filter_intra: None,
            luma: coeffs(&levels, BLOCK),
            u: coeffs(&best.u.levels, BLOCK / 2),
            v: coeffs(&best.v.levels, BLOCK / 2),
            mode: best.mode,
            uv_mode: DC_PRED,
            skip: best.skip,
            inter: best.inter,
            motion_mode: motion_won,
            eight: None,
            dv: None,
            palette: None,
            palette_uv: None,
            tx_depth,
        },
        best.cost + dcost,
    )
}

/// lane-av1tpl: the strength `k` of the temporal lambda weighting
/// ([`tpl_lambda_factors`]). 0 keeps every block at the frame's own lambda, which
/// is what every measurement before this lane was taken on. Swept by
/// `EC_AV1_TPL=<k>`.
///
/// SWEPT on lane-av1tpl at the native gate (1920x1024, 12 frames, BD-rate vs
/// libaom `cpu-used 6` / rav1e `speed 6`); `k=0` reproduces the pre-lane
/// baseline exactly, which is the control the rest of the table is read
/// against:
///
/// | k | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | 0 | +16.2/-1.1 | +46.6/+18.8 | +51.3/-14.4 |
/// | 0.25 | +16.4/-0.7 | +46.6/+18.8 | +51.2/-14.4 |
/// | **0.5** | **+15.9/-1.3** | **+46.5/+18.7** | **+51.2/-14.4** |
/// | 1 | +16.2/-0.9 | +46.6/+18.8 | +51.2/-14.4 |
/// | 2 | +16.5/-0.7 | +46.7/+19.0 | +51.1/-14.4 |
///
/// 0.5 is the only arm that is at least flat on every row and down on five of
/// the six columns, so it ships as the default. The lookahead pass that feeds
/// it costs 1.5 ms per 1920x1024 frame against a ~0.8 s encode (0.2%).
const TPL_STRENGTH: f64 = 0.5;

fn tpl_strength() -> f64 {
    std::env::var("EC_AV1_TPL")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|k| k.is_finite() && *k >= 0.0)
        .unwrap_or(TPL_STRENGTH)
}

/// lane-av1tpl2: how many SOURCE pictures the lambda map looks at, counting
/// the frame being coded. 1 turns the weighting off; 2 is the shipped
/// one-frame area map ([`tpl_area_map`]); 3 and up only mean anything under
/// [`tpl_propagating`], which is the map that can actually use them and is
/// the default since lane-av1tpl3, so the default window is 8. Swept by
/// `EC_AV1_TPL_D=<d>`.
///
/// The streaming facade holds `depth - 1` pictures back so it feeds
/// `encode_inter_frame` the same window `encode_sequence` does; both read
/// THIS function, so the two paths cannot disagree about the window.
pub(crate) const TPL_DEPTH: usize = 8;

/// The exponent the dependence ratio is read through ([`tpl_lambda_factors`]).
/// 0.5 is libaom's own square-root compression of its dependence ratio.
const TPL_POWER: f64 = 0.5;

/// lane-av1tpl2: the propagating map ([`tpl_lambda_factors`]) instead of the
/// one-frame area map ([`tpl_area_map`]), behind `EC_AV1_TPL_PROP=1`.
///
/// MEASURED at the native gate (1920x1024, 12 frames, BD-rate vs libaom
/// `cpu-used 6` / rav1e `speed 6`), against the shipped one-frame map
/// (+15.9/-1.3, +46.5/+18.7, +51.2/-14.4). `p` is [`TPL_POWER`]:
///
/// | k | D | p | film 1080p | film 2160p | screen |
/// |---|---|---|---|---|---|
/// | 0.0625 | 8 | 1 | +15.7/-1.6 | +46.6/+18.8 | +51.1/-14.5 |
/// | 0.125 | 4 | 1 | +15.7/-1.6 | +46.7/+18.9 | +51.2/-14.5 |
/// | 0.125 | 8 | 1 | +15.9/-1.3 | +46.8/+19.0 | +50.7/-14.7 |
/// | 0.25 | 4 | 1 | +15.5/-1.8 | +46.8/+19.1 | +51.2/-14.5 |
/// | 0.25 | 8 | 1 | +15.7/-1.6 | +46.9/+19.1 | +50.7/-14.6 |
/// | 0.5 | 4 | 1 | +16.1/-1.2 | +47.0/+19.2 | +51.1/-14.6 |
/// | 0.5 | 8 | 1 | +16.0/-1.4 | +47.1/+19.3 | +51.8/-13.9 |
/// | 1 | 4 | 1 | +16.2/-1.3 | +47.2/+19.4 | +51.9/-13.9 |
/// | 2 | 4 | 1 | +17.2/-0.5 | +47.2/+19.5 | +57.0/-10.7 |
/// | **0.5** | **8** | **0.5** | **+15.5/-1.7** | **+46.7/+19.0** | **+50.8/-14.6** |
/// | 0.5 | 4 | 0.5 | +15.6/-1.6 | +46.8/+19.0 | +51.0/-14.6 |
/// | 1 | 8 | 0.5 | +15.9/-1.5 | +47.0/+19.2 | +51.0/-14.5 |
///
/// The map is genuinely non-flat (0.25..1.12 across 7680 cells, only 58 of
/// them within 0.9..1.1, where the one-frame map had 7127 of 7680 inside
/// 1..1.5x its own mean), and its best arm was 0.4 down on both film 1080p
/// columns and 0.4/0.2 down on screen -- but EVERY arm of that lane was
/// 0.1..0.4 UP on film 2160p, so it shipped off.
///
/// lane-av1tpl3 found why and turned it on. The denominator is the cell's own
/// MAD from its DC, and on the gate's film clips (ffmpeg `testsrc2`, colour
/// bars) 89.4% (1080p) / 90.8% (2160p) of all 16x16 cells have a MAD of
/// EXACTLY ZERO -- so a per-16x16 ratio is a huge number divided by 1
/// wherever the window leans on a flat bar, which is exactly where coding is
/// cheap. Summing intra and mc_dep over the 64x64 superblock before the ratio
/// (libaom's own granularity) puts a busy cell's denominator next to the flat
/// one's, and reading the ratio through `log2(1 + r)` (libaom's compressed
/// beta) flattens what is left of the tail. Both on, at k=0.5 D=8 p=0.5, on
/// the same native gate (baseline = the shipped one-frame map):
///
/// | arm | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | one-frame map (was default) | +15.9/-1.3 | +46.5/+18.7 | +51.2/-14.4 |
/// | propagating, per-16x16 | +15.5/-1.7 | +46.7/+19.0 | +50.8/-14.6 |
/// | + denominator floor 0.25x mean | +15.7/-1.5 | +46.9/+19.1 | +50.9/-14.6 |
/// | + floor 0.5x mean | +15.7/-1.5 | +46.8/+19.0 | +50.8/-14.5 |
/// | + floor 1.0x mean | +15.8/-1.4 | +46.9/+19.1 | +51.0/-14.5 |
/// | + 64x64 aggregation | +15.9/-1.3 | +46.6/+18.8 | +51.1/-14.6 |
/// | + log2 beta | +16.0/-1.3 | +46.9/+19.0 | +51.0/-14.6 |
/// | + aggregation + floor 0.25x | +15.7/-1.5 | +46.6/+18.9 | +50.9/-14.6 |
/// | + aggregation + floor 0.5x | +16.0/-1.3 | +46.9/+19.1 | +50.8/-14.7 |
/// | **+ aggregation + log2 beta** | **+15.6/-1.7** | **+46.5/+18.8** | **+51.0/-14.5** |
///
/// The last row is the only arm that is down on two rows (film 1080p 0.3/0.4,
/// screen 0.2/0.1) and flat on the third (film 2160p 0.0/+0.1), so it ships
/// as the default: `EC_AV1_TPL_PROP=0` goes back to the one-frame map. The
/// floor is a real effect on the ratio and never a win once the aggregation
/// is in -- the aggregation is the same fix, done where libaom does it.
fn tpl_propagating() -> bool {
    std::env::var("EC_AV1_TPL_PROP").ok().as_deref() != Some("0")
}

pub(crate) fn tpl_depth() -> usize {
    std::env::var("EC_AV1_TPL_D")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|d| *d >= 1)
        .unwrap_or_else(|| crate::speed::at(&crate::speed::TPL_DEPTH))
        .max(1)
}

/// Nanoseconds the lookahead pass ([`tpl_lambda_factors`]) has spent this
/// process, so the gate can price it against the encode it rides on.
static TPL_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One coarse full-pel 16x16 motion pass of `next` against `cur`: per cell,
/// the winning displacement and its SAD.
///
/// Deterministic: integer SADs, a fixed 3-step pattern seeded at zero and at
/// the left/above winners, first-best tie-break. Built once on the frame's
/// own thread and read-only afterwards, so it cannot make a tile's bytes
/// depend on the thread count.
fn tpl_coarse_pass(
    cur: &[u8],
    next: &[u8],
    width: usize,
    height: usize,
) -> Vec<((i32, i32), u32)> {
    let (cells_x, cells_y) = (width / 16, height / 16);
    let sad = |bx: usize, by: usize, (mx, my): (i32, i32)| -> u32 {
        let (sx, sy) = (bx as i32 + mx, by as i32 + my);
        if sx < 0 || sy < 0 || sx as usize + 16 > width || sy as usize + 16 > height {
            return u32::MAX;
        }
        let (sx, sy) = (sx as usize, sy as usize);
        let mut sum = 0u32;
        for r in 0..16 {
            let a = &next[(by + r) * width + bx..][..16];
            let b = &cur[(sy + r) * width + sx..][..16];
            sum += a.iter().zip(b).map(|(&p, &q)| u32::from(p.abs_diff(q))).sum::<u32>();
        }
        sum
    };
    let mut out = Vec::with_capacity(cells_x * cells_y);
    let mut above = vec![(0i32, 0i32); cells_x];
    for cy in 0..cells_y {
        let mut left = (0i32, 0i32);
        for cx in 0..cells_x {
            let (bx, by) = (cx * 16, cy * 16);
            let mut best = (sad(bx, by, (0, 0)), (0i32, 0i32));
            for cand in [left, above[cx]] {
                if cand != (0, 0) {
                    let s = sad(bx, by, cand);
                    if s < best.0 {
                        best = (s, cand);
                    }
                }
            }
            for step in [8i32, 4, 2, 1] {
                let centre = best.1;
                for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                    let mv = (centre.0 + dx, centre.1 + dy);
                    if mv.0.abs() > 32 || mv.1.abs() > 32 {
                        continue;
                    }
                    let s = sad(bx, by, mv);
                    if s < best.0 {
                        best = (s, mv);
                    }
                }
            }
            left = best.1;
            above[cx] = best.1;
            out.push((best.1, best.0));
        }
    }
    out
}

/// The intra-cost proxy libaom's tpl gets from a DC prediction plus a
/// transform: here, the mean absolute deviation of the cell from its own DC.
/// It is the denominator every propagated cost is read against, so only its
/// SHAPE across the frame matters, not its scale.
fn tpl_intra_costs(frame: &[u8], width: usize, height: usize) -> Vec<u32> {
    let (cells_x, cells_y) = (width / 16, height / 16);
    let mut out = Vec::with_capacity(cells_x * cells_y);
    for cy in 0..cells_y {
        for cx in 0..cells_x {
            let (bx, by) = (cx * 16, cy * 16);
            let mut sum = 0u32;
            for r in 0..16 {
                sum += frame[(by + r) * width + bx..][..16].iter().map(|&p| u32::from(p)).sum::<u32>();
            }
            let dc = ((sum + 128) / 256) as u8;
            let mut cost = 0u32;
            for r in 0..16 {
                cost += frame[(by + r) * width + bx..][..16]
                    .iter()
                    .map(|&p| u32::from(p.abs_diff(dc)))
                    .sum::<u32>();
            }
            out.push(cost);
        }
    }
    out
}

/// lane-av1tpl's one-frame map, in terms of the pass above: how much of THIS
/// frame the next one predicts from, per 16x16 cell -- each cell of `next`
/// splats its own area, at its winning displacement, onto the (at most four)
/// cells of `cur` it lands on. 1.0 is "referenced exactly once".
fn tpl_area_map(cur: &[u8], next: &[u8], width: usize, height: usize) -> Vec<f64> {
    let start = std::time::Instant::now();
    let (cells_x, cells_y) = (width / 16, height / 16);
    let pass = tpl_coarse_pass(cur, next, width, height);
    let mut area = vec![0.0f64; cells_x * cells_y];
    for cy in 0..cells_y {
        for cx in 0..cells_x {
            let (mv, _) = pass[cy * cells_x + cx];
            let (x0, y0) = ((cx * 16) as i32 + mv.0, (cy * 16) as i32 + mv.1);
            let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
            for ci in x0 / 16..=((x0 + 15) / 16).min(cells_x - 1) {
                let ox = (x0 + 16).min(ci * 16 + 16) - x0.max(ci * 16);
                for ri in y0 / 16..=((y0 + 15) / 16).min(cells_y - 1) {
                    let oy = (y0 + 16).min(ri * 16 + 16) - y0.max(ri * 16);
                    area[ri * cells_x + ci] += (ox * oy) as f64 / 256.0;
                }
            }
        }
    }
    TPL_NANOS.fetch_add(
        start.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    area
}

/// [`tpl_area_map`]'s per-cell referenced-ness turned into one lambda
/// factor per 64x64 superblock: `f = (1 + k) / (1 + k * w_rel)`, where
/// `w_rel` is the superblock's mean referenced-ness over the FRAME's mean.
/// Decreasing in referenced-ness (a block the next frame leans on is coded
/// more distortion-averse) and 1.0 at the frame's own mean, so the frame's
/// total lambda scale is only redistributed, never moved -- a global move is
/// the thing lane-av1lambda already measured inert.
fn tpl_sb_factors(map: &[f64], cells_x: usize, cells_y: usize, k: f64) -> Vec<f64> {
    let (sb_cols, sb_rows) = (cells_x / 4, cells_y / 4);
    let mean = (map.iter().sum::<f64>() / map.len().max(1) as f64).max(1e-6);
    let mut out = Vec::with_capacity(sb_cols * sb_rows);
    for sr in 0..sb_rows {
        for sc in 0..sb_cols {
            let mut sum = 0.0;
            for r in 0..4 {
                for c in 0..4 {
                    sum += map[(sr * 4 + r) * cells_x + sc * 4 + c];
                }
            }
            let w_rel = sum / 16.0 / mean;
            out.push(((1.0 + k) / (1.0 + k * w_rel)).clamp(0.25, 4.0));
        }
    }
    if std::env::var("EC_AV1_TPL_HIST").ok().as_deref() == Some("1") {
        let mut hist = [0usize; 8];
        for w in map {
            hist[((w / mean * 2.0) as usize).min(7)] += 1;
        }
        let (lo, hi) = out.iter().fold((f64::MAX, 0.0f64), |(l, h), &f| (l.min(f), h.max(f)));
        eprintln!(
            "tpl k={k}: {} cells mean w={mean:.3}, w/mean hist (0-.5,.5-1,..,>3.5) {hist:?}, \
             sb factor {lo:.3}..{hi:.3}, lookahead wall {:.1} ms cumulative",
            map.len(),
            TPL_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        );
    }
    out
}

/// libaom's `tpl_model_update` in this crate's cheapest shape: one lambda
/// factor per 16x16 cell of `frames[0]` (the frame being coded), from how
/// much of the WHOLE lookahead window leans on that cell.
///
/// Per lookahead level `f` (source `frames[f]` against `frames[f-1]`) the
/// coarse pass gives a displacement and an inter cost; a cell's own
/// propagated cost is `max(intra - inter, 0) + mc_dep` -- what coding that
/// cell well saves the frames that predict from it -- and it is splatted,
/// area-weighted, onto the (at most four) cells of `frames[f-1]` its
/// displacement lands on. Walking `f` from the far end of the window back to
/// 1 accumulates the whole chain into level 0, which lane-av1tpl's one-frame
/// area map could not do at all.
///
/// The factor is `f = 1 / (1 + k * mc_dep / intra)`, normalised so the mean
/// factor over the frame is exactly 1: a cell the window leans on is coded
/// more distortion-averse (smaller lambda), and the frame's TOTAL lambda
/// scale only moves around, never up or down -- a global move is what
/// lane-av1lambda already measured inert.
fn tpl_lambda_factors(frames: &[&[u8]], width: usize, height: usize, k: f64) -> Vec<f64> {
    let start = std::time::Instant::now();
    let (cells_x, cells_y) = (width / 16, height / 16);
    let cells = cells_x * cells_y;
    let levels = frames.len();
    let intra: Vec<Vec<u32>> = frames.iter().map(|f| tpl_intra_costs(f, width, height)).collect();
    let passes: Vec<Vec<((i32, i32), u32)>> = (1..levels)
        .map(|f| tpl_coarse_pass(frames[f - 1], frames[f], width, height))
        .collect();
    let mut mc_dep = vec![vec![0.0f64; cells]; levels];
    for f in (1..levels).rev() {
        for cy in 0..cells_y {
            for cx in 0..cells_x {
                let ci = cy * cells_x + cx;
                let (mv, inter) = passes[f - 1][ci];
                let saved = f64::from(intra[f][ci].saturating_sub(inter));
                let propagate = saved + mc_dep[f][ci];
                if propagate <= 0.0 {
                    continue;
                }
                // The 16x16 window of the PREVIOUS frame this cell predicted
                // from, spread over the cells it overlaps.
                let (x0, y0) = ((cx * 16) as i32 + mv.0, (cy * 16) as i32 + mv.1);
                let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
                for rc in x0 / 16..=((x0 + 15) / 16).min(cells_x - 1) {
                    let ox = (x0 + 16).min(rc * 16 + 16) - x0.max(rc * 16);
                    for rr in y0 / 16..=((y0 + 15) / 16).min(cells_y - 1) {
                        let oy = (y0 + 16).min(rr * 16 + 16) - y0.max(rr * 16);
                        mc_dep[f - 1][rr * cells_x + rc] += propagate * (ox * oy) as f64 / 256.0;
                    }
                }
            }
        }
    }
    // The tail compression `p`: `mc_dep/intra` is a ratio with a very long
    // tail (a nearly flat cell the window leans on has a tiny denominator),
    // and at p = 1 that tail slams into the clamp. libaom reads its own
    // dependence ratio through a sqrt (`dr_from_beta`); `EC_AV1_TPL_P`
    // sweeps the exponent, 1 being the raw ratio.
    let power = std::env::var("EC_AV1_TPL_P")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p > 0.0)
        .unwrap_or(TPL_POWER);
    // lane-av1tpl3, the three shape fixes, each its own knob so the gate can
    // read them apart. `SB` and `LOG` are the shipped shape (set the knob to
    // `0` to take either back out); `FLOOR` defaults to off.
    //
    // * `EC_AV1_TPL_FLOOR=<f>` floors the denominator at `f` times the
    //   frame's own mean intra cost. A cell of flat content has a near-zero
    //   MAD, so its ratio explodes exactly where coding is cheap.
    // * `EC_AV1_TPL_SB=0` goes back to a per-16x16 ratio; by default intra
    //   and mc_dep are summed over the 64x64 superblock first -- libaom's
    //   granularity (`mc_dep_cost` and `intra_cost` are SB sums there, not
    //   per-16x16 numbers), which is also what makes the ratio survive one
    //   flat cell next to a busy one.
    // * `EC_AV1_TPL_LOG=0` reads the raw ratio; by default it goes through
    //   `log2(1 + r)`, libaom's log-compressed beta, which flattens the tail
    //   harder than the `p` exponent alone can.
    let floor_frac = std::env::var("EC_AV1_TPL_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|f| f.is_finite() && *f >= 0.0)
        .unwrap_or(0.0);
    let sb_agg = std::env::var("EC_AV1_TPL_SB").ok().as_deref() != Some("0");
    let log_beta = std::env::var("EC_AV1_TPL_LOG").ok().as_deref() != Some("0");
    let intra_mean =
        intra[0].iter().map(|&c| f64::from(c)).sum::<f64>() / cells.max(1) as f64;
    let floor = floor_frac * intra_mean;
    let denom = |c: usize| f64::from(intra[0][c].max(1)).max(floor).max(1.0);
    // Per-cell ratios, or one ratio per 64x64 superblock (4x4 cells) shared by
    // every cell under it. The edge groups are partial and summed as they are.
    let ratio: Vec<f64> = if sb_agg {
        let (sb_cols, sb_rows) = (cells_x.div_ceil(4), cells_y.div_ceil(4));
        let mut sb = vec![(0.0f64, 0.0f64); sb_cols * sb_rows];
        for cy in 0..cells_y {
            for cx in 0..cells_x {
                let e = &mut sb[(cy / 4) * sb_cols + cx / 4];
                e.0 += mc_dep[0][cy * cells_x + cx];
                e.1 += denom(cy * cells_x + cx);
            }
        }
        (0..cells)
            .map(|c| {
                let (dep, den) = sb[(c / cells_x / 4) * sb_cols + (c % cells_x) / 4];
                dep / den.max(1.0)
            })
            .collect()
    } else {
        (0..cells).map(|c| mc_dep[0][c] / denom(c)).collect()
    };
    let mut out: Vec<f64> = ratio
        .iter()
        .map(|&r| {
            let r = if log_beta { (1.0 + r).log2() } else { r };
            (1.0 + k * r).powf(-power)
        })
        .collect();
    let mean = (out.iter().sum::<f64>() / cells.max(1) as f64).max(1e-9);
    for f in &mut out {
        *f = (*f / mean).clamp(0.25, 4.0);
    }
    TPL_NANOS.fetch_add(
        start.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    if std::env::var("EC_AV1_TPL_HIST").ok().as_deref() == Some("1") {
        let mut hist = [0usize; 8];
        for &f in &out {
            hist[((f * 4.0) as usize).min(7)] += 1;
        }
        let (lo, hi) = out.iter().fold((f64::MAX, 0.0f64), |(l, h), &f| (l.min(f), h.max(f)));
        let flat = out.iter().filter(|&&f| (0.9..=1.1).contains(&f)).count();
        eprintln!(
            "tpl k={k} d={levels}: {cells} cells, factor hist (0-.25,.25-.5,..,>1.75) {hist:?}, \
             range {lo:.3}..{hi:.3}, {flat} within 0.9..1.1, lookahead wall {:.1} ms cumulative",
            TPL_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        );
    }
    out
}

/// Where one inter frame sits in a coding-order pyramid: which DPB slots its
/// references name, which slot it refreshes, whether it is shown when it is
/// coded, and the `ref_frame_sign_bias` that follows from all of it. `None`
/// keeps `encode_inter_frame`'s pre-pyramid flat behaviour verbatim (slot
/// 0/2 alternation, every frame shown, every reference in the past).
///
/// The sign bias is carried rather than derived here because only the caller
/// knows what order hint each slot holds; it is armed onto the tile writer
/// ([`crate::tile::arm_sign_bias`]) and set on the encoder's own MV grid, so
/// the writer scans the same stacks the decoder will.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PyramidFrame {
    /// The slot `LAST_FRAME` (and every reference this encoder does not name
    /// apart) reads.
    pub last_slot: u8,
    /// The single slot `refresh_frame_flags` sets.
    pub self_slot: u8,
    /// The slot `ALTREF_FRAME` reads -- a FUTURE frame for a leaf of a
    /// pyramid, which is what makes `sign_bias`'s last entry true.
    pub altref_slot: u8,
    /// `show_frame`: false for a hidden frame, output later by a
    /// `show_existing_frame` header.
    pub show_frame: bool,
    /// This frame's `ref_frame_sign_bias` (spec 5.9.2).
    pub sign_bias: crate::mvstack::SignBiasTable,
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
    // The order hint each of the seven `ref_frame_idx` entries names, which a
    // decoder reads out of its own reference state -- only spec 5.9.22's
    // `skipModeAllowed` (in `frame::write_frame_header`) reads it, and only
    // on a `reference_select` frame. [`flat_order_hints`] builds it for the
    // callers that follow this crate's own flat slot plan.
    order_hints: [u32; 7],
    render: (usize, usize),
    // The tables this frame's writer starts from: what the previous frame
    // stored into the slot this one's `primary_ref_frame` names. `None`
    // starts from the defaults, which is right only for the frame straight
    // after a key frame.
    start_cdfs: Option<&crate::cdf_state::Cdfs>,
    // The key frame's own (padded) reconstruction, which every frame of the
    // GOP still holds in DPB slot 1 -- `GOLDEN_FRAME`. `None` codes the frame
    // with `LAST_FRAME` alone, exactly as before.
    golden: Option<&Picture>,
    // The frame two back, held in the slot this frame is about to refresh
    // (the encoder alternates 0/2): `ALTREF_FRAME`. `None` for the first two
    // inter frames of a GOP, where the frame two back IS the key frame.
    altref: Option<&Picture>, fctx: &crate::decode::FrameCtx,
    // Where this frame sits in the coding-order pyramid; `None` is the flat
    // one-shown-frame-per-picture stream this encoder wrote before.
    pyramid: Option<PyramidFrame>,
    // lane-av1tpl2: the NEXT pictures' sources (up to [`tpl_depth`] - 1 of
    // them), padded like this one -- the lookahead window the per-block
    // lambda weighting propagates back through ([`tpl_lambda_factors`]). An
    // EMPTY slice codes the frame at one lambda, as before.
    lookahead: &[Picture],
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
    // Slot 0 and slot 2 alternate so the frame two back survives one more
    // frame: frame n writes A(n) = 0 for odd n, 2 for even, so A(n) = A(n-2)
    // and the picture this frame names as `ALTREF_FRAME` is the one sitting
    // in the very slot it will overwrite.
    let self_slot = pyramid.map_or(if order_hint % 2 == 1 { 0 } else { 2 }, |p| p.self_slot);
    let last_slot = pyramid.map_or(
        if order_hint <= 1 {
            LAST_SLOT
        } else if (order_hint - 1) % 2 == 1 {
            0
        } else {
            2
        },
        |p| p.last_slot,
    );
    // `render`, not `picture.width`/`picture.height`: `picture` here is
    // already padded to a whole number of superblocks (`encode_sequence`'s
    // `padded_to(SUPERBLOCK)`), and the header's `mi_cols`/`mi_rows` (and so
    // every true-edge clamp downstream) must come from the frame's real,
    // unpadded size, same as `key_frame_headers_colour` takes `render` and
    // not the padded picture in `encode_key_frame_inner`.
    let (seq, mut header) = inter_frame_headers_slots(
        render.0,
        render.1,
        base_q_idx,
        order_hint,
        last_slot,
        self_slot,
        match pyramid {
            Some(p) => p.altref_slot,
            None if altref.is_some() => self_slot,
            None => GOLDEN_SLOT,
        },
    )?;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;
    let sign_bias = pyramid.map_or(crate::mvstack::NO_SIGN_BIAS, |p| p.sign_bias);
    // The order hint behind each `ref_frame_idx` entry. The flat path's own
    // slot plan (`inter_frame_headers_slots`) is: every reference but
    // `GOLDEN_FRAME` reads `last_slot` (the frame just coded) except
    // `ALTREF_FRAME`, which reads the frame two back when there is one;
    // `GOLDEN_FRAME` is the key frame, order hint 0. Only
    // `frame::write_frame_header`'s `skipModeAllowed` reads this.
    header.order_hints = order_hints;
    header.reference_select = reference_select();
    // Armed HERE, not only beside the tile write below: `ref_distance`'s own
    // per-reference order-hint distance is read by the block SEARCH, which
    // runs long before the writer arms these (the distance-scaled search step
    // read `(0, 0, [0; 7])` and came out 1 for every reference -- the knob
    // never reached the tool).
    crate::tile::arm_order_hints(seq.order_hint_bits, order_hint, order_hints);
    if let Some(p) = pyramid {
        header.show_frame = p.show_frame;
        header.showable_frame = !p.show_frame;
        header.ref_frame_sign_bias = p.sign_bias;
    }
    // `TxMode::Select` here too: an inter block then codes one `txfm_split`
    // flag (kept at zero -- its var-tx tree is not searched yet) and an INTRA
    // block inside this frame codes and searches its own `tx_depth`, the same
    // as in a key frame (`crate::tile::write_tx_syntax_inter`).
    let tx_select = tx_select() && tx_select_inter();
    if tx_select {
        header.tx_mode = TxMode::Select;
    }

    let (true_width, true_height) = (header.mi_cols as usize * 4, header.mi_rows as usize * 4);
    // lane-hbd r4: encoder stays 8-bit by design; narrow the source once
    // here (see `encode_key_frame_inner`).
    let picture_y8: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let picture_u8: Vec<u8> = picture.u.iter().map(|&v| v as u8).collect();
    let picture_v8: Vec<u8> = picture.v.iter().map(|&v| v as u8).collect();
    // This frame's own `allow_screen_content_tools`. It can only be set when
    // the SEQUENCE offers the bit at all, which is the key frame's decision
    // ([`SEQ_SCREEN`]): a frame header that contradicts
    // `seq_force_screen_content_tools` is a corrupt stream, not a tighter
    // encode.
    let screen = seq_screen()
        && screen_content(&picture_y8, picture.width, true_width, true_height);
    header.allow_screen_content_tools = screen;
    // The search prices coefficients against the tables this frame's writer
    // starts from, not against the defaults (lane-av1txbits) -- but only on a
    // frame the screen detector said no to (lane-av1price2): armed HERE, after
    // `screen` exists, and again on every search/tile worker below.
    crate::tile::arm_pricing_cdfs(start_cdfs, screen);
    let mut luma = Plane {
        source: &picture_y8,
        reconstruction: vec![128; picture_y8.len()],
        width: picture.width,
        height: picture.height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: picture.width,
        tile_y1: picture.height,
        ctx: CoefCtxMap::default(),
    };
    let mut chroma = [
        Plane {
            source: &picture_u8,
            reconstruction: vec![128; picture_u8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
        Plane {
            source: &picture_v8,
            reconstruction: vec![128; picture_v8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
    ];

    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale() * step * step,
        modes: &KEY_FRAME_MODES,
        top_k: prune_top_k_inter(),
        screen,
    };
    let mode_bits_table = inter_mode_bits();
    // lane-av1tpl: one lambda factor per 64x64 superblock, from how much of
    // this frame the NEXT one predicts from. Off (`None`) without a
    // lookahead picture or at strength 0.
    let tpl_k = tpl_strength();
    let window: Vec<&Picture> = lookahead
        .iter()
        .take_while(|n| n.width == picture.width && n.height == picture.height)
        .collect();
    let tpl_sources: Vec<Vec<u8>> =
        window.iter().map(|n| n.y.iter().map(|&v| v as u8).collect()).collect();
    let tpl_cells_x = picture.width / 16;
    let tpl_factors: Option<Vec<f64>> = (tpl_k > 0.0 && !tpl_sources.is_empty()).then(|| {
        if tpl_propagating() {
            let mut frames: Vec<&[u8]> = vec![&picture_y8];
            frames.extend(tpl_sources.iter().map(Vec::as_slice));
            tpl_lambda_factors(&frames, picture.width, picture.height, tpl_k)
        } else {
            // The shipped map is one factor per 64x64; it is spread over that
            // superblock's own 16 cells so the application site below (which
            // averages the four cells of a 32x32 block) reads exactly the
            // superblock factor, whichever map fed it.
            let map =
                tpl_area_map(&picture_y8, &tpl_sources[0], picture.width, picture.height);
            let cells_y = picture.height / 16;
            let sb = tpl_sb_factors(&map, tpl_cells_x, cells_y, tpl_k);
            (0..tpl_cells_x * cells_y)
                .map(|c| sb[(c / tpl_cells_x / 4) * (tpl_cells_x / 4) + (c % tpl_cells_x) / 4])
                .collect()
        }
    });

    // The true grid ([`crate::tile::block_grid`]'s ceiling), not the padded
    // coding surface's: `blocks` carries one entry per 32x32 block the tile
    // writer will actually visit, same count it checks `blocks.len()`
    // against, which is fewer than the padded surface's own block count
    // whenever the true size is not a 64x64 multiple.
    let (cols, rows) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (cols, rows) = (cols as usize, rows as usize);
    let (sb_cols, _sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let (mi_cols, mi_rows) = (header.mi_cols as usize, header.mi_rows as usize);
    // This frame's tile grid, as in `encode_key_frame_inner`.
    let (tile_cols_log2, tile_rows_log2) = armed_tiles();
    let layout =
        crate::tile::TileLayout::new(header.mi_cols, header.mi_rows, tile_cols_log2, tile_rows_log2);
    header.tile_info = tile_info_of(&layout, 1);
    let header_tile_info = header.tile_info.clone();
    // lane-hbd r4: `motion::search` is 8-bit only (its SAD/cost math is
    // integer-`u8`-scale), so the DPB reference plane is narrowed for it --
    // once per frame (lane-av1rd2), not once per block, which is a whole
    // plane's map per 32x32 block and now per leaf too.
    let mut blocks = vec![Quadrant::Whole(BlockCoeffs::default()); cols * rows];
    // lane-av1tsearch: one search job per TILE (`search_tiles`). Neighbour
    // availability -- intra edges and the MV stack's scans alike -- already
    // stopped at the superblock's own tile (`PlaneBuf::set_tile_origin`,
    // `MiGrid::set_tile_bounds`), so a tile reads nothing another tile wrote;
    // giving each job its own planes and its own `MiGrid` therefore leaves
    // every decision, and so every coded bit, exactly where the whole-frame
    // loop left it, at any tile count and any thread count.
    let (frame_width, frame_height) = (picture.width, picture.height);
    let luma_out = crate::par::Shared::new(&mut luma.reconstruction);
    let [cb, cr] = &mut chroma;
    let chroma_out = [
        crate::par::Shared::new(&mut cb.reconstruction),
        crate::par::Shared::new(&mut cr.reconstruction),
    ];
    let search_tile = |index: usize,
                       fctx: &crate::decode::FrameCtx|
     -> Result<Vec<(usize, Quadrant)>> {
        // Thread-local, and this job may be running on a `search_tiles`
        // worker: `ref_distance`'s order-hint distance (the scaled motion
        // search step) is read by the SEARCH, so every worker arms it or the
        // tile's bits depend on the thread count.
        crate::tile::arm_order_hints(seq.order_hint_bits, order_hint, order_hints);
        crate::tile::arm_pricing_cdfs(start_cdfs, screen);
        let rect = layout.rect(index);
        let mut luma = fresh_plane(&picture_y8, frame_width, frame_height, true_width, true_height);
        let mut chroma = [
            fresh_plane(&picture_u8, frame_width / 2, frame_height / 2, true_width / 2, true_height / 2),
            fresh_plane(&picture_v8, frame_width / 2, frame_height / 2, true_width / 2, true_height / 2),
        ];
        clip_planes_to_tile(&mut luma, &mut chroma, rect);
        let mut grid = MiGrid::new(mi_cols, mi_rows);
        grid.set_sign_bias(sign_bias);
        // lane-av1obmc2: every picture an OBMC neighbour of a LEAF can name,
        // built once per tile job (`obmc_prediction`).
        let mut obmc_refs: [Option<&Picture>; 8] = [None; 8];
        obmc_refs[crate::mvstack::LAST_FRAME as usize] = Some(reference);
        obmc_refs[crate::mvstack::GOLDEN_FRAME as usize] = golden;
        obmc_refs[crate::mvstack::ALTREF_FRAME as usize] = altref;
        grid.set_tile_bounds(
            rect.mi_row0 as usize,
            rect.mi_col0 as usize,
            rect.mi_row1 as usize,
            rect.mi_col1 as usize,
        );
        let mut blocks: Vec<(usize, Quadrant)> = Vec::new();
    for sb_r in rect.sb_row0 as usize..rect.sb_row1 as usize {
        for sb_c in rect.sb_col0 as usize..rect.sb_col1 as usize {
            // lane-b64: the whole superblock as ONE 64x64 block, tried before
            // its quadrants are (`search_skip_64`) and compared against the
            // four of them below. Only a superblock wholly inside the true
            // frame is offered it: a 64x64 root whose right or bottom part
            // hangs over the edge is legal AV1 (`has_cols`/`has_rows` only
            // ask for half), but its trial would score prediction the frame
            // does not have -- corner-cut, ceiling = the edge superblocks of
            // a frame whose size is not a multiple of 64, which stay split.
            let (x64, y64) = (sb_c * SUPERBLOCK, sb_r * SUPERBLOCK);
            let sb64_legal = b64_root()
                && sb_r * 2 + 1 < rows
                && sb_c * 2 + 1 < cols
                && (sb_c as u32 + 1) * crate::tile::SB_MI <= header.mi_cols
                && (sb_r as u32 + 1) * crate::tile::SB_MI <= header.mi_rows
                && x64 + SUPERBLOCK <= luma.true_width
                && y64 + SUPERBLOCK <= luma.true_height;
            // One lambda for the whole superblock, the same mean the 32x32
            // level takes over its own four tpl cells: the 64-vs-four-32
            // comparison below weighs one cost against four, so both sides
            // have to be priced at the same lambda.
            let sb_search = match &tpl_factors {
                Some(f) if sb64_legal => {
                    let mean = (0..16)
                        .map(|i| f[(sb_r * 4 + i / 4) * tpl_cells_x + sb_c * 4 + i % 4])
                        .sum::<f64>()
                        / 16.0;
                    Search { lambda: search.lambda * mean, ..search }
                }
                _ => search,
            };
            let mark = blocks.len();
            let mut sb64: Option<(f64, BlockCoeffs, [(Vec<u8>, Vec<CoefCtx>); 3])> = None;
            let mut sb_cost = 0.0f64;
            if sb64_legal {
                let base = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                let (mi_row, mi_col) = (sb_r * 16, sb_c * 16);
                let stack =
                    find_mv_stack(&grid, mi_row, mi_col, 16, 16, LAST_FRAME, mi_cols, mi_rows);
                if let Some((cost, block)) = search_skip_64(
                    &mut luma,
                    &mut chroma,
                    (x64, y64),
                    &sb_search,
                    b64_residual() && !screen,
                    reference,
                    &stack,
                    fctx,
                ) {
                    let after = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                    restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &base);
                    let cost = cost
                        + sb_search.lambda * crate::tile::partition_bits(SUPERBLOCK, false);
                    sb64 = Some((cost, block, after));
                }
            }
            // A superblock whose 64x64 trial already codes almost nothing per
            // pixel is taken outright and its quadrants are never searched --
            // the same `split_rd_breakout` shape the 32x32 level uses one
            // size up, and where this lever BUYS wall instead of spending it.
            let early64 = sb64.as_ref().is_some_and(|(c, _, _)| {
                split_rd_breakout_at(
                    b64_breakout_threshold(),
                    *c,
                    SUPERBLOCK,
                    sb_search.lambda,
                )
            });
            for quadrant in 0..if early64 { 0 } else { 4 } {
                let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                // lane-av1tpl2: this 32x32 block's own lambda -- every RD
                // decision under it (partition, mode, tx, motion) reads
                // `search.lambda`, so scaling the one field here scales all
                // of them. 32x32 and not the 16x16 the map is built at
                // because the whole-vs-split comparison right below weighs
                // one 32x32 cost against four 16x16 ones: a per-leaf lambda
                // would make those two numbers incomparable.
                let search = match &tpl_factors {
                    Some(f) => {
                        let mean = (0..4)
                            .map(|i| f[(r32 * 2 + i / 2) * tpl_cells_x + c32 * 2 + i % 2])
                            .sum::<f64>()
                            / 4.0;
                        Search { lambda: search.lambda * mean, ..search }
                    }
                    None => search,
                };
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
                    let extra_stacks: Vec<(i8, &Picture, MvStack)> = [
                        (crate::mvstack::GOLDEN_FRAME, golden),
                        (crate::mvstack::ALTREF_FRAME, altref),
                    ]
                    .into_iter()
                    .filter_map(|(r, pic)| {
                        pic.map(|p| {
                            (
                                r,
                                p,
                                find_mv_stack(&grid, mi_row, mi_col, 8, 8, r, mi_cols, mi_rows),
                            )
                        })
                    })
                    .collect();
                    let extra: Vec<(i8, &Picture, &MvStack)> = extra_stacks
                        .iter()
                        .map(|(r, p, st)| (*r, *p, st))
                        .collect();
                    // The COMPOUND stack of each `LAST` + extra pair, off the
                    // same grid (and the same armed sign bias) the tile writer
                    // rebuilds it from. Empty unless this frame codes
                    // `reference_select`, which is what gates the syntax.
                    let compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> =
                        if header.reference_select {
                            extra_stacks
                                .iter()
                                .map(|&(r, p, _)| {
                                    (
                                        r,
                                        p,
                                        crate::mvstack::find_mv_stack_compound(
                                            &grid,
                                            mi_row,
                                            mi_col,
                                            8,
                                            8,
                                            (crate::mvstack::LAST_FRAME, r),
                                            mi_cols,
                                            mi_rows,
                                            grid.sign_bias_table(),
                                            &[(0, 0); 7],
                                            None,
                                        ),
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };

                    let base = snapshot(&luma, &chroma, (x, y), BLOCK);
                    let (block, mut cost_whole) = search_inter_block(
                        &mut luma,
                        &mut chroma,
                        (x, y),
                        &search,
                        &mode_bits_table,
                        reference,
                        &stack,
                        &extra,
                        &compound,
                        &grid,
                        (mi_row, mi_col),
                        (mi_rows, mi_cols),
                        fctx,
                    );
                    cost_whole += search.lambda * partition_bits(BLOCK, false);
                    let after_whole = snapshot(&luma, &chroma, (x, y), BLOCK);

                    // What four 16x16 leaves cost instead (spec
                    // PARTITION_SPLIT at BLOCK_32X32, which
                    // `sb_coeff_inter_frame_tile`'s `Quadrant::Split` arm
                    // already writes for a straddling quadrant). Each leaf is
                    // searched against the reconstruction -- and the `mi`
                    // grid -- the ones before it left, exactly as the writer
                    // and the decoder will read them.
                    //
                    // A block the whole-32 search left skipped (no residual
                    // at all) is not offered the split: libaom prunes the
                    // same way at cpu-used 6, and it is where the wall would
                    // otherwise go on static content.
                    let leaf_positions: Vec<(usize, usize)> = (0..4)
                        .map(|i| (r32 * 2 + i / 2, c32 * 2 + i % 2))
                        .collect();
                    let leaves_legal = leaf_positions.iter().all(|&(sr, sc)| {
                        crate::tile::has_half(
                            sc as u32 * crate::tile::SUB_MI,
                            crate::tile::SUB_MI,
                            header.mi_cols,
                        ) && crate::tile::has_half(
                            sr as u32 * crate::tile::SUB_MI,
                            crate::tile::SUB_MI,
                            header.mi_rows,
                        )
                    });
                    // The breakout above: a 32x32 block that codes almost
                    // nothing is not offered the split at all.
                    let breakout = split_breakout_coeffs();
                    let split = (!block.skip
                        && !split_rd_breakout(cost_whole, BLOCK, search.lambda)
                        && (breakout == 0 || block.luma.len() > 4 * breakout)
                        && leaves_legal
                        && split_inter_blocks())
                        .then(|| {
                            restore(&mut luma, &mut chroma, (x, y), BLOCK, &base);
                            let mut cost = search.lambda * partition_bits(BLOCK, true);
                            let mut leaves = Vec::with_capacity(4);
                            for &(sr, sc) in &leaf_positions {
                                let (mi_row, mi_col) = (sr * 4, sc * 4);
                                let (lx, ly) = (sc * SUB, sr * SUB);
                                let leaf_base = snapshot(&luma, &chroma, (lx, ly), SUB);
                                let stack = find_mv_stack(
                                    &grid, mi_row, mi_col, 4, 4, LAST_FRAME, mi_cols, mi_rows,
                                );
                                let cstacks = leaf_compound_stacks(
                                    &grid, mi_row, mi_col, 4, 4, mi_cols, mi_rows,
                                    header.reference_select, golden, altref,
                                );
                                let (leaf, leaf_cost) = code_square_inter(
                                    &mut luma,
                                    &mut chroma,
                                    (lx, ly),
                                    SUB,
                                    &search,
                                    &mode_bits_table,
                                    reference,
                                    &stack,
                                    &cstacks,
                                    &grid,
                                    (mi_row, mi_col),
                                    (mi_rows, mi_cols),
                                    &obmc_refs,
                                    fctx,
                                );
                                let leaf_cost = leaf_cost
                                    + search.lambda * partition_bits(SUB, false);
                                let after_leaf = snapshot(&luma, &chroma, (lx, ly), SUB);

                                // And what four 8x8 leaves cost instead --
                                // the same tree one step further down, which
                                // the writer codes as a real PARTITION_SPLIT
                                // at BLOCK_16X16 (lane-av1rd2). Same
                                // skipped-block prune as the level above.
                                let eight = (!leaf.skip
                                    && !split_rd_breakout(leaf_cost, SUB, search.lambda)
                                    && (breakout == 0 || leaf.luma.len() > breakout)
                                    && split_inter_8())
                                    .then(|| {
                                        restore(
                                            &mut luma, &mut chroma, (lx, ly), SUB, &leaf_base,
                                        );
                                        let mut cost8 = search.lambda
                                            * (partition_bits(SUB, true)
                                                + 4.0 * partition_bits(8, false));
                                        let mut eight = Vec::with_capacity(4);
                                        for i in 0..4 {
                                            let (mr, mc) =
                                                (mi_row + (i / 2) * 2, mi_col + (i % 2) * 2);
                                            let stack8 = find_mv_stack(
                                                &grid, mr, mc, 2, 2, LAST_FRAME, mi_cols, mi_rows,
                                            );
                                            let cstacks8: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> =
                                                if header.reference_select && leaf_compound() {
                                                    [
                                                        (crate::mvstack::GOLDEN_FRAME, golden),
                                                        (crate::mvstack::ALTREF_FRAME, altref),
                                                    ]
                                                    .into_iter()
                                                    .filter_map(|(r, pic)| {
                                                        pic.map(|p| {
                                                            (
                                                                r,
                                                                p,
                                                                crate::mvstack::find_mv_stack_compound(
                                                                    &grid, mr, mc, 2, 2,
                                                                    (crate::mvstack::LAST_FRAME, r), mi_cols, mi_rows,
                                                                    grid.sign_bias_table(), &[(0, 0); 7], None,
                                                                ),
                                                            )
                                                        })
                                                    })
                                                    .collect()
                                                } else {
                                                    Vec::new()
                                                };
                                            let (leaf8, cost) = code_square_inter(
                                                &mut luma,
                                                &mut chroma,
                                                (lx + (i % 2) * 8, ly + (i / 2) * 8),
                                                8,
                                                &search,
                                                &mode_bits_table,
                                                reference,
                                                &stack8,
                                                &cstacks8,
                                                &grid,
                                                (mr, mc),
                                                (mi_rows, mi_cols),
                                                &obmc_refs,
                                                fctx,
                                            );
                                            cost8 += cost;
                                            record_mi(&mut grid, mr, mc, 2, leaf8.inter);
                                            eight.push(leaf8);
                                        }
                                        (eight, cost8)
                                    });
                                match eight {
                                    Some((eight, cost8)) if cost8 < leaf_cost => {
                                        partition_hit(3);
                                        cost += cost8;
                                        leaves.push(BlockCoeffs {
                                            eight: Some(eight),
                                            ..BlockCoeffs::default()
                                        });
                                        continue;
                                    }
                                    Some(_) => restore(
                                        &mut luma, &mut chroma, (lx, ly), SUB, &after_leaf,
                                    ),
                                    None => {}
                                }
                                partition_hit(2);
                                cost += leaf_cost;
                                record_mi(&mut grid, mi_row, mi_col, 4, leaf.inter);
                                leaves.push(leaf);
                            }
                            (leaves, cost)
                        });
                    if let Some((leaves, cost)) = split {
                        if cost < cost_whole {
                            // The leaves' own `mi` cells are already in the
                            // grid, and their reconstruction is what the
                            // planes hold.
                            partition_hit(1);
                            sb_cost += cost;
                            blocks.push((r32 * cols + c32, Quadrant::Split(leaves)));
                            continue;
                        }
                        restore(&mut luma, &mut chroma, (x, y), BLOCK, &after_whole);
                    }
                    partition_hit(0);
                    sb_cost += cost_whole;
                    record_mi(&mut grid, mi_row, mi_col, 8, block.inter);
                    blocks.push((r32 * cols + c32, Quadrant::Whole(block)));
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
                        if has_cols16 && has_rows16 {
                            let (x, y) = (sc * SUB, sr * SUB);
                            let (mi_row, mi_col) = (sr * 4, sc * 4);
                            let stack = find_mv_stack(
                                &grid, mi_row, mi_col, 4, 4, LAST_FRAME, mi_cols, mi_rows,
                            );
                            let cstacks = leaf_compound_stacks(
                                &grid, mi_row, mi_col, 4, 4, mi_cols, mi_rows,
                                header.reference_select, golden, altref,
                            );
                            let (block, _) = code_square_inter(
                                &mut luma,
                                &mut chroma,
                                (x, y),
                                SUB,
                                &search,
                                &mode_bits_table,
                                reference,
                                &stack,
                                &cstacks,
                                &grid,
                                (mi_row, mi_col),
                                (mi_rows, mi_cols),
                                &obmc_refs,
                                fctx,
                            );
                            // One publication point for every coded leaf
                            // (`record_mi`): this site used to spell the vote
                            // out with `ref_frame` pinned to LAST and `mv1`
                            // to (0,0), which is the encoder-grid-drift class
                            // -- correct only for as long as a leaf can never
                            // be compound.
                            record_mi(&mut grid, mi_row, mi_col, 4, block.inter);
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
                                let cstacks = leaf_compound_stacks(
                                    &grid, mi_row, mi_col, 2, 2, mi_cols, mi_rows,
                                    header.reference_select, golden, altref,
                                );
                                let (leaf, _) = code_square_inter(
                                    &mut luma,
                                    &mut chroma,
                                    (leaf_x, leaf_y),
                                    8,
                                    &search,
                                    &mode_bits_table,
                                    reference,
                                    &stack,
                                    &cstacks,
                                    &grid,
                                    (mi_row, mi_col),
                                    (mi_rows, mi_cols),
                                    &obmc_refs,
                                    fctx,
                                );
                                // Same publication point as the 16x16
                                // straddling leaf above (`record_mi`).
                                record_mi(&mut grid, mi_row, mi_col, 2, leaf.inter);
                                eight.push(leaf);
                            }
                            leaves.push(BlockCoeffs {
                                eight: Some(eight),
                                ..BlockCoeffs::default()
                            });
                        }
                    }
                    blocks.push((r32 * cols + c32, Quadrant::Split(leaves)));
                }
            }
            // The 64x64 root against what its four quadrants really cost.
            if let Some((cost64, block, after)) = sb64
                && (early64
                    || cost64
                        < sb_cost
                            + sb_search.lambda * crate::tile::partition_bits(SUPERBLOCK, true))
            {
                B64_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                blocks.truncate(mark);
                restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &after);
                // Overwrites every 16x16 mi cell the quadrant searches
                // published under this superblock, so the next block's stack
                // is built off the block that was really coded.
                record_mi(&mut grid, sb_r * 16, sb_c * 16, 16, block.inter);
                blocks.push(((sb_r * 2) * cols + sb_c * 2, Quadrant::Whole64(block)));
                for q in 1..4 {
                    let (r32, c32) = (sb_r * 2 + q / 2, sb_c * 2 + q % 2);
                    blocks.push((r32 * cols + c32, Quadrant::Covered));
                }
            }
        }
    }
        let (x0, y0) = (rect.mi_col0 as usize * 4, rect.mi_row0 as usize * 4);
        let (x1, y1) = (rect.mi_col1 as usize * 4, rect.mi_row1 as usize * 4);
        copy_rect(luma_out, &luma.reconstruction, frame_width, x0, y0, x1, y1);
        for (&out, tile) in chroma_out.iter().zip(&chroma) {
            copy_rect(
                out,
                &tile.reconstruction,
                frame_width / 2,
                x0 / 2,
                y0 / 2,
                x1.div_ceil(2),
                y1.div_ceil(2),
            );
        }
        Ok(blocks)
    };
    for (at, quadrant) in search_tiles(layout.count(), fctx, search_tile)?
        .into_iter()
        .flatten()
    {
        blocks[at] = quadrant;
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
    for block in blocks.iter().flat_map(Quadrant::blocks) {
        if let Some(info) = block.inter {
            let r = (info.ref_frame as usize).min(7);
            REF_FRAME_HITS[r].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(test)]
    record_predicted_bits(crate::tile::predicted_coeff_bits(&blocks, base_q_idx));
    // This frame's header leaves `disable_frame_end_update_cdf` off, so it
    // stores its end-of-tile tables with the counts reset (spec 7.20) and the
    // next frame's writer starts from them -- exactly what
    // `crate::stream::stored_cdfs_for` hands the decoder.
    let (mi_cols, mi_rows) = (header.mi_cols, header.mi_rows);
    let cdfs = start_cdfs
        .cloned()
        .unwrap_or_else(|| crate::cdf_state::Cdfs::new(crate::tile::q_ctx_of(base_q_idx)));
    let start_cdfs = CdfSnapshot(cdfs);
    let reference_select_bit = header.reference_select;
    let switchable_motion_mode = header.is_motion_mode_switchable;
    let warped_motion = header.allow_warped_motion;
    let order_hint_bits = seq.order_hint_bits;
    let lr_horz = crate::restoration::count_units(header.frame_width, 64) as u32;
    let lr_vert = crate::restoration::count_units(header.frame_height, 64) as u32;
    // What this frame stores is the LARGEST tile's end-of-tile tables --
    // its own header's `context_update_tile_id` (spec 7.20), which
    // `tile_info_of` set to exactly that tile.
    let store_tile = layout.largest_tile();
    let end_cdfs = std::cell::RefCell::new(None);
    // As in `encode_key_frame_inner`: every tile starts from this frame's
    // own tables, so no tile depends on any other.
    let code_tiles = |bits: u8,
                      sb_cols: usize,
                      cdef_grid: &[u8],
                      units: &[Option<crate::restoration::WienerInfo>]|
     -> Result<Vec<Vec<u8>>> {
        let (out, stored) = write_tiles(layout.count(), store_tile, |index| {
            crate::tile::arm_cdef_idx(bits, sb_cols, cdef_grid.to_vec());
            crate::tile::arm_lr(64, lr_horz, lr_vert, units.to_vec());
            crate::tile::arm_sign_bias(sign_bias);
            // Thread-locals: every tile job arms its own worker.
            crate::tile::arm_reference_select(reference_select_bit);
            crate::tile::arm_motion_mode(switchable_motion_mode);
            crate::tile::arm_warped_motion(warped_motion);
            crate::tile::arm_order_hints(order_hint_bits, order_hint, order_hints);
            crate::tile::arm_screen(screen);
            crate::tile::arm_filter_intra(filter_intra_on());
            let mut cdfs = start_cdfs.0.clone();
            crate::tile::arm_pricing_cdfs(Some(&cdfs), screen);
            // Each tile's own MV grid: a candidate scan never reaches past
            // the tile's bounds, so a grid holding only this tile's own
            // blocks reads exactly as the decoder's frame-wide one does
            // under `MiGrid::set_tile_bounds`.
            let mut mv_grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
            mv_grid.set_sign_bias(sign_bias);
            let bytes = crate::tile::sb_coeff_inter_frame_tile_cdfs(
                mi_cols,
                mi_rows,
                base_q_idx,
                &blocks,
                tx_select,
                &mut cdfs,
                layout.rect(index),
                &mut mv_grid,
            )?;
            Ok((bytes, Some(cdfs)))
        })?;
        if let Some(cdfs) = stored {
            *end_cdfs.borrow_mut() = Some(cdfs);
        }
        Ok(out)
    };
    let mut tiles = code_tiles(0, sb_cols, &[], &[])?;
    // Deblocking, as in `encode_key_frame_inner`: the decoder reconstructs
    // this tile against the same single-slot reference this encoder predicted
    // from, under each candidate level.
    // The reference a DECODER holds is this frame's cropped size, not the
    // encoder's padded coding surface -- `decode_inter_frame_tile` refuses a
    // reference whose height is not the header's own `frame_height` (spec
    // 7.9's compatible-size rule), and the chained-decode round trips prove
    // the two predict identically.
    let (fw, fh) = (header.frame_width as usize, header.frame_height as usize);
    let trial_ref = if (reference.width, reference.height) == (fw, fh) {
        reference.clone()
    } else {
        Picture {
            width: fw,
            height: fh,
            y: crop_plane(&reference.y, reference.width, fw, fh),
            u: crop_plane(&reference.u, reference.width / 2, fw / 2, fh / 2),
            v: crop_plane(&reference.v, reference.width / 2, fw / 2, fh / 2),
        }
    };
    // `RefPix` is indexed by the reference NAME (`LAST_FRAME` = 1,
    // `GOLDEN_FRAME` = 4), not by DPB slot: the trial decode of a frame whose
    // blocks may name GOLDEN needs that plane too, cropped the same way.
    let trial_golden = golden.map(|g| {
        if (g.width, g.height) == (fw, fh) {
            g.clone()
        } else {
            Picture {
                width: fw,
                height: fh,
                y: crop_plane(&g.y, g.width, fw, fh),
                u: crop_plane(&g.u, g.width / 2, fw / 2, fh / 2),
                v: crop_plane(&g.v, g.width / 2, fw / 2, fh / 2),
            }
        }
    });
    let trial_altref = altref.map(|a| {
        if (a.width, a.height) == (fw, fh) {
            a.clone()
        } else {
            Picture {
                width: fw,
                height: fh,
                y: crop_plane(&a.y, a.width, fw, fh),
                u: crop_plane(&a.u, a.width / 2, fw / 2, fh / 2),
                v: crop_plane(&a.v, a.width / 2, fw / 2, fh / 2),
            }
        }
    });
    let mut refs: [Option<&Picture>; 8] = [None; 8];
    refs[1] = Some(&trial_ref);
    refs[4] = trial_golden.as_ref();
    refs[7] = trial_altref.as_ref();
    let refpix = crate::decode::RefPix::ready(refs);
    pick_and_apply_filters(
        &mut header,
        &mut luma,
        &mut chroma,
        [&picture_y8, &picture_u8, &picture_v8],
        // Merge of lane-av1filt with lane-av1tx2: the trial decoder takes
        // this frame's own `tx_select`, so the search runs under inter
        // `TxMode::Select` too (it used to skip and leave every inter frame
        // unfiltered once Select became the default).
        true,
        &mut tiles,
        search.lambda,
        fctx,
        // As in `encode_key_frame_inner`, but this frame's end-of-tile tables
        // are what the NEXT frame writes from, so the winning re-code
        // publishes its own into `end_cdfs` -- the first write's are stale
        // the moment a second tile is coded.
        |bits, sb_cols, grid, units| code_tiles(bits, sb_cols, grid, units),
        |lf, cdef, h, tiles: &[Vec<u8>], lr| {
            let payloads: Vec<&[u8]> = tiles.iter().map(Vec::as_slice).collect();
            crate::decode::decode_inter_frame_tiles_lr(
                &payloads,
                &header_tile_info,
                h.mi_cols,
                h.mi_rows,
                base_q_idx,
                h.frame_width,
                h.frame_height,
                &refpix,
                cdef,
                lf,
                h.allow_high_precision_mv,
                h.force_integer_mv,
                Some(mc::InterpFilterKind::Regular),
                seq.enable_dual_filter,
                h.reference_select,
                tx_select,
                lr,
                // The trial decode re-reads the tile the writer just wrote,
                // so it starts from the same tables the writer did (this
                // frame's `start_cdfs`), not from the defaults.
                Some(start_cdfs.0.clone()),
                sign_bias,
                h.allow_screen_content_tools,
                switchable_motion_mode,
                h.allow_warped_motion,
                seq.enable_filter_intra,
                fctx,
            )
        },
    )?;
    let mut cdfs = end_cdfs
        .into_inner()
        .unwrap_or_else(|| start_cdfs.0.clone());
    cdfs.reset_counts();
    let next_cdfs = CdfSnapshot(cdfs);
    let tile_size_bytes = crate::frame::tile_size_bytes_for(&tiles);
    header.tile_info.tile_size_bytes = tile_size_bytes;
    let tile = crate::frame::tile_group_payload(&tiles, tile_size_bytes);
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
        tx_select,
        switchable_motion_mode,
        screen,
        start_cdfs,
        next_cdfs,
        loop_filter: header.loop_filter,
        cdef: header.cdef,
        loop_restoration: header.loop_restoration,
        // An inter frame never allows intrabc (spec 5.9.2 codes the bit on
        // intra frames only).
        allow_intrabc: false,
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
    /// One entry per picture, in DISPLAY order — the order a decoder outputs
    /// them and the order `pictures` came in. Without a pyramid that is also
    /// the coding order; with one the two differ, and `coding_order` is the
    /// map between them.
    pub frames: Vec<Encoded>,
    /// The index into `frames` of each coded frame, in CODING order — what a
    /// per-coded-frame census (`take_predicted_bits`) must be zipped against.
    /// `0..frames.len()` on the flat path.
    pub coding_order: Vec<usize>,
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
pub(crate) fn encode_sequence_with_ctx(
    pictures: &[Picture],
    base_q_idx: u8,
    deadzone: f64, fctx: &crate::decode::FrameCtx,
) -> Result<EncodedSequence> {
    let Some((first, rest)) = pictures.split_first() else {
        return Err(Error::unsupported(
            "AV1 encode",
            "a sequence needs at least one picture",
        ));
    };
    first.check_even()?;
    let render = (first.width, first.height);
    // THE CODING PYRAMID (lane-av1pyrdef): a sequence of camera material is
    // coded as mini-GOPs -- a hidden ALTREF ahead of its display position,
    // the leaves that predict backward off it, and a `show_existing_frame`
    // header that re-outputs it -- unless `EC_AV1_PYRAMID` turns it off for
    // an A/B. Screen content stays flat (the content gate in
    // `Av1Encoder::encode_frames`, whose detector this asks first so the
    // whole sequence path is decided in one place), and so does a run that
    // asks for a quantizer rounding offset the facade does not carry: the
    // facade codes at `encoder::DEADZONE` (0.5), so any other deadzone --
    // the ablations' -- must keep the flat path rather than be silently
    // ignored.
    //
    // The reordering itself is NOT reimplemented here: it is the facade's
    // own driver ([`crate::encoder::Av1Encoder::encode_sequence_pyramid`]),
    // called with this sequence's shape, so the two paths cannot drift.
    let pyramid = crate::encoder::Pyramid::from_env()
        .filter(|_| !rest.is_empty() && (deadzone - 0.5).abs() < f64::EPSILON)
        .filter(|_| !picture_is_screen(first));
    LAST_SEQ_PYRAMID.with(|c| c.set(pyramid));
    if let Some(pyramid) = pyramid {
        for picture in rest {
            picture.check_even()?;
            if (picture.width, picture.height) != render {
                return Err(Error::unsupported(
                    "AV1 encode",
                    "every picture in a sequence must be the same size as the first",
                ));
            }
        }
        return crate::encoder::Av1Encoder::encode_sequence_pyramid(
            pictures,
            base_q_idx,
            pyramid,
            armed_tiles(),
        );
    }
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
        unspecified_color_config(), fctx,
    )?;
    let mut stream = key.stream.clone();
    // Slot 1 keeps the key frame's own reconstruction for the whole GOP (the
    // key frame refreshes every slot, each inter frame only slot 0), which is
    // what `GOLDEN_FRAME` names.
    let golden = key.reconstruction.clone();
    // The frame two back, once there is one that is not the key frame
    // itself (which `GOLDEN_FRAME` already names).
    let mut prev2: Option<Picture> = None;
    let mut carried = key.next_cdfs.clone();
    let mut reference = key.reconstruction.clone();
    let mut frames = vec![crop_encoded(&key, render.0, render.1)];
    for (i, picture) in rest.iter().enumerate() {
        picture.check_even()?;
        // lane-av1tpl2: the lookahead window the per-block lambda weighting
        // propagates back through -- the next [`tpl_depth`] - 1 pictures'
        // sources at this frame's coded (padded) size. The tail of the
        // sequence gets a shorter window, and the last frame none at all.
        let lookahead: Vec<Picture> = rest[(i + 1).min(rest.len())..]
            .iter()
            .take(tpl_depth() - 1)
            .map(|n| n.padded_to(SUPERBLOCK))
            .collect();
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
            flat_order_hints(order_hint, 0, prev2.is_some()),
            render,
            Some(&carried.0),
            Some(&golden),
            prev2.as_ref(), fctx,
            None,
            &lookahead,
        )?;
        stream.extend_from_slice(&inter.stream);
        carried = inter.next_cdfs.clone();
        prev2 = Some(std::mem::replace(
            &mut reference,
            inter.reconstruction.clone(),
        ));
        frames.push(crop_encoded(&inter, render.0, render.1));
    }
    Ok(EncodedSequence {
        stream,
        coding_order: (0..frames.len()).collect(),
        frames,
    })
}

#[cfg(test)]
mod tests {

    /// lane-av1tpl2's propagated map, on the two properties every decision
    /// under it rests on: a frame the whole lookahead window repeats exactly
    /// (uniform texture) leans on every cell alike, so every factor is 1 and
    /// the weighting is a no-op; and where the window really does save bits
    /// by predicting -- the textured half of a frame whose other half is
    /// flat -- the factor FALLS below 1, i.e. that half is coded more
    /// distortion-averse than the half nothing gains from.
    #[test]
    fn tpl_factors_are_flat_on_a_uniform_repeat_and_fall_where_the_window_leans() {
        // Four 64x64 superblocks across: the shipped map forms its ratio per
        // SUPERBLOCK (`EC_AV1_TPL_SB`), so a frame one superblock wide could
        // only ever produce one ratio -- and one ratio, mean-normalised, is
        // the flat map, whatever the content underneath it.
        let (w, h) = (256usize, 64usize);
        let uniform: Vec<u8> = (0..w * h).map(|i| ((i * 37) % 251) as u8).collect();
        let frames: Vec<&[u8]> = vec![&uniform, &uniform, &uniform];
        let f = super::tpl_lambda_factors(&frames, w, h, 1.0);
        assert_eq!(f.len(), 64, "16x4 cells of 16x16");
        for (i, &v) in f.iter().enumerate() {
            assert!((v - 1.0).abs() < 1e-6, "cell {i} factor {v}, a flat map must move no lambda");
        }

        // Left half textured, right half constant: the window saves bits
        // predicting the left half and nothing at all on the right.
        let split: Vec<u8> = (0..w * h)
            .map(|i| if i % w < w / 2 { ((i * 37) % 251) as u8 } else { 128 })
            .collect();
        let frames: Vec<&[u8]> = vec![&split, &split, &split];
        let cells_x = w / 16;
        for k in [0.25f64, 0.5, 1.0, 2.0] {
            let f = super::tpl_lambda_factors(&frames, w, h, k);
            for r in 0..h / 16 {
                let (lean, idle) = (f[r * cells_x], f[r * cells_x + cells_x - 1]);
                assert!(lean < 1.0, "k={k} row {r}: leaned-on cell factor {lean} must be < 1");
                assert!(idle > 1.0, "k={k} row {r}: idle cell factor {idle} must be > 1");
            }
        }
    }

    /// lane-rectx r5: libaom lays `has_tr_*`/`has_bl_*` out row-major for a
    /// 128x128 superblock -- `MAX_MIB_SIZE_LOG2 = 5` mi columns per row, so a
    /// row holds `32 >> bw_in_mi_log2` bits and the table holds
    /// `32 >> bh_in_mi_log2` such rows. Walking every block position of that
    /// superblock must therefore hit every bit of the table EXACTLY ONCE.
    /// r4's `4 - bw_log2` index halved the row stride: the walk then covered
    /// only a quarter of the 16x8 table and read the wrong byte for every
    /// block row but the first (silently wrong above-right/below-left
    /// availability -- wrong pixels, no error).
    #[test]
    fn rect_reach_tables_are_indexed_with_a_32_mi_row_stride() {
        for (bw, bh) in [(16usize, 8usize), (8, 16), (32, 16), (16, 32), (64, 32), (32, 64)] {
            let (bw_log2, bh_log2) =
                ((bw / 4).trailing_zeros() as usize, (bh / 4).trailing_zeros() as usize);
            let (rows, cols) = (32 >> bh_log2, 32 >> bw_log2);
            for (name, table) in
                [("has_tr", rect_reach_tables(bw, bh).0), ("has_bl", rect_reach_tables(bw, bh).1)]
            {
                assert_eq!(
                    table.len() * 8,
                    rows * cols,
                    "{name}_{bw}x{bh}: libaom's table is {rows} rows of {cols} bits"
                );
                let mut seen = vec![0u32; table.len() * 8];
                for blk_row in 0..rows {
                    for blk_col in 0..cols {
                        let idx = Reach::rect_table_index(bw_log2, blk_row, blk_col);
                        assert!(
                            idx < seen.len(),
                            "{name}_{bw}x{bh}: index {idx} past the table at ({blk_row},{blk_col})"
                        );
                        seen[idx] += 1;
                    }
                }
                assert!(
                    seen.iter().all(|&n| n == 1),
                    "{name}_{bw}x{bh}: the block walk is not a bijection onto the table's bits \
                     (row stride is not {cols})"
                );
            }
        }
    }
    use super::*;
    use crate::intra::{D45_PRED, D135_PRED, H_PRED, KEY_FRAME_MODES, NON_DIRECTIONAL, V_PRED};
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Every rectangular shape this decoder codes, at every position a block
    /// of that shape can sit inside a 64x64 superblock: [`Reach::of_rect`]
    /// must answer exactly what libaom's `has_top_right`/`has_bottom_left`
    /// (`reconintra.c`) answer, whose bodies are ported below.
    ///
    /// lane-tx4x8 r3 ([[enumerate-table-domain]], [[tool-disabled-in-every-gate]]):
    /// `rect_reach_tables` used to route every shape but 4x8/8x4/16x32 to the
    /// 32x16 row, so live 16x8 strips read the wrong bit at 10 (above-right)
    /// and 7 (below-left) of the 21 reachable positions with no gate seeing
    /// it. The per-shape `(len, byte sum)` fingerprints below come from the
    /// oracle's own arrays, so a re-routed or truncated table fails here even
    /// where the two rows happen to agree.
    ///
    /// Not covered (no parameter for either on `of_rect`): chroma `ss_x`/
    /// `ss_y`, and sub-block transforms (`row_off`/`col_off` > 0), which the
    /// callers handle by predicting each transform unit as its own block.
    /// `PARTITION_VERT_A`/`VERT_B` need no arm either: libaom's
    /// `has_tr_vert_tables`/`has_bl_vert_tables` entry for every RECT bsize is
    /// either the plain table itself (4x8, 8x16, 16x32, 32x64) or NULL
    /// (the horizontal shapes, which those partitions never produce).
    #[test]
    fn every_rect_shape_reaches_what_libaom_says_over_the_whole_superblock() {
        let fctx = &crate::decode::FrameCtx::new();
        // (bw, bh, has_tr len, has_tr byte sum, has_bl len, has_bl byte sum).
        const SHAPES: [(usize, usize, usize, u32, usize, u32); 14] = [
        (4, 8, 64, 9280, 64, 550),
        (8, 4, 64, 3712, 64, 9630),
        (8, 16, 16, 2352, 16, 134),
        (16, 8, 16, 960, 16, 2400),
        (16, 32, 4, 620, 4, 32),
        (32, 16, 4, 32, 4, 184),
        (32, 64, 1, 127, 1, 0),
        (64, 32, 1, 19, 1, 34),
        (4, 16, 32, 5472, 32, 14),
        (16, 4, 32, 960, 32, 6464),
        (8, 32, 8, 1400, 8, 2),
        (32, 8, 8, 32, 8, 1136),
        (16, 64, 2, 382, 2, 0),
        (64, 16, 2, 4, 2, 84),
        ];
        /// libaom `reconintra.c` `has_top_right`/`has_bottom_left`, ported for
        /// a block whose transform covers it whole (`row_off == col_off == 0`,
        /// `tx_size == bsize`) and luma (`ss_x == ss_y == 0`) on a 64-pixel
        /// superblock (`sb_mi_size == 16`).
        fn libaom_reach(
            bw: usize,
            bh: usize,
            x: usize,
            y: usize,
            width: usize,
            height: usize, _fctx: &crate::decode::FrameCtx,
        ) -> (bool, bool) {
            let (bw_log2, bh_log2) = ((bw / 4).ilog2() as usize, (bh / 4).ilog2() as usize);
            let (_bw_unit, bh_unit) = (bw / 4, bh / 4);
            let sb_mi_size = 16usize;
            let blk_row_in_sb = ((y / 4) & (sb_mi_size - 1)) >> bh_log2;
            let blk_col_in_sb = ((x / 4) & (sb_mi_size - 1)) >> bw_log2;
            let this_blk_index = (blk_row_in_sb << (5 - bw_log2)) + blk_col_in_sb;
            let (tr_table, bl_table) = rect_reach_tables(bw, bh);
            let bit = |t: &[u8]| (t[this_blk_index / 8] >> (this_blk_index % 8)) & 1 != 0;
            // has_top_right: top_available && right_available, then row_off == 0.
            let top_right = if y == 0 || x + bw >= width {
                false
            } else if blk_row_in_sb == 0 {
                // (`col_off + tx_wide_unit < plane_bw_unit` above this is
                // `bw_unit < bw_unit`, false for a whole-block transform.)
                true
            } else if ((blk_col_in_sb + 1) << bw_log2) >= sb_mi_size {
                false
            } else {
                bit(tr_table)
            };
            // has_bottom_left: bottom_available && left_available, col_off == 0.
            let bottom_left = if x == 0 || y + bh >= height {
                false
            } else if blk_col_in_sb == 0 {
                // (`row_off + tx_high_unit < plane_bh_unit` is likewise false.)
                (blk_row_in_sb << bh_log2) + bh_unit < sb_mi_size
            } else if ((blk_row_in_sb + 1) << bh_log2) >= sb_mi_size {
                false
            } else {
                bit(bl_table)
            };
            (top_right, bottom_left)
        }
        for (bw, bh, tr_len, tr_sum, bl_len, bl_sum) in SHAPES {
            let (tr, bl) = rect_reach_tables(bw, bh);
            let sum = |t: &[u8]| t.iter().map(|&b| u32::from(b)).sum::<u32>();
            assert_eq!(
                (tr.len(), sum(tr), bl.len(), sum(bl)),
                (tr_len, tr_sum, bl_len, bl_sum),
                "rect_reach_tables({bw}, {bh}) is not libaom's has_tr_{bw}x{bh}/has_bl_{bw}x{bh}"
            );
            // Two superblocks across and down, so the superblock-relative
            // position (and not the frame position) is what decides.
            for sb_y in [0usize, 64] {
                for sb_x in [0usize, 64] {
                    for y in (sb_y..sb_y + 64).step_by(bh) {
                        for x in (sb_x..sb_x + 64).step_by(bw) {
                            let got = Reach::of_rect(bw, bh, x, y, 256, 256, fctx);
                            let want = libaom_reach(bw, bh, x, y, 256, 256, fctx);
                            assert_eq!(
                                (got.above_right, got.below_left),
                                want,
                                "{bw}x{bh} at ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Whether ffmpeg is on PATH. Absence normally SKIPs, but
    /// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
    /// aomenc gate decodes its stream through ffmpeg and is meaningless
    /// without it -- turns it into a hard failure. Without this the require
    /// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
    /// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
    /// printed SKIP and reported green (class gate-skips-on-its-own-failure).
    fn have_ffmpeg() -> bool {
        let present = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            present
                || (std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_none()
                    && std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none()),
            "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
        );
        present
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
        // lane-t900 r10: the stream goes down stdin on ITS OWN THREAD. Writing
        // it inline deadlocks the moment ffmpeg's stdout pipe buffer (64 KiB)
        // fills before the last input byte is written -- which is exactly what
        // a 1.1 MB fixture decoding to 150 MB of raw 10-bit frames does
        // (measured: 45 min, both processes at 0% CPU). A write error here is
        // swallowed on purpose: ffmpeg's own exit status and stderr, asserted
        // below, are the real diagnosis.
        let mut stdin = child.stdin.take().expect("ffmpeg stdin");
        let payload = stream.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        });
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        writer.join().expect("ffmpeg stdin writer thread");
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
    /// The in-loop filters actually FIRE and the stream still decodes to
    /// exactly what the encoder kept as its reconstruction
    /// (gate-blind-to-feature: a filter search that always answered "off"
    /// would leave every other test just as green, and the BD gate that
    /// would notice is `#[ignore]`d).
    ///
    /// Run:
    ///     cargo test -p ec-av1 --release --lib -- \
    ///         encode::tests::the_filter_search_picks_real_filters
    #[test]
    fn the_filter_search_picks_real_filters() {
        let fctx = &crate::decode::FrameCtx::new();
        let pictures: Vec<_> = (0..3).map(|i| panned_test_card(128, 128, i * 3)).collect();
        let encoded = encode_sequence_with_ctx(&pictures, 120, 0.5, fctx).unwrap();
        let fired = encoded
            .frames
            .iter()
            .filter(|f| f.loop_filter.level[0] > 0 || f.cdef.y_pri_strength[0] > 0)
            .count();
        assert!(
            fired > 0,
            "no frame of this sequence chose any in-loop filtering: {:?}",
            encoded
                .frames
                .iter()
                .map(|f| (f.loop_filter.level, f.cdef.y_pri_strength[0]))
                .collect::<Vec<_>>()
        );
        // The reconstruction the encoder predicts from is the FILTERED one,
        // and it is what a decoder of the stream produces -- byte for byte,
        // frame for frame.
        let decoded = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), encoded.frames.len());
        for (i, (dec, enc)) in decoded.iter().zip(&encoded.frames).enumerate() {
            assert_eq!(dec.y, enc.reconstruction.y, "frame {i} luma");
            assert_eq!(dec.u, enc.reconstruction.u, "frame {i} U");
            assert_eq!(dec.v, enc.reconstruction.v, "frame {i} V");
        }
    }

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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_decodes_exactly_what_the_encoder_reconstructed: no ffmpeg");
            return;
        }
        for &(width, height) in &[(64usize, 64usize), (96, 64), (160, 96), (32, 48)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
        let _ = take_tx_depth_hits();
        for &q in &[15u8, 45, 100, 200] {
            let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(decoded.y, encoded.reconstruction.y, "q={q}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "q={q}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "q={q}: V");
        }
        // lane-av1tx: the exactness above is only a claim about SPLIT
        // transforms if some block actually split one (`gate-blind-to-feature`
        // -- with `tx_depth` stuck at 0 every assert here passes on the
        // pre-lane stream). The test card's edges split at these four
        // quantizers; the counter is per-process (another test encoding in
        // parallel can only ADD to it, so this can be masked, never falsely
        // failed), so it is read once for the whole q ladder.
        let depths = take_tx_depth_hits();
        assert!(
            depths[1] + depths[2] > 0,
            "no block split its transform: depths {depths:?}"
        );
    }

    /// A synthetic screen-content picture -- a few flat colours in a grid of
    /// boxes with hard edges and text-like bars, the shape a desktop capture
    /// has -- so the palette path is exercised on content it is for.
    #[cfg(test)]
    fn screen_card(width: usize, height: usize) -> Picture {
        const INK: [u16; 4] = [16, 90, 180, 235];
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let cell = (x / 24 + y / 24) % 2;
                let bar = usize::from(y % 24 < 6 && x % 24 >= 3 && x % 24 < 20);
                picture.y[y * width + x] = INK[cell * 2 + bar];
            }
        }
        // Four chroma colours, not two: a two-colour chroma block only ever
        // codes a `PaletteSizeUV` of 2, which leaves
        // `write_palette_colors_uv`'s cache/delta half (the n-1 shrinking
        // deltas and the neighbour cache flags, exercised only when
        // neighbouring blocks share colours) unproven by the ffmpeg oracle.
        const CHROMA_U: [u16; 4] = [96, 118, 150, 176];
        const CHROMA_V: [u16; 4] = [148, 126, 104, 92];
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let cell = (x / 12 + 2 * (y / 12)) % 4;
                picture.u[y * (width / 2) + x] = CHROMA_U[cell];
                picture.v[y * (width / 2) + x] = CHROMA_V[cell];
            }
        }
        picture
    }

    /// The lane's own end-to-end check: on screen content the detector fires,
    /// blocks really take a palette (fire count, class
    /// `gate-blind-to-feature`), and ffmpeg -- libaom's own reader of every
    /// symbol this lane writes -- reconstructs exactly what the encoder did.
    /// The film-shaped `test_card` must NOT trip the detector, which is what
    /// keeps every non-screen stream byte-identical.
    #[test]
    fn a_screen_content_picture_codes_palette_blocks_ffmpeg_decodes_exactly() {
        let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (192usize, 96usize);
        let _ = crate::tile::take_palette_hits();
        let picture = screen_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        assert!(
            encoded.screen,
            "the detector did not fire on screen content -- no palette syntax was coded at all"
        );
        let hits = crate::tile::take_palette_hits();
        assert!(hits[0] > 0, "no block took a palette: {hits:?}");
        eprintln!("palette blocks {} sizes {:?}", hits[0], &hits[2..]);
        // The chroma half is a claim about the plane-1 syntax only if some
        // block really took a chroma palette (`gate-blind-to-feature`): every
        // exactness assert below passes on a stream that codes
        // `palette_uv_mode == 0` everywhere.
        let uv_hits = crate::tile::take_palette_uv_hits();
        assert!(uv_hits[0] > 0, "no block took a chroma palette: {uv_hits:?}");
        let uv_cached = crate::tile::take_palette_uv_cache_hits();
        eprintln!(
            "chroma palette blocks {} sizes {:?}, {uv_cached} colours from the neighbour cache",
            uv_hits[0],
            &uv_hits[2..]
        );
        assert!(
            uv_cached > 0,
            "no chroma base colour came out of the neighbour cache -- \
             `write_palette_colors_uv`'s cache half is unproven by the oracle below"
        );
        let via_us = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(via_us[0].y, encoded.reconstruction.y, "our decoder: luma");
        assert_eq!(via_us[0].u, encoded.reconstruction.u, "our decoder: U");
        assert_eq!(via_us[0].v, encoded.reconstruction.v, "our decoder: V");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "ffmpeg: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "ffmpeg: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "ffmpeg: V");
        // A whole GOP: the inter frames' own intra blocks carry the palette
        // syntax too (the three intra arms of the inter writers), which only
        // ffmpeg reading the frames back can prove.
        let moved: Vec<Picture> = (0..4)
            .map(|f| {
                let base = screen_card(width, height);
                let mut out = Picture::grey(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let sx = (x + f * 7) % width;
                        out.y[y * width + x] = base.y[y * width + sx];
                    }
                }
                for y in 0..height / 2 {
                    for x in 0..width / 2 {
                        let sx = (x + f * 3) % (width / 2);
                        out.u[y * (width / 2) + x] = base.u[y * (width / 2) + sx];
                        out.v[y * (width / 2) + x] = base.v[y * (width / 2) + sx];
                    }
                }
                out
            })
            .collect();
        let _ = crate::tile::take_palette_hits();
        let _ = crate::tile::take_palette_uv_hits();
        let gop = encode_sequence_with_ctx(&moved, 100, 0.5, fctx).unwrap();
        let gop_hits = crate::tile::take_palette_hits();
        let gop_uv_hits = crate::tile::take_palette_uv_hits();
        let inter_screen = gop.frames.iter().skip(1).filter(|f| f.screen).count();
        eprintln!(
            "GOP: {} frames with screen tools after the key frame, {} palette blocks, \
             {} chroma palette blocks",
            inter_screen, gop_hits[0], gop_uv_hits[0]
        );
        assert!(
            gop_uv_hits[0] > 0,
            "no block of the GOP took a chroma palette: {gop_uv_hits:?}"
        );
        assert!(
            inter_screen > 0,
            "no inter frame set allow_screen_content_tools"
        );
        let gop_decoded = crate::stream::decode_stream(&gop.stream).unwrap();
        for (i, (ours, frame)) in gop_decoded.iter().zip(gop.frames.iter()).enumerate() {
            assert_eq!(ours.y, frame.reconstruction.y, "our decoder: GOP frame {i} luma");
        }
        if have_ffmpeg() {
            let ff = ffmpeg_decode_sequence(&gop.stream, width, height, moved.len());
            for (i, (theirs, frame)) in ff.iter().zip(gop.frames.iter()).enumerate() {
                assert_eq!(theirs.y, frame.reconstruction.y, "ffmpeg: GOP frame {i} luma");
                assert_eq!(theirs.u, frame.reconstruction.u, "ffmpeg: GOP frame {i} U");
                assert_eq!(theirs.v, frame.reconstruction.v, "ffmpeg: GOP frame {i} V");
            }
        }

        // The other half of the keep rule: film-shaped content leaves the
        // detector (and so the whole sequence header) alone.
        let film = encode_key_frame_with_ctx(&test_card(width, height), 100, 0.5, fctx).unwrap();
        assert!(!film.screen, "the detector fired on the film-shaped test card");
    }

    /// The lane's keep-rule measurement: the gate's own screen capture, one
    /// KEY FRAME, coded with and without `allow_intrabc` at four quantizers.
    /// Setting the bit forces deblocking, CDEF and loop restoration off for
    /// that frame, so the table is exactly the filter loss against the bytes
    /// the copied blocks save.
    #[test]
    #[ignore = "needs the real-library manifest and ffmpeg"]
    fn probe_intrabc_key_frame() {
        let fctx = &crate::decode::FrameCtx::new();
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures");
        let Ok(manifest) = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv")) else {
            eprintln!("SKIP probe_intrabc_key_frame: no real-library manifest");
            return;
        };
        let Some(clip) = manifest
            .lines()
            .skip(1)
            .filter_map(|l| l.split('\t').next())
            .find(|p| p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists())
        else {
            eprintln!("SKIP probe_intrabc_key_frame: no OBS recording");
            return;
        };
        let (width, height) = (640usize, 384usize);
        let picture = clip_frame(clip, "0", width, height);
        eprintln!("| base_q_idx | intrabc | bytes | PSNR | blocks | searches/found/won |");
        for q in [5u8, 20, 35, 45] {
            for on in [false, true] {
                force_intrabc(Some(on));
                let _ = crate::tile::take_intrabc_hits();
                let _ = take_intrabc_search();
                let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
                let hits = crate::tile::take_intrabc_hits();
                let search = take_intrabc_search();
                eprintln!(
                    "| {q} | {} | {} | {:.2} dB | {} (dv hist {:?}) | {}/{}/{} |",
                    if encoded.allow_intrabc { "on" } else { "off" },
                    encoded.stream.len(),
                    psnr_all(&encoded.reconstruction, &picture),
                    hits[0],
                    &hits[1..],
                    search[0], search[1], search[2],
                );
            }
        }
        force_intrabc(None);
    }

    /// lane-av1ibc's own end-to-end check: a repeated-pattern screen card
    /// sets `allow_intrabc` (so every in-loop filter is off), blocks really
    /// take a block vector (fire count + DV histogram, class
    /// `gate-blind-to-feature`), and both our own decoder and ffmpeg --
    /// libaom's reader of the `use_intrabc`/`assign_dv` syntax -- reconstruct
    /// exactly what the encoder did.
    #[test]
    fn a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly() {
        let fctx = &crate::decode::FrameCtx::new();
        // Wide enough for the wavefront rule (`INTRABC_DELAY_SB64` = 4
        // superblocks of 64) and tall enough for a source superblock row
        // above the blocks that copy from it.
        let (width, height) = (512usize, 256usize);
        let picture = screen_card(width, height);
        let _ = crate::tile::take_intrabc_hits();
        // The default is OFF (it measured worse on the real screen capture --
        // see [`intrabc_enabled`]); this gate proves the SYNTAX, so it turns
        // the arm on for its own encode and puts it back straight after.
        force_intrabc(Some(true));
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        force_intrabc(None);
        assert!(
            encoded.allow_intrabc,
            "allow_intrabc was not set on a fully periodic screen card              (source repeat share {:.1}%)",
            intrabc_source_share() * 100.0
        );
        let hits = crate::tile::take_intrabc_hits();
        eprintln!(
            "intrabc blocks {} DV magnitude histogram (1,2,4,8,16,32+ px) {:?},              source repeat share {:.1}%",
            hits[0],
            &hits[1..],
            intrabc_source_share() * 100.0
        );
        let search = take_intrabc_search();
        eprintln!(
            "intrabc searches {} found-valid {} won-RD {}",
            search[0], search[1], search[2]
        );
        assert!(
            hits[0] > 0,
            "no block took a block vector -- the `use_intrabc` syntax is unproven: {hits:?}"
        );
        let via_us = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(via_us[0].y, encoded.reconstruction.y, "our decoder: luma");
        assert_eq!(via_us[0].u, encoded.reconstruction.u, "our decoder: U");
        assert_eq!(via_us[0].v, encoded.reconstruction.v, "our decoder: V");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "ffmpeg: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "ffmpeg: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "ffmpeg: V");
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean: no ffmpeg"
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let picture = Picture::grey(width, height);
        let key = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();

        let (seq, _) = key_frame_headers(width, height, 100).unwrap();
        let (_, inter_header) = inter_frame_headers(width, height, 100, 1, 0).unwrap();

        let residual_block = BlockCoeffs {
            angle_delta_y: 0,
            cfl_alphas: None,
            filter_intra: None,
            luma: vec![Coeff {
                row: 0,
                col: 0,
                level: 4,
            }],
            u: Vec::new(),
            v: Vec::new(),
            mode: 0,
            uv_mode: 0,
            skip: false,
            eight: None,
            dv: None,
            palette: None,
            palette_uv: None,
            tx_depth: 0,
            inter: Some(InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            }),
            motion_mode: 0,
        };
        let skipped_block = BlockCoeffs {
            skip: true,
            inter: Some(InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            }),
            ..BlockCoeffs::default()
        };
        let blocks = vec![
            Quadrant::Whole(residual_block),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block),
        ];
        // lane-av1obmc: the header this tile is decoded under carries
        // `is_motion_mode_switchable`, so the writer must be armed with it
        // too or the decoder reads a `motion_mode` symbol nobody wrote.
        crate::tile::arm_motion_mode(inter_header.is_motion_mode_switchable);
        crate::tile::arm_warped_motion(inter_header.allow_warped_motion);
        let tile =
            crate::tile::sb_coeff_inter_frame_tile(inter_header.mi_cols, inter_header.mi_rows, 100, &blocks)
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

    /// lane-av1cap: the final filter replay against the capture decode it
    /// replaced, at the block-padded and straddling sizes the whole-frame
    /// tests cover -- `set_verify_final_replay` makes
    /// [`pick_and_apply_filters`] run both on every frame and assert, per
    /// frame, that the spliced picture and the two filter stages the
    /// restoration search reads are bit-identical over the frame's own crop.
    #[test]
    fn the_final_filter_replay_matches_the_capture_decode_at_odd_sizes() {
        let fctx = &crate::decode::FrameCtx::new();
        crate::decode::set_verify_final_replay(true);
        for &(width, height) in
            &[(64usize, 56usize), (216, 96), (192, 120), (640, 352), (1280, 720)]
        {
            eprintln!("--- {width}x{height}");
            let picture = test_card(width, height);
            let _ = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        }
        crate::decode::set_verify_final_replay(false);
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
    let fctx = &crate::decode::FrameCtx::new();
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
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            return;
        }
        let (width, height) = (854usize, 480usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "854x480: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "854x480: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "854x480: V");
    }

    /// [`Reach::of_tu`] against libaom's `has_top_right`/`has_bottom_left`
    /// transform-unit path, for every 4x4 unit of every 8x8 block of a
    /// superblock, both partition tables:
    ///
    /// 1. outside a `PARTITION_VERT_A`/`_B` it must answer exactly what the
    ///    unit-as-its-own-block `Reach::of(4, ..)` answered before r2, so
    ///    wiring the TU sites onto it changes no existing stream;
    /// 2. inside one it must not panic -- `has_tr_vert_tables[BLOCK_4X4]` is
    ///    NULL in libaom because that lookup never happens, and reading a
    ///    4x4 row out of the three-row vert tables is what crashed a real
    ///    `--enable-tx-size-search=1` stream (r2);
    /// 3. and it must be the BLOCK's vert answer where the two tables
    ///    disagree: the top-right 4x4 unit of the 8x8 block at superblock
    ///    row 1, column 0 reads its above-right samples from the 8x8 to its
    ///    upper right, which a vertical AB partition codes AFTER it
    ///    (`has_tr_vert_8x8` bit 16 = 0, ordinary `has_tr_8x8` bit 16 = 1).
    #[test]
    fn of_tu_follows_the_block_row_including_under_vert_ab() {
    let fctx = &crate::decode::FrameCtx::new();
        // The 8x8 square at superblock row 1, column 0, and the TX4 unit in
        // its top-right corner (`col_off = 4`, `row_off = 0`): that unit's
        // above-right answer is the BLOCK's, which is the whole reason a unit
        // asks the block instead of looking itself up (the
        // `has_tr_vert_*` tables have no 4x4 row at all, lane-ab16 r2).
        let plain = Reach::of(8, 0, 8, SUPERBLOCK, SUPERBLOCK, fctx);
        assert_eq!(
            Reach::of_tu(8, 8, 4, 0, 4, 4, plain).above_right,
            plain.above_right,
            "ordinary table: the top-right TX4 unit takes the block's row"
        );
        let guard = Reach::vert_ab_partition();
        let vert = Reach::of(8, 0, 8, SUPERBLOCK, SUPERBLOCK, fctx);
        assert_eq!(
            Reach::of_tu(8, 8, 4, 0, 4, 4, vert).above_right,
            vert.above_right,
            "vert AB: the unit follows the vert row the block just picked"
        );
        assert_ne!(
            plain.above_right, vert.above_right,
            "this position is the cell where the vert tables disagree; pick another if libaom's tables change"
        );
        drop(guard);
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
        let fctx = &crate::decode::FrameCtx::new();
        // Transcribed from has_top_right/has_bottom_left's row_off==0,
        // col_off==0 (whole-transform) path, with MAX_MIB_SIZE_LOG2 = 5 (a
        // 128px reference grid) pinned as libaom pins it, independent of the
        // 64px superblock this crate actually codes.
        fn libaom_top_right(side: usize, x: usize, y: usize, width: usize, _height: usize) -> bool {
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

        fn libaom_bottom_left(side: usize, x: usize, y: usize, height: usize, _fctx: &crate::decode::FrameCtx) -> bool {
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
                    let reach = Reach::of(side, x, y, width, height, fctx);
                    assert_eq!(
                        reach.above_right,
                        libaom_top_right(side, x, y, width, height),
                        "side={side} x={x} y={y}: above_right"
                    );
                    assert_eq!(
                        reach.below_left,
                        libaom_bottom_left(side, x, y, height, fctx),
                        "side={side} x={x} y={y}: below_left"
                    );
                }
            }
        }
    }

    /// lane-part32 r6: inside a `PARTITION_VERT_A`/`_B` the 8x8 squares are
    /// visited TL, BL, TR, BR, so the TR square (even 8x8 row, odd 8x8
    /// column inside the superblock) may read the below-left samples the
    /// raster table forbids: `has_bl_8x8` is 0 at exactly those 16 slots
    /// where `has_bl_vert_8x8` is 1. Pinned here because no aomenc recipe
    /// swept in r6 produced a decodable 16x16-level VERT_B stream (every
    /// stream that partitions that small first hits the still-refused
    /// HORZ_A/HORZ_B/VERT_A-below-16 or coded-rect-strip-below-16 arms).
    /// [`Reach::of_tu`] against a from-scratch transcription of libaom's
    /// `has_top_right`/`has_bottom_left` TU branches (`reconintra.c`, luma,
    /// `ss_x = ss_y = 0`), which work in MI units where `of_tu` works in
    /// pixels -- lane-palette2 r12: the square multi-TU path used the
    /// standalone-block tables instead of this rule and granted a 16x16
    /// D203 block's top-right 8x8 unit bottom-left pixels libaom refuses.
    #[test]
    fn of_tu_matches_libaom_has_top_right_and_has_bottom_left_per_unit() {
        // libaom, in MI units: bw_unit/bh_unit = block size / 4,
        // *_count_unit = tx size / 4.
        fn libaom_tr(bw: usize, bh: usize, col_off: usize, row_off: usize, tx: usize, blk: bool) -> bool {
            let (plane_bw_unit, count) = (bw / 4, tx / 4);
            let (col_off_u, row_off_u) = (col_off / 4, row_off / 4);
            let _ = bh;
            if row_off_u > 0 {
                col_off_u + count < plane_bw_unit
            } else if col_off_u + count < plane_bw_unit {
                true
            } else {
                blk
            }
        }
        fn libaom_bl(bw: usize, bh: usize, col_off: usize, row_off: usize, tx: usize, blk: bool) -> bool {
            let (plane_bh_unit, count) = (bh / 4, tx / 4);
            let (col_off_u, row_off_u) = (col_off / 4, row_off / 4);
            let _ = bw;
            if col_off_u > 0 {
                false
            } else if row_off_u + count < plane_bh_unit {
                true
            } else {
                blk
            }
        }
        for &(bw, bh) in &[(8, 8), (16, 16), (32, 32), (64, 64), (16, 8), (8, 16), (32, 16), (16, 32), (64, 32), (32, 64)] {
            for &tx in &[4usize, 8, 16, 32] {
                if tx > bw.min(bh) {
                    continue;
                }
                for row_off in (0..bh).step_by(tx) {
                    for col_off in (0..bw).step_by(tx) {
                        for &blk in &[Reach { above_right: false, below_left: false }, Reach { above_right: true, below_left: true }] {
                            let got = Reach::of_tu(bw, bh, col_off, row_off, tx, tx, blk);
                            assert_eq!(
                                got.above_right,
                                libaom_tr(bw, bh, col_off, row_off, tx, blk.above_right),
                                "above_right bw={bw} bh={bh} tx={tx} row_off={row_off} col_off={col_off} blk={blk:?}"
                            );
                            assert_eq!(
                                got.below_left,
                                libaom_bl(bw, bh, col_off, row_off, tx, blk.below_left),
                                "below_left bw={bw} bh={bh} tx={tx} row_off={row_off} col_off={col_off} blk={blk:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn vert_ab_partition_flips_below_left_for_the_top_right_8x8() {
    let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (SUPERBLOCK * 3, SUPERBLOCK * 3);
        // Interior superblock, so neither the frame edge nor the col == 0 /
        // last-row early returns answer instead of the table.
        let (sb_x, sb_y) = (SUPERBLOCK, SUPERBLOCK);
        let mut flipped = 0;
        for row in 0..8 {
            for col in 1..8 {
                let (x, y) = (sb_x + col * 8, sb_y + row * 8);
                let raster = Reach::of(8, x, y, width, height, fctx).below_left;
                let vert = {
                    let _guard = Reach::vert_ab_partition();
                    Reach::of(8, x, y, width, height, fctx).below_left
                };
                // The TR square of a VERT_A/_B sits at an even row, odd
                // column; every other slot must be untouched by the guard.
                if row % 2 == 0 && col % 2 == 1 {
                    assert!(!raster, "raster below_left at row={row} col={col}");
                    assert!(vert, "vert below_left at row={row} col={col}");
                    flipped += 1;
                } else {
                    assert_eq!(raster, vert, "guard moved row={row} col={col}");
                }
            }
        }
        assert_eq!(flipped, 4 * 4, "expected the 16 8x8 TR slots to flip");
        // The guard is scoped: it must not leak past its own block.
        assert!(!Reach::of(8, sb_x + 8, sb_y, width, height, fctx).below_left);
    }

    /// An odd width or height is refused by name, not by however the padder
    /// or the block coder would happen to fail on it.
    #[test]
    fn odd_dimensions_are_refused() {
    let fctx = &crate::decode::FrameCtx::new();
        for &(width, height) in &[(1921usize, 1080usize), (1920, 1081), (63, 63)] {
            let picture = Picture::grey(width, height);
            let err = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx)
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!("SKIP sequence_round_trips_at_a_non_multiple_size: no ffmpeg");
            return;
        }
        let (width, height) = (160usize, 96usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!("SKIP a_sequence_round_trips_at_the_exactly_half_straddle_size: no ffmpeg");
            return;
        }
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (640usize, 360usize);
        let picture = Picture::grey(width, height);
        encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).expect("key frame now codes the mod-32==8 straddle");

        // `encode_sequence`'s second frame is the inter path: same size, now
        // wired the same way.
        let pictures = vec![picture.clone(), picture];
        encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
            .expect("inter frame now codes the mod-32==8 straddle too");
    }

    /// The r15 fix at production size: three 640x360 key frames (inter is
    /// still refused at this size, per the test above, so this stands in for
    /// the charter's "3-frame sequence" using three independent key frames of
    /// shifted content), each proven decoder-clean through ffmpeg rather than
    /// only encoder-clean.
    #[test]
    fn a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames() {
    let fctx = &crate::decode::FrameCtx::new();
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
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
            let dc = ladder(&picture, &[DC_PRED], fctx);
            let searched = ladder(&picture, &NON_DIRECTIONAL, fctx);
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
    let fctx = &crate::decode::FrameCtx::new();
        for (name, picture) in sweep_pictures() {
            let all = ladder(&picture, &KEY_FRAME_MODES, fctx);
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
    let fctx = &crate::decode::FrameCtx::new();
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
                            unspecified_color_config(), fctx,
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
                unspecified_color_config(), fctx,
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!("SKIP every_mode_decodes_to_what_the_encoder_predicted: no ffmpeg");
            return;
        }
        // A mode ablation measures the MODE search: a filter-intra block is
        // coded `DC_PRED` whatever mode the arm forces, so it would fail the
        // "the encoder coded something else" check for a reason that is not
        // about the mode search at all.
        force_filter_intra(Some(false));
        // 128 wide is a whole number of superblocks and 160 is not: the last
        // superblock of a 160-wide row is half a one, whose blocks have no
        // above-right samples inside the frame at all.
        for (width, height) in [(128, 96), (160, 96)] {
            let picture = test_card(width, height);
            for mode in KEY_FRAME_MODES {
                let encoded = encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[mode], fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        // Same confound as [`every_mode_decodes_to_what_the_encoder_predicted`]:
        // a filter-intra block is coded `DC_PRED` whatever mode the arm
        // offers, and these stripes are SCREEN content, where the filter arm
        // now searches its own transform depth (lane-fitu) and wins outright
        // -- which says nothing about the mode search this gate measures.
        force_filter_intra(Some(false));
        for (vertical, want) in [(true, V_PRED), (false, H_PRED)] {
            let picture = stripes(128, 96, vertical);
            // Only the pair, because on stripes a third mode predicts exactly
            // what the right one of the pair does -- PAETH reads the corner and
            // the left column, which a striped picture makes equal -- and the
            // tie then goes to whichever is cheaper to name, which is not what
            // this gate is about.
            let encoded =
                encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[V_PRED, H_PRED], fctx).unwrap();
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
        // lane-av1tx: `TxMode::Select` moved the directional ladder clear of
        // the non-directional one on the synthetic diagonals -- every point
        // both higher in PSNR *and* lower in rate -- so there is no PSNR band
        // to integrate over any more. A curve that dominates that way has
        // saved everything the measure can express (the
        // `instrument-at-bound` class: the answer is at the instrument's own
        // edge, not undefined), so name it rather than panicking; anything
        // less clean than strict dominance still is.
        if high <= low {
            let ref_lo_rate = reference.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            let other_hi_rate = other.iter().map(|p| p.1).fold(f64::MIN, f64::max);
            let dominates = other[0].0 > reference[reference.len() - 1].0
                && other_hi_rate <= ref_lo_rate;
            assert!(
                dominates,
                "the two ladders have to overlap in PSNR: {reference:?} vs {other:?}"
            );
            return -1.0;
        }
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
    fn ladder(picture: &Picture, modes: &[u8], fctx: &crate::decode::FrameCtx) -> Vec<(f64, f64)> {
        // A mode ablation measures the MODE search: the palette would code
        // these synthetic two-colour pictures losslessly under every arm and
        // leave nothing to compare (see [`force_screen`]).
        force_screen(Some(false));
        // Same reason, for the filter-intra candidate: it wins blocks under
        // BOTH arms of a mode ablation and so cancels the difference the
        // ablation measures (class `gate-recipe-confound`).
        force_filter_intra(Some(false));
        let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
            .iter()
            .map(|&q| {
                let encoded = encode_key_frame_with_modes_with_ctx(picture, q, 0.5, modes, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        for (name, picture, want) in [
            ("test card", test_card(160, 96), -0.05),
            ("stripes", stripes(160, 96, true), -0.40),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &[DC_PRED], fctx),
                &ladder(&picture, &NON_DIRECTIONAL, fctx),
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
    let fctx = &crate::decode::FrameCtx::new();
        for (name, picture) in sweep_pictures() {
            let dc = ladder(&picture, &[DC_PRED], fctx);
            let flat = ladder(&picture, &NON_DIRECTIONAL, fctx);
            let all = ladder(&picture, &KEY_FRAME_MODES, fctx);
            let encoded = encode_key_frame_with_ctx(&picture, 90, 0.5, fctx).unwrap();
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
    fn favourite_mode(picture: &Picture, fctx: &crate::decode::FrameCtx) -> (u8, usize, usize) {
        // A mode ablation measures the MODE search: the palette would code
        // these synthetic two-colour pictures losslessly under every arm and
        // leave nothing to compare (see [`force_screen`]).
        force_screen(Some(false));
        let encoded = encode_key_frame_with_ctx(picture, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        for (down_right, want) in [(true, D135_PRED), (false, D45_PRED)] {
            let picture = diagonal(160, 96, down_right);
            let (mode, count, blocks) = favourite_mode(&picture, fctx);
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
    let fctx = &crate::decode::FrameCtx::new();
        for (name, picture, want) in [
            ("down-right", diagonal(160, 96, true), -0.20),
            ("down-left", diagonal(160, 96, false), -0.20),
            ("test card", test_card(160, 96), 0.01),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &NON_DIRECTIONAL, fctx),
                &ladder(&picture, &KEY_FRAME_MODES, fctx),
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
    let fctx = &crate::decode::FrameCtx::new();
        let picture = test_card(64, 64);
        let message = encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[13], fctx)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 13"),
            "the refusal must name the mode, got {message}"
        );
        assert!(encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[], fctx).is_err());
    }

    /// The picture that comes back is the picture that went in, to within the
    /// quantizer. A prediction that read the wrong neighbours, or a block
    /// written into the wrong place, would still decode to what the encoder
    /// reconstructed — it is this gate that says the reconstruction is of the
    /// right picture.
    #[test]
    fn the_encoded_picture_is_the_one_that_went_in() {
    let fctx = &crate::decode::FrameCtx::new();
        let picture = test_card(160, 96);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        let picture = test_card(128, 128);
        let mut previous: Option<(usize, f64)> = None;
        for &q in &[70u8, 90, 110] {
            let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
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
            let encoded = encode_key_frame_with_ctx(&picture, 100, deadzone, fctx).unwrap();
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
        //
        // corner-cut (lane-av1rd3): the bound is 5% rather than exact. Since
        // chroma searches its own mode, a coarser quantizer can pick a
        // *different* uv_mode on a block whose modes now cost nearly the
        // same, and the search prices that symbol against the static default
        // CDF while the writer codes it against an adapted one -- 311 -> 319
        // bytes at q 120->121 on this picture. The table this gate is really
        // watching would move the rate by far more than the mode churn does.
        // lane-av1tx widened it again, 5% -> 10%: under `TxMode::Select` the
        // same block can also change transform DEPTH between two neighbouring
        // quantizers (297 -> 315 bytes at q 120->121 here), and the depth
        // symbol is priced off the static CDF row 0 for the same reason the
        // uv_mode one is.
        // Ceiling: a q-context table wrong by under 10% would pass. Upgrade
        // path is pricing through the adapting CDF state (what
        // `predicted_coeff_bits_track_the_tile_the_writer_wrote` measures the
        // drift of), after which the exact bound comes back.
        for &(lo, hi) in &[(20u8, 21u8), (60, 61), (120, 121)] {
            let lo_bytes = encode_key_frame_with_ctx(&picture, lo, 0.5, fctx).unwrap().stream.len();
            let hi_bytes = encode_key_frame_with_ctx(&picture, hi, 0.5, fctx).unwrap().stream.len();
            assert!(
                hi_bytes as f64 <= lo_bytes as f64 * 1.10,
                "q {lo}->{hi} crosses a context boundary: {lo_bytes} -> {hi_bytes} bytes"
            );
        }
    }

    /// A flat picture is a flat stream: every block predicts its neighbours'
    /// average, which is the picture's own value, and codes nothing.
    #[test]
    fn a_flat_picture_costs_almost_nothing() {
    let fctx = &crate::decode::FrameCtx::new();
        let mut picture = Picture::grey(128, 128);
        picture.y.fill(97);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
    /// panicking somewhere below. Sizes off the 32x32 block grid encode fine
    /// (see `a_frame_round_trips_at_its_own_size`) -- the true frame edge
    /// lands past the halfway point of whichever block it falls in, so
    /// `PARTITION_NONE` or a single gathered split flag still says everything
    /// the spec needs. lane-av1-rect r7 wired the 8x8-leaf path for a 16x16
    /// leaf straddling on exactly one axis (e.g. 40x32), and lane-av1rect the
    /// both-axes cut (e.g. 40x40, both dims mod 32 == 8), which spec 5.11.4
    /// codes with NO partition symbol at all -- `PARTITION_SPLIT` inferred,
    /// down to the in-frame 8x8s, no rectangular transform anywhere (the old
    /// refusal here claimed otherwise and was wrong; see
    /// `crate::stream::tests::a_sweep_of_doubly_straddling_sizes_round_trips_
    /// through_ffmpeg`). No size refusal is left: an 8x8 leaf can never
    /// straddle either, because `MiCols`/`MiRows` are always EVEN (spec
    /// 5.9.5 `compute_image_size`: `2 * ((frame_width + 7) >> 3)`), so the
    /// true edge always lands on an 8-luma-sample boundary in mi terms and a
    /// 4x4 leaf -- which neither writer has -- is never asked for. What is
    /// still refused is a malformed picture.
    #[test]
    fn a_picture_off_the_block_grid_encodes_and_a_malformed_one_is_refused() {
    let fctx = &crate::decode::FrameCtx::new();
        encode_key_frame_with_ctx(&Picture::grey(40, 40), 100, 0.5, fctx)
            .expect("40x40 cuts a 16x16 leaf on both axes, which is an inferred split");
        encode_key_frame_with_ctx(&Picture::grey(36, 40), 100, 0.5, fctx)
            .expect("36 luma columns still round up to an even MiCols");

        let mut short = Picture::grey(64, 64);
        short.u.truncate(10);
        assert!(encode_key_frame_with_ctx(&short, 100, 0.5, fctx).is_err());
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
    let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!("SKIP a_single_axis_straddle_round_trips_through_ffmpeg: no ffmpeg");
            return;
        }
        let (width, height) = (40usize, 32usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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

        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
        // lane-t900 r10: the stream goes down stdin on ITS OWN THREAD. Writing
        // it inline deadlocks the moment ffmpeg's stdout pipe buffer (64 KiB)
        // fills before the last input byte is written -- which is exactly what
        // a 1.1 MB fixture decoding to 150 MB of raw 10-bit frames does
        // (measured: 45 min, both processes at 0% CPU). A write error here is
        // swallowed on purpose: ffmpeg's own exit status and stderr, asserted
        // below, are the real diagnosis.
        let mut stdin = child.stdin.take().expect("ffmpeg stdin");
        let payload = stream.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        });
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        writer.join().expect("ffmpeg stdin writer thread");
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
    let fctx = &crate::decode::FrameCtx::new();
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
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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

    /// The same sequence through OUR OWN decoder, sample-exact against the
    /// encoder's reconstruction. ffmpeg stays the bitstream oracle
    /// ([[shared-oracle-blindness]]) -- but a stream ffmpeg REFUSES yields no
    /// divergence point at all, while our decoder can be traced symbol by
    /// symbol (`EC_TRACE_MODE_STEP`) on the very stream that broke. This is
    /// the instrument the inter `TxMode::Select` hunt lacked; on failure it
    /// names the first differing frame, plane and position.
    #[test]
    fn every_frame_of_a_sequence_decodes_through_our_own_decoder() {
        let fctx = &crate::decode::FrameCtx::new();
        // 256x128, not the 128x64 this started at: once a leaf could search
        // its second reference (lane-av1comp4) the small card's every
        // eligible block went to a compound winner, which commits its flat
        // transform, and the split below stopped firing on content that
        // still splits at every larger size ([[gate-blind-to-feature]] --
        // the guard is right, the fixture had shrunk under it).
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> =
            (0..5).map(|i| panned_test_card(width, height, i * 3)).collect();
        let _ = take_inter_tx_split_hits();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let raw = take_inter_tx_split_hits();
        // Summed over the four block classes the census keys on (leaf/32x32 x
        // single/compound): this assert is about the split firing at all.
        let split = [
            raw.iter().step_by(2).sum::<usize>(),
            raw.iter().skip(1).step_by(2).sum::<usize>(),
        ];
        eprintln!("inter var-tx: {} blocks flat, {} split", split[0], split[1]);
        // [[gate-blind-to-feature]]: with inter `TxMode::Select` on, this
        // content splits -- a green round trip in which the split never fires
        // would prove only the flat path.
        if tx_select() && tx_select_inter() {
            assert!(split[1] > 0, "the inter var-tx split never fired");
        }
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want, stride) in [
                ("luma", &dec.y, &recon.y, width),
                ("U", &dec.u, &recon.u, width / 2),
                ("V", &dec.v, &recon.v, width / 2),
            ] {
                assert_eq!(got.len(), want.len(), "frame {i}: {plane} size");
                if let Some(at) = got.iter().zip(want).position(|(a, b)| a != b) {
                    panic!(
                        "frame {i}: {plane} differs first at ({}, {}): decoded {} vs \
                         reconstruction {} ({} of {} samples differ)",
                        at % stride,
                        at / stride,
                        got[at],
                        want[at],
                        got.iter().zip(want).filter(|(a, b)| a != b).count(),
                        got.len(),
                    );
                }
            }
        }
    }

    /// The rate term the mode search ranks every decision by has to be the
    /// rate the writer then spends. The search prices a block's coefficients
    /// through `tile::coeff_bits` -- the writer's own symbol chain, but
    /// against the default CDFs a tile starts from and the contexts of a
    /// block whose neighbours coded nothing -- while the tile writer codes
    /// them against a state that has been adapting all frame (the
    /// price-the-narrowing class). This measures that drift: the sum of what
    /// the search paid for the coefficients it kept, against the bytes the
    /// writer actually emitted for the whole tile (which also carries the
    /// partition/mode/skip/mv syntax the sum leaves out, so the writer is
    /// expected to be the larger of the two).
    #[test]
    fn predicted_coeff_bits_track_the_tile_the_writer_wrote() {
        let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        let _ = take_predicted_bits();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let predicted = take_predicted_bits();
        assert_eq!(predicted.len(), encoded.frames.len());

        eprintln!("frame  predicted bits  written bits  drift");
        // Split by sign: a NEGATIVE drift is the search OVER-pricing (the
        // defect this gate exists to catch), a positive one is the written
        // side carrying syntax the coefficient sum leaves out -- and since
        // lane-av1mv an extra reference's `NEWMV` puts a whole MV residual
        // on that side, so the two directions no longer share a bound.
        let mut worst_over: f64 = 0.0;
        let mut worst_under: f64 = 0.0;
        // The census is per CODED frame, so it is zipped in coding order --
        // which is not display order under a pyramid.
        let coded: Vec<&Encoded> =
            encoded.coding_order.iter().map(|&i| &encoded.frames[i]).collect();
        for (i, (frame, &bits)) in coded.into_iter().zip(&predicted).enumerate() {
            let written = frame.tile.len() as f64 * 8.0;
            let drift = (written - bits) / written;
            worst_over = worst_over.max(-drift);
            // lane-av1pyrdef: a pyramid LEAF codes almost no coefficients at
            // all (150-250 bits of tile, nearly all of it partition/mode/mv
            // syntax the coefficient sum never counts), so its ratio measures
            // that floor rather than any drift. Frames under 512 written bits
            // are left out of the under-price bound for that reason; the
            // over-price bound -- the defect direction, where the search pays
            // MORE than the writer -- still covers every frame.
            if written >= 512.0 {
                worst_under = worst_under.max(drift);
            }
            eprintln!("{i:5}  {bits:14.0}  {written:12.0}  {:+6.2}%", drift * 100.0);
        }
        // Measured 2026-09-06 on this sequence: -13.5% at the key frame,
        // -8.1%, -3.8%, -2.1%, +1.8% down the inter frames. The search
        // over-prices every frame but the last (its sum EXCLUDES the
        // partition/mode/skip/mv syntax the written side carries, so the true
        // coefficient over-price is larger still): the writer's CDFs adapt to
        // the content while the search's stay at the defaults. The bound is
        // set just above the worst measured point -- a widening drift is the
        // thing this gate exists to catch.
        assert!(
            worst_over <= 0.16,
            "the search's rate term OVER-priced by {:.2}% against the bits the writer spent",
            worst_over * 100.0
        );
        // Measured 2026-09-06 with the extra-reference `NEWMV` in: +18.9% at
        // the worst inter frame, where nearly every block now codes an MV
        // residual and a reference the coefficient sum never counts. Re-measured
        // at +29.6% once the encoder's grid published compound blocks properly
        // (compound share 8.8% -> 11.5%): a compound block carries a second
        // reference tree, a compound mode symbol and -- for the half-new modes
        // -- an MV residual, none of which the coefficient sum counts.
        // lane-av1txbits: pricing against the tables the frame's writer really
        // starts from collapses the over-price above to nothing, so the whole
        // remaining gap is the syntax the coefficient sum leaves out. That is
        // the default on a non-screen frame since lane-av1price2 (this
        // fixture is one), and it reads +32.6% here.
        // lane-av1obmc: every eligible single-reference inter block now also
        // carries a `motion_mode` symbol the coefficient sum never counts,
        // and an OBMC block carries the more expensive of the two values --
        // re-measured at +35.0% here with `EC_AV1_OBMC=1`. Like
        // `EC_AV1_PRICE_FRAME_CDFS=1` before it, that knob changes encoder
        // decisions, so this bound and the byte pins above FAIL BY DESIGN
        // under it; the default path (no motion_mode syntax at all) is the
        // one they are measured on.
        // lane-av1pyrdef: `encode_sequence` codes this fixture under the
        // coding pyramid now, so the five frames are a key frame, a hidden
        // ALTREF at q-16 and three leaves at q+8: +2.6% / +6.9% (the two
        // frames over the 512-bit floor) and +44.7 / +74.4 / +74.4 on the
        // near-empty leaves below it. Both bounds stand where they were.
        // lane-av1rejudge: local warp is ON by default now, so every eligible
        // single-reference block carries that same `motion_mode` symbol (a
        // 3-value alphabet where a warp sample exists) on the DEFAULT path --
        // +34.9% here, one point past the OBMC reading above. The bound moves
        // with it; `EC_AV1_WARP=0` restores the +32.6% no-motion_mode number.
        // lane-av1cfl: a `UV_CFL_PRED` block carries a `cfl_alpha_signs`
        // symbol and one or two magnitude symbols, none of which the
        // coefficient sum counts either -- +36.9% here against the same
        // fixture's +34.9% with `EC_AV1_CFL=0`.
        // ... and a directional block whose `angle_delta_y` is no longer
        // always ZERO codes a costlier symbol off the same CDF -- +39.1%.
        assert!(
            worst_under <= 0.40,
            "the writer spent {:.2}% more than the search priced -- more than \
             the mode/mv syntax outside the coefficient sum explains",
            worst_under * 100.0
        );
    }

    /// The census behind step (1) of the pricer lane: for every transform
    /// block the writer really codes, what the search priced those levels at
    /// against what the writer spent on them, split by table set and non-zero
    /// count. One average hides the interesting part -- the classes the
    /// estimate is systematically wrong about (all-zero blocks, the
    /// one-coefficient blocks the inter search lives on) -- so the split is
    /// printed and only the total is asserted on.
    #[test]
    fn pricer_error_census_by_block_class() {
        let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        crate::tile::pricer_census_on();
        let _ = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let rows = crate::tile::take_pricer_census();
        assert!(!rows.is_empty(), "the census saw no transform block");

        println!("| set | nz | blocks | priced bits | written bits | error |");
        println!("|---|---|---|---|---|---|");
        let (mut priced_all, mut written_all) = (0.0, 0.0);
        for ((set, bucket), (blocks, priced, written)) in &rows {
            priced_all += priced;
            written_all += written;
            println!(
                "| {set} | {} | {blocks} | {priced:.0} | {written:.0} | {:+.1}% |",
                crate::tile::census_bucket_label(*bucket),
                if *written > 0.0 {
                    (priced - written) / written * 100.0
                } else {
                    0.0
                }
            );
        }
        println!(
            "| ALL | | {} | {priced_all:.0} | {written_all:.0} | {:+.1}% |",
            rows.values().map(|r| r.0).sum::<u64>(),
            (priced_all - written_all) / written_all * 100.0
        );
        assert!(written_all > 0.0);
    }

    /// Step (2) of the pricer lane: the same census, but on the REAL clips
    /// the BD gate scores -- one film clip and the screen capture -- so the
    /// question the film/screen split raises can be answered from data. Real
    /// tables (`EC_AV1_PRICE_FRAME_CDFS=1`) make both film clips smaller and
    /// the screen capture larger, and the census says why: on a screen
    /// capture the frame's own starting tables are the tables of a frame that
    /// coded PALETTE and long runs of zero blocks, so they are far from the
    /// tables the writer ends the frame with; the pricer error does not fall
    /// the way it does on film.
    ///
    /// Not a gate -- it prints. Run it once per mode:
    ///     EC_AV1_PRICE_FRAME_CDFS=0 cargo test -p ec-av1 --release --lib -- \
    ///         --ignored pricer_error_census_on_clips --nocapture
    #[test]
    #[ignore = "reads the real library, needs ffmpeg"]
    fn pricer_error_census_on_clips() {
        if !have_ffmpeg() {
            eprintln!("SKIP pricer_error_census_on_clips: no ffmpeg");
            return;
        }
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let mut clips: Vec<(String, std::path::PathBuf)> = Vec::new();
        let film = fixtures.join("video/h264-1080p-23.976-8bit.mp4");
        if film.exists() {
            clips.push(("film 1080p".to_string(), film));
        }
        if let Ok(manifest) = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv"))
            && let Some(p) = manifest
                .lines()
                .skip(1)
                .filter_map(|l| l.split('\t').next())
                .find(|p| p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists())
        {
            clips.push(("screen".to_string(), std::path::PathBuf::from(p)));
        }
        assert!(!clips.is_empty(), "no source clip available");
        let (width, height, frames) = (640usize, 384usize, 6usize);
        for (name, path) in &clips {
            let fctx = &crate::decode::FrameCtx::new();
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = crate::tile::take_pricing_hits();
            crate::tile::pricer_census_on();
            let _ = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
            let rows = crate::tile::take_pricer_census();
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            println!(
                "\n## {name} ({frames} frames at {width}x{height}, q=100): screen frames \
                 {screen_on}, pricer armings default={price_default} real={price_real}"
            );
            println!("| set | nz | blocks | priced bits | written bits | error |");
            println!("|---|---|---|---|---|---|");
            let (mut priced_all, mut written_all) = (0.0, 0.0);
            for ((set, bucket), (blocks, priced, written)) in &rows {
                priced_all += priced;
                written_all += written;
                println!(
                    "| {set} | {} | {blocks} | {priced:.0} | {written:.0} | {:+.1}% |",
                    crate::tile::census_bucket_label(*bucket),
                    if *written > 0.0 { (priced - written) / written * 100.0 } else { 0.0 }
                );
            }
            println!(
                "| ALL | | {} | {priced_all:.0} | {written_all:.0} | {:+.1}% |",
                rows.values().map(|r| r.0).sum::<u64>(),
                (priced_all - written_all) / written_all * 100.0
            );
            assert!(written_all > 0.0, "{name}: the census saw no transform block");
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
    let fctx = &crate::decode::FrameCtx::new();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();

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
    let fctx = &crate::decode::FrameCtx::new();
        use std::time::Instant;

        for &(width, height) in &[(1920usize, 1080usize), (3840, 2160)] {
            let picture = test_card(width, height);

            let t = Instant::now();
            let dc_only = encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[DC_PRED], fctx).unwrap();
            let dc_only_t = t.elapsed();

            let t = Instant::now();
            let non_directional =
                encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &NON_DIRECTIONAL, fctx).unwrap();
            let non_directional_t = t.elapsed();

            let t = Instant::now();
            let all_modes = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
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
                encode_inter_frame(&padded_pic, &padded_ref, 100, 0.5, 1, flat_order_hints(1, 0, false), (width, height), None, None, None, fctx, None, &[]).unwrap();
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
                std::hint::black_box(crate::transform::dequant_and_inverse(&levels, side, 8, 100));
            }
            let inverse_t = t.elapsed() / N;

            let set = if side == 32 {
                crate::cdf_state::TxbSet::Luma32
            } else {
                crate::cdf_state::TxbSet::Luma16
            };
            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(crate::tile::coeff_bits(&levels, set, 2, 0, 0));
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
                    &mut prediction, fctx,
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
    let fctx = &crate::decode::FrameCtx::new();
        use std::time::Instant;

        let (width, height) = (1280usize, 720);
        let picture = test_card(width, height);
        let key = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let padded_ref = key.reconstruction.padded_to(SUPERBLOCK);
        let padded_pic = picture.padded_to(SUPERBLOCK);

        stage_reset();
        let t = Instant::now();
        let inter =
            encode_inter_frame(&padded_pic, &padded_ref, 100, 0.5, 1, flat_order_hints(1, 0, false), (width, height), None, None, None, fctx, None, &[]).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
                let baseline = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
                let baseline_psnr = psnr(&baseline.reconstruction.y, &picture.y);
                eprint!(
                    "{label:9} q={q:3}  baseline {:6} bytes  {:6.2} dB",
                    baseline.stream.len(),
                    baseline_psnr
                );
                for &k in &[3usize, 4, 6] {
                    set_test_top_k_override(Some(k));
                    let pruned = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
                let key = encode_key_frame_with_ctx(&key_source, q, 0.5, fctx).unwrap();
                let baseline = encode_inter_frame(
                    &inter_source,
                    &key.reconstruction,
                    q,
                    0.5,
                    1,
                    flat_order_hints(1, 0, false),
                    (width, height), None, None, None, fctx, None, &[],
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
                        flat_order_hints(1, 0, false),
                        (width, height), None, None, None, fctx, None, &[],
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
        clip_frames_vf(clip, skip, &format!("scale={width}:{height}"), width, height, frames)
    }

    /// [`clip_frames`] with the filter chain spelled out, so a caller can ask
    /// for a native-resolution CROP instead of a downscale (the downscale is
    /// what destroys a screen capture's exact repeats -- class
    /// `gate-recipe-confound`). `width`/`height` must be what `vf` produces.
    fn clip_frames_vf(
        clip: &str,
        skip: &str,
        vf: &str,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip])
            .args(["-frames:v", &frames.to_string()])
            .args(["-vf", vf])
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
    let fctx = &crate::decode::FrameCtx::new();
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
        let encoded = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
        let encoded = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
        use std::time::Instant;
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..24)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let t = Instant::now();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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
    let fctx = &crate::decode::FrameCtx::new();
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
                let encoded = encode_sequence_with_ctx(&source, q, 0.5, fctx).unwrap();
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

    // -----------------------------------------------------------------------
    // The encoder's standing BD-rate baseline against libaom and rav1e.
    // Every PSNR below is measured on FFMPEG-DECODED pixels, never on an
    // encoder's own reconstruction ([[shared-oracle-blindness]]): our decoder
    // agreeing with our encoder proves nothing about the bitstream.
    // -----------------------------------------------------------------------

    /// All-plane PSNR of a decoded picture against its source, on the same
    /// 8-bit scale [`psnr`] uses.
    fn psnr_all(decoded: &Picture, source: &Picture) -> f64 {
        let cat = |p: &Picture| {
            let mut v = Vec::with_capacity(p.y.len() + p.u.len() + p.v.len());
            v.extend_from_slice(&p.y);
            v.extend_from_slice(&p.u);
            v.extend_from_slice(&p.v);
            v
        };
        psnr(&cat(decoded), &cat(source))
    }

    /// The pictures back as the planar 8-bit bytes an encoder reads.
    fn raw_yuv420p(source: &[Picture]) -> Vec<u8> {
        let mut raw = Vec::new();
        for p in source {
            for plane in [&p.y, &p.u, &p.v] {
                raw.extend(plane.iter().map(|&s| s as u8));
            }
        }
        raw
    }

    /// A (all-plane PSNR, log10 bytes) ladder from another AV1 encoder driven
    /// through ffmpeg: one point per entry of `points`, each a quality flag
    /// set. Returns the ladder ordered by fidelity and the total encode wall
    /// clock. The stream is decoded back through [`ffmpeg_decode_sequence`],
    /// so both curves are measured by the same decoder.
    fn external_ladder(
        source: &[Picture],
        width: usize,
        height: usize,
        encoder: &str,
        points: &[Vec<String>],
    ) -> (Vec<(f64, f64)>, f64) {
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let dir = std::env::temp_dir();
        let raw_path = dir.join(format!("ec-av1-bd-{}-{encoder}.yuv", std::process::id()));
        std::fs::write(&raw_path, raw_yuv420p(source)).expect("raw source");
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        for (i, params) in points.iter().enumerate() {
            let obu = dir.join(format!("ec-av1-bd-{}-{encoder}-{i}.obu", std::process::id()));
            let start = std::time::Instant::now();
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
                .args(["-s", &format!("{width}x{height}"), "-r", "24", "-i"])
                .arg(&raw_path)
                .args(["-an", "-threads", "1"])
                .args(["-g", &source.len().to_string(), "-c:v", encoder])
                .args(params)
                .args(["-f", "obu"])
                .arg(&obu)
                .output()
                .expect("ffmpeg failed to run");
            wall += start.elapsed().as_secs_f64();
            assert!(
                out.status.success(),
                "{encoder} {params:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = std::fs::read(&obu).expect("reference stream");
            let decoded = ffmpeg_decode_sequence(&stream, width, height, source.len());
            let mean: f64 = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .sum::<f64>()
                / source.len() as f64;
            ladder.push((mean, (stream.len() as f64).log10()));
            let _ = std::fs::remove_file(&obu);
        }
        let _ = std::fs::remove_file(&raw_path);
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall)
    }

    /// First differing sample between two pictures, as
    /// `(plane, index, left, right)` -- so a gate failure names the plane and
    /// the sample instead of just "differs". A length difference counts as a
    /// mismatch of that plane at the first index past the shorter one.
    fn first_plane_mismatch(a: &Picture, b: &Picture) -> Option<(&'static str, usize, i64, i64)> {
        for (plane, l, r) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
            if let Some(i) = l.iter().zip(r.iter()).position(|(x, y)| x != y) {
                return Some((plane, i, i64::from(l[i]), i64::from(r[i])));
            }
            if l.len() != r.len() {
                return Some((plane, l.len().min(r.len()), l.len() as i64, r.len() as i64));
            }
        }
        None
    }

    /// The same ladder for this encoder, at four `base_q_idx` points. The PSNR
    /// comes from ffmpeg's decode of our own stream, and at EVERY point the
    /// stream is decoded twice -- by ffmpeg and by our own
    /// [`crate::stream::decode_stream`] -- and all three pictures (encoder
    /// reconstruction, ffmpeg's decode, our decode) are asserted sample-exact
    /// in Y, U and V over every shown frame in display order. That is itself
    /// a gate: it proves the bitstream says what the encoder thinks, in the
    /// only two decoders that read it.
    fn our_ladder(
        name: &str,
        source: &[Picture],
        width: usize,
        height: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> (Vec<(f64, f64)>, f64) {
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        for &q in &[150u8, 120, 90, 60] {
            let start = std::time::Instant::now();
            let encoded = encode_sequence_with_ctx(source, q, 0.5, fctx).unwrap();
            wall += start.elapsed().as_secs_f64();
            let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, source.len());
            let ours = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
            assert_eq!(
                ours.len(),
                source.len(),
                "{name} q={q}: our decoder's display-order frame count"
            );
            for (i, (d, e)) in decoded.iter().zip(&encoded.frames).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(d, &e.reconstruction) {
                    panic!(
                        "{name} q={q} frame {i} plane {plane} sample {s}: ffmpeg decoded \
                         {got}, the encoder reconstructed {want}"
                    );
                }
            }
            for (i, (o, e)) in ours.iter().zip(&encoded.frames).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(o, &e.reconstruction) {
                    panic!(
                        "{name} q={q} frame {i} plane {plane} sample {s}: our decoder decoded \
                         {got}, the encoder reconstructed {want}"
                    );
                }
            }
            let mean: f64 = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .sum::<f64>()
                / source.len() as f64;
            ladder.push((mean, (encoded.stream.len() as f64).log10()));
        }
        // What this clip asked for and what the content gate left it coding
        // under: a screen clip requests a pyramid and codes flat, and that
        // difference is what keeps its row identical to the flat baseline
        // (class `gate-blind-to-feature`).
        eprintln!(
            "{name}: sequence, pyramid requested {:?} effective {:?}",
            crate::encoder::Pyramid::from_env(),
            last_sequence_pyramid(),
        );
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall)
    }

    /// [`our_ladder`] through the streaming facade — the surface the editor's
    /// export drives (`EC_AV1_GATE_FACADE=1`). With a [`crate::encoder::Pyramid`]
    /// each stream is also reordered (hidden `ALTREF` first,
    /// `show_existing_frame` last per group) and each level is coded at its
    /// own quantizer offset; without one it is the flat ladder, which
    /// `encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence`
    /// pins byte-identical to [`our_ladder`]'s, so this arm must print the
    /// same BD numbers. Returns the ladder, the wall, per pyramid level over
    /// the whole ladder how many frames and how many bytes it spent, and the
    /// pyramid the streams were EFFECTIVELY coded under — the content gate in
    /// `Av1Encoder::encode_frames` drops a screen-content clip back to the
    /// flat path, so a requested pyramid can come back `None` here.
    fn our_ladder_facade(
        name: &str,
        source: &[Picture],
        width: usize,
        height: usize,
        pyramid: Option<crate::encoder::Pyramid>,
    ) -> (Vec<(f64, f64)>, f64, [usize; 4], [usize; 4], Option<crate::encoder::Pyramid>) {
        use crate::encoder::{Av1Encoder, Colour, EncoderConfig, Level};
        let slot = |l: Level| match l {
            Level::Key => 0,
            Level::Arf => 1,
            Level::Leaf => 2,
            Level::ShowExisting => 3,
        };
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        let mut effective = None;
        let (mut counts, mut bytes) = ([0usize; 4], [0usize; 4]);
        for &q in &[150u8, 120, 90, 60] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: q,
                gop: source.len(),
                colour: Colour::Unspecified,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let start = std::time::Instant::now();
            let mut enc = match pyramid {
                None => Av1Encoder::new(config).unwrap(),
                Some(p) => Av1Encoder::with_pyramid(config, p).unwrap(),
            };
            let mut packets = Vec::new();
            for picture in source {
                packets.extend(enc.encode_frames(picture).unwrap());
            }
            packets.extend(enc.flush().unwrap());
            wall += start.elapsed().as_secs_f64();
            effective = enc.pyramid();
            let mut stream = Vec::new();
            for p in &packets {
                counts[slot(p.level)] += 1;
                bytes[slot(p.level)] += p.data.len();
                stream.extend_from_slice(&p.data);
            }
            // [[gate-blind-to-hidden-frames]]: a reordered stream is only
            // correct if the DISPLAY-order output still has one frame per
            // source picture, in the right order -- checked against our own
            // decoder as well as ffmpeg's, both below.
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), source.len(), "{name} q={q}: our display-order count");
            let decoded = ffmpeg_decode_sequence(&stream, width, height, source.len());
            for (i, (d, o)) in decoded.iter().zip(&ours).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(d, o) {
                    panic!(
                        "{name} q={q} display frame {i} plane {plane} sample {s}: ffmpeg \
                         decoded {got}, our decoder decoded {want}"
                    );
                }
            }
            let mean: f64 = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .sum::<f64>()
                / source.len() as f64;
            ladder.push((mean, (stream.len() as f64).log10()));
        }
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall, counts, bytes, effective)
    }

    /// Byte-exactness gate for encoder work that is meant to change only
    /// how long the encoder takes.
    ///
    /// The BD gate above measures quality, which a speed lane can move by a
    /// hair without failing anything; this pins the streams themselves.
    /// Re-measured on the lane-av1lr merge with main d5fc99e0 (per-64x64
    /// `cdef_idx` literals and per-unit loop restoration syntax are new bits
    /// in the tile) and again on lane-av1mv's merge of it: an extra
    /// reference's `NEWMV` is a coded-bits change too, judged by the BD gate
    /// above, not by this pin. Re-pinned on lane-av1price2: the search now
    /// prices coefficients against the tables the frame's writer really
    /// starts from on every non-screen frame (`tile::arm_pricing_cdfs`), a
    /// decision change the BD gate judges. Re-pinned again on
    /// lane-av1skipctx: the search now prices `txb_skip`/`dc_sign` at the
    /// real neighbour contexts ([`Plane::coef_ctx`]) instead of zero, which
    /// is the same kind of decision change. Re-pinned on lane-av1lambda:
    /// [`LAMBDA_SCALE`] moved 0.1 -> 0.05, so every RD decision in the
    /// search moved with it. Re-pinned on lane-av1rejudge: local warp is on
    /// by default ([`warp_on`], a sequence/frame header bit and a new
    /// motion_mode symbol on eligible blocks) and
    /// [`LEAF_SECOND_NEW_MARGIN`] moved 0.8 -> 1.2. Re-pinned on lane-av1tpl:
    /// every inter frame's superblocks now code at their own lambda
    /// ([`TPL_STRENGTH`]), so every RD decision in an inter frame moved.
    /// Re-pinned on lane-av1resweep: [`LAMBDA_SCALE`] moved 0.05 -> 0.0275
    /// and the two var-tx depth searches ([`tx32_depth_search`],
    /// [`compound_var_tx`]) are on by default, all three re-judged on the
    /// native gate's real film rows.
    /// Re-pinned on lane-av1cfl: the chroma search offers `UV_CFL_PRED`, so
    /// every block whose chroma takes it codes a different mode symbol, two
    /// alpha symbols and different chroma coefficients. `EC_AV1_CFL=0`
    /// restores the previous streams. Re-pinned again in the same lane once
    /// directional luma winners searched their `angle_delta_y`
    /// (`EC_AV1_ANGLE=0` restores that half).
    #[test]
    fn the_encoders_own_streams_are_byte_identical_to_their_pins() {
        if !have_ffmpeg() {
            eprintln!("SKIP the_encoders_own_streams_are_byte_identical_to_their_pins: no ffmpeg");
            return;
        }
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        if !clip.exists() {
            eprintln!("SKIP the_encoders_own_streams_are_byte_identical_to_their_pins: no clip");
            return;
        }
        let source = clip_frames(clip.to_str().unwrap(), "0", 640, 384, 4);
        // FNV-1a over the stream: a witness that every coded bit is where it
        // was, which a byte count alone is not.
        let fnv = |b: &[u8]| {
            b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &v| {
                (h ^ u64::from(v)).wrapping_mul(0x0000_0100_0000_01b3)
            })
        };
        // Re-taken on lane-av1pyrdef: `encode_sequence` codes this (non-screen)
        // clip under the coding pyramid now -- one hidden ALTREF per mini-GOP
        // of 4, its leaves at q+8 and a `show_existing_frame` header per
        // group. 7299 -> 8194 bytes at q=150 and 26804 -> 28285 at q=60 over
        // four frames; four pictures is one short mini-GOP, which is the
        // shape that pays least (the ARF's own quality has no later group to
        // carry it), so this is not the BD gate's number.
        // Re-taken again on lane-av1cfl: the chroma search offers
        // `UV_CFL_PRED` and directional luma winners search `angle_delta_y`,
        // so every block taking either codes different syntax and different
        // coefficients -- 8194 -> 7321 at q=150 and 28285 -> 27285 at q=60.
        // `EC_AV1_CFL=0` / `EC_AV1_ANGLE=0` restore each half.
        // Re-taken on lane-pyr3: the mini-GOP of the default pyramid is 16
        // pictures at `q-24` / `q+16` now (the sweep in [`Pyramid::default`]),
        // so these four pictures code as one truncated group -- a hidden
        // ALTREF at `q-24` and three leaves at `q+16` -- instead of one
        // mini-GOP of 4 at `q-16` / `q+8`: 7373 -> 8076 at q=150 and
        // 27074 -> 27585 at q=60. `EC_AV1_PYRAMID=4:-16:8` restores these.
        // Re-taken on lane-fintra: the sequence header sets
        // `enable_filter_intra`, so every DC_PRED intra block of at most
        // 32x32 without a luma palette carries a `use_filter_intra` flag and
        // the blocks that take one code a filter-intra mode, a different
        // prediction and different coefficients -- 7321 -> 7373 at q=150 and
        // 27285 -> 27074 at q=60. `EC_AV1_FILTER_INTRA=0` restores these.
        // Re-taken on lane-b64: the inter search offers the whole superblock
        // as one 64x64 `PARTITION_NONE` block coded skip ([`B64_ROOT`]), and
        // every superblock that takes one codes a single partition symbol,
        // one mode/mv chain and no residual where four quadrants used to --
        // 8076 -> 7291 at q=150 and 27585 -> 27466 at q=60. `EC_AV1_B64=0`
        // restores these.
        // Re-taken on lane-tx64: that 64x64 root can code a REAL residual now
        // (one TX_64X64 luma transform and two TX_32X32 chroma ones), so a
        // superblock whose skip arm was too coarse codes coefficients where
        // it used to fall back to four quadrants -- 7291 -> 7355 at q=150 and
        // 27466 -> 27573 at q=60. `EC_AV1_B64RES=0` restores these;
        // `EC_AV1_B64=0` still gives lane-b64's own pre-root 8076 / 27585.
        let pins: [(u8, usize, u64); 2] =
            [(150, 7355, 0xd074_3dc7_a231_8a20), (60, 27573, 0xd9fb_3c84_57cc_b86f)];
        for (q, bytes, hash) in pins {
            let encoded = encode_sequence(&source, q, 0.5).unwrap();
            assert_eq!(
                (encoded.stream.len(), fnv(&encoded.stream)),
                (bytes, hash),
                "q={q}: the encoder's stream moved"
            );
        }
    }

    /// A ladder is only a ladder if both axes rise together.
    fn assert_monotone(name: &str, ladder: &[(f64, f64)]) {
        assert_eq!(ladder.len(), 4, "{name}: wanted four quality points");
        for w in ladder.windows(2) {
            assert!(
                w[1].0 > w[0].0 && w[1].1 > w[0].1,
                "{name}: not monotone, {ladder:?}"
            );
        }
    }

    /// The sweep behind [`screen_content`]'s constants
    /// (`unswept-decision-constants`), on the SAME five clips
    /// [`bd_rate_screen_native`] keeps off (`native_gate_clips`) and at the
    /// same native crop: for each colour bound N the share of 16x16 luma
    /// blocks with 2..=N distinct colours, and the share of those whose
    /// per-pixel variance also clears a floor V -- libaom's
    /// `av1_set_screen_content_options` counts the variance-cleared blocks
    /// too (its `counts_2`, which gates intrabc), and without that term a
    /// smooth dark film frame reads as screen content.
    /// `cargo test -p ec-av1 --release --lib -- --ignored probe_screen_detect --nocapture`
    #[test]
    #[ignore = "reads the real library"]
    fn probe_screen_detect() {
        if !have_ffmpeg() {
            eprintln!("SKIP probe_screen_detect: no ffmpeg");
            return;
        }
        let limits = [4usize, 8, 16, 32, 64, 128];
        let floors = [0u64, 4, 16, 64, 256];
        // The census of one clip at one size: mean share, over the frames, of
        // 16x16 blocks with 2..=N colours whose per-pixel variance is > V.
        let census = |label: &str, frames: &[Picture], cw: usize, ch: usize| {
            let mut acc = [[0f64; 5]; 6];
            for picture in frames {
                let y: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
                let mut hit = [[0usize; 5]; 6];
                let mut blocks = 0usize;
                for by in (0..ch).step_by(16) {
                    for bx in (0..cw).step_by(16) {
                        let mut seen = [false; 256];
                        let (mut colors, mut sum, mut sq) = (0usize, 0u64, 0u64);
                        for row in 0..16 {
                            for col in 0..16 {
                                let v = y[(by + row) * cw + bx + col];
                                sum += u64::from(v);
                                sq += u64::from(v) * u64::from(v);
                                if !seen[usize::from(v)] {
                                    seen[usize::from(v)] = true;
                                    colors += 1;
                                }
                            }
                        }
                        let var = (sq - sum * sum / 256) / 256;
                        blocks += 1;
                        for (li, &limit) in limits.iter().enumerate() {
                            if colors > 1 && colors <= limit {
                                for (fi, &floor) in floors.iter().enumerate() {
                                    hit[li][fi] += usize::from(var > floor);
                                }
                            }
                        }
                    }
                }
                for li in 0..6 {
                    for fi in 0..5 {
                        acc[li][fi] += 100.0 * hit[li][fi] as f64 / blocks.max(1) as f64;
                    }
                }
            }
            let n = frames.len().max(1) as f64;
            eprintln!("{label} {cw}x{ch}, {} frames -- share of 16x16 blocks with 2..=N colours and per-pixel var > V:", frames.len());
            eprintln!("  N \\ V |{}", floors.iter().map(|f| format!("{f:>8}")).collect::<String>());
            for (li, &limit) in limits.iter().enumerate() {
                eprintln!(
                    "  {limit:>5} |{}",
                    (0..5).map(|fi| format!("{:>7.1}%", acc[li][fi] / n)).collect::<String>()
                );
            }
        };
        for (name, path, seek) in native_gate_clips() {
            let Some((nw, nh)) = probe_dims(&path) else {
                eprintln!("SKIP {name}: ffprobe gave no size");
                continue;
            };
            let cw = nw.min(1920) / 128 * 128;
            let ch = nh.min(1024) / 128 * 128;
            let (x, y0) = ((nw - cw) / 2 & !1, (nh - ch) / 2 & !1);
            let frames = clip_frames_vf(&path, &seek, &format!("crop={cw}:{ch}:{x}:{y0}"), cw, ch, 12);
            census(&name, &frames, cw, ch);
            // The 640x384 gate scales instead of cropping, and the scaling
            // changes the census (bars smooth out): the constants have to
            // separate at BOTH sizes.
            let small = clip_frames(&path, &seek, 640, 384, 12);
            census(&name, &small, 640, 384);
        }
    }

    /// The detector against the WHOLE real library (class
    /// `unswept-decision-constants`, user rule "his data is the gate"):
    /// every VIDEO row of `fixtures/real-library-manifest.tsv` -- plus the
    /// two colour-bar fixtures as anchors -- decoded at 10/50/90% of its
    /// duration, four frames each, 8-bit 4:2:0, at native size or a
    /// 1920x1080 centred crop when larger (the crop keeps a capture's exact
    /// repeats, which a downscale destroys -- class `gate-recipe-confound`).
    /// Prints per file and offset the share of qualifying 16x16 blocks, the
    /// [`screen_content`] verdict and a content hint (mean luma, luma
    /// standard deviation, inter MAD), then sweeps the colour bound against
    /// the variance floor over the whole library: for each (N, V) the worst
    /// (highest) share among the camera/film rows against the best (lowest)
    /// among the screen-recording rows, so the rule with the widest gap can
    /// be read off. Rows carry the manifest index, codec and size only --
    /// no titles, no home paths.
    /// `cargo test -p ec-av1 --release --lib -- --ignored probe_screen_library --nocapture`
    #[test]
    #[ignore = "reads the real library"]
    fn probe_screen_library() {
        if !have_ffmpeg() {
            eprintln!("SKIP probe_screen_library: no ffmpeg");
            return;
        }
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let Ok(manifest) = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv")) else {
            eprintln!("SKIP probe_screen_library: no real-library manifest");
            return;
        };
        let limits = [4usize, 8, 16, 32, 64];
        let floors = [0u64, 4, 8, 16, 32, 64, 128];
        // Whatever ffmpeg gave us, without the exact-count assertion the
        // gate loaders make: a seek near the end of a clip returns fewer
        // frames, and that is a row to print, not a panic.
        let decode = |clip: &str, seek: &str, vf: &str, w: usize, h: usize, n: usize| -> Vec<Picture> {
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", seek, "-i", clip])
                .args(["-frames:v", &n.to_string()])
                .args(["-vf", vf])
                .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
                .output();
            let Ok(out) = out else { return Vec::new() };
            if !out.status.success() {
                return Vec::new();
            }
            let (luma, chroma) = (w * h, w * h / 4);
            let frame_len = luma + 2 * chroma;
            (0..out.stdout.len() / frame_len)
                .map(|i| {
                    let b = &out.stdout[i * frame_len..][..frame_len];
                    Picture {
                        width: w,
                        height: h,
                        y: b[..luma].iter().map(|&v| u16::from(v)).collect(),
                        u: b[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                        v: b[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
                    }
                })
                .collect()
        };
        // Share (%) of 16x16 blocks with 2..=N colours and per-pixel
        // variance > V, for every (N, V) of the sweep grid, over one frame.
        let census = |y: &[u8], w: usize, h: usize| -> [[f64; 7]; 5] {
            let mut hit = [[0usize; 7]; 5];
            let mut blocks = 0usize;
            for by in (0..h).step_by(16) {
                for bx in (0..w).step_by(16) {
                    let (bh, bw) = ((h - by).min(16), (w - bx).min(16));
                    let mut seen = [false; 256];
                    let (mut colors, mut sum, mut sq) = (0usize, 0u64, 0u64);
                    for row in 0..bh {
                        for col in 0..bw {
                            let v = y[(by + row) * w + bx + col];
                            sum += u64::from(v);
                            sq += u64::from(v) * u64::from(v);
                            if !seen[usize::from(v)] {
                                seen[usize::from(v)] = true;
                                colors += 1;
                            }
                        }
                    }
                    let n = (bh * bw) as u64;
                    let var = (sq - sum * sum / n) / n;
                    blocks += 1;
                    for (li, &limit) in limits.iter().enumerate() {
                        if colors > 1 && colors <= limit {
                            for (fi, &floor) in floors.iter().enumerate() {
                                hit[li][fi] += usize::from(var > floor);
                            }
                        }
                    }
                }
            }
            let mut out = [[0f64; 7]; 5];
            for li in 0..5 {
                for fi in 0..7 {
                    out[li][fi] = 100.0 * hit[li][fi] as f64 / blocks.max(1) as f64;
                }
            }
            out
        };
        // (index, label, path, duration, screen-by-its-own-content). The
        // OBS directory and the OBS file-name shape ("YYYY-MM-DD HH-MM-SS")
        // are desktop recordings; everything else in this library is camera
        // or film material.
        let mut rows: Vec<(String, String, String, f64, bool)> = Vec::new();
        for (label, file) in [("bars 1080p", "h264-1080p-23.976-8bit.mp4"), ("bars 2160p", "h264-2160p-23.976-8bit.mp4")] {
            let path = fixtures.join("video").join(file);
            if path.exists() {
                rows.push(("fx".into(), label.into(), path.to_str().unwrap().into(), 10.0, false));
            }
        }
        for (i, line) in manifest.lines().skip(1).enumerate() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 10 {
                continue;
            }
            let (path, codec) = (f[0], f[2]);
            if codec == "-" || codec.is_empty() {
                continue;
            }
            let (Ok(w), Ok(h)) = (f[3].parse::<usize>(), f[4].parse::<usize>()) else {
                eprintln!("SKIP {}: manifest carries no coded size", i + 1);
                continue;
            };
            if !std::path::Path::new(path).exists() {
                eprintln!("SKIP {}: file is gone", i + 1);
                continue;
            }
            let name = path.rsplit('/').next().unwrap_or(path);
            let is_capture = path.contains("/OBS/")
                || (name.len() > 19
                    && name.as_bytes()[4] == b'-'
                    && name.as_bytes()[7] == b'-'
                    && name.as_bytes()[10] == b' '
                    && name.as_bytes()[13] == b'-');
            rows.push((
                format!("{}", i + 1),
                format!("{codec} {w}x{h}"),
                path.to_string(),
                f[8].parse::<f64>().unwrap_or(0.0),
                is_capture,
            ));
        }
        // Per (N, V): the worst camera/film row and the best screen row, so
        // the separation of the whole library is one subtraction.
        let (mut cam_max, mut scr_min) = ([[0f64; 7]; 5], [[100f64; 7]; 5]);
        eprintln!("| idx | codec size | class | offset | qualifying blocks | verdict | mean/sd/MAD |");
        for (idx, label, path, dur, is_capture) in &rows {
            let Some((nw, nh)) = probe_dims(path) else {
                eprintln!("SKIP {idx}: ffprobe gave no size");
                continue;
            };
            let (cw, ch) = (nw.min(1920) & !1, nh.min(1080) & !1);
            let (x, y0) = ((nw - cw) / 2 & !1, (nh - ch) / 2 & !1);
            let vf = format!("crop={cw}:{ch}:{x}:{y0}");
            let mut file_grid = [[0f64; 7]; 5];
            let mut file_frames = 0usize;
            for pct in [10u32, 50, 90] {
                let seek = format!("{:.3}", dur * f64::from(pct) / 100.0);
                let frames = decode(path, &seek, &vf, cw, ch, 4);
                if frames.is_empty() {
                    eprintln!("| {idx} | {label} | {} | {pct}% | SKIP: ffmpeg decoded no frame here |", if *is_capture { "screen" } else { "camera" });
                    continue;
                }
                let (mut share, mut yes, mut mean, mut sd, mut mad) = (0f64, 0usize, 0f64, 0f64, 0f64);
                let mut prev: Option<Vec<u8>> = None;
                for picture in &frames {
                    let y: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
                    let grid = census(&y, cw, ch);
                    for li in 0..5 {
                        for fi in 0..7 {
                            file_grid[li][fi] += grid[li][fi];
                        }
                    }
                    file_frames += 1;
                    // The shipped rule is (16 colours, var > 16): grid row 2,
                    // floor 16 is column 3.
                    share += grid[2][3];
                    yes += usize::from(screen_content(&y, cw, cw, ch));
                    let n = y.len() as f64;
                    let sum: f64 = y.iter().map(|&v| f64::from(v)).sum();
                    let sq: f64 = y.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
                    mean += sum / n;
                    sd += (sq / n - (sum / n) * (sum / n)).max(0.0).sqrt();
                    if let Some(p) = &prev {
                        mad += p.iter().zip(&y).map(|(&a, &b)| f64::from(a.abs_diff(b))).sum::<f64>() / n;
                    }
                    prev = Some(y);
                }
                let f = frames.len() as f64;
                eprintln!(
                    "| {idx} | {label} | {} | {pct}% | {:.1}% | {} ({yes}/{} frames) | {:.0}/{:.1}/{:.2} |",
                    if *is_capture { "screen" } else { "camera" },
                    share / f,
                    if yes * 2 > frames.len() { "SCREEN" } else { "camera" },
                    frames.len(),
                    mean / f,
                    sd / f,
                    mad / (f - 1.0).max(1.0),
                );
            }
            if file_frames == 0 {
                continue;
            }
            for li in 0..5 {
                for fi in 0..7 {
                    let s = file_grid[li][fi] / file_frames as f64;
                    match is_capture {
                        true => scr_min[li][fi] = scr_min[li][fi].min(s),
                        false => cam_max[li][fi] = cam_max[li][fi].max(s),
                    }
                }
            }
        }
        eprintln!("sweep over the whole library -- worst camera/film share vs best screen share, per (N colours, var > V):");
        eprintln!("  N \\ V |{}", floors.iter().map(|f| format!("{:>18}", format!("V>{f}"))).collect::<String>());
        let (mut best, mut best_rule) = (f64::MIN, (16usize, 16u64));
        for (li, &limit) in limits.iter().enumerate() {
            let mut row = String::new();
            for fi in 0..7 {
                let (c, s) = (cam_max[li][fi], scr_min[li][fi]);
                row.push_str(&format!("{:>18}", format!("{c:.1}/{s:.1} {:+.1}", s - c)));
                if s - c > best {
                    best = s - c;
                    best_rule = (limit, floors[fi]);
                }
            }
            eprintln!("  {limit:>5} |{row}");
        }
        // The threshold sits at the midpoint of the widest gap; the shipped
        // constant is its reciprocal in whole parts of the frame.
        let (li, fi) = (
            limits.iter().position(|&l| l == best_rule.0).unwrap(),
            floors.iter().position(|&f| f == best_rule.1).unwrap(),
        );
        let mid = (cam_max[li][fi] + scr_min[li][fi]) / 2.0;
        eprintln!(
            "widest split: {} colours, var > {}, gap {:+.1} points (camera <= {:.1}%, screen >= {:.1}%); threshold {:.1}% = EC_AV1_SCREEN_PCT {}",
            best_rule.0,
            best_rule.1,
            best,
            cam_max[li][fi],
            scr_min[li][fi],
            mid,
            (100.0 / mid.max(0.1)).round() as usize,
        );
    }

    /// The missing instrument: what this encoder costs against libaom and
    /// rav1e at matched fidelity, and how long it takes to get there.
    ///
    /// Recipe, identical on all three sides: 12 consecutive frames of each
    /// clip scaled to 640x384 (a whole number of superblocks), one key frame
    /// then 11 inter frames (`-g 12`, matching our own sequence shape), one
    /// tile, `-threads 1`, no film grain synthesis (neither reference enables
    /// it by default), 4:2:0 8-bit in and out. Reference points:
    ///   libaom-av1: `-cpu-used 6 -b:v 0 -crf {5,20,35,45}`
    ///   librav1e:   `-rav1e-params speed=6:quantizer={50,100,150,200}:
    ///                tile_cols=1:tile_rows=1:threads=1`
    /// ours: `base_q_idx {60,90,120,150}`. Every PSNR is all-plane, on
    /// ffmpeg-decoded frames of each encoder's own stream.
    ///
    /// Measured 2026-09-06 after the in-loop filters (lane-av1filt: per-frame
    /// deblocking levels and CDEF strengths, both chosen by re-decoding the
    /// coded tile -- `crate::filter_search`), 12 frames, this box (wall = the
    /// four encodes of that ladder together):
    ///
    /// | clip | ours (q 150/120/90/60) | BD-rate vs libaom | vs rav1e | wall ours:aom:rav1e |
    /// |---|---|---|---|---|
    /// | 1080p fixture | 42.11 dB/17774 B .. 50.99 dB/76838 B | +143.6% | +94.0% | 9.2s:1.1s:2.6s |
    /// | 2160p fixture | 42.84 dB/12480 B .. 51.43 dB/46557 B | +179.5% | +128.8% | 8.6s:1.0s:2.2s |
    /// | screen capture | 39.76 dB/9536 B .. 48.87 dB/29189 B | +88.0% | +21.5% | 7.2s:1.0s:2.1s |
    ///
    /// Re-measured 2026-09-06 after lane-av1fwd, which changed only how long
    /// the encoder takes -- every stream it writes is byte-identical, so the
    /// BD columns are this box's own re-run of the same recipe rather than a
    /// quality move (+141.3/+174.2/+88.9 vs libaom, +91.9/+124.5/+21.6 vs
    /// rav1e). What moved is the wall of the four encodes:
    ///
    /// | clip | wall ours before | after | instructions:u before -> after |
    /// |---|---|---|---|
    /// | 1080p fixture | 9.2s | 7.2s | 191.5G -> 139.8G (-27.0%) |
    /// | 2160p fixture | 8.6s | 6.9s | 183.5G -> 136.4G (-25.6%) |
    /// | screen capture | 7.2s | 6.7s | 157.2G -> 122.5G (-22.1%) |
    ///
    /// Three exact steps, none of which touches a coded bit: the coefficient
    /// rate pricer stopped rebuilding every CDF table per candidate
    /// (`tile::coeff_bits`), the filter search stopped re-decoding its own
    /// tile once per candidate (`decode::FilterReplay`), and the forward
    /// transform stopped transforming the all-zero residual that 70% of its
    /// calls carry.
    ///
    /// The three steps of that lane, same recipe (vs libaom, then vs rav1e):
    ///
    /// | step | 1080p | 2160p | screen |
    /// |---|---|---|---|
    /// | no in-loop filter | +234.0 / +171.9 | +260.3 / +203.2 | +318.5 / +155.3 |
    /// | + deblocking | +198.2 / +143.1 | +218.1 / +162.4 | +136.9 / +52.7 |
    /// | + CDEF | +143.6 / +94.0 | +179.5 / +128.8 | +88.0 / +21.5 |
    ///
    /// Re-measured for the record 2026-09-07 (lane-av1resweep), after
    /// [`LAMBDA_SCALE`] moved 0.05 -> 0.0275 and the two var-tx depth
    /// searches ([`tx32_depth_search`], [`compound_var_tx`]) became defaults
    /// -- all three decided on the REAL film rows of
    /// [`bd_rate_screen_native`], which this downscaled recipe does not
    /// carry:
    ///
    /// | clip | BD vs libaom | BD vs rav1e | wall ours:libaom:rav1e |
    /// |---|---|---|---|
    /// | bars 1080p | +68.8% | +26.2% | 5.8s:1.2s:2.7s |
    /// | bars 2160p | +81.6% | +41.5% | 4.8s:1.0s:2.2s |
    /// | screen capture | +49.1% | -4.3% | 5.5s:1.0s:2.1s |
    ///
    /// RE-RECORDED 2026-09-07 (lane-av1pyrdef), with the coding pyramid the
    /// DEFAULT of the sequence path this gate codes through -- the bars are
    /// non-screen so they take it, the capture is gated flat:
    ///
    /// | clip | effective pyramid | BD vs libaom | BD vs rav1e | wall |
    /// |---|---|---|---|---|
    /// | bars 1080p | 4:-16:8 | +84.7% | +39.9% | 5.1s:1.1s:2.6s |
    /// | bars 2160p | 4:-16:8 | +124.4% | +75.2% | 4.4s:1.0s:2.8s |
    /// | screen capture | none (gated) | +49.1% | -4.3% | 6.5s:1.2s:2.3s |
    ///
    /// Those are, to the byte, the numbers lane-av1pyrgate recorded for the
    /// same clips under `EC_AV1_PYRAMID=4:-16:8 EC_AV1_GATE_FACADE=1` -- the
    /// facade arm -- which is what "one mini-GOP driver, two entry points"
    /// means here. This downscaled recipe HATES the pyramid (as it hates
    /// every knob the real film rows of `bd_rate_screen_native` keep, which
    /// is why it decides nothing); `EC_AV1_PYRAMID=0` restores the flat rows
    /// above. The capture's row is the flat row to the byte in both runs --
    /// that equality IS the content gate.
    ///
    /// Loop restoration is NOT in yet: it is the one filter whose parameters
    /// are per restoration UNIT inside the tile payload, so it needs both
    /// tile writers to code `lr` syntax and an encoder-visible post-CDEF
    /// buffer to solve against -- neither of which this frame-header-level
    /// search reaches.
    ///
    /// The baseline it replaced, before an inter frame's blocks chose their
    /// own partition, was +501.4/+504.4/+440.9 against libaom and
    /// +364.4/+401.0/+206.0 against rav1e at 5.1/4.6/5.9 s.
    ///
    /// So: roughly 2.5-3x libaom's rate and 1.5-2x rav1e's at matched
    /// fidelity, while spending far more time than either (the partition
    /// search's own cost, uncut: see the two intra-pruning variants measured
    /// and rejected in the lane report). No
    /// threshold is asserted yet -- this run sets the baseline; the gate
    /// asserts four monotone points per ladder, and at EVERY ladder point of
    /// EVERY clip it decodes our stream twice -- with ffmpeg and with our own
    /// `crate::stream::decode_stream` -- and asserts the encoder's
    /// reconstruction, ffmpeg's decode and our decode sample-exact in Y, U
    /// and V over every shown frame in display order. A failure names the
    /// clip, the q, the frame, the plane and the first differing sample.
    /// (Under `EC_AV1_PYRAMID` the frames come out of the streaming facade,
    /// which exposes no per-packet reconstruction, so that path asserts the
    /// two decoders against each other on all three planes.)
    ///
    /// Run:
    ///     cargo test -p ec-av1 --release --lib -- --ignored \
    ///         bd_rate_vs_libaom_and_rav1e --nocapture
    #[test]
    #[ignore = "the encoder BD-rate baseline: minutes per clip, needs ffmpeg"]
    fn bd_rate_vs_libaom_and_rav1e() {
        if !have_ffmpeg() {
            eprintln!("SKIP bd_rate_vs_libaom_and_rav1e: no ffmpeg");
            return;
        }
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let mut clips: Vec<(String, std::path::PathBuf)> = Vec::new();
        // testsrc2 COLOUR BARS, not film -- labelled so no reader takes these
        // rows for real content (the real rows live in the native gate).
        for (label, name) in [
            ("bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            ("bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            let path = fixtures.join("video").join(name);
            if path.exists() {
                clips.push((label.to_string(), path));
            } else {
                eprintln!("SKIP clip {name}: missing");
            }
        }
        // Screen capture: the repo carries no screen-capture fixture, so take
        // the first OBS recording the real-library manifest names.
        match std::fs::read_to_string(fixtures.join("real-library-manifest.tsv")) {
            Ok(manifest) => {
                let screen = manifest
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split('\t').next())
                    .find(|p| p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists());
                match screen {
                    Some(p) => clips.push((
                        format!("screen capture ({})", p.rsplit('/').next().unwrap_or(p)),
                        std::path::PathBuf::from(p),
                    )),
                    None => eprintln!("SKIP screen capture: no OBS recording in the manifest exists"),
                }
            }
            Err(e) => eprintln!("SKIP screen capture: no real-library manifest ({e})"),
        }
        assert!(!clips.is_empty(), "no source clip available");

        let (width, height, frames) = (640usize, 384usize, 12usize);
        let aom_points: Vec<Vec<String>> = [5, 20, 35, 45]
            .iter()
            .map(|q| {
                ["-cpu-used", "6", "-b:v", "0", "-crf", &q.to_string()]
                    .map(String::from)
                    .to_vec()
            })
            .collect();
        let rav1e_points: Vec<Vec<String>> = [50, 100, 150, 200]
            .iter()
            .map(|q| {
                vec![
                    "-rav1e-params".to_string(),
                    format!("speed=6:quantizer={q}:tile_cols=1:tile_rows=1:threads=1"),
                ]
            })
            .collect();
        println!(
            "\n{frames} frames of each clip at {width}x{height}, gop={frames}, 1 tile, 1 thread"
        );
        println!("| clip | ours PSNR/bytes per point | BD-rate vs libaom | BD-rate vs rav1e | wall ours:libaom:rav1e |");
        println!("|---|---|---|---|---|");
        for (name, path) in &clips {
            let fctx = &crate::decode::FrameCtx::new();
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            let _ = take_partition_hits();
            let _ = crate::tile::take_palette_hits();
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = take_uv_mode_hits();
            let _ = crate::tile::take_inter_mode_hits();
            let _ = crate::tile::take_drl_hits();
            let _ = crate::tile::take_ref_hits();
            let _ = crate::tile::take_compound_mode_hits();
            let _ = crate::tile::take_compound_pair_hits();
            let _ = crate::tile::take_compound_size_hits();
            let _ = crate::tile::take_compound_leaf_mode_hits();
            let _ = crate::motion::take_census();
            // The entry-surface arm: run the SAME ladder through the
            // streaming facade the editor's export calls, which must print
            // the same BD numbers as the sequence path -- byte for byte the
            // same stream now that both drive one mini-GOP driver
            // (lane-av1pyrdef).
            let facade = std::env::var_os("EC_AV1_GATE_FACADE").is_some();
            let (ours, ours_wall) = match facade {
                false => our_ladder(name, &source, width, height, fctx),
                true => {
                    let pyramid = crate::encoder::Pyramid::from_env();
                    let (ladder, wall, counts, bytes, effective) =
                        our_ladder_facade(name, &source, width, height, pyramid);
                    eprintln!(
                        "{name}: facade, pyramid requested {pyramid:?} effective \
                         {effective:?} -- frames key {} arf {} leaf {} \
                         show_existing {}; bytes key {} arf {} leaf {} show_existing {}",
                        counts[0], counts[1], counts[2], counts[3],
                        bytes[0], bytes[1], bytes[2], bytes[3],
                    );
                    (ladder, wall)
                }
            };
            // How often the inter split actually fired over this clip's four
            // encodes, against the whole 32x32 that was the only outcome
            // before (gate-blind-to-feature).
            let [whole32, split32, whole16, split16] = take_partition_hits();
            // Which inter modes actually fired over this clip's four encodes,
            // and at which DRL index -- a mode nobody picks is a mode the BD
            // number cannot be crediting (gate-blind-to-feature).
            let [nearest, near, global, new] = crate::tile::take_inter_mode_hits();
            let [drl0, drl1, drl2, drl3] = crate::tile::take_drl_hits();
            let refs = crate::tile::take_ref_hits();
            let modes_total = (nearest + near + global + new).max(1);
            eprintln!(
                "{name}: inter modes NEAREST {nearest} ({:.1}%) NEAR {near} ({:.1}%) \
                 GLOBAL {global} ({:.1}%) NEW {new} ({:.1}%); drl idx 0/1/2/3+ \
                 {drl0}/{drl1}/{drl2}/{drl3}",
                100.0 * nearest as f64 / modes_total as f64,
                100.0 * near as f64 / modes_total as f64,
                100.0 * global as f64 / modes_total as f64,
                100.0 * new as f64 / modes_total as f64,
            );
            // The compound census: what share of the coded inter blocks took
            // a compound reference at all, which compound mode they took, and
            // which pair (gate-blind-to-feature -- a compound mode nobody
            // picks cannot be what a BD number credits).
            let comp_modes = crate::tile::take_compound_mode_hits();
            let comp_pairs = crate::tile::take_compound_pair_hits();
            let comp_sizes = crate::tile::take_compound_size_hits();
            let comp_leaf = crate::tile::take_compound_leaf_mode_hits();
            let comp_total: usize = comp_modes.iter().sum();
            eprintln!(
                "{name}: compound {comp_total} of {} inter blocks ({:.1}%); modes \
                 NEAREST_NEAREST {} NEAR_NEAR {} NEAREST_NEW {} NEW_NEAREST {} \
                 GLOBAL_GLOBAL {} NEW_NEW {}; pairs LAST+GOLDEN {} LAST+ALTREF {}",
                modes_total + comp_total,
                100.0 * comp_total as f64 / (modes_total + comp_total) as f64,
                comp_modes[0], comp_modes[1], comp_modes[2], comp_modes[3],
                comp_modes[6], comp_modes[7],
                comp_pairs[3], comp_pairs[6],
            );
            eprintln!(
                "{name}: leaf (16x16 and below) compound modes NEAREST_NEAREST {} \
                 NEAR_NEAR {} NEAREST_NEW {} NEW_NEAREST {} GLOBAL_GLOBAL {} NEW_NEW {}",
                comp_leaf[0], comp_leaf[1], comp_leaf[2], comp_leaf[3], comp_leaf[6],
                comp_leaf[7],
            );
            eprintln!(
                "{name}: compound by size 8x8 {} 16x16 {} 32x32 {}",
                comp_sizes[1], comp_sizes[2], comp_sizes[3],
            );
            // Where the wall-owning stage actually goes: how many candidate
            // evaluations (one `mc::predict` + SAD each) a search spends, how
            // many of them are subpel, and how often the winner is the seed
            // it started from -- the census the seeded search is judged by.
            let [calls, evals, subpel, rounds, near_pred, unmoved] = crate::motion::take_census();
            let per = |n: u64| n as f64 / calls.max(1) as f64;
            eprintln!(
                "{name}: motion search {calls} calls, {evals} candidate evals ({:.1}/call, \
                 {:.1}% of them subpel), {rounds} integer rounds ({:.2}/call); winner within \
                 +-1 pel of pred_mv {:.1}%, winner = starting seed {:.1}%",
                per(evals),
                100.0 * subpel as f64 / evals.max(1) as f64,
                per(rounds),
                100.0 * near_pred as f64 / calls.max(1) as f64,
                100.0 * unmoved as f64 / calls.max(1) as f64,
            );
            eprintln!(
                "{name}: references LAST {} GOLDEN {} (LAST2 {} LAST3 {} BWD {} ALT2 {} ALT {})",
                refs[0], refs[3], refs[1], refs[2], refs[4], refs[5], refs[6]
            );
            eprintln!(
                "{name}: inter 32x32 split into 16s {split32} of {} blocks ({:.1}%); \
                 16x16 leaves split into 8s {split16} of {} ({:.1}%)",
                whole32 + split32,
                100.0 * split32 as f64 / (whole32 + split32).max(1) as f64,
                whole16 + split16,
                100.0 * split16 as f64 / (whole16 + split16).max(1) as f64
            );
            // The deblocking level each of this clip's frames chose
            // (lane-av1filt): 0 everywhere would mean the filter never fires.
            let levels = crate::filter_search::take_chosen_levels();
            type Chosen = ((u8, u8), (u8, u8), (u8, u8));
            let mean = |f: fn(&Chosen) -> u8| {
                levels.iter().map(|l| f(l) as f64).sum::<f64>() / levels.len().max(1) as f64
            };
            eprintln!(
                "{name}: over {} frames -- deblock luma mean {:.1} chroma {:.1}; \
                 cdef luma pri {:.1}/sec {:.1}, chroma pri {:.1}/sec {:.1}",
                levels.len(),
                mean(|l| l.0.0),
                mean(|l| l.0.1),
                mean(|l| l.1.0),
                mean(|l| l.1.1),
                mean(|l| l.2.0),
                mean(|l| l.2.1),
            );
            // How many strength presets each frame's CDEF list carried and
            // how the 64x64 units spread over them (lane-av1lr): `bits 0`
            // everywhere would mean the per-unit choice never fires.
            let presets = crate::filter_search::take_cdef_presets();
            let mut by_bits = [0usize; 4];
            for (b, _) in &presets {
                by_bits[usize::from(*b)] += 1;
            }
            let units: usize = presets.iter().flat_map(|(_, c)| c).sum();
            let on_extra: usize = presets
                .iter()
                .flat_map(|(_, c)| c.iter().skip(1))
                .sum();
            eprintln!(
                "{name}: cdef_bits frames {}/{}/{}/{} (0/1/2/3); {} of {} 64x64 units \
                 on a non-default preset ({:.1}%)",
                by_bits[0],
                by_bits[1],
                by_bits[2],
                by_bits[3],
                on_extra,
                units,
                100.0 * on_extra as f64 / units.max(1) as f64
            );
            // How the 64x64 luma restoration units split between
            // RESTORE_NONE and Wiener (lane-av1lr).
            let (lr_none, lr_wiener) = crate::filter_search::take_lr_histogram();
            eprintln!(
                "{name}: lr units none={lr_none} wiener={lr_wiener} ({:.1}% restored)",
                100.0 * lr_wiener as f64 / (lr_none + lr_wiener).max(1) as f64
            );
            // Which chroma modes the search actually wins with, against the
            // unconditional DC_PRED that was the only outcome before
            // (gate-blind-to-feature).
            // Screen-content detection and the palette it turns on
            // (gate-blind-to-feature): how many frames set
            // `allow_screen_content_tools`, how many blocks took a palette,
            // and of what size. A film clip must print `screen frames on=0`.
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            let screen_off = SCREEN_FRAMES[0].load(std::sync::atomic::Ordering::Relaxed);
            // Which tables the SEARCH priced against, per frame arming
            // (`tile::arm_pricing_cdfs`): the default tables or the frame's
            // real starting ones. A film clip must show every inter arming on
            // the real side, a screen clip none of them.
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            eprintln!("{name}: pricer armings default={price_default} real={price_real}");
            let palette = crate::tile::take_palette_hits();
            eprintln!(
                "{name}: screen frames on={screen_on} off={screen_off}; palette blocks {} sizes {}",
                palette[0],
                (2..=8)
                    .filter(|&n| palette[n] > 0)
                    .map(|n| format!("{n}={}", palette[n]))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            // The CfL alpha histogram (lane-av1cfl): one entry per chroma
            // plane of every `UV_CFL_PRED` block, by |alpha_q3|. A tool that
            // only ever fires at one magnitude is a search that never left
            // its first candidate (gate-blind-to-feature).
            // The luma angle-delta histogram (lane-av1cfl): how many
            // directional blocks won at each delta. All-zero would mean the
            // refinement never beat the base angle.
            let deltas = take_angle_delta_hits();
            if deltas.iter().sum::<usize>() > 0 {
                eprintln!(
                    "{name}: angle_delta_y {}",
                    deltas
                        .iter()
                        .enumerate()
                        .map(|(d, &n)| format!("{:+}={n}", d as i32 - 3))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            let alphas = take_cfl_alpha_hits();
            if alphas.iter().sum::<usize>() > 0 {
                eprintln!(
                    "{name}: cfl |alpha| {}",
                    alphas
                        .iter()
                        .enumerate()
                        .filter(|&(_, &n)| n > 0)
                        .map(|(a, &n)| format!("{a}={n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            let palette_uv = crate::tile::take_palette_uv_hits();
            eprintln!(
                "{name}: chroma palette blocks {} sizes {}",
                palette_uv[0],
                (2..=8)
                    .filter(|&n| palette_uv[n] > 0)
                    .map(|n| format!("{n}={}", palette_uv[n]))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let uv = take_uv_mode_hits();
            let uv_total: usize = uv.iter().sum();
            eprintln!(
                "{name}: uv modes {}",
                uv.iter()
                    .enumerate()
                    .filter(|&(_, &n)| n > 0)
                    .map(|(m, &n)| format!(
                        "{}={n} ({:.1}%)",
                        UV_MODE_NAMES[m],
                        100.0 * n as f64 / uv_total.max(1) as f64
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            // Which transform depth the intra blocks of this clip's key
            // frames resolved to (lane-av1tx): depth 0 is one transform over
            // the whole block, 1 and 2 its halvings.
            let depths = take_tx_depth_hits();
            let depth_total: usize = depths.iter().sum();
            eprintln!(
                "{name}: intra tx depths {}",
                depths
                    .iter()
                    .enumerate()
                    .map(|(d, &n)| format!(
                        "{d}={n} ({:.1}%)",
                        100.0 * n as f64 / depth_total.max(1) as f64
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            print_inter_tx_census(name);
            // lane-av1obmc: how many blocks actually CODED a motion_mode
            // symbol and which value they took, by footprint -- "OBMC is on"
            // as a measurement rather than a claim (class
            // `gate-blind-to-feature`).
            let mm = crate::tile::take_motion_mode_hits();
            eprintln!(
                "{name}: motion_mode 32x32 SIMPLE={} OBMC={} WARP={} | 16x16 SIMPLE={} OBMC={} \
                 WARP={} | 8x8 SIMPLE={} OBMC={} WARP={} ({:.1}% OBMC / {:.1}% WARP of {} \
                 eligible)",
                mm[0], mm[1], mm[2], mm[3], mm[4], mm[5], mm[6], mm[7], mm[8],
                100.0 * (mm[1] + mm[4] + mm[7]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                100.0 * (mm[2] + mm[5] + mm[8]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                mm.iter().sum::<usize>(),
            );
            let (aom, aom_wall) =
                external_ladder(&source, width, height, "libaom-av1", &aom_points);
            let (rav1e, rav1e_wall) =
                external_ladder(&source, width, height, "librav1e", &rav1e_points);
            assert_monotone(&format!("{name}: ours"), &ours);
            assert_monotone(&format!("{name}: libaom"), &aom);
            assert_monotone(&format!("{name}: rav1e"), &rav1e);
            let points = ours
                .iter()
                .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "| {name} | {points} | {:+.1}% | {:+.1}% | {ours_wall:.1}s:{aom_wall:.1}s:{rav1e_wall:.1}s |",
                bd_rate(&aom, &ours) * 100.0,
                bd_rate(&rav1e, &ours) * 100.0,
            );
            println!(
                "|   references | libaom {} | rav1e {} | | |",
                aom.iter()
                    .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                    .collect::<Vec<_>>()
                    .join(", "),
                rav1e
                    .iter()
                    .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
    /// Which var-tx depth this clip's INTER blocks resolved to (lane-av1tx2,
    /// split by block class in lane-av1txdepth): depth 0 is one transform over
    /// the whole block, 1 its four halves; the classes are the 16x16/8x8
    /// leaves and the 32x32 root, each single-reference or compound. A class
    /// with no counts at all never reached `commit_inter_luma` -- which is
    /// what its knob being off looks like (class `gate-blind-to-feature`).
    fn print_inter_tx_census(name: &str) {
        let splits = take_inter_tx_split_hits();
        let total: usize = splits.iter().sum();
        let row = |label: &str, base: usize| {
            let (n0, n1) = (splits[base], splits[base + 1]);
            format!(
                "{label} 0={n0} 1={n1} ({:.1}% split)",
                100.0 * n1 as f64 / (n0 + n1).max(1) as f64
            )
        };
        eprintln!(
            "{name}: inter var-tx depths of {total} blocks -- {} | {} | {} | {}",
            row("leaf single", 0),
            row("leaf compound", 2),
            row("32x32 single", 4),
            row("32x32 compound", 6),
        );
    }

    /// The video stream's coded size, from ffprobe.
    fn probe_dims(clip: &str) -> Option<(usize, usize)> {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(clip)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut f = text.trim().split(',');
        Some((f.next()?.trim().parse().ok()?, f.next()?.trim().parse().ok()?))
    }

    /// The rows of [`bd_rate_screen_native`] -- (label, path, seek) -- shared
    /// with `probe_screen_detect` so the detector sweep measures exactly the
    /// clips the keep table is read off. Selector env vars as documented on
    /// the gate; a missing clip prints a SKIP and drops its row.
    fn native_gate_clips() -> Vec<(String, String, String)> {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let on = |k: &str| std::env::var(k).ok().as_deref() == Some("1");
        let (film, film4k, screen) = (
            on("EC_AV1_NATIVE_FILM"),
            on("EC_AV1_NATIVE_FILM4K"),
            on("EC_AV1_NATIVE_SCREEN"),
        );
        // No selector at all means every clip; any selector means that subset.
        let all = !(film || film4k || screen);
        // (label, path, seek). The two `testsrc2` fixtures are COLOUR BARS,
        // not film -- they print as "bars" so no reader takes them for real
        // content -- and the two rows below them are the real AV1 films.
        let mut clips: Vec<(String, String, String)> = Vec::new();
        for (want, label, file) in [
            (all || film, "bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            (all || film4k, "bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            if !want {
                continue;
            }
            let path = fixtures.join("video").join(file);
            match path.exists() {
                true => {
                    clips.push((label.to_string(), path.to_str().unwrap().to_string(), "0".into()))
                }
                false => eprintln!("SKIP {label}: {file} missing"),
            }
        }
        let manifest = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv"));
        // The real films, taken out of the real-library manifest by codec and
        // coded width so no title and no home path lives in this file. Each
        // seek is PINNED (class `seeded fixture not reproducible`) at a
        // moving, non-black window -- a black leader would make every arm
        // read +0.000 (class `flat sample window`). Both sources are 10-bit
        // (one HDR10/PQ); the loader's plain `-pix_fmt yuv420p` conversion is
        // what all three encoders see, so the comparison stays
        // encoder-vs-encoder.
        let pick = |width: &str| -> Option<String> {
            let m = manifest.as_ref().ok()?;
            m.lines()
                .skip(1)
                .map(|l| l.split('\t').collect::<Vec<_>>())
                .find(|f| {
                    f.len() > 4
                        && f[2] == "av1"
                        && f[3] == width
                        && std::path::Path::new(f[0]).exists()
                })
                .map(|f| f[0].to_string())
        };
        for (want, label, width, seek) in [
            (all || film, "film A (1080p source)", "1920", "00:35:00"),
            (all || film4k, "film B (2160p HDR source)", "3840", "00:40:00"),
        ] {
            if !want {
                continue;
            }
            match pick(width) {
                Some(path) => clips.push((label.to_string(), path, seek.to_string())),
                None => eprintln!("SKIP {label}: no {width}-wide AV1 source in the manifest"),
            }
        }
        if all || screen {
            match &manifest {
                Ok(manifest) => match manifest
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split('\t').next())
                    .find(|p| p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists())
                {
                    Some(p) => clips.push((
                        format!("screen capture ({})", p.rsplit('/').next().unwrap_or(p)),
                        p.to_string(),
                        "0".into(),
                    )),
                    None => eprintln!("SKIP screen capture: no OBS recording in the manifest exists"),
                },
                Err(e) => eprintln!("SKIP screen capture: no real-library manifest ({e})"),
            }
        }
        clips
    }

    /// The NATIVE-resolution arm of the BD gate (all three clips). The default gate
    /// downscales the OBS capture to 640x384, which destroys exactly the
    /// pixel-identical 16x16 repeats intrabc and (part of) the palette exist
    /// for (class `gate-recipe-confound`), so every screen-tool decision taken
    /// on that recipe is taken on content the tool cannot serve. This arm
    /// crops -- never scales -- a superblock-aligned window out of the middle
    /// of the capture at its own resolution (at most 1920x1024, so the wall
    /// stays in the minutes) and runs the same 12-frame, four-quantizer
    /// ladder against libaom `cpu-used 6` and rav1e `speed=6`, one tile, one
    /// thread, with the same three-way (encoder / ffmpeg / our decoder)
    /// sample-exactness assertion `our_ladder` makes.
    ///
    /// It is its own `--ignored` test so the default gate's wall does not
    /// grow. Every variant is an environment knob, and each of those is a
    /// `OnceLock` read once per process, so a variant is a separate run:
    ///
    ///     cargo test -p ec-av1 --release --lib -- --ignored \
    ///         bd_rate_screen_native --nocapture           # baseline
    ///     EC_AV1_INTRABC=1 ... same command                # intra block copy
    ///     EC_AV1_PAL_MAXCOLORS=256 ... same command        # palette bound
    ///     EC_AV1_TILES=1:1 EC_AV1_TILE_THREADS=4 ...       # 2x2 tiles
    ///     EC_AV1_PYRAMID=0 ... same command                # no coding pyramid
    ///     EC_AV1_GATE_FACADE=1 ... same command            # the facade arm
    ///
    /// The coding pyramid is the DEFAULT of the sequence path this arm codes
    /// through (lane-av1pyrdef); the run prints the requested and the
    /// effective pyramid per clip, and a screen clip's effective one is
    /// `None`. `EC_AV1_GATE_FACADE=1` runs the same ladder through the
    /// streaming facade instead, which prints the per-level frame and byte
    /// census and asserts ffmpeg's decode against our own decoder on all
    /// three planes in display order; both arms code the same bytes
    /// (`encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence`).
    ///
    /// WHICH ROWS ARE REAL CONTENT: the `bars 1080p` / `bars 2160p` rows are
    /// the repo fixtures `scripts/gen-fixtures.sh` builds with ffmpeg
    /// `testsrc2` -- COLOUR BARS, ~90% of whose 16x16 cells have no intra
    /// cost -- so every "film" column in the historical tables of this file
    /// is that generator, not film. The real content is `film A`, `film B`
    /// (native crops out of the two AV1 films the real-library manifest
    /// names, at a pinned seek; added 2026-09-07) and the screen capture. A
    /// decision only the bars rows support is not supported.
    ///
    /// Measured 2026-09-07, the five rows before and after the screen
    /// detector grew libaom's variance term ([`screen_var`]) -- before it,
    /// BOTH real films were classified as screen content on 48/48 frames, so
    /// their coefficients were priced against the DEFAULT CDFs and the
    /// palette search ran over film; after, both are non-screen (0/48) and
    /// the capture is unchanged at 48/48:
    ///
    /// | clip | vs libaom before -> after | vs rav1e before -> after | wall ours |
    /// |---|---|---|---|
    /// | bars 1080p | +15.6% -> +15.6% | -1.7% -> -1.7% | 37.9s |
    /// | bars 2160p | +46.5% -> +46.5% | +18.8% -> +18.8% | 33.9s |
    /// | film A | +69.5% -> +69.0% | +37.7% -> +37.5% | 72s -> 64.4s |
    /// | film B | +102.4% -> +100.0% | +63.4% -> +61.5% | 72s -> 67.9s |
    /// | screen capture | +51.0% -> +51.0% | -14.5% -> -14.5% | 27.5s |
    ///
    /// Both bars rows and the capture are byte-identical across that change
    /// (their classification never moved); the palette search over film was
    /// costing roughly a tenth of the film rows' wall.
    ///
    /// CONFIRM RUN 2026-09-07 (lane-av1pyrdef), with the pyramid now the
    /// sequence path's default -- the four non-screen rows take `4:-16:8`,
    /// the capture is gated flat and does not move a byte:
    ///
    /// | clip | effective | flat -> default | vs rav1e | wall ours:libaom:rav1e |
    /// |---|---|---|---|---|
    /// | bars 1080p | 4:-16:8 | +16.4% -> +21.5% | -1.1% -> +3.7% | 37.8s:8.4s:16.4s |
    /// | bars 2160p | 4:-16:8 | +48.2% -> +48.3% | +20.4% -> +20.4% | 32.2s:5.6s:16.2s |
    /// | film A | 4:-16:8 | +62.8% -> +54.1% | +32.5% -> +24.2% | 58.9s:9.5s:13.6s |
    /// | film B | 4:-16:8 | +86.3% -> +82.6% | +50.5% -> +47.4% | 63.2s:18.7s:16.4s |
    /// | screen capture | none (gated) | +50.1% -> +50.1% | -15.4% -> -15.4% | 31.1s:8.4s:11.3s |
    ///
    /// Every row lands on lane-av1pyrgate's facade-arm number to the digit,
    /// which is the point of the shared driver: the two entry points code one
    /// stream. The bars rows are fixtures (recorded, never a decision); the
    /// two real films are what the pyramid ships for.
    ///
    /// All five gate clips are rows by default. `EC_AV1_NATIVE_FILM=1`,
    /// `EC_AV1_NATIVE_FILM4K=1` and `EC_AV1_NATIVE_SCREEN=1` select a subset:
    /// setting any one of them keeps only the selected rows (the two 1080p
    /// rows, the two 2160p rows, the capture row respectively). A missing
    /// real film SKIPs its row; it never fails the gate.
    #[test]
    #[ignore = "the native-resolution BD arm: minutes per row, needs ffmpeg"]
    fn bd_rate_screen_native() {
        if !have_ffmpeg() {
            eprintln!("SKIP bd_rate_screen_native: no ffmpeg");
            return;
        }
        let clips = native_gate_clips();
        if clips.is_empty() {
            eprintln!("SKIP bd_rate_screen_native: no clip");
            return;
        }

        let frames = 12usize;
        let aom_points: Vec<Vec<String>> = [5, 20, 35, 45]
            .iter()
            .map(|q| {
                ["-cpu-used", "6", "-b:v", "0", "-crf", &q.to_string()]
                    .map(String::from)
                    .to_vec()
            })
            .collect();
        let rav1e_points: Vec<Vec<String>> = [50, 100, 150, 200]
            .iter()
            .map(|q| {
                vec![
                    "-rav1e-params".to_string(),
                    format!("speed=6:quantizer={q}:tile_cols=1:tile_rows=1:threads=1"),
                ]
            })
            .collect();
        println!(
            "\nnative crop, {frames} frames, gop={frames}; tiles {:?}, tile threads {}, \
             intrabc {}, palette maxcolors {}",
            armed_tiles(),
            crate::par::tile_threads(),
            intrabc_enabled(),
            palette_max_colors(),
        );
        println!("| clip | ours PSNR/bytes per point | BD-rate vs libaom | BD-rate vs rav1e | wall ours:libaom:rav1e (noisy) |");
        println!("|---|---|---|---|---|");
        for (name, path, seek) in &clips {
            let fctx = &crate::decode::FrameCtx::new();
            let Some((nw, nh)) = probe_dims(path) else {
                eprintln!("SKIP {name}: ffprobe gave no size");
                continue;
            };
            // A whole number of 128-wide superblocks, at most 1920x1024, out
            // of the middle of the frame; the offsets stay even for 4:2:0.
            let cw = nw.min(1920) / 128 * 128;
            let ch = nh.min(1024) / 128 * 128;
            assert!(cw >= 128 && ch >= 128, "{name}: {nw}x{nh} is smaller than a superblock");
            let (x, y) = ((nw - cw) / 2 & !1, (nh - ch) / 2 & !1);
            let source = clip_frames_vf(
                path,
                seek,
                &format!("crop={cw}:{ch}:{x}:{y}"),
                cw,
                ch,
                frames,
            );
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = crate::tile::take_palette_hits();
            let _ = crate::tile::take_palette_uv_hits();
            let _ = crate::tile::take_intrabc_hits();
            let _ = take_intrabc_search();
            let _ = crate::tile::take_filter_intra_hits();
            // The sequence path itself codes the pyramid now
            // (lane-av1pyrdef), and prints what it requested and what the
            // content gate left effective.
            let (ours, ours_wall) = our_ladder(name, &source, cw, ch, fctx);
            // gate-blind-to-feature: what the screen tools actually did at
            // native resolution, over this clip's four encodes.
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            let screen_off = SCREEN_FRAMES[0].load(std::sync::atomic::Ordering::Relaxed);
            // Which tables the SEARCH priced against, per frame arming
            // (`tile::arm_pricing_cdfs`): the default tables or the frame's
            // real starting ones. A film clip must show every inter arming on
            // the real side, a screen clip none of them.
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            eprintln!("{name}: pricer armings default={price_default} real={price_real}");
            // lane-av1obmc2: the same motion_mode census the 640x384 gate
            // prints (class `gate-blind-to-feature`) -- how many blocks coded
            // the symbol at each footprint and how many took OBMC. Without it
            // the native table, which is the standing keep table, could not
            // say whether the tool fired at all.
            let mm = crate::tile::take_motion_mode_hits();
            eprintln!(
                "{name}: motion_mode 32x32 SIMPLE={} OBMC={} WARP={} | 16x16 SIMPLE={} OBMC={} \
                 WARP={} | 8x8 SIMPLE={} OBMC={} WARP={} ({:.1}% OBMC / {:.1}% WARP of {} \
                 eligible)",
                mm[0], mm[1], mm[2], mm[3], mm[4], mm[5], mm[6], mm[7], mm[8],
                100.0 * (mm[1] + mm[4] + mm[7]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                100.0 * (mm[2] + mm[5] + mm[8]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                mm.iter().sum::<usize>(),
            );
            print_inter_tx_census(name);
            let palette = crate::tile::take_palette_hits();
            let palette_uv = crate::tile::take_palette_uv_hits();
            let ibc = crate::tile::take_intrabc_hits();
            let search = take_intrabc_search();
            eprintln!(
                "{name}: screen frames on={screen_on} off={screen_off} ({:.1}% on); \
                 palette blocks luma {} chroma {}; intrabc blocks {} (searched {} / valid {} / \
                 won {})",
                100.0 * screen_on as f64 / (screen_on + screen_off).max(1) as f64,
                palette[0],
                palette_uv[0],
                ibc[0],
                search[0],
                search[1],
                search[2],
            );
            // gate-blind-to-feature: how often the five recursive filter-intra
            // modes actually won a block over this clip's four encodes -- a
            // tool that never fires is not measured by the row above it.
            let fi = crate::tile::take_filter_intra_hits();
            eprintln!(
                "{name}: filter intra on={} blocks {} (modes DC={} V={} H={} D157={} PAETH={})",
                filter_intra_on(),
                fi[0],
                fi[1],
                fi[2],
                fi[3],
                fi[4],
                fi[5],
            );
            let (aom, aom_wall) = external_ladder(&source, cw, ch, "libaom-av1", &aom_points);
            let (rav1e, rav1e_wall) = external_ladder(&source, cw, ch, "librav1e", &rav1e_points);
            assert_monotone(&format!("{name}: ours"), &ours);
            assert_monotone(&format!("{name}: libaom"), &aom);
            assert_monotone(&format!("{name}: rav1e"), &rav1e);
            let points = ours
                .iter()
                .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "| {name} {cw}x{ch} | {points} | {:+.1}% | {:+.1}% | {ours_wall:.1}s:{aom_wall:.1}s:{rav1e_wall:.1}s |",
                bd_rate(&aom, &ours) * 100.0,
                bd_rate(&rav1e, &ours) * 100.0,
            );
        }
    }

    /// lane-av1tpl3 step 1: the SHAPE of the propagating map's denominator
    /// ([`super::tpl_intra_costs`], the per-16x16 MAD from the cell's own DC)
    /// on the two film clips at their native gate crop. The 2160p clip loses
    /// at every arm of the propagating map and the named suspect was that its
    /// heavily filtered flat cells give a tiny denominator, so the dependence
    /// ratio explodes exactly where coding is cheap; this prints the numbers
    /// that confirm or refute it.
    #[test]
    #[ignore = "needs ffmpeg and the film fixtures"]
    fn tpl_intra_denominator_histogram() {
        if !have_ffmpeg() {
            eprintln!("SKIP tpl_intra_denominator_histogram: no ffmpeg");
            return;
        }
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        for (label, file) in [
            ("bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            ("bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            let path = fixtures.join("video").join(file);
            if !path.exists() {
                eprintln!("SKIP {label}: {file} missing");
                continue;
            }
            let path = path.to_str().unwrap();
            let Some((nw, nh)) = probe_dims(path) else {
                eprintln!("SKIP {label}: ffprobe gave no size");
                continue;
            };
            let (cw, ch) = (nw.min(1920) / 128 * 128, nh.min(1024) / 128 * 128);
            let (x, y) = ((nw - cw) / 2 & !1, (nh - ch) / 2 & !1);
            let source =
                clip_frames_vf(path, "0", &format!("crop={cw}:{ch}:{x}:{y}"), cw, ch, 8);
            let mut costs: Vec<f64> = Vec::new();
            for pic in &source {
                let y8: Vec<u8> = pic.y.iter().map(|&v| v as u8).collect();
                costs.extend(
                    super::tpl_intra_costs(&y8, cw, ch).iter().map(|&c| f64::from(c)),
                );
            }
            let mean = costs.iter().sum::<f64>() / costs.len() as f64;
            let mut sorted = costs.clone();
            sorted.sort_by(f64::total_cmp);
            let q = |f: f64| sorted[((sorted.len() - 1) as f64 * f) as usize] / mean;
            let below = |f: f64| costs.iter().filter(|&&c| c < f * mean).count();
            eprintln!(
                "{label} {cw}x{ch}: {} cells over 8 frames, mean intra MAD {mean:.0}; \
                 as a fraction of the mean p0={:.3} p1={:.3} p5={:.3} p25={:.3} p50={:.3} \
                 p75={:.3} p99={:.3} max={:.3}; below 0.25x {} ({:.1}%), below 0.5x {} ({:.1}%), \
                 below 1.0x {} ({:.1}%)",
                costs.len(),
                q(0.0),
                q(0.01),
                q(0.05),
                q(0.25),
                q(0.50),
                q(0.75),
                q(0.99),
                q(1.0),
                below(0.25),
                100.0 * below(0.25) as f64 / costs.len() as f64,
                below(0.5),
                100.0 * below(0.5) as f64 / costs.len() as f64,
                below(1.0),
                100.0 * below(1.0) as f64 / costs.len() as f64,
            );
        }
    }
}
