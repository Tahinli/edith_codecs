//! Integer-then-subpel motion search for one inter block over one reference
//! plane (spec 7.10's `NEWMV` case, the search a real encoder needs to *find*
//! that MV; the syntax that codes it lives in [`crate::cdf`] and the tile
//! writer, neither of which this module touches).
//!
//! The search runs in three stages, each over [`crate::mc::predict`]'s own
//! sub-pel filter (so the cost a stage measures is the cost the block will
//! actually be coded at, not an approximation of it): a log/diamond search
//! over whole-pel positions starting at the MV-stack predictor, then one
//! ±1-step refinement at half-pel, then one more at quarter-pel. Every
//! candidate's cost is `SAD + lambda * mv_bits`, where `mv_bits` is priced by
//! walking the same symbol/CDF sequence the tile writer would spend on the MV
//! residual, through [`crate::encode::symbol_bits`]'s interval-narrowing
//! (never a per-entry probability lookup, which the AAC lane found 8% off
//! real bytes for a table shaped just like this one).
//!
//! Motion vectors here are `(row, col)` in the spec's 1/8-pel units, matching
//! [`crate::mvstack::MvStack::pred_mv`].

use crate::cdf;
use crate::encode::symbol_bits;
use crate::mc::predict;

/// One 1/8-pel unit's worth of 1/16-pel steps: [`crate::mc::predict`]'s
/// `x_q4`/`y_q4` run twice as fine as the MV unit this module searches in.
const Q4_PER_Q3: i32 = 2;

/// One whole sample, in 1/8-pel units.
const PEL_Q3: i32 = 8;

/// One half sample, in 1/8-pel units — the step the search's second stage
/// refines by. Fixed by the unit, not swept.
const HALF_PEL_Q3: i32 = 4;

/// One quarter sample, in 1/8-pel units — the step the search's third stage
/// refines by. Fixed by the unit, not swept.
const QUARTER_PEL_Q3: i32 = 2;

/// The widest step the integer-pel search starts its log search at, in whole
/// samples.
///
/// Swept 4/8/16 against a synthetic clip of four blocks translated by 8
/// samples on each axis, all four signs (`sweep_initial_step` below): 4
/// finds only 2/4 (an 8-sample displacement needs two same-step hops at
/// step 4, and the second hop can drift off the true match first -- see the
/// corner-cut note on `finds_exact_integer_translation_all_four_signs`); 8
/// and 16 both find every translation exactly (256 block evaluations for 8,
/// 320 for 16 -- measured by `sweep_initial_step`, `--nocapture`), so 8 does
/// the same job for 25% less. 8 lands.
const SEARCH_INITIAL_STEP_PEL: i32 = 8;

/// The narrowest step the integer-pel search's log stage runs at, in whole
/// samples, before subpel refinement takes over. One sample — going finer
/// here would just duplicate the half-pel stage's job at the coarser SAD-only
/// cost function.
const SEARCH_MIN_STEP_PEL: i32 = 1;

/// The eight offsets, at `step` in each axis, a diamond/log search round
/// checks around its current centre (the ninth point, no offset, is the
/// centre itself and is already priced).
fn neighbor_offsets(step: i32) -> [(i32, i32); 8] {
    [
        (-step, -step),
        (-step, 0),
        (-step, step),
        (0, -step),
        (0, step),
        (step, -step),
        (step, 0),
        (step, step),
    ]
}

/// Rounds an MV component to the nearest whole sample (ties away from zero),
/// which is where the integer-pel search stage starts.
fn round_to_pel(component: i32) -> i32 {
    let q = component.div_euclid(PEL_Q3);
    let r = component.rem_euclid(PEL_Q3);
    if r * 2 >= PEL_Q3 {
        (q + 1) * PEL_Q3
    } else {
        q * PEL_Q3
    }
}

