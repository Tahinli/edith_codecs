//! Sub-pel motion compensation, spec 7.11.3 ("Motion vector scaling process"
//! through "Block inter predictor process"), the REGULAR (`EIGHTTAP`) filter
//! only, 8-bit samples only.
//!
//! A block's inter prediction is separable: an 8-tap horizontal pass over the
//! reference, rounded down by `InterRound0`, feeds an 8-tap vertical pass
//! over that intermediate, rounded down by `InterRound1`. Both fractions run
//! in 1/16-pel steps -- luma motion vectors are stored at 1/8-pel and always
//! land on an even step; chroma vectors, once scaled for subsampling, can
//! land on any of the 16.

/// `Round2` (spec 4.7): halves round up.
fn round2(value: i32, shift: u32) -> i32 {
    if shift == 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

/// `InterRound0` (spec 7.11.3.2), 8-bit, non-compound: the shift the
/// horizontal pass's sum is brought down by before it is stored as the
/// intermediate.
const INTER_ROUND_0: u32 = 3;
/// `InterRound1` (spec 7.11.3.2), 8-bit, non-compound: the shift the vertical
/// pass's sum is brought down by to land back on an 8-bit sample. Composed
/// with `INTER_ROUND_0` the two shifts are the filter's own gain (each pass
/// sums to 128, `128 * 128 == 1 << 14 == 1 << (INTER_ROUND_0 + INTER_ROUND_1)`)
/// -- so an identity or DC input comes back exactly, by construction of the
/// table rather than by a fitted constant.
const INTER_ROUND_1: u32 = 11;

/// `Subpel_Filters[EIGHTTAP]` (spec 7.11.3.3): 16 rows, one per 1/16-pel
/// fraction, each an 8-tap kernel centred so tap index 3 lands on the integer
/// sample (fraction 0 is the identity tap `[.., 128, ..]`).
const SUBPEL_FILTERS: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 2, -6, 126, 8, -2, 0, 0],
    [0, 2, -10, 122, 18, -4, 0, 0],
    [0, 2, -12, 116, 28, -8, 2, 0],
    [0, 2, -14, 110, 38, -10, 2, 0],
    [0, 2, -14, 102, 48, -12, 2, 0],
    [0, 2, -16, 94, 58, -12, 2, 0],
    [0, 2, -14, 84, 66, -12, 2, 0],
    [0, 2, -14, 76, 76, -14, 2, 0],
    [0, 2, -12, 66, 84, -14, 2, 0],
    [0, 2, -12, 58, 94, -16, 2, 0],
    [0, 2, -12, 48, 102, -14, 2, 0],
    [0, 2, -10, 38, 110, -14, 2, 0],
    [0, 2, -8, 28, 116, -12, 2, 0],
    [0, 0, -4, 18, 122, -10, 2, 0],
    [0, 0, -2, 8, 126, -6, 2, 0],
];

/// `Subpel_Filters[EIGHTTAP]`'s narrow-block counterpart (spec 7.11.3.4's
/// filter selection is per-axis: a `predict` axis whose output block
/// dimension is 4 or less reads this table instead of [`SUBPEL_FILTERS`]),
/// mirroring `av1_sub_pel_filters_4` (`filter.h`): the same 8-slot shape but
/// zeroed at taps 0/1/6/7, so a 4-wide (or 4-tall) block's filter never
/// reaches a full 3 samples past its own edge. Every 4:2:0 chroma block
/// under an 8x8 (or smaller) luma leaf hits this on at least one axis --
/// this is not an edge case, it is the common chroma shape.
const SUBPEL_FILTERS_4: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, -4, 126, 8, -2, 0, 0],
    [0, 0, -8, 122, 18, -4, 0, 0],
    [0, 0, -10, 116, 28, -6, 0, 0],
    [0, 0, -12, 110, 38, -8, 0, 0],
    [0, 0, -12, 102, 48, -10, 0, 0],
    [0, 0, -14, 94, 58, -10, 0, 0],
    [0, 0, -12, 84, 66, -10, 0, 0],
    [0, 0, -12, 76, 76, -12, 0, 0],
    [0, 0, -10, 66, 84, -12, 0, 0],
    [0, 0, -10, 58, 94, -14, 0, 0],
    [0, 0, -10, 48, 102, -12, 0, 0],
    [0, 0, -8, 38, 110, -12, 0, 0],
    [0, 0, -6, 28, 116, -10, 0, 0],
    [0, 0, -4, 18, 122, -8, 0, 0],
    [0, 0, -2, 8, 126, -4, 0, 0],
];

/// `Subpel_Filters[EIGHTTAP_SMOOTH]` (`filter.h`'s `av1_sub_pel_filters_8smooth`).
const SUBPEL_FILTERS_SMOOTH: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 2, 28, 62, 34, 2, 0, 0],
    [0, 0, 26, 62, 36, 4, 0, 0],
    [0, 0, 22, 62, 40, 4, 0, 0],
    [0, 0, 20, 60, 42, 6, 0, 0],
    [0, 0, 18, 58, 44, 8, 0, 0],
    [0, 0, 16, 56, 46, 10, 0, 0],
    [0, -2, 16, 54, 48, 12, 0, 0],
    [0, -2, 14, 52, 52, 14, -2, 0],
    [0, 0, 12, 48, 54, 16, -2, 0],
    [0, 0, 10, 46, 56, 16, 0, 0],
    [0, 0, 8, 44, 58, 18, 0, 0],
    [0, 0, 6, 42, 60, 20, 0, 0],
    [0, 0, 4, 40, 62, 22, 0, 0],
    [0, 0, 4, 36, 62, 26, 0, 0],
    [0, 0, 2, 34, 62, 28, 2, 0],
];

