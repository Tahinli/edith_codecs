//! `residual_coding()` (7.3.8.11) and the scan orders it walks (6.5.3 - 6.5.5).

use crate::cabac::{CabacEncoder, CabacState, ctx};
use crate::transform::{coeff_ssd_scale, dequant_level, ideal_level};
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

/// Make each sub-block's absolute levels carry the sign of its first
/// significant coefficient, so that sign need not be coded (7.3.8.11, the
/// `sign_data_hiding_enabled_flag` path). A sub-block qualifies when its first
/// and last significant coefficients are more than three scan positions apart;
/// where the parity is wrong, one level is nudged by one, the cheapest being
/// whichever coefficient the quantiser rounded furthest in that direction.
///
/// This runs on the levels before they are dequantised, so the encoder's own
/// reconstruction is built from what the decoder will read back.
pub fn hide_signs(coeffs: &[i32], levels: &mut [i32], n: usize, qp: i32, scan_idx: usize) -> usize {
    let log2_size = n.trailing_zeros();
    let sub_scan = scan(log2_size - 2, scan_idx);
    let pos_scan = scan(2, scan_idx);
    for &(xs, ys) in sub_scan {
        let (xs, ys) = (xs as usize, ys as usize);
        let at = |p: usize| {
            let (xp, yp) = pos_scan[p];
            (ys * 4 + yp as usize) * n + xs * 4 + xp as usize
        };
        let Some(first) = (0..16).find(|&p| levels[at(p)] != 0) else {
            continue;
        };
        let last = (0..16).rev().find(|&p| levels[at(p)] != 0).unwrap_or(first);
        if last - first <= 3 {
            continue;
        }
        let sum: i32 = (0..16).map(|p| levels[at(p)].abs()).sum();
        let negative = levels[at(first)] < 0;
        if (sum & 1 == 1) == negative {
            continue;
        }
        // The parity is wrong. Changing any level by one fixes it; take the
        // one whose rounding error the change reduces most (or grows least).
        // Only levels that stay non-zero are considered, so no coefficient
        // enters or leaves the block and the scan positions above still hold.
        let mut best: Option<(f64, usize, i32)> = None;
        for p in 0..16 {
            let i = at(p);
            let level = levels[i];
            if level == 0 {
                continue;
            }
            let magnitude = f64::from(level.abs());
            let ideal = ideal_level(coeffs[i], n, qp);
            // Distortion alone picks the coefficient: a rate bias favouring
            // decrements (or increments) was swept through the clip gate at
            // +-0.25 and +-0.5 units of squared level error per bit and lost
            // in both directions, so there is none.
            let mut consider = |cost: f64, delta: i32| {
                if best.is_none_or(|(b, _, _)| cost < b) {
                    best = Some((cost, i, delta));
                }
            };
            consider(1.0 - 2.0 * (ideal - magnitude), 1);
            if level.abs() > 1 {
                consider(1.0 + 2.0 * (ideal - magnitude), -1);
            }
        }
        if let Some((_, i, delta)) = best {
            levels[i] += if levels[i] < 0 { -delta } else { delta };
        }
    }
    levels[..n * n].iter().filter(|&&l| l != 0).count()
}

/// The largest level the rate-distortion search offers a smaller magnitude to.
///
/// A big level carries real signal and never wins by shrinking, so bounding
/// the search is what keeps it affordable: every trial re-codes the whole
/// block. Swept on 1080p film, BD-PSNR-YUV against x265: 1 gives +0.244, the 2
/// kept here +0.276, and 3 +0.277 for strictly more trials.
const RDOQ_MAX_LEVEL: i32 = 2;

/// How much of the mode-decision lambda the level search uses. The bits are
/// the coder's own count, not an estimate, so the scale starts at 1.
///
/// The two clips disagree above it and the disagreement is small: on
/// BD-PSNR-YUV against x265, 1080p film reads +0.276 at 1.0, +0.294 at 1.25
/// and +0.293 at 1.5, while a 2560x1440 screen capture reads -0.392, -0.408
/// and -0.441. The crossing sits just above 1.0, and the screen capture is the
/// content still behind x265, so it takes the tie.
const RDOQ_LAMBDA: f64 = 1.0;

/// How hard the significance map's per-position flag is priced in the
/// coefficient-statistics proxy below, in bits. Swept against
/// `bd_psnr_vs_x265` (synthetic clip + a real screen-capture clip) alongside
/// [`ESTIMATE_MAG_SCALE`]; see that constant's comment for the numbers.
const ESTIMATE_SIG_BIT: f64 = 0.15;

