//! The streaming encoder facade — I11 of `lanes/av1-inter-plan.md`, the
//! surface `edith_replica`'s engine calls in place of rav1e.
//!
//! One picture in, one [`Packet`] out, same contract as the repo's other
//! software encoder shims (`shims/rusty_h264`'s `Encoder::try_encode`,
//! "nothing is ever held back"): [`Av1Encoder::encode`] never buffers a
//! picture past its own call, so there is no flush to drain. That contract
//! still holds for every caller that does not opt into a coding pyramid;
//! [`Av1Encoder::with_pyramid`] does reorder pictures, and such a stream is
//! driven through [`Av1Encoder::encode_frames`] (zero or more packets per
//! picture) and [`Av1Encoder::flush`] instead. Internally it
//! is [`crate::encode::encode_key_frame_inner`] and
//! [`crate::encode::encode_inter_frame`] driven by a small state machine —
//! a key frame every `gop` pictures, an inter frame predicting from the
//! previous picture's own reconstruction otherwise — which is the same core
//! [`crate::encode::encode_sequence`] uses for its one-key-frame-then-all-inter
//! case; both now share it rather than each padding/cropping/refreshing the
//! reference on their own.

use ec_av1_syntax::sequence::{ChromaSamplePosition, ColorConfig};
use ec_core::{Error, Result};

use crate::encode::{
    Encoded, GOLDEN_SLOT, Picture, SUPERBLOCK, crop_encoded, encode_inter_frame,
    encode_key_frame_inner, split_blocks,
};
use crate::intra::KEY_FRAME_MODES;

/// A quantizer step round to nearest: half a step either way costs the same
/// rate, so this is the deadzone every facade-driven frame is coded with.
/// (`crate::encode`'s public entry points take a caller-chosen deadzone
/// instead; the facade has no field for it because nothing downstream of it
/// picks one.)
const DEADZONE: f64 = 0.5;

/// The colour a played-back frame is transformed by, named at what a
/// container/player picks a colour space from (spec 5.5.2's CICP triple plus
/// range) rather than at the raw integers. `edith_replica`'s rav1e seat sets
/// limited-range BT.709 or BT.601 (`export.rs:3494-3507`); this crate's
/// pre-facade sequence header always wrote [`Colour::Unspecified`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Colour {
    /// `color_primaries`/`transfer_characteristics`/`matrix_coefficients` =
    /// 1 (BT.709, H.273), studio range: consumer HD video's own space, and
    /// the default a player assumes when nothing is signalled at all.
    #[default]
    Bt709Limited,
    /// The same three fields = 6 (BT.601, H.273), studio range: SD video's
    /// space.
    Bt601Limited,
    /// Every field = 2 ("unspecified", H.273): what this crate wrote before
    /// any of this was configurable.
    Unspecified,
}

impl Colour {
    fn color_config(self) -> ColorConfig {
        let (color_primaries, transfer_characteristics, matrix_coefficients) = match self {
            Colour::Bt709Limited => (1, 1, 1),
            Colour::Bt601Limited => (6, 6, 6),
            Colour::Unspecified => (2, 2, 2),
        };
        ColorConfig {
            bit_depth: 8,
            mono_chrome: false,
            num_planes: 3,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            // Every variant here is studio (limited) swing; a full-range
            // variant would need its own name, not a fourth field on this
            // enum, since a played-back frame's range is a property of the
            // pictures fed in, not just a label.
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
            chroma_sample_position: ChromaSamplePosition::Unknown,
            separate_uv_delta_q: false,
        }
    }
}

/// A quality/size targeting surface for callers who would rather not pick a
/// `base_q_idx` themselves, oracled at CRF's own shape (a single "quality"
/// dial that holds a stable perceptual level across content, `ffmpeg -crf`)
/// since a from-scratch scheme is not this lane's charter to invent.
/// [`Av1Encoder::with_rate_target`] is additive: [`EncoderConfig::base_q_idx`]
/// keeps working exactly as before for every existing caller, this is a
/// second, opt-in constructor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateTarget {
    /// Exactly `base_q_idx`, unchanged — the `EncoderConfig` field's own
    /// value, for a caller that already has one.
    QIndex(u8),
    /// A CRF-like single dial, 0 (smallest/worst) to 100 (largest/best),
    /// mapped to a fixed `base_q_idx` by the calibration in
    /// `encode::tests::calibration_sweep_base_q_idx` (linear in `q_idx`
    /// across the sweep's own 40..=240 span, since the measured bytes/PSNR
    /// curve is close enough to log-linear there that a straight line is
    /// the "simplest correct" fit this lane's charter asks for). No
    /// per-frame feedback: one value, picked once at construction.
    Quality(u8),
    /// A closed loop that steers `base_q_idx` frame by frame to land each
    /// coded frame near this many bytes, via [`RateLoop`] — see there for
    /// the controller and its windup bound.
    BytesPerFrame(u32),
    /// A target BITRATE: the same closed loop, with the per-frame byte
    /// target derived from `bits_per_second` and `frames_per_second`
    /// (`bits_per_second / 8 / frames_per_second`), and — in pyramid mode —
    /// split across the levels by their reference weight, since a hidden
    /// `ALTREF` and a leaf differ by several times in size and one shared
    /// predictor would swing the quantizer every frame.
    Bitrate {
        /// The stream's target bitrate.
        bits_per_second: u32,
        /// The frame rate the bitrate is spent at.
        frames_per_second: f64,
    },
}

impl RateTarget {
    /// Applies this target to `config` and returns the closed loop it needs
    /// (`None` for the two open-loop variants). `mini_gop` is `Some` in
    /// pyramid mode, which is what makes the loop keep a quantizer per level.
    fn into_loop(self, config: &mut EncoderConfig, mini_gop: Option<usize>) -> Option<RateLoop> {
        let target_bytes = match self {
            RateTarget::QIndex(q) => {
                config.base_q_idx = q;
                return None;
            }
            RateTarget::Quality(quality) => {
                config.base_q_idx = quality_to_q_idx(quality);
                return None;
            }
            RateTarget::BytesPerFrame(target_bytes) => f64::from(target_bytes),
            RateTarget::Bitrate {
                bits_per_second,
                frames_per_second,
            } => f64::from(bits_per_second) / 8.0 / frames_per_second.max(1.0),
        };
        Some(RateLoop::new(
            target_bytes,
            f64::from(config.base_q_idx),
            mini_gop,
        ))
    }
}

/// `quality`'s mapping to `base_q_idx`, linear across the calibration sweep's
/// span (`q_idx` 40 at `quality` 100 down to `q_idx` 240 at `quality` 0);
/// `quality` above 100 clamps to the best point measured, matching a CRF
/// dial's own saturating ends rather than extrapolating past calibrated
/// data.
fn quality_to_q_idx(quality: u8) -> u8 {
    let quality = f64::from(quality.min(100));
    (240.0 - quality * 2.0).round() as u8
}

/// How much of a mini-GOP's byte budget the hidden `ALTREF` frame is worth
/// relative to one leaf: every leaf of the group predicts off it, so it is
/// bought at roughly twice a leaf's size (libaom's `gf_group` spends its
/// `ARF` the same way, through a lower quantizer rather than a byte target).
const ARF_WEIGHT: f64 = 2.0;

/// How much a key frame is worth relative to one leaf, same units.
const KEY_WEIGHT: f64 = 3.0;

/// The closed-loop controller behind [`RateTarget::BytesPerFrame`] and
/// [`RateTarget::Bitrate`]: a proportional step on `base_q_idx`, sized from
/// the calibration sweep's own log-linear slope (bytes roughly halve every
/// ~45 `q_idx` steps there, i.e. `d(ln bytes)/d(q_idx) ≈ -0.0154`), clamped
/// per frame. Pure proportional — no accumulated error term — is the windup
/// bound itself ([[vorbis-rate-loop-windup]]'s class): the only state carried
/// between frames is `q`, already clamped to `0..=255`, so a quiet lead-in
/// cannot build a debt a later transient has to repay; each frame's step is
/// bounded by `STEP_CLAMP` regardless of history.
///
/// In pyramid mode the state is kept PER LEVEL: each frame's size is
/// predicted from the previous frame of its own level, against that level's
/// own share of the budget, which is the whole "two-pass-free model" this
/// encoder carries.
#[derive(Debug, Clone, Copy)]
struct RateLoop {
    /// Byte target per level, indexed by [`RateLoop::slot`].
    target: [f64; 3],
    /// Quantizer per level, same index.
    q: [f64; 3],
    /// Whether the three slots are actually distinct (pyramid mode) or all
    /// one shared controller (the flat path, unchanged from before levels
    /// existed).
    per_level: bool,
}