/// `SUBPEL_FILTERS_SMOOTH`'s narrow-block counterpart (`av1_sub_pel_filters_4smooth`).
const SUBPEL_FILTERS_SMOOTH_4: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, 30, 62, 34, 2, 0, 0],
    [0, 0, 26, 62, 36, 4, 0, 0],
    [0, 0, 22, 62, 40, 4, 0, 0],
    [0, 0, 20, 60, 42, 6, 0, 0],
    [0, 0, 18, 58, 44, 8, 0, 0],
    [0, 0, 16, 56, 46, 10, 0, 0],
    [0, 0, 14, 54, 48, 12, 0, 0],
    [0, 0, 12, 52, 52, 12, 0, 0],
    [0, 0, 12, 48, 54, 14, 0, 0],
    [0, 0, 10, 46, 56, 16, 0, 0],
    [0, 0, 8, 44, 58, 18, 0, 0],
    [0, 0, 6, 42, 60, 20, 0, 0],
    [0, 0, 4, 40, 62, 22, 0, 0],
    [0, 0, 4, 36, 62, 26, 0, 0],
    [0, 0, 2, 34, 62, 30, 0, 0],
];

/// `Subpel_Filters[EIGHTTAP_SHARP]` (`av1_sub_pel_filters_8sharp`) -- unlike
/// REGULAR/SMOOTH, spec 7.11.3.4 never swaps this for a narrow-block table.
const SUBPEL_FILTERS_SHARP: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [-2, 2, -6, 126, 8, -2, 2, 0],
    [-2, 6, -12, 124, 16, -6, 4, -2],
    [-2, 8, -18, 120, 26, -10, 6, -2],
    [-4, 10, -22, 116, 38, -14, 6, -2],
    [-4, 10, -22, 108, 48, -18, 8, -2],
    [-4, 10, -24, 100, 60, -20, 8, -2],
    [-4, 10, -24, 90, 70, -22, 10, -2],
    [-4, 12, -24, 80, 80, -24, 12, -4],
    [-2, 10, -22, 70, 90, -24, 10, -4],
    [-2, 8, -20, 60, 100, -24, 10, -4],
    [-2, 8, -18, 48, 108, -22, 10, -4],
    [-2, 6, -14, 38, 116, -22, 10, -4],
    [-2, 6, -10, 26, 120, -18, 8, -2],
    [-2, 4, -6, 16, 124, -12, 6, -2],
    [0, 2, -2, 8, 126, -6, 2, -2],
];

/// `Subpel_Filters[BILINEAR]` (`av1_bilinear_filters`) -- also never swapped
/// for a narrow-block table (it is already only 2 non-zero taps).
const SUBPEL_FILTERS_BILINEAR: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, 0, 120, 8, 0, 0, 0],
    [0, 0, 0, 112, 16, 0, 0, 0],
    [0, 0, 0, 104, 24, 0, 0, 0],
    [0, 0, 0, 96, 32, 0, 0, 0],
    [0, 0, 0, 88, 40, 0, 0, 0],
    [0, 0, 0, 80, 48, 0, 0, 0],
    [0, 0, 0, 72, 56, 0, 0, 0],
    [0, 0, 0, 64, 64, 0, 0, 0],
    [0, 0, 0, 56, 72, 0, 0, 0],
    [0, 0, 0, 48, 80, 0, 0, 0],
    [0, 0, 0, 40, 88, 0, 0, 0],
    [0, 0, 0, 32, 96, 0, 0, 0],
    [0, 0, 0, 24, 104, 0, 0, 0],
    [0, 0, 0, 16, 112, 0, 0, 0],
    [0, 0, 0, 8, 120, 0, 0, 0],
];

/// Which of the spec's four `interpolation_filter` kernels a block's motion
/// compensation reads (spec 6.8.9 / `frame_header`'s `interpolation_filter`).
/// A frame codes exactly one of these when not `Switchable`; a `Switchable`
/// frame codes one per block per direction (spec 5.11.20's
/// `read_interp_filter`, see `decode::decode_inter_block`), so `predict`'s
/// callers resolve `Switchable` to a concrete pair of kinds themselves --
/// this type only ever names the resolved kernel, never `Switchable` itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpFilterKind {
    Regular,
    Smooth,
    Sharp,
    Bilinear,
}

impl InterpFilterKind {
    fn tables(self) -> (&'static [[i32; 8]; 16], &'static [[i32; 8]; 16]) {
        match self {
            InterpFilterKind::Regular => (&SUBPEL_FILTERS, &SUBPEL_FILTERS_4),
            InterpFilterKind::Smooth => (&SUBPEL_FILTERS_SMOOTH, &SUBPEL_FILTERS_SMOOTH_4),
            InterpFilterKind::Sharp => (&SUBPEL_FILTERS_SHARP, &SUBPEL_FILTERS_SHARP),
            InterpFilterKind::Bilinear => (&SUBPEL_FILTERS_BILINEAR, &SUBPEL_FILTERS_BILINEAR),
        }
    }

