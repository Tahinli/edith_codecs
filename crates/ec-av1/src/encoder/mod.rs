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

/// What one coded frame is worth, relative to a leaf, as a function of how
/// much FINER than the leaf it is coded (`leaf_q_offset - own offset`, in
/// `q_idx` steps): `exp(WEIGHT_LEVEL_GAIN * delta)`.
///
/// This is the level model the byte budget is split by, and it is MEASURED,
/// not the old flat "a hidden frame is worth two leaves" guess: the long-GOP
/// census (`lanes/rc.report.md`, films A and B at three quantizers each,
/// `lanes/census-longgop-v2.md`'s q150 row among them) reads a level's
/// bytes/frame as very close to log-linear in its own q offset, at
/// d(ln bytes)/d(q_idx) = -0.0557 -- three and a half times the SLOPE of the
/// controller's own `RateLoop::GAIN` (0.0154), because a deeper level is not
/// just quantized finer, it also carries the group's residual for every
/// frame that predicts off it.
const WEIGHT_LEVEL_GAIN: f64 = 0.0557;

/// The key frame's own correction on top of `WEIGHT_LEVEL_GAIN`: an intra
/// frame at the key's offset codes this much of what the level model alone
/// predicts (the census's key frames are consistently smaller than the pure
/// exponential, since the exponential is fitted on INTER levels whose size
/// keeps growing with reference depth).
const KEY_WEIGHT: f64 = 1.08;

/// `EC_AV1_RL_CENSUS=1` — one stderr line per frame the rate loop steers.
fn census_on() -> bool {
    std::env::var("EC_AV1_RL_CENSUS").is_ok_and(|v| v == "1")
}

/// One level of a run, as the re-plan sees it: how much of the run's weight
/// it carries, how much of that is still to be coded, and -- once its
/// effective quantizer has hit the coder's 1..=255 bound -- the leaf-
/// equivalent bytes it actually spends there.
#[derive(Debug, Clone, Copy)]
struct LevelPlan {
    /// The controller slot this level shares ([`RateLoop::slot`]).
    slot: usize,
    /// The level's own `base_q_idx` offset: what tells two hidden levels
    /// apart inside slot 1.
    offset: i16,
    weight: f64,
    left: f64,
    bound: bool,
    rate: f64,
}

