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
        let rcol0 = ceil_div(mi_col * mi_size, unit_size);
        let rcol1 = ceil_div((mi_col + sb_mi) * mi_size, unit_size).min(horz_units);
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

fn read_lr_unit(
    dec: &mut SymbolDecoder,
    cdfs: &mut crate::cdf_state::Cdfs,
    ftype: RestorationType,
    chroma: bool,
    reference: &mut (WienerInfo, SgrprojInfo),
) -> UnitFilter {
    match ftype {
        RestorationType::Switchable => {
            let t = dec.symbol(&mut cdfs.restore_switchable);
            match t {
                0 => UnitFilter::None,
                1 => UnitFilter::Wiener(read_wiener_filter(dec, chroma, &mut reference.0)),
                _ => UnitFilter::Sgrproj(read_sgrproj_filter(dec, &mut reference.1)),
            }
        }
        RestorationType::Wiener => {
            if dec.symbol(&mut cdfs.restore_wiener) != 0 {
                UnitFilter::Wiener(read_wiener_filter(dec, chroma, &mut reference.0))
            } else {
                UnitFilter::None
            }
        }
        RestorationType::Sgrproj => {
            if dec.symbol(&mut cdfs.restore_sgrproj) != 0 {
                UnitFilter::Sgrproj(read_sgrproj_filter(dec, &mut reference.1))
            } else {
                UnitFilter::None
            }
        }
        RestorationType::None => UnitFilter::None,
    }
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