    /// The three-symbol alphabet spec 5.11.20's `interp_filter[dir]` reads
    /// under a `Switchable` frame decodes to (`EIGHTTAP`/`_SMOOTH`/`_SHARP`
    /// in that order -- `BILINEAR` is never a `switchable_interp` outcome).
    ///
    /// # Panics
    /// Panics when `symbol` is not `0..=2`.
    pub fn from_switchable_symbol(symbol: usize) -> InterpFilterKind {
        match symbol {
            0 => InterpFilterKind::Regular,
            1 => InterpFilterKind::Smooth,
            2 => InterpFilterKind::Sharp,
            _ => panic!("switchable_interp's alphabet is exactly 3 symbols"),
        }
    }

    /// The frame header's own `interpolation_filter` (spec 6.8.9), for the
    /// non-`Switchable` values a fixed-filter frame codes directly.
    ///
    /// # Panics
    /// Panics on `Switchable`, which is not a concrete kernel -- callers
    /// resolve it per block instead (spec 5.11.20).
    pub fn from_header(filter: ec_av1_syntax::InterpolationFilter) -> InterpFilterKind {
        match filter {
            ec_av1_syntax::InterpolationFilter::Eighttap => InterpFilterKind::Regular,
            ec_av1_syntax::InterpolationFilter::EighttapSmooth => InterpFilterKind::Smooth,
            ec_av1_syntax::InterpolationFilter::EighttapSharp => InterpFilterKind::Sharp,
            ec_av1_syntax::InterpolationFilter::Bilinear => InterpFilterKind::Bilinear,
            ec_av1_syntax::InterpolationFilter::Switchable => {
                panic!("Switchable is resolved per block, not as one frame-wide kernel")
            }
        }
    }
}

/// A reference sample at `(x, y)`, clamped to the frame's true (unpadded)
/// extent -- `true_width`/`true_height`, which can be narrower than
/// `stride` -- rather than read out of range (spec 7.11.3.4's `Clip3`
/// against the frame's edges): a motion vector that points past the true
/// edge repeats the true edge's sample instead of panicking or picking up
/// whatever this encoder's own uncoded padding columns/rows hold.
#[allow(clippy::too_many_arguments)]
fn sample(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x: i32,
    y: i32,
) -> i32 {
    let cx = x.clamp(0, true_width as i32 - 1) as usize;
    let cy = y.clamp(0, true_height as i32 - 1) as usize;
    i32::from(reference[cy * stride + cx])
}

/// Predicts one `block_w * block_h` block into `dst`, row-major, from
/// `reference` (row-major, stride `stride`, true content
/// `true_width` x `true_height` -- a reference frame's decoded picture
/// buffer can be wider/taller than the true frame it holds, when this
/// encoder pads a picture to a whole number of superblocks; a spec decoder
/// never codes syntax for those extra columns/rows, so motion compensation
/// must clamp to the true extent, not the buffer's own stride).
///
/// `x_q4` / `y_q4` are the block's top-left position in the reference, in
/// 1/16-pel units (an integer MV has its low 4 bits zero); this function
/// splits each into its whole-sample part and its 0..16 fraction itself, so
/// the caller need not pre-split a motion vector.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)] // one reference plane, one position, one block shape
pub fn predict(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    dst: &mut [u8],
) {
    predict_with_filter(
        reference,
        stride,
        true_width,
        true_height,
        x_q4,
        y_q4,
        block_w,
        block_h,
        InterpFilterKind::Regular,
        dst,
    );
}

/// [`predict`], selecting the interpolation filter kernel explicitly (spec
/// 6.8.9's `interpolation_filter`) instead of always `Regular` -- the same
/// kernel both directions.
#[allow(clippy::too_many_arguments)]
pub fn predict_with_filter(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    filter_kind: InterpFilterKind,
    dst: &mut [u8],
) {
    predict_with_filters(
        reference,
        stride,
        true_width,
        true_height,
        x_q4,
        y_q4,
        block_w,
        block_h,
        filter_kind,
        filter_kind,
        dst,
    );
}