impl RateLoop {
    /// The steepest a single frame is allowed to move `base_q_idx`, in
    /// either direction — chosen so one wildly over/under-sized frame
    /// (a scene cut, a black lead-in) nudges the next frame's quantizer
    /// rather than slamming it to an extreme.
    const STEP_CLAMP: f64 = 12.0;
    /// The calibration sweep's own slope (see the struct doc), inverted to
    /// convert a bytes ratio into a `q_idx` step.
    const GAIN: f64 = 1.0 / 0.0154;

    fn new(target_bytes: f64, start_q: f64, mini_gop: Option<usize>) -> Self {
        let Some(m) = mini_gop.map(|m| m as f64) else {
            return Self {
                target: [target_bytes; 3],
                q: [start_q; 3],
                per_level: false,
            };
        };
        // A group of `m` pictures gets `m * target_bytes`, split one
        // `ARF_WEIGHT` share to the hidden frame and one each to the `m - 1`
        // leaves, so the group's own average is the target exactly.
        let share = target_bytes * m / (m - 1.0 + ARF_WEIGHT);
        Self {
            target: [target_bytes * KEY_WEIGHT, share * ARF_WEIGHT, share],
            q: [start_q; 3],
            per_level: true,
        }
    }

    /// Which controller a level uses: one shared slot on the flat path, three
    /// separate ones under a pyramid.
    fn slot(&self, level: Level) -> usize {
        if !self.per_level {
            return 0;
        }
        match level {
            Level::Key => 0,
            Level::Arf => 1,
            // A `show_existing_frame` packet codes no pixels and never
            // reaches `update`; it shares the leaf slot so `q_idx` is total.
            Level::Leaf | Level::ShowExisting => 2,
        }
    }

    fn q_idx(&self, level: Level) -> u8 {
        self.q[self.slot(level)].round().clamp(0.0, 255.0) as u8
    }

    /// Steers this level's `q` toward its own target from the frame's actual
    /// coded size.
    fn update(&mut self, level: Level, actual_bytes: usize) {
        let slot = self.slot(level);
        if self.target[slot] <= 0.0 || actual_bytes == 0 {
            return;
        }
        let ratio = actual_bytes as f64 / self.target[slot];
        let step = (ratio.ln() * Self::GAIN).clamp(-Self::STEP_CLAMP, Self::STEP_CLAMP);
        self.q[slot] = (self.q[slot] + step).clamp(0.0, 255.0);
    }
}

/// What [`Av1Encoder::new`] takes: geometry, rate, key-frame cadence and
/// colour. `base_q_idx` is picked directly (0..=255, `crate::encode`'s own
/// unit) rather than a bitrate — a bitrate control loop is not part of this
/// lane's charter, and a caller that wants one can still derive a
/// `base_q_idx` from its own target and set that field. A caller who wants
/// [`RateTarget`]'s quality/size dials instead of picking `base_q_idx`
/// itself uses [`Av1Encoder::with_rate_target`], which still starts from an
/// `EncoderConfig` (its `base_q_idx` is the loop's seed/fallback for
/// [`RateTarget::BytesPerFrame`], and is overwritten outright for the other
/// two variants).
#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    /// The picture's width in luma samples; must be even and nonzero.
    pub width: usize,
    /// The picture's height in luma samples; must be even and nonzero.
    pub height: usize,
    /// The quantizer index every frame is coded at (0..=255).
    pub base_q_idx: u8,
    /// Pictures between key frames, inclusive of the key frame itself: `1`
    /// codes every picture as a key frame, `gop` codes picture `gop` (and
    /// `2*gop`, ...) as a key frame and every other picture inter.
    pub gop: usize,
    /// The colour the sequence header signals.
    pub colour: Colour,
    /// `TileColsLog2` (spec 5.9.15): the frame is split into `1 <<
    /// tile_cols_log2` uniformly spaced tile columns, each coded and
    /// decodable on its own. `0` -- one tile column -- is the default and
    /// is byte for byte the stream this encoder wrote before tiles existed.
    pub tile_cols_log2: u32,
    /// `TileRowsLog2`, the same down the rows.
    pub tile_rows_log2: u32,
}

impl EncoderConfig {
    /// A config at `width`x`height` with one tile, `gop` pictures between
    /// key frames and no colour signalling -- what every caller that does
    /// not care about tiles writes, so that adding a field here does not
    /// rewrite them.
    #[must_use]
    pub fn new(width: usize, height: usize, base_q_idx: u8, gop: usize, colour: Colour) -> Self {
        Self {
            width,
            height,
            base_q_idx,
            gop,
            colour,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        }
    }
}

/// One coded picture: its OBUs, in the order a demuxer that just wants bytes
/// on the wire can concatenate them, and the two things a muxer needs to
/// know without parsing them back out.
#[derive(Debug, Clone)]
pub struct Packet {
    /// A temporal delimiter, a sequence header and the frame OBU for a key
    /// frame; a temporal delimiter and the frame OBU alone for an inter
    /// frame (the sequence header is identical across a stream's key
    /// frames, since only [`EncoderConfig`]'s own fields feed it, so it is
    /// simplest to let every key frame carry its own copy rather than have
    /// the facade special-case the first one).
    pub data: Vec<u8>,
    /// Whether this picture was coded as a key frame.
    pub key: bool,
    /// This picture's position in DISPLAY (presentation) order, starting at
    /// 0 — a muxer's `pts` in picture units. Without a [`Pyramid`] this is
    /// also the coding order and equals `dts`.
    pub order: u64,
    /// This packet's position in CODING (decode) order, starting at 0 — a
    /// muxer's `dts` in packet units. With a [`Pyramid`] the hidden frame of
    /// each mini-GOP is coded before the leaves it precedes in display order,
    /// so `dts` and `order` diverge, and the `show_existing_frame` packet
    /// that re-outputs the hidden frame carries the hidden frame's own
    /// `order` with its own later `dts`.
    pub dts: u64,
    /// Which pyramid level this packet is.
    pub level: Level,
}

/// One mini-GOP of a two-level coding pyramid, and the quantizer offsets its
/// two levels are coded at. libaom's `gf_group` shape, cut down to the two
/// levels this encoder's reference set can carry: the last picture of each
/// mini-GOP is coded FIRST, hidden (`show_frame == 0`), as the group's
/// `ALTREF_FRAME`, then the pictures before it are coded as shown leaves that
/// may predict backward off it, and finally a `show_existing_frame` header
/// re-outputs the hidden frame in its own display position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pyramid {
    /// Pictures per mini-GOP: the hidden frame is `mini_gop` pictures ahead
    /// of the group's anchor, and `mini_gop - 1` leaves sit between them.
    /// `1` would leave no leaf at all and is refused.
    pub mini_gop: usize,
    /// Added to `base_q_idx` for the hidden `ALTREF_FRAME` — negative, since
    /// every leaf of the group predicts off it.
    pub arf_q_offset: i16,
    /// Added to `base_q_idx` for each shown leaf — positive, since nothing
    /// predicts off a leaf but the next leaf.
    pub leaf_q_offset: i16,
}

