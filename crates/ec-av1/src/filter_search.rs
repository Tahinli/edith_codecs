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
use ec_av1_syntax::LoopFilterParams;
use ec_core::Result;

/// Luma deblocking levels tried at the coarse stage, refined by +-1/+-2
/// around the winner (libaom's `av1_pick_filter_level` runs a golden-section
/// search over the same 0..63 range; this ladder is the charter's).
const COARSE: [u8; 7] = [0, 4, 8, 16, 24, 32, 48];

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

/// One evaluated candidate: the decoded picture and its luma / chroma error.
struct Trial {
    level: (u8, u8),
    picture: Picture,
    sse_y: u64,
    sse_uv: u64,
}

/// Picks `loop_filter_level[0..4]` for one frame and hands back the filtered
/// reconstruction that goes with the winner.
///
/// `decode` reconstructs this frame's already-coded tile under the candidate
/// parameters; `source` is the encoder's padded (luma, U, V) coding surface
/// with luma stride `src_w`, and `(fw, fh)` the frame header's own true
/// size, which is what `decode` crops to.
///
/// Luma (`level[0..2]`, kept equal -- this encoder has no reason to filter
/// vertical and horizontal edges differently) is chosen on luma SSE with the
/// chroma levels riding along, then chroma (`level[2..4]`) is chosen on
/// chroma SSE with luma fixed. `sharpness` stays 0 and the ref/mode deltas
/// stay off, as [`LoopFilterParams::default`] has them.
///
/// # Errors
/// Whatever `decode` returns: a stream this crate's own decoder refuses is
/// a caller's cue to keep its unfiltered reconstruction.
pub(crate) fn pick_deblock(
    decode: impl Fn(&LoopFilterParams) -> Result<Picture>,
    source: [&[u8]; 3],
    src_w: usize,
    (fw, fh): (usize, usize),
) -> Result<(LoopFilterParams, Picture)> {
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
    let mut trials: Vec<Trial> = Vec::new();
    let eval = |level: (u8, u8), trials: &mut Vec<Trial>| -> Result<usize> {
        if let Some(i) = trials.iter().position(|t| t.level == level) {
            return Ok(i);
        }
        let lf = LoopFilterParams {
            level: [level.0, level.0, level.1, level.1],
            ..LoopFilterParams::default()
        };
        let picture = decode(&lf)?;
        let dec_cw = picture.width.div_ceil(2);
        let sse_y = plane_sse(&picture.y, picture.width, source[0], src_w, fw, fh);
        let sse_uv = plane_sse(&picture.u, dec_cw, source[1], src_w / 2, cw, ch)
            + plane_sse(&picture.v, dec_cw, source[2], src_w / 2, cw, ch);
        trials.push(Trial { level, picture, sse_y, sse_uv });
        Ok(trials.len() - 1)
    };

    // Luma: the coarse ladder, then +-1/+-2 around its winner.
    let mut best_y = 0u8;
    let mut best_sse = u64::MAX;
    for l in COARSE {
        let i = eval((l, l), &mut trials)?;
        if trials[i].sse_y < best_sse {
            best_sse = trials[i].sse_y;
            best_y = l;
        }
    }
    for delta in [-2i32, -1, 1, 2] {
        let Ok(l) = u8::try_from(i32::from(best_y) + delta) else {
            continue;
        };
        if l > 63 {
            continue;
        }
        let i = eval((l, l), &mut trials)?;
        if trials[i].sse_y < best_sse {
            best_sse = trials[i].sse_y;
            best_y = l;
        }
    }

    // Chroma: three points around the luma winner. A chroma level of 0 with
    // luma non-zero is a real outcome (flat chroma has no edges to soften),
    // which is why `level[2..4]` is searched at all rather than tied to
    // `level[0]`.
    let mut best_uv = best_y;
    let mut best_sse_uv = u64::MAX;
    for c in [0, best_y / 2, best_y] {
        let i = eval((best_y, c), &mut trials)?;
        if trials[i].sse_uv < best_sse_uv {
            best_sse_uv = trials[i].sse_uv;
            best_uv = c;
        }
    }

    let win = trials
        .into_iter()
        .find(|t| t.level == (best_y, best_uv))
        .expect("the winning pair was evaluated");
    Ok((
        LoopFilterParams {
            level: [best_y, best_y, best_uv, best_uv],
            ..LoopFilterParams::default()
        },
        win.picture,
    ))
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
