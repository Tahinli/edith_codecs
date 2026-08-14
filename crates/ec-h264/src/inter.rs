//! Inter prediction sample construction (spec 8.4.2): the quarter-sample luma
//! interpolation of 8.4.2.2.1, the eighth-sample chroma interpolation of
//! 8.4.2.2.2 and the weighted combination of 8.4.2.3.
//!
//! Reference planes are the same padded [`crate::decoder::Plane8`] the decoder
//! writes into, with the borders replicated once per picture
//! (`Plane8::extend_borders`). That turns the per-sample `Clip3` of Equations
//! 8-239 and 8-240 into a single clamp of the block origin: any position the
//! spec would clip to an edge sample lands on the replicated copy of that same
//! edge sample, so the interpolation loop never bounds-checks and never
//! branches per sample. The clamp bounds below are what makes that exact —
//! see [`clamp_origin`].

use wide::{i16x8, i16x16, i32x8};

/// A reference picture plane for motion compensation.
pub(crate) struct RefPlane<'a> {
    /// The whole padded plane.
    pub data: &'a [u8],
    /// Row pitch.
    pub stride: usize,
    /// Index of picture sample (0, 0).
    pub origin: usize,
    /// Picture width in samples.
    pub width: usize,
    /// Picture height in samples.
    pub height: usize,
    /// Replicated border width on every side.
    pub pad: usize,
}

impl RefPlane<'_> {
    #[inline]
    fn at(&self, x: i32, y: i32) -> usize {
        (self.origin as isize + y as isize * self.stride as isize + x as isize) as usize
    }
}

/// Clamp a block origin so that every tap the interpolator reads stays inside
/// the replicated border while producing the samples Equations 8-239/8-240
/// would have clipped to.
///
/// `left`/`right` are the tap reach on each side (2 and 3 for the 6-tap luma
/// filter, 0 and 1 for the bilinear chroma filter) and `size` the block extent.
/// Once the block is completely outside the picture every tap resolves to the
/// same edge sample, so clamping the origin to the last position where that is
/// still true is exact rather than approximate.
#[inline]
fn clamp_origin(v: i32, size: usize, extent: usize, pad: usize, left: i32, right: i32) -> i32 {
    let lo = -(pad as i32) + left;
    let hi = extent as i32 + pad as i32 - right - size as i32;
    v.clamp(lo.min(hi), hi)
}

/// Sixteen samples as 16-bit lanes, zero past the end of the plane.
///
/// A partition is at most 16 samples wide, so one vector covers a whole row and
/// the lanes past `w` are simply never stored. Every index the interpolator
/// reaches is inside the replicated border (see [`clamp_origin`]), so the zero
/// fill is reachable only by those discarded lanes.
#[inline]
fn load16(data: &[u8], at: usize) -> i16x16 {
    let mut a = [0i16; 16];
    // The fixed-length slice is what makes this one widening load: a zip
    // against an open-ended slice leaves the trip count unknown and the
    // compiler emits a byte loop instead.
    match data.get(at..at + 16) {
        Some(row) => {
            for (o, &b) in a.iter_mut().zip(row) {
                *o = i16::from(b);
            }
        }
        None => {
            for (o, &b) in a.iter_mut().zip(&data[at..]) {
                *o = i16::from(b);
            }
        }
    }
    i16x16::from(a)
}

/// The same over eight lanes, for chroma blocks (at most eight wide).
#[inline]
fn load8(data: &[u8], at: usize) -> i16x8 {
    let mut a = [0i16; 8];
    match data.get(at..at + 8) {
        Some(row) => {
            for (o, &b) in a.iter_mut().zip(row) {
                *o = i16::from(b);
            }
        }
        None => {
            for (o, &b) in a.iter_mut().zip(&data[at..]) {
                *o = i16::from(b);
            }
        }
    }
    i16x8::from(a)
}

