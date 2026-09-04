//! AV1 loop restoration: spec 5.11.57 `read_lr`/`read_lr_unit` (per-superblock
//! symbol reads, once corners are computed per
//! `av1_loop_restoration_corners_in_sb`, restoration.c:1277) and spec 7.17's
//! two pixel filters (Wiener, self-guided/SGR). See `lanes/lr.report.md` for
//! the full derivation (libaom `decodeframe.c`/`restoration.c` line refs).

use crate::msac::SymbolDecoder;
use ec_av1_syntax::obu::floor_log2;
use ec_av1_syntax::{LoopRestorationParams, RestorationType};

/// `ns(n)` (spec 4.10.7) read through the arithmetic coder's equiprobable
/// bit primitive (`aom_read_literal`/`SymbolDecoder::literal`), matching
/// libaom's `read_primitive_quniform_`. Same algorithm as
/// `ec_av1_syntax::obu::read_ns`, ported to the msac reader `decode_subexp`
/// needs (LR's coefficients ride the arithmetic-coded tile, not the
/// uncompressed header `BitReader` global motion's subexp already targets).
fn ns_msac(dec: &mut SymbolDecoder, n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    let w = floor_log2(n) + 1;
    let m = (1u32 << w) - n;
    let v = dec.literal(w - 1);
    if v < m {
        return v;
    }
    (v << 1) - m + dec.literal(1)
}

/// `decode_subexp()` (spec 5.9.28) with `k` a parameter, not the fixed `3`
/// global motion uses -- libaom's `read_primitive_subexpfin_`. LR's Wiener
/// taps and SGR `xqd` each pick a different `k` (1/2/3 for the three Wiener
/// taps, 4 for SGR).
fn decode_subexp_msac(dec: &mut SymbolDecoder, num_syms: u32, k: u32) -> u32 {
    let mut i = 0u32;
    let mut mk = 0u32;
    loop {
        let b = if i > 0 { k + i - 1 } else { k };
        let a = 1u32 << b;
        if num_syms <= mk + 3 * a {
            return ns_msac(dec, num_syms - mk) + mk;
        }
        if dec.literal(1) == 0 {
            return dec.literal(b) + mk;
        }
        i += 1;
        mk += a;
    }
}

/// `inv_recenter_nonneg` (`aom_dsp/recenter.h`) == spec `inverse_recenter`.
fn inverse_recenter(r: u32, v: u32) -> u32 {
    if v > 2 * r {
        v
    } else if v & 1 != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

/// `aom_read_primitive_refsubexpfin` (`inv_recenter_finite_nonneg` composed
/// with `read_primitive_subexpfin`): a value in `[0, n)` recentred around
/// `reference`.
fn decode_unsigned_subexp_with_ref_msac(dec: &mut SymbolDecoder, n: u32, k: u32, reference: u32) -> u32 {
    let v = decode_subexp_msac(dec, n, k);
    if (reference << 1) <= n {
        inverse_recenter(reference, v)
    } else {
        n - 1 - inverse_recenter(n - 1 - reference, v)
    }
}

/// Signed wrapper, matching the way `read_wiener_filter`/`read_sgrproj_filter`
/// call `aom_read_primitive_refsubexpfin(rb, max-min+1, k, ref-min)+min`.
fn decode_signed_subexp_with_ref_msac(dec: &mut SymbolDecoder, low: i32, high: i32, k: u32, reference: i32) -> i32 {
    let n = (high - low) as u32;
    let r = (reference - low).clamp(0, n as i32) as u32;
    decode_unsigned_subexp_with_ref_msac(dec, n, k, r) as i32 + low
}

const WIENER_HALFWIN: usize = 3;
pub(crate) const WIENER_WIN: usize = 7;
const WIENER_TAP_MINV: [i32; 3] = [-5, -23, -17];
const WIENER_TAP_MAXV: [i32; 3] = [10, 8, 46];
const WIENER_TAP_K: [u32; 3] = [1, 2, 3];
const WIENER_TAP_MIDV: [i32; 3] = [3, -7, 15];

/// `WienerInfo`: a restoration unit's 7-tap separable filter, symmetric
/// (spec `vfilter[i] == vfilter[6-i]`), with the centre tap derived
/// (`WIENER_FILT_STEP - 2*sum(taps 0..3)`, spec 7.17.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WienerInfo {
    pub vfilter: [i32; 7],
    pub hfilter: [i32; 7],
}

impl Default for WienerInfo {
    fn default() -> Self {
        Self {
            vfilter: midpoint_filter(),
            hfilter: midpoint_filter(),
        }
    }
}

fn midpoint_filter() -> [i32; 7] {
    let [t0, t1, t2] = WIENER_TAP_MIDV;
    let centre = 128 - 2 * (t0 + t1 + t2);
    [t0, t1, t2, centre, t2, t1, t0]
}

/// One direction's 3 free taps (spec `read_wiener_filter`'s per-direction
/// loop): reads with `k`/min/max per tap index, recentred on `ref_dir`
/// (which the caller updates to the just-decoded value after each unit,
/// spec `ref_wiener_info` -- reset to the midpoint filter at each tile).
fn read_wiener_direction(dec: &mut SymbolDecoder, chroma: bool, reference: &[i32; 7]) -> [i32; 7] {
    let mut taps = [0i32; 3];
    for i in 0..3 {
        // `wiener_win == WIENER_WIN_CHROMA` (5-tap) skips tap 0, which is
        // always 0 for chroma (spec `read_wiener_filter`, `wiener_win`
        // branch on plane).
        if i == 0 && chroma {
            taps[0] = 0;
            continue;
        }
        taps[i] = decode_signed_subexp_with_ref_msac(
            dec,
            WIENER_TAP_MINV[i],
            WIENER_TAP_MAXV[i] + 1,
            WIENER_TAP_K[i],
            reference[i],
        );
    }
    let centre = 128 - 2 * (taps[0] + taps[1] + taps[2]);
    [taps[0], taps[1], taps[2], centre, taps[2], taps[1], taps[0]]
}

/// `read_wiener_filter` (decodeframe.c ~1595): both directions, updating
/// `reference` in place (the running per-plane, per-tile reference state).
fn read_wiener_filter(dec: &mut SymbolDecoder, chroma: bool, reference: &mut WienerInfo) -> WienerInfo {
    let vfilter = read_wiener_direction(dec, chroma, &reference.vfilter);
    let hfilter = read_wiener_direction(dec, chroma, &reference.hfilter);
    let info = WienerInfo { vfilter, hfilter };
    *reference = info;
    info
}

