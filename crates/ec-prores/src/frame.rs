//! Picture assembly: slice tables, row groups, optional slice-parallel decode, weaving.

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Interlace, PixelFormat, Plane, VideoFrame};

use crate::header::{self, FrameInfo};
use crate::slice::{RowBuf, Scratch, SliceCtx, SliceIn, decode_slices};
use crate::tables::{INTERLACED_SCAN, PROGRESSIVE_SCAN};

/// Decoded alpha: 16-bit samples, `width` by `height`, full scale 0xFFFF (8-bit coded alpha is
/// replicated into both bytes). `data` holds `stride` samples per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaPlane {
    /// Samples per row visible.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Samples between row starts.
    pub stride: usize,
    /// Little-endian-neutral native `u16` samples.
    pub data: Vec<u16>,
}

/// A decoded frame, its header facts and its alpha plane when the stream has one.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// The picture: [`PixelFormat::I210`] (4:2:2) or [`PixelFormat::I412`] (4:4:4).
    pub video: VideoFrame,
    /// Alpha, when the frame header declares an alpha channel.
    pub alpha: Option<AlphaPlane>,
    /// Frame header facts.
    pub info: FrameInfo,
}

struct SliceDesc {
    mb_x: usize,
    mb_y: usize,
    mb_count: usize,
    start: usize,
    end: usize,
}

struct Pic {
    slices: Vec<SliceDesc>,
    mb_rows: usize,
}

fn build_slices(frame: &[u8], p: &header::Picture, mb_width: usize, mb_rows: usize) -> Result<Pic> {
    let mut slices = Vec::with_capacity(p.slice_count);
    let mut off = p.data_at;
    let mut n = 0usize;
    for mb_y in 0..mb_rows {
        let mut cnt = 1usize << p.log2_slice_mbs;
        let mut mb_x = 0;
        while mb_x < mb_width {
            while mb_width - mb_x < cnt {
                cnt >>= 1;
            }
            if n >= p.slice_count {
                return Err(Error::corrupt("ProRes picture: fewer slices than the macroblock grid needs"));
            }
            let sz = usize::from(header::be16(frame, p.table_at + n * 2).unwrap_or(0));
            let end = off.checked_add(sz).filter(|&e| e <= p.end).ok_or_else(|| {
                Error::corrupt("ProRes picture: slice sizes run past the picture")
            })?;
            slices.push(SliceDesc { mb_x, mb_y, mb_count: cnt, start: off, end });
            off = end;
            mb_x += cnt;
            n += 1;
        }
    }
    if n != p.slice_count {
        return Err(Error::corrupt(format!(
            "ProRes picture: {} slices declared, macroblock grid needs {n}",
            p.slice_count
        )));
    }
    Ok(Pic { slices, mb_rows })
}

fn decode_group(
    cx: &SliceCtx,
    frame: &[u8],
    pic: &Pic,
    r0: usize,
    r1: usize,
    mb_width: usize,
) -> Result<RowBuf> {
    let rows = (r1 - r0) * 16;
    let ys = mb_width * 16;
    let cs = if cx.is_444 { ys } else { mb_width * 8 };
    let mut rb = RowBuf {
        y: vec![0; rows * ys * 2],
        u: vec![0; rows * cs * 2],
        v: vec![0; rows * cs * 2],
        a: if cx.alpha_bits != 0 { vec![0; rows * ys] } else { Vec::new() },
        ys,
        cs,
    };
    let mut sc = Scratch::new();
    // Slices are stored row-major.
    let first = pic.slices.partition_point(|s| s.mb_y < r0);
    let list: Vec<SliceIn> = pic.slices[first..]
        .iter()
        .take_while(|s| s.mb_y < r1)
        .map(|s| SliceIn { data: &frame[s.start..s.end], mb_count: s.mb_count, mb_x: s.mb_x, row: s.mb_y - r0 })
        .collect();
    for pair in list.chunks(2) {
        decode_slices(cx, pair, &mut rb, &mut sc)?;
    }
    Ok(rb)
}

/// Decode one picture (a frame, or one field) into row groups.
fn decode_picture(cx: &SliceCtx, frame: &[u8], pic: &Pic, mb_width: usize, threads: usize) -> Result<Vec<(usize, RowBuf)>> {
    let rows = pic.mb_rows;
    let nthreads = threads.clamp(1, 6).min(rows);
    if nthreads <= 1 || pic.slices.len() < 8 {
        return Ok(vec![(0, decode_group(cx, frame, pic, 0, rows, mb_width)?)]);
    }
    // Contiguous bands of rows, one per thread.
    let mut bands = Vec::new();
    for t in 0..nthreads {
        let (a, b) = (rows * t / nthreads, rows * (t + 1) / nthreads);
        if a < b {
            bands.push((a, b));
        }
    }
    let results: Vec<Result<RowBuf>> = std::thread::scope(|s| {
        let hs: Vec<_> = bands
            .iter()
            .map(|&(a, b)| s.spawn(move || decode_group(cx, frame, pic, a, b, mb_width)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(Error::corrupt("ProRes: slice worker panicked"))))
            .collect()
    });
    let mut out = Vec::new();
    for (r, &(a, _)) in results.into_iter().zip(&bands) {
        out.push((a, r?));
    }
    Ok(out)
}