/// The same 6-tap filter over eight neighbouring positions.
///
/// Eight lanes is one SSE2 register, which is the widest vector this crate can
/// assume without a target feature it is not allowed to require. A partition
/// narrower than sixteen samples therefore costs half or a quarter as much
/// here as it does in the sixteen-lane form, where the surplus lanes are
/// computed and then discarded.
#[inline]
fn tap6_8(data: &[u8], at: usize, step: usize) -> i16x8 {
    let l = |k: isize| load8(data, (at as isize + k * step as isize) as usize);
    let five = i16x8::splat(5);
    let twenty = i16x8::splat(20);
    l(-2) - five * l(-1) + twenty * l(0) + twenty * l(1) - five * l(2) + l(3)
}

/// The same filter over sixteen positions, for a full-width partition.
#[inline]
fn tap6_16(data: &[u8], at: usize, step: usize) -> i16x16 {
    let l = |k: isize| load16(data, (at as isize + k * step as isize) as usize);
    let five = i16x16::splat(5);
    let twenty = i16x16::splat(20);
    l(-2) - five * l(-1) + twenty * l(0) + twenty * l(1) - five * l(2) + l(3)
}

/// `Clip1Y( ( v + 16 ) >> 5 )` over a row, storing the first `dst.len()` lanes.
#[inline]
fn store_half8(dst: &mut [u8], v: i16x8) {
    let c = ((v + i16x8::splat(16)) >> 5u32)
        .max(i16x8::ZERO)
        .min(i16x8::splat(255));
    for (o, &s) in dst.iter_mut().zip(c.as_array()) {
        *o = s as u8;
    }
}

/// The same over sixteen lanes.
#[inline]
fn store_half16(dst: &mut [u8], v: i16x16) {
    let c = ((v + i16x16::splat(16)) >> 5u32)
        .max(i16x16::ZERO)
        .min(i16x16::splat(255));
    for (o, &s) in dst.iter_mut().zip(c.as_array()) {
        *o = s as u8;
    }
}

/// One 6-tap half-sample pass over a `w` x `h` block whose top-left sample is
/// `base`, into a `w`-pitch buffer. `step` is the tap spacing: 1 for the
/// horizontal filter, the plane stride for the vertical one.
///
/// A partition is 16, 8 or 4 samples wide, so the narrow widths take a single
/// eight-lane vector rather than a sixteen-lane one whose upper half is
/// discarded.
#[inline]
fn half_block(
    dst: &mut [u8],
    data: &[u8],
    base: usize,
    stride: usize,
    step: usize,
    w: usize,
    h: usize,
) {
    if w == 16 {
        for row in 0..h {
            store_half16(
                &mut dst[row * 16..row * 16 + 16],
                tap6_16(data, base + row * stride, step),
            );
        }
    } else {
        for row in 0..h {
            store_half8(
                &mut dst[row * w..row * w + w],
                tap6_8(data, base + row * stride, step),
            );
        }
    }
}

/// Eight intermediate values as 32-bit lanes, zero past the end.
#[inline]
fn load_inter8(v: &[i16], at: usize) -> i32x8 {
    let mut a = [0i32; 8];
    for (o, &s) in a.iter_mut().zip(&v[at..]) {
        *o = i32::from(s);
    }
    i32x8::from(a)
}

/// The 6-tap filter over already-filtered intermediate values (Equation
/// 8-246), eight columns at a time. Those sums reach 449820, so this pass is
/// 32-bit where the first one was 16-bit.
#[inline]
fn tap6_inter8(v: &[i16], at: usize, step: usize) -> i32x8 {
    let l = |k: isize| load_inter8(v, (at as isize + k * step as isize) as usize);
    let five = i32x8::splat(5);
    let twenty = i32x8::splat(20);
    l(-2) - five * l(-1) + twenty * l(0) + twenty * l(1) - five * l(2) + l(3)
}

/// `Clip1Y( ( v + 512 ) >> 10 )` over eight lanes.
#[inline]
fn store_centre(dst: &mut [u8], v: i32x8) {
    let c = ((v + i32x8::splat(512)) >> 10u32)
        .max(i32x8::ZERO)
        .min(i32x8::splat(255));
    for (o, &s) in dst.iter_mut().zip(c.as_array()) {
        *o = s as u8;
    }
}