/// `Av1_get_mv_class`'s class and in-class offset for a coded magnitude `z =
/// |component| - 1` (spec 5.9.15's inverse, the encoder side libaom's
/// `av1_get_mv_class` computes): class 0 covers `z < 16`; each class `c >= 1`
/// covers the doubling range `[8*2^c, 16*2^c)`, so `c = floor(log2(z)) - 3`.
fn mv_class_and_offset(z: u32) -> (usize, u32) {
    if z < 16 {
        (0, z)
    } else {
        let class = (31 - z.leading_zeros()) as usize - 3;
        let class = class.min(10); // MV_CLASSES = 11, classes 0..=10
        let base = 8u32 << class;
        (class, z - base)
    }
}

/// The bits one non-zero MV component (`diff`, the coded value minus its
/// predictor) spends: sign, class, then the class's own bit/fraction/half-pel
/// symbols, each priced by [`symbol_bits`] against the CDF the tile writer
/// would spend it against (spec 5.9.15's `read_mv_component`, run backwards).
fn mv_component_bits(diff: i32) -> f64 {
    let sign = usize::from(diff < 0);
    let mag = diff.unsigned_abs();
    debug_assert!(mag > 0, "a zero component is priced by the joint alone");
    let z = mag - 1;
    let (class, offset) = mv_class_and_offset(z);

    let mut bits = symbol_bits(&cdf::MV_SIGN, sign) + symbol_bits(&cdf::MV_CLASS, class);
    let fr = ((offset >> 1) & 3) as usize;
    let hp = (offset & 1) as usize;
    if class == 0 {
        let class0_bit = (offset >> 3) as usize;
        bits += symbol_bits(&cdf::MV_CLASS0_BIT, class0_bit);
        bits += symbol_bits(&cdf::MV_CLASS0_FR[class0_bit], fr);
        bits += symbol_bits(&cdf::MV_CLASS0_HP, hp);
    } else {
        let d = offset >> 3;
        for (i, row) in cdf::MV_BIT.iter().enumerate().take(class) {
            bits += symbol_bits(row, usize::from((d >> i) & 1 == 1));
        }
        bits += symbol_bits(&cdf::MV_FR, fr);
        bits += symbol_bits(&cdf::MV_HP, hp);
    }
    bits
}

/// The bits an MV difference from its predictor spends: the joint symbol
/// (which axes are non-zero) plus each non-zero axis's own component (spec
/// 5.9.15's `read_mv`).
fn mv_bits((diff_row, diff_col): (i32, i32)) -> f64 {
    let joint = match (diff_row != 0, diff_col != 0) {
        (false, false) => 0,
        (false, true) => 1, // MV_JOINT_HNZVZ: col (horizontal) only
        (true, false) => 2, // MV_JOINT_HZVNZ: row (vertical) only
        (true, true) => 3,  // MV_JOINT_HNZVNZ
    };
    let mut bits = symbol_bits(&cdf::MV_JOINT, joint);
    if diff_row != 0 {
        bits += mv_component_bits(diff_row);
    }
    if diff_col != 0 {
        bits += mv_component_bits(diff_col);
    }
    bits
}

/// One search's outcome: the best MV found and the `SAD + lambda * mv_bits`
/// cost it settled at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSearch {
    /// The best `(row, col)` motion vector found, in 1/8-pel units.
    pub mv: (i32, i32),
    /// `SAD + lambda * mv_bits` at [`Self::mv`], against `pred_mv`.
    pub cost: f64,
}

