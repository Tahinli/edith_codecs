//! DCT coefficient (token) decoding — RFC 6386 §13.
//!
//! Per 4x4 block: tokens are tree-decoded at
//! `probs[type][band[position]][ctx]` where `ctx` (0..2) is the zero-
//! neighbour count — sum of the above/left block contexts for the first
//! coded coefficient, then derived from the magnitude of the previous
//! coefficient within the block (§13.3). Coefficients are written in
//! natural (de-zigzagged) order and dequantized on the fly (the first
//! coded position takes the DC factor, all others the AC factor — the
//! same arithmetic dixie applies inline and libvpx applies in
//! `vp8_dequantize_b`).
//!
//! After an EOB the block ends; after a `DCT_0` token the tree decode of
//! the next coefficient skips the EOB branch (§13.2). A block whose
//! coefficients fill all 16 positions cannot take an EOB (the decoder
//! stops at 16 exactly like the reference decoder, down to the sentinel
//! band entry).

use crate::bool::BoolDecoder;
use crate::tables::{PCAT3, PCAT4, PCAT5, PCAT6};

/// De-zigzag: coefficient scan position -> natural order index
/// (RFC 6386 §13, dixie/libvpx `zigzag`).
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Band per coefficient position, with the reference decoder's sentinel
/// entry at 16 (band 0 on the malformed no-EOB path — libvpx `kBands`).
const BANDS: [u8; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];

/// Extra-bit probability tables per category (cat3..cat6, RFC 6386
/// §13.2; the cat1/cat2 probabilities 159/165+145 are inlined in the
/// tree decode exactly as the reference does). Trailing 0 is the
/// DCTextra sentinel.
const CAT_EXTRA: [&[u8]; 4] = [&PCAT3, &PCAT4, &PCAT5, &PCAT6];

/// Dequantization factors for one macroblock (already scaled: Y2 AC has
/// the ×155/100 min-8 rule applied, Y2 DC ×2, UV DC capped at 132).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dq {
    pub y1_dc: i16,
    pub y1_ac: i16,
    pub uv_dc: i16,
    pub uv_ac: i16,
    pub y2_dc: i16,
    pub y2_ac: i16,
}

/// The 25 blocks of one macroblock, dequantized, in natural coefficient
/// order. `eobs[b]` is the number of significant coefficients (position
/// of the last non-zero + 1); `eob_mask` mirrors dixie's: bit b set iff
/// block b ended with more than one significant coefficient, bit 31 set
/// iff ANY block had a non-zero coefficient.
pub(crate) struct MbCoeffs {
    pub y: [[i16; 16]; 16],
    pub u: [[i16; 16]; 4],
    pub v: [[i16; 16]; 4],
    pub y2: [i16; 16],
    pub has_y2: bool,
    pub eobs: [u8; 25],
    pub eob_mask: u32,
}

impl MbCoeffs {
    pub(crate) fn empty(has_y2: bool) -> Self {
        Self {
            y: [[0; 16]; 16],
            u: [[0; 16]; 4],
            v: [[0; 16]; 4],
            y2: [0; 16],
            has_y2,
            eobs: [0; 25],
            eob_mask: 0,
        }
    }

    /// Whether any block carries a non-zero coefficient (the loop
    /// filter's interior-edge condition, §15.1).
    pub fn has_nonzero(&self) -> bool {
        self.eob_mask & (1 << 31) != 0
    }
}

/// Left/above context slot of each block (dixie `left_context_index` /
/// `above_context_index`).
const LEFT_INDEX: [usize; 25] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8,
];
const ABOVE_INDEX: [usize; 25] = [
    0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 8,
];

/// Token entropy contexts (dixie/libvpx layout): indices 0-3 are the Y
/// subblock columns, 4-5 U, 6-7 V, 8 the Y2 block. `left` is the running
/// one-MB-wide column state; `above` is the per-MB-column state for the
/// current row (9 slots per MB column).
pub(crate) struct TokenContexts {
    pub above: Vec<u8>,
    pub left: [u8; 9],
}

