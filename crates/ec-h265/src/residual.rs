//! `residual_coding()` (7.3.8.11) and the scan orders it walks (6.5.3 - 6.5.5).

use crate::cabac::{CabacEncoder, ctx};
use std::sync::LazyLock;

/// Scan orders: `SCANS[log2BlockSize][scanIdx][pos] = (x, y)`.
///
/// Built once rather than typed in — the diagonal scan is four lines of the
/// spec's own pseudo-code, and generating it cannot go wrong the way a
/// transcribed 64-entry table can.
/// One scan order: the `(x, y)` of every position, in scan sequence.
type Scan = Vec<(u8, u8)>;

/// Scans for block sizes 4..32 and the three scan indices.
type ScanTable = [[Scan; 3]; 4];

static SCANS: LazyLock<ScanTable> = LazyLock::new(|| {
    std::array::from_fn(|log2_size| {
        let size = 1usize << log2_size;
        std::array::from_fn(|scan_idx| match scan_idx {
            1 => (0..size * size)
                .map(|i| ((i % size) as u8, (i / size) as u8))
                .collect(),
            2 => (0..size * size)
                .map(|i| ((i / size) as u8, (i % size) as u8))
                .collect(),
            _ => {
                // 6.5.3, up-right diagonal.
                let mut out = Vec::with_capacity(size * size);
                let (mut x, mut y) = (0usize, 0usize);
                loop {
                    loop {
                        if x < size && y < size {
                            out.push((x as u8, y as u8));
                        }
                        if y == 0 {
                            break;
                        }
                        y -= 1;
                        x += 1;
                    }
                    y = x + 1;
                    x = 0;
                    if out.len() >= size * size {
                        break;
                    }
                }
                out
            }
        })
    })
});

/// `POSITIONS[log2 - 2][scanIdx][y * n + x]` = that coefficient's place in the
/// full scan of the block, sub-block major.
///
/// One table turns "where does the scan end" from a walk over every sub-block
/// into a single pass over the levels, which matters because the residual coder
/// runs once per rate-distortion trial.
static POSITIONS: LazyLock<[[Vec<u16>; 3]; 4]> = LazyLock::new(|| {
    std::array::from_fn(|size_idx| {
        let log2 = size_idx as u32 + 2;
        let n = 1usize << log2;
        std::array::from_fn(|scan_idx| {
            let sub_scan = &SCANS[size_idx][scan_idx];
            let pos_scan = &SCANS[2][scan_idx];
            let mut table = vec![0u16; n * n];
            for (i, &(xs, ys)) in sub_scan.iter().enumerate() {
                for (p, &(xp, yp)) in pos_scan.iter().enumerate() {
                    let xc = xs as usize * 4 + xp as usize;
                    let yc = ys as usize * 4 + yp as usize;
                    table[yc * n + xc] = (i * 16 + p) as u16;
                }
            }
            table
        })
    })
});

/// `ctxIdxMap[i]` for 4x4 blocks (Table 9-50); index 15 is never queried
/// because that position is always the last significant one.
const CTX_IDX_MAP: [usize; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

/// The scan order for one block size and scan index.
pub fn scan(log2_size: u32, scan_idx: usize) -> &'static [(u8, u8)] {
    &SCANS[log2_size as usize][scan_idx]
}

/// Truncated-Rice + EGk binarization of `coeff_abs_level_remaining` (9.3.3.11).
fn encode_remaining(enc: &mut CabacEncoder, value: u32, rice: u32) {
    let c_max = 4 << rice;
    if value < c_max {
        let prefix = value >> rice;
        for _ in 0..prefix {
            enc.encode_bypass(1);
        }
        enc.encode_bypass(0);
        if rice > 0 {
            enc.encode_bypass_bits(value & ((1 << rice) - 1), rice);
        }
    } else {
        for _ in 0..4 {
            enc.encode_bypass(1);
        }
        let mut rest = value - c_max;
        let mut k = rice + 1;
        loop {
            if rest >= (1 << k) {
                enc.encode_bypass(1);
                rest -= 1 << k;
                k += 1;
            } else {
                enc.encode_bypass(0);
                enc.encode_bypass_bits(rest, k);
                break;
            }
        }
    }
}

/// `last_sig_coeff_*_prefix` for a coordinate, and its suffix bit count.
fn last_prefix(coord: u32) -> (u32, u32) {
    if coord < 4 {
        (coord, 0)
    } else {
        let log2 = 31 - coord.leading_zeros();
        let prefix = 2 * log2 + ((coord >> (log2 - 1)) & 1);
        (prefix, (prefix >> 1) - 1)
    }
}