/// [`predict_with_filter`], with the horizontal (`interp_filter[0]`) and
/// vertical (`interp_filter[1]`) kernels chosen independently -- spec
/// 5.11.20's per-block `SWITCHABLE` read is per-direction (`enable_dual_filter`),
/// so the two passes can genuinely differ.
#[allow(clippy::too_many_arguments)]
pub fn predict_with_filters(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");

    #[cfg(test)]
    let stage_t = std::time::Instant::now();

    let x0 = x_q4.div_euclid(16);
    let xfrac = x_q4.rem_euclid(16) as usize;
    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;

    // Whole-pel fast path: `SUBPEL_FILTERS[0]` is the identity tap (`128` at
    // its centre, everything else `0`), and both rounding shifts are exactly
    // that filter's own gain (see `INTER_ROUND_1`'s doc comment), so the two
    // 8-tap passes reproduce the reference sample byte-for-byte here --
    // `integer_mv_is_identity` below pins that. Skipping straight to a
    // clamped copy is bit-identical, not an approximation: it matters
    // because the motion search's own log/diamond stage (`motion.rs`) is
    // whole-pel-only and calls `predict` for every candidate it prices, so
    // this path is what most of a search's `predict` calls actually hit
    // (measured: `stage_timing_breakdown_inter` attributed 403 of a 720p
    // inter frame's 408ms motion-search bucket to `predict`, almost all of
    // it whole-pel candidates from stage 1).
    if xfrac == 0 && yfrac == 0 {
        for row in 0..block_h {
            let y = y0 + row as i32;
            for col in 0..block_w {
                let x = x0 + col as i32;
                dst[row * block_w + col] =
                    sample(reference, stride, true_width, true_height, x, y) as u8;
            }
        }
        #[cfg(test)]
        crate::encode::stage_add(1, stage_t.elapsed());
        return;
    }

    let (h_wide, h_narrow) = h_kind.tables();
    let (v_wide, v_narrow) = v_kind.tables();
    let h_filter = if block_w <= 4 {
        &h_narrow[xfrac]
    } else {
        &h_wide[xfrac]
    };
    let v_filter = if block_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    // The vertical pass reads 3 rows above and 4 below the block, so the
    // horizontal pass must produce that many extra intermediate rows.
    let rows = block_h + 7;
    let mut intermediate = vec![0i32; rows * block_w];
    for r in 0..rows {
        let y = y0 - 3 + r as i32;
        for c in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in h_filter.iter().enumerate() {
                let x = x0 + c as i32 + t as i32 - 3;
                sum += tap * sample(reference, stride, true_width, true_height, x, y);
            }
            intermediate[r * block_w + c] = round2(sum, INTER_ROUND_0);
        }
    }

    for row in 0..block_h {
        for col in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in v_filter.iter().enumerate() {
                sum += tap * intermediate[(row + t) * block_w + col];
            }
            dst[row * block_w + col] = round2(sum, INTER_ROUND_1).clamp(0, 255) as u8;
        }
    }

    #[cfg(test)]
    crate::encode::stage_add(1, stage_t.elapsed());
}

/// `REF_SCALE_SHIFT`'s "no scaling" value (spec 7.11.3.3, libaom `scale.h`
/// `REF_NO_SCALE`): `x_scale_fp == REF_NO_SCALE` means the stored reference's
/// luma width equals the current frame's, so [`predict_scaled`] reduces
/// exactly to [`predict_with_filters`] (verified in
/// `predict_scaled_at_no_scale_matches_predict_with_filters` below).
pub const REF_NO_SCALE: i64 = 1 << 14;

/// The horizontal scale ratio spec 7.11.3.3 derives from luma widths only
/// (libaom `av1_setup_scale_factors_for_frame`, luma widths, `REF_SCALE_SHIFT
/// == 14`): `other_width` is the stored reference picture's own luma width,
/// `this_width` the current frame's coded luma width. AV1 superres never
/// scales height (r8's derivation), so there is no vertical counterpart.
pub fn scale_factor(other_width: usize, this_width: usize) -> i64 {
    (((other_width as i64) << 14) + this_width as i64 / 2) / this_width as i64
}

fn round_pow2_64(value: i64, shift: u32) -> i64 {
    if shift == 0 {
        value
    } else {
        (value + (1i64 << (shift - 1))) >> shift
    }
}

/// `ROUND_POWER_OF_TWO_SIGNED` (libaom `common/common.h`): halves round away
/// from zero's *magnitude* -- negative inputs are rounded like their
/// negation, not truncated toward zero the way a plain arithmetic shift
/// would.
fn round_pow2_signed_64(value: i64, shift: u32) -> i64 {
    if value < 0 {
        -round_pow2_64(-value, shift)
    } else {
        round_pow2_64(value, shift)
    }
}

/// [`predict_with_filters`]'s scaled-reference counterpart (spec 7.11.3.3):
/// used only when the stored reference's luma width differs from the current
/// frame's (`use_superres` on a non-key frame). `x_scale_fp` is
/// [`scale_factor`]'s ratio ([`REF_NO_SCALE`] reproduces
/// `predict_with_filters` bit-exact, pinned by
/// `predict_scaled_at_no_scale_matches_predict_with_filters`). Per r8's
/// derivation AV1 superres never scales height, so `y_q4`'s whole-sample /
/// fraction split and the vertical pass are untouched -- only the horizontal
/// pass's per-column integer position and filter phase come from a scaled
/// walk (`x_qn`) instead of a fixed stride-1 one.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)]
pub fn predict_scaled(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");

    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;

    // SCALE_SUBPEL_BITS == 10: x_step_qn is x_scale_fp (Q14) rounded down to
    // Q10; off/pos_x_q10 fold the block's own x_q4 (Q4) into that same Q10
    // grid (spec 7.11.3.3's `dec_calc_subpel_params`).
    let x_step_qn = round_pow2_64(x_scale_fp, 4);
    let off = (x_scale_fp - REF_NO_SCALE) * 8;
    let pos_x_q10 = round_pow2_signed_64(x_q4 as i64 * x_scale_fp + off, 8) + 32;

    let (h_wide, h_narrow) = h_kind.tables();
    let (v_wide, v_narrow) = v_kind.tables();
    let v_filter = if block_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    let rows = block_h + 7;
    let mut intermediate = vec![0i32; rows * block_w];
    for c in 0..block_w {
        let x_qn = pos_x_q10 + c as i64 * x_step_qn;
        let int_pel = (x_qn >> 10) as i32;
        let filter_idx = ((x_qn & 1023) >> 6) as usize;
        let h_filter = if block_w <= 4 {
            &h_narrow[filter_idx]
        } else {
            &h_wide[filter_idx]
        };
        for r in 0..rows {
            let y = y0 - 3 + r as i32;
            let mut sum = 0;
            for (t, &tap) in h_filter.iter().enumerate() {
                let x = int_pel + t as i32 - 3;
                sum += tap * sample(reference, stride, true_width, true_height, x, y);
            }
            intermediate[r * block_w + c] = round2(sum, INTER_ROUND_0);
        }
    }

    for row in 0..block_h {
        for col in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in v_filter.iter().enumerate() {
                sum += tap * intermediate[(row + t) * block_w + col];
            }
            dst[row * block_w + col] = round2(sum, INTER_ROUND_1).clamp(0, 255) as u8;
        }
    }
}

