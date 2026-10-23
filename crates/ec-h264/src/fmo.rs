//! Macroblock-to-slice-group map (clause 8.2.2), for FMO streams.

use ec_h264_syntax::pps::SliceGroupMap;
use ec_h264_syntax::{Pps, Sps};

/// The slice group of every macroblock of a progressive picture, in raster
/// order. `change_cycle` is `slice_group_change_cycle` of the slice header.
pub fn mb_to_slice_group(sps: &Sps, pps: &Pps, change_cycle: u32) -> Option<Vec<u8>> {
    let map = pps.slice_group_map.as_ref()?;
    let n = pps.num_slice_groups as usize;
    let w = sps.mb_width as usize;
    let h = sps.mb_height as usize;
    let size = w * h;
    let mut m = vec![0u8; size];
    match map {
        SliceGroupMap::Interleaved(runs) => {
            let mut i = 0;
            while i < size {
                for (g, run) in runs.iter().enumerate().take(n) {
                    let mut j = 0;
                    while j <= *run as usize && i + j < size {
                        m[i + j] = g as u8;
                        j += 1;
                    }
                    i += *run as usize + 1;
                    if i >= size {
                        break;
                    }
                }
            }
        }
        SliceGroupMap::Dispersed => {
            for (i, v) in m.iter_mut().enumerate() {
                *v = (((i % w) + (((i / w) * n) / 2)) % n) as u8;
            }
        }
        SliceGroupMap::Foreground(rects) => {
            m.fill((n - 1) as u8);
            for g in (0..n - 1).rev() {
                let (tl, br) = rects[g];
                let (tl, br) = (tl as usize, br as usize);
                let (y0, x0, y1, x1) = (tl / w, tl % w, br / w, br % w);
                for y in y0..=y1.min(h - 1) {
                    for x in x0..=x1.min(w - 1) {
                        m[y * w + x] = g as u8;
                    }
                }
            }
        }
        SliceGroupMap::Changing {
            map_type,
            change_direction,
            change_rate_minus1,
        } => {
            let dir = usize::from(*change_direction);
            let rate = *change_rate_minus1 as usize + 1;
            let in0 = (change_cycle as usize * rate).min(size);
            let upper_left = if dir == 1 { size - in0 } else { in0 };
            match map_type {
                3 => {
                    m.fill(1);
                    let (mut x, mut y) = (((w - dir) / 2) as i64, ((h - dir) / 2) as i64);
                    let (mut lb, mut tb, mut rb, mut bb) = (x, y, x, y);
                    let d = dir as i64;
                    let (mut xd, mut yd) = (d - 1, d);
                    let mut k = 0;
                    while k < in0 {
                        let idx = y as usize * w + x as usize;
                        let vacant = m[idx] == 1;
                        if vacant {
                            m[idx] = 0;
                        }
                        if xd == -1 && x == lb {
                            lb = (lb - 1).max(0);
                            x = lb;
                            xd = 0;
                            yd = 2 * d - 1;
                        } else if xd == 1 && x == rb {
                            rb = (rb + 1).min(w as i64 - 1);
                            x = rb;
                            xd = 0;
                            yd = 1 - 2 * d;
                        } else if yd == -1 && y == tb {
                            tb = (tb - 1).max(0);
                            y = tb;
                            xd = 1 - 2 * d;
                            yd = 0;
                        } else if yd == 1 && y == bb {
                            bb = (bb + 1).min(h as i64 - 1);
                            y = bb;
                            xd = 2 * d - 1;
                            yd = 0;
                        } else {
                            x += xd;
                            y += yd;
                        }
                        k += usize::from(vacant);
                    }
                }
                4 => {
                    for (i, v) in m.iter_mut().enumerate() {
                        *v = if i < upper_left { dir } else { 1 - dir } as u8;
                    }
                }
                _ => {
                    let mut k = 0;
                    for j in 0..w {
                        for i in 0..h {
                            m[i * w + j] = if k < upper_left { dir } else { 1 - dir } as u8;
                            k += 1;
                        }
                    }
                }
            }
        }
        SliceGroupMap::Explicit(ids) => {
            for (i, v) in m.iter_mut().enumerate() {
                *v = ids.get(i).copied().unwrap_or(0) as u8;
            }
        }
    }
    Some(m)
}

/// The macroblock after `addr` in the same slice group, or `m.len()`.
pub fn next_in_group(m: &[u8], addr: usize) -> usize {
    let g = m[addr];
    let mut a = addr + 1;
    while a < m.len() && m[a] != g {
        a += 1;
    }
    a
}
