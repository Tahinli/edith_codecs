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
) -> Result<(LoopFilterParams, CdefParams, Picture)> {
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
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
        trials.push(Trial { cand, picture, sse_y, sse_uv });
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
    let win = trials
        .into_iter()
        .find(|t| t.cand == best)
        .expect("the winning candidate was evaluated");
    Ok((loop_filter(best), cdef(best, damping), win.picture))
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