/// `InterRound1` for a compound ref (spec 7.11.3.2's `isCompound` branch,
/// `COMPOUND_ROUND1_BITS` in libaom's `convolve.h`): 4 bits shallower than
/// [`INTER_ROUND_1`] so the vertical pass lands in the `CONV_BUF` domain
/// (still above 8-bit pixel range) instead of a finished sample --
/// `predict_compound_intermediate` below stops there and leaves the final
/// clip to whichever combine step (simple/distance-weighted average, or a
/// future masked blend) runs on the pair of intermediates.
const INTER_ROUND_1_COMPOUND: u32 = 7;

/// `InterPostRound` (spec 7.11.3.2): `2 * FILTER_BITS - (InterRound0 +
/// InterRound1)` with the *compound* `InterRound1` above -- `2*7 - (3+7) ==
/// 4`. [`combine_compound`] folds this together with `DIST_PRECISION_BITS`
/// (below) into one final shift.
const INTER_POST_ROUND: u32 = 4;

/// `DIST_PRECISION_BITS` (spec 7.11.3.15 / libaom `enums.h`): the two
/// weights [`combine_compound`] takes always sum to `1 << 4 == 16`, whether
/// they are the simple-average split (8/8) or a distance-weighted split
/// from [`crate::compound::dist_wtd_comp_weight_assign`].
const DIST_PRECISION_BITS: u32 = 4;

/// [`predict_with_filters`]'s compound counterpart: same two-pass separable
/// filter, but the vertical pass rounds by [`INTER_ROUND_1_COMPOUND`]
/// instead of [`INTER_ROUND_1`] and is never clipped to a pixel -- the
/// `CONV_BUF` intermediate domain spec 7.11.3.2 keeps a compound block's two
/// per-reference predictions in until [`combine_compound`] blends them.
/// `dst` receives one `i32` per sample, row-major, same shape as
/// [`predict_with_filters`]'s `u8` `dst`.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)]
pub fn predict_compound_intermediate(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [i32],
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");

    let x0 = x_q4.div_euclid(16);
    let xfrac = x_q4.rem_euclid(16) as usize;
    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;

    let (h_wide, h_narrow) = h_kind.tables();
    let (v_wide, v_narrow) = v_kind.tables();
    let h_filter = if block_w <= 4 {
        &h_narrow[xfrac]
    } else {
        &h_wide[xfrac]
    };
    let v_filter = if block_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    let rows = block_h + 7;
    let mut intermediate = vec![0i32; rows * block_w];
    for r in 0..rows {
        let y = y0 - 3 + r as i32;
        for c in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in h_filter.iter().enumerate() {
                let x = x0 + c as i32 + t as i32 - 3;
                sum += tap * sample(reference, stride, true_width, true_height, x, y);
            }
            intermediate[r * block_w + c] = round2(sum, INTER_ROUND_0);
        }
    }

    for row in 0..block_h {
        for col in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in v_filter.iter().enumerate() {
                sum += tap * intermediate[(row + t) * block_w + col];
            }
            dst[row * block_w + col] = round2(sum, INTER_ROUND_1_COMPOUND);
        }
    }
}

/// Blends two [`predict_compound_intermediate`] outputs into a finished
/// 8-bit block (spec 7.11.3.15's weighted-average combine, the
/// `comp_group_idx == 0` path -- `fwd_weight`/`bck_weight` are either the
/// simple-average split `(8, 8)` or [`crate::compound::dist_wtd_comp_weight_assign`]'s
/// output; both always sum to `1 << DIST_PRECISION_BITS`). Masked compound
/// (`comp_group_idx == 1`, wedge/diffwtd) is a different combine this
/// function does not cover -- decode.rs still refuses those by name.
pub fn combine_compound(
    pred0: &[i32],
    pred1: &[i32],
    fwd_weight: i32,
    bck_weight: i32,
    dst: &mut [u8],
) {
    assert_eq!(pred0.len(), pred1.len(), "both refs predict the same block");
    assert_eq!(dst.len(), pred0.len(), "the destination is the block");
    for i in 0..dst.len() {
        let sum = pred0[i] * fwd_weight + pred1[i] * bck_weight;
        dst[i] = round2(sum, INTER_POST_ROUND + DIST_PRECISION_BITS).clamp(0, 255) as u8;
    }
}