/// Encode one transform block's levels.
///
/// `levels` is row-major `n x n` with `levels[y * n + x]`, `c_idx` is 0 for luma
/// and 1/2 for chroma, and at least one level must be non-zero (the caller codes
/// a `cbf` of zero instead).
pub fn encode_residual(
    enc: &mut CabacEncoder,
    levels: &[i32],
    log2_size: u32,
    c_idx: usize,
    scan_idx: usize,
) {
    let n = 1usize << log2_size;
    let sub_wide = n >> 2;
    let sub_scan = scan(log2_size - 2, scan_idx);
    let pos_scan = scan(2, scan_idx);
    let chroma = c_idx > 0;

    // One pass over the levels finds both the end of the scan and which
    // sub-blocks hold anything.
    let positions = &POSITIONS[(log2_size - 2) as usize][scan_idx];
    let mut csbf = [false; 64];
    let mut last_full = 0i32;
    for yc in 0..n {
        let row = &levels[yc * n..yc * n + n];
        for (xc, &level) in row.iter().enumerate() {
            if level != 0 {
                csbf[(yc >> 2) * sub_wide + (xc >> 2)] = true;
                last_full = last_full.max(i32::from(positions[yc * n + xc]));
            }
        }
    }
    let last_sub = (last_full / 16) as usize;
    let last_pos = (last_full % 16) as usize;
    let (last_xs, last_ys) = sub_scan[last_sub];
    let (last_xp, last_yp) = pos_scan[last_pos];
    let mut last_x = (last_xs as u32) * 4 + last_xp as u32;
    let mut last_y = (last_ys as u32) * 4 + last_yp as u32;
    if scan_idx == 2 {
        std::mem::swap(&mut last_x, &mut last_y);
    }

    // last_sig_coeff_x/y prefix and suffix (9.3.4.2.3 for the contexts).
    let (offset, shift) = if c_idx == 0 {
        (
            3 * (log2_size - 2) + ((log2_size - 1) >> 2),
            (log2_size + 1) >> 2,
        )
    } else {
        (15, log2_size - 2)
    };
    let c_max = (log2_size << 1) - 1;
    let (x_prefix, x_suffix_bits) = last_prefix(last_x);
    let (y_prefix, y_suffix_bits) = last_prefix(last_y);
    for (prefix, base) in [(x_prefix, ctx::LAST_X), (y_prefix, ctx::LAST_Y)] {
        for bin in 0..prefix {
            enc.encode_bin(base + ((bin >> shift) + offset) as usize, 1);
        }
        if prefix < c_max {
            enc.encode_bin(base + ((prefix >> shift) + offset) as usize, 0);
        }
    }
    if x_suffix_bits > 0 {
        let base = (1 << x_suffix_bits) * (2 + (x_prefix & 1));
        enc.encode_bypass_bits(last_x - base, x_suffix_bits);
    }
    if y_suffix_bits > 0 {
        let base = (1 << y_suffix_bits) * (2 + (y_prefix & 1));
        enc.encode_bypass_bits(last_y - base, y_suffix_bits);
    }

    // The DC sub-block is always coded (7.4.9.11 inference).
    let (dc_xs, dc_ys) = sub_scan[0];
    csbf[dc_ys as usize * sub_wide + dc_xs as usize] = true;

    let mut greater1_ctx = 1u32;
    for i in (0..=last_sub).rev() {
        let (xs, ys) = sub_scan[i];
        let (xs, ys) = (xs as usize, ys as usize);
        let coded = csbf[ys * sub_wide + xs];
        let mut infer_dc_sig = false;
        if i < last_sub && i > 0 {
            let mut csbf_ctx = 0;
            if xs < sub_wide - 1 && csbf[ys * sub_wide + xs + 1] {
                csbf_ctx += 1;
            }
            if ys < sub_wide - 1 && csbf[(ys + 1) * sub_wide + xs] {
                csbf_ctx += 1;
            }
            let inc = if chroma {
                2 + csbf_ctx.min(1)
            } else {
                csbf_ctx.min(1)
            };
            enc.encode_bin(ctx::CODED_SUB_BLOCK + inc, u32::from(coded));
            infer_dc_sig = true;
        }
        if !coded {
            continue;
        }

        // sig_coeff_flag over the sub-block, high scan position first.
        let mut significant = [false; 16];
        if i == last_sub {
            significant[last_pos] = true;
        }
        let first = if i == last_sub {
            last_pos as i32 - 1
        } else {
            15
        };
        let mut scan_pos = first;
        while scan_pos >= 0 {
            let p = scan_pos as usize;
            scan_pos -= 1;
            let (xp, yp) = pos_scan[p];
            let xc = xs * 4 + xp as usize;
            let yc = ys * 4 + yp as usize;
            let sig = levels[yc * n + xc] != 0;
            if p > 0 || !infer_dc_sig {
                let inc = sig_ctx_inc(xc, yc, xs, ys, sub_wide, &csbf, log2_size, c_idx, scan_idx);
                enc.encode_bin(ctx::SIG_COEFF + inc, u32::from(sig));
            }
            significant[p] = sig;
            if sig {
                infer_dc_sig = false;
            }
        }
        if infer_dc_sig {
            // The flag was not coded because it is inferred to be one.
            significant[0] = true;
        }

        let mut order = [0usize; 16];
        let mut order_len = 0;
        for p in (0..16).rev() {
            if significant[p] {
                order[order_len] = p;
                order_len += 1;
            }
        }
        let order = &order[..order_len];
        if order.is_empty() {
            continue;
        }

        // coeff_abs_level_greater1_flag, first eight only (9.3.4.2.6).
        let mut ctx_set = if i == 0 || chroma { 0 } else { 2 };
        if greater1_ctx == 0 {
            ctx_set += 1;
        }
        greater1_ctx = 1;
        let mut last_greater1 = None;
        let mut greater1 = [false; 16];
        for &p in order.iter().take(8) {
            let (xp, yp) = pos_scan[p];
            let level = levels[(ys * 4 + yp as usize) * n + xs * 4 + xp as usize].unsigned_abs();
            let flag = level > 1;
            let inc = ctx_set * 4 + greater1_ctx.min(3) + if chroma { 16 } else { 0 };
            enc.encode_bin(ctx::GREATER1 + inc as usize, u32::from(flag));
            greater1[p] = flag;
            if flag {
                if last_greater1.is_none() {
                    last_greater1 = Some(p);
                }
                greater1_ctx = 0;
            } else if greater1_ctx > 0 {
                greater1_ctx += 1;
            }
        }

        // coeff_abs_level_greater2_flag for the first greater-than-one level.
        let mut greater2 = false;
        if let Some(p) = last_greater1 {
            let (xp, yp) = pos_scan[p];
            let level = levels[(ys * 4 + yp as usize) * n + xs * 4 + xp as usize].unsigned_abs();
            greater2 = level > 2;
            let inc = ctx_set + if chroma { 4 } else { 0 };
            enc.encode_bin(ctx::GREATER2 + inc as usize, u32::from(greater2));
        }

        // Signs, then the remaining magnitudes. Sign data hiding is off, so
        // every significant coefficient carries its own sign bit.
        for &p in order {
            let (xp, yp) = pos_scan[p];
            let level = levels[(ys * 4 + yp as usize) * n + xs * 4 + xp as usize];
            enc.encode_bypass(u32::from(level < 0));
        }
        let mut rice = 0u32;
        for (num_sig, &p) in order.iter().enumerate() {
            let (xp, yp) = pos_scan[p];
            let level = levels[(ys * 4 + yp as usize) * n + xs * 4 + xp as usize].unsigned_abs();
            let coded_greater1 = num_sig < 8;
            let base_level = 1
                + u32::from(coded_greater1 && greater1[p])
                + u32::from(last_greater1 == Some(p) && greater2);
            let threshold = if num_sig < 8 {
                if last_greater1 == Some(p) { 3 } else { 2 }
            } else {
                1
            };
            if base_level == threshold {
                let remaining = level - base_level;
                encode_remaining(enc, remaining, rice);
                rice = (rice + u32::from(level > (3 << rice))).min(4);
            }
        }
    }
}

