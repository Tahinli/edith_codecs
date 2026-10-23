//! Frame header parsing, macroblock-row decoding and picture assembly.

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Interlace, PixelFormat, Plane, VideoFrame};

use crate::bits::BitReader;
use crate::idct::{Block, IdctPut, idct_for};
use crate::tables::{CodeSet, FLAG_MBAFF, ZIGZAG, cid_entry};

/// Chroma layout of a decoded picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chroma {
    /// 4:2:2 (half-width chroma).
    C422,
    /// 4:4:4.
    C444,
}

/// What the frame header said about a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    /// Compression id (1235-1260 DNxHD, 1270-1274 DNxHR).
    pub cid: u32,
    /// Coded sample depth: 8, 10 or 12.
    pub bit_depth: u32,
    /// Chroma layout.
    pub chroma: Chroma,
    /// Picture width in pixels (after the CID's own width override).
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// Two coded fields.
    pub interlaced: bool,
    /// The top field comes first (meaningful when `interlaced`).
    pub top_field_first: bool,
    /// Adaptive macroblock interlace (CID 1260).
    pub mbaff: bool,
    /// Sample aspect ratio implied by a CID that stores fewer columns than the
    /// header's width (1440 of 1920, 960 of 1280), as `(num, den)`.
    pub sample_aspect_ratio: Option<(u32, u32)>,
    /// `matrix_coefficients` code point of the header's colour field.
    pub matrix: u8,
}

/// A decoded picture at its coded bit depth: three tightly packed planes of
/// `u16` samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Header facts.
    pub info: FrameInfo,
    /// Y, Cb, Cr; `info.width` (or half, rounded up, for 4:2:2 chroma) samples per row.
    pub planes: [Vec<u16>; 3],
    /// Samples per row of each plane.
    pub strides: [usize; 3],
}

/// Largest picture area decoded (8192x4608).
const MAX_PIXELS: u64 = 8192 * 4608;

struct Header {
    info: FrameInfo,
    cur_field: usize,
    mb_width: usize,
    mb_height: usize,
    data_offset: usize,
    coding_unit_size: usize,
    scan: Vec<u32>,
    code_set_key: (u32, u32),
    hqx: bool,
}

fn be16(b: &[u8], o: usize) -> u32 {
    u32::from(b[o]) << 8 | u32::from(b[o + 1])
}
fn be32(b: &[u8], o: usize) -> u32 {
    be16(b, o) << 16 | be16(b, o + 2)
}