impl TokenContexts {
    /// `mb_cols` entries of 9 slots for the above state.
    pub fn new(mb_cols: usize) -> Self {
        Self {
            above: vec![0u8; mb_cols * 9],
            left: [0; 9],
        }
    }

    /// Reset the above state at the start of a row (§13.3: non-existent
    /// neighbours are empty).
    pub fn reset_above_row(&mut self) {
        self.above.iter_mut().for_each(|s| *s = 0);
    }

    /// Reset the left state at the start of a row (the caller MUST call
    /// this once per MB row before decoding its tokens).
    pub fn reset_left(&mut self) {
        self.left = [0; 9];
    }

    /// Skip-MB context reset (dixie `reset_mb_context`): everything to 0
    /// except the Y2 slots, which survive iff this MB has no Y2 block.
    pub fn reset_mb(&mut self, mb_col: usize, has_y2: bool) {
        let a = &mut self.above[mb_col * 9..mb_col * 9 + 9];
        a[..8].fill(0);
        self.left[..8].fill(0);
        if has_y2 {
            a[8] = 0;
            self.left[8] = 0;
        }
    }

    fn ctx_pair(&self, mb_col: usize, block: usize) -> (u8, u8) {
        let a = &self.above[mb_col * 9..mb_col * 9 + 9];
        (self.left[LEFT_INDEX[block]], a[ABOVE_INDEX[block]])
    }

    fn set_ctx(&mut self, mb_col: usize, block: usize, v: u8) {
        self.left[LEFT_INDEX[block]] = v;
        self.above[mb_col * 9 + ABOVE_INDEX[block]] = v;
    }
}

/// Decode one 4x4 block's tokens (libvpx `GetCoeffs` / RFC §13.2-13.3
/// verbatim). `probs_t` is `[band][ctx][node]` for the block's plane
/// type; `n0` the first coded position (1 for Y blocks after a Y2).
/// Returns the number of significant coefficients. Coefficients are
/// written dequantized into `out` (natural order).
fn get_coeffs(
    bc: &mut BoolDecoder<'_>,
    probs_t: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    n0: usize,
    dc_factor: i16,
    ac_factor: i16,
    out: &mut [i16; 16],
) -> usize {
    let trace = crate::trace_enabled();
    // One token decision: decode the bool, then — under EC_VP8_TRACE —
    // print the consulted (n, band, ctx, prob) and the decoded bit so the
    // stream can be diffed read-for-read against scripts/vp8ref_model.py.
    macro_rules! g {
        ($n:expr, $band:expr, $c:expr, $prob:expr) => {{
            let bit = bc.read_bool($prob);
            if trace {
                eprintln!(
                    "G n={} band={} ctx={} prob={} bit={}",
                    $n, $band, $c, $prob, u8::from(bit)
                );
            }
            bit
        }};
    }
    let mut n = n0;
    let mut band = BANDS[n0];
    let mut c = ctx;
    let mut p = &probs_t[band as usize][c];
    if !g!(n, band, c, p[0]) {
        return 0; // immediate EOB
    }
    loop {
        n += 1;
        let mut v: i32;
        if !g!(n, band, c, p[1]) {
            // DCT_0: the next decision skips the EOB branch (§13.2) and
            // libvpx forces the context to 0 after a zero token.
            c = 0;
            band = BANDS[n];
            p = &probs_t[band as usize][0];
        } else {
            if !g!(n, band, c, p[2]) {
                c = 1;
                band = BANDS[n];
                p = &probs_t[band as usize][1];
                v = 1;
            } else {
                if !g!(n, band, c, p[3]) {
                    if !g!(n, band, c, p[4]) {
                        v = 2;
                    } else {
                        v = 3 + i32::from(g!(n, band, c, p[5]));
                    }
                } else {
                    if !g!(n, band, c, p[6]) {
                        if !g!(n, band, c, p[7]) {
                            v = 5 + i32::from(g!(n, band, c, 159u8));
                        } else {
                            v = 7 + 2 * i32::from(g!(n, band, c, 165u8));
                            v += i32::from(g!(n, band, c, 145u8));
                        }
                    } else {
                        let bit1 = usize::from(g!(n, band, c, p[8]));
                        let bit0 = usize::from(g!(n, band, c, p[9 + bit1]));
                        let cat = 2 * bit1 + bit0;
                        v = 0;
                        for &prob in CAT_EXTRA[cat].iter() {
                            if prob == 0 {
                                break;
                            }
                            v += v + i32::from(g!(n, band, c, prob));
                        }
                        v += 3 + (8 << cat);
                    }
                }
                c = 2;
                band = BANDS[n];
                p = &probs_t[band as usize][2];
            }

            // Sign flag, then dequantize (first coded position is DC).
            // Shared by the ONE and magnitude paths, exactly like the
            // reference walk (libvpx detokenize.c GetCoeffs).
            let neg = g!(n, band, c, 128u8);
            let mag = if neg { -v } else { v };
            out[ZIGZAG[n - 1]] = if n - 1 == 0 {
                (mag * i32::from(dc_factor)) as i16
            } else {
                (mag * i32::from(ac_factor)) as i16
            };
            if n < 16 && !g!(n, band, c, p[0]) {
                return n; // EOB
            }
        }
        if n == 16 {
            return 16; // malformed without EOB: stop like the reference
        }
    }
}