/// lane-maskcomp r2: `build_compound_diffwtd_mask` (reconinter.c) --
/// `DIFFWTD_38`/`DIFFWTD_38_INV` (`mask_type` bit: 0/1). Operates directly on
/// the two [`predict_compound_intermediate`] outputs: libaom's own
/// `av1_build_compound_diffwtd_mask_d16_c` runs on its CONV_BUF domain (which
/// carries a constant `round_offset` bias baked into every sample by its
/// convolve stage), but `abs(src0 - src1)` cancels an equal bias on both
/// sides exactly, so the unbiased `i32` intermediates this crate already
/// produces give the identical `diff` libaom computes. `round` is
/// `2*FILTER_BITS - round_0 - round_1 + (bd-8)` == [`INTER_POST_ROUND`] for
/// 8-bit content (`round_0=3`, compound `round_1=7`, `bd=8`).
pub fn diffwtd_mask(pred0: &[i32], pred1: &[i32], inv: bool, mask: &mut [u8]) {
    assert_eq!(pred0.len(), pred1.len(), "both refs predict the same block");
    assert_eq!(mask.len(), pred0.len(), "one mask byte per pixel");
    for i in 0..mask.len() {
        let diff = round2((pred0[i] - pred1[i]).abs(), INTER_POST_ROUND);
        let m = (38 + diff / 16).clamp(0, 64);
        mask[i] = if inv { (64 - m) as u8 } else { m as u8 };
    }
}

/// lane-maskcomp r2: `aom_lowbd_blend_a64_d16_mask_c` (blend_a64_mask.c),
/// algebraically simplified to this crate's unbiased `i32` intermediate
/// domain the same way [`diffwtd_mask`]'s doc comment derives -- libaom's
/// `res -= round_offset` step cancels exactly against the bias `m + (64-m)
/// == 64` contributes equally from both inputs, leaving `(m*pred0 +
/// (64-m)*pred1) >> 6` (a plain, unrounded shift, matching the C `>>` on
/// `int32_t`) then one final [`round2`] by [`INTER_POST_ROUND`]. `mask` is
/// always the LUMA-resolution mask (`mask_stride` == luma block width);
/// `subsampled` selects the 2x2-average chroma read (spec 7.11.3.14 /
/// libaom's `subw == 1 && subh == 1` branch) vs. the direct luma read.
#[allow(clippy::too_many_arguments)]
pub fn blend_masked_compound(
    pred0: &[i32],
    pred1: &[i32],
    mask: &[u8],
    mask_stride: usize,
    w: usize,
    h: usize,
    subsampled: bool,
    dst: &mut [u8],
) {
    assert_eq!(pred0.len(), w * h, "pred0 is the destination-sized block");
    assert_eq!(pred1.len(), w * h, "pred1 is the destination-sized block");
    assert_eq!(dst.len(), w * h, "the destination is the block");
    for i in 0..h {
        for j in 0..w {
            let m = if subsampled {
                let idx = (2 * i) * mask_stride + 2 * j;
                round2(
                    i32::from(mask[idx])
                        + i32::from(mask[idx + 1])
                        + i32::from(mask[idx + mask_stride])
                        + i32::from(mask[idx + mask_stride + 1]),
                    2,
                )
            } else {
                i32::from(mask[i * mask_stride + j])
            };
            let res = (m * pred0[i * w + j] + (64 - m) * pred1[i * w + j]) >> 6;
            dst[i * w + j] = round2(res, INTER_POST_ROUND).clamp(0, 255) as u8;
        }
    }
}

/// Whole-pel identity case: [`predict_compound_intermediate`]'s two-pass
/// filter reduces to `16 * source_sample` exactly (the fraction-0 row is a
/// single `128` tap both passes, and `128*128 == 1 << 14`, `14 - 7 == 7 ==
/// INTER_ROUND_1_COMPOUND`, so the extra `2^4` of gain the compound path
/// keeps over [`predict`]'s 11-bit round survives untouched) --
/// [`combine_compound`] then divides that back out exactly with an 8/8
/// simple-average weight split, so a compound block whose two references
/// (and MVs) happen to coincide reproduces the plain source sample, not an
/// off-by-one from double rounding.
#[test]
fn compound_intermediate_whole_pel_identity_round_trips_through_combine() {
    let reference = vec![100u8; 16 * 16];
    let mut pred0 = vec![0i32; 16];
    predict_compound_intermediate(
        &reference,
        16,
        16,
        16,
        0,
        0,
        4,
        4,
        InterpFilterKind::Regular,
        InterpFilterKind::Regular,
        &mut pred0,
    );
    assert!(pred0.iter().all(|&v| v == 1600), "{pred0:?}");

    let pred1 = pred0.clone();
    let mut dst = vec![0u8; 16];
    combine_compound(&pred0, &pred1, 8, 8, &mut dst);
    assert!(dst.iter().all(|&v| v == 100), "{dst:?}");
}

#[cfg(test)]
mod tests {
    use super::{
        InterpFilterKind, REF_NO_SCALE, predict, predict_scaled, predict_with_filter,
        predict_with_filters,
    };

