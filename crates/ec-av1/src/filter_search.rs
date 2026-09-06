//! Encoder-side in-loop filter parameter search (spec 7.14 deblocking).
//!
//! The encoder does not carry a second implementation of the loop filters:
//! it hands its own coded tile back to this crate's DECODER with candidate
//! filter parameters ([`crate::decode::decode_key_frame_tile`] /
//! [`crate::decode::decode_inter_frame_tile`]) and keeps the picture that
//! comes out. That is what keeps the encoder's reference frame identical to
//! what a decoder reconstructs -- a hand-written encoder-side deblock would
//! be a second implementation to drift from the first, and the BD gate's
//! ffmpeg-decodes-sample-exact assertion is what would find the drift, one
//! lane later. The cost is one tile decode per candidate; the tile itself is
//! coded once (loop filter parameters live in the frame header, never in the
//! tile payload, so no candidate re-runs the search or the entropy coder).

use crate::encode::Picture;
use ec_av1_syntax::{CdefParams, LoopFilterParams};
use ec_core::Result;

/// Luma deblocking levels tried at the coarse stage, refined by +-1/+-2
/// around the winner (libaom's `av1_pick_filter_level` runs a golden-section
/// search over the same 0..63 range; this ladder is the charter's).
const COARSE: [u8; 7] = [0, 4, 8, 16, 24, 32, 48];

