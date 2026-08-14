//! In-loop deblocking filter (spec 8.7), intra-picture scope: every
//! macroblock is intra, so boundary strength is 4 on macroblock edges and 3
//! inside (8.7.2.1) — no motion-vector or reference comparisons yet.

use crate::tables::{ALPHA, BETA, CHROMA_QP, TC0};

/// Filter parameters of one edge segment.
#[derive(Debug, Clone, Copy)]
pub struct EdgeParams {
    /// indexA-resolved alpha threshold.
    pub alpha: i32,
    /// indexB-resolved beta threshold.
    pub beta: i32,
    /// tC0 for bS < 4 (unused for bS 4).
    pub tc0: i32,
    /// Boundary strength (3 or 4 here).
    pub bs: u8,
}

/// Resolve thresholds (spec 8.7.2.2) for a luma or chroma edge.
/// `qp_avg` = (qPp + qPq + 1) >> 1, offsets are the slice's FilterOffsetA/B.
#[inline]
pub fn edge_params(qp_avg: i32, offset_a: i32, offset_b: i32, bs: u8) -> EdgeParams {
    let index_a = (qp_avg + offset_a).clamp(0, 51) as usize;
    let index_b = (qp_avg + offset_b).clamp(0, 51) as usize;
    EdgeParams {
        alpha: i32::from(ALPHA[index_a]),
        beta: i32::from(BETA[index_b]),
        tc0: i32::from(TC0[(bs - 1).min(2) as usize][index_a]),
        bs,
    }
}

/// Map a luma QP to the chroma QP for deblocking (spec 8.7.2 via 8.5.8).
#[inline]
pub fn chroma_qp(qp_luma: i32, offset: i32) -> i32 {
    i32::from(CHROMA_QP[(qp_luma + offset).clamp(0, 51) as usize])
}