    /// Regression pin for the lane-av1flake ±1 defect: a real aomenc chroma
    /// inter block at the frame's top-left corner (mv q4 = (row 7, col 15),
    /// bw=bh=16) predicted the wrong value because this decoder always used
    /// the REGULAR filter table -- aomdec's own `av1_convolve_2d_sr_c` trace
    /// showed this block actually used `EIGHTTAP_SMOOTH`
    /// (`0 0 2 34 62 28 2 0` at xfrac=15, not REGULAR's `0 0 -2 8 126 -6 2
    /// 0`). Window and expected output dumped straight from an
    /// instrumented aomdec's `av1_convolve_2d_sr_c` on the pinned stream.
    #[test]
    fn smooth_filter_matches_aomdec_chroma_inter_block() {
        #[rustfmt::skip]
        let window: [[u8; 24]; 24] = [
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,114,115,115,115,116,116,116,116,116,117,117,116,116,115,115,115],
            [114,114,114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,116,116,115,115,115,115],
            [113,113,113,113,113,113,113,113,114,114,114,114,115,115,115,115,116,116,116,116,115,115,115,115],
            [113,113,113,113,113,113,113,113,113,113,114,114,114,114,115,115,115,115,115,115,115,115,115,115],
            [112,112,112,112,112,112,112,112,112,113,113,113,113,114,114,114,114,114,115,115,115,115,115,115],
            [111,111,111,111,111,111,111,111,112,112,112,112,113,113,113,113,114,114,114,114,115,115,115,115],
            [110,110,110,110,110,110,110,110,111,111,111,111,112,112,112,113,113,113,113,113,114,114,114,115],
            [109,109,109,109,109,109,109,110,110,110,110,111,111,111,111,112,112,112,112,113,113,114,114,114],
            [108,108,108,108,108,109,109,109,109,109,110,110,110,110,111,111,111,111,111,112,112,113,113,113],
            [108,108,108,108,108,108,108,108,108,108,109,109,109,110,110,110,110,110,111,111,112,112,112,112],
            [107,107,107,107,107,107,107,107,107,108,108,108,109,109,109,109,109,110,110,110,110,111,111,111],
            [106,106,106,106,106,106,106,107,107,107,107,108,108,108,108,109,109,109,109,109,109,110,110,110],
            [106,106,106,106,106,106,106,106,106,107,107,107,107,108,108,108,108,109,109,109,109,109,109,109],
            [105,105,105,105,105,105,105,105,106,106,106,107,107,107,107,108,108,108,108,108,108,108,108,109],
            [104,104,104,104,104,104,104,104,104,105,105,106,106,106,107,107,107,107,107,107,107,108,108,108],
            [102,102,102,102,102,103,103,103,103,104,104,105,105,105,106,106,107,107,107,107,107,107,107,107],
            [101,101,101,101,101,101,101,102,102,103,103,104,104,104,105,105,106,106,106,106,106,107,107,107],
            [100,100,100,100,100,100,100,101,101,102,102,103,103,104,104,105,105,105,106,106,106,106,106,106],
            [99,99,99,99,99,99,100,100,100,101,101,102,103,103,104,104,105,105,105,105,105,105,105,106],
        ];
        // The dumped window spans real cols/rows -4..19; the block itself
        // starts at real (0, 0), so pad the reference plane so col/row 4 of
        // this buffer is real col/row 0, and hand `predict_with_filter` the
        // real (0, 0) coordinates -- clamping then reproduces the identical
        // extended border the dump already captured.
        let stride = 24;
        let mut reference = vec![0u8; stride * 24];
        for (r, row) in window.iter().enumerate() {
            reference[r * stride..r * stride + stride].copy_from_slice(row);
        }
        // predict()'s own edge clamp starts at true (0,0); shift so the
        // dumped window's row/col 4 lands there by biasing x_q4/y_q4's
        // whole-pel part by -4 and reading from a plane whose true origin
        // is the window's row/col 4 -- simplest is to pass true_width /
        // true_height covering the whole dumped window and offset the
        // whole-pel part of x_q4/y_q4 by +4 to land on real (0,0).
        let x_q4 = (4 + 0) * 16 + 15; // real block x0=0, xfrac=15
        let y_q4 = (4 + 0) * 16 + 7; // real block y0=0, yfrac=7
        let mut dst = vec![0u8; 16 * 16];
        predict_with_filter(
            &reference,
            stride,
            24,
            24,
            x_q4,
            y_q4,
            16,
            16,
            InterpFilterKind::Smooth,
            &mut dst,
        );
        #[rustfmt::skip]
        let expected_row0: [u8; 16] =
            [114,114,114,114,115,115,115,116,116,116,116,116,117,117,116,116];
        assert_eq!(
            &dst[0..16],
            &expected_row0,
            "row0 vs aomdec's real av1_convolve_2d_sr_c output"
        );
        #[rustfmt::skip]
        let expected_row15: [u8; 16] =
            [103,103,104,104,104,105,105,106,106,106,107,107,107,107,107,107];
        assert_eq!(
            &dst[15 * 16..15 * 16 + 16],
            &expected_row15,
            "row15 vs aomdec"
        );
    }

    #[test]
    fn integer_mv_is_identity() {
        let width = 12;
        let height = 12;
        let reference: Vec<u8> = (0..width * height).map(|i| (i * 7 % 251) as u8).collect();
        let mut dst = vec![0u8; 6 * 6];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            4 * 16,
            6,
            6,
            &mut dst,
        );
        for row in 0..6 {
            for col in 0..6 {
                assert_eq!(
                    dst[row * 6 + col],
                    reference[(4 + row) * width + 3 + col],
                    "integer MV must reproduce the reference block byte-for-byte"
                );
            }
        }
    }

    #[test]
    fn half_pel_reproduces_the_ramp_midpoint() {
        // Step 2 so every half-pel position lands on an exact integer.
        let width = 16;
        let height = 16;
        let reference: Vec<u8> = (0..width * height)
            .map(|i| (2 * (i % width)) as u8)
            .collect();
        let mut dst = vec![0u8; 4 * 4];
        // Half-pel in x only: fraction 8/16, integer part at column 5.
        predict(
            &reference,
            width,
            width,
            height,
            5 * 16 + 8,
            5 * 16,
            4,
            4,
            &mut dst,
        );
        for row in 0..4 {
            for col in 0..4 {
                let midpoint = 2 * (5 + col) + 1; // halfway between col and col+1
                assert_eq!(
                    dst[row * 4 + col] as i32,
                    midpoint as i32,
                    "half-pel MV over a linear ramp must land exactly on the midpoint"
                );
            }
        }
    }

    #[test]
    fn constant_plane_has_unit_dc_gain_at_every_subpel_position() {
        let width = 20;
        let height = 20;
        let reference = vec![142u8; width * height];
        for yfrac in 0..16 {
            for xfrac in 0..16 {
                let mut dst = vec![0u8; 5 * 5];
                predict(
                    &reference,
                    width,
                    width,
                    height,
                    5 * 16 + xfrac,
                    5 * 16 + yfrac,
                    5,
                    5,
                    &mut dst,
                );
                assert!(
                    dst.iter().all(|&v| v == 142),
                    "DC gain must be exactly 1 at fraction ({xfrac}, {yfrac}), got {dst:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_and_vertical_only_filtering_differ() {
        // A plane linear in both x and y with different, even coefficients so
        // each axis's half-pel interpolation lands on an exact integer: this
        // pins the actual value each pass produces, not just that the two
        // differ, so swapping which pass reads which axis is caught even if
        // it happened to produce two merely-different numbers.
        let width = 10;
        let height = 10;
        let reference: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (20 * x + 4 * y) as u8))
            .collect();

        let mut horiz = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16 + 8,
            3 * 16,
            4,
            4,
            &mut horiz,
        );
        let mut vert = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            3 * 16 + 8,
            4,
            4,
            &mut vert,
        );

        for row in 0..4 {
            for col in 0..4 {
                // Horizontal half-pel: x interpolates to x0+col+0.5, y stays put.
                assert_eq!(
                    horiz[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 10 + 4 * (3 + row as i32),
                    "horizontal-only MV must interpolate along x only"
                );
                // Vertical half-pel: y interpolates to y0+row+0.5, x stays put.
                assert_eq!(
                    vert[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 4 * (3 + row as i32) + 2,
                    "vertical-only MV must interpolate along y only"
                );
            }
        }
        assert_ne!(
            horiz, vert,
            "horizontal-only and vertical-only sub-pel MVs must give different outputs"
        );
    }

    #[test]
    fn mv_past_the_edge_clamps_instead_of_panicking() {
        let width = 8;
        let height = 8;
        let reference: Vec<u8> = (0..width * height).map(|i| (i * 3 % 200) as u8).collect();
        let mut dst = vec![0u8; 4 * 4];
        // Whole-sample position far above and to the left of the plane, with
        // an odd fraction so both filter passes exercise the clamp.
        predict(
            &reference,
            width,
            width,
            height,
            -50 * 16 + 5,
            -50 * 16 + 5,
            4,
            4,
            &mut dst,
        );
        let expected = reference[0]; // the whole plane clamps to the top-left corner
        assert!(
            dst.iter().all(|&v| v == expected),
            "an MV past the top-left edge must clamp to the corner sample, got {dst:?}"
        );

        let mut dst2 = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            50 * 16 + 5,
            50 * 16 + 5,
            4,
            4,
            &mut dst2,
        );
        let expected2 = reference[height * width - 1]; // clamps to the bottom-right corner
        assert!(
            dst2.iter().all(|&v| v == expected2),
            "an MV past the bottom-right edge must clamp to the corner sample, got {dst2:?}"
        );
    }

    #[test]
    fn predict_scaled_at_no_scale_matches_predict_with_filters() {
        // Algebraic pin from the r8/r9 derivation: x_scale_fp == REF_NO_SCALE
        // must reduce predict_scaled's per-column scaled walk to the exact
        // same int_pel/filter_idx sequence predict_with_filters computes from
        // a fixed stride-1 walk -- every subpel fraction, every block width.
        let width = 24;
        let height = 24;
        let reference: Vec<u8> = (0..width * height).map(|i| (i * 7 % 251) as u8).collect();
        for block_w in [4usize, 8, 16] {
            for x_frac in 0..16i32 {
                let x_q4 = 3 * 16 + x_frac;
                let y_q4 = 2 * 16 + 5;
                let mut expected = vec![0u8; block_w * 8];
                predict_with_filters(
                    &reference,
                    width,
                    width,
                    height,
                    x_q4,
                    y_q4,
                    block_w,
                    8,
                    InterpFilterKind::Regular,
                    InterpFilterKind::Regular,
                    &mut expected,
                );
                let mut got = vec![0u8; block_w * 8];
                predict_scaled(
                    &reference,
                    width,
                    width,
                    height,
                    x_q4,
                    y_q4,
                    REF_NO_SCALE,
                    block_w,
                    8,
                    InterpFilterKind::Regular,
                    InterpFilterKind::Regular,
                    &mut got,
                );
                assert_eq!(
                    got, expected,
                    "block_w={block_w} x_frac={x_frac}: REF_NO_SCALE must reproduce predict_with_filters exactly"
                );
            }
        }
    }
}
