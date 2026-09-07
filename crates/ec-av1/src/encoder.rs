//! The streaming encoder facade — I11 of `lanes/av1-inter-plan.md`, the
//! surface `edith_replica`'s engine calls in place of rav1e.
//!
//! ONE FRAME OF LATENCY, everywhere: a picture handed to
//! [`Av1Encoder::encode`]/[`Av1Encoder::encode_frames`] is held until the
//! NEXT picture arrives, because an inter frame's per-superblock temporal
//! lambda map reads the next source picture as its lookahead (lane-av1tpl)
//! — exactly as [`crate::encode::encode_sequence`] does, so the facade and
//! the sequence path now code the same bytes for the same pictures. A
//! stream therefore ENDS with [`Av1Encoder::flush`], which codes whatever is
//! still held. [`Av1Encoder::with_pyramid`] holds a whole mini-GOP for the
//! same kind of reason (it reorders pictures) and is driven through
//! [`Av1Encoder::encode_frames`]/[`Av1Encoder::flush`] too. Internally it
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
    fn into_loop(self, config: &mut EncoderConfig, pyramid: Option<Pyramid>) -> Option<RateLoop> {
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
            pyramid,
            config.gop,
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
    /// Whether each slot has already steered off a real coded frame. The
    /// FIRST update of a slot is the seeding one and takes the model's whole
    /// step ([`RateLoop::update`]); every later one is clamped.
    seeded: [bool; 3],
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

    fn new(target_bytes: f64, start_q: f64, pyramid: Option<Pyramid>, gop: usize) -> Self {
        let Some(p) = pyramid else {
            return Self {
                target: [target_bytes; 3],
                q: [start_q; 3],
                per_level: false,
                seeded: [false; 3],
            };
        };
        let m = p.mini_gop as f64;
        // THE KEY FRAME'S SHARE, in frame targets. `KEY_WEIGHT` prices it at
        // the quantizer the loop steers it to, and
        // [`Pyramid::key_q_offset`] then codes it that much FINER, which the
        // key's own slot cannot steer back: a run holds exactly one key
        // frame, so the slot never gets a second frame to correct on. The
        // calibration slope (`GAIN`) says what the offset costs in bytes, and
        // the rest of the run pays for it -- without this the deeper key ran
        // the 48-frame bitrate gate +10.8% over target.
        let key = KEY_WEIGHT * (-f64::from(p.key_q_offset) / Self::GAIN).exp();
        // What is left for the `gop - 1` non-key pictures, per picture: a run
        // of `gop` pictures is worth `gop` frame targets in total.
        let rest = match gop > 1 {
            true => (target_bytes * (gop as f64 - key) / (gop as f64 - 1.0)).max(0.0),
            false => target_bytes,
        };
        // A group of `m` pictures gets `m * rest`, split one `ARF_WEIGHT`
        // share to the hidden frame and one each to the `m - 1` leaves, so
        // the group's own average is `rest` exactly.
        //
        // ONE hidden frame, deliberately, even though the group has coded two
        // since the mid level landed (and four with
        // [`Pyramid::quarter_q_offset`] on). Pricing the real count
        // (`m / (m - h + h * ARF_WEIGHT)`, h = 2) is the truthful model and
        // it lands the 48-frame bitrate gate -9.4% / -14.9% under target
        // against this line's -1.8% / -6.3%: the h = 1 split over-allocates
        // the group by exactly the amount `ARF_WEIGHT` under-prices a hidden
        // frame, and the two errors cancel. Fixing the split alone therefore
        // makes the loop WORSE; `ARF_WEIGHT` has to be recalibrated in the
        // same diff, which is a rate-control lane, not a pyramid-shape one
        // (lane-pyr6, `lanes/pyr6.sweep.txt`).
        let share = rest * m / (m - 1.0 + ARF_WEIGHT);
        Self {
            target: [target_bytes * key, share * ARF_WEIGHT, share],
            q: [start_q; 3],
            per_level: true,
            seeded: [false; 3],
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
        // `STEP_CLAMP` protects an operating point this slot has not found
        // yet: its `q` is still the caller's `base_q_idx` guess, and the
        // clamp makes the loop WALK to the right quantizer at twelve steps a
        // frame. Measured on the 48-frame bitrate gate, the pyramid's ARF
        // slot spent five of its twelve frames pinned at `-STEP_CLAMP` and
        // the stream landed -10.2% under target -- convergence lag, not a
        // steady-state offset (the fixed point of this loop is `actual ==
        // target` exactly). So a slot's FIRST frame takes the model's whole
        // step and seeds `q` where the log-linear slope says it belongs; from
        // the second frame on the clamp is back, and nothing is accumulated
        // between frames, so [[vorbis-rate-loop-windup]]'s bound still holds.
        let raw = ratio.ln() * Self::GAIN;
        let step = if std::mem::replace(&mut self.seeded[slot], true) {
            raw.clamp(-Self::STEP_CLAMP, Self::STEP_CLAMP)
        } else {
            raw
        };
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
    /// `1` would leave no leaf at all and is refused. It is the group size a
    /// long run takes; the last group of a run is up to 1.5x it, since
    /// [`group_target`] absorbs a tail shorter than half a group rather than
    /// leaving one behind.
    pub mini_gop: usize,
    /// Added to `base_q_idx` for the hidden `ALTREF_FRAME` — negative, since
    /// every leaf of the group predicts off it.
    pub arf_q_offset: i16,
    /// Added to `base_q_idx` for each shown leaf — positive, since nothing
    /// predicts off a leaf but the next leaf.
    pub leaf_q_offset: i16,
    /// THE THIRD LEVEL, or `None` for the two-level pyramid: the offset a
    /// second hidden frame, at the middle of the mini-GOP, is coded at
    /// (between [`Pyramid::arf_q_offset`] and [`Pyramid::leaf_q_offset`], so
    /// the group reads key -> top ARF -> mid ARF -> leaves). The leaves
    /// before it name it as their `ALTREF_FRAME` and the leaves after it
    /// start their chain from it, which halves every leaf's distance to its
    /// nearest good reference. Ignored by a group of fewer than four
    /// pictures, where there is no leaf on both sides of a midpoint.
    pub mid_q_offset: Option<i16>,
    /// THE FOURTH LEVEL, or `None` for the three-level pyramid: the offset
    /// the two hidden QUARTER-POINT ARFs are coded at (between
    /// [`Pyramid::mid_q_offset`] and [`Pyramid::leaf_q_offset`], so the group
    /// reads key -> top ARF -> mid ARF -> quarter ARFs -> leaves). In a group
    /// of eight they sit at pictures 2 and 6, halving again the distance from
    /// a leaf to its nearest good reference; every leaf then names the
    /// nearest hidden frame on each side. Ignored by a group with fewer than
    /// seven leaves, where a quarter point has no leaf on both sides, and --
    /// like the mid level -- by a run no longer than one mini-GOP.
    pub quarter_q_offset: Option<i16>,
    /// Added to `base_q_idx` for the GOP's KEY FRAME — negative, since every
    /// frame of the run reads it, directly or through an ARF. The 48-frame
    /// census (`lanes/census-longgop.md`) found ours coded at base q while
    /// its own ARFs sat 32 steps finer, where rav1e (key 100 / arf 120 /
    /// leaf 153) and libaom (63 / 95 / 163) both make the key the best frame
    /// of the run and spend 22--37% of the stream on it against our 10--11%.
    pub key_q_offset: i16,
}

/// How many pictures the mini-GOP now being collected takes, given how many
/// are left in this run (the pictures before the next key frame) and the
/// [`Pyramid::mini_gop`] shape.
///
/// A fixed `mini_gop` leaves a SHORT TAIL group whenever the run is not a
/// multiple of it: a 12-picture GOP under `mini_gop` 8 codes 8 + 3, and that
/// 3-picture tail is the expensive part -- the 12-frame gate preferred one
/// group of 11 by 7.0 / 20.5 BD points while the 48-frame gate picked 8
/// (lane-pyr4). The rule here ABSORBS a tail shorter than half a group into
/// the group before it, so a run takes groups of `mini_gop` until at most
/// 1.5 x `mini_gop` is left and then one final group of all of it:
///   remaining 11 under 8 -> 11 (was 8 + 3)
///   remaining 47 under 8 -> 8, 8, 8, 8, 8, 7 -- what the fixed rule already
///   coded, so a long GOP is byte-identical
/// (`the_mini_gop_layout_leaves_no_short_tail`; sweep `lanes/gopad.sweep.txt`).
///
/// `EC_AV1_GOP_LAYOUT=fixed` restores the old fixed-size rule for an A/B.
fn group_target(remaining: usize, mini_gop: usize, absorb_tail: bool) -> usize {
    match absorb_tail && remaining <= mini_gop + mini_gop / 2 {
        true => remaining.max(1),
        false => mini_gop,
    }
}

/// Whether this run absorbs a short tail group (the default) or cuts every
/// mini-GOP at `mini_gop`: `EC_AV1_GOP_LAYOUT=fixed`, read once.
fn absorb_tail() -> bool {
    static ABSORB: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("EC_AV1_GOP_LAYOUT").ok().as_deref() != Some("fixed")
    });
    *ABSORB
}