pub fn decode_frame(data: &[u8], threads: usize) -> Result<DecodedFrame> {
    let h = header::parse_frame(data)?;
    let mut info = h.info.clone();
    let frame = &data[..h.end];
    let (w, hgt) = (info.width as usize, info.height as usize);
    let mb_width = w.div_ceil(16);
    let interlaced = info.interlace != Interlace::Progressive;
    let mb_rows_frame = if interlaced { hgt.div_ceil(32) } else { hgt.div_ceil(16) };
    let npics = if interlaced { 2 } else { 1 };
    // Block storage is transposed (see `idct`), so the scan table is too.
    let mut scan_t = [0u8; 64];
    for (t, &at) in scan_t.iter_mut().zip(if interlaced { &INTERLACED_SCAN } else { &PROGRESSIVE_SCAN }) {
        *t = (at & 7) * 8 + (at >> 3);
    }
    let scan = &scan_t;
    let is444 = info.is_444;
    let cx = SliceCtx {
        is_444: is444,
        alpha_bits: info.alpha_bits,
        scan,
        qmat_luma: &h.qmat_luma,
        qmat_chroma: &h.qmat_chroma,
        b12: is444,
    };

    // Parse every picture header before allocating anything.
    let mut pics = Vec::new();
    let mut at = h.pictures_at;
    for i in 0..npics {
        let p = header::parse_picture(frame, at, frame.len())?;
        if i == 0 {
            info.log2_slice_mbs = p.log2_slice_mbs as u8;
        }
        // Every slice needs at least its 6-byte header: bounds the picture area by the data.
        if (p.end - p.data_at) < p.slice_count * 6 {
            return Err(Error::corrupt("ProRes picture: slice data smaller than the slice headers"));
        }
        pics.push(build_slices(frame, &p, mb_width, mb_rows_frame)?);
        at = p.end;
    }

    let ypw = mb_width * 16;
    let cpw = if is444 { ypw } else { mb_width * 8 };
    let total_rows = mb_rows_frame * 16 * npics; // padded frame height
    let bytes = |rows: usize, pw: usize| vec![0u8; rows * pw * 2];
    let (mut py, mut pu, mut pv) = (Vec::new(), Vec::new(), Vec::new());
    let mut pa: Vec<u16> = Vec::new();

    for (i, pic) in pics.iter().enumerate() {
        let groups = decode_picture(&cx, frame, pic, mb_width, threads)?;
        if npics == 1 && groups.len() == 1 {
            // One group covers the whole frame: its buffers are the planes.
            let (_, g) = groups.into_iter().next().unwrap();
            (py, pu, pv, pa) = (g.y, g.u, g.v, g.a);
            continue;
        }
        if i == 0 {
            py = bytes(total_rows, ypw);
            pu = bytes(total_rows, cpw);
            pv = bytes(total_rows, cpw);
            if info.alpha_bits != 0 {
                pa = vec![0; total_rows * ypw];
            }
        }
        // Field i lands on even lines when it is the top field, odd lines otherwise.
        let bottom = interlaced && ((i == 0) == (info.interlace == Interlace::BottomFieldFirst));
        for (r0, g) in groups {
            let rows = g.y.len() / (ypw * 2);
            for r in 0..rows {
                let line = (r0 * 16 + r) * npics + usize::from(bottom);
                py[line * ypw * 2..(line + 1) * ypw * 2].copy_from_slice(&g.y[r * ypw * 2..(r + 1) * ypw * 2]);
                pu[line * cpw * 2..(line + 1) * cpw * 2].copy_from_slice(&g.u[r * cpw * 2..(r + 1) * cpw * 2]);
                pv[line * cpw * 2..(line + 1) * cpw * 2].copy_from_slice(&g.v[r * cpw * 2..(r + 1) * cpw * 2]);
                if !pa.is_empty() {
                    pa[line * ypw..(line + 1) * ypw].copy_from_slice(&g.a[r * ypw..(r + 1) * ypw]);
                }
            }
        }
    }

    let format = if is444 { PixelFormat::I412 } else { PixelFormat::I210 };
    let planes = vec![Plane::new(py, ypw * 2), Plane::new(pu, cpw * 2), Plane::new(pv, cpw * 2)];
    let mut video = VideoFrame::try_new(format, info.width, info.height, planes)?;
    video.color = ColorInfo {
        primaries: info.primaries,
        transfer: info.transfer,
        matrix: info.matrix,
        full_range: false,
    };
    video.interlace = info.interlace;
    let alpha = (info.alpha_bits != 0).then_some(AlphaPlane {
        width: info.width,
        height: info.height,
        stride: ypw,
        data: pa,
    });
    Ok(DecodedFrame { video, alpha, info })
}