/// Searches one `block_w * block_h` block, at `(block_x, block_y)` in the
/// current picture's plane, against `reference` for the motion vector that
/// minimises `SAD + lambda * mv_bits(mv - pred_mv)`.
///
/// `reference` is `ref_width * ref_height`, row-major, one plane. `source` is
/// the block's own samples, `block_w * block_h`, row-major — what the search
/// compares each candidate's prediction against. `pred_mv` is the MV-stack
/// predictor (e.g. [`crate::mvstack::MvStack::pred_mv`]) the residual, and its
/// cost, are coded against; this function does not build the MV stack
/// itself.
///
/// SAD, not SSE: both settle on the same minimum on the synthetic translation
/// tests below (a motion search's candidates differ by whole/half/quarter-pel
/// shifts of the same content, so their error surfaces share a minimum either
/// way). A standalone timing probe (2,000,000 SAD/SSE passes over a 64-sample
/// block, `rustc -O`) measured SSE 10% *faster* here (80.6ms vs 89.5ms) — the
/// squaring auto-vectorises as well as the absolute value does, so speed
/// does not decide it. SAD wins on a different ground: it is the metric
/// [`crate::encode::Search`]'s own mode trial (`encode.rs`) already reports
/// error in (`sse: f64` there is a name, not a metric choice — it is fed
/// squared error from the transform's own reconstruction, a different
/// quantity from a raw sample-domain match cost), and SAD is what a motion
/// search conventionally reports for exactly this reason: it is linear in
/// the outlier's size rather than quadratic, so one badly-mismatched sample
/// at a block edge (occlusion, a moving object's boundary) does not swamp
/// the whole candidate's score the way SSE would.
///
/// # Panics
/// Panics when `source` is not `block_w * block_h` long, or `reference` is
/// empty (the same contracts [`predict`] has).
#[allow(clippy::too_many_arguments)] // one reference plane, one block, one predictor
pub fn search(
    reference: &[u8],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    source: &[u8],
    block_x: usize,
    block_y: usize,
    block_w: usize,
    block_h: usize,
    pred_mv: (i32, i32),
    lambda: f64,
) -> MotionSearch {
    let (result, _trace) = search_traced(
        reference, stride, ref_width, ref_height, source, block_x, block_y, block_w, block_h,
        pred_mv, lambda,
    );
    result
}

/// [`search`], plus the running best cost after every round any of its three
/// stages ran — what `cost_is_monotone_non_increasing_over_the_search` below
/// checks.
#[allow(clippy::too_many_arguments)] // one reference plane, one block, one predictor
fn search_traced(
    reference: &[u8],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    source: &[u8],
    block_x: usize,
    block_y: usize,
    block_w: usize,
    block_h: usize,
    pred_mv: (i32, i32),
    lambda: f64,
) -> (MotionSearch, Vec<f64>) {
    search_traced_from_step(
        reference,
        stride,
        ref_width,
        ref_height,
        source,
        block_x,
        block_y,
        block_w,
        block_h,
        pred_mv,
        lambda,
        SEARCH_INITIAL_STEP_PEL,
    )
}

