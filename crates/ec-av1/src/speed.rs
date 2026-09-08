//! The encoder's SPEED PRESET axis (lane-av1fast): one number, 0..=10 like
//! rav1e's `--speed`, that picks a default for every search lever this crate
//! already has a knob for.
//!
//! Speed 0 is today's search, byte for byte -- every table below reads its
//! shipped default at index 0, so `encoder::tests`' byte pins are the pin on
//! that. Higher speeds trade BD-rate for wall by switching off the levers in
//! [`levers`], each of which was already measured on its own (the doc comment
//! on each knob in [`crate::encode`] carries its sweep).
//!
//! The value is PROCESS-GLOBAL, the same shape [`crate::par`]'s thread counts
//! use, because the levers are read from the tile-search worker threads --
//! a thread-local would not reach them. One speed per process at a time:
//! [`crate::encoder::Av1Encoder::with_speed`] sets it at construction, so two
//! encoders at different speeds must not run concurrently in one process.
//! `EC_AV1_SPEED=<n>` is the environment form the gates and `ec-bench` use.
//! Every individual `EC_AV1_*` lever override still wins over the preset.

//! # The lever table (film A, native crop 1920x768, 12 frames)
//!
//! One ablation per lever on top of speed 0, `bd_rate_screen_native`'s film A
//! row (BD-rate vs libaom `cpu-used 6` / rav1e `speed 6`), against the arm's
//! own wall divided by the rav1e anchor's wall in the SAME arm -- the box ran
//! three arms at a time, so only that ratio is comparable (class: wall tables
//! are only comparable inside one interleaved batch). Baseline 75.7s/14.2s =
//! 5.33 = 1.00x. Logs: `lanes/ab-*.log`.
//!
//! | lever switched off | BD vs libaom / rav1e | rel. wall |
//! |---|---|---|
//! | -- (speed 0) | +53.2 / +23.4 | 1.00 |
//! | loop restoration | +53.0 / +23.3 | 0.83 |
//! | warp | +53.0 / +23.3 | 0.88 |
//! | tpl lambda map | +53.2 / +23.4 | 0.85 |
//! | chroma top-2 | +53.3 / +23.4 | 0.79 |
//! | split-RD breakout 0.5 | +53.6 / +23.7 | 0.82 |
//! | extra-reference NEWMV | +53.7 / +23.9 | 0.90 |
//! | **8x8 split** | **+53.8 / +23.6** | **0.41** |
//! | intra top-2 | +54.0 / +24.2 | 0.96 |
//! | CfL + angle delta | +54.1 / +24.2 | 0.71 |
//! | coefficient breakout 1 | +54.3 / +24.3 | 0.73 |
//! | 32x32 tx depth | +54.7 / +24.8 | 0.75 |
//! | inter var-tx | +55.5 / +25.2 | 0.95 |
//! | leaf second-reference search | +58.9 / +27.5 | 0.86 |
//! | leaf compound | +62.8 / +29.7 | 0.74 |
//! | compound | +83.7 / +45.8 | 0.62 |
//!
//! The ladder is that column read greedily by BD per wall: the 8x8 split is
//! more than half the film wall for +0.6 points, three levers (LR, warp, the
//! tpl map) are free or better on real film, and compound -- the tool the old
//! bars-fixture sweeps rated cheap -- is the most expensive thing to lose, so
//! it only goes at speed 9. MEASURED end to end on the same row:
//!
//! Filter intra (merged into speed 0 after the table above was measured) was
//! priced the same way, but at speed 3 rather than 0 -- two sequential arms on
//! film A native, `lanes/fi-s3-{on,off}.log`: ON +54.6 / +24.3 at 43.1s/13.7s
//! anchor = 3.15, OFF +55.1 / +24.7 at 39.6s/14.2s = 2.79. It buys 0.5 BD
//! points for 12.8% wall = 0.039 points per 1% wall, which lands BETWEEN the
//! levers speed 3 already cuts (CfL + angle, 0.031) and the next one it keeps
//! (coefficient breakout, 0.041) -- i.e. right at the greedy frontier, inside
//! this gate's own noise. It ships ON at preset 0 only, where the byte pins
//! live, and off above it, because a preset whose whole purpose is wall does
//! not spend 12.8% of it on half a BD point.
//!
//! # The Pareto table, RE-MEASURED 2026-09-08 (lane-pareto2)
//!
//! Every row is `encode::tests::bd_rate_screen_native` on THIS head: 12
//! frames, gop 12, four quantizers, the native crops (film A 1920x768, film B
//! and the capture 1920x1024), one tile, one thread; BD-rate vs libaom
//! `cpu-used 6` / vs rav1e `speed 6`. The arms ran THREE at a time on a box
//! carrying two other lanes (load 9-25), so the wall is read ONLY as the
//! ours:rav1e-speed-6 ratio inside one arm -- `x rav1e` below -- and the fps
//! column is our own 48 coded frames over our own ladder wall in that same
//! arm. BD is exact across runs; wall is not -- the shipped preset-6 row was
//! re-run with the invariant suite alongside, so its wall columns are the
//! most depressed of the table; the SERIAL pair (control 75.2s, both levers
//! 95.6s on the film A row) is what prices this lane's change: +27% wall for
//! 2.8 BD points, and film B gains 3.4 points on top of that for free.
//!
//! | preset | film A | film B | screen capture | x rav1e s6 (A/B/screen) | 1-thread fps (A/B/screen) |
//! |---|---|---|---|---|---|
//! | 0 | +21.7 / -4.4 | +26.9 / -0.6 | +19.8 / -30.5 | 10.9 / 9.5 / 7.5 | 0.24 / 0.28 / 0.37 |
//! | 3 | +22.3 / -4.1 | +27.3 / -0.3 | +23.7 / -28.4 | 7.4 / 6.6 / 4.4 | 0.40 / 0.48 / 0.60 |
//! | 6 (as it shipped) | +26.6 / -0.4 | +34.7 / +6.4 | +23.1 / -28.5 | 2.2 / 3.9 / 3.0 | 0.71 / 0.47 / 0.71 |
//! | **6 (this lane)** | **+23.0 / -3.2** | **+30.7 / +3.0** | **+23.2 / -28.5** | 3.9 / 2.5 / 2.8 | 0.33 / 0.44 / 0.70 |
//! | 8 | +61.1 / +23.5 | +75.6 / +38.4 | +50.8 / -13.8 | 1.0 / 1.5 / 1.3 | 1.50 / 1.34 / 2.10 |
//! | 10 | +112.0 / +64.4 | +155.0 / +102.7 | +106.0 / +12.7 | 0.8 / 1.4 / 0.6 | 2.46 / 2.03 / 4.17 |
//!
//! The whole ladder moved since it was composed (2026-09-07): the encoder is
//! four points BETTER than rav1e `speed 6` on film A at preset 0 where the
//! old table read +23.4, so every "the keep rule fails at every preset"
//! statement that table carried is dead. PARITY WITH rav1e `speed 6` IS
//! REACHED AT PRESET 6 on film A and on the screen capture; on film B it is
//! reached at preset 3 (-0.3), preset 6 sitting between rav1e `speed 8`
//! (+3.5) and `speed 10` (+28.7).
//!
//! # Against the reference encoders at THEIR fast presets
//!
//! Measured in the SAME arms as the shipped preset-6 row above
//! (`EC_AV1_PARETO_REFS=1`, which runs each reference's fast ladder through
//! `external_ladder` right after ours), so the wall column is comparable
//! WITHIN a row. `libaom cpu-used 8` codes the same bytes as `cpu-used 6` in
//! this ffmpeg -- it reads +0.0% against it on all three rows, one point on
//! the plot, not two.
//!
//! | row | encoder | vs libaom cpu-6 | vs rav1e s6 | 4-point ladder wall (rav1e s6 in that arm) |
//! |---|---|---|---|---|
//! | film A | ours preset 6 (pre-lane) | +26.6 | -0.4 | 67.6s (31.3s) |
//! | film A | rav1e speed 8 | +29.0 | +2.0 | 24.1s |
//! | film A | rav1e speed 10 | +45.3 | +17.1 | 8.4s |
//! | film A | libaom cpu-used 8 | +0.0 | -20.1 | 15.4s |
//! | film A | SVT-AV1 preset 8 | +24.1 | -1.9 | 2.4s |
//! | film A | SVT-AV1 preset 10 | +45.3 | +12.1 | 2.0s |
//! | film B | ours preset 6 (pre-lane) | +34.7 | +6.4 | 103.0s (26.4s) |
//! | film B | rav1e speed 8 | +37.3 | +3.5 | 20.5s |
//! | film B | rav1e speed 10 | +61.4 | +28.7 | 10.5s |
//! | film B | libaom cpu-used 8 | +0.0 | -22.2 | 32.3s |
//! | film B | SVT-AV1 preset 8 | +41.7 | +7.4 | 2.9s |
//! | film B | SVT-AV1 preset 10 | +65.9 | +23.3 | 2.0s |
//! | screen | ours preset 6 (pre-lane) | +23.1 | -28.5 | 68.0s (22.6s) |
//! | screen | rav1e speed 8 | +84.2 | +4.5 | 13.8s |
//! | screen | rav1e speed 10 | +332.5 | +103.4 | 9.8s |
//! | screen | libaom cpu-used 8 | +0.0 | -42.4 | 17.6s |
//! | screen | SVT-AV1 preset 8 | +21.5 | -31.6 | 5.0s |
//! | screen | SVT-AV1 preset 10 | +39.4 | -24.5 | 1.9s |
//!
//! So the QUALITY bar is met and the WALL bar is not: at preset 6 we are
//! ahead of rav1e `speed 8` on film A and the capture and behind it on film
//! B, but we spend 2-4x rav1e `speed 6`'s wall to do it, and SVT-AV1 preset 8
//! is within 2 points of us on film A at a THIRTIETH of the wall. libaom
//! `cpu-used 6/8` is 20 points ahead of everything on every row.
//!
//! # Per-lever re-price at preset 6 (lane-pareto2, film A row, this head)
//!
//! The ladder was composed off a lever table measured BEFORE RDOQ. This is
//! the same one-lever-at-a-time ablation redone AT preset 6, the fast
//! software preset, control +26.6 / -0.4. "gain" is BD points off the
//! vs-rav1e column; wall is only quoted where it decided a row (three serial
//! control arms measured 66.3 / 71.1 / 75.2s, so anything under ~13% wall is
//! below this box's floor).
//!
//! | lever re-enabled at 6 | vs libaom / rav1e | gain | shipped |
//! |---|---|---|---|
//! | **extra-reference NEWMV** | +24.4 / -2.4 | **2.0** | ON (+15..30% wall = 0.067 pts/1%) |
//! | **coefficient breakout 0** | +24.8 / -1.6 | **1.2** | ON (+11% wall = 0.109 pts/1%) |
//! | 8x8 split | +25.9 / -0.8 | 0.4 | off (half the film wall in the first table) |
//! | warp | +26.2 / -0.8 | 0.4 | off |
//! | CfL | +26.2 / -0.7 | 0.3 | off |
//! | chroma top-k off | +26.3 / -0.7 | 0.3 | off |
//! | inter var-tx | +26.3 / -0.7 | 0.3 | off |
//! | angle delta | +26.5 / -0.6 | 0.2 | off |
//! | split-RD 0.125 | +26.4 / -0.6 | 0.2 | off |
//! | inter-intra top-13 | +26.7 / -0.3 | 0.1 | off |
//! | intra top-13 | +26.7 / -0.4 | 0.0 | off |
//! | filter intra | +26.7 / -0.4 | 0.0 | off |
//! | 32x32 tx depth | +26.6 / -0.4 | 0.0 | off -- INERT at 6 (inter tx select is off) |
//! | compound var-tx | +26.6 / -0.4 | 0.0 | off -- inert for the same reason |
//! | loop restoration | +26.7 / -0.2 | -0.2 | off (re-enabling it LOSES) |
//!
//! and the other half of the greedy rule, every lever preset 6 KEEPS, taken
//! away one at a time:
//!
//! | lever removed from 6 | vs libaom / rav1e | cost of losing it |
//! |---|---|---|
//! | RDOQ | +41.2 / +10.2 | 10.6 |
//! | 64x64 inter root | +30.7 / +3.4 | 3.8 |
//! | key 64x64 intra root | +28.0 / +0.9 | 1.3 |
//! | per-superblock delta_q | +27.2 / -0.2 | 0.2 |
//! | tpl window 4 -> 1 | +27.0 / -0.3 | 0.1 |
//!
//! Every keep is confirmed. The two re-enables above ship (see
//! [`EXTRA_REF_NEW`] and [`SPLIT_BREAKOUT`]); together they read +23.0 / -3.2
//! on film A in the ablation arm, 2.8 points for wall that stayed inside the
//! control spread of three serial arms. Presets 4 and 5 take them by
//! bracketing, not by their own arm; presets 3, 8 and 10 are untouched, and
//! preset 0 is untouched by construction (the byte pins).
//!
//! NOT RE-MEASURED, and the reason: the THREADED fps table (1080p and
//! 3840x1608 at 4x2 tiles / 8 threads) cannot be read on this box while two
//! other lanes run -- three interleaved passes of `filter_stage_wall_*` put
//! preset 3 BELOW preset 0 in every pass at load 16-25. The single-thread fps
//! column above comes from the BD arms themselves and is ordered correctly.