fn parse_header(buf: &[u8], first_field: bool, prev_field: usize) -> Result<Header> {
    if buf.len() < 0x280 {
        return Err(Error::corrupt(format!("DNxHD frame too small ({} < 640)", buf.len())));
    }
    let prefix = u64::from(be32(buf, 0)) << 16 | u64::from(buf[4]) << 8;
    let hr = {
        let off = prefix >> 16;
        (prefix & 0xFFFF_0000_FFFF) == 0x0300 && (0x0280..=0x2170).contains(&off) && off & 3 == 0
    };
    if !(prefix == 0x0000_0280_0100 || prefix == 0x0000_0280_0200 || hr) {
        return Err(Error::corrupt("DNxHD: unknown header prefix"));
    }
    let interlaced = buf[5] & 2 != 0;
    let cur_field = if interlaced {
        if first_field { usize::from(buf[5] & 1) } else { 1 - prev_field }
    } else {
        0
    };
    let top_first = interlaced && ((usize::from(first_field)) ^ cur_field) != 0;
    let mbaff = (buf[6] >> 5) & 1 != 0;
    if buf[7] & 1 != 0 {
        return Err(Error::unsupported(
            "DNxHD alpha channel",
            "streams with an alpha plane are refused",
        ));
    }
    let mut height = be16(buf, 0x18);
    let mut width = be16(buf, 0x1a);
    let bit_depth = match buf[0x21] >> 5 {
        1 => 8,
        2 => 10,
        3 => 12,
        d => return Err(Error::corrupt(format!("DNxHD: unknown bit depth indicator {d}"))),
    };
    let cid = be32(buf, 0x28);
    let entry = cid_entry(cid)
        .ok_or_else(|| Error::unsupported(format!("DNxHD compression id {cid}"), "unknown cid"))?;
    if entry.bit_depth != bit_depth && entry.bit_depth != 0 {
        return Err(Error::corrupt(format!(
            "DNxHD: bit depth {bit_depth} does not match cid {cid}"
        )));
    }
    let matrix = match (buf[0x2c] >> 1) & 3 {
        0 => 1,
        1 => 9,
        2 => 10,
        _ => 2,
    };
    let act = buf[0x2c] & 1 != 0;
    let is_444 = (buf[0x2c] >> 6) & 1 != 0;
    if is_444 {
        if bit_depth == 8 {
            return Err(Error::unsupported("DNxHD 4:4:4 8-bit", "no such profile"));
        }
        if !act {
            return Err(Error::unsupported(
                "DNxHR 4:4:4 RGB (GBR planes)",
                "only YCbCr 4:4:4 (ACT flag set, as ffmpeg writes it) is decoded",
            ));
        }
    } else if act {
        return Err(Error::unsupported(
            "DNxHD adaptive colour transform",
            "the ACT variants are refused",
        ));
    }
    let hqx = cid == 1271;
    let mut sar = None;
    if entry.width != 0 && width != entry.width {
        let g = gcd(width, entry.width);
        sar = Some((width / g, entry.width / g));
        width = entry.width;
    }
    if buf.len() < entry.coding_unit_size as usize {
        return Err(Error::corrupt("DNxHD: incorrect frame size"));
    }
    let mb_width = width.div_ceil(16) as usize;
    let mb_height = be16(buf, 0x16c) as usize;
    if height.div_ceil(16) as usize == mb_height && interlaced {
        height <<= 1;
    }
    let data_offset = if mb_height > 68 && hr {
        0x170 + (mb_height << 2)
    } else {
        if mb_height > 68 {
            return Err(Error::corrupt("DNxHD: mb height too big"));
        }
        0x280
    };
    if (mb_height << usize::from(interlaced)) > height.div_ceil(16) as usize {
        return Err(Error::corrupt("DNxHD: mb height too big"));
    }
    if width == 0 || height == 0 || mb_height == 0 {
        return Err(Error::corrupt("DNxHD: empty picture"));
    }
    // Allocation bound for hostile headers (8K UHD is the largest picture the
    // format is used for).
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::unsupported(
            format!("DNxHD picture {width}x{height}"),
            "larger than 8192x4608",
        ));
    }
    if buf.len() < data_offset {
        return Err(Error::corrupt("DNxHD: buffer too small for the scan index"));
    }
    let mut scan = Vec::with_capacity(mb_height);
    for i in 0..mb_height {
        let v = be32(buf, 0x170 + 4 * i);
        if ((buf.len() - data_offset) as u64) < u64::from(v) {
            return Err(Error::corrupt("DNxHD: invalid mb scan index"));
        }
        scan.push(v);
    }
    Ok(Header {
        info: FrameInfo {
            cid,
            bit_depth,
            chroma: if is_444 { Chroma::C444 } else { Chroma::C422 },
            width,
            height,
            interlaced,
            top_field_first: top_first,
            mbaff,
            sample_aspect_ratio: sar,
            matrix,
        },
        cur_field,
        mb_width,
        mb_height,
        data_offset,
        coding_unit_size: entry.coding_unit_size as usize,
        scan,
        code_set_key: (cid, bit_depth),
        hqx,
    })
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Per-bit-depth dequantisation parameters of the coefficient decoder.
#[derive(Clone, Copy)]
struct Quant {
    index_bits: u32,
    level_bias: i32,
    level_shift: u32,
    dc_shift: u32,
}

fn quant_for(info: &FrameInfo, hqx: bool) -> Quant {
    let q = |index_bits, level_bias, level_shift, dc_shift| Quant {
        index_bits,
        level_bias,
        level_shift,
        dc_shift,
    };
    match (info.chroma, info.bit_depth) {
        (Chroma::C444, 10) => q(6, 32, 6, 0),
        (Chroma::C444, _) => q(6, 32, 4, 2),
        (Chroma::C422, 12) => q(6, 8, 4, 2),
        (Chroma::C422, 10) if hqx => q(6, 32, 6, 0),
        (Chroma::C422, 10) => q(6, 8, 4, 0),
        _ => q(4, 32, 6, 0),
    }
}

/// Everything a row decoder reads and shares between rows.
struct RowShared<'a> {
    codes: &'a CodeSet,
    quant: Quant,
    is_444: bool,
    mbaff: bool,
    interlaced: bool,
    cur_field: usize,
    mb_width: usize,
    bit_depth: u32,
    idct: IdctPut,
    /// Bytes per sample (1 or 2).
    bps: usize,
    /// Row pitch of each plane, in samples.
    strides: [usize; 3],
}

struct RowState {
    last_dc: [i32; 3],
    last_qscale: i32,
    luma_scale: [i32; 64],
    chroma_scale: [i32; 64],
}

