//! Chroma upsampling, shared by the two formats that subsample it.
//!
//! Both JPEG and VP8 store chroma at half resolution and both are decoded by
//! their reference implementations with the triangle ("fancy") filter — the
//! 9/3/3/1 weighting a separable 3:1 pass in each direction produces. Pixel
//! replication is cheaper and visibly wrong: it puts a two-pixel staircase on
//! every saturated colour edge, and costs about a decibel against the source.

/// Triangle ("fancy") upsampling of one component to full resolution.
///
/// The 3:1 weighting is the same filter libjpeg uses, which is why a 4:2:0
/// photo decoded here and by the incumbent differ by rounding rather than by a
/// visible staircase on every colour edge.
pub fn upsample(plane: &[u8], pw: usize, ph: usize, out_w: usize, out_h: usize) -> Vec<u8> {
    if pw == out_w && ph == out_h {
        return plane.to_vec();
    }
    // Horizontal first, then vertical; each pass either doubles with the
    // triangle filter or falls back to nearest for uncommon ratios.
    let mut wide = vec![0u8; out_w * ph];
    for y in 0..ph {
        let row = &plane[y * pw..y * pw + pw];
        if out_w.div_ceil(2) == pw {
            for x in 0..pw {
                let left = row[x.saturating_sub(1)];
                let right = row[(x + 1).min(pw - 1)];
                let here = u32::from(row[x]) * 3;
                let a = ((here + u32::from(left) + 2) >> 2) as u8;
                let b = ((here + u32::from(right) + 1) >> 2) as u8;
                if 2 * x < out_w {
                    wide[y * out_w + 2 * x] = a;
                }
                if 2 * x + 1 < out_w {
                    wide[y * out_w + 2 * x + 1] = b;
                }
            }
        } else {
            for x in 0..out_w {
                wide[y * out_w + x] = row[(x * pw / out_w).min(pw - 1)];
            }
        }
    }
    let mut out = vec![0u8; out_w * out_h];
    for y in 0..out_h {
        if out_h.div_ceil(2) == ph {
            let near = y / 2;
            let far = if y % 2 == 0 {
                near.saturating_sub(1)
            } else {
                (near + 1).min(ph - 1)
            };
            for x in 0..out_w {
                let n = u32::from(wide[near * out_w + x]) * 3;
                let f = u32::from(wide[far * out_w + x]);
                out[y * out_w + x] = ((n + f + 2) >> 2) as u8;
            }
        } else {
            let src = (y * ph / out_h).min(ph - 1);
            out[y * out_w..(y + 1) * out_w].copy_from_slice(&wide[src * out_w..(src + 1) * out_w]);
        }
    }
    out
}