/// Scale on the per-level magnitude term of [`estimate_residual_bits`].
///
/// Swept BD-PSNR-YUV against x265 on the synthetic clip (real-bits baseline
/// +7.862 dB): 1.0 reads +7.790, 1.25 reads +7.817, 1.5 reads +7.868 — closest
/// to the baseline, and confirmed on a real 2560x1440 screen capture too
/// (real-bits baseline -0.317 dB, this scale -0.331, both other scales
/// untried there once the synthetic sweep picked a direction).
const ESTIMATE_MAG_SCALE: f64 = 1.5;

/// A cheap stand-in for [`encode_residual`]'s exact bit count, for ranking
/// intra-mode candidates before the real search runs once on the winner
/// (`choose_luma_mode`). No CABAC context is touched — the last position is
/// priced by the same closed-form binarization the real coder uses (a few
/// arithmetic ops, not a per-bin call), and every level's cost comes from a
/// log2 magnitude estimate instead of walking greater1/greater2/remaining
/// bins one context update at a time. This is a ranking proxy only: the
/// committed bitstream always goes through `encode_residual`.
pub fn estimate_residual_bits(levels: &[i32], log2_size: u32, scan_idx: usize) -> f64 {
    let n = 1usize << log2_size;
    let positions = &POSITIONS[(log2_size - 2) as usize][scan_idx];
    let mut last_full = 0i32;
    let mut mag_bits = 0.0f64;
    let mut any = false;
    for (i, &level) in levels[..n * n].iter().enumerate() {
        if level != 0 {
            any = true;
            last_full = last_full.max(i32::from(positions[i]));
            let v = level.unsigned_abs();
            // 1 (sign) + 2 for the first two magnitudes (mirroring the real
            // coder's greater1/greater2 flags), Exp-Golomb-ish beyond that.
            mag_bits += 1.0
                + if v <= 2 {
                    v as f64
                } else {
                    2.0 + 2.0 * ((v - 1) as f64).log2()
                };
        }
    }
    if !any {
        return 0.0;
    }
    let last_sub = (last_full / 16) as usize;
    let last_pos = (last_full % 16) as usize;
    let sub_scan = scan(log2_size - 2, scan_idx);
    let pos_scan = scan(2, scan_idx);
    let (last_xs, last_ys) = sub_scan[last_sub];
    let (last_xp, last_yp) = pos_scan[last_pos];
    let mut last_x = (last_xs as u32) * 4 + last_xp as u32;
    let mut last_y = (last_ys as u32) * 4 + last_yp as u32;
    if scan_idx == 2 {
        std::mem::swap(&mut last_x, &mut last_y);
    }
    let (x_prefix, x_suffix) = last_prefix(last_x);
    let (y_prefix, y_suffix) = last_prefix(last_y);
    let last_bits = (x_prefix + y_prefix + x_suffix + y_suffix) as f64;
    last_bits + ESTIMATE_SIG_BIT * (last_full + 1) as f64 + ESTIMATE_MAG_SCALE * mag_bits
}

/// What one candidate set of levels costs in bits, priced by the CABAC coder
/// itself against `base`'s context state.
#[allow(clippy::too_many_arguments)]
fn levels_bits(
    enc: &mut CabacEncoder,
    base: &CabacState,
    levels: &[i32],
    log2_size: u32,
    c_idx: usize,
    scan_idx: usize,
    cbf_ctx: usize,
    coded: bool,
    transform_skip: bool,
) -> u64 {
    enc.restore(base);
    let before = enc.bit_count();
    enc.encode_bin(cbf_ctx, u32::from(coded));
    if coded {
        encode_residual(
            enc,
            levels,
            log2_size,
            c_idx,
            scan_idx,
            false,
            transform_skip,
        );
    }
    enc.bit_count() - before
}