/// `SgrprojInfo`: one self-guided restoration unit's parameter-set index
/// and the two (at most) coded correction weights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SgrprojInfo {
    pub ep: usize,
    pub xqd: [i32; 2],
}

const SGRPROJ_PRJ_BITS: i32 = 7;
const SGRPROJ_PRJ_MIN0: i32 = -(1 << SGRPROJ_PRJ_BITS) * 3 / 4;
const SGRPROJ_PRJ_MAX0: i32 = SGRPROJ_PRJ_MIN0 + (1 << SGRPROJ_PRJ_BITS) - 1;
const SGRPROJ_PRJ_MIN1: i32 = -(1 << SGRPROJ_PRJ_BITS) / 4;
const SGRPROJ_PRJ_MAX1: i32 = SGRPROJ_PRJ_MIN1 + (1 << SGRPROJ_PRJ_BITS) - 1;
const SGRPROJ_PRJ_SUBEXP_K: u32 = 4;

/// `av1_sgr_params` (`restoration.c:36`): `(r0, r1, s0, s1)` per of the 16
/// `SGRPROJ_PARAMS_BITS`-selected parameter sets. `r == 0` for a radius
/// means that pass is skipped (its `xqd` is then forced, not coded).
pub(crate) const SGR_PARAMS: [(i32, i32, i32, i32); 16] = [
    (2, 1, 140, 3236),
    (2, 1, 112, 2158),
    (2, 1, 93, 1618),
    (2, 1, 80, 1438),
    (2, 1, 70, 1295),
    (2, 1, 58, 1177),
    (2, 1, 47, 1079),
    (2, 1, 37, 996),
    (2, 1, 30, 925),
    (2, 1, 25, 863),
    (0, 1, -1, 2589),
    (0, 1, -1, 1618),
    (0, 1, -1, 1177),
    (0, 1, -1, 925),
    (2, 0, 56, -1),
    (2, 0, 22, -1),
];

/// `read_sgrproj_filter` (decodeframe.c ~1651). `reference` is the running
/// per-plane, per-tile `xqd` state (spec `ref_sgrproj_info`), updated in
/// place after every read (both coded and forced components).
fn read_sgrproj_filter(dec: &mut SymbolDecoder, reference: &mut SgrprojInfo) -> SgrprojInfo {
    let ep = dec.literal(4) as usize;
    let (r0, r1, _s0, _s1) = SGR_PARAMS[ep];
    let mut xqd = [0i32; 2];
    if r0 == 0 {
        xqd[0] = 0;
        xqd[1] = decode_unsigned_subexp_with_ref_msac(
            dec,
            (SGRPROJ_PRJ_MAX1 - SGRPROJ_PRJ_MIN1 + 1) as u32,
            SGRPROJ_PRJ_SUBEXP_K,
            (reference.xqd[1] - SGRPROJ_PRJ_MIN1) as u32,
        ) as i32
            + SGRPROJ_PRJ_MIN1;
    } else if r1 == 0 {
        xqd[0] = decode_unsigned_subexp_with_ref_msac(
            dec,
            (SGRPROJ_PRJ_MAX0 - SGRPROJ_PRJ_MIN0 + 1) as u32,
            SGRPROJ_PRJ_SUBEXP_K,
            (reference.xqd[0] - SGRPROJ_PRJ_MIN0) as u32,
        ) as i32
            + SGRPROJ_PRJ_MIN0;
        xqd[1] = ((1 << SGRPROJ_PRJ_BITS) - xqd[0]).clamp(SGRPROJ_PRJ_MIN1, SGRPROJ_PRJ_MAX1);
    } else {
        xqd[0] = decode_unsigned_subexp_with_ref_msac(
            dec,
            (SGRPROJ_PRJ_MAX0 - SGRPROJ_PRJ_MIN0 + 1) as u32,
            SGRPROJ_PRJ_SUBEXP_K,
            (reference.xqd[0] - SGRPROJ_PRJ_MIN0) as u32,
        ) as i32
            + SGRPROJ_PRJ_MIN0;
        xqd[1] = decode_unsigned_subexp_with_ref_msac(
            dec,
            (SGRPROJ_PRJ_MAX1 - SGRPROJ_PRJ_MIN1 + 1) as u32,
            SGRPROJ_PRJ_SUBEXP_K,
            (reference.xqd[1] - SGRPROJ_PRJ_MIN1) as u32,
        ) as i32
            + SGRPROJ_PRJ_MIN1;
    }
    let info = SgrprojInfo { ep, xqd };
    *reference = info;
    info
}

impl Default for SgrprojInfo {
    fn default() -> Self {
        Self {
            ep: 0,
            xqd: [
                (SGRPROJ_PRJ_MIN0 + SGRPROJ_PRJ_MAX0) / 2,
                (SGRPROJ_PRJ_MIN1 + SGRPROJ_PRJ_MAX1) / 2,
            ],
        }
    }
}

/// One restoration unit's decoded filter (spec `RestorationUnitInfo`,
/// `RESTORE_NONE`/`WIENER`/`SGRPROJ` -- `Switchable`-frame units pick one of
/// these per unit; `Wiener`/`Sgrproj`-frame units are all this same variant
/// or `None`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UnitFilter {
    None,
    Wiener(WienerInfo),
    Sgrproj(SgrprojInfo),
}

/// Per-plane restoration-unit grid for one tile/frame (spec `RestorationInfo`
/// per plane) -- `av1_lr_count_units`'s `horz_units`/`vert_units` sizing plus
/// the flattened `runit_idx = rcol + rrow*horz_units` storage `read_lr`
/// fills in superblock order.
pub(crate) struct RestorationGrid {
    pub horz_units: [usize; 3],
    pub vert_units: [usize; 3],
    units: [Vec<UnitFilter>; 3],
}