/// The rounded average of Equation 8-247 over `n` samples, sixteen at a time.
#[inline]
fn avg_rows(out: &mut [u8], a: &[u8], b: &[u8], n: usize) {
    let mut i = 0;
    while i + 16 <= n {
        let v = (load16(a, i) + load16(b, i) + i16x16::ONE) >> 1u32;
        for (o, &s) in out[i..i + 16].iter_mut().zip(v.as_array()) {
            *o = s as u8;
        }
        i += 16;
    }
    for k in i..n {
        out[k] = avg(a[k], b[k]);
    }
}

/// The same average, written into a destination of pitch `os` from two
/// `w`-pitch sources.
#[inline]
fn avg_rows_to(out: &mut [u8], os: usize, a: &[u8], b: &[u8], w: usize, h: usize) {
    if os == w {
        avg_rows(out, a, b, w * h);
        return;
    }
    for row in 0..h {
        avg_rows(
            &mut out[row * os..row * os + w],
            &a[row * w..],
            &b[row * w..],
            w,
        );
    }
}

/// Copy `w` x `h` samples from a `w`-pitch source into an `os`-pitch
/// destination.
#[inline]
fn copy_rows(out: &mut [u8], os: usize, src: &[u8], w: usize, h: usize) {
    for row in 0..h {
        out[row * os..row * os + w].copy_from_slice(&src[row * w..row * w + w]);
    }
}

#[inline]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline]
fn avg(a: u8, b: u8) -> u8 {
    ((u32::from(a) + u32::from(b) + 1) >> 1) as u8
}

/// Widest intermediate row the luma interpolator needs: a 16-wide partition
/// plus the five taps of the horizontal filter.
const TMP_W: usize = 16 + 5;

/// Index of the top-left sample of a whole-sample block, clamped exactly as
/// [`mc_luma`] clamps it.
///
/// A motion search spends most of its time on whole-sample positions, where
/// prediction is a copy; this lets it read the reference plane in place
/// instead of copying every candidate into a buffer first.
#[inline]
pub(crate) fn integer_origin(r: &RefPlane<'_>, x: i32, y: i32, w: usize, h: usize) -> usize {
    let xi = clamp_origin(x, w, r.width, r.pad, 2, 3);
    let yi = clamp_origin(y, h, r.height, r.pad, 2, 3);
    r.at(xi, yi)
}