impl Pyramid {
    /// The pyramid a run asks for through the environment:
    /// `EC_AV1_PYRAMID=<mini_gop>[:<arf_q_offset>:<leaf_q_offset>[:<mid>[:<key>]]]`,
    /// where `<mid>` is the third level's offset or `off` for none, with
    /// `0` (or anything that is not a mini-GOP of at least 2, e.g. `flat`)
    /// meaning the flat one-picture-one-frame path. UNSET is
    /// [`Pyramid::default`]: the pyramid is what
    /// [`crate::encode::encode_sequence`], the BD gate and `ec-bench` code
    /// under unless a run turns it off for an A/B.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let d = Self::default();
        let Some(spec) = std::env::var("EC_AV1_PYRAMID").ok() else {
            return Some(d);
        };
        let mut f = spec.split(':');
        let mini_gop: usize = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        if mini_gop < 2 {
            return None;
        }
        Some(Self {
            mini_gop,
            arf_q_offset: f.next().and_then(|v| v.parse().ok()).unwrap_or(d.arf_q_offset),
            leaf_q_offset: f.next().and_then(|v| v.parse().ok()).unwrap_or(d.leaf_q_offset),
            // A fourth field is the mid-level offset; the literal `off` asks
            // for the two-level pyramid at the same shape (the A/B arm the
            // sweep needs), and no field at all keeps the default's.
            mid_q_offset: match f.next() {
                None => d.mid_q_offset,
                Some("off") => None,
                Some(v) => v.parse().ok().or(d.mid_q_offset),
            },
            // A fifth field is the key frame's own offset.
            key_q_offset: f.next().and_then(|v| v.parse().ok()).unwrap_or(d.key_q_offset),
            // A sixth field is the quarter level's offset, `off` for none.
            quarter_q_offset: match f.next() {
                None => d.quarter_q_offset,
                Some("off") => None,
                Some(v) => v.parse().ok().or(d.quarter_q_offset),
            },
        })
    }
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
    /// Re-measured a third time at NATIVE resolution (`encode::tests::
    /// bd_rate_screen_native`, 1920x1024 crops, 12 frames, four quantizers,
    /// BD-rate vs libaom / vs rav1e), because every earlier verdict was taken
    /// on the 4x-downscaled gate (class `gate-recipe-confound`) and because
    /// leaf compound and the distance-scaled search have landed since:
    ///
    /// | arm | film 1080p | film 2160p | screen |
    /// |---|---|---|---|
    /// | flat (no pyramid) | +18.7 / +1.4 | +48.1 / +19.8 | +59.4 / -10.0 |
    /// | 4:-16:8 | +23.2 / +5.3 | +48.1 / +19.7 | +62.7 / -6.5 |
    /// | 2:-16:8 | +24.9 / +6.9 | +46.9 / +19.5 | +69.0 / -3.8 |
    /// | 8:-16:8 | +22.6 / +4.6 | +48.7 / +19.8 | +59.3 / -8.2 |
    /// | 4:-24:12 (before the leaf-compound merge) | +24.3 / +6.3 | +48.5 / +20.2 | +67.2 / -3.7 |
    ///
    /// The confound was real -- at native the gap is 5 BD points, not 30 --
    /// but the sign is not: no arm beats flat on both films, every arm costs
    /// 4-6 points on 1080p, and the best 2160p arm (`2:-16:8`, -1.1 vs
    /// libaom) pays +10 on screen. So the pyramid still ships OFF.
    ///
    /// A native offset sweep on the 2160p clip alone (mini-GOP 4) says the
    /// offsets are not what is missing -- the whole 3x3 grid spans 2.9 BD
    /// points and none of it reaches flat's +48.1:
    ///
    /// | arf \ leaf | +4 | +8 | +12 |
    /// |---|---|---|---|
    /// | -8 | +48.9 / +20.4 | +49.2 / +20.1 | +50.2 / +20.5 |
    /// | -16 | +47.8 / +20.0 | +48.1 / +19.7 | +49.2 / +20.2 |
    /// | -24 | +47.3 / +20.0 | +47.6 / +19.7 | +48.5 / +20.2 |
    ///
    /// The gradient is monotone in both knobs (deeper ARF better, shallower
    /// leaf better), which is the shape of a group whose leaves cannot cash
    /// in the ARF's extra quality: what the leaves reach the hidden frame
    /// with is still one forward search per reference. Shrinking the mini-GOP
    /// helps for the same reason (2:-16:8 is the only arm that beats flat on
    /// any clip, and it beats it on the clip with the fastest motion).
    ///
    /// RE-SWEPT on lane-av1pyrgate, once the content gate below existed and
    /// on the current defaults (`LAMBDA_SCALE` 0.0275, the 32x32 tx-depth
    /// search and compound var-tx on): the earlier verdicts were taken with
    /// the screen capture inside the arm, and it is the one clip a pyramid
    /// never helps. With the gate, the screen row is the flat row by
    /// construction, so the sweep is decided on the two real films
    /// (`encode::tests::bd_rate_screen_native`, BD-rate vs libaom / vs
    /// rav1e; bars are fixtures, recorded only):
    ///
    /// | arm | film A | film B | screen | bars 1080p | bars 2160p |
    /// |---|---|---|---|---|---|
    /// | flat (no pyramid) | +62.8 / +32.5 | +86.3 / +50.5 | +50.1 / -15.4 | +16.4 / -1.1 | +48.2 / +20.4 |
    /// | 2:-16:8 | +60.3 / +29.1 | +83.6 / +50.2 | +50.1 / -15.4 | +24.0 / +5.9 | +47.2 / +20.2 |
    /// | **4:-16:8** | **+54.1 / +24.2** | **+82.6 / +47.4** | +50.1 / -15.4 | +21.5 / +3.7 | +48.3 / +20.4 |
    /// | 8:-16:8 | +55.9 / +25.8 | +83.6 / +47.3 | +50.1 / -15.4 | +19.8 / +2.0 | +48.7 / +20.4 |
    /// | 4:-24:12 | +51.5 / +22.3 | +84.9 / +50.3 | +50.1 / -15.4 | +21.9 / +4.0 | +48.4 / +20.7 |
    /// | 4:-12:6 | +57.6 / +27.1 | +84.8 / +49.3 | +50.1 / -15.4 | +21.7 / +3.9 | +48.8 / +20.9 |
    ///
    /// `4:-16:8` is the only arm that improves BOTH films on BOTH columns by
    /// several points (-8.7 / -8.3 on film A, -3.7 / -3.1 on film B) and it
    /// stays these defaults. `4:-24:12` buys another 2.6 on film A and gives
    /// 2.3 back on film B (where it barely beats flat vs rav1e, -0.2), which
    /// is the same "leaves cannot cash in the ARF's quality" shape as before,
    /// now content-dependent rather than uniform. The screen column is
    /// IDENTICAL in every arm -- that is the content gate, measured, not
    /// asserted. `EC_AV1_LAMBDA=0.035` on top of `4:-16:8` was re-judged once
    /// (the pyramid changes the reference structure): +54.5 / +24.5 and
    /// +85.2 / +49.4, worse on both films, so 0.0275 stands.
    ///
    /// SHIPPED AS THE DEFAULT on lane-av1pyrdef. `4:-16:8` is what
    /// [`crate::encode::encode_sequence`] (through
    /// [`Av1Encoder::encode_sequence_pyramid`], the one mini-GOP driver both
    /// paths run), the BD gates and `ec-bench` code a non-screen stream
    /// under; a screen stream still codes flat, decided once per stream by
    /// the content gate in [`Av1Encoder::encode_frames`]. `EC_AV1_PYRAMID=0`
    /// restores the flat path everywhere for an A/B, and
    /// `EC_AV1_PYRAMID=<mini_gop>[:<arf>:<leaf>]` another shape of it
    /// ([`Pyramid::from_env`]).
    ///
    /// [`Av1Encoder::new`] is unchanged: the one-picture-one-packet facade
    /// entry point still codes flat, since a picture no longer maps to a
    /// packet under a pyramid — [`Av1Encoder::with_pyramid`] plus
    /// [`Av1Encoder::encode_frames`] is the streaming surface that reorders.
    /// RE-SWEPT on lane-pyr3, on main 503d7aa9 -- `4:-16:8` was chosen when
    /// the gate's "film" rows were still colour bars, and CfL, the luma
    /// angle deltas, filter intra, the propagating tpl map and the rate-loop
    /// step have all landed since (class `unswept decision constants`). The
    /// axes were swept in the order mini-GOP first at the shipped offsets,
    /// then both offset axes at the best mini-GOP, then the joint corners
    /// (`encode::tests::bd_rate_screen_native`, BD-rate vs libaom / vs
    /// rav1e; the bars rows move by at most 4 points across the WHOLE sweep
    /// and decide nothing, the screen row is byte-identical in every arm
    /// because the content gate codes it flat):
    ///
    /// | arm | film A | film B |
    /// |---|---|---|
    /// | flat (no pyramid) | +61.4 / +31.3 | +85.3 / +49.7 |
    /// | 2:-16:8 | +59.2 / +28.1 | +82.3 / +49.1 |
    /// | 4:-16:8 (was shipped) | +52.8 / +23.3 | +81.0 / +46.1 |
    /// | 8:-16:8 | +54.3 / +24.4 | +81.8 / +45.8 |
    /// | 16:-16:8 | +50.8 / +21.4 | +73.8 / +38.7 |
    /// | 8:-8:8 | +58.0 / +27.5 | +83.3 / +46.5 |
    /// | 8:-24:8 | +52.7 / +23.4 | +83.7 / +48.4 |
    /// | 8:-32:8 | +52.3 / +23.1 | +86.8 / +51.9 |
    /// | 8:-16:0 | +60.7 / +30.7 | +85.3 / +51.2 |
    /// | 8:-16:4 | +57.6 / +27.4 | +84.1 / +48.6 |
    /// | 8:-16:12 | +51.8 / +22.2 | +80.4 / +43.9 |
    /// | 8:-16:16 | +50.1 / +20.5 | +80.6 / +43.4 |
    /// | 16:-24:8 | +47.8 / +19.0 | +71.5 / +37.4 |
    /// | 16:-16:12 | +48.0 / +18.9 | +71.1 / +35.7 |
    /// | 16:-16:16 | +45.9 / +17.0 | +69.3 / +34.0 |
    /// | 16:-24:12 | +45.6 / +17.0 | +70.2 / +35.6 |
    /// | **16:-24:16** | **+44.2 / +15.7** | **+70.5 / +35.2** |
    /// | 16:-32:16 | +43.6 / +15.2 | +71.2 / +36.2 |
    /// | 16:-24:20 (past the swept range) | +42.9 / +14.5 | +70.8 / +34.9 |
    /// | 16:-24:24 (past the swept range) | +42.6 / +14.1 | +71.3 / +35.7 |
    ///
    /// `16:-24:16` ships: it takes 8.6 / 7.6 points off film A and 10.5 /
    /// 10.9 off film B against the old defaults, and every axis through it
    /// is at or next to its own minimum. The two arms past the swept leaf
    /// range were run because leaf `+16` was the range's edge and still
    /// improving (class `instrument at bound`): film A keeps falling to
    /// `+24` but film B turns back up at `+20`, so `+16` is where BOTH films
    /// are minimal and the bound is not the answer. Deeper ARF is the same
    /// shape -- `-32` buys 0.6 on film A and gives 0.7 back on film B.
    ///
    /// WHAT THAT SWEEP COULD NOT SAY, and the LONG-GOP sweep that answers it
    /// (lane-pyr4, `lanes/pyr4.sweep.txt`). The table above is measured
    /// against a gate that codes 12 frames with `gop = 12`, and a group is
    /// cut at every key frame (`encode_frames`), so `16` there is really
    /// "one hidden ARF per GOP" and ANY mini-GOP over 12 codes the same
    /// stream (class `instrument at bound`). The user's exports are long
    /// GOPs, so the shape is decided on
    /// [`crate::encode::tests::bd_rate_film_long_gop`] -- 48 pictures, `gop
    /// = 48`, the two real films, four quantizers, vs libaom `cpu-used 6`
    /// and rav1e `speed=6` at the same key interval:
    ///
    /// | mini_gop:arf:leaf | film A vs libaom / rav1e | film B vs libaom / rav1e |
    /// |---|---|---|
    /// | 32:-24:16 | +79.0 / +29.8 | +164.4 / +58.8 |
    /// | 16:-24:16 (was the default) | +67.4 / +21.0 | +165.5 / +57.6 |
    /// | 8:-16:16 | +63.7 / +19.0 | +167.6 / +58.6 |
    /// | 4:-24:16 | +61.9 / +19.5 | +160.8 / +56.7 |
    /// | 8:-24:16 | +58.5 / +15.5 | +166.1 / +56.8 |
    /// | 8:-24:12 | +60.2 / +17.3 | +166.8 / +58.0 |
    /// | 8:-24:20 | +57.1 / +14.0 | +166.2 / +56.6 |
    /// | 8:-40:16 | +56.1 / +13.3 | +167.6 / +57.3 |
    /// | 8:-32:12 | +56.5 / +14.7 | +163.0 / +56.3 |
    /// | 8:-32:20 | +55.4 / +12.9 | +167.9 / +57.4 |
    /// | **8:-32:16** | **+55.7 / +13.5** | **+165.1 / +56.6** |
    ///
    /// `8:-32:16` ships: against the old `16:-24:16` it takes 11.7 / 7.5
    /// points off film A and 0.4 / 1.0 off film B -- all four columns down.
    /// A real 16- or 32-picture group is WORSE than an 8-picture one on film
    /// A once the GOP is long enough for the distinction to exist, which the
    /// 12-frame gate could not see; `4` was probed below the swept edge and
    /// turns back up on film A. ARF `-32` is an interior optimum (`-40`
    /// turns back on both films). The leaf axis has content-opposite optima
    /// (film A wants `20`, film B wants `12`, within 1.1 and 4.9 points) so
    /// `16` stays, being the only value down on all four columns.
    ///
    /// THE PRICE THAT WAS, and how it was paid (lane-gopad): on the
    /// SHORT-GOP gate (12 frames, `gop = 12`) a fixed mini-GOP of 8 split the
    /// run into 8 + 3 and read film A +51.2 / +21.4, film B +91.0 / +53.5,
    /// against `16:-24:16`'s +44.2 / +15.7 and +70.5 / +35.2 -- the tail
    /// group, not the shape, was the cost. [`group_target`] now absorbs a
    /// short tail, so the same 12 pictures code as ONE group of 11 and read
    /// film A +43.9 / +15.6 and film B +68.4 / +38.2 (three of the four
    /// columns better than `16:-24:16` ever was, film B vs rav1e 3.0 short),
    /// while 47 inter pictures still take 8 x 5 + 7 -- the long-GOP table
    /// above is coded byte for byte as it was measured.
    /// THE THIRD LEVEL (lane-pyr5, `lanes/pyr5.sweep.txt`), swept on the
    /// long-GOP gate against a control arm on this very head:
    ///
    /// | mini_gop:arf:leaf:mid | film A vs libaom / rav1e | film B vs libaom / rav1e |
    /// |---|---|---|
    /// | 8:-32:16 (control, 2-level) | +55.2 / +12.8 | +151.3 / +47.4 |
    /// | 8:-32:16:-4 | +51.8 / +11.4 | +146.2 / +45.4 |
    /// | **8:-32:16:-8** | **+52.1 / +11.7** | **+145.3 / +45.0** |
    /// | 8:-32:16:-16 | +53.4 / +13.0 | +146.0 / +46.1 |
    /// | 8:-32:20:-8 | +52.4 / +11.5 | +148.0 / +46.1 |
    /// | 16:-32:16:-8 | +55.0 / +12.3 | +146.0 / +44.3 |
    /// | 16:-32:16:-16 | +54.6 / +12.1 | +148.7 / +46.0 |
    /// | 4:-32:16:-8 | +61.7 / +20.4 | +147.5 / +51.3 |
    ///
    /// `-4` and `-8` are within a point of each other and split the two
    /// films; `-8` ships because film B is the wider gap. A mini-GOP of 4
    /// with a mid level -- rav1e's own shape at speed 6 -- is far worse here,
    /// so the third level is worth more than a shorter group, and the census
    /// (`lanes/census-longgop.md`) says why: at 8 pictures our leaves are
    /// already cheap and the bytes sit in the ARFs.
    ///
    /// The 12-frame gate keeps its numbers exactly (+43.9 / +15.6 and +68.4 /
    /// +38.2, byte-identical streams): a GOP that is one single mini-GOP does
    /// not code the third level at all (see `drain_pending`).
    /// THE LEAF OFFSET, RE-SWEPT ONCE THE ANCHORS MOVED (lane-arfq,
    /// `lanes/arfq.sweep.txt`). The table above chose `16` when the key frame
    /// still sat at the base quantizer; with [`Pyramid::key_q_offset`] at
    /// `-48` the whole run predicts off a much finer anchor and the leaves
    /// want to be finer with it. Long-GOP gate, control arm on the same head:
    ///
    /// | mini_gop:arf:leaf:mid:key | film A vs libaom / rav1e | film B vs libaom / rav1e |
    /// |---|---|---|
    /// | 8:-32:8:-8:-48 | +43.4 / +5.1 | +127.6 / +34.1 |
    /// | **8:-32:12:-8:-48** | **+43.1 / +4.6** | **+129.0 / +34.3** |
    /// | 8:-32:16:-8:-48 (control) | +43.5 / +4.6 | +131.3 / +35.0 |
    /// | 8:-32:24:-8:-48 | +45.6 / +5.6 | +138.8 / +38.4 |
    /// | 8:-32:12:-12:-48 | +43.4 / +5.0 | +128.1 / +33.8 |
    /// | 8:-32:12:-16:-48 | +44.0 / +5.6 | +127.7 / +34.1 |
    /// | 8:-24:12:-8:-48 | +44.5 / +5.2 | +128.6 / +33.3 |
    /// | 16:-32:12:-8:-48 | +47.2 / +6.3 | +134.7 / +35.6 |
    ///
    /// `12` ships: film B 2.3 / 0.7 down, film A 0.4 down against libaom and
    /// flat against rav1e. `8` is past the optimum (it buys film B and gives
    /// film A back), `24` is worse on all four. The other two axes were
    /// re-swept at `12` and did not move: a deeper mid (`-12`, `-16`) and a
    /// shallower ARF (`-24`) both trade film A away for film B, so the top
    /// ARF keeps its `-32` and the mid its `-8`. A 16-picture mini-GOP is
    /// worse on all four -- the pyramid has exactly three levels, so one mid
    /// frame cannot carry fifteen leaves.
    ///
    /// Unlike the third level this is NOT gated on GOP length: the 12-frame
    /// gate improves on every row too (film A +38.5 / +8.2 -> +37.5 / +7.6,
    /// film B +58.2 / +26.5 -> +54.1 / +23.9, both bars rows down, the screen
    /// capture byte-identical).
    fn default() -> Self {
        Self {
            mini_gop: 8,
            arf_q_offset: -32,
            leaf_q_offset: 12,
            mid_q_offset: MID_DEFAULT,
            key_q_offset: KEY_DEFAULT,
            quarter_q_offset: QUARTER_DEFAULT,
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

/// The slot the mid-level hidden frame of a three-level group refreshes
/// ([`Pyramid::mid_q_offset`]): free of the two anchors (3, 4), of the leaf
/// chain (0) and of `GOLDEN_SLOT` (1), so all four levels are live at once.
const MID_SLOT: u8 = 2;

/// The two slots the QUARTER-POINT hidden frames refresh
/// ([`Pyramid::quarter_q_offset`]). THE WHOLE SLOT MAP, with all four levels
/// live at once:
///
/// | slot | holds | written by | read as |
/// |---|---|---|---|
/// | 0 (`LEAF_SLOT`) | the previous shown leaf | every leaf | `LAST_FRAME` of the next leaf |
/// | 1 (`GOLDEN_SLOT`) | the key frame | the key | `GOLDEN_FRAME` everywhere |
/// | 2 (`MID_SLOT`) | the mid ARF | the mid ARF | `LAST_FRAME` / `ALTREF_FRAME` of the leaves and quarters around it |
/// | 3, 4 (`ANCHOR_SLOTS`) | this group's anchor and the next one (the top ARF) | the key, each top ARF | anchor = `LAST_FRAME`, next = `ALTREF_FRAME` |
/// | 5, 6 (`QUARTER_SLOTS`) | the first and second quarter ARF | the quarter ARFs | `LAST_FRAME` / `ALTREF_FRAME` of the leaves beside them |
/// | 7 | unused (the key's copy is never read back) | the key | -- |
///
/// A frame names exactly two of them plus `GOLDEN_SLOT`
/// ([`Av1Encoder::encode_pyramid_inter`] fills all seven `ref_frame_idx`
/// entries from `last_slot`/`altref_slot`), and `refresh_frame_flags` is the
/// single `self_slot` bit, so no level ever overwrites a slot another level
/// is still reading.
const QUARTER_SLOTS: [u8; 2] = [5, 6];

/// [`Pyramid::default`]'s fourth level (`None` = the three-level pyramid).
/// The long-GOP sweep's winner (`lanes/pyr6.sweep.txt`).
const QUARTER_DEFAULT: Option<i16> = None;

/// [`Pyramid::default`]'s third level (`None` = the two-level pyramid), the
/// long-GOP sweep's winner (`lanes/pyr5.sweep.txt`): every one of the four
/// long-GOP columns improves against the two-level shape, film B (the wider
/// gap) by 6.0 / 2.4 BD points.
const MID_DEFAULT: Option<i16> = Some(-8);

/// [`Pyramid::default`]'s key-frame offset: the long-GOP sweep's winner
/// (`lanes/keyq.sweep.txt`, 48 pictures, gop 48, BD-rate vs libaom cpu-used 6
/// / vs rav1e speed 6), which the 12-frame gate agrees with, so it is NOT
/// gated on GOP length the way the third level is:
///
/// | key offset | film A (48) | film B (48) | film A (12) | film B (12) |
/// |---|---|---|---|---|
/// | 0 (base q) | +52.0 / +11.7 | +144.5 / +44.3 | +43.3 / +15.0 | +66.9 / +35.1 |
/// | -16 | +48.8 / +9.4 | +138.0 / +40.2 | | |
/// | -32 | +45.3 / +6.7 | +132.8 / +36.8 | | |
/// | -48 | +43.5 / +4.6 | +131.3 / +35.0 | +38.5 / +8.2 | +58.2 / +26.5 |
/// | -64 | +44.9 / +3.8 | +140.7 / +37.5 | | |
///
/// `-64` overshoots on film B, and a deeper ARF (`-40`) under the same key
/// gives bytes back on both films (+44.4 / +5.1, +137.4 / +38.6), so what the
/// group was short of is the ANCHOR, not more ARF. The bars rows move by
/// 0.3 / 0.6 BD points and the screen capture is byte-identical (its pyramid
/// is gated off).
const KEY_DEFAULT: i16 = -48;

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
    /// The flat path's delay queue: pictures whose packets later calls
    /// return, each coded with the ones behind it as its lookahead window
    /// (lane-av1facade, deepened to [`crate::encode::tpl_depth`] - 1 on
    /// lane-av1tpl2). Unused in pyramid mode, which buffers in `pending`.
    held: std::collections::VecDeque<Picture>,
    /// Which of [`ANCHOR_SLOTS`] the group about to be coded reads as its
    /// anchor.
    anchor: usize,
    /// Coding-order position of the next packet — a packet's `dts`.
    next_dts: u64,
    /// Set by [`Av1Encoder::encode_sequence_pyramid`]: every coded frame's
    /// own cropped [`Encoded`] (its display position and its reconstruction),
    /// which is what [`crate::encode::EncodedSequence`] carries and a packet
    /// does not. `None` for every other caller, which pays nothing for it.
    collected: Option<Vec<(u64, Encoded)>>,
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
    /// [`Av1Encoder::new`] at a SPEED PRESET (`speed` 0..=10, rav1e's scale:
    /// 0 is the full search this encoder has always run and is byte for byte
    /// identical to [`Av1Encoder::new`]; higher presets switch search levers
    /// off, [`crate::speed::levers`] naming which).
    ///
    /// The preset is process-global (the tile search reads it from worker
    /// threads -- see [`crate::speed`]), so it is set here for every encode
    /// that follows on this process, and two encoders at different speeds
    /// must not run concurrently. `EC_AV1_SPEED=<n>` is the same knob for a
    /// caller that does not construct the encoder itself.
    ///
    /// # Errors
    /// [`Av1Encoder::new`]'s.
    pub fn with_speed(config: EncoderConfig, speed: u8) -> Result<Self> {
        crate::speed::set_speed(speed);
        Self::new(config)
    }

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
            held: std::collections::VecDeque::new(),
            anchor: 0,
            next_dts: 0,
            collected: None,
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
        let rate_loop = rate.into_loop(&mut config, Some(pyramid));
        let mut encoder = Self::with_pyramid(config, pyramid)?;
        encoder.rate_loop = rate_loop;
        Ok(encoder)
    }

    /// The [`Pyramid`] this stream is actually coding under: what
    /// [`Av1Encoder::with_pyramid`] asked for, or `None` once the content gate
    /// in [`Av1Encoder::encode_frames`] has dropped a screen-content stream
    /// back to the flat path. Meaningful after the first picture has been
    /// handed in.
    #[must_use]
    pub fn pyramid(&self) -> Option<Pyramid> {
        self.pyramid
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

    /// Takes one picture and returns the PREVIOUS picture's packet, key or
    /// inter by this stream's `gop` cadence — the facade's one frame of
    /// latency (the picture just handed in is what the previous one is coded
    /// against as its lookahead). The FIRST call has no previous picture and
    /// returns a packet with an EMPTY `data` (its other fields are
    /// placeholders; test `data.is_empty()`), and the last picture's packet
    /// comes out of [`Av1Encoder::flush`] — so a caller that concatenates
    /// `data` in call order still gets the whole stream, provided it flushes
    /// at the end.
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
        Ok(packets.pop().unwrap_or(Packet {
            data: Vec::new(),
            key: false,
            order: 0,
            dts: 0,
            level: Level::Leaf,
        }))
    }

    /// Takes one picture and returns every packet it completed, in coding
    /// order. Without a [`Pyramid`] that is one packet — the PREVIOUS
    /// picture's, since this one is its lookahead — and none at all on the
    /// first call; with one it is
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
        // THE CONTENT GATE (lane-av1pyrgate): the pyramid is for camera
        // material. A desktop capture is the one content class every pyramid
        // sweep measured WORSE under it, and it is exactly the class the
        // screen detector separates (`encode::screen_content`: 0/48 frames on
        // both real films, 48/48 on the OBS capture), so a stream whose first
        // picture reads as screen content drops back to the flat path and
        // codes byte for byte what it coded before the pyramid existed.
        //
        // Decided ONCE, on the first picture, not per mini-GOP: the flat path
        // keeps its references in `reference`/`golden`/`prev2` and the pyramid
        // path in `dpb`, so a switch after the first frame would read slots
        // the other path never filled. Per-GOP re-decision needs the two
        // reference sets kept in step first; the gate's own recipe codes one
        // GOP per stream, where the two are the same decision.
        if self.pyramid.is_some()
            && self.next_index == 0
            && crate::encode::picture_is_screen(picture)
        {
            self.pyramid = None;
        }
        let Some(pyramid) = self.pyramid else {
            // The delay queue: the pictures behind this one are the
            // lookahead window the one at its head is coded with, which is
            // what makes the facade's bytes identical to
            // `encode_sequence`'s (lane-av1facade, lane-av1tpl2).
            self.held.push_back(picture.clone());
            if self.held.len() <= crate::encode::tpl_depth() - 1 {
                return Ok(Vec::new());
            }
            let previous = self.held.pop_front().expect("queue is over depth");
            let window: Vec<Picture> = self.held.iter().cloned().collect();
            return Ok(vec![self.encode_flat(&previous, &window)?]);
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
        // The group closes on the size the LAYOUT gives it (which depends on
        // how many pictures are left before the next key frame, so no short
        // tail group is left behind), or early when the next picture would be
        // a key frame.
        let run_start = self.pending[0].0;
        let remaining = self.config.gop - (run_start % self.config.gop as u64) as usize;
        let next_is_key = (order + 1).is_multiple_of(self.config.gop as u64);
        if self.pending.len() >= group_target(remaining, pyramid.mini_gop, absorb_tail())
            || next_is_key
        {
            return self.drain_pending();
        }
        Ok(Vec::new())
    }

    /// Codes and returns every picture still held back — the last picture of
    /// a flat stream (which has no lookahead and is coded without one), or
    /// the tail of a pyramid stream whose last mini-GOP never filled up.
    /// Empty once there is nothing held.
    ///
    /// # Errors
    /// As [`Av1Encoder::encode_frames`].
    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        crate::encode::arm_tiles(self.config.tile_cols_log2, self.config.tile_rows_log2);
        if !self.held.is_empty() {
            let mut packets = Vec::with_capacity(self.held.len());
            while let Some(previous) = self.held.pop_front() {
                let window: Vec<Picture> = self.held.iter().cloned().collect();
                packets.push(self.encode_flat(&previous, &window)?);
            }
            return Ok(packets);
        }
        self.drain_pending()
    }

    /// The flat path: one picture and its lookahead (the next source picture,
    /// `None` at the end of the stream) in, one packet out — the same inputs
    /// [`crate::encode::encode_sequence`] gives each of its inter frames, so
    /// the bytes are identical.
    fn encode_flat(&mut self, picture: &Picture, lookahead: &[Picture]) -> Result<Packet> {
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
                // The flat path's own slot plan, in order hints: the key frame
                // of the GOP this picture sits in is what `GOLDEN_FRAME`
                // names, and every reference but `ALTREF_FRAME` is the frame
                // just coded.
                crate::encode::flat_order_hints(
                    order as u32 & 0x7f,
                    (order - order % self.config.gop as u64) as u32 & 0x7f,
                    self.prev2.is_some(),
                ),
                render,
                self.carried_cdfs.as_ref().map(|c| &c.0),
                self.golden.as_ref(),
                self.prev2.as_ref(),
                &self.fctx,
                None,
                &lookahead.iter().map(|n| n.padded_to(SUPERBLOCK)).collect::<Vec<_>>(),
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

    /// THE SHARED MINI-GOP DRIVER: codes a whole sequence — one key frame
    /// then all inter, which is [`crate::encode::encode_sequence`]'s only
    /// shape — through this very facade, under `pyramid`, and hands back what
    /// that path returns. The two are byte-identical BY CONSTRUCTION rather
    /// than by a mirrored second implementation: there is one reordering,
    /// one DPB/slot policy and one set of per-level quantizer offsets, here
    /// (`encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence`
    /// still pins it, now over a pyramid sequence too).
    ///
    /// The content gate lives in [`Av1Encoder::encode_frames`] and applies
    /// unchanged, so a screen-content clip handed in here codes flat.
    ///
    /// `EncodedSequence::frames` comes back in DISPLAY order, one entry per
    /// source picture — the order the flat path's coding order happens to be,
    /// and the order every reader of it (the BD gate's sample-exactness
    /// assertions against a decoder's output) needs. `stream` stays in coding
    /// order, which is the order a decoder reads.
    ///
    /// # Errors
    /// As [`Av1Encoder::encode_frames`].
    pub(crate) fn encode_sequence_pyramid(
        pictures: &[Picture],
        base_q_idx: u8,
        pyramid: Pyramid,
        tiles: (u32, u32),
    ) -> Result<crate::encode::EncodedSequence> {
        let first = pictures.first().ok_or_else(|| {
            Error::unsupported("AV1 encode", "a sequence needs at least one picture")
        })?;
        let config = EncoderConfig {
            width: first.width,
            height: first.height,
            base_q_idx,
            // One key frame, at picture 0, and inter for the rest.
            gop: pictures.len().max(1),
            // `encode_sequence` writes an unspecified colour config.
            colour: Colour::Unspecified,
            tile_cols_log2: tiles.0,
            tile_rows_log2: tiles.1,
        };
        let mut encoder = Self::with_pyramid(config, pyramid)?;
        encoder.collected = Some(Vec::with_capacity(pictures.len()));
        let mut stream = Vec::new();
        for picture in pictures {
            for packet in encoder.encode_frames(picture)? {
                stream.extend_from_slice(&packet.data);
            }
        }
        for packet in encoder.flush()? {
            stream.extend_from_slice(&packet.data);
        }
        let mut collected = encoder.collected.take().expect("collecting");
        // Every source picture is coded exactly once, so a frame's display
        // order IS its index once the list is sorted by it.
        let coding_order: Vec<usize> =
            collected.iter().map(|(order, _)| *order as usize).collect();
        collected.sort_by_key(|(order, _)| *order);
        let frames: Vec<Encoded> = collected.into_iter().map(|(_, encoded)| encoded).collect();
        assert!(
            coding_order.iter().all(|&i| i < frames.len()),
            "a coded frame's display position is outside the sequence"
        );
        Ok(crate::encode::EncodedSequence { stream, frames, coding_order })
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
        let base = self
            .rate_loop
            .as_ref()
            .map_or(i16::from(self.config.base_q_idx), |r| i16::from(r.q_idx(Level::Key)));
        let offset = self.pyramid.map_or(0, |p| p.key_q_offset);
        let base_q_idx = (base + offset).clamp(1, 255) as u8;
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
        if let Some(collected) = self.collected.as_mut() {
            collected.push((order, cropped.clone()));
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
        // The quantizer offset comes from the CALLER, not from `level`: a
        // three-level group codes two `Level::Arf` frames (the group's top
        // ARF and the mid one) at different offsets, and they share the
        // rate-control slot but not the offset.
        offset: i16,
    ) -> Result<Packet> {
        let render = (self.config.width, self.config.height);
        let order_hint = (order & 0x7f) as u32;
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
        let slots = [
            last_slot, last_slot, last_slot, GOLDEN_SLOT, last_slot, last_slot, altref_slot,
        ];
        for (i, slot) in slots.into_iter().enumerate() {
            sign_bias[i] = ahead(slot, &self.dpb);
        }
        // The same seven slots' order hints, for `skipModeAllowed` (spec
        // 5.9.22) in the frame-header writer.
        let order_hints: [u32; 7] = slots.map(|slot| {
            self.dpb[slot as usize]
                .as_ref()
                .map_or(0, |s| (s.order_hint & 0x7f) as u32)
        });
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
            order_hints,
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
            // The pyramid path reorders pictures, so its own buffer is not
            // a lookahead window: the temporal lambda weighting is off on
            // this path (lane-av1tpl).
            &[],
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
        if let Some(collected) = self.collected.as_mut() {
            collected.push((order, cropped.clone()));
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
        let pyramid = self.pyramid.expect("pyramid mode");
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
                pyramid.leaf_q_offset,
            )?);
            self.anchor = 1 - self.anchor;
            return Ok(packets);
        }
        // The group's top level: the last picture, hidden, off the anchor.
        packets.push(self.encode_pyramid_inter(
            arf_order,
            &arf_picture,
            anchor_slot,
            next_anchor_slot,
            GOLDEN_SLOT,
            false,
            Level::Arf,
            pyramid.arf_q_offset,
        )?);
        let leaves = group.len() - 1;
        // THE THIRD LEVEL: a second hidden frame at the middle of the leaf
        // run, coded off the anchor with the top ARF as its own backward
        // reference. It needs a leaf on both sides of it, so a group of
        // three or fewer stays two-level.
        //
        // AND IT NEEDS A RUN LONG ENOUGH TO AMORTISE IT: a GOP that is one
        // single mini-GOP (`group_target` absorbs a tail, so a 12-picture GOP
        // under `mini_gop` 8 is ONE group of 11) pays for the second hidden
        // frame out of eleven pictures and reads +2.8 BD points worse against
        // libaom on film B at every mid offset measured (-4: +71.1, -8:
        // +71.2, against the two-level +68.4), while the 48-picture GOP -- six
        // groups sharing the same machinery -- is 6.0 points BETTER. So the
        // third level is switched on by the RUN's length, not by the group's:
        // the same GOP-length dependence `group_target` itself was built for.
        let long_run = self.config.gop > pyramid.mini_gop + pyramid.mini_gop / 2;
        let mid = pyramid.mid_q_offset.filter(|_| leaves >= 3 && long_run).map(|offset| {
            let at = (leaves - 1) / 2;
            (at, offset)
        });
        if let Some((at, offset)) = mid {
            let (order, picture) = group[at].clone();
            packets.push(self.encode_pyramid_inter(
                order,
                &picture,
                anchor_slot,
                MID_SLOT,
                next_anchor_slot,
                false,
                Level::Arf,
                offset,
            )?);
        }
        // THE FOURTH LEVEL: one hidden frame at the middle of each half of
        // the leaf run, so no leaf is more than one picture from a hidden
        // reference on either side -- the depth the references run (rav1e
        // speed 6 codes 23 hidden frames per 48 pictures, libaom four levels
        // per 16-picture group) and the lever three levels had run out of.
        // It hangs off the mid level, and it needs a leaf on both sides of
        // both quarter points, which a run of fewer than seven leaves cannot
        // give.
        let quarters = match (mid, pyramid.quarter_q_offset) {
            (Some((at, _)), Some(offset)) if leaves >= 7 => {
                Some(([(at - 1) / 2, (at + leaves) / 2], offset))
            }
            _ => None,
        };
        if let Some((at, offset)) = quarters {
            // The first quarter predicts forward off the group's anchor and
            // backward off the mid ARF; the second starts FROM the mid ARF
            // and reads the group's top ARF backward.
            for (i, &pos) in at.iter().enumerate() {
                let (order, picture) = group[pos].clone();
                let (last_slot, altref_slot) = match i {
                    0 => (anchor_slot, MID_SLOT),
                    _ => (MID_SLOT, next_anchor_slot),
                };
                packets.push(self.encode_pyramid_inter(
                    order,
                    &picture,
                    last_slot,
                    QUARTER_SLOTS[i],
                    altref_slot,
                    false,
                    Level::Arf,
                    offset,
                )?);
            }
        }
        // Every hidden frame INSIDE the leaf run, by leaf index, in display
        // order: what a leaf reads forward (the nearest one behind it) and
        // backward (the nearest one ahead of it, the group's top ARF when
        // there is none).
        let mut inner: Vec<(usize, u8)> = Vec::new();
        if let Some((at, _)) = mid {
            inner.push((at, MID_SLOT));
        }
        if let Some((at, _)) = quarters {
            inner.extend(at.iter().zip(QUARTER_SLOTS).map(|(&at, slot)| (at, slot)));
        }
        inner.sort_unstable();
        let (seq, _) = crate::encode::key_frame_headers(
            self.config.width,
            self.config.height,
            self.config.base_q_idx,
        )?;
        let show_existing = |encoder: &mut Self, slot: u8, order: u64| -> Result<Packet> {
            let mut data = crate::obu::temporal_delimiter();
            data.extend_from_slice(&crate::frame::show_existing_frame_obu(&seq, slot)?);
            Ok(encoder.packet(data, false, order, Level::ShowExisting))
        };
        // The leaves, in display order, so every `show_existing_frame` lands
        // in its own display position. Each leaf names the nearest hidden
        // frame BEHIND it as `LAST_FRAME` (the group's anchor for the first
        // leaf, otherwise the previous leaf) and the nearest one AHEAD as
        // `ALTREF_FRAME` (the group's top ARF when none is closer).
        for (i, (order, picture)) in group[..leaves].iter().enumerate() {
            if let Some(&(_, slot)) = inner.iter().find(|(at, _)| *at == i) {
                let packet = show_existing(self, slot, *order)?;
                packets.push(packet);
                continue;
            }
            let last_slot = match i {
                0 => anchor_slot,
                i => inner
                    .iter()
                    .find(|(at, _)| at + 1 == i)
                    .map_or(LEAF_SLOT, |&(_, slot)| slot),
            };
            let altref_slot = inner
                .iter()
                .find(|(at, _)| *at > i)
                .map_or(next_anchor_slot, |&(_, slot)| slot);
            packets.push(self.encode_pyramid_inter(
                *order,
                picture,
                last_slot,
                LEAF_SLOT,
                altref_slot,
                true,
                Level::Leaf,
                pyramid.leaf_q_offset,
            )?);
        }
        let packet = show_existing(self, next_anchor_slot, arf_order)?;
        packets.push(packet);
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

    /// Drives a whole stream through the facade the way a caller must since
    /// it holds one picture of lookahead: every picture through
    /// [`Av1Encoder::encode_frames`], then [`Av1Encoder::flush`].
    fn encode_all(enc: &mut Av1Encoder, pictures: &[Picture]) -> Vec<Packet> {
        let mut packets = Vec::with_capacity(pictures.len());
        for (i, picture) in pictures.iter().enumerate() {
            packets.extend(
                enc.encode_frames(picture)
                    .unwrap_or_else(|e| panic!("picture {i}: {e}")),
            );
        }
        packets.extend(enc.flush().unwrap_or_else(|e| panic!("flush: {e}")));
        packets
    }

    /// The mini-GOP layout leaves no short tail group, and leaves the long
    /// GOP the fixed rule already coded alone: 47 inter pictures are
    /// 8 x 5 + 7 under both rules (so a 48-picture GOP is byte-identical),
    /// while a 12-picture GOP stops being 8 + 3 (lane-gopad).
    #[test]
    fn the_mini_gop_layout_leaves_no_short_tail() {
        let groups = |remaining: usize, mini_gop: usize, absorb: bool| {
            let (mut left, mut out) = (remaining, Vec::new());
            while left > 0 {
                let g = group_target(left, mini_gop, absorb).min(left);
                assert!(g > 0);
                out.push(g);
                left -= g;
            }
            out
        };
        // gop 48: what mini_gop 8 already coded, under either rule.
        assert_eq!(groups(47, 8, true), vec![8, 8, 8, 8, 8, 7]);
        assert_eq!(groups(47, 8, false), vec![8, 8, 8, 8, 8, 7]);
        // gop 12: 8 + 3 becomes one group of 11.
        assert_eq!(groups(11, 8, false), vec![8, 3]);
        assert_eq!(groups(11, 8, true), vec![11]);
        // A run shorter than a group is one group, as before (the 4-picture
        // pin clip).
        assert_eq!(groups(3, 8, true), vec![3]);
        // No group is ever below half the shape, and none is over 1.5x it.
        for mini_gop in 2..=16 {
            for remaining in 1..=64 {
                for g in groups(remaining, mini_gop, true) {
                    assert!(g * 2 <= 3 * mini_gop, "{remaining}/{mini_gop}: long {g}");
                    assert!(
                        g * 2 >= mini_gop || remaining < mini_gop,
                        "{remaining}/{mini_gop}: short tail {g}"
                    );
                }
            }
        }
    }

    /// Every OBU stream this test writes to `ffmpeg`/`ffprobe`, concatenated
    /// in order.
    fn concat(packets: &[Packet]) -> Vec<u8> {
        packets.iter().flat_map(|p| p.data.clone()).collect()
    }

    /// The facade's latency contract (lane-av1facade, deepened on
    /// lane-av1tpl2): the first [`crate::encode::tpl_depth`] - 1 calls hold
    /// their picture back and return an empty packet, every later call
    /// returns the packet of the picture that many frames earlier, and
    /// [`Av1Encoder::flush`] yields the whole queue — so a caller still gets
    /// exactly one non-empty packet per picture, `depth - 1` calls later.
    #[test]
    fn one_in_one_out_delayed_by_the_lookahead_depth() {
        let _knobs = crate::speed::knob_read();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 2,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let delay = crate::encode::tpl_depth() as u64 - 1;
        let pictures = delay + 5;
        let mut enc = Av1Encoder::new(config).unwrap();
        for t in 0..pictures {
            let packet = enc.encode(&test_card(64, 64, t as usize)).unwrap();
            if t < delay {
                assert!(packet.data.is_empty(), "call {t} holds its picture back");
                continue;
            }
            assert!(!packet.data.is_empty(), "picture {t}: empty packet");
            assert_eq!(packet.order, t - delay, "picture {t}: order");
        }
        let tail = enc.flush().unwrap();
        assert_eq!(tail.len() as u64, delay, "flush yields the whole queue");
        for (i, p) in tail.iter().enumerate() {
            assert_eq!(p.order, pictures - delay + i as u64, "flushed packet {i}: order");
            assert!(!p.data.is_empty(), "flushed packet {i}: empty");
        }
        assert!(enc.flush().unwrap().is_empty(), "nothing is held twice");
    }

    /// A screen-like card: hard-edged 4-pixel bands of two flat luma values
    /// with a moving block over the lower half — few colours per 16x16 block
    /// at high variance, which is exactly what
    /// [`crate::encode::picture_is_screen`] separates. Used unforced, so the
    /// gate below runs the real detector rather than an override.
    fn screen_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let band = if (x / 4).is_multiple_of(2) { 40 } else { 200 };
                let over = y >= height / 2 && (x + shift) % width < width / 3;
                picture.y[y * width + x] = if over { 128 } else { band };
            }
        }
        picture
    }

    /// THE ENTRY-SURFACE GATE (lane-av1facade, extended to the pyramid on
    /// lane-av1pyrdef): the streaming facade — what the editor's export
    /// drives — codes byte for byte what [`crate::encode::encode_sequence`]
    /// codes, which is the path the BD gate measures. Both now drive ONE
    /// mini-GOP driver ([`Av1Encoder::encode_sequence_pyramid`]), so the
    /// identity is by construction; this pins it anyway, over both content
    /// classes the content gate splits:
    ///
    ///   * film-like (the gate's own clip when the fixture is there, a
    ///     multi-tone moving card otherwise): the pyramid is EFFECTIVE, the
    ///     stream is reordered (a hidden `ALTREF` per mini-GOP), and the two
    ///     paths' whole streams are identical;
    ///   * screen-like: the content gate drops BOTH paths back to flat, one
    ///     packet per picture, identical frame by frame as well as whole.
    ///
    /// Never skipped, since a skipped identity gate is the defect it is meant
    /// to catch.
    #[test]
    fn the_facade_codes_the_same_bytes_as_encode_sequence() {
        let _knobs = crate::speed::knob_read();
        let frames = 12usize;
        let film = h264_clip_frames(640, 384, frames).unwrap_or_else(|| {
            eprintln!("no h264 clip: the facade identity gate runs on a synthetic card");
            (0..frames).map(|t| test_card(64, 64, t)).collect()
        });
        let screen: Vec<Picture> = (0..frames).map(|t| screen_card(64, 64, t)).collect();
        let pyramid = Pyramid::from_env().expect("the pyramid is this build's default");
        for (content, pictures, is_screen) in
            [("film", &film, false), ("screen", &screen, true)]
        {
            let (width, height) = (pictures[0].width, pictures[0].height);
            for q in [150u8, 60] {
                let config = EncoderConfig {
                    width,
                    height,
                    base_q_idx: q,
                    // One key frame then all inter, which is
                    // `encode_sequence`'s only shape, and the gate's recipe.
                    gop: frames,
                    // `encode_sequence` writes an unspecified colour config.
                    colour: Colour::Unspecified,
                    tile_cols_log2: 0,
                    tile_rows_log2: 0,
                };
                let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
                let mut packets = Vec::new();
                for picture in pictures {
                    packets.extend(enc.encode_frames(picture).unwrap());
                }
                packets.extend(enc.flush().unwrap());
                assert_eq!(
                    enc.pyramid().is_none(),
                    is_screen,
                    "{content} q={q}: the content gate read this clip the other way round"
                );
                let sequence = crate::encode::encode_sequence(pictures, q, DEADZONE).unwrap();
                assert_eq!(
                    (packets.iter().map(|p| p.data.len()).sum::<usize>(), concat(&packets)),
                    (sequence.stream.len(), sequence.stream.clone()),
                    "{content} q={q}: the facade's stream is not the sequence path's"
                );
                assert_eq!(
                    sequence.frames.len(),
                    frames,
                    "{content} q={q}: one entry per picture, in display order"
                );
                if is_screen {
                    assert_eq!(packets.len(), frames, "{content} q={q}: one packet per picture");
                    for (i, (packet, coded)) in packets.iter().zip(&sequence.frames).enumerate() {
                        let cropped = crate::encode::crop_encoded(coded, width, height);
                        assert_eq!(
                            (packet.data.len(), packet.data.clone()),
                            (cropped.stream.len(), cropped.stream.clone()),
                            "{content} q={q} frame {i}: the facade's bytes are not the \
                             sequence path's"
                        );
                    }
                } else {
                    assert!(
                        packets.iter().any(|p| p.level == Level::Arf),
                        "{content} q={q}: a non-screen stream coded no hidden frame"
                    );
                }
            }
        }
    }

    /// THE CONTENT GATE (lane-av1pyrgate): the coding pyramid is asked for by
    /// the caller but applied only to camera material. Over screen content
    /// the encoder must drop back to the flat path and code byte for byte
    /// what a flat encoder codes — that is what keeps the BD gate's screen
    /// row identical to the flat baseline — and over non-screen content the
    /// same request must still reorder (class `gate-blind-to-feature`: a gate
    /// that only checked the screen half would pass with the pyramid deleted).
    #[test]
    fn the_content_gate_keeps_screen_streams_flat() {
        let _knobs = crate::speed::knob_read();
        let frames = 8usize;
        let pictures: Vec<Picture> = (0..frames).map(|t| test_card(64, 64, t)).collect();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 120,
            gop: frames,
            colour: Colour::Unspecified,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        crate::encode::force_screen(Some(true));
        let mut flat = Av1Encoder::new(config).unwrap();
        let flat_packets = encode_all(&mut flat, &pictures);
        let mut gated = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
        let gated_packets = encode_all(&mut gated, &pictures);
        assert_eq!(gated.pyramid(), None, "the gate left a screen stream on the pyramid");
        assert_eq!(
            concat(&gated_packets),
            concat(&flat_packets),
            "a screen stream's bytes are not the flat path's"
        );
        crate::encode::force_screen(Some(false));
        let mut kept = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
        let kept_packets = encode_all(&mut kept, &pictures);
        assert_eq!(kept.pyramid(), Some(Pyramid::default()), "the gate ate a film stream");
        assert!(
            kept_packets.iter().any(|p| p.level == Level::Arf),
            "a non-screen stream coded no hidden frame"
        );
        crate::encode::force_screen(None);
    }

    /// A key frame every `gop` pictures, inter otherwise — checked against
    /// what the facade itself reports, and (below) against what `ffprobe`
    /// reads back out of the coded bytes.
    #[test]
    fn gop_cadence_is_honored() {
        let _knobs = crate::speed::knob_read();
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
        let pictures: Vec<Picture> = (0..7).map(|t| test_card(64, 64, t)).collect();
        let keys: Vec<bool> = encode_all(&mut enc, &pictures)
            .iter()
            .map(|p| p.key)
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
        let _knobs = crate::speed::knob_read();
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

        let pictures: Vec<Picture> = (0..30).map(|t| test_card(width, height, t)).collect();
        let packets: Vec<Packet> = encode_all(&mut enc, &pictures);
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
        let _knobs = crate::speed::knob_read();
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
        let packets = encode_all(&mut enc, &[test_card(64, 64, 0)]);
        let colour = ffprobe_colour(&packets[0].data);
        assert_eq!(colour, "tv,bt709,bt709,bt709", "ffprobe colour fields");
    }

    /// The same, for BT.601: a different set of CICP integers must produce a
    /// different string, or the wiring could be a no-op that happens to read
    /// as BT.709 for every input.
    #[test]
    fn bt601_limited_colour_is_reported_by_ffprobe() {
        let _knobs = crate::speed::knob_read();
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
        let packets = encode_all(&mut enc, &[test_card(64, 64, 0)]);
        let colour = ffprobe_colour(&packets[0].data);
        assert_eq!(
            colour, "tv,smpte170m,smpte170m,smpte170m",
            "ffprobe colour fields"
        );
    }

    /// A frame whose size does not match the encoder's configured geometry
    /// is refused by name.
    #[test]
    fn geometry_mismatch_is_refused() {
        let _knobs = crate::speed::knob_read();
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
        let _knobs = crate::speed::knob_read();
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
        // lane-av1speed3: the wall tables below are read on REAL film as
        // well as on the colour-bar fixture (`bars` vs `film` in the BD
        // gate's own vocabulary), and the film crop lives outside the repo --
        // `EC_AV1_WALL_CLIP` names it. Unset everywhere else, so every other
        // caller still gets the fixture.
        let clip = match std::env::var("EC_AV1_WALL_CLIP") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4"),
        };
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
        let _knobs = crate::speed::knob_read();
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
        let sizes: Vec<usize> = encode_all(&mut enc, &pictures)
            .iter()
            .map(|p| p.data.len())
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
        let _knobs = crate::speed::knob_read();
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
    ///
    /// The SEEDING frame is the one exemption ([`RateLoop::update`]): a slot
    /// that has coded nothing yet is still sitting on the caller's
    /// `base_q_idx` guess, and its first step is the acquisition jump, not
    /// windup. Exactly one frame is exempt -- the loop below asserts the
    /// bound on every frame after it, which is where a scene cut would
    /// break it.
    #[test]
    fn bytes_per_frame_controller_never_oscillates_past_its_clamp() {
        let _knobs = crate::speed::knob_read();
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
        // The first frame this encoder EMITS is several `encode_frames` calls
        // in (the lookahead queue holds the earlier ones), so the seeding
        // step is not frame zero -- it is the first frame `q` moves at all.
        let mut seeded = false;
        let mut seed_frame = None;
        for (frame, picture) in pictures.iter().enumerate() {
            enc.encode_frames(picture).unwrap();
            let q = enc.rate_loop.as_ref().unwrap().q_idx(Level::Leaf);
            let step = (i32::from(q) - i32::from(prev_q)).abs();
            if step > 0 && !seeded {
                seeded = true;
                seed_frame = Some(frame);
            } else {
                assert!(
                    f64::from(step) <= RateLoop::STEP_CLAMP + 1.0, // +1 for u8 rounding
                    "frame {frame}: q stepped from {prev_q} to {q}, past the {} clamp",
                    RateLoop::STEP_CLAMP
                );
            }
            prev_q = q;
        }
        assert!(
            seed_frame.is_some(),
            "the controller never steered at all -- the bound below is vacuous"
        );
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
        let _knobs = crate::speed::knob_read();
        pyramid_round_trip(Pyramid { mini_gop: 4, mid_q_offset: None, ..Pyramid::default() }, 2);
    }

    /// The same round trip over a THREE-level group (`mid_q_offset`): each
    /// mini-GOP now codes two hidden frames -- the top ARF at the group's
    /// last picture and the mid ARF at the middle of the leaf run -- and
    /// re-outputs both with `show_existing_frame`, so a mid frame emitted in
    /// the wrong display position, or twice, fails the display-order compare
    /// against ffmpeg exactly as a top ARF does.
    #[test]
    fn a_three_level_pyramid_stream_decodes_in_display_order() {
        let _knobs = crate::speed::knob_read();
        pyramid_round_trip(
            Pyramid { mini_gop: 8, mid_q_offset: Some(-8), ..Pyramid::default() },
            2,
        );
    }

    /// The same round trip over a FOUR-level group
    /// ([`Pyramid::quarter_q_offset`]): a mini-GOP of eight now codes four
    /// hidden frames -- the top ARF at picture 8, the mid at 4 and the two
    /// quarters at 2 and 6 -- each re-output by its own
    /// `show_existing_frame`. Every leaf reads a different pair of DPB slots
    /// than it did at three levels, so a slot map that lets one level
    /// overwrite a picture another is still reading shows up here as a
    /// display-order mismatch against ffmpeg.
    #[test]
    fn a_four_level_pyramid_stream_decodes_in_display_order() {
        let _knobs = crate::speed::knob_read();
        pyramid_round_trip(
            Pyramid {
                mini_gop: 8,
                mid_q_offset: Some(-8),
                quarter_q_offset: Some(4),
                ..Pyramid::default()
            },
            4,
        );
    }

    /// [`Pyramid::key_q_offset`] reaches the key frame: the whole run reads
    /// the key, so an offset that never arrived would be invisible in the BD
    /// table except as "no change" (class symbol-consumption-gap). A finer
    /// key must cost strictly more bytes than the same picture at base q.
    #[test]
    fn the_key_q_offset_reaches_the_key_frame() {
        let _knobs = crate::speed::knob_read();
        crate::encode::force_screen(Some(false));
        let key_bytes = |offset: i16| {
            let config = EncoderConfig {
                width: 128,
                height: 128,
                base_q_idx: 120,
                gop: 32,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let pyramid = Pyramid { key_q_offset: offset, ..Pyramid::default() };
            let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
            let packets = enc.encode_frames(&test_card(128, 128, 0)).unwrap();
            let key = packets.iter().find(|p| p.level == Level::Key).expect("key frame");
            key.data.len()
        };
        let (base, finer) = (key_bytes(0), key_bytes(-48));
        eprintln!("key at base q {base} bytes, at base q - 48 {finer} bytes");
        assert!(finer > base, "key_q_offset -48 did not reach the key frame ({finer} <= {base})");
    }

    fn pyramid_round_trip(pyramid: Pyramid, hidden: usize) {
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
        // The content gate ([`Av1Encoder::encode_frames`]) would drop a
        // screen-content stream to the flat path, and a synthetic test card
        // is exactly the few-colour picture the detector fires on: this gate
        // is about the reordering, so it asks for camera content explicitly.
        crate::encode::force_screen(Some(false));
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
        assert_eq!(shown, hidden, "one show_existing_frame per hidden frame");
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
        assert_eq!(hist(Level::Arf), hidden, "hidden frames");
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

    /// A STATIC clip: every inter frame predicts perfectly from the last, so
    /// the search takes the 64x64 root ([`crate::encode::B64_ROOT`]) instead
    /// of coding four 32x32 quadrants, and the stream both decoders
    /// reconstruct is still sample-exact against the encoder's own
    /// reconstruction. The counter is the point (class
    /// `gate-blind-to-feature`): without it a green three-way compare says
    /// nothing about whether a single 64x64 block was ever written.
    #[test]
    fn a_static_clip_codes_64x64_roots_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3).map(|_| still.clone()).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_root_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let roots = crate::encode::take_b64_root_hits();
        // 8 superblocks per frame, two inter frames.
        assert!(
            roots > 0,
            "a static clip coded no 64x64 root at all (of 16 inter superblocks)"
        );
        eprintln!("64x64 roots on a static clip: {roots} of 16 inter superblocks");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
        }
    }

    /// The same clip with a DC step between frames (lane-tx64): the 64x64
    /// root still predicts the whole superblock from the last frame, but the
    /// prediction is uniformly off, so the root's NON-skip arm -- one
    /// TX_64X64 luma transform and two TX_32X32 chroma ones -- prices below
    /// its skip arm and a 64x64 residual is really written. The counter is
    /// the gate (class `gate-blind-to-feature`; the static clip above cannot
    /// reach this arm at all because its prediction is exact), and both
    /// decoders have to reconstruct those coefficients sample-exact -- a
    /// writer that names the wrong coefficient set or the wrong `txfm_split`
    /// desyncs the whole tile here.
    #[test]
    fn a_dc_stepped_clip_codes_a_64x64_residual_and_decodes_sample_exact() {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3)
            .map(|t| {
                let mut p = still.clone();
                let step = (t * 6) as u16;
                for v in &mut p.y {
                    *v = (*v + step).min(255);
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_residual_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let residuals = crate::encode::take_b64_residual_hits();
        assert!(
            residuals > 0,
            "a DC-stepped clip coded no 64x64 root with a residual at all"
        );
        eprintln!("64x64 roots with a residual: {residuals} of 16 inter superblocks");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
        }
    }

    #[test]
    fn every_tile_layout_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_read();
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
            for packet in encode_all(&mut enc, &sources) {
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

    /// Every SHIPPED SPEED PRESET codes a stream both decoders reconstruct
    /// sample-exact -- ours and ffmpeg's -- and codes a DIFFERENT stream from
    /// its neighbour (a preset that changes no byte is a preset that buys no
    /// wall: class `gate-blind-to-feature`). Preset 0's bytes are the
    /// full-search ones, which is the cheap in-suite statement of the byte
    /// pins' rule.
    ///
    /// Ignored by default because the preset is PROCESS-GLOBAL
    /// ([`crate::speed`]): flipping it while the suite's other tests encode
    /// in parallel would move their bytes under them. Run it on its own:
    /// `cargo test -p ec-av1 --release -- --ignored every_speed_preset`.
    #[test]
    #[ignore = "sets the process-global speed preset: run it alone"]
    fn every_speed_preset_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (640usize, 384usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let mut sizes: Vec<(u8, usize)> = Vec::new();
        for speed in [0u8, 3, 6, 10] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 4,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 1,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::with_speed(config, speed).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), sources.len(), "speed {speed}: our decoder's frames");
            if have_ffmpeg() {
                let theirs = ffmpeg_decode_luma(&stream, width, height);
                assert_eq!(theirs.len(), sources.len(), "speed {speed}: ffmpeg's frames");
                for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                    let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                    if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                        panic!(
                            "speed {speed} frame {i}: luma differs first at ({}, {}): \
                             ours {} vs ffmpeg {}",
                            at % width,
                            at / width,
                            got[at],
                            b[at],
                        );
                    }
                }
            } else {
                eprintln!("SKIP the ffmpeg half of speed {speed}: no ffmpeg");
            }
            eprintln!(
                "speed {speed}: {} bytes [{}]",
                stream.len(),
                crate::speed::levers(speed).join(", ")
            );
            sizes.push((speed, stream.len()));
        }
        crate::speed::set_speed(0);
        for w in sizes.windows(2) {
            assert_ne!(
                w[0].1, w[1].1,
                "speed {} and {} coded the same bytes: the preset changed no decision",
                w[0].0, w[1].0
            );
        }
    }

    /// What tiles and tile threads are worth at the size the editor exports
    /// at: 1920x1080, eight real pictures, two passes per cell (ABAB), wall
    /// per cell. Ignored by default (minutes).
    #[test]
    #[ignore = "1080p wall table: minutes, run it with --ignored"]
    fn tile_wall_table_at_1080p() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (1920usize, 1080usize);
        let Some(sources) = h264_clip_frames(width, height, 8) else {
            eprintln!("SKIP tile_wall_table_at_1080p: no fixture");
            return;
        };
        let layouts = [(0u32, 0u32), (1, 0), (1, 1), (2, 1)];
        let threadings = [1usize, 2, 4, 8];
        let mut table: Vec<(String, usize, f64, usize)> = Vec::new();
        for pass in 0..2 {
            for &(cols_log2, rows_log2) in &layouts {
                for &threads in &threadings {
                    crate::par::set_tile_threads(threads);
                    let config = EncoderConfig {
                        width,
                        height,
                        base_q_idx: 120,
                        gop: 8,
                        colour: Colour::Bt709Limited,
                        tile_cols_log2: cols_log2,
                        tile_rows_log2: rows_log2,
                    };
                    let mut enc = Av1Encoder::new(config).unwrap();
                    let start = std::time::Instant::now();
                    let mut bytes = 0usize;
                    for packet in encode_all(&mut enc, &sources) {
                        bytes += packet.data.len();
                    }
                    let wall = start.elapsed().as_secs_f64();
                    let name = format!("{}x{}", 1 << cols_log2, 1 << rows_log2);
                    match table.iter_mut().find(|r| r.0 == name && r.1 == threads) {
                        Some(row) => row.2 = row.2.min(wall),
                        None => table.push((name, threads, wall, bytes)),
                    }
                    let _ = pass;
                }
            }
        }
        crate::par::set_tile_threads(1);
        eprintln!("| tiles | threads | wall (s, best of 2) | bytes |");
        for (name, threads, wall, bytes) in &table {
            eprintln!("| {name} | {threads} | {wall:.2} | {bytes} |");
        }
    }

    /// The tiles of a frame are entropy-independent, so the bytes must not
    /// depend on how many workers wrote them: the same stream at one tile
    /// thread and at four, per layout.
    #[test]
    fn tile_bytes_do_not_depend_on_the_thread_count() {
        let _knobs = crate::speed::knob_write();
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
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
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

    /// What the per-tile search (lane-av1tsearch) is worth in wall and in
    /// frames per second at the editor's own export sizes: every tile layout
    /// crossed with every tile-thread count, plus librav1e at the matching
    /// `tile_cols`/`tile_rows`/`threads` as the reference point. Two passes
    /// over the whole table (A,B,A,B... rather than AA,BB) with the best of
    /// the two kept, so a thermal drift cannot order the rows.
    ///
    /// Measurement, not a pass/fail gate: it asserts only that every cell
    /// produced a stream.
    #[test]
    #[ignore = "wall measurement: minutes, run it with --ignored --nocapture"]
    fn tile_search_wall_1080p() {
        let _knobs = crate::speed::knob_write();
        tile_search_wall(1920, 1080, 12, &[(0, 0), (1, 0), (1, 1), (2, 1)], &[1, 2, 4, 8]);
    }

    /// The same table at the 4K frame size the editor exports (3840x1608),
    /// at the two layouts with enough tiles to fill this box.
    #[test]
    #[ignore = "wall measurement: minutes, run it with --ignored --nocapture"]
    fn tile_search_wall_4k() {
        let _knobs = crate::speed::knob_write();
        tile_search_wall(3840, 1608, 6, &[(2, 1), (3, 2)], &[1, 8, 12]);
    }

    /// lane-av1fpar: the frame-level FILTER stage is banded across the same
    /// workers the tile search uses (`par::override_filter_threads`), so the
    /// deblock/CDEF replays, the per-64 SSE and the plane copies all run in
    /// pieces. Every piece is a disjoint band of the same integers, so the
    /// stream must not move -- at a size that crops and is several superblock
    /// rows tall, which is what makes the bands non-trivial.
    #[test]
    fn filter_stage_bytes_do_not_depend_on_the_thread_count() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (384usize, 288usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let coded = |threads: usize| {
            crate::par::set_tile_threads(threads);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            stream
        };
        // One tile, so only the filter stage can differ between these.
        let one = coded(1);
        for threads in [2usize, 4, 8] {
            assert_eq!(one, coded(threads), "the filter stage moved at {threads} threads");
        }
        crate::par::set_tile_threads(1);
    }

    /// lane-av1fpar: the filter search scores its ~30 candidates by replaying
    /// the loop filters on one captured reconstruction instead of decoding
    /// the coded tile again per candidate. The replay must be that decode,
    /// byte for byte -- including at a size that CROPS (1080 and 1608 both
    /// pad to a superblock row, i.e. every export size the editor uses),
    /// which is the arm this lane opened.
    #[test]
    fn filter_replay_codes_the_same_stream_as_a_full_decode() {
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let run = |width: usize, height: usize, off: bool| {
            let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
            crate::decode::set_filter_replay_disabled(off);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            crate::decode::set_filter_replay_disabled(false);
            stream
        };
        // 320x160 crops (the coding surface is 320x192, one superblock row
        // taller); 320x192 is a whole number of superblocks, the shape the
        // replay already covered.
        for (width, height) in [(320usize, 160usize), (320, 192)] {
            let replayed = run(width, height, false);
            let decoded = run(width, height, true);
            assert_eq!(
                replayed,
                decoded,
                "{width}x{height}: the filter replay is not the decode it stands in for \
                 ({} bytes replayed, {} decoded)",
                replayed.len(),
                decoded.len(),
            );
        }
    }

    /// lane-av1cap: the filter stage's FINAL replay -- the one that replaced
    /// the capture decode -- against that decode. `set_verify_final_replay`
    /// makes `encode::pick_and_apply_filters` run both on every frame and
    /// assert, per frame, that the spliced picture and the two filter stages
    /// the loop-restoration search reads are bit-identical.
    #[test]
    fn filter_replay_final_matches_the_capture_decode() {
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let run = |sources: &[Picture], width: usize, height: usize, tiles: (u32, u32)| {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: tiles.0,
                tile_rows_log2: tiles.1,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            for packet in encode_all(&mut enc, sources) {
                assert!(!packet.data.is_empty());
            }
        };
        crate::decode::set_verify_final_replay(true);
        // 320x160 crops (its coding surface is one superblock row taller);
        // 320x192 is a whole number of superblocks.
        for (width, height) in [(320usize, 160usize), (320, 192)] {
            let sources: Vec<Picture> =
                (0..4).map(|t| test_card(width, height, t * 3)).collect();
            run(&sources, width, height, (0, 0));
        }
        // Real gate content, where the CDEF preset search actually chooses
        // `bits > 0` and the tile is re-coded, one tile and four.
        if let Some(sources) = h264_clip_frames(640, 384, 3) {
            run(&sources, 640, 384, (0, 0));
            run(&sources, 640, 384, (1, 1));
        } else {
            eprintln!("SKIP the gate-clip arm: no fixture or no ffmpeg");
        }
        crate::decode::set_verify_final_replay(false);
    }

    /// lane-av1fpar: where a frame's wall actually goes, stage by stage --
    /// the per-tile search, the tile write, the frame-level filter search
    /// (with its deblock/CDEF replay and per-64 SSE inside it), the capture
    /// decode the loop-restoration search rides on, and that search itself.
    /// Measurement, not a pass/fail gate.
    #[test]
    #[ignore = "wall measurement: minutes, run it with --ignored --nocapture"]
    fn filter_stage_wall_1080p() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(1920, 1080, 8, (2, 1), &[8]);
    }

    #[test]
    #[ignore = "wall measurement: minutes, run it with --ignored --nocapture"]
    fn filter_stage_wall_4k() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(3840, 1608, 4, (2, 1), &[8, 12]);
    }

    /// The same breakdown at the real films' own crop size. With
    /// `EC_AV1_WALL_CLIP` pointing at a film window this is the film row; with
    /// it unset it is the colour-bar fixture at the same size, which is what
    /// makes the pair a comparison rather than a number.
    #[test]
    #[ignore = "wall measurement: minutes, run it with --ignored --nocapture"]
    fn filter_stage_wall_film() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(1920, 768, 4, (2, 1), &[8]);
    }

    fn filter_stage_wall(
        width: usize,
        height: usize,
        frames: usize,
        (cols_log2, rows_log2): (u32, u32),
        threads: &[usize],
    ) {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let Some(sources) = h264_clip_frames(width, height, frames) else {
            eprintln!("SKIP the filter-stage breakdown: no fixture or no ffmpeg");
            return;
        };
        crate::par::set_stage_times(true);
        for &t in threads {
            crate::par::set_tile_threads(t);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let _ = crate::par::take_stage_ns();
            let start = std::time::Instant::now();
            for packet in encode_all(&mut enc, &sources) {
                assert!(!packet.data.is_empty());
            }
            let wall = start.elapsed().as_secs_f64();
            let ns = crate::par::take_stage_ns();
            let ms = |i: usize| ns[i] as f64 / 1e6;
            eprintln!(
                "\n{width}x{height}, {frames} frames, {}x{} tiles, {t} tile threads: \
                 {wall:.2}s wall, {:.2} fps",
                1 << cols_log2,
                1 << rows_log2,
                frames as f64 / wall,
            );
            eprintln!("| stage | ms | ms/frame | % of wall |");
            for (i, name) in crate::par::STAGES.iter().enumerate() {
                eprintln!(
                    "| {name} | {:.0} | {:.1} | {:.1}% |",
                    ms(i),
                    ms(i) / frames as f64,
                    100.0 * ms(i) / (wall * 1e3),
                );
            }
            // Top-level stages only (the deblock/CDEF/SSE rows are inside
            // the filter-search total); the rest is per-frame setup, the
            // source conversion and the splice.
            let top: f64 = [
                crate::par::S_TILE_SEARCH,
                crate::par::S_TILE_WRITE,
                crate::par::S_FILTER,
                crate::par::S_CAPTURE,
                crate::par::S_LR,
            ]
            .iter()
            .map(|&i| ms(i))
            .sum();
            eprintln!(
                "accounted {:.1}% of wall; outside the tile search {:.1}%",
                100.0 * top / (wall * 1e3),
                100.0 * (1.0 - ms(crate::par::S_TILE_SEARCH) / (wall * 1e3)),
            );
        }
        crate::par::set_stage_times(false);
        crate::par::set_tile_threads(1);
    }

    fn tile_search_wall(
        width: usize,
        height: usize,
        frames: usize,
        layouts: &[(u32, u32)],
        threads: &[usize],
    ) {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let Some(sources) = h264_clip_frames(width, height, frames) else {
            eprintln!("SKIP the tile-search wall table: no fixture or no ffmpeg");
            return;
        };
        let run = |cols_log2: u32, rows_log2: u32, t: usize| -> (std::time::Duration, usize) {
            crate::par::set_tile_threads(t);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let start = std::time::Instant::now();
            let mut bytes = 0usize;
            for packet in encode_all(&mut enc, &sources) {
                bytes += packet.data.len();
            }
            (start.elapsed(), bytes)
        };
        let cells: Vec<((u32, u32), usize)> = layouts
            .iter()
            .flat_map(|&l| threads.iter().map(move |&t| (l, t)))
            .collect();
        let mut best: Vec<Option<(std::time::Duration, usize)>> = vec![None; cells.len()];
        for _pass in 0..2 {
            for (i, &((c, r), t)) in cells.iter().enumerate() {
                let got = run(c, r, t);
                assert!(got.1 > 0, "{}x{} tiles at {t} threads: empty stream", 1 << c, 1 << r);
                if best[i].is_none_or(|(w, _)| got.0 < w) {
                    best[i] = Some(got);
                }
            }
        }
        crate::par::set_tile_threads(1);
        eprintln!("\n{width}x{height}, {frames} frames, base_q_idx 120 -- best of two passes");
        eprintln!("| layout | tiles | threads | wall | fps | speedup vs 1 tile 1 thread | bytes |");
        let base = best[0].expect("the first cell ran").0.as_secs_f64();
        for (i, &((c, r), t)) in cells.iter().enumerate() {
            let (wall, bytes) = best[i].expect("every cell ran");
            let secs = wall.as_secs_f64();
            eprintln!(
                "| {}x{} | {} | {t} | {secs:.2}s | {:.2} | {:.2}x | {bytes} |",
                1 << c,
                1 << r,
                1 << (c + r),
                frames as f64 / secs,
                base / secs,
            );
        }
        // Parallel efficiency per layout: what the extra threads actually
        // took off that layout's own single-threaded wall, and so what share
        // of the added cores sat idle.
        for &(c, r) in layouts {
            let one = cells
                .iter()
                .position(|&(l, t)| l == (c, r) && t == threads[0])
                .map(|i| best[i].expect("cell").0.as_secs_f64());
            let Some(one) = one else { continue };
            for &t in &threads[1..] {
                let i = cells.iter().position(|&(l, tt)| l == (c, r) && tt == t).expect("cell");
                let secs = best[i].expect("cell").0.as_secs_f64();
                let workers = (t.min(1 << (c + r))) as f64 / threads[0] as f64;
                let speedup = one / secs;
                eprintln!(
                    "{}x{} tiles, {} -> {t} threads: {speedup:.2}x of a possible {workers:.2}x, \
                     idle share {:.0}%",
                    1 << c,
                    1 << r,
                    threads[0],
                    100.0 * (1.0 - speedup / workers).max(0.0),
                );
            }
        }
        rav1e_wall_reference(width, height, frames, layouts, threads);
    }

    /// librav1e at the same sizes, layouts and thread counts, through ffmpeg
    /// -- the reference the wall table is read against.
    fn rav1e_wall_reference(
        width: usize,
        height: usize,
        frames: usize,
        layouts: &[(u32, u32)],
        threads: &[usize],
    ) {
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        let listed = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output();
        let has_rav1e = listed
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("librav1e"))
            .unwrap_or(false);
        if !has_rav1e || !clip.exists() {
            eprintln!("SKIP the rav1e reference: no librav1e or no fixture");
            return;
        }
        eprintln!("| rav1e layout | threads | wall | fps | bytes |");
        for &(c, r) in layouts {
            for &t in threads {
                let out = std::env::temp_dir().join(format!("ec-av1-rav1e-{}.ivf", std::process::id()));
                let params = format!(
                    "speed=6:quantizer=100:tile_cols={}:tile_rows={}:threads={t}",
                    1 << c,
                    1 << r
                );
                let start = std::time::Instant::now();
                let status = Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-i", clip.to_str().unwrap()])
                    .args(["-frames:v", &frames.to_string()])
                    .args(["-vf", &format!("scale={width}:{height}")])
                    .args(["-c:v", "librav1e", "-rav1e-params", &params])
                    .arg(out.to_str().unwrap())
                    .output()
                    .expect("ffmpeg failed to run");
                let secs = start.elapsed().as_secs_f64();
                if !status.status.success() {
                    eprintln!("| {}x{} | {t} | FAILED: {} |", 1 << c, 1 << r, String::from_utf8_lossy(&status.stderr).lines().last().unwrap_or(""));
                    continue;
                }
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(&out);
                eprintln!(
                    "| {}x{} | {t} | {secs:.2}s | {:.2} | {bytes} |",
                    1 << c,
                    1 << r,
                    frames as f64 / secs
                );
            }
        }
    }

    /// The same round trip at a real 1920x1080 crop of the gate's own clip
    /// -- the size the editor's export actually runs at, where a tile grid
    /// is worth having. Ignored by default only for its wall (a 1080p
    /// encode of three pictures), not for any weakness in the check.
    #[test]
    #[ignore = "1080p encode: minutes, run it with --ignored"]
    fn a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_read();
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
            for packet in encode_all(&mut enc, &sources) {
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
        let _knobs = crate::speed::knob_read();
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
            for packet in encode_all(&mut enc, &pictures) {
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