/// Rate-distortion quantisation of one transform block: each small level is
/// offered the magnitudes below it, and takes one when the squared error it
/// gives up costs less than the bits it saves.
///
/// The rate is the CABAC coder's own price for the whole block — `enc` is a
/// counting engine, and every trial is priced from the same context state — so
/// dropping a level is priced together with the significance map and the last
/// position that follow from it, which is where most of the saving is. The
/// distortion is the transform-domain error scaled back to samples by
/// [`coeff_ssd_scale`], so `lambda` is the caller's sample-domain lambda.
///
/// Returns the number of non-zero levels left.
#[allow(clippy::too_many_arguments)]
pub fn rdoq(
    coeffs: &[i32],
    levels: &mut [i32],
    n: usize,
    qp: i32,
    c_idx: usize,
    scan_idx: usize,
    cbf_ctx: usize,
    lambda: f64,
    transform_skip: bool,
    enc: &mut CabacEncoder,
) -> usize {
    let log2_size = n.trailing_zeros();
    let scale = coeff_ssd_scale(n);
    let lambda = RDOQ_LAMBDA * lambda;
    let base = enc.snapshot();
    let err = |level: i32, coeff: i32| {
        let d = f64::from(dequant_level(level, n, qp) - coeff);
        d * d
    };

    let mut dist: f64 = (0..n * n).map(|i| err(levels[i], coeffs[i])).sum();
    let bits = levels_bits(
        enc,
        &base,
        levels,
        log2_size,
        c_idx,
        scan_idx,
        cbf_ctx,
        true,
        transform_skip,
    );
    let mut cost = scale * dist + lambda * bits as f64;

    // Highest scanning position first: dropping the last significant level is
    // what shortens the significance map and moves the coded last position, so
    // it is tried while the levels above it are already gone.
    let sub_scan = scan(log2_size - 2, scan_idx);
    let pos_scan = scan(2, scan_idx);
    let order: Vec<usize> = sub_scan
        .iter()
        .flat_map(|&(xs, ys)| {
            pos_scan.iter().map(move |&(xp, yp)| {
                (ys as usize * 4 + yp as usize) * n + xs as usize * 4 + xp as usize
            })
        })
        .collect();
    for &i in order.iter().rev() {
        let level = levels[i];
        if level == 0 || level.abs() > RDOQ_MAX_LEVEL {
            continue;
        }
        let mut tried = level.abs();
        for candidate in [level.abs() - 1, 0] {
            if candidate == tried {
                continue;
            }
            tried = candidate;
            let keep = levels[i];
            levels[i] = candidate * level.signum();
            let trial_dist = dist - err(keep, coeffs[i]) + err(levels[i], coeffs[i]);
            let any = levels[..n * n].iter().any(|&l| l != 0);
            let trial_bits = levels_bits(
                enc,
                &base,
                levels,
                log2_size,
                c_idx,
                scan_idx,
                cbf_ctx,
                any,
                transform_skip,
            );
            let trial = scale * trial_dist + lambda * trial_bits as f64;
            if trial < cost {
                cost = trial;
                dist = trial_dist;
            } else {
                levels[i] = keep;
            }
        }
    }
    // And the block as a whole: zeroing it drops the coded-block flag as well
    // as every level, which no single-coefficient step can see.
    let zero_dist: f64 = (0..n * n).map(|i| err(0, coeffs[i])).sum();
    let zero_bits = levels_bits(
        enc,
        &base,
        levels,
        log2_size,
        c_idx,
        scan_idx,
        cbf_ctx,
        false,
        transform_skip,
    );
    if scale * zero_dist + lambda * zero_bits as f64 <= cost {
        levels[..n * n].fill(0);
        enc.restore(&base);
        return 0;
    }
    enc.restore(&base);
    levels[..n * n].iter().filter(|&&l| l != 0).count()
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
    sign_hiding: bool,
    transform_skip: bool,
) {
    if transform_skip && log2_size == 2 {
        let ctx_idx = ctx::TRANSFORM_SKIP + if c_idx == 0 { 0 } else { 1 };
        enc.encode_bin(ctx_idx, 1);
    }
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

        // Signs, then the remaining magnitudes. With sign data hiding on and
        // the significant coefficients of this sub-block more than three scan
        // positions apart, the first one's sign is not coded at all: the
        // decoder reads it off the parity of the sub-block's absolute levels,
        // which `hide_signs` has already made match.
        let hidden = sign_hiding && order[0] - order[order_len - 1] > 3;
        for (k, &p) in order.iter().enumerate() {
            if hidden && k == order_len - 1 {
                continue;
            }
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
            encode_residual(&mut enc, levels, 3, 0, 0, false, false);
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
                    encode_residual(&mut enc, &levels, log2, c_idx, scan_idx, false, false);
                    assert!(enc.bit_count() > 0);
                }
            }
        }
    }
}
