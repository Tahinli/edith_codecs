//! Intra prediction (spec 7.11; libvpx `vp9_reconintra.c
//! build_intra_predictors` + `vpx_dsp/intrapred.c` kernels, ported
//! verbatim — including the 127/129 border fills and the edge
//! replication rules at frame boundaries).
//!
//! Neighbour staging: `above_data[15]` is `above[-1]` (the above-left
//! corner), `above_data[16 + i]` is `above[i]` — exactly the C's
//! 16-pixel offset into `above_data[64 + 16]`.

/// `AVG3`.
#[inline]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((a as u32 + 2 * b as u32 + c as u32 + 2) >> 2) as u8
}

/// `AVG2`.
#[inline]
fn avg2(a: u8, b: u8) -> u8 {
    ((a as u32 + b as u32 + 1) >> 1) as u8
}

const A: usize = 16;

/// `extend_modes` (vp9_reconintra.c): (NEED_LEFT, NEED_ABOVE,
/// NEED_ABOVERIGHT) per mode.
fn needs(mode: u8) -> (bool, bool, bool) {
    match mode {
        0 => (true, true, false),  // DC
        1 => (false, true, false), // V
        2 => (true, false, false), // H
        3 => (false, false, true), // D45
        4 => (true, true, false),  // D135
        5 => (true, true, false),  // D117
        6 => (true, true, false),  // D153
        7 => (true, false, false), // D207
        8 => (false, false, true), // D63
        _ => (true, true, false),  // TM
    }
}

/// `build_intra_predictors` + mode dispatch for one plane of the
/// reconstructed frame. `data` is the whole plane; `(x0, y0)` the
/// block's absolute sample position; `frame_w`/`frame_h` the coded
/// (aligned) plane size.
pub(crate) fn build_intra_predictors(
    data: &mut [u8],
    stride: usize,
    frame_w: usize,
    frame_h: usize,
    x0: usize,
    y0: usize,
    mode: u8,
    bs: usize,
    up_available: bool,
    left_available: bool,
    right_available: bool,
) {
    let (need_left, need_above, need_aboveright) = needs(mode);
    let mut left_col = [0u8; 32];
    let mut above_data = [0u8; 64 + 16];
    let at = |dx: isize, dy: isize| -> u8 {
        data[(((y0 as isize + dy) * stride as isize) + x0 as isize + dx) as usize]
    };

    if need_left {
        if left_available {
            if y0 + bs <= frame_h {
                for i in 0..bs {
                    left_col[i] = at(-1, i as isize);
                }
            } else {
                let extend_bottom = frame_h - y0;
                for i in 0..extend_bottom {
                    left_col[i] = at(-1, i as isize);
                }
                for i in extend_bottom..bs {
                    left_col[i] = at(-1, extend_bottom as isize - 1);
                }
            }
        } else {
            left_col[..bs].fill(129);
        }
    }

    if need_above {
        if up_available {
            if x0 + bs <= frame_w {
                for i in 0..bs {
                    above_data[A + i] = at(i as isize, -1);
                }
            } else if x0 <= frame_w {
                let r = frame_w - x0;
                for i in 0..r {
                    above_data[A + i] = at(i as isize, -1);
                }
                let v = above_data[A + r - 1];
                let n = x0 + bs - frame_w;
                above_data[A + r..A + r + n].fill(v);
            }
            above_data[A - 1] = if left_available { at(-1, -1) } else { 129 };
        } else {
            above_data[A..A + bs].fill(127);
            above_data[A - 1] = 127;
        }
    }

    if need_aboveright {
        if up_available {
            if x0 + 2 * bs <= frame_w {
                for i in 0..2 * bs {
                    above_data[A + i] = at(i as isize, -1);
                }
                if !(bs == 4 && right_available) {
                    let v = above_data[A + bs - 1];
                    above_data[A + bs..A + 2 * bs].fill(v);
                }
            } else if x0 + bs <= frame_w {
                let r = frame_w - x0;
                for i in 0..r {
                    above_data[A + i] = at(i as isize, -1);
                }
                if bs == 4 && right_available {
                    let v = above_data[A + r - 1];
                    let n = x0 + 2 * bs - frame_w;
                    above_data[A + r..A + r + n].fill(v);
                } else {
                    let v = above_data[A + bs - 1];
                    above_data[A + bs..A + 2 * bs].fill(v);
                }
            } else if x0 <= frame_w {
                let r = frame_w - x0;
                for i in 0..r {
                    above_data[A + i] = at(i as isize, -1);
                }
                let v = above_data[A + r - 1];
                let n = x0 + 2 * bs - frame_w;
                above_data[A + r..A + r + n].fill(v);
            }
            above_data[A - 1] = if left_available { at(-1, -1) } else { 129 };
        } else {
            above_data[A..A + 2 * bs].fill(127);
            above_data[A - 1] = 127;
        }
    }

    // The C's `const_above_row = above_ref` shortcut for bs==4 &&
    // right&&left stages the same bytes, so above_data is equivalent.
    if crate::trace_enabled()
        && std::env::var("EC_VP9_PROBE_TU")
            .ok()
            .and_then(|s| {
                let mut it = s.split(',');
                Some((it.next()?.parse::<usize>().ok()?, it.next()?.parse().ok()?))
            })
            .is_some_and(|(px, py)| x0 == px && y0 == py)
    {
        eprintln!(
            "STAGE x0={x0} y0={y0} mode={mode} bs={bs} above={:?} cor={} left={:?}",
            &above_data[A..A + 8],
            above_data[A - 1],
            &left_col[..4]
        );
    }
    let dst = &mut data[y0 * stride + x0..];
    dispatch(
        mode, bs, dst, stride, &above_data, &left_col, up_available, left_available,
    );
}