impl Default for Pyramid {
    /// The best point of the offset sweep run through the BD gate's own
    /// `EC_AV1_PYRAMID=<mini_gop>[:<arf_q_offset>:<leaf_q_offset>]` knob
    /// (`encode::tests::bd_rate_vs_libaom_and_rav1e`, 12 frames of each of
    /// the three gate clips, BD-rate vs libaom / vs rav1e, wall = the four
    /// encodes of the ladder together).
    ///
    /// First sweep, before the motion-search lane merged (its extra-reference
    /// `NEWMV` is exactly what a pyramid leaf needs to reach its hidden
    /// frame), so read it for the OFFSETS, not for the verdict:
    ///
    /// | mini_gop:arf:leaf | 1080p | 2160p | screen |
    /// |---|---|---|---|
    /// | flat (no pyramid) | +122.7 / +73.4 | +147.3 / +98.5 | +79.4 / +17.2 |
    /// | 4:0:0 | +152.2 / +97.9 | +214.7 / +158.1 | +91.4 / +26.2 |
    /// | 4:-16:8 | +139.9 / +87.1 | +201.5 / +143.7 | +78.5 / +19.5 |
    /// | 4:-24:12 | +141.3 / +85.0 | +190.0 / +132.9 | +84.5 / +22.8 |
    /// | 2:0:0 | +149.3 / +93.1 | +203.0 / +149.4 | +88.3 / +23.6 |
    /// | 2:-8:4 | +143.1 / +89.1 | +196.2 / +141.6 | +82.5 / +20.4 |
    /// | 2:-16:8 | +137.6 / +84.5 | +187.2 / +130.6 | +84.9 / +23.1 |
    ///
    /// `0:0` is the worst row at both mini-GOP sizes and `-16/+8` buys 12-16
    /// BD points over it, in the direction libaom's `gf_group` spends them
    /// (lowest quantizer on the frame everything else predicts from). Those
    /// are these defaults.
    ///
    /// Re-measured after that lane merged (wall ours, 1080p/2160p/screen):
    ///
    /// | arm | 1080p | 2160p | screen | wall |
    /// |---|---|---|---|---|
    /// | flat (no pyramid) | +121.1 / +71.8 | +146.1 / +97.5 | +79.3 / +17.2 | 6.3 / 5.8 / 6.2 s |
    /// | 4:-16:8 | +131.9 / +81.1 | +178.7 / +123.6 | +78.4 / +19.5 | 6.2 / 5.6 / 5.5 s |
    /// | 2:-16:8 | +125.4 / +76.2 | +173.0 / +119.5 | +84.9 / +23.1 | 6.2 / 5.6 / 6.2 s |
    ///
    /// So the pyramid still does NOT pay, and ships OFF: it wins 0.9 points
    /// on screen capture at `4:-16:8` and loses 11 on 1080p and 33 on 2160p.
    /// What keeps it from paying, in the order the numbers point at it:
    ///
    ///   * The hidden frame predicts `mini_gop` pictures ahead of its own
    ///     reference with the SAME forward search a neighbouring frame gets,
    ///     and it is the largest frame in the group (73.8 kB of the 1080p
    ///     ladder's 184 kB at `4:-16:8`). On the 2160p clip, whose motion the
    ///     search only just reaches at distance 1, distance 4 is where it
    ///     falls off — that clip loses twice what 1080p does, and shrinking
    ///     the mini-GOP to 2 recovers most of the gap (+178.7 -> +173.0,
    ///     +131.9 -> +125.4). A distance-scaled search range is the fix, and
    ///     it lives in the motion-search lane, not here.
    ///   * The leaves reach the hidden frame through the extra-reference
    ///     modes only: `ALTREF_FRAME` wins about 13% of blocks
    ///     (`encode::take_ref_frame_hits`, printed by the round-trip test),
    ///     up from 4% before the motion lane merged. A compound
    ///     (`LAST` + `ALTREF`) prediction mode is the other half of what a
    ///     real B frame buys, and this crate codes no compound blocks at all.
    ///
    /// Neither is a defect in the pyramid itself: the reordering is exact
    /// (`encoder::tests::a_pyramid_stream_decodes_in_display_order_through_both_decoders`
    /// checks every shown frame in display order against ffmpeg AND our own
    /// decoder, with the hidden frame proven to win blocks), and the
    /// quantizer offsets behave. Nothing selects the pyramid but an explicit
    /// [`Av1Encoder::with_pyramid`] call (or `EC_AV1_PYRAMID` on the gate and
    /// on `ec-bench`); the default one-in-one-out path is byte-identical to
    /// before it existed.
    fn default() -> Self {
        Self {
            mini_gop: 4,
            arf_q_offset: -16,
            leaf_q_offset: 8,
        }
    }
}

/// Which level of the pyramid a coded frame sits at, reported on every
/// [`Packet`] so a caller (and the BD gate's histogram) can tell them apart
/// without parsing the frame headers back out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A key frame.
    Key,
    /// A hidden `ALTREF_FRAME`, coded ahead of its display position.
    Arf,
    /// A shown leaf.
    Leaf,
    /// A `show_existing_frame` header re-outputting a hidden frame: no coded
    /// pixels at all, four bits of header.
    ShowExisting,
}

/// The DPB slot the group anchors alternate between (see
/// [`Av1Encoder::encode_frames`]): the hidden frame of group `g` refreshes
/// `ANCHOR_SLOTS[(g + 1) % 2]`, which is the slot the leaves of group `g`
/// name as `ALTREF_FRAME` and which group `g + 1` reads as its own anchor.
/// Two slots alternate because a group's leaves must still be able to read
/// the previous anchor while the new one is already written.
const ANCHOR_SLOTS: [u8; 2] = [3, 4];

/// The slot the leaf chain refreshes and reads as `LAST_FRAME`.
const LEAF_SLOT: u8 = 0;

/// The AV1 software encoder: [`EncoderConfig`] in, one [`Packet`] out per
/// [`Av1Encoder::encode`] call — or, with a [`Pyramid`] configured, a
/// coding-order burst of them per [`Av1Encoder::encode_frames`] call plus a
/// [`Av1Encoder::flush`] at the end of the stream.
#[derive(Debug)]
pub struct Av1Encoder {
    config: EncoderConfig,
    color_config: ColorConfig,
    /// The previous picture's own (padded, uncropped) reconstruction — what
    /// the next inter frame predicts from — or `None` right after a key
    /// frame's turn comes up again, where it is about to be replaced rather
    /// than read.
    reference: Option<Picture>,
    next_index: u64,
    /// `Some` only when constructed via
    /// [`Av1Encoder::with_rate_target`]`(_, RateTarget::BytesPerFrame(_))`;
    /// otherwise every picture is coded at `config.base_q_idx`, unchanged
    /// from before this field existed.
    rate_loop: Option<RateLoop>,
    /// This stream's decode-side per-frame state, owned here so `encode`
    /// keeps the public signature it had before the state stopped being
    /// thread-local. One per encoder, never per frame: several of its fields
    /// (the inter-frame inheritance guards) carry state ACROSS frames.
    fctx: crate::decode::FrameCtx,
    /// What the last coded frame stored into the reference slot the next
    /// inter frame reads (spec 7.20): the tables that frame's tile writer
    /// must start from, since the header leaves
    /// `disable_frame_end_update_cdf` off. `None` before the first frame.
    carried_cdfs: Option<crate::encode::CdfSnapshot>,
    /// The last key frame's own (padded) reconstruction: `GOLDEN_FRAME`,
    /// which stays in DPB slot 1 until the next key frame refreshes it.
    golden: Option<Picture>,
    /// The frame two back: `ALTREF_FRAME`, in the slot the next frame is
    /// about to refresh (`encode_inter_frame`'s 0/2 alternation).
    prev2: Option<Picture>,
    /// `Some` puts the encoder in pyramid mode: [`Av1Encoder::encode`] then
    /// refuses (a picture no longer maps to one packet) and the state below
    /// drives coding order.
    pyramid: Option<Pyramid>,
    /// The eight DPB slots, modelled exactly as the decoder does (spec 7.20):
    /// what picture each holds, the order hint it was coded at, and the CDF
    /// tables it stored. Only pyramid mode reads these; the flat path keeps
    /// `reference`/`golden`/`prev2` so its bytes are unchanged.
    dpb: [Option<DpbSlot>; 8],
    /// Pictures held back waiting for their mini-GOP's hidden frame.
    pending: Vec<(u64, Picture)>,
    /// Which of [`ANCHOR_SLOTS`] the group about to be coded reads as its
    /// anchor.
    anchor: usize,
    /// Coding-order position of the next packet — a packet's `dts`.
    next_dts: u64,
}