impl LevelPlan {
    fn new(slot: usize, offset: i16, weight: f64) -> Self {
        Self {
            slot,
            offset,
            weight,
            left: weight,
            bound: false,
            rate: 0.0,
        }
    }
}

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
    /// The leaf's own q offset: the zero of the level weight
    /// ([`WEIGHT_LEVEL_GAIN`]), so slot 1's target can be read in
    /// LEAF-EQUIVALENT bytes and scaled per frame by that frame's offset.
    leaf_q_offset: i16,
    /// The key frame's own leaf-equivalent weight ([`KEY_WEIGHT`]).
    key_weight: f64,
    /// The leaf-equivalent weight of a whole run (key included), and the
    /// run's whole byte budget (`gop` frame targets): what a run RE-PLANS
    /// from, and what a key frame resets the re-plan to.
    run_weight: f64,
    run_bytes: f64,
    /// What is left of both, ahead of the next frame.
    left_weight: f64,
    left_bytes: f64,
    /// The run's shape, ONE ENTRY PER LEVEL: the key, each distinct hidden
    /// offset, the leaves. A level whose effective quantizer has hit a bound
    /// cannot follow the plan, so [`RateLoop::replan`] prices its remaining
    /// weight at what it MEASURABLY spends and re-solves the run's budget
    /// over the levels that can still move. Per LEVEL and not per slot: the
    /// hidden slot mixes the top and mid ARFs, whose leaf-equivalent rates
    /// differ by 4x at the floor (both code the same bytes at `q_idx` 1, at
    /// weights that differ by that much), and projecting one onto the other's
    /// remaining weight drove the whole plan negative.
    plans: [Option<LevelPlan>; 5],
    /// The plan's scale at construction — the WINDUP BOUND on the re-plan:
    /// a level held at its bound hands its unspent bytes to the others, but
    /// no level's target may exceed this many times its own opening share.
    scale0: f64,
    /// `EC_AV1_RL_CENSUS=1`: one line per coded frame on stderr.
    census: bool,
    coded: usize,
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
    /// The windup bound on [`RateLoop::replan`]: however much a bound level
    /// fails to spend, no other level's target may grow past this multiple of
    /// its opening share. Without it a run whose hidden levels are all at the
    /// floor would keep piling the whole run's budget onto the leaves.
    const REPLAN_CLAMP: f64 = 4.0;
    /// The same bound the other way: a re-plan may not drive a level's target
    /// below this multiple of its opening share. A target of zero would STOP
    /// the loop outright ([`RateLoop::update`] returns on a non-positive
    /// target), which is how the first cut of this fix froze every level of a
    /// run at whatever quantizer it had reached.
    const REPLAN_FLOOR: f64 = 0.25;

    fn new(target_bytes: f64, start_q: f64, pyramid: Option<Pyramid>, gop: usize) -> Self {
        let Some(p) = pyramid else {
            return Self {
                target: [target_bytes; 3],
                q: [start_q; 3],
                per_level: false,
                seeded: [false; 3],
                leaf_q_offset: 0,
                key_weight: 1.0,
                run_weight: 0.0,
                run_bytes: 0.0,
                left_weight: 0.0,
                left_bytes: 0.0,
                plans: [None; 5],
                scale0: target_bytes,
                census: census_on(),
                coded: 0,
            };
        };
        // THE LEVEL MODEL. Every coded frame of the run is priced by its own
        // q offset through `WEIGHT_LEVEL_GAIN`, in units of one leaf, and the
        // run's `gop` frame targets are shared out in those units. The old
        // model priced a group as "one hidden frame worth `ARF_WEIGHT` = 2
        // leaves plus `m - 1` leaves", which was wrong twice over and the two
        // errors cancelled (lane-pyr6): the group has coded TWO hidden frames
        // since the mid level landed (four with
        // [`Pyramid::quarter_q_offset`]), and a hidden frame is worth far
        // more than two leaves -- the census reads the top ARF at 24x a leaf
        // and the mid at 5x. Both are fixed here, in one diff.
        let w = |offset: i16| {
            ((f64::from(p.leaf_q_offset) - f64::from(offset)) * WEIGHT_LEVEL_GAIN).exp()
        };
        let (hidden, leaves) = gop_shape(p, gop);
        let key = KEY_WEIGHT * w(p.key_q_offset);
        let total: f64 =
            key + leaves as f64 + hidden.iter().map(|&(o, n)| n as f64 * w(o)).sum::<f64>();
        // One leaf's byte target: the run is worth `gop` frame targets and
        // `total` leaves.
        let scale = target_bytes * gop.max(1) as f64 / total.max(f64::EPSILON);
        // THE HIDDEN SLOT holds every hidden level at once (`Level::Arf` is
        // the top ARF, the mid and the quarters alike -- one controller,
        // different q offsets), so its target is kept in LEAF-EQUIVALENT
        // bytes and [`RateLoop::update`] scales it by the coded frame's own
        // `w(offset)`. Measuring each hidden frame against its OWN predicted
        // size is what keeps the shared slot from oscillating between the
        // levels it mixes.
        // One plan per level, in coding order: key, hidden levels (at most
        // three distinct offsets -- top, mid, quarters), leaves.
        let mut plans = [None; 5];
        plans[0] = Some(LevelPlan::new(0, p.key_q_offset, key));
        let mut i = 1;
        for &(o, n) in hidden.iter().take(3) {
            plans[i] = Some(LevelPlan::new(1, o, n as f64 * w(o)));
            i += 1;
        }
        plans[i] = Some(LevelPlan::new(2, p.leaf_q_offset, leaves as f64));
        Self {
            target: [scale * key, scale, scale],
            q: [start_q; 3],
            per_level: true,
            seeded: [false; 3],
            leaf_q_offset: p.leaf_q_offset,
            key_weight: key,
            run_weight: total,
            run_bytes: target_bytes * gop.max(1) as f64,
            left_weight: total,
            left_bytes: target_bytes * gop.max(1) as f64,
            plans,
            scale0: scale,
            census: census_on(),
            coded: 0,
        }
    }

    /// What a frame coded `offset` steps off `base_q_idx` is worth in
    /// leaf-equivalent bytes ([`WEIGHT_LEVEL_GAIN`]).
    fn weight(&self, offset: i16) -> f64 {
        ((f64::from(self.leaf_q_offset) - f64::from(offset)) * WEIGHT_LEVEL_GAIN).exp()
    }

    /// Which [`LevelPlan`] a coded frame belongs to. Only the hidden slot
    /// holds more than one level, so only there does the offset decide.
    fn plan_of(&self, slot: usize, offset: i16) -> Option<usize> {
        self.plans
            .iter()
            .position(|p| p.is_some_and(|p| p.slot == slot && (slot != 1 || p.offset == offset)))
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
    ///
    /// `raw_q` is the quantizer the frame WANTED (`base_q_idx + offset`,
    /// before the coder's own 1..=255 clamp): outside that range the level is
    /// at its bound and its share of the budget has to move elsewhere.
    fn update(&mut self, level: Level, offset: i16, raw_q: i16, actual_bytes: usize) {
        let slot = self.slot(level);
        if self.target[slot] <= 0.0 || actual_bytes == 0 {
            return;
        }
        // A hidden frame is measured against ITS OWN level's predicted size:
        // slot 1's target is in leaf-equivalent bytes.
        let frame_weight = match slot {
            0 => self.key_weight,
            1 => self.weight(offset),
            _ => 1.0,
        };
        let target = match self.per_level && slot == 1 {
            true => self.target[slot] * frame_weight,
            false => self.target[slot],
        };
        let ratio = actual_bytes as f64 / target;
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
        let bound = !(1..=255).contains(&raw_q);
        if let Some(plan) = self
            .plan_of(slot, offset)
            .and_then(|i| self.plans[i].as_mut())
        {
            plan.bound = bound;
            plan.rate = actual_bytes as f64 / frame_weight.max(f64::EPSILON);
        }
        if self.census {
            eprintln!(
                "RLCENSUS n={} level={level:?} slot={slot} target={target:.0} actual={actual_bytes} \
                 base_q={:.1} raw_q={raw_q} eff_q={} bound={} left_bytes={:.0} left_w={:.1}",
                self.coded,
                self.q[slot],
                raw_q.clamp(1, 255),
                bound,
                self.left_bytes,
                self.left_weight,
            );
            self.coded += 1;
        }
        self.replan(level, slot, offset, frame_weight, actual_bytes as f64);
    }

    /// Re-plans the REST OF THE RUN from what it has actually spent: the
    /// level model says how the budget SPLITS, and nothing but a measurement
    /// says what a frame of this content costs at all.
    ///
    /// Two frames of a run cannot be steered by the loop and both are pure
    /// model error without this: the KEY, coded once at the caller's seed `q`
    /// before anything has been measured (the census's key frames run 0.5x to
    /// 2.7x the model across the two films), and any level whose `q` has hit
    /// the floor -- at 3 Mbit/s on the gate's clip the hidden slot codes at
    /// `q_idx` 1 and physically cannot spend its share, which the leaves must
    /// then absorb or the stream lands a third under target.
    ///
    /// It is NOT an accumulated error term in `q` ([[vorbis-rate-loop-windup]]
    /// is about exactly that): `q` still moves only by this frame's own
    /// clamped proportional step, the re-plan only moves the TARGET the next
    /// frame is measured against, and every key frame resets the whole plan,
    /// so no debt outlives one run.
    fn replan(
        &mut self,
        level: Level,
        slot: usize,
        offset: i16,
        frame_weight: f64,
        actual_bytes: f64,
    ) {
        if !self.per_level {
            return;
        }
        if level == Level::Key {
            self.left_weight = self.run_weight;
            self.left_bytes = self.run_bytes;
            for plan in self.plans.iter_mut().flatten() {
                plan.left = plan.weight;
            }
        }
        self.left_weight -= frame_weight;
        self.left_bytes -= actual_bytes;
        if let Some(plan) = self
            .plan_of(slot, offset)
            .and_then(|i| self.plans[i].as_mut())
        {
            plan.left = (plan.left - frame_weight).max(0.0);
        }
        // The tail of a run is not re-planned: under a leaf's worth of weight
        // left, the division is noise.
        if self.left_weight <= 1.0 {
            return;
        }
        // A BOUND level is held at what it measurably spends and its weight
        // is taken out of the division, so the bytes it cannot spend land on
        // the levels that can (and, at the ceiling, the other way round). The
        // old code divided the whole remainder by the whole remaining weight,
        // which handed the biggest share of the shortfall straight back to
        // the level that had just failed to spend it -- at the default
        // pyramid the hidden levels carry most of the run's weight, so most
        // of the surplus evaporated there every frame.
        // ABLATION (`EC_AV1_RL_NOREDIST=1`, this lane's before/after arm):
        // the old division, whole remainder over whole remaining weight.
        if std::env::var("EC_AV1_RL_NOREDIST").is_ok_and(|v| v == "1") {
            let scale = (self.left_bytes / self.left_weight).max(0.0);
            self.target = [scale * self.key_weight, scale, scale];
            return;
        }
        let (mut bytes, mut weight) = (self.left_bytes, 0.0);
        for plan in self.plans.iter().flatten() {
            if plan.bound {
                bytes -= plan.rate * plan.left;
            } else {
                weight += plan.left;
            }
        }
        if weight <= 1.0 {
            // Every movable level is spent: nothing left to re-solve over.
            return;
        }
        let scale = (bytes / weight).clamp(
            self.scale0 * Self::REPLAN_FLOOR,
            self.scale0 * Self::REPLAN_CLAMP,
        );
        self.target = [scale * self.key_weight, scale, scale];
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

/// How one GOP of `gop` pictures is actually coded, as [`Av1Encoder::encode_frames`]
/// and [`Av1Encoder::drain_pending`] lay it out: the HIDDEN frames as
/// `(q offset, count)` (the group's top `ALTREF`, the mid level and the two
/// quarter-point ARFs, each at its own offset) and the number of shown
/// leaves. The key frame is the remaining picture and is not listed.
///
/// This is what [`RateLoop::new`] prices the run by, so the two must agree on
/// the gating rules -- a mid level needs three leaves and a run longer than
/// 1.5 mini-GOPs, quarters need seven leaves and a mid -- which
/// `the_rate_loop_prices_the_frames_the_pyramid_codes` pins against the real
/// packet stream.
fn gop_shape(p: Pyramid, gop: usize) -> (Vec<(i16, usize)>, usize) {
    let (mut hidden, mut leaves) = (Vec::<(i16, usize)>::new(), 0usize);
    let mut add = |offset: i16, n: usize| match hidden.iter_mut().find(|(o, _)| *o == offset) {
        Some((_, c)) => *c += n,
        None => hidden.push((offset, n)),
    };
    let long_run = gop > p.mini_gop + p.mini_gop / 2;
    let mut left = gop.saturating_sub(1);
    while left > 0 {
        let g = group_target(left, p.mini_gop, absorb_tail()).min(left);
        left -= g;
        if g == 1 {
            leaves += 1;
            continue;
        }
        let inner = g - 1;
        add(p.arf_q_offset, 1);
        let mid = p.mid_q_offset.filter(|_| inner >= 3 && long_run);
        let quarters = mid.and(p.quarter_q_offset).filter(|_| inner >= 7);
        if let Some(o) = mid {
            add(o, 1);
        }
        if let Some(o) = quarters {
            add(o, 2);
        }
        leaves += inner - usize::from(mid.is_some()) - 2 * usize::from(quarters.is_some());
    }
    (hidden, leaves)
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
            arf_q_offset: f
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.arf_q_offset),
            leaf_q_offset: f
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.leaf_q_offset),
            // A fourth field is the mid-level offset; the literal `off` asks
            // for the two-level pyramid at the same shape (the A/B arm the
            // sweep needs), and no field at all keeps the default's.
            mid_q_offset: match f.next() {
                None => d.mid_q_offset,
                Some("off") => None,
                Some(v) => v.parse().ok().or(d.mid_q_offset),
            },
            // A fifth field is the key frame's own offset.
            key_q_offset: f
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.key_q_offset),
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