thread_local! {
    /// Every frame's chosen filter parameters, in coding
    /// order -- the BD gate prints the distribution once per clip
    /// (gate-blind-to-feature: a filter nobody counts is a filter that may
    /// never fire).
    static CHOSEN: std::cell::RefCell<Vec<Cand>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Takes and clears [`CHOSEN`].
#[allow(dead_code)] // read from the `#[cfg(test)]` BD gate
pub(crate) fn take_chosen_levels() -> Vec<((u8, u8), (u8, u8), (u8, u8))> {
    CHOSEN.with(|c| {
        std::mem::take(&mut *c.borrow_mut())
            .into_iter()
            .map(|c| (c.lf, c.y, c.uv))
            .collect()
    })
}

thread_local! {
    /// Per frame, in coding order: the `cdef_bits` chosen and how many 64x64
    /// superblocks ended up on each strength preset -- the BD gate prints the
    /// distribution once per clip (gate-blind-to-feature).
    static PRESET_COUNTS: std::cell::RefCell<Vec<(u8, Vec<usize>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Takes and clears [`PRESET_COUNTS`].
#[allow(dead_code)] // read from the `#[cfg(test)]` BD gate
pub(crate) fn take_cdef_presets() -> Vec<(u8, Vec<usize>)> {
    PRESET_COUNTS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Squared error between a decoded plane and the source it was coded from,
/// over the frame's own cropped `w x h` (the two planes have different
/// strides: the source is the encoder's padded coding surface).
fn plane_sse(dec: &[u16], dec_w: usize, src: &[u8], src_w: usize, w: usize, h: usize) -> u64 {
    let mut sse = 0u64;
    for row in 0..h {
        let (d, s) = (&dec[row * dec_w..][..w], &src[row * src_w..][..w]);
        for (&a, &b) in d.iter().zip(s) {
            let diff = i64::from(a) - i64::from(b);
            sse += (diff * diff) as u64;
        }
    }
    sse
}

/// [`plane_sse`], accumulated per CDEF unit instead of over the frame: `out`
/// is one entry per 64x64 SUPERBLOCK (stride `sb_cols`) and `unit` is the
/// unit side in this plane's own samples (64 for luma, 32 for 4:2:0 chroma),
/// which is what makes the luma and chroma sums land in the same slot.
fn unit_sse(
    dec: &[u16],
    dec_w: usize,
    src: &[u8],
    src_w: usize,
    w: usize,
    h: usize,
    unit: usize,
    sb_cols: usize,
    out: &mut [u64],
) {
    for row in 0..h {
        let base = (row / unit) * sb_cols;
        let (d, s) = (&dec[row * dec_w..][..w], &src[row * src_w..][..w]);
        for (col, (&a, &b)) in d.iter().zip(s).enumerate() {
            let diff = i64::from(a) - i64::from(b);
            out[base + col / unit] += (diff * diff) as u64;
        }
    }
}

/// CDEF primary strengths tried (libaom's own range is 0..15 for 8-bit
/// content) and the secondary ladder, whose only legal values are 0, 1, 2
/// and 4 -- 3 is not codeable (spec 5.9.19 codes a secondary strength of 4
/// as 3).
const CDEF_PRI: [u8; 5] = [0, 1, 2, 4, 8];
const CDEF_SEC: [u8; 4] = [0, 1, 2, 4];

/// One point of the filter search: the deblocking levels and the single
/// CDEF strength pair per plane type this encoder writes (`cdef_bits == 0`,
/// so no `cdef_idx` literal is coded in the tile at all and the choice is
/// purely a frame-header one).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cand {
    /// `(luma, chroma)` deblocking level.
    lf: (u8, u8),
    /// Luma `(primary, secondary)` CDEF strength.
    y: (u8, u8),
    /// Chroma `(primary, secondary)` CDEF strength.
    uv: (u8, u8),
}

/// One evaluated candidate: the decoded picture and its luma / chroma error.
struct Trial {
    cand: Cand,
    picture: Picture,
    sse_y: u64,
    sse_uv: u64,
    /// Luma + chroma error per 64x64 CDEF unit, in the tile writer's own
    /// superblock raster order -- what the per-unit `cdef_idx` choice below
    /// is made on.
    sse64: Vec<u64>,
}

/// Picks this frame's deblocking levels and CDEF strengths and hands back
/// the filtered reconstruction that goes with the winner.
///
/// `decode` reconstructs this frame's already-coded tile under the candidate
/// parameters; `source` is the encoder's padded (luma, U, V) coding surface
/// with luma stride `src_w`, `(fw, fh)` the frame header's own true size,
/// which is what `decode` crops to, and `damping` the CDEF damping the
/// caller's header carries.
///
/// A coordinate search, in the decoder's own filter order and each stage on
/// the plane it moves: deblocking luma (`level[0..2]`, kept equal -- nothing
/// here filters vertical and horizontal edges differently), deblocking
/// chroma (`level[2..4]`, a real choice of its own: flat chroma has no edges
/// to soften), then CDEF's luma and chroma primary/secondary strengths.
/// `sharpness` stays 0, the loop-filter deltas stay off, and `cdef_bits`
/// stays 0 -- one strength pair for the whole frame, which is the only CDEF
/// shape that costs no `cdef_idx` literal in the tile payload.
///
/// # Errors
/// Whatever `decode` returns: a stream this crate's own decoder refuses is
/// a caller's cue to keep its unfiltered reconstruction.
pub(crate) fn pick_filters(
    decode: impl Fn(&LoopFilterParams, &CdefParams) -> Result<Picture>,
    source: [&[u8]; 3],
    src_w: usize,
    (fw, fh): (usize, usize),
    damping: u8,
    (sb_cols, sb_rows): (usize, usize),
    lambda: f64,
) -> Result<(LoopFilterParams, CdefParams, Vec<u8>, Picture)> {
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
    let nsb = sb_cols * sb_rows;
    let mut trials: Vec<Trial> = Vec::new();
    let eval = |cand: Cand, trials: &mut Vec<Trial>| -> Result<usize> {
        if let Some(i) = trials.iter().position(|t| t.cand == cand) {
            return Ok(i);
        }
        let picture = decode(&loop_filter(cand), &cdef(cand, damping))?;
        let dec_cw = picture.width.div_ceil(2);
        let sse_y = plane_sse(&picture.y, picture.width, source[0], src_w, fw, fh);
        let sse_uv = plane_sse(&picture.u, dec_cw, source[1], src_w / 2, cw, ch)
            + plane_sse(&picture.v, dec_cw, source[2], src_w / 2, cw, ch);
        let mut sse64 = vec![0u64; nsb];
        unit_sse(&picture.y, picture.width, source[0], src_w, fw, fh, 64, sb_cols, &mut sse64);
        unit_sse(&picture.u, dec_cw, source[1], src_w / 2, cw, ch, 32, sb_cols, &mut sse64);
        unit_sse(&picture.v, dec_cw, source[2], src_w / 2, cw, ch, 32, sb_cols, &mut sse64);
        trials.push(Trial { cand, picture, sse_y, sse_uv, sse64 });
        Ok(trials.len() - 1)
    };
    // One stage of the coordinate search: try `values` in the slot `set`
    // moves and keep the one with the lowest error `sse` reads.
    let stage = |best: &mut Cand,
                     values: &[u8],
                     set: fn(&mut Cand, u8),
                     sse: fn(&Trial) -> u64,
                     trials: &mut Vec<Trial>|
     -> Result<()> {
        let mut winner = *best;
        let mut lowest = u64::MAX;
        for &v in values {
            let mut cand = *best;
            set(&mut cand, v);
            let i = eval(cand, trials)?;
            if sse(&trials[i]) < lowest {
                lowest = sse(&trials[i]);
                winner = cand;
            }
        }
        *best = winner;
        Ok(())
    };

    let mut best = Cand { lf: (0, 0), y: (0, 0), uv: (0, 0) };
    // Deblocking luma: the coarse ladder, then +-1/+-2 around its winner.
    stage(&mut best, &COARSE, |c, v| c.lf = (v, v), |t| t.sse_y, &mut trials)?;
    let refine: Vec<u8> = [-2i32, -1, 1, 2]
        .iter()
        .filter_map(|d| u8::try_from(i32::from(best.lf.0) + d).ok())
        .filter(|&l| l <= 63)
        .collect();
    stage(&mut best, &refine, |c, v| c.lf = (v, v), |t| t.sse_y, &mut trials)?;
    // Deblocking chroma, three points around the luma winner.
    let chroma_levels = [0, best.lf.0 / 2, best.lf.0];
    stage(&mut best, &chroma_levels, |c, v| c.lf.1 = v, |t| t.sse_uv, &mut trials)?;
    // CDEF, primary then secondary, luma on luma error and chroma on chroma.
    stage(&mut best, &CDEF_PRI, |c, v| c.y.0 = v, |t| t.sse_y, &mut trials)?;
    stage(&mut best, &CDEF_SEC, |c, v| c.y.1 = v, |t| t.sse_y, &mut trials)?;
    stage(&mut best, &CDEF_PRI, |c, v| c.uv.0 = v, |t| t.sse_uv, &mut trials)?;
    stage(&mut best, &CDEF_SEC, |c, v| c.uv.1 = v, |t| t.sse_uv, &mut trials)?;

    CHOSEN.with(|c| c.borrow_mut().push(best));

    // libaom `av1_cdef_search`'s shape, on the candidates this coordinate
    // search already decoded: every trial that ran under the WINNING
    // deblocking levels is a legal strength pair for this frame, and each
    // one already carries its own per-64x64 error. Pick up to eight of them
    // as the header's strength list and give every superblock the one that
    // costs it least -- the `cdef_idx` literal the tile writer then codes
    // (`crate::tile::arm_cdef_idx`).
    let mut presets: Vec<Cand> = vec![best];
    let pool: Vec<Cand> = trials
        .iter()
        .map(|t| t.cand)
        .filter(|c| c.lf == best.lf && *c != best)
        .collect();
    // How often each pool candidate would win a superblock outright, which
    // is the order the list is grown in (libaom picks its presets by the
    // same per-unit histogram, not by frame-level error).
    let sse_of = |c: &Cand| -> &Vec<u64> {
        &trials
            .iter()
            .find(|t| t.cand == *c)
            .expect("every pool candidate was evaluated")
            .sse64
    };
    let mut counts = vec![0usize; pool.len()];
    let best_sse = sse_of(&best).clone();
    for sb in 0..nsb {
        let mut winner = (best_sse[sb], usize::MAX);
        for (i, c) in pool.iter().enumerate() {
            if sse_of(c)[sb] < winner.0 {
                winner = (sse_of(c)[sb], i);
            }
        }
        if winner.1 != usize::MAX {
            counts[winner.1] += 1;
        }
    }
    let mut order: Vec<usize> = (0..pool.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(counts[i]));
    presets.extend(order.iter().take(7).map(|&i| pool[i]));
    // The cheapest list length: `bits` costs one literal per superblock plus
    // twelve header bits per extra pair, and buys whatever per-unit error the
    // wider choice removes.
    let unit_min = |n: usize, sb: usize| -> u64 {
        presets[..n].iter().map(|c| sse_of(c)[sb]).min().unwrap_or(0)
    };
    let mut bits = 0u8;
    let mut cost = best_sse.iter().sum::<u64>() as f64;
    for b in 1..=3u8 {
        let n = 1usize << b;
        if n > presets.len() {
            break;
        }
        let sse: u64 = (0..nsb).map(|sb| unit_min(n, sb)).sum();
        let c = sse as f64 + lambda * ((b as usize * nsb) as f64 + 12.0 * (n - 1) as f64);
        if c < cost {
            cost = c;
            bits = b;
        }
    }
    let grid: Vec<u8> = if bits == 0 {
        Vec::new()
    } else {
        let n = 1usize << bits;
        (0..nsb)
            .map(|sb| {
                (0..n)
                    .min_by_key(|&i| sse_of(&presets[i])[sb])
                    .expect("at least one preset") as u8
            })
            .collect()
    };
    let mut params = cdef(best, damping);
    params.bits = bits;
    for (i, c) in presets.iter().take(1usize << bits).enumerate() {
        params.y_pri_strength[i] = c.y.0;
        params.y_sec_strength[i] = c.y.1;
        params.uv_pri_strength[i] = c.uv.0;
        params.uv_sec_strength[i] = c.uv.1;
    }
    let mut counts_used = vec![0usize; 1usize << bits];
    for &g in &grid {
        counts_used[usize::from(g)] += 1;
    }
    if grid.is_empty() {
        counts_used = vec![nsb];
    }
    PRESET_COUNTS.with(|c| c.borrow_mut().push((bits, counts_used)));

    let win = trials
        .into_iter()
        .find(|t| t.cand == best)
        .expect("the winning candidate was evaluated");
    Ok((loop_filter(best), params, grid, win.picture))
}

/// The header's `loop_filter_params` for one candidate.
fn loop_filter(cand: Cand) -> LoopFilterParams {
    LoopFilterParams {
        level: [cand.lf.0, cand.lf.0, cand.lf.1, cand.lf.1],
        ..LoopFilterParams::default()
    }
}

/// The header's `cdef_params` for one candidate: a single strength pair per
/// plane type (`bits == 0`).
fn cdef(cand: Cand, damping: u8) -> CdefParams {
    let mut cdef = CdefParams { damping, bits: 0, ..CdefParams::default() };
    cdef.y_pri_strength[0] = cand.y.0;
    cdef.y_sec_strength[0] = cand.y.1;
    cdef.uv_pri_strength[0] = cand.uv.0;
    cdef.uv_sec_strength[0] = cand.uv.1;
    cdef
}

/// Writes the frame's own (cropped) region of `filtered` back over the
/// encoder's padded reconstruction planes, leaving whatever lies past the
/// frame edge untouched -- those columns/rows are the encoder's private
/// coding surface, never anything a decoder outputs.
pub(crate) fn splice(dst: &mut [u8], dst_w: usize, src: &[u16], src_w: usize, w: usize, h: usize) {
    for row in 0..h {
        for col in 0..w {
            dst[row * dst_w + col] = src[row * src_w + col] as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// lane-av1lr: loop restoration (spec 7.17). Unlike deblocking and CDEF, whose
// parameters live in the frame header, a restoration unit's filter is coded
// in the TILE payload, so a whole-frame re-decode cannot search it: the
// search runs here, on the two pictures the decoder captured between its own
// filter stages (`crate::decode::FrameCtx::capture_stages`), through the
// decoder's OWN Wiener kernel (`crate::restoration::
// apply_loop_restoration_plane`) -- never a second implementation.

/// The Wiener filters offered per unit, as the three free taps of a
/// direction (the centre tap is derived). Both directions take the same
/// candidate, so a unit picks one of four separable filters: two strengths
/// of low pass, one wider low pass, and one sharpener.
///
/// corner-cut: libaom's `av1_pick_filter_restoration` SOLVES for each unit's
/// taps (the Wiener-Hopf normal equations over that unit's autocorrelation)
/// instead of picking from a ladder, and also offers self-guided restoration.
/// The ceiling is whatever a four-point ladder leaves on the table; the
/// upgrade path is a per-unit solve feeding the same `write_wiener_filter`
/// syntax this already writes, plus `RestorationType::Switchable` for the
/// SGR arm.
const WIENER_CANDIDATES: [[i32; 3]; 4] = [[0, 0, 8], [0, 0, 16], [0, 4, 12], [0, 0, -8]];

/// What a unit spends on its own syntax, for the RD choice below: one
/// `restore_wiener` symbol for `RESTORE_NONE`, and roughly the two
/// directions' three subexp-coded taps when it takes a filter.
const LR_NONE_BITS: f64 = 1.0;
const LR_WIENER_BITS: f64 = 25.0;

/// Picks each luma restoration unit's filter, on the frame the decoder
/// captured after CDEF (with its post-deblock stripe borders), against the
/// source it was coded from. Returns one entry per unit in `rcol + rrow *
/// horz_units` order -- what `crate::tile::arm_lr` writes.
pub(crate) fn pick_restoration(
    stages: &crate::decode::FilterStages,
    source: &[u8],
    src_w: usize,
    (fw, fh): (usize, usize),
    lr: &ec_av1_syntax::LoopRestorationParams,
    lambda: f64,
    fctx: &crate::decode::FrameCtx,
) -> Vec<Option<crate::restoration::WienerInfo>> {
    let grid_dims = crate::restoration::RestorationGrid::new(lr, fw as u32, fh as u32);
    let (horz, vert) = (grid_dims.horz_units[0], grid_dims.vert_units[0]);
    let unit = lr.loop_restoration_size[0] as usize;
    let stride = stages.stride[0];
    let rows = crate::restoration::lr_unit_rows(fh, unit as u32, 0, lr.frame_restoration_type[0]);
    if rows.len() != vert {
        return Vec::new();
    }
    // Each unit's own sample span: the row walk's (a short last row is
    // swallowed by the one above it), and the column mirror of it.
    let span = |rrow: usize, rcol: usize| -> (usize, usize, usize, usize) {
        let (r0, r1) = rows[rrow];
        let c0 = rcol * unit;
        let c1 = if rcol + 1 == horz { fw } else { (rcol + 1) * unit };
        (r0 as usize, r1 as usize, c0, c1)
    };
    let unit_sse = |plane: &[u16], rrow: usize, rcol: usize| -> u64 {
        let (r0, r1, c0, c1) = span(rrow, rcol);
        let mut sse = 0u64;
        for row in r0..r1 {
            for col in c0..c1 {
                let diff = i64::from(plane[row * stride + col]) - i64::from(source[row * src_w + col]);
                sse += (diff * diff) as u64;
            }
        }
        sse
    };
    // Cost of leaving every unit alone, then one whole-plane pass per
    // candidate to price taking it.
    let mut best: Vec<(f64, Option<crate::restoration::WienerInfo>)> = (0..horz * vert)
        .map(|i| {
            let sse = unit_sse(&stages.cdefed[0], i / horz, i % horz) as f64;
            (sse + lambda * LR_NONE_BITS, None)
        })
        .collect();
    let mut out = Vec::new();
    for taps in WIENER_CANDIDATES {
        let info = crate::restoration::wiener_from_taps(taps, taps);
        let mut grid = crate::restoration::RestorationGrid::new(lr, fw as u32, fh as u32);
        for rrow in 0..vert {
            for rcol in 0..horz {
                grid.set(0, rrow, rcol, crate::restoration::UnitFilter::Wiener(info));
            }
        }
        crate::restoration::apply_loop_restoration_plane(
            &stages.cdefed[0],
            &stages.deblocked[0],
            stride,
            fw,
            fh,
            0,
            lr.frame_restoration_type[0],
            lr.loop_restoration_size[0],
            &grid,
            0,
            &mut out,
            None,
            fctx,
        );
        for rrow in 0..vert {
            for rcol in 0..horz {
                let cost = unit_sse(&out, rrow, rcol) as f64 + lambda * LR_WIENER_BITS;
                let slot = &mut best[rcol + rrow * horz];
                if cost < slot.0 {
                    *slot = (cost, Some(info));
                }
            }
        }
    }
    let chosen: Vec<Option<crate::restoration::WienerInfo>> =
        best.into_iter().map(|(_, f)| f).collect();
    LR_CHOSEN.with(|c| {
        let mut hist = c.borrow_mut();
        for f in &chosen {
            hist.0 += usize::from(f.is_none());
            hist.1 += usize::from(f.is_some());
        }
    });
    chosen
}

thread_local! {
    /// `(RESTORE_NONE units, Wiener units)` over every frame since the last
    /// [`take_lr_histogram`] -- the BD gate's own proof the feature fires.
    static LR_CHOSEN: std::cell::RefCell<(usize, usize)> =
        const { std::cell::RefCell::new((0, 0)) };
}

/// Takes and clears [`LR_CHOSEN`].
#[allow(dead_code)] // read from the `#[cfg(test)]` BD gate
pub(crate) fn take_lr_histogram() -> (usize, usize) {
    LR_CHOSEN.with(|c| std::mem::take(&mut *c.borrow_mut()))
}