fn dispatch(
    mode: u8,
    bs: usize,
    dst: &mut [u8],
    stride: usize,
    above: &[u8],
    left: &[u8],
    up: bool,
    lft: bool,
) {
    // `above` is the full above_data: [A-1] = above[-1], [A + i] = above[i].
    let cor = above[A - 1];
    let ab = &above[A..];
    match mode {
        0 if up && lft => {
            let sum: u32 = ab[..bs]
                .iter()
                .chain(left[..bs].iter())
                .map(|&v| v as u32)
                .sum();
            fill_dc(dst, stride, bs, ((sum + bs as u32) / (2 * bs as u32)) as u8);
        }
        0 if lft => {
            let sum: u32 = left[..bs].iter().map(|&v| v as u32).sum();
            fill_dc(dst, stride, bs, ((sum + bs as u32 / 2) / bs as u32) as u8);
        }
        0 if up => {
            let sum: u32 = ab[..bs].iter().map(|&v| v as u32).sum();
            fill_dc(dst, stride, bs, ((sum + bs as u32 / 2) / bs as u32) as u8);
        }
        0 => fill_dc(dst, stride, bs, 128),
        1 => {
            for r in 0..bs {
                dst[r * stride..r * stride + bs].copy_from_slice(&ab[..bs]);
            }
        }
        2 => {
            for r in 0..bs {
                dst[r * stride..r * stride + bs].fill(left[r]);
            }
        }
        // The 4x4 kernels in intrapred.c are specialized implementations,
        // NOT the shared `d45_predictor`/`d63_predictor`/`d153_predictor`
        // INLINE loops: they continue the diagonal with the staged
        // above-right samples (vpx_d45/d63_predictor_4x4_c, and d153's
        // DST(2,3)). Only 8x8+ use the generic loops below.
        3 if bs == 4 => {
            // vpx_d45_predictor_4x4_c
            for r in 0..4usize {
                for c in 0..4usize {
                    dst[r * stride + c] = if r + c < 6 {
                        avg3(ab[r + c], ab[r + c + 1], ab[r + c + 2])
                    } else {
                        ab[7]
                    };
                }
            }
        }
        6 if bs == 4 => {
            // vpx_d153_predictor_4x4_c
            dst[0] = avg2(left[0], cor);
            dst[stride] = avg2(left[1], left[0]);
            dst[2 * stride] = avg2(left[2], left[1]);
            dst[3 * stride] = avg2(left[3], left[2]);
            dst[1] = avg3(left[0], cor, ab[0]);
            dst[stride + 1] = avg3(left[1], left[0], cor);
            dst[2 * stride + 1] = avg3(left[2], left[1], left[0]);
            dst[3 * stride + 1] = avg3(left[3], left[2], left[1]);
            dst[2] = avg3(cor, ab[0], ab[1]);
            dst[stride + 2] = dst[0];
            dst[2 * stride + 2] = dst[stride];
            dst[3 * stride + 2] = dst[2 * stride];
            dst[3] = avg3(ab[0], ab[1], ab[2]);
            dst[stride + 3] = dst[1];
            dst[2 * stride + 3] = avg3(left[1], left[0], cor); // DST(3, 2)
            dst[3 * stride + 3] = avg3(left[2], left[1], left[0]);
        }
        8 if bs == 4 => {
            // vpx_d63_predictor_4x4_c
            for c in 0..4usize {
                dst[c] = avg2(ab[c], ab[c + 1]);
                dst[stride + c] = avg3(ab[c], ab[c + 1], ab[c + 2]);
                dst[2 * stride + c] = avg2(ab[c + 1], ab[c + 2]);
                dst[3 * stride + c] = avg3(ab[c + 1], ab[c + 2], ab[c + 3]);
            }
        }
        3 => {
            // d45
            let above_right = ab[bs - 1];
            for x in 0..bs - 1 {
                dst[x] = avg3(ab[x], ab[x + 1], ab[x + 2]);
            }
            dst[bs - 1] = above_right;
            let mut x = 1;
            let mut size = bs - 2;
            while x < bs {
                let src: Vec<u8> = dst[..x + size].to_vec();
                let off = x * stride;
                dst[off..off + size].copy_from_slice(&src[x..x + size]);
                dst[off + size..off + bs].fill(above_right);
                x += 1;
                size = size.saturating_sub(1);
            }
        }
        4 => {
            // d135
            let mut border = [0u8; 64];
            for i in 0..bs - 2 {
                border[i] = avg3(left[bs - 3 - i], left[bs - 2 - i], left[bs - 1 - i]);
            }
            border[bs - 2] = avg3(cor, left[0], left[1]);
            border[bs - 1] = avg3(left[0], cor, ab[0]);
            border[bs] = avg3(cor, ab[0], ab[1]);
            for i in 0..bs - 2 {
                border[bs + 1 + i] = avg3(ab[i], ab[i + 1], ab[i + 2]);
            }
            for i in 0..bs {
                dst[i * stride..i * stride + bs]
                    .copy_from_slice(&border[bs - 1 - i..bs - 1 - i + bs]);
            }
        }
        5 => {
            // d117
            for c in 0..bs {
                dst[c] = avg2(above[A - 1 + c], ab[c]);
            }
            dst[stride] = avg3(left[0], cor, ab[0]);
            for c in 1..bs {
                dst[stride + c] = avg3(above[A + c - 2], ab[c - 1], ab[c]);
            }
            dst[2 * stride] = avg3(cor, left[0], left[1]);
            // libvpx anchors this loop at row 2 (`dst` walked twice);
            // our `dst` is the block base, so the index is absolute.
            for r in 3..bs {
                dst[r * stride] = avg3(left[r - 3], left[r - 2], left[r - 1]);
            }
            for r in 2..bs {
                for c in 1..bs {
                    dst[r * stride + c] = dst[(r - 2) * stride + c - 1];
                }
            }
        }
        6 => {
            // d153
            dst[0] = avg2(cor, left[0]);
            for r in 1..bs {
                dst[r * stride] = avg2(left[r - 1], left[r]);
            }
            dst[1] = avg3(left[0], cor, ab[0]);
            dst[stride + 1] = avg3(cor, left[0], left[1]);
            for r in 2..bs {
                dst[r * stride + 1] = avg3(left[r - 2], left[r - 1], left[r]);
            }
            for c in 0..bs - 2 {
                dst[2 + c] = avg3(above[A - 1 + c], ab[c], ab[c + 1]);
            }
            for r in 1..bs {
                for c in 0..bs - 2 {
                    dst[(r + 1) * stride + 2 + c] = dst[r * stride + c];
                }
            }
        }
        7 => {
            // d207
            for r in 0..bs - 1 {
                dst[r * stride] = avg2(left[r], left[r + 1]);
            }
            dst[(bs - 1) * stride] = left[bs - 1];
            for r in 0..bs.saturating_sub(2) {
                dst[r * stride + 1] = avg3(left[r], left[r + 1], left[r + 2]);
            }
            if bs >= 2 {
                dst[(bs - 2) * stride + 1] = avg3(left[bs - 2], left[bs - 1], left[bs - 1]);
            }
            dst[(bs - 1) * stride + 1] = left[bs - 1];
            for c in 0..bs - 2 {
                dst[(bs - 1) * stride + 2 + c] = left[bs - 1];
            }
            for r in (0..bs - 1).rev() {
                for c in 0..bs - 2 {
                    dst[r * stride + 2 + c] = dst[(r + 1) * stride + c];
                }
            }
        }
        8 => {
            // d63
            for c in 0..bs {
                dst[c] = avg2(ab[c], ab[c + 1]);
                dst[stride + c] = avg3(ab[c], ab[c + 1], ab[c + 2]);
            }
            let mut r = 2;
            let mut size = bs - 2;
            while r < bs {
                let src0: Vec<u8> = dst[(r >> 1)..(r >> 1) + size].to_vec();
                let base1 = stride + (r >> 1);
                let src1: Vec<u8> = dst[base1..base1 + size].to_vec();
                let o0 = r * stride;
                dst[o0..o0 + size].copy_from_slice(&src0);
                dst[o0 + size..o0 + bs].fill(ab[bs - 1]);
                let o1 = (r + 1) * stride;
                dst[o1..o1 + size].copy_from_slice(&src1);
                dst[o1 + size..o1 + bs].fill(ab[bs - 1]);
                r += 2;
                size -= 1;
            }
        }
        _ => {
            // TM
            for r in 0..bs {
                for c in 0..bs {
                    dst[r * stride + c] =
                        (left[r] as i32 + ab[c] as i32 - cor as i32).clamp(0, 255) as u8;
                }
            }
        }
    }
}

fn fill_dc(dst: &mut [u8], stride: usize, bs: usize, v: u8) {
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(v);
    }
}