/// `ctxInc` for `sig_coeff_flag` (9.3.4.2.5).
#[allow(clippy::too_many_arguments)]
fn sig_ctx_inc(
    xc: usize,
    yc: usize,
    xs: usize,
    ys: usize,
    sub_wide: usize,
    csbf: &[bool],
    log2_size: u32,
    c_idx: usize,
    scan_idx: usize,
) -> usize {
    let mut sig_ctx;
    if log2_size == 2 {
        sig_ctx = CTX_IDX_MAP[(yc << 2) + xc];
    } else if xc + yc == 0 {
        sig_ctx = 0;
    } else {
        let mut prev_csbf = 0usize;
        if xs < sub_wide - 1 && csbf[ys * sub_wide + xs + 1] {
            prev_csbf += 1;
        }
        if ys < sub_wide - 1 && csbf[(ys + 1) * sub_wide + xs] {
            prev_csbf += 2;
        }
        let (xp, yp) = (xc & 3, yc & 3);
        sig_ctx = match prev_csbf {
            0 => {
                if xp + yp == 0 {
                    2
                } else if xp + yp < 3 {
                    1
                } else {
                    0
                }
            }
            1 => {
                if yp == 0 {
                    2
                } else if yp == 1 {
                    1
                } else {
                    0
                }
            }
            2 => {
                if xp == 0 {
                    2
                } else if xp == 1 {
                    1
                } else {
                    0
                }
            }
            _ => 2,
        };
        if c_idx == 0 {
            if xs + ys > 0 {
                sig_ctx += 3;
            }
            if log2_size == 3 {
                sig_ctx += if scan_idx == 0 { 9 } else { 15 };
            } else {
                sig_ctx += 21;
            }
        } else if log2_size == 3 {
            sig_ctx += 9;
        } else {
            sig_ctx += 12;
        }
    }
    if c_idx == 0 { sig_ctx } else { 27 + sig_ctx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cabac::Contexts;

    #[test]
    fn diagonal_scan_matches_the_spec_walk() {
        // 4x4 up-right diagonal, from 6.5.3.
        let expect: Vec<(u8, u8)> = vec![
            (0, 0),
            (0, 1),
            (1, 0),
            (0, 2),
            (1, 1),
            (2, 0),
            (0, 3),
            (1, 2),
            (2, 1),
            (3, 0),
            (1, 3),
            (2, 2),
            (3, 1),
            (2, 3),
            (3, 2),
            (3, 3),
        ];
        assert_eq!(scan(2, 0), &expect[..]);
        // Horizontal and vertical are the plain raster orders.
        assert_eq!(scan(2, 1)[..4], [(0, 0), (1, 0), (2, 0), (3, 0)]);
        assert_eq!(scan(2, 2)[..4], [(0, 0), (0, 1), (0, 2), (0, 3)]);
        // Every size is a permutation of its block.
        for log2 in 0..4u32 {
            for idx in 0..3 {
                let s = scan(log2, idx);
                let size = 1usize << log2;
                assert_eq!(s.len(), size * size);
                let mut seen = vec![false; size * size];
                for &(x, y) in s {
                    let slot = y as usize * size + x as usize;
                    assert!(!seen[slot]);
                    seen[slot] = true;
                }
            }
        }
    }

    #[test]
    fn last_position_binarization_inverts_the_spec_formula() {
        for coord in 0..32u32 {
            let (prefix, suffix_bits) = last_prefix(coord);
            let decoded = if suffix_bits == 0 {
                prefix
            } else {
                let base = (1 << ((prefix >> 1) - 1)) * (2 + (prefix & 1));
                let suffix = coord - base;
                assert!(suffix < (1 << suffix_bits), "coord {coord}");
                base + suffix
            };
            assert_eq!(decoded, coord);
            assert!(prefix <= 9, "coord {coord} prefix {prefix}");
        }
    }

    #[test]
    fn residual_coding_costs_less_for_sparser_blocks() {
        // Not a bit-exactness check — that is the the oracle conformance test — but
        // the invariant that makes rate-distortion decisions meaningful: fewer
        // and smaller levels must cost fewer bits.
        let mut sparse = vec![0i32; 64];
        sparse[0] = 1;
        let mut dense = vec![0i32; 64];
        for (i, v) in dense.iter_mut().enumerate() {
            *v = ((i % 7) as i32) - 3;
        }
        let mut costs = Vec::new();
        for levels in [&sparse, &dense] {
            let mut enc = CabacEncoder::counter(Contexts::new(30));
            encode_residual(&mut enc, levels, 3, 0, 0);
            costs.push(enc.bit_count());
        }
        assert!(costs[0] < costs[1], "{costs:?}");
        assert!(costs[0] > 0);
    }

    #[test]
    fn every_block_size_and_scan_codes_without_panicking() {
        for log2 in 2..=5u32 {
            let n = 1usize << log2;
            for scan_idx in 0..3 {
                for c_idx in [0usize, 1] {
                    let mut levels = vec![0i32; n * n];
                    for (i, v) in levels.iter_mut().enumerate() {
                        *v = match i % 11 {
                            0 => 0,
                            1 => 1,
                            2 => -2,
                            3 => 3,
                            4 => -17,
                            5 => 400,
                            _ => 0,
                        };
                    }
                    let mut enc = CabacEncoder::counter(Contexts::new(27));
                    encode_residual(&mut enc, &levels, log2, c_idx, scan_idx);
                    assert!(enc.bit_count() > 0);
                }
            }
        }
    }
}
