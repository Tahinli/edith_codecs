//! Sample adaptive offset (clause 8.7.3). It runs on the whole picture after
//! deblocking. Every CTB and colour component whose `SaoTypeIdx` is not 0 is
//! rewritten from a copy of the deblocked planes, so an edge offset always
//! compares against deblocked neighbours and never against a sample that SAO
//! has already changed. Samples of PCM units (when `pcm_loop_filter_disabled`
//! is set) and of transquant-bypass units are left alone. So are edge samples
//! whose neighbour lies outside the picture or across a slice boundary that
//! the slice header forbids.

use super::pic::{PicState, SaoParams};

/// `hPos` and `vPos` of the two neighbours for each `SaoEoClass`.
const EO_NEIGHBOURS: [([isize; 2], [isize; 2]); 4] = [
    ([-1, 1], [0, 0]),
    ([0, 0], [-1, 1]),
    ([-1, 1], [-1, 1]),
    ([1, -1], [-1, 1]),
];

/// The size of one colour plane and how luma positions map onto it.
#[derive(Clone, Copy)]
struct Plane {
    /// Width in samples of this plane.
    w: usize,
    /// Height in samples of this plane.
    h: usize,
    /// Horizontal shift from a luma position to a position in this plane (1 for
    /// 4:2:0 and 4:2:2 chroma, 0 for luma and 4:4:4 chroma).
    sub_x: usize,
    /// Vertical shift (1 for 4:2:0 chroma only).
    sub_y: usize,
}

/// Apply SAO to the whole picture in place.
pub fn sao_picture(pic: &mut PicState) {
    sao_picture_threads(pic, 1);
}

/// [`sao_picture`] with each plane's CTB rows split into `threads` bands, filtered
/// in parallel. A band reads the deblocked input (shared, read-only) and writes only
/// its own rows of the output, so the bands never touch the same sample.
pub fn sao_picture_threads(pic: &mut PicState, threads: usize) {
    if !pic.has_pixels() {
        return;
    }
    let ro: &PicState = pic;
    let y = filter_plane(ro, 0, &ro.planes.y, threads);
    let cb = filter_plane(ro, 1, &ro.planes.cb, threads);
    let cr = filter_plane(ro, 2, &ro.planes.cr, threads);
    pic.planes.y = y;
    pic.planes.cb = cb;
    pic.planes.cr = cr;
}

/// The SAO output of colour component `c`, computed from the input plane `src`.
///
/// Each CTB is handled with its neighbour availability worked out once (see
/// [`neighbour_cells`]), so the per-sample loop only compares a neighbour's CTB
/// cell against a 3x3 table instead of looking up slice and tile state.
fn filter_plane(pic: &PicState, c: usize, src: &[u16], threads: usize) -> Vec<u16> {
    let mut dst = src.to_vec();
    let plane = if c == 0 {
        Plane {
            w: pic.width,
            h: pic.height,
            sub_x: 0,
            sub_y: 0,
        }
    } else {
        Plane {
            w: pic.width.div_ceil(1 << pic.sub_x()),
            h: pic.height.div_ceil(1 << pic.sub_y()),
            sub_x: pic.sub_x() as usize,
            sub_y: pic.sub_y() as usize,
        }
    };
    let ctb_h = (1usize << pic.ctb_log2) >> plane.sub_y;
    let threads = threads.clamp(1, pic.ctb_rows.max(1));
    if threads == 1 {
        filter_ctb_rows(pic, c, src, plane, 0..pic.ctb_rows, &mut dst, 0);
    } else {
        let rows_per_band = pic.ctb_rows.div_ceil(threads);
        let band_len = rows_per_band * ctb_h * plane.w;
        std::thread::scope(|s| {
            for (i, band) in dst.chunks_mut(band_len).enumerate() {
                let ry0 = i * rows_per_band;
                let ry1 = (ry0 + rows_per_band).min(pic.ctb_rows);
                let y0 = ry0 * ctb_h;
                s.spawn(move || filter_ctb_rows(pic, c, src, plane, ry0..ry1, band, y0));
            }
        });
    }
    dst
}