fn corrupt(what: &str) -> Error {
    Error::corrupt(format!("DNxHD: {what}"))
}

fn decode_block(
    sh: &RowShared,
    st: &mut RowState,
    r: &mut BitReader,
    n: usize,
    block: &mut Block,
) -> Result<usize> {
    *block = [[0; 8]; 8];
    let cid = sh.codes.cid;
    let component = if sh.is_444 { (n >> 1) % 3 } else if n & 2 != 0 { 1 + (n & 1) } else { 0 };
    let (scale, weights) = if component != 0 {
        (&st.chroma_scale, cid.chroma_weight)
    } else {
        (&st.luma_scale, cid.luma_weight)
    };
    let q = sh.quant;
    r.refill();
    let len = sh.codes.dc.get(r).ok_or_else(|| corrupt("bad dc code"))?;
    if len != 0 {
        let v = r.get(len) as i32;
        let level = if v >> (len - 1) != 0 { v } else { v - ((1 << len) - 1) };
        st.last_dc[component] = st.last_dc[component].wrapping_add(level.wrapping_shl(q.dc_shift));
    }
    block[0][0] = st.last_dc[component] as i16;
    let eob = cid.eob_index;
    let ac_info = cid.ac_info;
    let mut i = 0usize;
    loop {
        r.refill();
        let index1 = sh.codes.ac.get(r).ok_or_else(|| corrupt("bad ac code"))?;
        if index1 == eob {
            break;
        }
        let mut level = i32::from(ac_info[2 * index1 as usize]);
        let flags = ac_info[2 * index1 as usize + 1];
        let sign = -(r.get(1) as i32);
        if flags & 1 != 0 {
            level += (r.get(q.index_bits) as i32) << 7;
        }
        if flags & 2 != 0 {
            let run = sh.codes.run.get(r).ok_or_else(|| corrupt("bad run code"))?;
            i += run as usize;
        }
        i += 1;
        if i > 63 {
            return Err(corrupt("ac coefficient overrun"));
        }
        let j = ZIGZAG[i] as usize & 63;
        level = level.wrapping_mul(scale[i]);
        level = level.wrapping_add(scale[i] >> 1);
        if q.level_bias < 32 || i32::from(weights[i]) != q.level_bias {
            level = level.wrapping_add(q.level_bias);
        }
        level >>= q.level_shift;
        block[j >> 3][j & 7] = ((level ^ sign) - sign) as i16;
    }
    Ok(i)
}

/// Decode macroblock row `data` (starting at the row's bit offset) into the
/// plane windows `win` (one MB-row high: 16 lines, 32 for interlaced).
fn decode_row(sh: &RowShared, data: &[u8], win: &mut [&mut [u8]; 3]) -> Result<()> {
    let mut r = BitReader::new(data);
    let mut st = RowState {
        last_dc: [1 << (sh.bit_depth + 2); 3],
        last_qscale: 0,
        luma_scale: [0; 64],
        chroma_scale: [0; 64],
    };
    let cid = sh.codes.cid;
    let nblocks = if sh.is_444 { 12 } else { 8 };
    let mut blocks = [[[0i16; 8]; 8]; 12];
    let mut last = [0usize; 12];
    for x in 0..sh.mb_width {
        r.refill();
        let (interlaced_mb, qscale) = if sh.mbaff {
            (r.get(1) != 0, r.get(10) as i32)
        } else {
            (false, r.get(11) as i32)
        };
        let _act = r.get(1);
        if qscale != st.last_qscale {
            for i in 0..64 {
                st.luma_scale[i] = qscale * i32::from(cid.luma_weight[i]);
                st.chroma_scale[i] = qscale * i32::from(cid.chroma_weight[i]);
            }
            st.last_qscale = qscale;
        }
        for n in 0..nblocks {
            last[n] = decode_block(sh, &mut st, &mut r, n, &mut blocks[n])?;
        }
        if r.overrun() {
            return Err(corrupt("macroblock data past the end"));
        }
        // Destination geometry, mirroring the reference layout.
        let mut dls = sh.strides;
        let mut base = [0usize; 3];
        if sh.interlaced {
            for p in 0..3 {
                dls[p] <<= 1;
                if sh.cur_field != 0 {
                    base[p] = sh.strides[p];
                }
            }
        }
        let yoff = |p: usize, il: bool, dls: &[usize; 3]| if il { sh.strides[p] } else { dls[p] << 3 };
        if interlaced_mb {
            for d in &mut dls {
                *d <<= 1;
            }
        }
        let xl = x * 16;
        let xc = if sh.is_444 { x * 16 } else { x * 8 };
        let bps = sh.bps;
        let mut put = |win: &mut [&mut [u8]; 3], p: usize, n: usize, off: usize| {
            (sh.idct)(&mut blocks[n], &mut win[p][off * bps..], dls[p] * bps, last[n]);
        };
        let yo = yoff(0, interlaced_mb, &dls);
        let co = yoff(1, interlaced_mb, &dls);
        let b0 = base[0] + xl;
        let b1 = base[1] + xc;
        let b2 = base[2] + xc;
        if !sh.is_444 {
            put(win, 0, 0, b0);
            put(win, 0, 1, b0 + 8);
            put(win, 0, 4, b0 + yo);
            put(win, 0, 5, b0 + yo + 8);
            put(win, 1, 2, b1);
            put(win, 2, 3, b2);
            put(win, 1, 6, b1 + co);
            put(win, 2, 7, b2 + co);
        } else {
            put(win, 0, 0, b0);
            put(win, 0, 1, b0 + 8);
            put(win, 0, 6, b0 + yo);
            put(win, 0, 7, b0 + yo + 8);
            put(win, 1, 2, b1);
            put(win, 1, 3, b1 + 8);
            put(win, 1, 8, b1 + co);
            put(win, 1, 9, b1 + co + 8);
            put(win, 2, 4, b2);
            put(win, 2, 5, b2 + 8);
            put(win, 2, 10, b2 + co);
            put(win, 2, 11, b2 + co + 8);
        }
    }
    Ok(())
}