/// lane-last2: the two slots the leaf chain alternates its refresh between
/// once [`last2`] is on, so that the shown frame BEFORE `LAST_FRAME` is still
/// in the DPB and can be named `LAST2_FRAME`. Slot 7 is the one the key frame
/// writes and nothing has ever read back, so this costs no other level a slot;
/// leaf `k` refreshes `LEAF_SLOTS[k % 2]`, which is the slot leaf `k - 2`
/// wrote -- read before write, spec 7.20's decode order.
const LEAF_SLOTS: [u8; 2] = [LEAF_SLOT, 7];

/// Whether the leaf chain retains that second past picture and offers it to
/// the block search as `LAST2_FRAME` (`EC_AV1_LAST2=0` / `=1`). The census
/// that priced the lever is `lanes/refs.report.md` §1; what it buys on the
/// gate is `lanes/last2.report.md`.
fn last2() -> bool {
    match LAST2_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    match std::env::var("EC_AV1_LAST2").ok() {
        Some(v) => v != "0",
        None => LAST2,
    }
}

/// A PROCESS-GLOBAL [`last2`] override that wins over the environment
/// (`u8::MAX` = unset), the shape `crate::encode::set_frame_interp` has --
/// this crate's tests never call `set_var` (`crate::envflags`), so the
/// witness turns the lever on through this while holding
/// [`crate::speed::knob_write`].
static LAST2_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`LAST2_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` witness
pub(crate) fn set_last2(on: Option<bool>) {
    LAST2_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// [`last2`]'s default.
const LAST2: bool = false;

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

/// Whether the pyramid path hands each frame a lookahead window, i.e.
/// whether the temporal lambda map (`encode::tpl_lambda_factors`) runs on
/// the DEFAULT stream at all. It did not until lane-arfcen: the pyramid
/// reorders pictures, so this path passed `&[]` and the map -- measured and
/// shipped on the flat path -- was inert on every frame of every pyramid
/// stream, including the ARFs that carry 54% of a long-GOP stream's bytes
/// (class `tool disabled in every gate recipe`).
/// lane-arfpred: the top ARF source temporal filter's strength
/// ([`crate::speed::ARF_TF`]), `EC_AV1_ARF_TF=<strength>` overriding it;
/// `0` is off.
fn arf_tf_strength() -> f64 {
    std::env::var("EC_AV1_ARF_TF")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::ARF_TF))
}