/// Luma quarter-sample interpolation (8.4.2.2.1) of a `w` x `h` partition whose
/// full-sample origin is `(x, y)` and whose motion vector is `mv` in quarter
/// samples. Writes `w` x `h` prediction samples into `out` at pitch `os`.
///
/// The pitch is a parameter so that the single-list, default-weight case —
/// which is most of a P slice — interpolates straight into the picture instead
/// of into a temporary that is then copied there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mc_luma(
    r: &RefPlane<'_>,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    os: usize,
    out: &mut [u8],
) {
    let (fx, fy) = ((mv[0] & 3) as usize, (mv[1] & 3) as usize);
    let xi = clamp_origin(x + (mv[0] as i32 >> 2), w, r.width, r.pad, 2, 3);
    let yi = clamp_origin(y + (mv[1] as i32 >> 2), h, r.height, r.pad, 2, 3);
    let stride = r.stride;
    let base = r.at(xi, yi);

    // Full sample: a straight copy.
    if fx == 0 && fy == 0 {
        for row in 0..h {
            let src = base + row * stride;
            out[row * os..row * os + w].copy_from_slice(&r.data[src..src + w]);
        }
        return;
    }

    // Half-sample rows ("b" of Equation 8-243) for rows yi..yi+h, plus one more
    // row when the quarter position also needs "s".
    let need_b = fx != 0 && fy != 2;
    let need_h = fy != 0 && fx != 2;
    let need_j = (fx == 2 && fy != 0) || (fy == 2 && fx != 0);

    let mut bb = [0u8; 16 * 17];
    if need_b {
        // "s" is the same filter one row down, so row fy>>1 selects b or s.
        let row0 = yi + if fy == 3 { 1 } else { 0 };
        half_block(&mut bb, r.data, r.at(xi, row0), stride, 1, w, h);
    }
    let mut hh = [0u8; 16 * 17];
    if need_h {
        // "m" is the same filter one column right, so column fx>>1 selects h or m.
        let col0 = xi + if fx == 3 { 1 } else { 0 };
        half_block(&mut hh, r.data, r.at(col0, yi), stride, stride, w, h);
    }
    let mut jj = [0u8; 16 * 16];
    if need_j {
        // Equation 8-246: filter horizontally into intermediates, then
        // vertically over those. The horizontal pass needs two rows above and
        // three below the partition.
        let mut inter = [0i16; TMP_W * (16 + 5)];
        for row in 0..h + 5 {
            let src = r.at(xi, yi + row as i32 - 2);
            let base = row * TMP_W;
            if w == 16 {
                let v = tap6_16(r.data, src, 1);
                inter[base..base + 16].copy_from_slice(v.as_array());
            } else {
                let v = tap6_8(r.data, src, 1);
                for (o, &t) in inter[base..base + w].iter_mut().zip(v.as_array()) {
                    *o = t;
                }
            }
        }
        for row in 0..h {
            for col in (0..w).step_by(8) {
                let v = tap6_inter8(&inter, (row + 2) * TMP_W + col, TMP_W);
                let n = (w - col).min(8);
                store_centre(&mut jj[row * w + col..row * w + col + n], v);
            }
        }
    }

    // Table 8-12: each quarter position is one of the half-sample arrays, or
    // the average of two of them. Resolving it once per partition rather than
    // once per sample is what keeps the copy loops straight.
    match (fx, fy) {
        (2, 0) => copy_rows(out, os, &bb, w, h),
        (0, 2) => copy_rows(out, os, &hh, w, h),
        (2, 2) => copy_rows(out, os, &jj, w, h),
        (2, 1) | (2, 3) => avg_rows_to(out, os, &bb, &jj, w, h),
        (1, 2) | (3, 2) => avg_rows_to(out, os, &hh, &jj, w, h),
        (1, 0) | (3, 0) | (0, 1) | (0, 3) => {
            // Averaged with an integer sample one row or column over.
            let (dx, dy) = match (fx, fy) {
                (3, 0) => (1, 0),
                (0, 3) => (0, 1),
                _ => (0, 0),
            };
            let half: &[u8] = if fy == 0 { &bb } else { &hh };
            for row in 0..h {
                let src = base + (row + dy) * stride + dx;
                let s = &r.data[src..src + w];
                let hl = &half[row * w..row * w + w];
                let dst = &mut out[row * os..row * os + w];
                for col in 0..w {
                    dst[col] = avg(s[col], hl[col]);
                }
            }
        }
        // Diagonal quarter positions: the two nearest half samples.
        _ => avg_rows_to(out, os, &bb, &hh, w, h),
    }
}

/// Equation 8-270 over a block of statically known width.
///
/// The width is a const parameter because a chroma block is 2, 4 or 8 samples
/// wide and the 2x2 case dominates real streams: with `W` known the row slices
/// have constant length, which drops the bounds checks and lets the smallest
/// blocks compile to a handful of scalar multiplies instead of a vector setup
/// whose lanes are then thrown away.
#[inline]
fn chroma_rows<const W: usize>(
    data: &[u8],
    base: usize,
    stride: usize,
    h: usize,
    wt: [u16; 4],
    os: usize,
    out: &mut [u8],
) {
    for row in 0..h {
        let src = base + row * stride;
        let p = &data[src..src + W + 1];
        let q = &data[src + stride..src + stride + W + 1];
        let dst = &mut out[row * os..row * os + W];
        for c in 0..W {
            let v = u16::from(p[c]) * wt[0]
                + u16::from(p[c + 1]) * wt[1]
                + u16::from(q[c]) * wt[2]
                + u16::from(q[c + 1]) * wt[3]
                + 32;
            dst[c] = (v >> 6) as u8;
        }
    }
}