/// A decoded frame as the decoder holds it: whole-macroblock planes of 8-bit
/// samples or little-endian 16-bit samples (`bps` bytes each).
pub(crate) struct Canvas {
    pub info: FrameInfo,
    pub planes: [Vec<u8>; 3],
    /// Row pitch in bytes.
    pub strides: [usize; 3],
    pub bps: usize,
}

/// Decoder state kept between frames: the code tables of the last CID.
pub(crate) struct Core {
    codes: Option<CodeSet>,
    pub threads: usize,
}

impl Core {
    pub fn new(threads: usize) -> Core {
        Core { codes: None, threads: threads.clamp(1, 6) }
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Canvas> {
        let mut hdr = parse_header(data, true, 0)?;
        let info = hdr.info.clone();
        let is_444 = info.chroma == Chroma::C444;
        let bps = if info.bit_depth > 8 { 2 } else { 1 };
        // Canvas rows: whole macroblock rows of the whole frame (the header
        // only guarantees the coded rows do not exceed these).
        let rows = (info.height as usize).div_ceil(16) * 16;
        let cstride = hdr.mb_width * if is_444 { 16 } else { 8 };
        let ystride = hdr.mb_width * 16;
        let strides = [ystride, cstride, cstride];
        let mut planes: [Vec<u8>; 3] = [
            vec![0; ystride * bps * rows],
            vec![0; cstride * bps * rows],
            vec![0; cstride * bps * rows],
        ];
        let units = if info.interlaced { 2 } else { 1 };
        let mut off = 0usize;
        for u in 0..units {
            if u == 1 {
                off += hdr.coding_unit_size;
                if off >= data.len() {
                    return Err(corrupt("second field missing"));
                }
                let h2 = parse_header(&data[off..], false, hdr.cur_field)?;
                if h2.info.cid != info.cid
                    || h2.info.width != info.width
                    || h2.info.bit_depth != info.bit_depth
                    || h2.info.chroma != info.chroma
                    || h2.mb_height != hdr.mb_height
                {
                    return Err(corrupt("second field header differs from the first"));
                }
                hdr = h2;
            }
            self.decode_unit(&hdr, &data[off..], &info, strides, bps, &mut planes)?;
        }
        Ok(Canvas { info, planes, strides: strides.map(|s| s * bps), bps })
    }

    fn decode_unit(
        &mut self,
        hdr: &Header,
        unit: &[u8],
        info: &FrameInfo,
        strides: [usize; 3],
        bps: usize,
        canvas: &mut [Vec<u8>; 3],
    ) -> Result<()> {
        if self.codes.as_ref().map(|c| (c.cid.cid, c.bit_depth)) != Some(hdr.code_set_key) {
            let e = cid_entry(hdr.code_set_key.0).expect("cid validated by the header");
            self.codes = Some(CodeSet::new(e, hdr.code_set_key.1));
        }
        let codes = self.codes.as_ref().unwrap();
        if info.mbaff && codes.cid.flags & FLAG_MBAFF == 0 {
            // The reference decoder only warns; so do we, by decoding as stated.
        }
        let sh = RowShared {
            codes,
            quant: quant_for(info, hdr.hqx),
            is_444: info.chroma == Chroma::C444,
            mbaff: info.mbaff,
            interlaced: info.interlaced,
            cur_field: hdr.cur_field,
            mb_width: hdr.mb_width,
            bit_depth: info.bit_depth,
            idct: idct_for(info.bit_depth),
            bps,
            strides,
        };
        let body = &unit[hdr.data_offset..];
        let lines = if info.interlaced { 32 } else { 16 };
        let [cy, cu, cv] = canvas;
        let mut work: Vec<(usize, [&mut [u8]; 3])> = cy
            .chunks_mut(lines * strides[0] * bps)
            .zip(cu.chunks_mut(lines * strides[1] * bps))
            .zip(cv.chunks_mut(lines * strides[2] * bps))
            .take(hdr.mb_height)
            .enumerate()
            .map(|(y, ((a, b), c))| (y, [a, b, c]))
            .collect();
        let nthreads = self.threads.min(work.len()).max(1);
        let run = |items: &mut [(usize, [&mut [u8]; 3])]| -> Result<()> {
            for (y, win) in items.iter_mut() {
                let start = hdr.scan[*y] as usize;
                decode_row(&sh, &body[start..], win)?;
            }
            Ok(())
        };
        if nthreads == 1 {
            return run(&mut work);
        }
        // Rows are handed out one at a time, so a thread that gets descheduled
        // or draws dense rows does not hold the others back.
        let queue = std::sync::Mutex::new(work.into_iter());
        let mut result = Ok(());
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..nthreads)
                .map(|_| {
                    let (queue, sh) = (&queue, &sh);
                    s.spawn(move || -> Result<()> {
                        loop {
                            let item = queue.lock().ok().and_then(|mut q| q.next());
                            let Some((y, mut win)) = item else { return Ok(()) };
                            let start = hdr.scan[y] as usize;
                            decode_row(sh, &body[start..], &mut win)?;
                        }
                    })
                })
                .collect();
            for h in handles {
                if let Err(e) = h.join().unwrap_or_else(|_| Err(corrupt("worker panicked"))) {
                    result = Err(e);
                }
            }
        });
        result
    }
}