use std::sync::atomic::{AtomicU8, Ordering};

/// The highest preset. 10 is the fastest, as in rav1e.
pub const MAX_SPEED: u8 = 10;

/// 255 = "not read yet"; the env read happens once and the value is a plain
/// atomic afterwards, so a caller can set it without touching the process
/// environment.
static SPEED: AtomicU8 = AtomicU8::new(u8::MAX);

/// This process's speed preset, 0..=[`MAX_SPEED`].
#[must_use]
pub fn speed() -> u8 {
    match SPEED.load(Ordering::Relaxed) {
        u8::MAX => {
            let n = crate::envflags::var("EC_AV1_SPEED")
                .ok()
                .and_then(|v| v.trim().parse::<u8>().ok())
                .unwrap_or(0)
                .min(MAX_SPEED);
            SPEED.store(n, Ordering::Relaxed);
            n
        }
        n => n,
    }
}

/// Sets the preset for everything encoded afterwards in this process.
pub fn set_speed(n: u8) {
    SPEED.store(n.min(MAX_SPEED), Ordering::Relaxed);
}

/// `TABLE[speed()]`, the shape every lever below is read through.
pub(crate) fn at<T: Copy>(table: &[T; 11]) -> T {
    table[speed() as usize]
}

// ---------------------------------------------------------------------------
// The lever tables. Index 0 of each is the shipped default of the constant
// named in the comment; the rest is this lane's composition (see the report
// table for what each step measured).
// ---------------------------------------------------------------------------