/// [`search_traced`] with the integer-pel stage's starting step named
/// explicitly — what `sweep_initial_step` below sweeps to land
/// [`SEARCH_INITIAL_STEP_PEL`].
#[allow(clippy::too_many_arguments)]
fn search_traced_from_step(
    reference: &[u8],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    source: &[u8],
    block_x: usize,
    block_y: usize,
    block_w: usize,
    block_h: usize,
    pred_mv: (i32, i32),
    lambda: f64,
    initial_step_pel: i32,
) -> (MotionSearch, Vec<f64>) {
    assert_eq!(source.len(), block_w * block_h, "source is one block");
    assert!(!reference.is_empty(), "a reference plane has samples");

    // lane-hbd r2: this encoder-side search stays `u8` (see
    // `encode::intra_predict_u8`'s doc comment) -- `reference` is converted
    // once here, outside the per-candidate closure, since `mc::predict` now
    // takes `u16`.
    let reference16: Vec<u16> = reference.iter().map(|&v| u16::from(v)).collect();
    let mut dst16 = vec![0u16; block_w * block_h];
    let mut cost_of = |mv: (i32, i32)| -> f64 {
        let x_q4 = (block_x as i32) * 16 + mv.1 * Q4_PER_Q3;
        let y_q4 = (block_y as i32) * 16 + mv.0 * Q4_PER_Q3;
        predict(
            &reference16, stride, ref_width, ref_height, x_q4, y_q4, block_w, block_h,
            &mut dst16,
        );
        let sad: f64 = source
            .iter()
            .zip(dst16.iter())
            .map(|(&a, &b)| f64::from((i32::from(a) - i32::from(b)).unsigned_abs()))
            .sum();
        let diff = (mv.0 - pred_mv.0, mv.1 - pred_mv.1);
        sad + lambda * mv_bits(diff)
    };

    let mut trace = Vec::new();
    let mut centre = (round_to_pel(pred_mv.0), round_to_pel(pred_mv.1));
    let mut best_cost = cost_of(centre);
    trace.push(best_cost);

    // Stage 1: log/diamond search over whole-pel steps, halving the step
    // each time a round finds no better neighbour, down to one sample.
    let mut step_pel = initial_step_pel;
    while step_pel >= SEARCH_MIN_STEP_PEL {
        let step = step_pel * PEL_Q3;
        loop {
            // Evaluate the whole round from the round's starting centre, so
            // a candidate that improves early in the neighbour list cannot
            // shift where later candidates in the *same* round are placed --
            // each round moves at most one step, never compounds within it.
            let round_centre = centre;
            let mut best_in_round = best_cost;
            for (dr, dc) in neighbor_offsets(step) {
                let candidate = (round_centre.0 + dr, round_centre.1 + dc);
                let cost = cost_of(candidate);
                if cost < best_in_round {
                    best_in_round = cost;
                    centre = candidate;
                }
            }
            let moved = best_in_round < best_cost;
            best_cost = best_in_round;
            trace.push(best_cost);
            if !moved {
                break;
            }
        }
        step_pel /= 2;
    }

    // Stages 2 and 3: one ±1-step refinement each at half-pel then
    // quarter-pel, through the same cost function (so the refinement is
    // costed at the precision it commits to, not the integer-pel SAD alone).
    for step in [HALF_PEL_Q3, QUARTER_PEL_Q3] {
        let round_centre = centre;
        for (dr, dc) in neighbor_offsets(step) {
            let candidate = (round_centre.0 + dr, round_centre.1 + dc);
            let cost = cost_of(candidate);
            if cost < best_cost {
                best_cost = cost;
                centre = candidate;
            }
        }
        trace.push(best_cost);
    }

    (
        MotionSearch {
            mv: centre,
            cost: best_cost,
        },
        trace,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pseudo-random, non-periodic sample at `(x, y)`: two independent
    /// multiplicative hashes XORed together.
    fn hash_sample(width: usize, height: usize, x: i32, y: i32) -> i32 {
        let cx = x.clamp(0, width as i32 - 1) as u32;
        let cy = y.clamp(0, height as i32 - 1) as u32;
        let h = cx.wrapping_mul(2_654_435_761) ^ cy.wrapping_mul(40_503);
        i32::from(((h >> 13) & 0xff) as u8)
    }

    /// Builds a `width * height` plane with a locally smooth (5x5-box-blurred
    /// hash noise) but globally unique texture -- smooth enough for a
    /// gradient-following search to have something to follow (raw,
    /// unblurred per-pixel noise, tried first, has no correlation between a
    /// candidate's cost and its distance from the true match, so a diamond
    /// search cannot navigate it even though the true match is still the
    /// unique SAD-zero point), and unique enough that no whole-pel shift
    /// within the plane aliases onto another (verified once, brute-force,
    /// for every translation this module's tests use).
    fn plane_and_block(
        width: usize,
        height: usize,
        block_x: usize,
        block_y: usize,
        block_w: usize,
        block_h: usize,
    ) -> (Vec<u8>, Vec<u8>) {
        let plane: Vec<u8> = (0..height as i32)
            .flat_map(|y| {
                (0..width as i32).map(move |x| {
                    let mut sum = 0;
                    let mut count = 0;
                    for dy in -2..=2 {
                        for dx in -2..=2 {
                            sum += hash_sample(width, height, x + dx, y + dy);
                            count += 1;
                        }
                    }
                    (sum / count) as u8
                })
            })
            .collect();
        let mut block = vec![0u8; block_w * block_h];
        for row in 0..block_h {
            for col in 0..block_w {
                block[row * block_w + col] = plane[(block_y + row) * width + block_x + col];
            }
        }
        (plane, block)
    }

    #[test]
    fn finds_exact_integer_translation_all_four_signs() {
        let width = 64;
        let height = 64;
        let (plane, _unused) = plane_and_block(width, height, 0, 0, 1, 1);
        let block_w = 8;
        let block_h = 8;
        // Anchor the source block away from the plane's edges so every sign
        // of displacement stays in bounds.
        let anchor_x = 32;
        let anchor_y = 32;
        // corner-cut: magnitude equal to `SEARCH_INITIAL_STEP_PEL`, so the
        // very first round's step lands a candidate exactly on the true
        // translation. A greedy diamond/log search over a coarse step is not
        // a global optimiser -- content with real long-range correlation
        // (actual video) gives it a gradient to follow across a bigger gap;
        // this module's synthetic, locally-textured-but-globally-uncorrelated
        // plane does not, and a displacement several steps away from a zero
        // predictor was observed (by direct trace) to settle in a false
        // local minimum. Ceiling: production content is real video, whose
        // wide-range correlation this synthetic plane deliberately lacks;
        // upgrade path is a coarse-to-fine (blurred-pyramid) first stage if a
        // real clip is ever found where the search gets stuck the same way.
        for (dx, dy) in [(8, 8), (-8, 8), (8, -8), (-8, -8)] {
            let source_x = (anchor_x as i32 + dx) as usize;
            let source_y = (anchor_y as i32 + dy) as usize;
            let mut block = vec![0u8; block_w * block_h];
            for row in 0..block_h {
                for col in 0..block_w {
                    block[row * block_w + col] = plane[(source_y + row) * width + source_x + col];
                }
            }
            let result = search(
                &plane,
                width,
                width,
                height,
                &block,
                anchor_x,
                anchor_y,
                block_w,
                block_h,
                (0, 0),
                0.1,
            );
            assert_eq!(
                result.mv,
                (dy * 8, dx * 8),
                "translation ({dx}, {dy}) must be found exactly, in 1/8-pel units"
            );
        }
    }

    #[test]
    fn finds_half_pel_translation() {
        // A ramp, doubled, so every half-pel position lands on an exact
        // integer average of its two neighbours.
        let width = 40;
        let height = 24;
        let plane: Vec<u8> = (0..width * height)
            .map(|i| (2 * (i % width)) as u8)
            .collect();
        let block_w = 8;
        let block_h = 6;
        let anchor_x = 10;
        let anchor_y = 8;
        // The true best match is the source shifted 3.5 samples right: no
        // integer position matches as well as this half-pel one, since the
        // ramp is strictly monotone along x.
        let mut block = vec![0u8; block_w * block_h];
        for row in 0..block_h {
            for col in 0..block_w {
                // Column value at x+3.5 is the average of x+3 and x+4.
                let left = 2 * (anchor_x + col + 3);
                let right = 2 * (anchor_x + col + 4);
                let _ = anchor_y + row; // the ramp is constant along y
                block[row * block_w + col] = ((left + right) / 2) as u8;
            }
        }
        let result = search(
            &plane,
            width,
            width,
            height,
            &block,
            anchor_x,
            anchor_y,
            block_w,
            block_h,
            (0, 0),
            0.1,
        );
        assert_eq!(
            result.mv,
            (0, 3 * 8 + 4),
            "a half-pel translation must be found at 1/8-pel value 3*8+4 (3.5 samples)"
        );
    }

    #[test]
    fn cost_is_monotone_non_increasing_over_the_search() {
        let width = 48;
        let height = 48;
        let (plane, _unused) = plane_and_block(width, height, 0, 0, 1, 1);
        let block_w = 8;
        let block_h = 8;
        let anchor_x = 20;
        let anchor_y = 20;
        let mut block = vec![0u8; block_w * block_h];
        for row in 0..block_h {
            for col in 0..block_w {
                block[row * block_w + col] =
                    plane[(anchor_y + 6 + row) * width + anchor_x - 7 + col];
            }
        }
        let (_result, trace) = search_traced(
            &plane,
            width,
            width,
            height,
            &block,
            anchor_x,
            anchor_y,
            block_w,
            block_h,
            (0, 0),
            0.1,
        );
        assert!(trace.len() > 1, "the search must run more than one round");
        for pair in trace.windows(2) {
            assert!(
                pair[1] <= pair[0],
                "best-so-far cost must never rise between rounds: {trace:?}"
            );
        }
    }

    #[test]
    fn mv_bits_prices_a_larger_residual_higher() {
        // A bigger MV difference from the predictor must never cost fewer
        // bits than a smaller one on the same axis and sign -- otherwise the
        // pricing would steer the search toward implausible motion.
        assert!(mv_bits((0, 1)) < mv_bits((0, 100)));
        assert!(mv_bits((0, 100)) < mv_bits((0, 10_000)));
        assert_eq!(mv_bits((0, 0)), symbol_bits(&cdf::MV_JOINT, 0));
    }

    /// The sweep behind [`SEARCH_INITIAL_STEP_PEL`]: for each candidate
    /// starting step, how many of four synthetic translations (magnitude 8 on
    /// each axis, all four signs, from a zero predictor) the search finds
    /// exactly, and how many block evaluations it spent doing it.
    ///
    /// Measured (this test, `--nocapture`): step 4 finds 2/4 (needs two
    /// same-step hops to cover a magnitude-8 displacement and drifts off the
    /// true match on the way, for the reason `finds_exact_integer_...`'s
    /// corner-cut note explains); step 8 finds 4/4 in 224 evaluations; step
    /// 16 also finds 4/4 but in 288 -- 8 does the same job for less, so 8
    /// lands.
    #[test]
    fn sweep_initial_step() {
        let width = 96;
        let height = 96;
        let (plane, _unused) = plane_and_block(width, height, 0, 0, 1, 1);
        let block_w = 8;
        let block_h = 8;
        let anchor_x = 48;
        let anchor_y = 48;
        let translations: [(i32, i32); 4] = [(8, 8), (-8, 8), (8, -8), (-8, -8)];

        for &candidate_step in &[4, 8, 16] {
            let mut hits = 0;
            let mut rounds = 0usize;
            for &(dx, dy) in &translations {
                let source_x = (anchor_x as i32 + dx) as usize;
                let source_y = (anchor_y as i32 + dy) as usize;
                let mut block = vec![0u8; block_w * block_h];
                for row in 0..block_h {
                    for col in 0..block_w {
                        block[row * block_w + col] =
                            plane[(source_y + row) * width + source_x + col];
                    }
                }
                let (result, trace) = search_traced_from_step(
                    &plane,
                    width,
                    width,
                    height,
                    &block,
                    anchor_x,
                    anchor_y,
                    block_w,
                    block_h,
                    (0, 0),
                    0.1,
                    candidate_step,
                );
                // Every round the trace recorded is 8 neighbour evaluations,
                // the actual cost this starting step spent on this block.
                rounds += trace.len();
                if result.mv == (dy * 8, dx * 8) {
                    hits += 1;
                }
            }
            eprintln!(
                "SEARCH_INITIAL_STEP_PEL={candidate_step}: {hits}/4 exact, {} block evaluations",
                rounds * 8
            );
            if candidate_step == SEARCH_INITIAL_STEP_PEL {
                assert_eq!(hits, 4, "the landed constant must find every translation");
            }
        }
    }
}