/// Visible-area planes at native depth as `u16`.
pub(crate) fn to_picture(c: Canvas) -> Picture {
    let info = c.info;
    let (w, h) = (info.width as usize, info.height as usize);
    let cw = if info.chroma == Chroma::C444 { w } else { w.div_ceil(2) };
    let widths = [w, cw, cw];
    let planes = std::array::from_fn(|p| {
        let mut out = Vec::with_capacity(widths[p] * h);
        for y in 0..h {
            let row = &c.planes[p][y * c.strides[p]..y * c.strides[p] + widths[p] * c.bps];
            if c.bps == 1 {
                out.extend(row.iter().map(|&v| u16::from(v)));
            } else {
                out.extend(row.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])));
            }
        }
        out
    });
    Picture { info, planes, strides: widths }
}

/// An [`ec_core::frame::VideoFrame`] from a decoded canvas: 8- and 10-bit
/// samples are used as they are, 12-bit ones narrowed to 10 (`(v + 2) >> 2`).
pub(crate) fn to_video_frame(mut c: Canvas) -> Result<VideoFrame> {
    let info = c.info.clone();
    let is_444 = info.chroma == Chroma::C444;
    let format = match (is_444, info.bit_depth) {
        (false, 8) => PixelFormat::I422,
        (false, _) => PixelFormat::I210,
        (true, 8) => PixelFormat::I444,
        (true, _) => PixelFormat::I410,
    };
    if info.bit_depth == 12 {
        for p in &mut c.planes {
            for b in p.chunks_exact_mut(2) {
                let v = (u32::from(u16::from_le_bytes([b[0], b[1]])) + 2) >> 2;
                b.copy_from_slice(&(v.min(1023) as u16).to_le_bytes());
            }
        }
    }
    let planes = c
        .planes
        .into_iter()
        .zip(c.strides)
        .map(|(data, stride)| Plane::new(data, stride))
        .collect();
    let mut f = VideoFrame::try_new(format, info.width, info.height, planes)?;
    f.color = ColorInfo { primaries: 2, transfer: 2, matrix: info.matrix, full_range: false };
    f.interlace = if !info.interlaced {
        Interlace::Progressive
    } else if info.top_field_first {
        Interlace::TopFieldFirst
    } else {
        Interlace::BottomFieldFirst
    };
    Ok(f)
}