/// Filter one line of samples across a luma edge (spec 8.7.2.3 / 8.7.2.4).
///
/// `p` indexes the plane; `s` is the distance between consecutive samples
/// across the edge (1 for vertical edges, stride for horizontal), and `edge`
/// is the index of q0. Samples p3..q3 must be in bounds.
#[inline]
pub fn filter_luma_line(data: &mut [u8], edge: usize, s: usize, e: &EdgeParams) {
    let q0 = i32::from(data[edge]);
    let p0 = i32::from(data[edge - s]);
    if (p0 - q0).abs() >= e.alpha {
        return;
    }
    let p1 = i32::from(data[edge - 2 * s]);
    let q1 = i32::from(data[edge + s]);
    if (p1 - p0).abs() >= e.beta || (q1 - q0).abs() >= e.beta {
        return;
    }
    let p2 = i32::from(data[edge - 3 * s]);
    let q2 = i32::from(data[edge + 2 * s]);
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if e.bs < 4 {
        let tc = e.tc0 + i32::from(ap < e.beta) + i32::from(aq < e.beta);
        let delta = (((q0 - p0) * 4 + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        data[edge - s] = (p0 + delta).clamp(0, 255) as u8;
        data[edge] = (q0 - delta).clamp(0, 255) as u8;
        if ap < e.beta {
            let d = ((p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1).clamp(-e.tc0, e.tc0);
            data[edge - 2 * s] = (p1 + d) as u8;
        }
        if aq < e.beta {
            let d = ((q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1).clamp(-e.tc0, e.tc0);
            data[edge + s] = (q1 + d) as u8;
        }
    } else {
        // bS == 4 (8.7.2.4).
        let strong = (p0 - q0).abs() < (e.alpha >> 2) + 2;
        if strong && ap < e.beta {
            let p3 = i32::from(data[edge - 4 * s]);
            data[edge - s] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u8;
            data[edge - 2 * s] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u8;
            data[edge - 3 * s] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u8;
        } else {
            data[edge - s] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        }
        if strong && aq < e.beta {
            let q3 = i32::from(data[edge + 3 * s]);
            data[edge] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u8;
            data[edge + s] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u8;
            data[edge + 2 * s] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u8;
        } else {
            data[edge] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
        }
    }
}

/// Filter one line across a chroma edge (chromaStyleFilteringFlag = 1).
#[inline]
pub fn filter_chroma_line(data: &mut [u8], edge: usize, s: usize, e: &EdgeParams) {
    let q0 = i32::from(data[edge]);
    let p0 = i32::from(data[edge - s]);
    if (p0 - q0).abs() >= e.alpha {
        return;
    }
    let p1 = i32::from(data[edge - 2 * s]);
    let q1 = i32::from(data[edge + s]);
    if (p1 - p0).abs() >= e.beta || (q1 - q0).abs() >= e.beta {
        return;
    }
    if e.bs < 4 {
        let tc = e.tc0 + 1;
        let delta = (((q0 - p0) * 4 + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        data[edge - s] = (p0 + delta).clamp(0, 255) as u8;
        data[edge] = (q0 - delta).clamp(0, 255) as u8;
    } else {
        data[edge - s] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        data[edge] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
    }
}

/// Vectorised luma edge filter core (spec 8.7.2.3 / 8.7.2.4), 16 lanes.
///
/// Takes the eight sample vectors across the edge, returns the six filtered
/// vectors `[p2', p1', p0', q0', q1', q2']`, or `None` when no lane passes
/// the filterSamplesFlag gate. Bit-identical to [`filter_luma_line`] per
/// lane.
#[allow(clippy::too_many_arguments)]
fn luma_edge_core16(
    p3: wide::i16x16,
    p2: wide::i16x16,
    p1: wide::i16x16,
    p0: wide::i16x16,
    q0: wide::i16x16,
    q1: wide::i16x16,
    q2: wide::i16x16,
    q3: wide::i16x16,
    e: &EdgeParams,
) -> Option<[wide::i16x16; 6]> {
    use wide::i16x16;
    let alpha = i16x16::splat(e.alpha as i16);
    let beta = i16x16::splat(e.beta as i16);
    let filter = (p0 - q0).abs().simd_lt(alpha)
        & (p1 - p0).abs().simd_lt(beta)
        & (q1 - q0).abs().simd_lt(beta);
    if !filter.any() {
        return None;
    }
    let ap = (p2 - p0).abs().simd_lt(beta);
    let aq = (q2 - q0).abs().simd_lt(beta);
    let one = i16x16::ONE;

    Some(if e.bs < 4 {
        let tc0 = i16x16::splat(e.tc0 as i16);
        let tc = tc0 + ap.select(one, i16x16::ZERO) + aq.select(one, i16x16::ZERO);
        let four = i16x16::splat(4);
        let delta = ((((q0 - p0) << 2u32) + (p1 - q1) + four) >> 3u32)
            .max(-tc)
            .min(tc);
        let avg = (p0 + q0 + one) >> 1u32;
        let dp1 = ((p2 + avg - (p1 << 1u32)) >> 1u32).max(-tc0).min(tc0);
        let dq1 = ((q2 + avg - (q1 << 1u32)) >> 1u32).max(-tc0).min(tc0);
        [
            p2,
            (filter & ap).select(p1 + dp1, p1),
            filter.select(p0 + delta, p0),
            filter.select(q0 - delta, q0),
            (filter & aq).select(q1 + dq1, q1),
            q2,
        ]
    } else {
        let two = i16x16::splat(2);
        let four = i16x16::splat(4);
        let strong = (p0 - q0)
            .abs()
            .simd_lt(i16x16::splat(((e.alpha >> 2) + 2) as i16));
        let sp = filter & strong & ap;
        let sq = filter & strong & aq;
        let p0s = (p2 + (p1 << 1u32) + (p0 << 1u32) + (q0 << 1u32) + q1 + four) >> 3u32;
        let p1s = (p2 + p1 + p0 + q0 + two) >> 2u32;
        let p2s = ((p3 << 1u32) + p2 + (p2 << 1u32) + p1 + p0 + q0 + four) >> 3u32;
        let p0w = ((p1 << 1u32) + p0 + q1 + two) >> 2u32;
        let q0s = (p1 + (p0 << 1u32) + (q0 << 1u32) + (q1 << 1u32) + q2 + four) >> 3u32;
        let q1s = (p0 + q0 + q1 + q2 + two) >> 2u32;
        let q2s = ((q3 << 1u32) + q2 + (q2 << 1u32) + q1 + q0 + p0 + four) >> 3u32;
        let q0w = ((q1 << 1u32) + q0 + p1 + two) >> 2u32;
        [
            sp.select(p2s, p2),
            sp.select(p1s, p1),
            sp.select(p0s, filter.select(p0w, p0)),
            sq.select(q0s, filter.select(q0w, q0)),
            sq.select(q1s, q1),
            sq.select(q2s, q2),
        ]
    })
}

#[inline]
fn load_row16(data: &[u8], at: usize) -> wide::i16x16 {
    let mut a = [0i16; 16];
    for (o, &b) in a.iter_mut().zip(&data[at..at + 16]) {
        *o = i16::from(b);
    }
    wide::i16x16::from(a)
}

#[inline]
fn store_row16(data: &mut [u8], at: usize, v: wide::i16x16) {
    let clipped = v.max(wide::i16x16::ZERO).min(wide::i16x16::splat(255));
    for (o, &s) in data[at..at + 16].iter_mut().zip(clipped.as_array().iter()) {
        *o = s as u8;
    }
}

/// Filter a whole horizontal luma edge (16 samples wide): the eight rows
/// around the edge are contiguous, so the filter runs 16 lanes at once with
/// masked selects instead of per-pixel branches. `q0_row` indexes the first
/// sample of the q0 row.
pub fn filter_luma_h_edge16(data: &mut [u8], q0_row: usize, stride: usize, e: &EdgeParams) {
    let p3 = load_row16(data, q0_row - 4 * stride);
    let p2 = load_row16(data, q0_row - 3 * stride);
    let p1 = load_row16(data, q0_row - 2 * stride);
    let p0 = load_row16(data, q0_row - stride);
    let q0 = load_row16(data, q0_row);
    let q1 = load_row16(data, q0_row + stride);
    let q2 = load_row16(data, q0_row + 2 * stride);
    let q3 = load_row16(data, q0_row + 3 * stride);
    let Some(out) = luma_edge_core16(p3, p2, p1, p0, q0, q1, q2, q3, e) else {
        return;
    };
    store_row16(data, q0_row - 3 * stride, out[0]);
    store_row16(data, q0_row - 2 * stride, out[1]);
    store_row16(data, q0_row - stride, out[2]);
    store_row16(data, q0_row, out[3]);
    store_row16(data, q0_row + stride, out[4]);
    store_row16(data, q0_row + 2 * stride, out[5]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_clamp_and_lookup() {
        let e = edge_params(26, 0, 0, 3);
        assert_eq!(e.alpha, i32::from(ALPHA[26]));
        assert_eq!(e.beta, i32::from(BETA[26]));
        assert_eq!(e.tc0, i32::from(TC0[2][26]));
        // Offsets push the index outside [0, 51] and clamp.
        let e2 = edge_params(51, 12, 12, 4);
        assert_eq!(e2.alpha, 255);
        let e3 = edge_params(0, -12, -12, 3);
        assert_eq!(e3.alpha, 0);
        assert_eq!(chroma_qp(51, 0), 39);
        assert_eq!(chroma_qp(20, 2), 22);
    }

    /// A hard step across a bS=4 edge at moderate QP gets smoothed; a flat
    /// area is untouched.
    #[test]
    fn strong_filter_smooths_step() {
        // Line: p3 p2 p1 p0 | q0 q1 q2 q3.
        let mut line = [60u8, 60, 60, 60, 80, 80, 80, 80];
        let e = edge_params(32, 0, 0, 4);
        filter_luma_line(&mut line, 4, 1, &e);
        // (p0, q0) move toward each other.
        assert!(line[3] > 60 && line[4] < 80, "{line:?}");
        let mut flat = [90u8; 8];
        filter_luma_line(&mut flat, 4, 1, &e);
        assert_eq!(flat, [90; 8]);
    }

    /// Above alpha the edge is treated as a real feature and left alone.
    #[test]
    fn large_step_is_preserved() {
        let mut line = [10u8, 10, 10, 10, 200, 200, 200, 200];
        let e = edge_params(26, 0, 0, 4);
        filter_luma_line(&mut line, 4, 1, &e);
        assert_eq!(line, [10, 10, 10, 10, 200, 200, 200, 200]);
    }

    #[test]
    fn chroma_filter_touches_only_p0_q0() {
        let mut line = [60u8, 60, 60, 60, 74, 74, 74, 74];
        let e = edge_params(32, 0, 0, 3);
        filter_chroma_line(&mut line, 4, 1, &e);
        assert_eq!(line[2], 60);
        assert_eq!(line[5], 74);
        assert!(line[3] >= 60 && line[4] <= 74);
        assert!(line[3] != 60 || line[4] != 74);
    }
}