/// Chroma eighth-sample interpolation (Equation 8-270) of a `w` x `h` block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mc_chroma(
    r: &RefPlane<'_>,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    os: usize,
    out: &mut [u8],
) {
    let (fx, fy) = ((mv[0] & 7) as u16, (mv[1] & 7) as u16);
    let xi = clamp_origin(x + (mv[0] as i32 >> 3), w, r.width, r.pad, 0, 1);
    let yi = clamp_origin(y + (mv[1] as i32 >> 3), h, r.height, r.pad, 0, 1);
    let stride = r.stride;
    let base = r.at(xi, yi);

    // Full sample: a straight copy. A zero motion vector is the most common
    // vector in real content, and it lands here.
    if fx == 0 && fy == 0 {
        for row in 0..h {
            let src = base + row * stride;
            out[row * os..row * os + w].copy_from_slice(&r.data[src..src + w]);
        }
        return;
    }

    // The four weights sum to 64, so the weighted sum of Equation 8-270 never
    // exceeds 64 * 255 and the whole bilinear fits in 16-bit lanes.
    let wt = [(8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy];
    match w {
        2 => chroma_rows::<2>(r.data, base, stride, h, wt, os, out),
        4 => chroma_rows::<4>(r.data, base, stride, h, wt, os, out),
        _ => chroma_rows::<8>(r.data, base, stride, h, wt, os, out),
    }
}

/// Weights of one component for one partition (spec 8.4.3), already resolved
/// to the explicit/implicit/default case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Weights {
    /// `logWDC`.
    pub log_wd: i32,
    /// `w0C`, `w1C`.
    pub w: [i32; 2],
    /// `o0C`, `o1C`.
    pub o: [i32; 2],
}

impl Weights {
    /// The identity weighting, which reproduces 8.4.2.3.1 exactly.
    pub(crate) const DEFAULT: Weights = Weights {
        log_wd: 0,
        w: [1, 1],
        o: [0, 0],
    };

    /// True when 8.4.2.3.1 applies, so a single-list prediction is its own
    /// final prediction and needs no combining pass at all.
    pub(crate) fn is_default(&self) -> bool {
        *self == Weights::DEFAULT
    }
}