/// lane-arftf: the same filter's strength on the MID ARF, whose own window
/// is the display-FUTURE leaves of its group (`tpl_window(at, false)`).
/// `EC_AV1_ARF_TF_MID=<strength>` overrides [`crate::speed::ARF_TF_MID`];
/// `0` is off.
fn arf_tf_mid_strength() -> f64 {
    std::env::var("EC_AV1_ARF_TF_MID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::ARF_TF_MID))
}

/// lane-arftf: how many of the window's pictures the filter averages in
/// ([`crate::speed::ARF_TF_WIN`], `EC_AV1_ARF_TF_WIN=<n>`). The top ARF's
/// window holds only DISPLAY-PAST neighbours -- it is the group's last
/// picture and the next group's sources are not buffered when it is coded --
/// so this widens the filter backwards only.
pub(crate) fn arf_tf_window() -> usize {
    std::env::var("EC_AV1_ARF_TF_WIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::ARF_TF_WIN))
}

/// lane-lookahead: how many DISPLAY-FUTURE neighbours the top ARF's filter
/// takes out of the buffered next group ([`crate::speed::ARF_TF_WIN_FUT`],
/// `EC_AV1_ARF_TF_WIN_FUT=<n>`). Inert without [`lookahead`].
pub(crate) fn arf_tf_window_future() -> usize {
    match ARF_TF_FUT_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        usize::MAX => {}
        n => return n,
    }
    std::env::var("EC_AV1_ARF_TF_WIN_FUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::ARF_TF_WIN_FUT))
}

/// PROCESS-GLOBAL overrides of the two lane-lookahead knobs, the shape
/// [`set_last2`] has (this crate's tests never call `set_var`); `u8::MAX` /
/// `usize::MAX` is unset, and a witness holds [`crate::speed::knob_write`]
/// while it uses them.
static LOOKAHEAD_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
static ARF_TF_FUT_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
static TPL_FUT_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Sets [`LOOKAHEAD_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` witness
pub(crate) fn set_lookahead(on: Option<bool>) {
    LOOKAHEAD_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Sets [`ARF_TF_FUT_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` witness
pub(crate) fn set_arf_tf_future(n: Option<usize>) {
    ARF_TF_FUT_OVERRIDE.store(
        n.unwrap_or(usize::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Sets [`TPL_FUT_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` witness
pub(crate) fn set_tpl_fut(n: Option<usize>) {
    TPL_FUT_OVERRIDE.store(
        n.unwrap_or(usize::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// lane-tplfut: which window the TOP ARF's temporal lambda map propagates
/// back through ([`crate::speed::TPL_FUT`], `EC_AV1_TPL_FUT=<n>`). Inert
/// without [`lookahead`] -- mode 0 is the shipped past window.
pub(crate) fn tpl_fut() -> usize {
    match TPL_FUT_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        usize::MAX => {}
        n => return n,
    }
    std::env::var("EC_AV1_TPL_FUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::TPL_FUT))
}

/// lane-tplhalf: mode 2's split -- how many display-PAST neighbours the top
/// ARF's window opens with, the rest of the budget coming from the buffered
/// future ([`crate::speed::TPL_FUT_HALF`], `EC_AV1_TPL_FUT_HALF=<n>`).
pub(crate) fn tpl_fut_half() -> usize {
    std::env::var("EC_AV1_TPL_FUT_HALF")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::TPL_FUT_HALF))
}

/// lane-tplhalf: how many entries mode 2's window holds in total, `0` meaning
/// `tpl_depth() - 1` -- the ARF window's budget alone, so raising it does not
/// deepen every other frame's lambda pass the way `EC_AV1_TPL_D` does
/// ([`crate::speed::TPL_FUT_WIN`], `EC_AV1_TPL_FUT_WIN=<n>`).
pub(crate) fn tpl_fut_win(depth: usize) -> usize {
    match std::env::var("EC_AV1_TPL_FUT_WIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| crate::speed::at(&crate::speed::TPL_FUT_WIN))
    {
        0 => depth - 1,
        n => n,
    }
}

/// lane-lookahead: whether the pyramid path holds one group of lookahead
/// ([`crate::speed::LOOKAHEAD`], `EC_AV1_LOOKAHEAD=0|1`).
fn lookahead() -> bool {
    match LOOKAHEAD_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    match std::env::var("EC_AV1_LOOKAHEAD").ok() {
        Some(v) => v != "0",
        None => crate::speed::LOOKAHEAD,
    }
}

fn tpl_pyramid() -> bool {
    match std::env::var("EC_AV1_TPL_PYRAMID").ok() {
        Some(v) => v != "0",
        None => TPL_PYRAMID,
    }
}

/// [`tpl_pyramid`]'s default.
const TPL_PYRAMID: bool = true;

/// Whether a group's TOP ARF names the ARF from TWO groups back as its own
/// `ALTREF_FRAME` (lane-arfcen). It has always named `GOLDEN_SLOT` there,
/// which for a top ARF is the key frame it already reads as `GOLDEN`, i.e.
/// its second reference is the same picture as its first for the first group
/// and adds nothing after: the ARF-vs-ARF census reads
/// `LAST@-8 GOLDEN@-8` on our first ARF where rav1e's reads two DISTINCT
/// anchors. The slot the ARF is about to refresh still holds that older
/// anchor while this frame is coded, so naming it costs no extra DPB slot.
fn arf_altref() -> bool {
    match std::env::var("EC_AV1_ARF_ALTREF").ok() {
        Some(v) => v != "0",
        None => ARF_ALTREF,
    }
}

/// [`arf_altref`]'s default. ON since its long-GOP arm: film A
/// +42.8%/+4.5% -> +40.9%/+3.1% and film B +127.3%/+32.9% -> +125.6%/+32.0%
/// (BD vs libaom cpu-used 6 / vs rav1e speed 6), i.e. both films down on both
/// columns.
const ARF_ALTREF: bool = true;

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
    carried_cdfs: Option<Box<crate::encode::CdfSnapshot>>,
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
    /// lane-lookahead: the CLOSED group waiting for the next group's sources
    /// before it is coded, so its top ARF -- the group's last picture in
    /// display order -- has display-future neighbours available. Always empty
    /// with [`lookahead`] off.
    ready: Vec<(u64, Picture)>,
    /// The flat path's delay queue: pictures whose packets later calls
    /// return, each coded with the ones behind it as its lookahead window
    /// (lane-av1facade, deepened to [`crate::encode::tpl_depth`] - 1 on
    /// lane-av1tpl2). Unused in pyramid mode, which buffers in `pending`.
    held: std::collections::VecDeque<Picture>,
    /// Which of [`ANCHOR_SLOTS`] the group about to be coded reads as its
    /// anchor.
    anchor: usize,
    /// lane-last2: the [`LEAF_SLOTS`] the last two SHOWN leaves refreshed,
    /// most recent first, across mini-GOP boundaries (the first leaf of a
    /// group reads the anchor as `LAST_FRAME`, so its `LAST2_FRAME` is the
    /// previous group's last leaf). Cleared by every key frame.
    leaf_hist: Vec<u8>,
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
///
/// lane-av1enchang: the `CdfSnapshot` is BOXED, not held by value. Eight
/// inline copies made [`Av1Encoder`] 139960 bytes, and that struct is
/// returned by value through three nested `Result<Self>` constructors
/// ([`Av1Encoder::with_pyramid_and_rate_target`] ->
/// [`Av1Encoder::with_pyramid`] -> [`Av1Encoder::new`]), so an unoptimized
/// build put several of those on the stack at once: the pyramid path
/// overflowed libtest's 2 MiB test thread and aborted the whole crate suite
/// with `fatal runtime error: stack overflow`. One heap box per DPB slot
/// brings the struct from 139960 bytes down to 18168 (measured). The tables'
/// VALUES are untouched.
#[derive(Debug, Clone)]
struct DpbSlot {
    picture: Picture,
    order_hint: u32,
    cdfs: Box<crate::encode::CdfSnapshot>,
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
            fctx: crate::decode::FrameCtx::for_encoder(),
            pyramid: None,
            dpb: [const { None }; 8],
            pending: Vec::new(),
            ready: Vec::new(),
            held: std::collections::VecDeque::new(),
            anchor: 0,
            leaf_hist: Vec::new(),
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
        // lane-split: the split census prints on every exit of this call when
        // armed, so no `return` path loses it; the counters are cumulative, so
        // the last table a run prints is that run's total.
        struct SplitCensusDump;
        impl Drop for SplitCensusDump {
            fn drop(&mut self) {
                if crate::encode::split_census_on() {
                    crate::encode::dump_split_census();
                }
                if crate::encode::newmv_census_on() {
                    crate::encode::dump_newmv_census();
                }
            }
        }
        // `then_some` would CONSTRUCT the guard eagerly and drop it when the
        // flag is off, which runs `Drop` -- and printed the census header on
        // every unarmed encode. `then` builds it only when armed.
        let _census = (crate::encode::split_census_on() || crate::encode::newmv_census_on())
            .then(|| SplitCensusDump);
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
            if self.held.len() < crate::encode::tpl_depth() {
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
            let mut packets = self.drain_all(Some(picture))?;
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
        self.drain_all(None)
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
                None,
                &self.fctx,
                None,
                &lookahead
                    .iter()
                    .map(|n| n.padded_to(SUPERBLOCK))
                    .collect::<Vec<_>>(),
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
            // The flat path codes every frame at the slot's own q, no offset.
            rate_loop.update(level, 0, i16::from(base_q_idx), cropped.stream.len());
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
        let coding_order: Vec<usize> = collected.iter().map(|(order, _)| *order as usize).collect();
        collected.sort_by_key(|(order, _)| *order);
        let frames: Vec<Encoded> = collected.into_iter().map(|(_, encoded)| encoded).collect();
        assert!(
            coding_order.iter().all(|&i| i < frames.len()),
            "a coded frame's display position is outside the sequence"
        );
        Ok(crate::encode::EncodedSequence {
            stream,
            frames,
            coding_order,
        })
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
            // `next_cdfs` is already a `Box<CdfSnapshot>` (lane-av1stacksweep),
            // so this is a pointer copy rather than a second boxing -- the
            // same fix lane-av1enchang made here, now that the producer hands
            // over a box instead of an inline 15232-byte `Cdfs`.
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
            .map_or(i16::from(self.config.base_q_idx), |r| {
                i16::from(r.q_idx(Level::Key))
            });
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
        // Every leaf slot now holds the key frame, which `GOLDEN_FRAME`
        // already names: the chain starts empty again.
        self.leaf_hist.clear();
        let cropped = crop_encoded(&encoded, render.0, render.1);
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(Level::Key, 0, base + offset, cropped.stream.len());
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
        // lane-last2: the slot read as `LAST2_FRAME`. `None` -- and any slot
        // holding no picture, or the same one `LAST_FRAME` reads -- leaves
        // the reference set exactly as it was before this lane.
        last2_slot: Option<u8>,
        show_frame: bool,
        level: Level,
        // The quantizer offset comes from the CALLER, not from `level`: a
        // three-level group codes two `Level::Arf` frames (the group's top
        // ARF and the mid one) at different offsets, and they share the
        // rate-control slot but not the offset.
        offset: i16,
        // The lookahead window this frame's temporal lambda map is built
        // from, already padded (lane-arfcen). Empty codes the frame at one
        // lambda, which is what the whole pyramid path used to do.
        lookahead: &[Picture],
        // lane-lookahead: DISPLAY-FUTURE pictures for the SOURCE temporal
        // filter only (the top ARF's, out of the buffered next group), never
        // for the temporal lambda map -- keeping the filter's window and the
        // tpl window separate levers. Empty everywhere else.
        future: &[Picture],
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
        // lane-arfq: the top ARF of a mini-GOP is the hidden frame coded at
        // the pyramid's OWN `arf_q_offset` (the mid and quarter levels are
        // `Level::Arf` too but sit at their own offsets), and it is the level
        // the whole group predicts from -- so it reads its own `delta_q`
        // strength (`crate::speed::DQ_LEVEL_K`).
        let dq_level = match (show_frame, self.pyramid.map(|p| p.arf_q_offset)) {
            (true, _) => crate::encode::DqLevel::Leaf,
            (false, Some(arf)) if arf == offset => crate::encode::DqLevel::TopArf,
            (false, _) => crate::encode::DqLevel::MidArf,
        };
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
        // A LAST2 slot only counts when it holds a picture that is not the
        // one `LAST_FRAME` already names; otherwise the frame codes the
        // reference set it always did.
        let last2_slot = last2_slot.filter(|&s| s != last_slot && self.dpb[s as usize].is_some());
        let slots = [
            last_slot,
            last2_slot.unwrap_or(last_slot),
            last_slot,
            GOLDEN_SLOT,
            last_slot,
            last_slot,
            altref_slot,
        ];
        for (i, slot) in slots.into_iter().enumerate() {
            sign_bias[i] = ahead(slot, &self.dpb);
        }
        // The same seven slots' order hints, for `skipModeAllowed` (spec
        // 5.9.22) in the frame-header writer.
        let order_hints: [u32; 7] = slots.map(|slot| {
            self.dpb[slot as usize]
                .as_ref()
                .map_or(0, |s| s.order_hint & 0x7f)
        });
        let reference = self.dpb[last_slot as usize]
            .as_ref()
            .map(|s| s.picture.clone())
            .ok_or_else(|| {
                Error::unsupported(
                    "AV1 encode",
                    "an inter frame needs a previous reconstruction",
                )
            })?;
        let last2 =
            last2_slot.and_then(|s| self.dpb[s as usize].as_ref().map(|d| d.picture.clone()));
        let golden = self.dpb[GOLDEN_SLOT as usize]
            .as_ref()
            .map(|s| s.picture.clone());
        let altref = (altref_slot != GOLDEN_SLOT)
            .then(|| {
                self.dpb[altref_slot as usize]
                    .as_ref()
                    .map(|s| s.picture.clone())
            })
            .flatten();
        let start_cdfs = self.dpb[last_slot as usize]
            .as_ref()
            .map(|s| s.cdfs.0.clone());
        // lane-arfpred: the top ARF is coded from a TEMPORALLY FILTERED
        // source when `speed::ARF_TF` is non-zero -- the grain it would
        // otherwise spend its residual on is averaged out against the
        // group's own display neighbours (`lookahead`, already padded).
        let source = picture.padded_to(SUPERBLOCK);
        // ... and never at the very top of the quality range, where the
        // anchor's own quantizer has hit the rate loop's floor
        // ([`crate::speed::ARF_TF_QMIN`]).
        let tf = match dq_level {
            crate::encode::DqLevel::TopArf => arf_tf_strength(),
            // lane-arftf: the mid ARF gets its own strength -- its window is
            // the group's later leaves, i.e. the filter runs FORWARD there.
            crate::encode::DqLevel::MidArf => arf_tf_mid_strength(),
            crate::encode::DqLevel::Leaf => 0.0,
        };
        let tf = match base_q_idx >= crate::speed::ARF_TF_QMIN {
            true => tf,
            false => 0.0,
        };
        let source = match tf > 0.0 {
            true => {
                // lane-lookahead: the filter's window is sized HERE -- up to
                // `ARF_TF_WIN` display-past neighbours (nearest first) and
                // then the display-future ones the caller buffered.
                let window: Vec<Picture> = lookahead
                    .iter()
                    .take(arf_tf_window())
                    .chain(future.iter())
                    .cloned()
                    .collect();
                crate::encode::arf_temporal_filter(&source, &window, tf)
            }
            false => source,
        };
        let encoded = encode_inter_frame(
                &source,
                &reference,
                base_q_idx,
                DEADZONE,
                order_hint,
                order_hints,
                render,
                start_cdfs.as_ref(),
                golden.as_ref(),
                altref.as_ref(),
                last2.as_ref(),
                &self.fctx,
                Some(crate::encode::PyramidFrame {
                    last_slot,
                    self_slot,
                    altref_slot,
                    last2_slot,
                    show_frame,
                    sign_bias,
                    dq_level,
                }),
                // lane-arfcen: the group's own sources ARE this frame's window
                // (`tpl_window`); it was `&[]` here, which switched the temporal
                // lambda map off on every frame the default stream codes.
                lookahead,
            );
        // Name the pyramid position in any failure: an error out of the tile
        // writer or the filter search is otherwise indistinguishable between
        // the hidden frame and the leaves that read it.
        let encoded = encoded.map_err(|e| {
            Error::unsupported(
                "AV1 encode",
                format!(
                    "{level:?} frame at order {order} (last slot {last_slot}, self {self_slot}, \
                     altref {altref_slot}, last2 {last2_slot:?}, shown {show_frame}, golden {}, \
                     altref pic {}): {e}",
                    golden.is_some(),
                    altref.is_some(),
                ),
            )
        })?;
        self.refresh(&[self_slot], &encoded, order_hint);
        let cropped = crop_encoded(&encoded, render.0, render.1);
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(level, offset, base + offset, cropped.stream.len());
        }
        if let Some(collected) = self.collected.as_mut() {
            collected.push((order, cropped.clone()));
        }
        Ok(self.packet(cropped.stream, false, order, level))
    }

    /// Closes the open mini-GOP. With the lookahead off that codes it
    /// straight away, which is byte for byte what this encoder did before
    /// lane-lookahead; with it on the open group takes the `ready` slot and
    /// the group ALREADY there is coded, with the open one's sources as its
    /// display-future window.
    fn drain_pending(&mut self) -> Result<Vec<Packet>> {
        if !lookahead() {
            let group = std::mem::take(&mut self.pending);
            return self.code_group(group, &[]);
        }
        let group = std::mem::take(&mut self.ready);
        let next = std::mem::take(&mut self.pending);
        let packets = self.code_group(group, &next);
        self.ready = next;
        packets
    }

    /// Codes every group still buffered, in coding order -- what a key frame
    /// (`next` is the key picture, the last buffered group's own
    /// display-future) and the end of stream (`None`) both need. The tail
    /// group can be any length, down to the single picture `code_group`
    /// codes as one shown leaf.
    fn drain_all(&mut self, next: Option<&Picture>) -> Result<Vec<Packet>> {
        let tail: Vec<(u64, Picture)> = next
            .filter(|_| lookahead())
            .map(|p| vec![(self.next_index, p.clone())])
            .unwrap_or_default();
        let ready = std::mem::take(&mut self.ready);
        let pending = std::mem::take(&mut self.pending);
        let mut packets = self.code_group(ready, &pending)?;
        packets.extend(self.code_group(pending, &tail)?);
        Ok(packets)
    }

    /// Codes one mini-GOP: its hidden frame first, then its leaves, then
    /// the `show_existing_frame` header that puts the hidden frame back in
    /// display order. `future` is the display-future sources the caller has
    /// buffered past this group (lane-lookahead), empty without a lookahead.
    fn code_group(
        &mut self,
        group: Vec<(u64, Picture)>,
        future: &[(u64, Picture)],
    ) -> Result<Vec<Packet>> {
        if group.is_empty() {
            return Ok(Vec::new());
        }
        let mut packets = Vec::with_capacity(group.len() + 1);
        // The lookahead window of the group's picture at index `pos`: the
        // display-order successors inside this group, or -- for the group's
        // top ARF, the last picture, whose successors are not buffered yet --
        // the leaves BEHIND it in reverse display order, which are exactly
        // the pictures that predict backward from it. Padded like the frame
        // itself, as `encode_inter_frame` wants it.
        let tpl_window = |pos: usize, back: bool| -> Vec<Picture> {
            let depth = crate::encode::tpl_depth();
            if !tpl_pyramid() || depth < 2 {
                return Vec::new();
            }
            let window: Vec<&Picture> = match back {
                false => group[pos + 1..].iter().map(|(_, p)| p).collect(),
                true => group[..pos].iter().rev().map(|(_, p)| p).collect(),
            };
            window
                .into_iter()
                .take(depth - 1)
                .map(|p| p.padded_to(SUPERBLOCK))
                .collect()
        };
        // lane-last2: the slot a frame reading `last_slot` as `LAST_FRAME`
        // names as `LAST2_FRAME` -- the most recent shown leaf that is not
        // the one it already reads. `None` with the lever off, or before two
        // distinct past pictures exist.
        let last2_of = |hist: &[u8], last_slot: u8| -> Option<u8> {
            last2()
                .then(|| hist.iter().copied().find(|&s| s != last_slot))
                .flatten()
        };
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
                last2_of(&self.leaf_hist, anchor_slot),
                true,
                Level::Leaf,
                pyramid.leaf_q_offset,
                &tpl_window(0, false),
                &[],
            )?);
            self.anchor = 1 - self.anchor;
            return Ok(packets);
        }
        // lane-lookahead: the display-future half of the top ARF's temporal
        // filter window, padded like the frame itself. Empty when the
        // lookahead is off (`future` is then always empty) or when the filter
        // asks for no future neighbours, which is what keeps the default
        // stream byte-identical.
        let arf_future: Vec<Picture> = future
            .iter()
            .take(arf_tf_window_future())
            .map(|(_, p)| p.padded_to(SUPERBLOCK))
            .collect();
        // lane-tplfut: the top ARF's TEMPORAL LAMBDA window. Mode 0 (and any
        // mode with no buffered future picture) keeps the group's own
        // display-PAST sources, reversed -- the only window that exists
        // without a lookahead, and the one every measurement so far was
        // taken with. The other modes read the next group's sources, which
        // is the direction libaom's tpl looks at an ARF.
        // `n` of the buffered display-future sources, nearest first, padded
        // like the frame being coded.
        let arf_tpl_future = |n: usize| -> Vec<Picture> {
            future
                .iter()
                .take(n)
                .map(|(_, p)| p.padded_to(SUPERBLOCK))
                .collect()
        };
        let arf_tpl: Vec<Picture> = {
            let depth = crate::encode::tpl_depth();
            let past = || tpl_window(group.len() - 1, true);
            match (tpl_fut(), future.is_empty(), tpl_pyramid() && depth >= 2) {
                (_, _, false) => Vec::new(),
                (0, _, _) | (_, true, _) => past(),
                (1, _, _) => arf_tpl_future(depth - 1),
                // Half the budget each side (`EC_AV1_TPL_FUT_HALF=<n>`
                // moves the split), past first: the chain then has
                // ONE discontinuity in the middle (the coarse pass between
                // the nearest past and the nearest future neighbour spans the
                // whole group), which is the arm's known cost.
                (2, _, _) => {
                    let budget = tpl_fut_win(depth);
                    let mut w = past();
                    w.truncate(tpl_fut_half().max(1));
                    let room = budget.saturating_sub(w.len());
                    w.extend(arf_tpl_future(room));
                    w
                }
                // Mode 3 and anything above: the WHOLE buffered next group.
                (_, _, _) => arf_tpl_future(future.len()),
            }
        };
        // The group's top level: the last picture, hidden, off the anchor.
        packets.push(self.encode_pyramid_inter(
            arf_order,
            &arf_picture,
            anchor_slot,
            next_anchor_slot,
            // lane-arfcen: the slot this ARF refreshes still holds the ARF
            // from two groups back, so naming it here gives the frame a
            // SECOND, distinct past anchor instead of the key it already
            // reads as GOLDEN.
            match arf_altref() {
                true => next_anchor_slot,
                false => GOLDEN_SLOT,
            },
            // The ARF levels take no LAST2: the anchor pair already gives a
            // top ARF two distinct past anchors (`arf_altref`), and the mid
            // and quarter frames have only the one past anchor in the DPB.
            None,
            false,
            Level::Arf,
            pyramid.arf_q_offset,
            &arf_tpl,
            // lane-lookahead: the group's top ARF is its LAST picture in
            // display order, so every picture of the next group is a
            // display-future neighbour of it -- nearest first.
            &arf_future,
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
        let mid = pyramid
            .mid_q_offset
            .filter(|_| leaves >= 3 && long_run)
            .map(|offset| {
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
                None,
                false,
                Level::Arf,
                offset,
                &tpl_window(at, false),
                &[],
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
                    None,
                    false,
                    Level::Arf,
                    offset,
                    &tpl_window(pos, false),
                    &[],
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
            // The previous shown leaf's own slot: `LEAF_SLOT` with the
            // lever off, and with it on whichever of [`LEAF_SLOTS`] that leaf
            // refreshed.
            let prev_leaf = self.leaf_hist.first().copied().unwrap_or(LEAF_SLOT);
            let last_slot = match i {
                0 => anchor_slot,
                i => inner
                    .iter()
                    .find(|(at, _)| at + 1 == i)
                    .map_or(prev_leaf, |&(_, slot)| slot),
            };
            // Alternate the refresh: this leaf takes the leaf slot the
            // PREVIOUS leaf did not, which is the one the leaf before that
            // wrote and which this frame may still read as `LAST2_FRAME`
            // (read before write).
            let self_slot = match last2() {
                true => LEAF_SLOTS
                    .into_iter()
                    .find(|&s| Some(s) != self.leaf_hist.first().copied())
                    .unwrap_or(LEAF_SLOT),
                false => LEAF_SLOT,
            };
            let last2_slot = last2_of(&self.leaf_hist, last_slot);
            let altref_slot = inner
                .iter()
                .find(|(at, _)| *at > i)
                .map_or(next_anchor_slot, |&(_, slot)| slot);
            self.leaf_hist.insert(0, self_slot);
            self.leaf_hist.truncate(2);
            packets.push(self.encode_pyramid_inter(
                *order,
                picture,
                last_slot,
                self_slot,
                altref_slot,
                last2_slot,
                true,
                Level::Leaf,
                pyramid.leaf_q_offset,
                &tpl_window(i, false),
                &[],
            )?);
        }
        let packet = show_existing(self, next_anchor_slot, arf_order)?;
        packets.push(packet);
        self.anchor = 1 - self.anchor;
        Ok(packets)
    }
}

#[cfg(test)]
pub(crate) mod tests;