/// `encode::B64_ROOT`: the 64x64 `PARTITION_NONE` skip trial at the
/// superblock root. At preset 0 it is a bit saver AND a wall saver -- every
/// native row improves on both columns (film A +44.2/+15.7 -> +43.0/+15.0,
/// film B +70.5/+35.2 -> +64.7/+34.0, screen +48.2/-16.4 -> +33.4/-23.8) at
/// -4% to -20% wall on four of the five rows.
///
/// lane-b64 MEASURED it off from preset 4 up: at `EC_AV1_SPEED=6` ON cost
/// film A +55.1/+24.8 -> +64.5/+37.5 and film B +91.5/+53.4 -> +114.6/+71.5.
/// The cause was named there and FIXED in lane-tx64: the trial's early-out
/// rode [`SPLIT_RD`], so at 0.5 (preset 6) a superblock was taken whole with
/// its quadrants never searched. `encode::b64_breakout_threshold` clamps that
/// early-out at the shipped 0.125 whatever the preset, and the same gate at
/// `EC_AV1_SPEED=6` now reads, every row better on both columns:
///
/// | row | B64 off | B64 on, clamped |
/// |---|---|---|
/// | bars 1080p | +88.9 / +54.8 | +79.5 / +47.2 |
/// | bars 2160p | +91.6 / +49.8 | +72.6 / +35.1 |
/// | film A | +55.1 / +24.8 | +51.0 / +21.7 |
/// | film B | +91.5 / +53.4 | +77.9 / +44.3 |
/// | screen | +57.7 / -11.7 | +40.3 / -20.0 |
///
/// at 28.1s vs 30.8s (film A), 31.3s vs 28.0s (film B), 14.3s vs 15.2s
/// (screen). So it is on at every preset.
pub(crate) const B64_ROOT: [bool; 11] = [crate::encode::B64_ROOT; 11];

