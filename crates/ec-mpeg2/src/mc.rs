//! Motion compensation primitives (7.6.4): half-sample interpolation from a
//! reference image with edge replication.

/// A reference image (one plane of a picture), optionally one field of it.
#[derive(Clone, Copy)]
pub(crate) struct Image<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) stride: usize,
    /// Width in samples.
    pub(crate) w: usize,
    /// Height in lines of this image (half the plane for a field).
    pub(crate) h: usize,
    /// `Some(parity)` when reading one field of the plane.
    pub(crate) field: Option<u8>,
}

impl Image<'_> {
    #[inline(always)]
    fn row(&self, line: i32) -> &[u8] {
        let l = line.clamp(0, self.h as i32 - 1) as usize;
        let r = match self.field {
            Some(p) => 2 * l + p as usize,
            None => l,
        };
        &self.data[r * self.stride..r * self.stride + self.w]
    }
}

/// Predict a `w` x `rows` block at integer position (`x`, `y`) with half-sample
/// fractions (`fx`, `fy`); rows are written to `dst` at line `dst_row0 +
/// k * dst_step` (pitch 16). With `avg` the result is averaged into `dst`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict(
    img: &Image,
    x: i32,
    y: i32,
    fx: bool,
    fy: bool,
    w: usize,
    rows: usize,
    dst: &mut [u8; 256],
    dst_row0: usize,
    dst_step: usize,
    avg: bool,
) {
    // Source patch of (rows + 1) lines of (w + 1) samples.
    let mut patch = [[0u8; 17]; 17];
    let inside = x >= 0 && (x as usize) + w < img.w;
    for (j, prow) in patch.iter_mut().enumerate().take(rows + 1) {
        let src = img.row(y + j as i32);
        if inside {
            prow[..w + 1].copy_from_slice(&src[x as usize..x as usize + w + 1]);
        } else {
            for (i, p) in prow.iter_mut().enumerate().take(w + 1) {
                *p = src[(x + i as i32).clamp(0, img.w as i32 - 1) as usize];
            }
        }
    }
    for j in 0..rows {
        let out = &mut dst[(dst_row0 + j * dst_step) * 16..][..w];
        let a = &patch[j];
        let b = &patch[j + 1];
        match (fx, fy) {
            (false, false) => {
                if avg {
                    for i in 0..w {
                        out[i] = ((out[i] as u32 + a[i] as u32 + 1) >> 1) as u8;
                    }
                } else {
                    out.copy_from_slice(&a[..w]);
                }
            }
            (true, false) => {
                for i in 0..w {
                    let v = (a[i] as u32 + a[i + 1] as u32 + 1) >> 1;
                    out[i] = if avg { ((out[i] as u32 + v + 1) >> 1) as u8 } else { v as u8 };
                }
            }
            (false, true) => {
                for i in 0..w {
                    let v = (a[i] as u32 + b[i] as u32 + 1) >> 1;
                    out[i] = if avg { ((out[i] as u32 + v + 1) >> 1) as u8 } else { v as u8 };
                }
            }
            (true, true) => {
                for i in 0..w {
                    let v = (a[i] as u32 + a[i + 1] as u32 + b[i] as u32 + b[i + 1] as u32 + 2) >> 2;
                    out[i] = if avg { ((out[i] as u32 + v + 1) >> 1) as u8 } else { v as u8 };
                }
            }
        }
    }
}