/// Decode all 25 blocks of one macroblock (dixie `decode_mb_tokens`):
/// Y2 first when present, then the 16 Y, then U, then V. A skipped MB
/// must not be run through here — call [`skip_mb_tokens`] instead.
pub(crate) fn decode_mb_tokens(
    bc: &mut BoolDecoder<'_>,
    probs: &[[[[u8; 11]; 3]; 8]; 4],
    dq: &Dq,
    has_y2: bool,
    ctxs: &mut TokenContexts,
    mb_col: usize,
) -> MbCoeffs {
    let mut out = MbCoeffs::empty(has_y2);
    let mut eob_mask = 0u32;

    // Decode one block and fold its contexts/mask in.
    macro_rules! block {
        ($idx:expr, $ptype:expr, $dc:expr, $ac:expr, $slot:expr) => {{
            let n0 = usize::from($ptype == 0);
            let (l, a) = ctxs.ctx_pair(mb_col, $idx);
            let ctx = (usize::from(l) + usize::from(a)).min(2);
            if crate::trace_enabled() {
                eprintln!("TB {} ctx={} l={} a={}", $idx, ctx, l, a);
            }
            let eob = get_coeffs(bc, &probs[$ptype], ctx, n0, $dc, $ac, $slot);
            if crate::trace_enabled() {
                eprintln!("TBE {} eob={}", $idx, eob);
            }
            out.eobs[$idx] = eob as u8;
            let t = u8::from(eob > 0);
            ctxs.set_ctx(mb_col, $idx, t);
            eob_mask |= (u32::from(eob > 1)) << $idx;
            eob_mask |= u32::from(t != 0) << 31;
        }};
    }

    if has_y2 {
        block!(24, 1, dq.y2_dc, dq.y2_ac, &mut out.y2);
        for b in 0..16 {
            block!(b, 0, dq.y1_dc, dq.y1_ac, &mut out.y[b]);
        }
    } else {
        for b in 0..16 {
            block!(b, 3, dq.y1_dc, dq.y1_ac, &mut out.y[b]);
        }
    }
    for b in 0..4 {
        block!(16 + b, 2, dq.uv_dc, dq.uv_ac, &mut out.u[b]);
    }
    for b in 0..4 {
        block!(20 + b, 2, dq.uv_dc, dq.uv_ac, &mut out.v[b]);
    }

    out.eob_mask = eob_mask;
    out
}

/// The context reset a skipped MB applies (dixie `reset_mb_context`):
/// the returned [`MbCoeffs`] is all-zero and the contexts look like a
/// fully-empty MB — except the Y2 slots survive when the skipped MB has
/// no Y2 block.
pub(crate) fn skip_mb_tokens(ctxs: &mut TokenContexts, mb_col: usize, has_y2: bool) -> MbCoeffs {
    ctxs.reset_mb(mb_col, has_y2);
    MbCoeffs::empty(has_y2)
}