/// `encode::rdoq_on`: rate-distortion optimised quantisation
/// ([`crate::tile::rdoq`], lane-rdoq). MEASURED at both ends of the range on
/// the 12-frame native gate (BD vs libaom `cpu-used 6` / rav1e `speed 6`,
/// our wall):
///
/// | row | preset 0 off | preset 0 on | preset 6 off | preset 6 on |
/// |---|---|---|---|---|
/// | film A | +37.1 / +7.3 (77s) | +24.4 / -1.8 (138s) | +46.0 / +14.5 (32s) | +30.4 / +2.9 (60s) |
/// | film B | +52.5 / +22.3 (78s) | +35.5 / +7.6 (130s) | +67.8 / +35.0 (30s) | +43.5 / +14.2 (51s) |
/// | screen | +33.4 / -23.8 (45s) | +28.1 / -25.7 (79s) | +40.3 / -20.0 (15s) | +34.9 / -22.0 (34s) |
///
/// It costs about 2x wall and pays at BOTH ends, and it DOMINATES the ladder:
/// preset 6 with it on is better than preset 0 with it off on every row and
/// on both columns, at less wall. So it is on up to preset 6. The top rungs
/// (7..=10) are the real-time ones and are UNMEASURED here, so they keep the
/// cheaper choice rather than an assumed one; `EC_AV1_RDOQ=1` turns it on
/// there.
pub(crate) const RDOQ: [bool; 11] = [
    true, true, true, true, true, true, true, false, false, false, false,
];

/// `encode::I64_ROOT`: the KEY frame's 64x64 intra root. On at every preset
/// (see the lane-i64 report's speed-6 arm); its early-out reads
/// `encode::b64_breakout_threshold`, the same preset-clamped 0.125 the inter
/// root uses, so the preset's looser `SPLIT_RD` never withholds four quadrant
/// searches here either.
pub(crate) const I64_ROOT: [bool; 11] = [crate::encode::I64_ROOT; 11];

/// `encode::deltaq_res_log2`: the per-superblock quantizer's `delta_q_res`
/// LOG2 (2 = a step of 4 qindex), or `4` for "no delta_q syntax at all".
///
/// ON (res 4) AT PRESETS 5 AND 6, off everywhere else, and never on a screen
/// frame -- `encode_inter_frame` gates the whole map on
/// `!allow_screen_content_tools`, the same content gate `b64_residual` and
/// `b64_compound` take, so a desktop capture codes no delta syntax and stays
/// byte-identical to the pre-lane encoder.
///
/// lane-deltaq measured the mapping sweep with the tpl lookahead still cut to
/// one picture, so the map it read was inert and every arm was flat. With the
/// window back (lane-tplwin) lane-dq2 re-measured and lane-dq3 shipped it,
/// 12-frame `bd_rate_screen_native`, BD vs libaom / vs rav1e, off -> on:
///
/// | preset | film A | film B | screen capture |
/// |---|---|---|---|
/// | 6 | +27.2/-0.2 -> +26.6/-0.4 | +35.2/+6.6 -> +34.7/+6.4 | +26.0/-27.2, byte-identical |
/// | 5 | +26.6/-0.7 -> +25.8/-1.1 | +33.3/+4.9 -> +32.8/+4.6 | gated off |
/// | 4 | +23.7/-2.9 -> +23.1/-3.1 | +29.9/+2.0 -> +30.2/+2.6 | gated off |
///
/// Both films gain on both columns at 5 and 6 (the four q points of the
/// capture row code the identical byte counts on and off). PRESET 4 REJECTS
/// -- film B goes the wrong way on both columns there. Preset 3 rejects
/// (lane-dq2: film B worse on both columns) and preset 0 is neutral, so they
/// stay off. Coarser steps (res 8) were worse everywhere in lane-deltaq's
/// sweep. The syntax is proven three ways
/// (`a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly`);
/// `EC_AV1_DELTAQ=2` forces it on at any preset, `EC_AV1_DELTAQ=0` off.
/// Above preset 6 it is inert anyway -- [`TPL_DEPTH`] cuts the lookahead to
/// one picture there, so there is no map to vary the quantizer by.
pub(crate) const DELTAQ_RES: [u8; 11] = [2, 4, 4, 4, 4, 2, 2, 4, 4, 4, 4];