/// One decoded-picture-buffer slot: what a `refresh_frame_flags` bit stores
/// and what a `ref_frame_idx` entry reads back.
#[derive(Debug, Clone)]
struct DpbSlot {
    picture: Picture,
    order_hint: u32,
    cdfs: crate::encode::CdfSnapshot,
}

/// The encoder stays `Send` now that it owns a `FrameCtx` (whose cells are
/// `Send` but `!Sync`): it can move to another thread, it just cannot be
/// shared by reference across threads.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Av1Encoder>();
};

impl Av1Encoder {
    /// # Errors
    /// Returns an error when `config.width`/`config.height` are zero, odd,
    /// or larger than the 16-bit frame size an AV1 sequence header carries,
    /// or when `config.gop` is zero (there would be no picture to code the
    /// next key frame from a cadence of).
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if config.gop == 0 {
            return Err(Error::unsupported("AV1 encode", "gop must be at least 1"));
        }
        // Validate the geometry the same way the picture-level entry points
        // do, before the first `encode()` call rather than only surfacing it
        // then.
        Picture::grey(config.width, config.height).check_even()?;
        // A tile is a whole number of superblocks, so a frame cannot carry
        // more tile columns (rows) than it has 64x64 superblocks across
        // (down) -- the header's own `max_log2_tile_cols` bound (spec
        // 5.9.15), refused here rather than at the first `encode()` call.
        let sb_cols = config.width.div_ceil(64) as u32;
        let sb_rows = config.height.div_ceil(64) as u32;
        if (1u32 << config.tile_cols_log2) > sb_cols || (1u32 << config.tile_rows_log2) > sb_rows {
            return Err(Error::unsupported(
                "AV1 encode",
                format!(
                    "{}x{} tiles need more than the {sb_cols}x{sb_rows} superblocks this frame has",
                    1 << config.tile_cols_log2,
                    1 << config.tile_rows_log2
                ),
            ));
        }
        Ok(Self {
            carried_cdfs: None,
            golden: None,
            prev2: None,
            color_config: config.colour.color_config(),
            config,
            reference: None,
            next_index: 0,
            rate_loop: None,
            fctx: crate::decode::FrameCtx::new(),
            pyramid: None,
            dpb: [const { None }; 8],
            pending: Vec::new(),
            anchor: 0,
            next_dts: 0,
        })
    }

    /// [`Av1Encoder::new`] in pyramid mode: pictures are coded out of display
    /// order, so a call to [`Av1Encoder::encode_frames`] returns zero, one or
    /// several packets and [`Av1Encoder::flush`] drains whatever is still
    /// held at the end of the stream. [`Av1Encoder::encode`]'s one-in-one-out
    /// contract cannot hold here and it returns an error instead; a caller
    /// that wants the old shape simply does not call this constructor.
    ///
    /// # Errors
    /// As [`Av1Encoder::new`], plus a `mini_gop` below 2 (no leaf would be
    /// left between the anchor and the hidden frame) or above `config.gop`
    /// (the group would straddle a key frame).
    pub fn with_pyramid(config: EncoderConfig, pyramid: Pyramid) -> Result<Self> {
        if pyramid.mini_gop < 2 {
            return Err(Error::unsupported(
                "AV1 encode",
                "a pyramid mini-GOP is at least 2 pictures",
            ));
        }
        let mut encoder = Self::new(config)?;
        encoder.pyramid = Some(pyramid);
        Ok(encoder)
    }

    /// [`Av1Encoder::new`], but `rate` picks `base_q_idx` (or steers it,
    /// frame by frame, for [`RateTarget::BytesPerFrame`]) instead of
    /// `config.base_q_idx` being used as-is.
    ///
    /// # Errors
    /// Same as [`Av1Encoder::new`].
    pub fn with_rate_target(mut config: EncoderConfig, rate: RateTarget) -> Result<Self> {
        let rate_loop = rate.into_loop(&mut config, None);
        let mut encoder = Self::new(config)?;
        encoder.rate_loop = rate_loop;
        Ok(encoder)
    }

    /// [`Av1Encoder::with_pyramid`] and [`Av1Encoder::with_rate_target`] at
    /// once: the rate loop then keeps a separate quantizer per pyramid level,
    /// predicting each frame's size from the previous frame of its OWN level
    /// (a leaf and a hidden `ALTREF` differ by several times in size, so one
    /// shared predictor would swing the quantizer every frame).
    ///
    /// # Errors
    /// As both.
    pub fn with_pyramid_and_rate_target(
        mut config: EncoderConfig,
        pyramid: Pyramid,
        rate: RateTarget,
    ) -> Result<Self> {
        let rate_loop = rate.into_loop(&mut config, Some(pyramid.mini_gop));
        let mut encoder = Self::with_pyramid(config, pyramid)?;
        encoder.rate_loop = rate_loop;
        Ok(encoder)
    }

    /// The coded (padded-to-64) frame size every picture in this stream is
    /// coded at — what a decoder allocates and what `ffprobe` reports.
    #[must_use]
    pub fn coded_size(&self) -> (usize, usize) {
        (
            self.config.width.next_multiple_of(SUPERBLOCK),
            self.config.height.next_multiple_of(SUPERBLOCK),
        )
    }

    /// The display (render) size every frame header signals — [`EncoderConfig`]'s
    /// own `width`/`height`, always, whatever the coded size pads it to.
    #[must_use]
    pub fn display_size(&self) -> (usize, usize) {
        (self.config.width, self.config.height)
    }

    /// Encodes one picture, key or inter by this stream's `gop` cadence, and
    /// returns its packet — always exactly one, never held back for a later
    /// call.
    ///
    /// # Errors
    /// Returns an error when `picture`'s size does not match
    /// [`EncoderConfig::width`]/[`EncoderConfig::height`], when the encoder
    /// was built by [`Av1Encoder::with_pyramid`] (which reorders pictures, so
    /// one call cannot return one packet), or under the same conditions
    /// [`crate::encode::encode_key_frame`]/[`crate::encode::encode_sequence`]
    /// do.
    pub fn encode(&mut self, picture: &Picture) -> Result<Packet> {
        if self.pyramid.is_some() {
            return Err(Error::unsupported(
                "AV1 encode",
                "a pyramid stream reorders pictures -- use encode_frames()/flush()",
            ));
        }
        let mut packets = self.encode_frames(picture)?;
        Ok(packets.pop().expect("the flat path emits exactly one packet"))
    }

    /// Encodes one picture and returns every packet it completed, in coding
    /// order. Without a [`Pyramid`] that is always exactly one packet and
    /// this is [`Av1Encoder::encode`] with a `Vec` around it; with one it is
    /// empty for most pictures and a whole mini-GOP's worth (the hidden
    /// frame, its leaves and the `show_existing_frame` that re-outputs it)
    /// for the picture that closes a group.
    ///
    /// # Errors
    /// As [`Av1Encoder::encode`], minus the pyramid refusal.
    pub fn encode_frames(&mut self, picture: &Picture) -> Result<Vec<Packet>> {
        crate::encode::arm_tiles(self.config.tile_cols_log2, self.config.tile_rows_log2);
        if (picture.width, picture.height) != (self.config.width, self.config.height) {
            return Err(Error::unsupported(
                "AV1 encode",
                format!(
                    "picture is {}x{}, encoder is {}x{}",
                    picture.width, picture.height, self.config.width, self.config.height
                ),
            ));
        }
        let Some(pyramid) = self.pyramid else {
            return Ok(vec![self.encode_flat(picture)?]);
        };
        let order = self.next_index;
        self.next_index += 1;
        if order.is_multiple_of(self.config.gop as u64) {
            // A key frame closes whatever group is open (there is nothing for
            // its leaves to point forward at across the boundary) and then
            // restarts the pyramid from the key itself.
            let mut packets = self.drain_pending()?;
            packets.push(self.encode_key(order, picture)?);
            return Ok(packets);
        }
        self.pending.push((order, picture.clone()));
        // The group closes on its own size, or early when the next picture
        // would be a key frame.
        let next_is_key = (order + 1).is_multiple_of(self.config.gop as u64);
        if self.pending.len() >= pyramid.mini_gop || next_is_key {
            return self.drain_pending();
        }
        Ok(Vec::new())
    }

    /// Codes and returns every picture still held back — the tail of a stream
    /// whose last mini-GOP never filled up. Empty (and cheap) for a stream
    /// with no [`Pyramid`], which never holds a picture at all.
    ///
    /// # Errors
    /// As [`Av1Encoder::encode_frames`].
    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        crate::encode::arm_tiles(self.config.tile_cols_log2, self.config.tile_rows_log2);
        self.drain_pending()
    }

    /// The pre-pyramid path, byte for byte: one picture in, one packet out.
    fn encode_flat(&mut self, picture: &Picture) -> Result<Packet> {
        let render = (self.config.width, self.config.height);
        let padded = picture.padded_to(SUPERBLOCK);
        let is_key = self.next_index.is_multiple_of(self.config.gop as u64);
        let order = self.next_index;
        let base_q_idx = self.rate_loop.as_ref().map_or(self.config.base_q_idx, |r| {
            r.q_idx(if is_key { Level::Key } else { Level::Leaf })
        });

        let encoded: Encoded = if is_key {
            encode_key_frame_inner(
                &padded,
                base_q_idx,
                DEADZONE,
                &KEY_FRAME_MODES,
                split_blocks(),
                render,
                self.color_config,
                &self.fctx,
            )?
        } else {
            let reference = self.reference.as_ref().ok_or_else(|| {
                Error::unsupported(
                    "AV1 encode",
                    "an inter frame needs a previous reconstruction",
                )
            })?;
            encode_inter_frame(
                &padded,
                reference,
                base_q_idx,
                DEADZONE,
                order as u32,
                render,
                self.carried_cdfs.as_ref().map(|c| &c.0),
                self.golden.as_ref(),
                self.prev2.as_ref(),
                &self.fctx,
                None,
            )?
        };

        self.carried_cdfs = Some(encoded.next_cdfs.clone());
        if is_key {
            self.golden = Some(encoded.reconstruction.clone());
            self.prev2 = None;
        } else {
            self.prev2 = self.reference.take();
        }
        self.reference = Some(encoded.reconstruction.clone());
        self.next_index += 1;
        let cropped = crop_encoded(&encoded, render.0, render.1);
        let level = if is_key { Level::Key } else { Level::Leaf };
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(level, cropped.stream.len());
        }
        Ok(self.packet(cropped.stream, is_key, order, level))
    }

    /// Stamps a packet with its display and coding positions and advances the
    /// coding-order clock.
    fn packet(&mut self, data: Vec<u8>, key: bool, order: u64, level: Level) -> Packet {
        let dts = self.next_dts;
        self.next_dts += 1;
        Packet {
            data,
            key,
            order,
            dts,
            level,
        }
    }

    /// Stores a coded frame into every DPB slot its `refresh_frame_flags`
    /// names (spec 7.20).
    fn refresh(&mut self, slots: &[u8], encoded: &Encoded, order_hint: u32) {
        let slot = DpbSlot {
            picture: encoded.reconstruction.clone(),
            order_hint,
            cdfs: encoded.next_cdfs.clone(),
        };
        for &s in slots {
            self.dpb[s as usize] = Some(slot.clone());
        }
    }

    /// Codes a key frame in pyramid mode: it refreshes all eight slots, so it
    /// becomes the anchor, the `GOLDEN_FRAME` and the CDF source for the
    /// whole GOP at once.
    fn encode_key(&mut self, order: u64, picture: &Picture) -> Result<Packet> {
        let render = (self.config.width, self.config.height);
        let base_q_idx = self
            .rate_loop
            .as_ref()
            .map_or(self.config.base_q_idx, |r| r.q_idx(Level::Key));
        let encoded = encode_key_frame_inner(
            &picture.padded_to(SUPERBLOCK),
            base_q_idx,
            DEADZONE,
            &KEY_FRAME_MODES,
            split_blocks(),
            render,
            self.color_config,
            &self.fctx,
        )?;
        self.refresh(&[0, 1, 2, 3, 4, 5, 6, 7], &encoded, 0);
        self.anchor = 0;
        let cropped = crop_encoded(&encoded, render.0, render.1);
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(Level::Key, cropped.stream.len());
        }
        Ok(self.packet(cropped.stream, true, order, Level::Key))
    }

    /// Codes one inter frame at `order` from the DPB, in coding order.
    #[allow(clippy::too_many_arguments)]
    fn encode_pyramid_inter(
        &mut self,
        order: u64,
        picture: &Picture,
        last_slot: u8,
        self_slot: u8,
        altref_slot: u8,
        show_frame: bool,
        level: Level,
    ) -> Result<Packet> {
        let render = (self.config.width, self.config.height);
        let order_hint = (order & 0x7f) as u32;
        let pyramid = self.pyramid.expect("pyramid mode");
        let offset = match level {
            Level::Arf => pyramid.arf_q_offset,
            _ => pyramid.leaf_q_offset,
        };
        let base = self
            .rate_loop
            .as_ref()
            .map_or(i16::from(self.config.base_q_idx), |r| {
                i16::from(r.q_idx(level))
            });
        let base_q_idx = (base + offset).clamp(1, 255) as u8;
        // spec 5.9.2's `ref_frame_sign_bias`, derived from what the slots
        // this frame names actually hold: a reference whose order hint is
        // AHEAD of this frame's is backward-biased, and the MV stack scans
        // (writer and decoder alike) flip borrowed candidates across the
        // boundary. Only `ALTREF_FRAME` can be ahead here — the leaves of a
        // mini-GOP name the group's hidden frame there.
        let mut sign_bias = crate::mvstack::NO_SIGN_BIAS;
        let ahead = |slot: u8, dpb: &[Option<DpbSlot>; 8]| {
            dpb[slot as usize]
                .as_ref()
                .is_some_and(|s| s.order_hint > order_hint)
        };
        for (i, slot) in [
            last_slot, last_slot, last_slot, GOLDEN_SLOT, last_slot, last_slot, altref_slot,
        ]
        .into_iter()
        .enumerate()
        {
            sign_bias[i] = ahead(slot, &self.dpb);
        }
        let reference = self.dpb[last_slot as usize]
            .as_ref()
            .map(|s| s.picture.clone())
            .ok_or_else(|| {
                Error::unsupported("AV1 encode", "an inter frame needs a previous reconstruction")
            })?;
        let golden = self.dpb[GOLDEN_SLOT as usize].as_ref().map(|s| s.picture.clone());
        let altref = (altref_slot != GOLDEN_SLOT)
            .then(|| self.dpb[altref_slot as usize].as_ref().map(|s| s.picture.clone()))
            .flatten();
        let start_cdfs = self.dpb[last_slot as usize].as_ref().map(|s| s.cdfs.0.clone());
        let encoded = || -> Result<Encoded> { encode_inter_frame(
            &picture.padded_to(SUPERBLOCK),
            &reference,
            base_q_idx,
            DEADZONE,
            order_hint,
            render,
            start_cdfs.as_ref(),
            golden.as_ref(),
            altref.as_ref(),
            &self.fctx,
            Some(crate::encode::PyramidFrame {
                last_slot,
                self_slot,
                altref_slot,
                show_frame,
                sign_bias,
            }),
        ) }();
        // Name the pyramid position in any failure: an error out of the tile
        // writer or the filter search is otherwise indistinguishable between
        // the hidden frame and the leaves that read it.
        let encoded = encoded.map_err(|e| {
            Error::unsupported(
                "AV1 encode",
                format!(
                    "{level:?} frame at order {order} (last slot {last_slot}, self {self_slot}, \
                     altref {altref_slot}, shown {show_frame}, golden {}, altref pic {}): {e}",
                    golden.is_some(),
                    altref.is_some(),
                ),
            )
        })?;
        self.refresh(&[self_slot], &encoded, order_hint);
        let cropped = crop_encoded(&encoded, render.0, render.1);
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(level, cropped.stream.len());
        }
        Ok(self.packet(cropped.stream, false, order, level))
    }

    /// Codes the open mini-GOP: its hidden frame first, then its leaves, then
    /// the `show_existing_frame` header that puts the hidden frame back in
    /// display order.
    fn drain_pending(&mut self) -> Result<Vec<Packet>> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let group: Vec<(u64, Picture)> = std::mem::take(&mut self.pending);
        let mut packets = Vec::with_capacity(group.len() + 1);
        let anchor_slot = ANCHOR_SLOTS[self.anchor];
        let next_anchor_slot = ANCHOR_SLOTS[1 - self.anchor];
        let (arf_order, arf_picture) = group.last().cloned().expect("non-empty");
        if group.len() == 1 {
            // Nothing to predict backward, so no hidden frame: one shown leaf
            // that also becomes the next group's anchor.
            packets.push(self.encode_pyramid_inter(
                arf_order,
                &arf_picture,
                anchor_slot,
                next_anchor_slot,
                GOLDEN_SLOT,
                true,
                Level::Leaf,
            )?);
            self.anchor = 1 - self.anchor;
            return Ok(packets);
        }
        packets.push(self.encode_pyramid_inter(
            arf_order,
            &arf_picture,
            anchor_slot,
            next_anchor_slot,
            GOLDEN_SLOT,
            false,
            Level::Arf,
        )?);
        for (i, (order, picture)) in group[..group.len() - 1].iter().enumerate() {
            let last_slot = if i == 0 { anchor_slot } else { LEAF_SLOT };
            packets.push(self.encode_pyramid_inter(
                *order,
                picture,
                last_slot,
                LEAF_SLOT,
                next_anchor_slot,
                true,
                Level::Leaf,
            )?);
        }
        let (seq, _) = crate::encode::key_frame_headers(
            self.config.width,
            self.config.height,
            self.config.base_q_idx,
        )?;
        let mut data = crate::obu::temporal_delimiter();
        data.extend_from_slice(&crate::frame::show_existing_frame_obu(
            &seq,
            next_anchor_slot,
        )?);
        packets.push(self.packet(data, false, arf_order, Level::ShowExisting));
        self.anchor = 1 - self.anchor;
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

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

    fn test_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x + shift) % width;
                picture.y[y * width + x] = ((sx * 3 + y * 5) % 256) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * (width / 2) + x;
                picture.u[i] = ((100 + shift * 4) % 256) as u16;
                picture.v[i] = ((200 + 256 - shift * 2 % 256) % 256) as u16;
            }
        }
        picture
    }

    /// Every OBU stream this test writes to `ffmpeg`/`ffprobe`, concatenated
    /// in order.
    fn concat(packets: &[Packet]) -> Vec<u8> {
        packets.iter().flat_map(|p| p.data.clone()).collect()
    }

    /// One call to [`Av1Encoder::encode`] returns exactly one packet, always
    /// — the facade's whole "nothing held back" contract, checked without
    /// ffmpeg since it is a property of the return type, not the bytes.
    #[test]
    fn one_in_one_out() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 2,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        for t in 0..5u64 {
            let packet = enc.encode(&test_card(64, 64, t as usize)).unwrap();
            assert!(!packet.data.is_empty(), "picture {t}: empty packet");
            assert_eq!(packet.order, t, "picture {t}: order");
        }
    }

    /// A key frame every `gop` pictures, inter otherwise — checked against
    /// what the facade itself reports, and (below) against what `ffprobe`
    /// reads back out of the coded bytes.
    #[test]
    fn gop_cadence_is_honored() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 3,
            colour: Colour::Unspecified,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let keys: Vec<bool> = (0..7)
            .map(|t| enc.encode(&test_card(64, 64, t)).unwrap().key)
            .collect();
        assert_eq!(keys, vec![true, false, false, true, false, false, true]);
    }

    fn ffprobe_frames(stream: &[u8]) -> Vec<(bool, u32, u32)> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
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
                "frame=key_frame,width,height",
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
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| {
                let mut f = line.trim().split(',');
                let key: u32 = f.next().unwrap().parse().unwrap();
                let width: u32 = f.next().unwrap().parse().unwrap();
                let height: u32 = f.next().unwrap().parse().unwrap();
                (key == 1, width, height)
            })
            .collect()
    }

    fn ffprobe_colour(stream: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-colour-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
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
                "stream=color_primaries,color_transfer,color_space,color_range",
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
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A 30-picture stream at gop 15 codes 2 key frames (pictures 0 and 15)
    /// and 28 inter ones, at an odd-ish (not superblock-multiple) size that
    /// exercises the pad/crop path -- `ffprobe`'s own `key_frame` flags and
    /// coded size confirm what the facade already reported.
    #[test]
    fn thirty_pictures_at_gop_fifteen_decode_to_two_key_frames() {
        if !have_ffmpeg() {
            eprintln!("SKIP thirty_pictures_at_gop_fifteen_decode_to_two_key_frames: no ffmpeg");
            return;
        }
        let (width, height) = (96usize, 96usize);
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 15,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        assert_eq!(enc.display_size(), (width, height));
        let (coded_w, coded_h) = enc.coded_size();
        assert_eq!(
            (coded_w, coded_h),
            (
                width.next_multiple_of(SUPERBLOCK),
                height.next_multiple_of(SUPERBLOCK)
            )
        );

        let packets: Vec<Packet> = (0..30)
            .map(|t| enc.encode(&test_card(width, height, t)).unwrap())
            .collect();
        let expect_key: Vec<bool> = (0..30).map(|t| t % 15 == 0).collect();
        assert_eq!(
            packets.iter().map(|p| p.key).collect::<Vec<_>>(),
            expect_key,
            "facade's own key/inter flags"
        );

        let stream = concat(&packets);
        let frames = ffprobe_frames(&stream);
        assert_eq!(frames.len(), 30, "ffprobe frame count");
        for (t, (key, w, h)) in frames.iter().enumerate() {
            assert_eq!(*key, expect_key[t], "picture {t}: ffprobe key_frame flag");
            assert_eq!(
                (*w, *h),
                (width as u32, height as u32),
                "picture {t}: ffprobe reports the true (display) size, not the padded coded one"
            );
        }
    }

    /// The configured colour properties are what `ffprobe` reports back,
    /// spec 5.5.2's CICP triple plus range -- BT.709 limited here, since it
    /// is the default a player assumes and the one this test would silently
    /// pass without wiring for (`ffprobe` reports "unknown" for the
    /// hardcoded "unspecified" this crate wrote before the facade existed,
    /// which is a visibly different string).
    #[test]
    fn bt709_limited_colour_is_reported_by_ffprobe() {
        if !have_ffmpeg() {
            eprintln!("SKIP bt709_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packet = enc.encode(&test_card(64, 64, 0)).unwrap();
        let colour = ffprobe_colour(&packet.data);
        assert_eq!(colour, "tv,bt709,bt709,bt709", "ffprobe colour fields");
    }

    /// The same, for BT.601: a different set of CICP integers must produce a
    /// different string, or the wiring could be a no-op that happens to read
    /// as BT.709 for every input.
    #[test]
    fn bt601_limited_colour_is_reported_by_ffprobe() {
        if !have_ffmpeg() {
            eprintln!("SKIP bt601_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt601Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packet = enc.encode(&test_card(64, 64, 0)).unwrap();
        let colour = ffprobe_colour(&packet.data);
        assert_eq!(
            colour, "tv,smpte170m,smpte170m,smpte170m",
            "ffprobe colour fields"
        );
    }

    /// A frame whose size does not match the encoder's configured geometry
    /// is refused by name.
    #[test]
    fn geometry_mismatch_is_refused() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let err = enc.encode(&Picture::grey(32, 32)).unwrap_err();
        assert!(err.to_string().contains("64x64"), "{err}");
    }

    /// `gop == 0` is refused at construction, not the first `encode()` call.
    #[test]
    fn zero_gop_is_refused() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 0,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let err = Av1Encoder::new(config).unwrap_err();
        assert!(err.to_string().contains("gop"), "{err}");
    }

    /// The 12-frame real clip `real_clip_encodes_within_its_quality_and_size_budget`
    /// already gates on at `q=100` (`crates/ec-av1/src/encode.rs`), decoded down
    /// to this facade's own input size, and re-coded through the facade so the
    /// rate-target surface is exercised at the entry point a caller actually
    /// drives.
    fn h264_clip_frames(width: usize, height: usize, frames: usize) -> Option<Vec<Picture>> {
        if !have_ffmpeg() {
            return None;
        }
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        if !clip.exists() {
            return None;
        }
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i", clip.to_str().unwrap()])
            .args(["-frames:v", &frames.to_string()])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_len * frames,
            "expected {frames} 4:2:0 frames"
        );
        Some(
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
                .collect(),
        )
    }

    /// [`RateTarget::BytesPerFrame`] on a real, 24-frame clip lands its total
    /// coded size within ±20% of `frames * target` once the first few
    /// frames' settling has been discarded (the key frame's own reference
    /// state and the controller's first couple of steps).
    #[test]
    fn bytes_per_frame_target_settles_within_20_percent() {
        let Some(pictures) = h264_clip_frames(640, 384, 24) else {
            eprintln!("SKIP bytes_per_frame_target_settles_within_20_percent: no ffmpeg/fixture");
            return;
        };
        let target_bytes = 4_000u32;
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(target_bytes)).unwrap();
        let sizes: Vec<usize> = pictures
            .iter()
            .map(|p| enc.encode(p).unwrap().data.len())
            .collect();
        // Discard the key frame (always far larger than an inter target) and
        // the next 4 inter frames the loop needs to step toward it.
        let settled = &sizes[5..];
        let mean = settled.iter().sum::<usize>() as f64 / settled.len() as f64;
        let low = f64::from(target_bytes) * 0.8;
        let high = f64::from(target_bytes) * 1.2;
        assert!(
            mean >= low && mean <= high,
            "settled mean {mean:.0} bytes/frame outside ±20% of {target_bytes} ({low:.0}..{high:.0}); sizes={sizes:?}"
        );
    }

    /// [`RateTarget::Bitrate`] lands the achieved bitrate within ±10% of the
    /// target over 48 frames of a real clip, flat and under a pyramid alike.
    /// This is the accuracy claim of the target-bitrate mode: no two-pass, no
    /// lookahead, just each frame's size predicted from the previous frame of
    /// its own level and one clamped proportional step.
    #[test]
    fn bitrate_target_lands_within_10_percent_over_48_frames() {
        let (width, height, frames) = (640usize, 384usize, 48usize);
        let Some(pictures) = h264_clip_frames(width, height, frames) else {
            eprintln!("SKIP bitrate_target_lands_within_10_percent_over_48_frames: no ffmpeg/fixture");
            return;
        };
        let fps = 24.0;
        for bits_per_second in [768_000u32, 1_536_000] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 100,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let rate = RateTarget::Bitrate {
                bits_per_second,
                frames_per_second: fps,
            };
            for pyramid in [None, Some(Pyramid::default())] {
                let mut enc = match pyramid {
                    None => Av1Encoder::with_rate_target(config, rate).unwrap(),
                    Some(p) => Av1Encoder::with_pyramid_and_rate_target(config, p, rate).unwrap(),
                };
                let mut coded = 0usize;
                for picture in &pictures {
                    for packet in enc.encode_frames(picture).unwrap() {
                        coded += packet.data.len();
                    }
                }
                for packet in enc.flush().unwrap() {
                    coded += packet.data.len();
                }
                let seconds = frames as f64 / fps;
                let achieved = coded as f64 * 8.0 / seconds;
                let error = achieved / f64::from(bits_per_second) - 1.0;
                eprintln!(
                    "bitrate {bits_per_second} bps, pyramid {}: {coded} bytes over {seconds:.2}s \
                     = {achieved:.0} bps ({:+.1}%)",
                    pyramid.is_some(),
                    100.0 * error,
                );
                assert!(
                    error.abs() <= 0.10,
                    "achieved {achieved:.0} bps is {:+.1}% off the {bits_per_second} bps target \
                     (pyramid {})",
                    100.0 * error,
                    pyramid.is_some(),
                );
            }
        }
    }

    /// The `BytesPerFrame` controller's own windup bound: no frame's coded
    /// `base_q_idx` step exceeds [`RateLoop::STEP_CLAMP`], across a real
    /// clip's full range of content (so a scene cut can't be the one frame
    /// that breaks the bound).
    #[test]
    fn bytes_per_frame_controller_never_oscillates_past_its_clamp() {
        let Some(pictures) = h264_clip_frames(640, 384, 24) else {
            eprintln!(
                "SKIP bytes_per_frame_controller_never_oscillates_past_its_clamp: no ffmpeg/fixture"
            );
            return;
        };
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(4_000)).unwrap();
        let mut prev_q = enc.rate_loop.as_ref().unwrap().q_idx(Level::Leaf);
        for picture in &pictures {
            enc.encode(picture).unwrap();
            let q = enc.rate_loop.as_ref().unwrap().q_idx(Level::Leaf);
            let step = (i32::from(q) - i32::from(prev_q)).abs();
            assert!(
                f64::from(step) <= RateLoop::STEP_CLAMP + 1.0, // +1 for u8 rounding
                "q stepped from {prev_q} to {q}, past the {} clamp",
                RateLoop::STEP_CLAMP
            );
            prev_q = q;
        }
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

    /// Decodes an OBU stream back to raw luma planes via `ffmpeg`/dav1d, one
    /// entry per coded frame, in order — this facade's own equivalent of
    /// `encode::tests::ffmpeg_decode_sequence`, needed here too since a
    /// PSNR check needs the decoded pixels, not just the coded byte count.
    fn ffmpeg_decode_luma(stream: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-rate-decode-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the decode probe stream");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "obu", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        out.stdout
            .chunks_exact(frame_len)
            .map(|f| f[..luma].to_vec())
            .collect()
    }

    /// A pyramid stream reorders pictures: the hidden `ALTREF` of each
    /// mini-GOP is coded before the leaves that precede it in display order,
    /// and a `show_existing_frame` header puts it back. This pins the shape
    /// (one packet per source picture plus one `show_existing_frame` per
    /// group, `dts` monotone, `order` NOT monotone) and then decodes the
    /// whole stream through OUR decoder and through ffmpeg/dav1d and requires
    /// the two display-order outputs to agree sample for sample — the
    /// [[gate-blind-to-hidden-frames]] check: a hidden frame that is never
    /// output, or output twice, or output in the wrong position, changes the
    /// display-order list and fails here.
    #[test]
    fn a_pyramid_stream_decodes_in_display_order_through_both_decoders() {
        let (width, height) = (128usize, 128usize);
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 32,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let pyramid = Pyramid {
            mini_gop: 4,
            ..Pyramid::default()
        };
        let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
        let sources: Vec<Picture> = (0..9).map(|t| test_card(width, height, t * 3)).collect();
        let _ = crate::encode::take_ref_frame_hits();
        let mut packets = Vec::new();
        for picture in &sources {
            packets.extend(enc.encode_frames(picture).unwrap());
        }
        packets.extend(enc.flush().unwrap());

        // Shape: every source picture is coded exactly once, and each closed
        // group adds one `show_existing_frame` packet.
        let coded: Vec<u64> = packets
            .iter()
            .filter(|p| p.level != Level::ShowExisting)
            .map(|p| p.order)
            .collect();
        let mut sorted = coded.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..9).collect::<Vec<u64>>(), "one packet per picture");
        assert_ne!(coded, sorted, "the pyramid never reordered anything");
        let shown = packets.iter().filter(|p| p.level == Level::ShowExisting).count();
        assert_eq!(shown, 2, "one show_existing_frame per closed mini-GOP");
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(p.dts, i as u64, "packet {i}: dts is the coding position");
        }
        let hist = |l: Level| packets.iter().filter(|p| p.level == l).count();
        eprintln!(
            "pyramid levels: key {} arf {} leaf {} show_existing {}",
            hist(Level::Key),
            hist(Level::Arf),
            hist(Level::Leaf),
            hist(Level::ShowExisting),
        );
        assert_eq!(hist(Level::Arf), 2, "two hidden frames");
        let refs = crate::encode::take_ref_frame_hits();
        eprintln!(
            "blocks per reference: LAST {} GOLDEN {} ALTREF {}",
            refs[crate::mvstack::LAST_FRAME as usize],
            refs[crate::mvstack::GOLDEN_FRAME as usize],
            refs[crate::mvstack::ALTREF_FRAME as usize],
        );
        // [[gate-blind-to-feature]]: the whole point of the hidden frame is
        // that the leaves predict BACKWARD off it, which is also the only
        // thing that exercises `ref_frame_sign_bias` in the MV stack. A green
        // round trip in which `ALTREF_FRAME` never won a block would prove
        // only that a hidden frame can be written and re-output.
        assert!(
            refs[crate::mvstack::ALTREF_FRAME as usize] > 0,
            "no block predicted off the hidden ALTREF"
        );

        let stream = concat(&packets);
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's display-order count");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half of the pyramid round trip: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's display-order count");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            assert_eq!(got.len(), b.len(), "frame {i}: luma size");
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "display frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
        }
    }

    /// Every tile layout this encoder can write decodes SAMPLE-EXACT
    /// through both decoders -- ours and ffmpeg's -- and the frames really
    /// carry the tiles the header claims (the OBU parser locates one payload
    /// per tile, so a stream that quietly stayed single-tile fails here
    /// rather than passing on a green round trip).
    ///
    /// The bytes are what the gate is on: an encoder that clipped its
    /// neighbour availability wrong writes a stream whose two decoders agree
    /// with each other and disagree with nothing -- so the ffmpeg half is
    /// the one that catches a tile boundary the writer respected and the
    /// spec does not, and the parser half catches the reverse.
    #[test]
    fn every_tile_layout_decodes_sample_exact_through_both_decoders() {
        let (width, height) = (640usize, 384usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let mut sizes = Vec::new();
        for (cols_log2, rows_log2) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1), (2, 1)] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 4,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let layout = format!("{}x{} tiles", 1 << cols_log2, 1 << rows_log2);
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for (i, picture) in sources.iter().enumerate() {
                let packet = enc
                    .encode(picture)
                    .unwrap_or_else(|e| panic!("{layout} frame {i}: {e}"));
                stream.extend_from_slice(&packet.data);
            }
            sizes.push((layout.clone(), stream.len()));

            // The frames carry the tiles their headers claim.
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut frames = 0usize;
            let mut offset = 0usize;
            while offset < stream.len() {
                let obus = parser.parse_temporal_unit(&stream[offset..]).unwrap();
                let unit: usize = obus.iter().map(|o| o.total_size).sum();
                for obu in &obus {
                    if let ec_av1_syntax::ObuKind::Frame(parsed, tiles) = &obu.kind {
                        assert_eq!(
                            (parsed.tile_info.cols, parsed.tile_info.rows),
                            (1 << cols_log2, 1 << rows_log2),
                            "{layout}: the header's own tile grid"
                        );
                        assert_eq!(
                            tiles.len(),
                            1 << (cols_log2 + rows_log2),
                            "{layout}: tile payloads located in the tile group"
                        );
                        frames += 1;
                    }
                }
                offset += unit;
            }
            assert_eq!(frames, sources.len(), "{layout}: coded frames");

            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), sources.len(), "{layout}: our decoder's frames");
            if !have_ffmpeg() {
                eprintln!("SKIP the ffmpeg half of {layout}: no ffmpeg");
                continue;
            }
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "{layout}: ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                assert_eq!(got.len(), b.len(), "{layout} frame {i}: luma size");
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "{layout} frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
            }
        }
        let base = sizes[0].1 as f64;
        for (layout, bytes) in &sizes {
            eprintln!(
                "{layout}: {bytes} bytes ({:+.2}% vs one tile)",
                (*bytes as f64 / base - 1.0) * 100.0
            );
        }
    }

    /// The tiles of a frame are entropy-independent, so the bytes must not
    /// depend on how many workers wrote them: the same stream at one tile
    /// thread and at four, per layout.
    #[test]
    fn tile_bytes_do_not_depend_on_the_thread_count() {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (320usize, 192usize);
        let sources: Vec<Picture> = (0..3).map(|t| test_card(width, height, t * 3)).collect();
        let coded = |threads: usize, cols_log2: u32, rows_log2: u32| {
            crate::par::set_tile_threads(threads);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 3,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for picture in &sources {
                stream.extend_from_slice(&enc.encode(picture).unwrap().data);
            }
            stream
        };
        for (cols_log2, rows_log2) in [(1u32, 0u32), (0, 1), (1, 1), (2, 1)] {
            let one = coded(1, cols_log2, rows_log2);
            let four = coded(4, cols_log2, rows_log2);
            assert_eq!(
                one,
                four,
                "{}x{} tiles: {} bytes at one thread, {} at four",
                1 << cols_log2,
                1 << rows_log2,
                one.len(),
                four.len()
            );
        }
        crate::par::set_tile_threads(1);
    }

    /// The same round trip at a real 1920x1080 crop of the gate's own clip
    /// -- the size the editor's export actually runs at, where a tile grid
    /// is worth having. Ignored by default only for its wall (a 1080p
    /// encode of three pictures), not for any weakness in the check.
    #[test]
    #[ignore = "1080p encode: minutes, run it with --ignored"]
    fn a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders() {
        let (width, height) = (1920usize, 1080usize);
        let Some(sources) = h264_clip_frames(width, height, 3) else {
            eprintln!("SKIP the 1080p tile round trip: no fixture");
            return;
        };
        for (cols_log2, rows_log2) in [(1u32, 0u32), (1, 1), (2, 1)] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 3,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let layout = format!("{}x{} tiles", 1 << cols_log2, 1 << rows_log2);
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for (i, picture) in sources.iter().enumerate() {
                let packet = enc
                    .encode(picture)
                    .unwrap_or_else(|e| panic!("{layout} frame {i}: {e}"));
                stream.extend_from_slice(&packet.data);
            }
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), sources.len(), "{layout}: our decoder's frames");
            if !have_ffmpeg() {
                eprintln!("SKIP the ffmpeg half of 1080p {layout}: no ffmpeg");
                continue;
            }
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "{layout}: ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "1080p {layout} frame {i}: luma differs first at ({}, {})",
                        at % width,
                        at / width
                    );
                }
            }
            eprintln!("1080p {layout}: {} bytes, sample-exact", stream.len());
        }
    }

    /// [`RateTarget::Quality`] is monotone: a higher quality dial never
    /// produces a smaller stream or a worse mean PSNR than a lower one, on
    /// the same real clip.
    #[test]
    fn quality_target_is_monotone_in_bytes_and_psnr() {
        if !have_ffmpeg() {
            eprintln!("SKIP quality_target_is_monotone_in_bytes_and_psnr: no ffmpeg");
            return;
        }
        let (width, height) = (640, 384);
        let Some(pictures) = h264_clip_frames(width, height, 8) else {
            eprintln!("SKIP quality_target_is_monotone_in_bytes_and_psnr: no fixture");
            return;
        };
        let mut prev_bytes = 0usize;
        let mut prev_psnr = 0.0f64;
        for quality in [20u8, 50, 80] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 100,
                gop: 8,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc =
                Av1Encoder::with_rate_target(config, RateTarget::Quality(quality)).unwrap();
            let mut stream = Vec::new();
            let mut total_bytes = 0usize;
            for picture in &pictures {
                let packet = enc.encode(picture).unwrap();
                total_bytes += packet.data.len();
                stream.extend_from_slice(&packet.data);
            }
            let decoded = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(
                decoded.len(),
                pictures.len(),
                "quality {quality}: dav1d frame count"
            );
            let mean_psnr: f64 = decoded
                .iter()
                .zip(&pictures)
                .map(|(d, p)| {
                    let d16: Vec<u16> = d.iter().map(|&v| u16::from(v)).collect();
                    psnr(&d16, &p.y)
                })
                .sum::<f64>()
                / decoded.len() as f64;
            assert!(
                total_bytes >= prev_bytes,
                "quality {quality}: {total_bytes} bytes not >= previous {prev_bytes}"
            );
            assert!(
                mean_psnr >= prev_psnr - 0.01, // rounding slack, same-ish q_idx neighbours
                "quality {quality}: {mean_psnr:.2} dB not >= previous {prev_psnr:.2} dB"
            );
            prev_bytes = total_bytes;
            prev_psnr = mean_psnr;
        }
    }
}