impl RestorationGrid {
    pub(crate) fn new(lr: &LoopRestorationParams, frame_width: u32, frame_height: u32) -> Self {
        let mut horz_units = [1usize; 3];
        let mut vert_units = [1usize; 3];
        let mut units: [Vec<UnitFilter>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for plane in 0..3 {
            if lr.frame_restoration_type[plane] == RestorationType::None {
                continue;
            }
            let (pw, ph) = if plane == 0 {
                (frame_width, frame_height)
            } else {
                ((frame_width + 1) / 2, (frame_height + 1) / 2)
            };
            let unit_size = lr.loop_restoration_size[plane];
            horz_units[plane] = count_units(pw, unit_size);
            vert_units[plane] = count_units(ph, unit_size);
            units[plane] = vec![UnitFilter::None; horz_units[plane] * vert_units[plane]];
        }
        Self {
            horz_units,
            vert_units,
            units,
        }
    }

    fn set(&mut self, plane: usize, rrow: usize, rcol: usize, filter: UnitFilter) {
        let idx = rcol + rrow * self.horz_units[plane];
        self.units[plane][idx] = filter;
    }

    pub(crate) fn get(&self, plane: usize, rrow: usize, rcol: usize) -> UnitFilter {
        let idx = rcol + rrow * self.horz_units[plane];
        self.units[plane][idx]
    }
}

/// `av1_lr_count_units` (restoration.c:63): round, not ceil, to nearest.
fn count_units(plane_size: u32, unit_size: u32) -> usize {
    (((plane_size + (unit_size >> 1)) / unit_size).max(1)) as usize
}

fn ceil_div(a: u32, b: u32) -> u32 {
    a.div_ceil(b)
}

/// `read_lr` (spec 5.11.57) for one superblock at mode-info position
/// `(mi_row, mi_col)`: for every plane the frame uses LR on, computes the
/// unit-grid corners this superblock covers
/// (`av1_loop_restoration_corners_in_sb`, restoration.c:1277 -- every unit
/// is >= 64px, so only the top-of-superblock call ever has non-empty
/// corners, matching this decoder's own once-per-SB call site) and reads
/// one `read_lr_unit` per covered `(rrow, rcol)`, per plane, in plane
/// order. `sb_mi` is the superblock's mode-info width (`SB_MI`, 16 for the
/// 64px superblocks this decoder always uses).
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_lr(
    dec: &mut SymbolDecoder,
    cdfs: &mut crate::cdf_state::Cdfs,
    lr: &LoopRestorationParams,
    grid: &mut RestorationGrid,
    reference: &mut [(WienerInfo, SgrprojInfo); 3],
    mi_row: u32,
    mi_col: u32,
    sb_mi: u32,
) {
    if !lr.uses_lr {
        return;
    }
    for plane in 0..3 {
        let ftype = lr.frame_restoration_type[plane];
        if ftype == RestorationType::None {
            continue;
        }
        let mi_size = if plane == 0 { 4u32 } else { 2u32 };
        let unit_size = lr.loop_restoration_size[plane];
        let horz_units = grid.horz_units[plane] as u32;
        let vert_units = grid.vert_units[plane] as u32;
        // `av1_loop_restoration_corners_in_sb` (restoration.c:1314): under
        // superres the mi column is in DOWNSCALED units while the unit grid
        // is in upscaled ones, so the column division scales by
        // `superres_denom / SCALE_NUMERATOR` (`mi_to_num_x`/`denom_x`). Rows
        // are never scaled (spec 7.16 widens columns only).
        let (num_x, denom_x) = match crate::decode::superres() {
            Some((_, denom)) => (mi_size * denom, unit_size * 8),
            None => (mi_size, unit_size),
        };
        let rcol0 = ceil_div(mi_col * num_x, denom_x);
        let rcol1 = ceil_div((mi_col + sb_mi) * num_x, denom_x).min(horz_units);
        let rrow0 = ceil_div(mi_row * mi_size, unit_size);
        let rrow1 = ceil_div((mi_row + sb_mi) * mi_size, unit_size).min(vert_units);
        let chroma = plane != 0;
        for rrow in rrow0..rrow1 {
            for rcol in rcol0..rcol1 {
                let filter = read_lr_unit(dec, cdfs, ftype, chroma, &mut reference[plane]);
                grid.set(plane, rrow as usize, rcol as usize, filter);
            }
        }
    }
}

/// r3 gate self-pin: how many units decoded a real (non-`None`) filter, by
/// kind -- proves a real `--enable-restoration=1` aomenc stream actually
/// exercises `read_lr_unit`'s Wiener/SGR/switchable arms, not just that the
/// symbol-count/refusal-string wiring compiles. Never reset -- summed across
/// a gate's whole attempt loop, matching `MASKED_COMPOUND_HITS`/`WEDGE_HITS`
/// (decode.rs) which this mirrors.
thread_local! {
    static WIENER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SGRPROJ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SWITCHABLE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// lane-troykf2 r1: how many *stripes* a real filter ran on with the frame's
/// own top row as their above boundary (`stripe_v_start == 0`, the 56-row
/// first stripe of `RESTORATION_UNIT_OFFSET == 8`), and how many ran on a
/// short LAST stripe (`stripe_v_end == plane_h` with a partial height). Those
/// are the two stripe shapes whose boundary substitution differs from every
/// interior stripe, and no gate size before this one had either.
thread_local! {
    static LR_STRIPE0_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LR_LAST_STRIPE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`LR_STRIPE0_HITS`].
pub(crate) fn lr_stripe0_hits() -> usize {
    LR_STRIPE0_HITS.with(|c| c.get())
}
/// Current value of [`LR_LAST_STRIPE_HITS`].
pub(crate) fn lr_last_stripe_hits() -> usize {
    LR_LAST_STRIPE_HITS.with(|c| c.get())
}

/// Current value of [`WIENER_HITS`].
pub(crate) fn wiener_hits() -> usize {
    WIENER_HITS.with(|c| c.get())
}
/// Current value of [`SGRPROJ_HITS`].
pub(crate) fn sgrproj_hits() -> usize {
    SGRPROJ_HITS.with(|c| c.get())
}
/// Current value of [`SWITCHABLE_HITS`] -- bumped once per unit whose frame
/// `restoration_type` is `Switchable`, regardless of which `RestoreType`
/// (`None`/Wiener/Sgrproj) that unit's own symbol resolved to.
pub(crate) fn switchable_hits() -> usize {
    SWITCHABLE_HITS.with(|c| c.get())
}

fn read_lr_unit(
    dec: &mut SymbolDecoder,
    cdfs: &mut crate::cdf_state::Cdfs,
    ftype: RestorationType,
    chroma: bool,
    reference: &mut (WienerInfo, SgrprojInfo),
) -> UnitFilter {
    match ftype {
        RestorationType::Switchable => {
            SWITCHABLE_HITS.with(|c| c.set(c.get() + 1));
            let t = dec.symbol(&mut cdfs.restore_switchable);
            match t {
                0 => UnitFilter::None,
                1 => {
                    WIENER_HITS.with(|c| c.set(c.get() + 1));
                    UnitFilter::Wiener(read_wiener_filter(dec, chroma, &mut reference.0))
                }
                _ => {
                    SGRPROJ_HITS.with(|c| c.set(c.get() + 1));
                    UnitFilter::Sgrproj(read_sgrproj_filter(dec, &mut reference.1))
                }
            }
        }
        RestorationType::Wiener => {
            if dec.symbol(&mut cdfs.restore_wiener) != 0 {
                WIENER_HITS.with(|c| c.set(c.get() + 1));
                UnitFilter::Wiener(read_wiener_filter(dec, chroma, &mut reference.0))
            } else {
                UnitFilter::None
            }
        }
        RestorationType::Sgrproj => {
            let bit = dec.symbol(&mut cdfs.restore_sgrproj);
            if bit != 0 {
                SGRPROJ_HITS.with(|c| c.set(c.get() + 1));
                UnitFilter::Sgrproj(read_sgrproj_filter(dec, &mut reference.1))
            } else {
                UnitFilter::None
            }
        }
        RestorationType::None => UnitFilter::None,
    }
}

// ---------------------------------------------------------------------
// Spec 7.17: the two pixel filters (Wiener, self-guided) plus the
// restoration-unit/stripe walk that drives them.
// ---------------------------------------------------------------------

/// `Round2` (spec 4.7): halves round up, arithmetic shift for negative sums
/// -- same primitive as `mc.rs`/`warp.rs`'s own `round2`, here at `i64`
/// since the self-guided filter's intermediate products don't fit `i32`.
fn round2(value: i64, shift: u32) -> i64 {
    if shift == 0 {
        value
    } else {
        (value + (1i64 << (shift - 1))) >> shift
    }
}

/// One plane's sample fetch for the pixel filters (spec 7.17.2's stripe
/// boundary substitution, libaom `setup_processing_stripe_boundary`/
/// `get_stripe_boundary_info`): within the stripe `[stripe_v_start,
/// stripe_v_end)` currently being filtered, read the post-CDEF plane;
/// outside it (the 3-row border both filters' kernels reach into), read the
/// post-deblock, pre-CDEF plane instead -- UNLESS the stripe is the very
/// first/last stripe in the whole plane, where the frame's own top/bottom
/// edge replicates (`av1_extend_frame`) rather than crossing into deblocked
/// data. Columns always clamp to the plane's own true width -- restoration
/// unit COLUMN boundaries have no such substitution, only stripe (row)
/// boundaries do.
#[allow(clippy::too_many_arguments)]
fn lr_sample(
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    stripe_v_start: usize,
    stripe_v_end: usize,
    row: i64,
    col: i64,
) -> i32 {
    let col_c = col.clamp(0, plane_w as i64 - 1) as usize;
    if row >= stripe_v_start as i64 && row < stripe_v_end as i64 {
        i32::from(cdef[row as usize * stride + col_c])
    } else if row < stripe_v_start as i64 {
        if stripe_v_start == 0 {
            i32::from(cdef[col_c])
        } else {
            // libaom saves 2 deblocked context lines and duplicates the
            // outer one to fill a 3-row border (`RESTORATION_BORDER`):
            // row `stripe_v_start-1` reads deblocked row `stripe_v_start-1`,
            // rows `stripe_v_start-2`/`-3` both read deblocked row
            // `stripe_v_start-2`.
            let dist = stripe_v_start as i64 - 1 - row;
            let src_row = if dist == 0 {
                stripe_v_start - 1
            } else {
                stripe_v_start.saturating_sub(2)
            };
            i32::from(deblocked[src_row * stride + col_c])
        }
    } else if stripe_v_end == plane_h {
        i32::from(cdef[(plane_h - 1) * stride + col_c])
    } else {
        // Mirror image of the above: rows `stripe_v_end`/`stripe_v_end+1`
        // read their own deblocked row, row `stripe_v_end+2` duplicates
        // `stripe_v_end+1`.
        let dist = (row - stripe_v_end as i64).min(1) as usize;
        let src_row = (stripe_v_end + dist).min(plane_h - 1);
        i32::from(deblocked[src_row * stride + col_c])
    }
}

/// Wiener pixel filter (spec 7.17.4, libaom `wiener_filter_stripe` ->
/// `av1_wiener_convolve_add_src_c`), one stripe-height chunk of one
/// restoration unit: a separable 7-tap horizontal pass (rounded by
/// `WIENER_ROUND0_BITS==3`, clipped to spec's intermediate range) feeding a
/// 7-tap vertical pass (rounded by `2*FILTER_BITS-3==11`) -- the same
/// `INTER_ROUND_0`/`INTER_ROUND_1` shifts `mc.rs`'s motion compensation
/// uses, since AV1 reuses that same separable-filter machinery here with a
/// custom 7-tap kernel instead of the fixed subpel tables. libaom's C
/// folds an extra `+/- (1 << ...)` bias into each pass to compensate for a
/// pointer-alignment trick that borrows the interpolation filter's 8-tap
/// convolution loop for a 7-tap kernel; that bias cancels algebraically
/// against `WienerInfo`'s own derived centre tap (`128 - 2*sum(taps)`), so
/// plugging the real taps straight into `sum(tap * sample)` (no bias term)
/// reproduces the same output byte-for-byte.
#[allow(clippy::too_many_arguments)]
fn apply_wiener_stripe(
    out: &mut [u16],
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    h_start: usize,
    h_end: usize,
    v_start: usize,
    v_end: usize,
    info: &WienerInfo,
) {
    let w = h_end - h_start;
    let h = v_end - v_start;
    if w == 0 || h == 0 {
        return;
    }
    let bd = i64::from(crate::decode::bit_depth());
    let wiener_bias = 1i64 << (bd + 3);
    let wiener_limit = 1i64 << (bd + 5);
    let rows = h + 6;
    let mut inter = vec![0i32; rows * w];
    for r in 0..rows {
        let row = v_start as i64 - 3 + r as i64;
        for c in 0..w {
            let col = h_start as i64 + c as i64;
            let mut sum = 0i64;
            for (t, &tap) in info.hfilter.iter().enumerate() {
                let x = col - 3 + t as i64;
                sum += tap as i64
                    * lr_sample(cdef, deblocked, stride, plane_w, plane_h, v_start, v_end, row, x) as i64;
            }
            // libaom's `highbd_convolve_add_src_horiz_hip` clamps its
            // *biased* intermediate to `[0, WIENER_CLAMP_LIMIT(3, bd) - 1]`
            // (`convolve.h:43`, `(1 << (bd + 1 + FILTER_BITS - round0))`).
            // Its bias over this crate's unbiased sum is exactly
            // `(1 << (bd + FILTER_BITS - 1)) >> round0 == 1 << (bd + 3)`
            // (the `rounding` term of that function, the rest cancelling
            // against `WienerInfo`'s derived centre tap as the doc comment
            // above derives), so the same clamp in *this* domain is the
            // bound shifted down by that bias. At 8-bit neither end is ever
            // reached by real content, which is why the old fixed `[0, 8191]`
            // survived every 8-bit gate; at 10-bit the upper end is
            // `128 * 1023 >> 3 == 16368`, well past `8191`, so a fixed
            // 8-bit limit silently truncated most 10-bit Wiener pixels.
            inter[r * w + c] = round2(sum, 3).clamp(-wiener_bias, wiener_limit - 1 - wiener_bias) as i32;
        }
    }
    for r in 0..h {
        for c in 0..w {
            let mut sum = 0i64;
            for (t, &tap) in info.vfilter.iter().enumerate() {
                sum += tap as i64 * inter[(r + t) * w + c] as i64;
            }
            out[(v_start + r) * stride + h_start + c] = round2(sum, 11).clamp(0, crate::decode::sample_max() as i64) as u16;
        }
    }
}

/// `av1_x_by_xplus1` (`restoration.c`): `round(256*z/(z+1))` for `z` in
/// `[0,255]`, saturating the `z==0` case to `1` instead of `0` (see the
/// libaom source comment this ports -- keeps `256-A[k]` inside a `u8` and
/// avoids a later overflow, without visibly affecting a near-flat block).
// r8: this table was transcribed with two errors -- index 101 (253 written
// as 254) and indices 165..=169 (254 written as 255) -- traced by comparing
// a call-unique dump (gated on the pinned stream's `xqd == [-16,-32]`, not a
// reused coordinate) against libaom's real `av1_x_by_xplus1` at the same
// `z=101`; both tables now generated straight from
// `~/.cache/aom-oracle/src/av1/common/restoration.c` to rule out any other
// transcription slip.
const SGR_X_BY_XPLUS1: [i32; 256] = [
    1, 128, 171, 192, 205, 213, 219, 224, 228, 230, 233, 235, 236, 238, 239,
    240, 241, 242, 243, 243, 244, 244, 245, 245, 246, 246, 247, 247, 247, 247,
    248, 248, 248, 248, 249, 249, 249, 249, 249, 250, 250, 250, 250, 250, 250,
    250, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 252, 252, 252, 252,
    252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 253, 253,
    253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253,
    253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 254, 254, 254,
    254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254,
    254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254,
    254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254,
    254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254,
    254, 254, 254, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    256,
];

/// `av1_one_by_x` (`restoration.c`): `round(4096/n)` for `n` in `1..=25`
/// (`MAX_NELEM`, `(2*MAX_RADIUS+1)^2`), indexed `[n-1]`.
const SGR_ONE_BY_X: [i32; 25] = [
    4096, 2048, 1365, 1024, 819, 683, 585, 512, 455, 410, 372, 341, 315, 293, 273, 256, 241, 228, 216, 205, 195, 186,
    178, 171, 164,
];

const SGRPROJ_MTABLE_BITS: u32 = 20;
const SGRPROJ_RECIP_BITS: u32 = 12;
const SGRPROJ_SGR_BITS: u32 = 8;
const SGRPROJ_RST_BITS: u32 = 4;

/// `calculate_intermediate_result` (`restoration.c`): the blend-factor pair
/// `(A[k], B[k])` at every position in `-1..=h` x `-1..=w` relative to a
/// restoration unit (a 1-pixel border around the unit, spec 7.17.3's
/// `BoxFilter`), for one of the two SGR radii. `r`/`s` are `SGR_PARAMS[ep]`'s
/// `(r0,s0)` or `(r1,s1)`; libaom computes this only at odd rows for the
/// `r==2` ("fast") radius as a running-sum speed optimisation -- the box
/// sum itself is identical at every row regardless, so computing it
/// everywhere (this function does, via a plain windowed sum through
/// [`lr_sample`] rather than libaom's O(1)-per-step sliding sum) and only
/// *reading* the odd rows back in [`apply_sgrproj_stripe`]'s fast branch
/// reproduces the same values without porting the sliding-window trick.
#[allow(clippy::too_many_arguments)]
fn compute_ab(
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    h_start: usize,
    v_start: usize,
    v_end: usize,
    w: usize,
    h: usize,
    r: i32,
    s: i32,
) -> Vec<(i32, i64)> {
    let gw = w + 2;
    let gh = h + 2;
    let mut ab = vec![(0i32, 0i64); gw * gh];
    let n = ((2 * r + 1) * (2 * r + 1)) as i64;
    for gi in 0..gh {
        let i = gi as i64 - 1;
        for gj in 0..gw {
            let j = gj as i64 - 1;
            let mut a = 0i64;
            let mut b = 0i64;
            for dr in -r..=r {
                let row = v_start as i64 + i + dr as i64;
                for dc in -r..=r {
                    let col = h_start as i64 + j + dc as i64;
                    let px =
                        lr_sample(cdef, deblocked, stride, plane_w, plane_h, v_start, v_end, row, col) as i64;
                    a += px * px;
                    b += px;
                }
            }
            // libaom `restoration.c:660` -- the box sums are brought back
            // to the 8-bit scale before `p` is formed (`a` is a sum of
            // squares, so it loses twice the shift `b` does); `B[k]` below
            // keeps the RAW `b`, only `p` uses the scaled pair. At 8-bit
            // both shifts are 0, which is why this was invisible until now.
            let bd_shift = u32::from(crate::decode::bit_depth()).saturating_sub(8);
            let a_s = round2(a, 2 * bd_shift);
            let b_s = round2(b, bd_shift);
            let p = if a_s * n < b_s * b_s { 0 } else { a_s * n - b_s * b_s };
            let z = round2(p * s as i64, SGRPROJ_MTABLE_BITS).clamp(0, 255) as usize;
            let a_val = SGR_X_BY_XPLUS1[z];
            let b_val = round2(
                (256 - a_val) as i64 * b * SGR_ONE_BY_X[(n - 1) as usize] as i64,
                SGRPROJ_RECIP_BITS,
            );
            if crate::envflags::env_flag!("EC_LR_CALL_DUMP")
                && v_start as i64 + i == 60
                && h_start as i64 + j == 6
                && r == 1
            {
                eprintln!(
                    "EC_LR_CALL_DUMP compute_ab u_tap: r={r} s={s} n={n} box_a={a} box_b={b} \
                     p={p} z={z} a_val={a_val} b_val={b_val}"
                );
            }
            ab[gi * gw + gj] = (a_val, b_val);
        }
    }
    ab
}

/// Self-guided pixel filter (spec 7.17.3, libaom `sgrproj_filter_stripe` ->
/// `av1_apply_selfguided_restoration_c`), one stripe-height chunk of one
/// restoration unit: up to two box-filtered blends (`SGR_PARAMS[ep]`'s `r0`
/// "fast"/even-odd-row and `r1` dense radii, either may be `0` and skipped)
/// combined with the decoded `xqd` weights via `av1_decode_xq`.
#[allow(clippy::too_many_arguments)]
fn apply_sgrproj_stripe(
    out: &mut [u16],
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    h_start: usize,
    h_end: usize,
    v_start: usize,
    v_end: usize,
    info: &SgrprojInfo,
) {
    let w = h_end - h_start;
    let h = v_end - v_start;
    if w == 0 || h == 0 {
        return;
    }
    let (r0, r1, s0, s1) = SGR_PARAMS[info.ep];
    let gw = w + 2;
    let idx = |gi: i64, gj: i64| -> usize { (gi + 1) as usize * gw + (gj + 1) as usize };

    let ab0 = (r0 > 0).then(|| compute_ab(cdef, deblocked, stride, plane_w, plane_h, h_start, v_start, v_end, w, h, r0, s0));
    let ab1 = (r1 > 0).then(|| compute_ab(cdef, deblocked, stride, plane_w, plane_h, h_start, v_start, v_end, w, h, r1, s1));

    // r8: call-unique debug dump -- r6's coordinate-only gate (`v_start==60`)
    // matched several unrelated calls across the gate's attempt sweep, so its
    // captured bytes were never provably from the failing call. `info.xqd`
    // is this restoration unit's own decoded weights, unique to the one call
    // r7 traced (`[-16,-32]`); dump the real 9-byte dense-arm (r1) "u"-tap
    // window (physical rows 59..=61, cols 5..=7 -- r7's traced tap) straight
    // from `lr_sample` for this exact call, no coordinate matching involved.
    if crate::envflags::env_flag!("EC_LR_CALL_DUMP") && r1 > 0 && info.xqd == [-16, -32] {
        let bytes: Vec<i32> = (59..=61)
            .flat_map(|row| {
                (5..=7).map(move |col| {
                    lr_sample(cdef, deblocked, stride, plane_w, plane_h, v_start, v_end, row, col)
                })
            })
            .collect();
        eprintln!(
            "EC_LR_CALL_DUMP: xqd={:?} v_start={v_start} v_end={v_end} h_start={h_start} \
             u-tap 9-byte window (rows 59..=61, cols 5..=7) = {bytes:?}",
            info.xqd
        );
    }

    let mut xq = [0i32; 2];
    if r0 == 0 {
        xq[0] = 0;
        xq[1] = (1 << SGRPROJ_PRJ_BITS) - info.xqd[1];
    } else if r1 == 0 {
        xq[0] = info.xqd[0];
        xq[1] = 0;
    } else {
        xq[0] = info.xqd[0];
        xq[1] = (1 << SGRPROJ_PRJ_BITS) - xq[0] - info.xqd[1];
    }

    for i in 0..h as i64 {
        for j in 0..w as i64 {
            let dgd = lr_sample(
                cdef,
                deblocked,
                stride,
                plane_w,
                plane_h,
                v_start,
                v_end,
                v_start as i64 + i,
                h_start as i64 + j,
            ) as i64;
            let u = dgd << SGRPROJ_RST_BITS;
            let mut v = u << SGRPROJ_PRJ_BITS;
            if let Some(ab) = &ab0 {
                // "Fast" radius (`r0==2`): A/B were only ever meaningfully
                // computed at odd rows (see `compute_ab`'s doc comment) --
                // an even output row blends its two odd neighbour rows
                // (weight 6/5, `nb=5`), an odd output row reuses its own
                // row's A/B plus column neighbours (weight 6/5, `nb=4`).
                let (a, b, nb) = if i & 1 == 0 {
                    let (a_um1, b_um1) = ab[idx(i - 1, j)];
                    let (a_up1, b_up1) = ab[idx(i + 1, j)];
                    let (a_uml, b_uml) = ab[idx(i - 1, j - 1)];
                    let (a_umr, b_umr) = ab[idx(i - 1, j + 1)];
                    let (a_upl, b_upl) = ab[idx(i + 1, j - 1)];
                    let (a_upr, b_upr) = ab[idx(i + 1, j + 1)];
                    let a = (a_um1 + a_up1) as i64 * 6 + (a_uml + a_umr + a_upl + a_upr) as i64 * 5;
                    let b = (b_um1 + b_up1) * 6 + (b_uml + b_umr + b_upl + b_upr) * 5;
                    (a, b, 5u32)
                } else {
                    let (a_c, b_c) = ab[idx(i, j)];
                    let (a_l, b_l) = ab[idx(i, j - 1)];
                    let (a_r, b_r) = ab[idx(i, j + 1)];
                    let a = a_c as i64 * 6 + (a_l + a_r) as i64 * 5;
                    let b = b_c * 6 + (b_l + b_r) * 5;
                    (a, b, 4u32)
                };
                let flt0 = round2(a * dgd + b, SGRPROJ_SGR_BITS + nb - SGRPROJ_RST_BITS);
                v += xq[0] as i64 * (flt0 - u);
            }
            if let Some(ab) = &ab1 {
                // Dense radius (`r1==1`): every row was computed, so the
                // combine is the full 3x3 neighbourhood (weight 4/3).
                let (a_c, b_c) = ab[idx(i, j)];
                let (a_u, b_u) = ab[idx(i - 1, j)];
                let (a_d, b_d) = ab[idx(i + 1, j)];
                let (a_l, b_l) = ab[idx(i, j - 1)];
                let (a_r, b_r) = ab[idx(i, j + 1)];
                let (a_ul, b_ul) = ab[idx(i - 1, j - 1)];
                let (a_ur, b_ur) = ab[idx(i - 1, j + 1)];
                let (a_dl, b_dl) = ab[idx(i + 1, j - 1)];
                let (a_dr, b_dr) = ab[idx(i + 1, j + 1)];
                let a = (a_c + a_u + a_d + a_l + a_r) as i64 * 4 + (a_ul + a_ur + a_dl + a_dr) as i64 * 3;
                let b = (b_c + b_u + b_d + b_l + b_r) * 4 + (b_ul + b_ur + b_dl + b_dr) * 3;
                let flt1 = round2(a * dgd + b, SGRPROJ_SGR_BITS + 5 - SGRPROJ_RST_BITS);
                v += xq[1] as i64 * (flt1 - u);
                if crate::envflags::env_flag!("EC_LR_CALL_DUMP")
                    && info.xqd == [-16, -32]
                    && v_start as i64 + i == 61
                    && h_start as i64 + j == 6
                {
                    eprintln!(
                        "EC_LR_CALL_DUMP combine: c={:?} u_tap(row60)={:?} d={:?} l={:?} r={:?} \
                         ul={:?} ur={:?} dl={:?} dr={:?} dense a={a} b={b} dgd={dgd} \
                         u={u} flt1={flt1} xq={xq:?} v_before_final_round={v}",
                        ab[idx(i, j)], ab[idx(i - 1, j)], ab[idx(i + 1, j)], ab[idx(i, j - 1)],
                        ab[idx(i, j + 1)], ab[idx(i - 1, j - 1)], ab[idx(i - 1, j + 1)],
                        ab[idx(i + 1, j - 1)], ab[idx(i + 1, j + 1)]
                    );
                }
            }
            let out_v = round2(v, SGRPROJ_PRJ_BITS as u32 + SGRPROJ_RST_BITS).clamp(0, crate::decode::sample_max() as i64) as u16;
            out[(v_start as i64 + i) as usize * stride + (h_start as i64 + j) as usize] = out_v;
        }
    }
}

/// Chunks one restoration unit's `[v_start, v_end)` extent into 64-row
/// (56 for the plane's very first stripe -- `RESTORATION_UNIT_OFFSET==8`)
/// processing stripes and filters each (libaom
/// `av1_loop_restoration_filter_unit`'s own `while (i < unit_h)` loop) --
/// a restoration unit can be up to `256*1.5` px tall, spanning several
/// stripes, each of which needs its own boundary-source decision from
/// [`lr_sample`].
#[allow(clippy::too_many_arguments)]
fn filter_restoration_unit(
    out: &mut [u16],
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    h_start: usize,
    h_end: usize,
    v_start: usize,
    v_end: usize,
    ss_y: u32,
    filter: UnitFilter,
) {
    let full_stripe_height = 64usize >> ss_y;
    let runit_offset = 8usize >> ss_y;
    let unit_h = v_end - v_start;
    let mut i = 0usize;
    while i < unit_h {
        let stripe_v_start = v_start + i;
        let frame_stripe = (stripe_v_start + runit_offset) / full_stripe_height;
        let nominal_h = full_stripe_height - if frame_stripe == 0 { runit_offset } else { 0 };
        let h = nominal_h.min(unit_h - i);
        let stripe_v_end = stripe_v_start + h;
        if !matches!(filter, UnitFilter::None) {
            if stripe_v_start == 0 {
                LR_STRIPE0_HITS.with(|c| c.set(c.get() + 1));
            }
            if stripe_v_end == plane_h && h < nominal_h {
                LR_LAST_STRIPE_HITS.with(|c| c.set(c.get() + 1));
            }
        }
        match filter {
            UnitFilter::Wiener(info) => apply_wiener_stripe(
                out, cdef, deblocked, stride, plane_w, plane_h, h_start, h_end, stripe_v_start, stripe_v_end, &info,
            ),
            UnitFilter::Sgrproj(info) => apply_sgrproj_stripe(
                out, cdef, deblocked, stride, plane_w, plane_h, h_start, h_end, stripe_v_start, stripe_v_end, &info,
            ),
            UnitFilter::None => {}
        }
        i += h;
    }
}

/// Applies loop restoration to one whole plane's post-CDEF samples (spec
/// 7.17). Walks restoration units exactly as libaom's
/// `foreach_rest_unit_in_plane` (`unit_size` steps, the tail folded into
/// the previous unit when the remainder is under `1.5*unit_size` -- the
/// same round-to-nearest [`count_units`] already used to size
/// [`RestorationGrid`]), then re-offsets internal row boundaries by
/// `RESTORATION_UNIT_OFFSET==8` (the first/last row of the plane keeps its
/// full extent). Returns a fresh copy of `cdef` with only this plane's
/// restoration-unit pixels replaced -- a restoration unit must never read
/// another, already-filtered unit's output (spec keeps a separate
/// destination buffer for exactly this reason), so this never filters in
/// place -- `out` is that buffer, refilled from `cdef` on entry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_loop_restoration_plane(
    cdef: &[u16],
    deblocked: &[u16],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    ss_y: u32,
    ftype: RestorationType,
    unit_size: u32,
    grid: &RestorationGrid,
    plane: usize,
    // lane-perf6: the destination buffer, refilled from `cdef` here. It used
    // to be a fresh `cdef.to_vec()` per plane per frame -- a multi-megabyte
    // allocation whose every page then faulted in on first touch.
    out: &mut Vec<u16>,
) {
    out.clear();
    out.extend_from_slice(cdef);
    if ftype == RestorationType::None || unit_size == 0 {
        return;
    }
    let voffset = 8u32 >> ss_y;
    let ext_size = (unit_size * 3) / 2;
    let mut y0 = 0u32;
    let mut rrow = 0usize;
    while y0 < plane_h as u32 {
        let remaining_h = plane_h as u32 - y0;
        let uh = if remaining_h < ext_size { remaining_h } else { unit_size };
        let v_start = (y0 as i64 - voffset as i64).max(0) as u32;
        let mut v_end = y0 + uh;
        if v_end < plane_h as u32 {
            v_end -= voffset;
        }

        let mut x0 = 0u32;
        let mut rcol = 0usize;
        while x0 < plane_w as u32 {
            let remaining_w = plane_w as u32 - x0;
            let uw = if remaining_w < ext_size { remaining_w } else { unit_size };
            let filter = grid.get(plane, rrow, rcol);
            if filter != UnitFilter::None {
                filter_restoration_unit(
                    out,
                    cdef,
                    deblocked,
                    stride,
                    plane_w,
                    plane_h,
                    x0 as usize,
                    (x0 + uw) as usize,
                    v_start as usize,
                    v_end as usize,
                    ss_y,
                    filter,
                );
            }
            x0 += uw;
            rcol += 1;
        }
        y0 += uh;
        rrow += 1;
    }
    debug_assert_eq!(rrow, grid.vert_units[plane], "RU row walk must match RestorationGrid::new's count_units");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msac::{SymbolDecoder, SymbolEncoder};

    /// `write_primitive_quniform`, the exact inverse of [`ns_msac`] -- used
    /// only to build roundtrip fixtures for the decode-only helpers above
    /// (this decoder never writes loop restoration itself).
    fn write_ns(enc: &mut SymbolEncoder, n: u32, v: u32) {
        if n <= 1 {
            return;
        }
        let w = floor_log2(n) + 1;
        let m = (1u32 << w) - n;
        if v < m {
            enc.literal(v, w - 1);
        } else {
            enc.literal((v + m) >> 1, w - 1);
            enc.literal((v + m) & 1, 1);
        }
    }

    fn write_subexp(enc: &mut SymbolEncoder, num_syms: u32, k: u32, v: u32) {
        let mut i = 0u32;
        let mut mk = 0u32;
        loop {
            let b = if i > 0 { k + i - 1 } else { k };
            let a = 1u32 << b;
            if num_syms <= mk + 3 * a {
                write_ns(enc, num_syms - mk, v - mk);
                return;
            }
            if v < mk + a {
                enc.literal(0, 1);
                enc.literal(v - mk, b);
                return;
            }
            enc.literal(1, 1);
            i += 1;
            mk += a;
        }
    }

    fn recenter(r: u32, v: u32) -> u32 {
        if v > 2 * r {
            v
        } else if v >= r {
            (v - r) << 1
        } else {
            ((r - v) << 1) - 1
        }
    }

    fn write_unsigned_subexp_with_ref(enc: &mut SymbolEncoder, n: u32, k: u32, reference: u32, v: u32) {
        let r = reference.clamp(0, n.saturating_sub(1));
        let coded = if (r << 1) <= n {
            recenter(r, v)
        } else {
            recenter(n - 1 - r, n - 1 - v)
        };
        write_subexp(enc, n, k, coded);
    }

    /// `decode_subexp_msac`/`ns_msac` roundtrip every value in a handful of
    /// `(num_syms, k)` alphabets -- the shapes LR's tap ranges (`k` 1..=4,
    /// `num_syms` up to 96) actually hit, per `WIENER_TAP_MAXV`-`MINV`+1 /
    /// `SGRPROJ_PRJ_MAX-MIN+1`.
    #[test]
    fn subexp_roundtrips_every_value() {
        for &(num_syms, k) in &[(16u32, 1u32), (32, 2), (64, 3), (96, 4), (128, 4), (3, 1), (1, 3)] {
            for v in 0..num_syms {
                let mut enc = SymbolEncoder::new();
                write_subexp(&mut enc, num_syms, k, v);
                let payload = enc.finish();
                let mut dec = SymbolDecoder::new(&payload);
                let got = decode_subexp_msac(&mut dec, num_syms, k);
                assert_eq!(got, v, "num_syms={num_syms} k={k} v={v}");
            }
        }
    }

    /// The recentred, ref-relative form `read_wiener_filter`/
    /// `read_sgrproj_filter` actually call -- every `(n, reference, v)`
    /// combination for LR's own tap-0 range (16 values, matching
    /// `WIENER_FILT_TAP0_MAXV-MINV+1`).
    #[test]
    fn unsigned_subexp_with_ref_roundtrips() {
        let n = 16u32;
        for k in 1..=4u32 {
            for reference in 0..n {
                for v in 0..n {
                    let mut enc = SymbolEncoder::new();
                    write_unsigned_subexp_with_ref(&mut enc, n, k, reference, v);
                    let payload = enc.finish();
                    let mut dec = SymbolDecoder::new(&payload);
                    let got = decode_unsigned_subexp_with_ref_msac(&mut dec, n, k, reference);
                    assert_eq!(got, v, "n={n} k={k} reference={reference} v={v}");
                }
            }
        }
    }

    /// Signed wrapper sanity: a handful of `(low, high, k, reference, v)`
    /// covering LR's actual Wiener tap-1/tap-2 and SGR `xqd0`/`xqd1` ranges.
    #[test]
    fn signed_subexp_with_ref_roundtrips() {
        let cases: [(i32, i32, u32); 4] = [
            (-5, 11, 1),   // Wiener tap0
            (-23, 9, 2),   // Wiener tap1
            (-17, 47, 3),  // Wiener tap2
            (-96, 32, 4),  // SGR xqd0
        ];
        for (low, high, k) in cases {
            let n = (high - low) as u32;
            for reference_v in [low, low + n as i32 / 3, high - 1] {
                for v in [low, low + n as i32 / 2, high - 1] {
                    let r = (reference_v - low).clamp(0, n as i32) as u32;
                    let coded_v = (v - low) as u32;
                    let mut enc = SymbolEncoder::new();
                    write_unsigned_subexp_with_ref(&mut enc, n, k, r, coded_v);
                    let payload = enc.finish();
                    let mut dec = SymbolDecoder::new(&payload);
                    let got = decode_signed_subexp_with_ref_msac(&mut dec, low, high, k, reference_v);
                    assert_eq!(got, v, "low={low} high={high} k={k} reference={reference_v} v={v}");
                }
            }
        }
    }
}