/// Combine the list predictions of one partition into the final prediction
/// (spec 8.4.2.3). `p0`/`p1` hold `n` samples each; the entry for a list that
/// is not used is ignored.
///
/// The result is contiguous rather than written at the picture's pitch: the
/// bi-predictive average is the one case where the samples of several rows are
/// adjacent, and vectors that span the row boundary measured 6 to 9 per cent
/// fewer instructions on 1080p than a per-row average at picture pitch.
pub(crate) fn combine(
    out: &mut [u8],
    p0: &[u8],
    p1: &[u8],
    use0: bool,
    use1: bool,
    wt: &Weights,
    n: usize,
) {
    if wt.is_default() {
        // 8.4.2.3.1.
        match (use0, use1) {
            (true, false) => out[..n].copy_from_slice(&p0[..n]),
            (false, true) => out[..n].copy_from_slice(&p1[..n]),
            _ => avg_rows(out, p0, p1, n),
        }
        return;
    }
    // 8.4.2.3.2.
    if use0 && use1 {
        let round = 1 << wt.log_wd;
        let off = (wt.o[0] + wt.o[1] + 1) >> 1;
        for i in 0..n {
            let v = (i32::from(p0[i]) * wt.w[0] + i32::from(p1[i]) * wt.w[1] + round)
                >> (wt.log_wd + 1);
            out[i] = clip8(v + off);
        }
        return;
    }
    let (src, k) = if use0 { (p0, 0) } else { (p1, 1) };
    if wt.log_wd >= 1 {
        let round = 1 << (wt.log_wd - 1);
        for i in 0..n {
            out[i] = clip8(((i32::from(src[i]) * wt.w[k] + round) >> wt.log_wd) + wt.o[k]);
        }
    } else {
        for i in 0..n {
            out[i] = clip8(i32::from(src[i]) * wt.w[k] + wt.o[k]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plane whose interior is a known ramp and whose borders are replicated,
    /// the way `Plane8::extend_borders` leaves them.
    fn ramp_plane(w: usize, h: usize, pad: usize) -> (Vec<u8>, usize, usize) {
        let stride = w + 2 * pad;
        let origin = pad * stride + pad;
        let mut data = vec![0u8; stride * (h + 2 * pad)];
        for y in 0..h {
            for x in 0..w {
                data[origin + y * stride + x] = ((x * 3 + y * 7) % 251) as u8;
            }
        }
        for y in 0..h {
            let row = origin + y * stride;
            let (l, r) = (data[row], data[row + w - 1]);
            data[row - pad..row].fill(l);
            data[row + w..row + w + pad].fill(r);
        }
        for y in 0..pad {
            let top = origin - (y + 1) * stride - pad;
            let src = origin - pad;
            let (a, b) = data.split_at_mut(src);
            a[top..top + stride].copy_from_slice(&b[..stride]);
            let bot = origin + (h + y) * stride - pad;
            let src = origin + (h - 1) * stride - pad;
            let (a, b) = data.split_at_mut(bot);
            b[..stride].copy_from_slice(&a[src..src + stride]);
        }
        (data, stride, origin)
    }

    fn plane(
        data: &[u8],
        stride: usize,
        origin: usize,
        w: usize,
        h: usize,
        pad: usize,
    ) -> RefPlane<'_> {
        RefPlane {
            data,
            stride,
            origin,
            width: w,
            height: h,
            pad,
        }
    }

    /// The spec's clipped, per-sample definition of the same interpolation,
    /// written straight from Equations 8-239 to 8-261 with no padding tricks.
    #[allow(clippy::too_many_arguments)]
    fn spec_luma(
        pic: &[u8],
        stride: usize,
        origin: usize,
        w: usize,
        h: usize,
        x: i32,
        y: i32,
        mv: [i16; 2],
        pw: usize,
        ph: usize,
        out: &mut [u8],
    ) {
        let s = |px: i32, py: i32| -> i32 {
            let cx = px.clamp(0, w as i32 - 1) as usize;
            let cy = py.clamp(0, h as i32 - 1) as usize;
            i32::from(pic[origin + cy * stride + cx])
        };
        let hf = |px: i32, py: i32| {
            s(px - 2, py) - 5 * s(px - 1, py) + 20 * s(px, py) + 20 * s(px + 1, py)
                - 5 * s(px + 2, py)
                + s(px + 3, py)
        };
        let vf = |px: i32, py: i32| {
            s(px, py - 2) - 5 * s(px, py - 1) + 20 * s(px, py) + 20 * s(px, py + 1)
                - 5 * s(px, py + 2)
                + s(px, py + 3)
        };
        let j1 = |px: i32, py: i32| {
            let t = |k: i32| hf(px, py + k);
            t(-2) - 5 * t(-1) + 20 * t(0) + 20 * t(1) - 5 * t(2) + t(3)
        };
        let (fx, fy) = ((mv[0] & 3) as i32, (mv[1] & 3) as i32);
        let (xi, yi) = (x + (mv[0] as i32 >> 2), y + (mv[1] as i32 >> 2));
        for row in 0..ph {
            for col in 0..pw {
                let (px, py) = (xi + col as i32, yi + row as i32);
                let g = |dx: i32, dy: i32| s(px + dx, py + dy) as u8;
                let b = |dy: i32| clip8((hf(px, py + dy) + 16) >> 5);
                let hs = |dx: i32| clip8((vf(px + dx, py) + 16) >> 5);
                let j = clip8((j1(px, py) + 512) >> 10);
                out[row * pw + col] = match (fx, fy) {
                    (0, 0) => g(0, 0),
                    (1, 0) => avg(g(0, 0), b(0)),
                    (2, 0) => b(0),
                    (3, 0) => avg(g(1, 0), b(0)),
                    (0, 1) => avg(g(0, 0), hs(0)),
                    (0, 2) => hs(0),
                    (0, 3) => avg(g(0, 1), hs(0)),
                    (1, 1) => avg(b(0), hs(0)),
                    (3, 1) => avg(b(0), hs(1)),
                    (1, 3) => avg(hs(0), b(1)),
                    (3, 3) => avg(hs(1), b(1)),
                    (2, 1) => avg(b(0), j),
                    (2, 3) => avg(b(1), j),
                    (1, 2) => avg(hs(0), j),
                    (3, 2) => avg(hs(1), j),
                    _ => j,
                };
            }
        }
    }

    /// Every fractional position, at interior positions and far outside the
    /// picture, agrees with the spec's clipped definition.
    #[test]
    fn luma_interpolation_matches_the_clipped_definition() {
        let (w, h, pad) = (48usize, 32usize, 32usize);
        let (data, stride, origin) = ramp_plane(w, h, pad);
        let r = plane(&data, stride, origin, w, h, pad);
        let mut ours = [0u8; 256];
        let mut theirs = [0u8; 256];
        for &(px, py) in &[
            (8i32, 8i32),
            (0, 0),
            (32, 16),
            (-40, -40),
            (60, 44),
            (44, 4),
        ] {
            for fy in 0..4i16 {
                for fx in 0..4i16 {
                    for &(pw, ph) in &[(16usize, 16usize), (8, 4), (4, 8), (4, 4)] {
                        let mv = [fx - 20 * 4, fy + 12];
                        mc_luma(&r, px, py, mv, pw, ph, pw, &mut ours);
                        spec_luma(&data, stride, origin, w, h, px, py, mv, pw, ph, &mut theirs);
                        assert_eq!(
                            ours[..pw * ph],
                            theirs[..pw * ph],
                            "({px},{py}) frac ({fx},{fy}) {pw}x{ph}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chroma_interpolation_matches_equation_8_270() {
        let (w, h, pad) = (24usize, 16usize, 16usize);
        let (data, stride, origin) = ramp_plane(w, h, pad);
        let r = plane(&data, stride, origin, w, h, pad);
        let mut ours = [0u8; 64];
        for &(px, py) in &[(4i32, 4i32), (0, 0), (-30, -30), (30, 22)] {
            for fy in 0..8i16 {
                for fx in 0..8i16 {
                    let mv = [fx, fy];
                    mc_chroma(&r, px, py, mv, 8, 8, 8, &mut ours);
                    for row in 0..8i32 {
                        for col in 0..8i32 {
                            let s = |dx: i32, dy: i32| -> i32 {
                                let cx = (px + col + dx).clamp(0, w as i32 - 1) as usize;
                                let cy = (py + row + dy).clamp(0, h as i32 - 1) as usize;
                                i32::from(data[origin + cy * stride + cx])
                            };
                            let (fx, fy) = (i32::from(fx), i32::from(fy));
                            let want = ((8 - fx) * (8 - fy) * s(0, 0)
                                + fx * (8 - fy) * s(1, 0)
                                + (8 - fx) * fy * s(0, 1)
                                + fx * fy * s(1, 1)
                                + 32)
                                >> 6;
                            assert_eq!(
                                i32::from(ours[(row * 8 + col) as usize]),
                                want,
                                "({px},{py}) frac ({fx},{fy}) at ({col},{row})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn weighted_combination_follows_8_4_2_3() {
        let p0 = [100u8; 4];
        let p1 = [200u8; 4];
        let mut out = [0u8; 4];
        combine(&mut out, &p0, &p1, true, true, &Weights::DEFAULT, 4);
        assert_eq!(out, [150; 4]);
        // Explicit, single list: ((100 * 2 + 2) >> 2) + 5 = 55.
        let w = Weights {
            log_wd: 2,
            w: [2, 3],
            o: [5, -5],
        };
        combine(&mut out, &p0, &p1, true, false, &w, 4);
        assert_eq!(out, [55; 4]);
        // Explicit, both lists: ((100*2 + 200*3 + 4) >> 3) + ((5 - 5 + 1) >> 1).
        combine(&mut out, &p0, &p1, true, true, &w, 4);
        assert_eq!(out, [100; 4]);
        // logWD 0 skips the rounding term entirely.
        let w0 = Weights {
            log_wd: 0,
            w: [2, 1],
            o: [3, 0],
        };
        combine(&mut out, &p0, &p1, true, false, &w0, 4);
        assert_eq!(out, [203; 4]);
    }
}