/// SAO for the CTB rows `rows` of plane `c`, writing into `dst`, which holds the
/// plane rows from `row0` down (so `dst` may be a band of the whole output).
fn filter_ctb_rows(
    pic: &PicState,
    c: usize,
    src: &[u16],
    plane: Plane,
    rows: std::ops::Range<usize>,
    dst: &mut [u16],
    row0: usize,
) {
    let ctb_w = (1usize << pic.ctb_log2) >> plane.sub_x;
    let ctb_h = (1usize << pic.ctb_log2) >> plane.sub_y;
    let max = (1i32 << pic.bit_depth(c)) - 1;
    let band_shift = pic.bit_depth(c).saturating_sub(5);
    for ry in rows {
        for rx in 0..pic.ctb_cols {
            let Some(ctb) = pic.ctb.get(ry * pic.ctb_cols + rx) else {
                continue;
            };
            let p = &ctb.sao;
            if p.type_idx[c] == 0 {
                continue;
            }
            let x0 = rx * ctb_w;
            let y0 = ry * ctb_h;
            let bounds = Bounds {
                x0,
                y0,
                x1: (x0 + ctb_w).min(plane.w),
                y1: (y0 + ctb_h).min(plane.h),
                ctb_w,
                ctb_h,
            };
            let excl = ctb_has_excluded(pic, plane, &bounds);
            match p.type_idx[c] {
                1 => {
                    // Band offset: `band_index` of 8.7.3.2 as a 32-entry table.
                    let mut table = [0i32; 32];
                    let bp = usize::from(p.band_position[c]) & 31;
                    for k in 0..4 {
                        table[(k + bp) & 31] = sao_offset(p, c, k + 1);
                    }
                    for y in bounds.y0..bounds.y1 {
                        for x in bounds.x0..bounds.x1 {
                            if excl && excluded(pic, x << plane.sub_x, y << plane.sub_y) {
                                continue;
                            }
                            let i = src_index(plane, x, y, row0);
                            let cur = i32::from(src[y * plane.w + x]);
                            let off = table[((cur >> band_shift) & 31) as usize];
                            dst[i] = (cur + off).clamp(0, max) as u16;
                        }
                    }
                }
                2 => {
                    let Some(&pos) = EO_NEIGHBOURS.get(usize::from(p.eo_class[c])) else {
                        continue;
                    };
                    // SaoOffsetVal by sign sum + 2: edgeIdx 1, 2, 0, 3, 4.
                    let eo_off = [
                        sao_offset(p, c, 1),
                        sao_offset(p, c, 2),
                        0,
                        sao_offset(p, c, 3),
                        sao_offset(p, c, 4),
                    ];
                    let cells = neighbour_cells(pic, rx, ry);
                    for y in bounds.y0..bounds.y1 {
                        for x in bounds.x0..bounds.x1 {
                            if excl && excluded(pic, x << plane.sub_x, y << plane.sub_y) {
                                continue;
                            }
                            let i = src_index(plane, x, y, row0);
                            let cur = i32::from(src[y * plane.w + x]);
                            let Some(sign_sum) =
                                edge_sign_sum(src, plane, &bounds, &cells, pos, x, y)
                            else {
                                continue;
                            };
                            let off = eo_off[(sign_sum + 2) as usize];
                            dst[i] = (cur + off).clamp(0, max) as u16;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// SAO of the CTB row `ry` of colour component `c` (8.7.3), written into `dst`, which
/// holds the plane rows of that CTB row from its first row on. `src` is the deblocked
/// plane (a full-size buffer; the rows of `ry` and the row of each side are read from it).
pub(super) fn sao_ctb_row(pic: &PicState, c: usize, src: &[u16], ry: usize, dst: &mut [u16]) {
    let plane = plane_of(pic, c);
    let row0 = (ry << pic.ctb_log2) >> plane.sub_y;
    filter_ctb_rows(pic, c, src, plane, ry..ry + 1, dst, row0);
}

/// The size of colour component `c` of `pic`.
fn plane_of(pic: &PicState, c: usize) -> Plane {
    if c == 0 {
        Plane {
            w: pic.width,
            h: pic.height,
            sub_x: 0,
            sub_y: 0,
        }
    } else {
        Plane {
            w: pic.width.div_ceil(1 << pic.sub_x()),
            h: pic.height.div_ceil(1 << pic.sub_y()),
            sub_x: pic.sub_x() as usize,
            sub_y: pic.sub_y() as usize,
        }
    }
}

/// The index of plane position `(x, y)` in a buffer that starts at plane row `row0`.
fn src_index(plane: Plane, x: usize, y: usize, row0: usize) -> usize {
    (y - row0) * plane.w + x
}

/// The luma-to-plane bounds of one CTB: `[x0, x1) x [y0, y1)` in plane samples,
/// clipped to the picture, and the unclipped CTB size in plane samples.
#[derive(Clone, Copy)]
struct Bounds {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    ctb_w: usize,
    ctb_h: usize,
}

/// Sum of the two edge signs of sample `(x, y)` (each in -1..=1), or `None` when a
/// neighbour is outside the picture or across a forbidden slice or tile boundary.
fn edge_sign_sum(
    src: &[u16],
    plane: Plane,
    b: &Bounds,
    cells: &[bool; 9],
    pos: ([isize; 2], [isize; 2]),
    x: usize,
    y: usize,
) -> Option<i32> {
    let cur = i32::from(*src.get(y * plane.w + x)?);
    let mut sign_sum = 0i32;
    for k in 0..2 {
        let nx = x as isize + pos.0[k];
        let ny = y as isize + pos.1[k];
        if nx < 0 || ny < 0 || nx as usize >= plane.w || ny as usize >= plane.h {
            return None;
        }
        let (nx, ny) = (nx as usize, ny as usize);
        let cx = cell(nx, b.x0, b.ctb_w);
        let cy = cell(ny, b.y0, b.ctb_h);
        if !cells[cy * 3 + cx] {
            return None;
        }
        let n = i32::from(src[ny * plane.w + nx]);
        sign_sum += (cur - n).signum();
    }
    Some(sign_sum)
}

/// Which of the three CTB columns (or rows) a neighbouring plane position falls in,
/// given the first position of the current CTB: 0 before it, 2 past its end, else 1.
fn cell(v: usize, start: usize, ctb_size: usize) -> usize {
    if v < start {
        0
    } else if v >= start + ctb_size {
        2
    } else {
        1
    }
}

/// Whether a neighbouring CTB may be used by the CTB at `(rx, ry)`, for each cell
/// of its 3x3 neighbourhood (row-major, `cell = cy * 3 + cx`). The centre cell is
/// always usable; a cell off the CTB grid is not. Slice and tile rules are decided
/// per CTB pair by [`across_slice_allowed`] on the CTB origins.
fn neighbour_cells(pic: &PicState, rx: usize, ry: usize) -> [bool; 9] {
    let luma = 1usize << pic.ctb_log2;
    let mut cells = [false; 9];
    for cy in 0..3 {
        for cx in 0..3 {
            let nrx = rx as isize + cx as isize - 1;
            let nry = ry as isize + cy as isize - 1;
            let ok = if cx == 1 && cy == 1 {
                true
            } else if nrx < 0
                || nry < 0
                || nrx as usize >= pic.ctb_cols
                || nry as usize >= pic.ctb_rows
            {
                false
            } else {
                across_slice_allowed(
                    pic,
                    (rx * luma, ry * luma),
                    (nrx as usize * luma, nry as usize * luma),
                )
            };
            cells[cy * 3 + cx] = ok;
        }
    }
    cells
}

/// Whether any 4x4 unit of the CTB is excluded from SAO (see [`excluded`]).
fn ctb_has_excluded(pic: &PicState, plane: Plane, b: &Bounds) -> bool {
    let (lx0, ly0) = (b.x0 << plane.sub_x, b.y0 << plane.sub_y);
    let lx1 = (b.x1 << plane.sub_x).min(pic.width);
    let ly1 = (b.y1 << plane.sub_y).min(pic.height);
    (ly0..ly1)
        .step_by(4)
        .any(|y| (lx0..lx1).step_by(4).any(|x| excluded(pic, x, y)))
}

/// `SaoOffsetVal[idx]` of component `c`: 0 for index 0, else the signed,
/// scaled offset `idx`.
fn sao_offset(p: &SaoParams, c: usize, idx: usize) -> i32 {
    idx.checked_sub(1)
        .and_then(|k| p.offset[c].get(k))
        .map_or(0, |&v| i32::from(v))
}

/// Whether the neighbouring luma position `nb` may be used by a sample at
/// luma position `cur` under the slice and tile boundary rules of 8.7.3.2. For a
/// slice boundary the flag of the later slice in decoding order decides.
fn across_slice_allowed(pic: &PicState, cur: (usize, usize), nb: (usize, usize)) -> bool {
    let (Some(cc), Some(nc)) = (
        pic.ctb.get(pic.ctb_rs(cur.0, cur.1)),
        pic.ctb.get(pic.ctb_rs(nb.0, nb.1)),
    ) else {
        return false;
    };
    if cc.tile != nc.tile && !pic.loop_filter_across_tiles {
        return false;
    }
    if cc.slice_addr == nc.slice_addr {
        return true;
    }
    let owner = if pic.zscan(nb.0, nb.1) < pic.zscan(cur.0, cur.1) {
        cc.slice
    } else {
        nc.slice
    };
    pic.slices
        .get(usize::from(owner))
        .is_some_and(|s| s.loop_filter_across_slices)
}

/// Whether the luma unit at `(x, y)` is a PCM unit with loop filtering
/// disabled or a transquant-bypass unit, whose samples SAO must not change.
fn excluded(pic: &PicState, x: usize, y: usize) -> bool {
    pic.info
        .get(pic.unit(x, y))
        .is_some_and(|i| i.bypass || (i.pcm && pic.pcm_loop_filter_disabled))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16x16 8-bit picture with one 16x16 CTB and no slices.
    fn picture() -> PicState {
        PicState::new(16, 16, 4, 2, 8, 8, true)
    }

    /// SAO parameters for the luma component only.
    fn luma_sao(type_idx: u8, eo_class: u8, offset: [i16; 4]) -> SaoParams {
        SaoParams {
            type_idx: [type_idx, 0, 0],
            band_position: [0; 3],
            eo_class: [eo_class, 0, 0],
            offset: [offset, [0; 4], [0; 4]],
        }
    }

    /// Fill the luma plane with 10 on even columns and 20 on odd columns, so
    /// every interior sample is a local extremum along the horizontal.
    fn zigzag(pic: &mut PicState) {
        for y in 0..16 {
            for x in 0..16 {
                pic.planes.y[y * 16 + x] = if x % 2 == 0 { 10 } else { 20 };
            }
        }
    }

    /// The class-0 edge result of [`zigzag`] with offsets `[5, 6, 7, 8]`:
    /// a 10 sample has edgeIdx 1 (offset 5), a 20 sample edgeIdx 4 (offset 8).
    fn zigzag_result(x: usize) -> u16 {
        if x.is_multiple_of(2) { 15 } else { 28 }
    }

    #[test]
    fn band_offset_adds_offset_only_inside_bands() {
        let mut pic = picture();
        pic.planes.y.iter_mut().for_each(|s| *s = 8);
        pic.planes.y[5] = 40;
        pic.ctb[0].sao = luma_sao(1, 0, [1, 2, 3, 4]);
        sao_picture(&mut pic);
        // 8 >> 3 = band 1, which is k = 1 and so offset[1] = 2.
        assert_eq!(pic.planes.y[0], 10);
        // 40 >> 3 = band 5, outside the four bands: unchanged.
        assert_eq!(pic.planes.y[5], 40);
    }

    #[test]
    fn edge_offset_class0_on_ramp_keeps_picture_edges() {
        let mut pic = picture();
        zigzag(&mut pic);
        pic.ctb[0].sao = luma_sao(2, 0, [5, 6, 7, 8]);
        sao_picture(&mut pic);
        for y in 0..16 {
            for x in 1..15 {
                assert_eq!(pic.planes.y[y * 16 + x], zigzag_result(x), "x={x} y={y}");
            }
            // Picture edge samples have a neighbour outside the picture.
            assert_eq!(pic.planes.y[y * 16], 10);
            assert_eq!(pic.planes.y[y * 16 + 15], 20);
        }
    }

    #[test]
    fn bypass_units_are_untouched() {
        let mut pic = picture();
        zigzag(&mut pic);
        pic.update_info(4, 0, 4, 16, |i| i.bypass = true);
        pic.ctb[0].sao = luma_sao(2, 0, [5, 6, 7, 8]);
        sao_picture(&mut pic);
        for y in 0..16 {
            for x in 4..8 {
                assert_eq!(
                    pic.planes.y[y * 16 + x],
                    if x % 2 == 0 { 10 } else { 20 },
                    "x={x} y={y}"
                );
            }
            // The neighbour of a bypass unit is still filtered (x = 3 is odd).
            assert_eq!(pic.planes.y[y * 16 + 3], 28);
        }
    }

    #[test]
    fn disabled_type_leaves_samples_unchanged() {
        let mut pic = picture();
        zigzag(&mut pic);
        let before = pic.planes.y.clone();
        pic.ctb[0].sao = luma_sao(0, 0, [5, 6, 7, 8]);
        sao_picture(&mut pic);
        assert_eq!(pic.planes.y, before);
    }
}