/// lane-dq: the strength of libaom's OBJECTIVE per-superblock mapping
/// ([`crate::encode::deltaq_libaom`]) -- `0` keeps the shipped
/// `deltaq_for_factor` derivation, anything above it takes that fraction of
/// libaom's own offset. Only meaningful where [`DELTAQ_RES`] is on.
///
/// Preset 0 only, at 0.25. The census (`EC_AV1_DQ_CENSUS=1`) says the two
/// mappings read the SAME tpl map into offsets an order of magnitude apart --
/// on film B's 12-frame window, ours is `-4..8` with `|mean| 3.0` while
/// libaom's is `-24..32` with `|mean| 14` -- so a strength is the only honest
/// knob between them. Measured on the 48-picture long-GOP gate (BD vs libaom
/// / rav1e), which is the shape his exports have:
///
/// | arm | film A | film B |
/// |---|---|---|
/// | control (no delta_q) | +25.2/-7.2 | +86.7/+7.3 |
/// | shipped mapping | +24.6/-7.3 | +86.1/+7.5 |
/// | **objective, 0.25** | **+24.4/-7.5** | **+86.0/+7.3** |
/// | objective, 0.5 | +23.8/-7.6 | +86.5/+7.9 |
/// | objective, 1.0 | +24.0/-7.0 | +88.4/+9.7 |
///
/// 0.25 is the only arm down on both films on the libaom column with neither
/// rav1e column worse; 0.5 buys film A another 0.6 and gives film B back 0.6.
/// The 12-frame gate reads the same arm as flat (film A +21.0/-4.8 ->
/// +20.7/-4.8, film B +24.9/-1.9 -> +24.8/-1.7) and the screen capture is
/// BYTE-IDENTICAL at every arm -- `encode_inter_frame` codes no delta syntax
/// on a screen frame at all (lane-dq3's content gate).
pub(crate) const DQ_TPL_K: [f64; 11] = [0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

/// lane-arfq: a per-LEVEL multiplier on [`DQ_TPL_K`], indexed by
/// [`crate::encode::DqLevel`] -- `[top ARF, mid ARF, leaf]`. The key frame is
/// absent because it codes no `delta_q` syntax at all.
///
/// `EC_AV1_DQ_K=<top>:<mid>:<leaf>` overrides it for an A/B. The default is
/// flat: see `lanes/arfq.report.md` for the sweep this table records.
pub(crate) const DQ_LEVEL_K: [f64; 3] = [1.0, 1.0, 1.0];

/// `encode::SPLIT_RD_THRESHOLD`: how cheap a block has to be before its split
/// trial is withheld. The single biggest wall lever in the tile search.
pub(crate) const SPLIT_RD: [f64; 11] = [
    crate::encode::SPLIT_RD_THRESHOLD,
    0.125,
    0.125,
    0.125,
    0.25,
    0.25,
    0.5,
    0.5,
    1.0,
    2.0,
    4.0,
];

/// `encode::SPLIT_BREAKOUT_COEFFS`: libaom's coefficient-count breakout.
///
/// lane-pareto2 RE-PRICED it at preset 6: switching the breakout OFF there
/// (back to the shipped 0) takes film A +26.6/-0.4 to +24.8/-1.6 -- 1.2 BD
/// points against rav1e `speed 6` -- for +11% wall (73.7s against the 66.3s
/// control) = 0.109 points per 1% wall, three times the frontier. Presets 5
/// and 6 take 0; preset 5 by BRACKETING (4 already had 0), 6 measured.
pub(crate) const SPLIT_BREAKOUT: [usize; 11] = [
    crate::encode::SPLIT_BREAKOUT_COEFFS,
    0,
    0,
    0,
    0,
    0,
    0,
    1,
    1,
    2,
    4,
];

/// `encode::SPLIT_INTER_8`: a 16x16 leaf may split into four 8x8 ones.
pub(crate) const SPLIT_8: [bool; 11] = [
    crate::encode::SPLIT_INTER_8,
    true,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
];

/// `encode::SPLIT_INTER_BLOCKS`: a 32x32 inter block may split at all
/// (`false` = 32x32-only partitioning).
pub(crate) const SPLIT_INTER: [bool; 11] = [
    crate::encode::SPLIT_INTER_BLOCKS,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    false,
];

/// `encode::leaf_second_new_mv`: a leaf searches its SECOND reference.
pub(crate) const LEAF_SECOND: [bool; 11] = [
    true, true, true, true, true, true, true, false, false, false, false,
];

/// `encode::EXTRA_REF_NEW_MV`: GOLDEN/ALTREF get a `NEWMV` search of their own.
/// lane-pareto2 RE-PRICED it at preset 6 (the fast software preset) on the
/// 12-frame film A row of `encode::tests::bd_rate_screen_native`, this head:
/// re-enabling it takes +26.6/-0.4 to +24.4/-2.4, TWO BD points against rav1e
/// `speed 6`, for +15% .. +30% wall (86.4s against a control that measured
/// 66.3/71.1/75.2s over three serial arms) = 0.067 points per 1% wall, which
/// clears the ladder's own frontier (CfL + angle 0.031, filter intra 0.039)
/// by better than 1.5x. The old ranking dropped it at preset 4 off a table
/// measured BEFORE RDOQ; with RDOQ on, a second reference's NEWMV is worth
/// twice what that table said. It is ON through preset 6 now; presets 4 and 5
/// follow by BRACKETING (0..3 and 6 all keep it), not by their own arm.
pub(crate) const EXTRA_REF_NEW: [bool; 11] = [
    crate::encode::EXTRA_REF_NEW_MV,
    true,
    true,
    true,
    true,
    true,
    true,
    false,
    false,
    false,
    false,
];

/// `encode::LEAF_COMPOUND`: a leaf is offered compound candidates.
pub(crate) const LEAF_COMPOUND: [bool; 11] = [
    crate::encode::LEAF_COMPOUND,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    false,
    false,
    false,
];

/// `encode::REFERENCE_SELECT`: compound prediction at all.
pub(crate) const COMPOUND: [bool; 11] = [
    crate::encode::REFERENCE_SELECT,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    true,
    false,
    false,
];

/// `encode::warp_on`: local warped motion.
pub(crate) const WARP: [bool; 11] = [
    true, false, false, false, false, false, false, false, false, false, false,
];

/// `encode::tx_select_inter`: var-tx / tx-depth search inside inter frames.
pub(crate) const TX_SELECT_INTER: [bool; 11] = [
    true, true, true, true, true, true, false, false, false, false, false,
];

/// `encode::tx32_depth_search`.
pub(crate) const TX32_DEPTH: [bool; 11] = [
    true, true, true, true, true, false, false, false, false, false, false,
];

/// `encode::compound_var_tx`.
pub(crate) const COMPOUND_VAR_TX: [bool; 11] = [
    true, true, true, true, true, true, false, false, false, false, false,
];

/// `encode::tx_select`: the KEY frame's transform-depth search.
pub(crate) const TX_SELECT_KEY: [bool; 11] = [
    true, true, true, true, true, true, true, true, true, false, false,
];

/// `encode::PRUNE_TOP_K`: intra luma modes fully tried on a key frame.
pub(crate) const PRUNE_K: [Option<usize>; 11] = [
    crate::encode::PRUNE_TOP_K,
    None,
    None,
    None,
    None,
    None,
    Some(4),
    Some(2),
    Some(2),
    Some(1),
    Some(1),
];

/// `encode::INTER_PRUNE_TOP_K`: the same for intra candidates inside an inter
/// frame.
pub(crate) const PRUNE_K_INTER: [Option<usize>; 11] = [
    crate::encode::INTER_PRUNE_TOP_K,
    Some(3),
    Some(3),
    Some(3),
    Some(3),
    Some(3),
    Some(2),
    Some(2),
    Some(1),
    Some(1),
    Some(1),
];

/// `encode::CHROMA_TOP_K`: chroma modes fully tried.
pub(crate) const CHROMA_K: [Option<usize>; 11] = [
    crate::encode::CHROMA_TOP_K,
    None,
    None,
    Some(3),
    Some(2),
    Some(2),
    Some(2),
    Some(2),
    Some(1),
    Some(1),
    Some(1),
];

/// `encode::angle_delta_on`: refining a directional intra winner's delta.
pub(crate) const ANGLE: [bool; 11] = [
    true, true, true, false, false, false, false, false, false, false, false,
];

/// `encode::cfl_on`: chroma-from-luma in the chroma search.
pub(crate) const CFL: [bool; 11] = [
    true, true, true, false, false, false, false, false, false, false, false,
];

/// `encode::filter_intra_on`: the five recursive filter-intra modes as intra
/// candidates, and with them the sequence header's `enable_filter_intra`.
pub(crate) const FILTER_INTRA: [bool; 11] = [
    true, false, false, false, false, false, false, false, false, false, false,
];

/// `encode::RESTORATION`: the loop-restoration (Wiener) search.
pub(crate) const RESTORATION: [bool; 11] = [
    crate::encode::RESTORATION,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
    false,
];

/// `encode::TPL_DEPTH`: lookahead pictures the lambda map reads (1 = off).
///
/// MEASURED per preset (lane-tplwin, 12-frame `bd_rate_screen_native`, all
/// five rows, BD vs libaom `cpu-used 6` / rav1e `speed 6`; the two arms of a
/// batch ran side by side so only the ours:rav1e ratio inside one arm is a
/// wall statement). The film rows code through the pyramid, whose window is
/// the mini-GOP's display successors, so a depth above 8 cannot mean anything
/// there.
///
/// | preset | depth | film A | film B | screen | wall vs its own depth-1 arm |
/// |---|---|---|---|---|---|
/// | 0 | 8 (shipped) | +21.7 / -4.4 | +26.9 / -0.6 | +27.8 / -26.0 | -- |
/// | 0 | 4 | +21.5 / -4.5 | **+27.6 / +0.1** | +27.7 / -26.1 | -8% .. +4% |
/// | 3 | 1 | +22.5 / -3.9 | +27.6 / +0.1 | +33.5 / -23.1 | 1.00 |
/// | 3 | **4** | **+22.3 / -4.1** | **+27.3 / -0.3** | **+33.4 / -23.2** | not batched |
/// | 3 | 8 | +22.5 / -3.9 | +27.7 / +0.1 | +32.9 / -23.4 | -2% .. -4% |
/// | 6 | 1 | +27.0 / -0.3 | +35.9 / +7.2 | +34.7 / -22.4 | 1.00 |
/// | 6 | **4** | +27.2 / -0.2 | **+35.2 / +6.6** | +34.8 / -22.3 | -8% .. +2% |
/// | 6 | 8 | +27.0 / -0.4 | +35.6 / +6.9 | +34.9 / -22.3 | +3% .. +8% |
///
/// Every ladder above reproduced BYTE-IDENTICALLY on a second run of the
/// preset-6 depth-1 and depth-4 arms, so these BD deltas are signal; only the
/// wall column carries the box's noise (another lane ran throughout, load
/// 13-15), which is why depth 4's cost reads as a spread straddling zero.
///
/// Read greedily, the way the lever table above is read:
///
/// * **preset 0 keeps 8.** Shortening to 4 costs film B 0.7 points on BOTH
///   columns -- the largest single move in the whole sweep -- for at most 8%
///   of that row's wall. The byte pins do not move.
/// * **presets 3..6 take 4**, up from the 1 that shipped before. At preset 3
///   it is the best arm on every real-content row (film A -0.2/-0.2, film B
///   -0.3/-0.4, screen -0.1/-0.1 against depth 1) and the lookahead pass does
///   not appear in the wall at all. At preset 6 it buys film B 0.7/0.6 for a
///   wall that still measures below this box's noise, where depth 8 buys only
///   0.3/0.3 and does cost a visible 3-8%. Film A moves +0.2 at preset 6, so
///   this is 0.5 net film points across the two rows for no measured wall --
///   which clears the frontier the preset already dropped (CfL + angle at
///   0.031 BD points per 1% wall, coefficient breakout at 0.041) by an order
///   of magnitude.
/// * **presets 1..2 keep 8**, unmeasured: their search is within a quarter of
///   preset 0's wall, so the pass is the same negligible share there, and 8
///   is the optimum at the nearest MEASURED neighbour (0). The monotone rule
///   (`presets_are_monotone_in_speed`) forbids them going below preset 3's 4
///   in any case.
/// * **presets 7..10 stay at 1**, unmeasured: not swept by this lane.
pub(crate) const TPL_DEPTH: [usize; 11] =
    [crate::encode::TPL_DEPTH, 8, 8, 4, 4, 4, 4, 1, 1, 1, 1];

/// `encode::tx_type_search`: whether an intra luma transform unit searches
/// its own `tx_type` instead of coding `DCT_DCT` (lane-txset). The five-type
/// reduced-set alphabet the writer already codes is what it picks from, so a
/// preset that turns it off is byte-identical to the encoder before the lane.
pub(crate) const TX_TYPE_SEARCH: [bool; 11] = [
    true, true, true, true, true, true, true, false, false, false, false,
];

/// `tx_type_candidates`: whether an INTER luma transform unit searches the
/// two-type `TX_SET_INTER_3` (`IDTX` vs `DCT_DCT`) the writer already codes a
/// symbol for, instead of always coding `DCT_DCT` (lane-txset2). On at the
/// presets that carry the intra search, and SCREEN CONTENT ONLY on top of it
/// (measured, 12-frame `bd_rate_screen_native`: the capture goes
/// +20.8/-30.1 -> +20.1/-30.4 while both synthetic bars rows lose twenty
/// points and film B 0.3 -- see `encode::inter_tx_type_candidates`).
/// `EC_AV1_TXSET_INTER` overrides.
pub(crate) const TX_TYPE_SEARCH_INTER: [bool; 11] = [
    true, true, true, true, true, true, true, false, false, false, false,
];

/// `encode::wide_tx_set`: whether a frame codes `reduced_tx_set = 0`, the
/// wider `tx_type` alphabets (intra 8x8/4x4 seven types, inter 16x16 twelve,
/// inter 8x8/4x4 sixteen) instead of the reduced sets every stream this
/// encoder wrote before lane-txi carries. ON at preset 0, SCREEN CONTENT ONLY
/// like both halves of the type search itself (`encode::wide_tx_set` returns
/// the screen flag), so a non-screen frame codes the same
/// `reduced_tx_set = 1` bit and the same bytes as before.
/// MEASURED at preset 0 (lane-txw, 12-frame `bd_rate_screen_native`, screen
/// row): +19.8/-30.5 -> +14.8/-32.9 -- five BD points against libaom and 2.4
/// against rav1e, on 2.8% `V_DCT` + 7.4% `H_DCT` of the row's intra units,
/// the two types only the seven-type `TX_SET_INTRA_1` can name. It costs
/// wall: 92.8s -> 123.3s for the row. Presets 1..6 carry the type search but
/// are UNMEASURED here, so they stay off. `EC_AV1_TXSET_WIDE` overrides.
pub(crate) const WIDE_TX_SET: [bool; 11] = [
    true, false, false, false, false, false, false, false, false, false, false,
];

/// `filter_search`: whether the deblock ladder's +-1/+-2 refinement stage runs.
pub(crate) const DEBLOCK_REFINE: [bool; 11] = [
    true, true, true, true, true, true, true, false, false, false, false,
];

/// `filter_search`: whether chroma gets a deblock stage of its own.
pub(crate) const DEBLOCK_CHROMA: [bool; 11] = [
    true, true, true, true, true, true, true, true, false, false, false,
];

/// `filter_search`: how many of `CDEF_PRI` / `CDEF_SEC` each CDEF stage tries
/// (0 = no CDEF search at all, the frame keeps strength 0 = deblock only).
pub(crate) const CDEF_STRENGTHS: [usize; 11] = [5, 5, 5, 5, 5, 5, 5, 3, 3, 2, 0];

/// `filter_search`: how many extra per-superblock CDEF presets the header may
/// carry beyond the frame winner (0 = `cdef_bits` 0).
pub(crate) const CDEF_PRESETS: [usize; 11] = [7, 7, 7, 7, 7, 7, 7, 7, 3, 0, 0];

/// What preset `n` switches off relative to speed 0, for a gate or a bench row
/// to print (class `gate-blind-to-feature`: a preset that silently disables
/// nothing is a preset that buys nothing).
#[must_use]
pub fn levers(n: u8) -> Vec<String> {
    let n = usize::from(n.min(MAX_SPEED));
    let mut out = Vec::new();
    let mut flag = |name: &str, t: &[bool; 11]| {
        if !t[n] {
            out.push(format!("no {name}"));
        }
    };
    flag("64x64 root", &B64_ROOT);
    flag("8x8 split", &SPLIT_8);
    flag("32x32 split (32x32-only partitions)", &SPLIT_INTER);
    flag("leaf second-reference search", &LEAF_SECOND);
    flag("extra-reference NEWMV", &EXTRA_REF_NEW);
    flag("leaf compound", &LEAF_COMPOUND);
    flag("compound", &COMPOUND);
    flag("warp", &WARP);
    flag("inter var-tx/tx-depth", &TX_SELECT_INTER);
    flag("32x32 tx depth", &TX32_DEPTH);
    flag("compound var-tx", &COMPOUND_VAR_TX);
    flag("key tx-depth", &TX_SELECT_KEY);
    flag("angle delta", &ANGLE);
    flag("CfL", &CFL);
    flag("filter intra", &FILTER_INTRA);
    flag("loop restoration", &RESTORATION);
    flag("deblock refine", &DEBLOCK_REFINE);
    flag("chroma deblock stage", &DEBLOCK_CHROMA);
    if SPLIT_RD[n] > SPLIT_RD[0] {
        out.push(format!("split-RD breakout {}", SPLIT_RD[n]));
    }
    if SPLIT_BREAKOUT[n] > 0 {
        out.push(format!("coeff breakout {}", SPLIT_BREAKOUT[n]));
    }
    if PRUNE_K[n] != PRUNE_K[0] {
        out.push(format!("intra top-{}", PRUNE_K[n].unwrap_or(13)));
    }
    if PRUNE_K_INTER[n] != PRUNE_K_INTER[0] {
        out.push(format!(
            "inter-intra top-{}",
            PRUNE_K_INTER[n].unwrap_or(13)
        ));
    }
    if CHROMA_K[n] != CHROMA_K[0] {
        out.push(format!("chroma top-{}", CHROMA_K[n].unwrap_or(7)));
    }
    if TPL_DEPTH[n] != TPL_DEPTH[0] {
        out.push(if TPL_DEPTH[n] <= 1 {
            "no tpl".to_string()
        } else {
            format!("tpl depth {}", TPL_DEPTH[n])
        });
    }
    if CDEF_STRENGTHS[n] == 0 {
        out.push("no CDEF search".to_string());
    } else if CDEF_STRENGTHS[n] != CDEF_STRENGTHS[0] {
        out.push(format!("CDEF {} strengths", CDEF_STRENGTHS[n]));
    }
    if CDEF_PRESETS[n] != CDEF_PRESETS[0] {
        out.push(format!("CDEF presets {}", CDEF_PRESETS[n]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speed 0 must be every lever's shipped default -- the byte pins are the
    /// pin on that, and this is the cheap statement of the same rule.
    #[test]
    fn speed_zero_disables_nothing() {
        assert!(levers(0).is_empty(), "{:?}", levers(0));
        assert_eq!(SPLIT_RD[0], 0.125);
        assert_eq!(SPLIT_BREAKOUT[0], 0);
        assert_eq!(TPL_DEPTH[0], 8);
        // lane-tplwin: the fast presets carry a window too, but never a longer
        // one than the full search's.
        assert_eq!(TPL_DEPTH[6], 4);
        assert_eq!(TPL_DEPTH[7], 1);
        assert_eq!(PRUNE_K[0], None);
        assert_eq!(PRUNE_K_INTER[0], Some(3));
        assert_eq!(CHROMA_K[0], None);
        assert_eq!(CDEF_STRENGTHS[0], 5);
    }

    /// Every preset switches off something the one below it did not, and the
    /// monotone levers never come back on.
    #[test]
    fn presets_are_monotone_in_speed() {
        for n in 1..=MAX_SPEED {
            let (a, b) = (levers(n - 1), levers(n));
            assert!(b.len() >= a.len(), "speed {n} loosens: {a:?} -> {b:?}");
            assert!(SPLIT_RD[n as usize] >= SPLIT_RD[n as usize - 1]);
            assert!(TPL_DEPTH[n as usize] <= TPL_DEPTH[n as usize - 1]);
            assert!(CDEF_STRENGTHS[n as usize] <= CDEF_STRENGTHS[n as usize - 1]);
        }
        assert!(!levers(MAX_SPEED).is_empty());
    }
}

// ---------------------------------------------------------------------------
// Test-only serialisation of the process-global encoder knobs (lane-gaterace)
// ---------------------------------------------------------------------------

/// [`SPEED`] and [`crate::par::TILE_THREADS`] are PROCESS-global, so a test
/// that stores one of them changes what every test encoding concurrently on
/// another test thread codes: with the default `--test-threads`,
/// `every_speed_preset_decodes_sample_exact_through_both_decoders` (presets
/// 0..=10) and the `tile_*`/`filter_stage_*` wall tables poisoned five of the
/// ignored `encoder::` gates, each of which passes standalone. The rule is
/// reader/writer, not a plain mutex: a test that SETS a knob holds
/// [`knob_write`] for its whole body, and every test that ENCODES (i.e. reads
/// the knobs, over more than one call) holds [`knob_read`], so a setter never
/// overlaps an encode. Poison is ignored deliberately -- one failing gate
/// must not turn every later one into a lock panic.
#[cfg(test)]
static KNOBS: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Held by a test that reads the process-global knobs; see [`KNOBS`].
#[cfg(test)]
#[must_use]
pub(crate) fn knob_read() -> std::sync::RwLockReadGuard<'static, ()> {
    KNOBS.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Held by a test that SETS a process-global knob; see [`KNOBS`].
#[cfg(test)]
#[must_use]
pub(crate) fn knob_write() -> std::sync::RwLockWriteGuard<'static, ()> {
    KNOBS.write().unwrap_or_else(std::sync::PoisonError::into_inner)
}
