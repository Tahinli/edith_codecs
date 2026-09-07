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

/// `encode::SPLIT_RD_THRESHOLD`: how cheap a block has to be before its split
/// trial is withheld. The single biggest wall lever in the tile search.
pub(crate) const SPLIT_RD: [f64; 11] =
    [crate::encode::SPLIT_RD_THRESHOLD, 0.125, 0.25, 0.5, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];

/// `encode::SPLIT_BREAKOUT_COEFFS`: libaom's coefficient-count breakout.
pub(crate) const SPLIT_BREAKOUT: [usize; 11] = [crate::encode::SPLIT_BREAKOUT_COEFFS, 0, 0, 0, 0, 0, 1, 1, 2, 4, 4];

/// `encode::SPLIT_INTER_8`: a 16x16 leaf may split into four 8x8 ones.
pub(crate) const SPLIT_8: [bool; 11] =
    [crate::encode::SPLIT_INTER_8, true, true, true, false, false, false, false, false, false, false];

/// `encode::SPLIT_INTER_BLOCKS`: a 32x32 inter block may split at all
/// (`false` = 32x32-only partitioning).
pub(crate) const SPLIT_INTER: [bool; 11] =
    [crate::encode::SPLIT_INTER_BLOCKS, true, true, true, true, true, true, true, false, false, false];

/// `encode::leaf_second_new_mv`: a leaf searches its SECOND reference.
pub(crate) const LEAF_SECOND: [bool; 11] =
    [true, true, true, false, false, false, false, false, false, false, false];

/// `encode::EXTRA_REF_NEW_MV`: GOLDEN/ALTREF get a `NEWMV` search of their own.
pub(crate) const EXTRA_REF_NEW: [bool; 11] =
    [crate::encode::EXTRA_REF_NEW_MV, true, true, true, false, false, false, false, false, false, false];

/// `encode::LEAF_COMPOUND`: a leaf is offered compound candidates.
pub(crate) const LEAF_COMPOUND: [bool; 11] =
    [crate::encode::LEAF_COMPOUND, true, false, false, false, false, false, false, false, false, false];

/// `encode::REFERENCE_SELECT`: compound prediction at all.
pub(crate) const COMPOUND: [bool; 11] =
    [crate::encode::REFERENCE_SELECT, true, true, true, true, true, false, false, false, false, false];

/// `encode::warp_on`: local warped motion.
pub(crate) const WARP: [bool; 11] =
    [true, true, true, false, false, false, false, false, false, false, false];

/// `encode::tx_select_inter`: var-tx / tx-depth search inside inter frames.
pub(crate) const TX_SELECT_INTER: [bool; 11] =
    [true, true, true, true, true, false, false, false, false, false, false];

/// `encode::tx32_depth_search`.
pub(crate) const TX32_DEPTH: [bool; 11] =
    [true, true, true, true, false, false, false, false, false, false, false];

/// `encode::compound_var_tx`.
pub(crate) const COMPOUND_VAR_TX: [bool; 11] =
    [true, true, true, false, false, false, false, false, false, false, false];

/// `encode::tx_select`: the KEY frame's transform-depth search.
pub(crate) const TX_SELECT_KEY: [bool; 11] =
    [true, true, true, true, true, true, true, false, false, false, false];

/// `encode::PRUNE_TOP_K`: intra luma modes fully tried on a key frame.
pub(crate) const PRUNE_K: [Option<usize>; 11] = [
    crate::encode::PRUNE_TOP_K,
    None,
    Some(4),
    Some(4),
    Some(3),
    Some(3),
    Some(2),
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
    Some(2),
    Some(2),
    Some(2),
    Some(1),
    Some(1),
    Some(1),
    Some(1),
    Some(1),
];

/// `encode::CHROMA_TOP_K`: chroma modes fully tried.
pub(crate) const CHROMA_K: [Option<usize>; 11] = [
    crate::encode::CHROMA_TOP_K,
    Some(4),
    Some(3),
    Some(3),
    Some(2),
    Some(2),
    Some(2),
    Some(1),
    Some(1),
    Some(1),
    Some(1),
];

/// `encode::angle_delta_on`: refining a directional intra winner's delta.
pub(crate) const ANGLE: [bool; 11] =
    [true, true, true, true, false, false, false, false, false, false, false];

/// `encode::cfl_on`: chroma-from-luma in the chroma search.
pub(crate) const CFL: [bool; 11] =
    [true, true, true, true, true, false, false, false, false, false, false];

/// `encode::RESTORATION`: the loop-restoration (Wiener) search.
pub(crate) const RESTORATION: [bool; 11] =
    [crate::encode::RESTORATION, true, true, true, true, false, false, false, false, false, false];

/// `encode::TPL_DEPTH`: lookahead pictures the lambda map reads (1 = off).
pub(crate) const TPL_DEPTH: [usize; 11] = [crate::encode::TPL_DEPTH, 8, 8, 4, 4, 2, 1, 1, 1, 1, 1];

/// `filter_search`: whether the deblock ladder's +-1/+-2 refinement stage runs.
pub(crate) const DEBLOCK_REFINE: [bool; 11] =
    [true, true, true, true, true, false, false, false, false, false, false];

/// `filter_search`: whether chroma gets a deblock stage of its own.
pub(crate) const DEBLOCK_CHROMA: [bool; 11] =
    [true, true, true, true, true, true, false, false, false, false, false];

/// `filter_search`: how many of `CDEF_PRI` / `CDEF_SEC` each CDEF stage tries
/// (0 = no CDEF search at all, the frame keeps strength 0 = deblock only).
pub(crate) const CDEF_STRENGTHS: [usize; 11] = [5, 5, 5, 5, 3, 3, 3, 2, 2, 0, 0];

/// `filter_search`: how many extra per-superblock CDEF presets the header may
/// carry beyond the frame winner (0 = `cdef_bits` 0).
pub(crate) const CDEF_PRESETS: [usize; 11] = [7, 7, 7, 7, 7, 3, 3, 0, 0, 0, 0];

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
        out.push(format!("inter-intra top-{}", PRUNE_K_INTER[n].unwrap_or(13)));
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
