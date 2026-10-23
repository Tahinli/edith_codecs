//! DVB subtitles (ETSI EN 300 743): bitmap subtitles in MPEG-TS private PES (and Matroska
//! `S_DVBSUB`).
//!
//! A packet is `0x20 0x00` (data identifier, stream id; absent in Matroska, accepted either
//! way) followed by segments `0x0F, type, page_id:u16, length:u16, body`, ended by `0xFF`.
//! Segments: page composition (0x10: which regions, where, timeout), region composition (0x11:
//! size, depth, CLUT, objects and their positions), CLUT definition (0x12), object data (0x13:
//! 2/4/8-bit pixel code strings, two interlaced fields), display definition (0x14: canvas size)
//! and end of display set (0x80).
//!
//! Output matches `ec-pgs`: one straight-alpha RGBA canvas per display set, transparent where
//! nothing is painted. An empty page produces an empty canvas (a "clear" for the compositor).
//! Colour follows ffmpeg's `dvbsubdec`: BT.601 limited-range Y'CrCb expanded to full range,
//! alpha = 255 - T, `Y = 0` transparent, and the spec's default CLUTs when none is sent.

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};

use ec_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, PixelFormat, Plane, Result, Timestamp,
    VideoFrame,
};

/// Largest canvas or region side accepted.
pub const MAX_DIMENSION: u32 = 4096;
/// Finished display sets kept for [`DvbSubDecoder::take_cue`].
pub const MAX_PENDING: usize = 32;

const SEG_PAGE: u8 = 0x10;
const SEG_REGION: u8 = 0x11;
const SEG_CLUT: u8 = 0x12;
const SEG_OBJECT: u8 = 0x13;
const SEG_DISPLAY: u8 = 0x14;
const SEG_END: u8 = 0x80;

/// One finished display set.
#[derive(Debug, Clone)]
pub struct DvbCue {
    /// The canvas, straight-alpha RGBA.
    pub frame: VideoFrame,
    /// Page timeout: how long the set stays up, in milliseconds (the page's timeout is seconds).
    pub timeout_ms: u32,
    /// No region is shown: this set clears the screen.
    pub empty: bool,
    /// Page state: 0 normal case, 1 acquisition point, 2 mode change.
    pub page_state: u8,
    /// The regions shown: x, y, width, height.
    pub regions: Vec<(u32, u32, u32, u32)>,
}

type Rgba = [u8; 4];

struct Clut {
    c2: [Rgba; 4],
    c4: [Rgba; 16],
    c8: [Rgba; 256],
}

struct Region {
    width: u32,
    height: u32,
    depth: u8, // bits: 2, 4, 8
    clut: u8,
    pix: Vec<u8>,
    objects: Vec<(u16, u32, u32)>, // id, x, y
}

/// A decoder of DVB subtitle packets.
pub struct DvbSubDecoder {
    params: CodecParameters,
    regions: HashMap<u8, Region>,
    cluts: HashMap<u8, Clut>,
    page: Vec<(u8, u32, u32)>,
    timeout: u8,
    page_state: u8,
    display: (u32, u32),
    ready: VecDeque<DvbCue>,
    pts: Option<Timestamp>,
}

impl Default for DvbSubDecoder {
    fn default() -> Self {
        DvbSubDecoder::new()
    }
}

fn default_clut() -> Clut {
    let mut c = Clut { c2: [[0; 4]; 4], c4: [[0; 4]; 16], c8: [[0; 4]; 256] };
    c.c2 = [[0, 0, 0, 0], [255, 255, 255, 255], [0, 0, 0, 255], [127, 127, 127, 255]];
    for i in 1..16usize {
        let bit = |m: usize, v: u8| if i & m != 0 { v } else { 0 };
        c.c4[i] = if i < 8 {
            [bit(1, 255), bit(2, 255), bit(4, 255), 255]
        } else {
            [bit(1, 127), bit(2, 127), bit(4, 127), 255]
        };
    }
    for i in 1..256usize {
        let f = |m: usize, v: u32| if i & m != 0 { v } else { 0 };
        let (r, g, b, a);
        if i < 8 {
            r = f(1, 255);
            g = f(2, 255);
            b = f(4, 255);
            a = 63;
        } else {
            match i & 0x88 {
                0x00 => {
                    r = f(1, 85) + f(0x10, 170);
                    g = f(2, 85) + f(0x20, 170);
                    b = f(4, 85) + f(0x40, 170);
                    a = 255;
                }
                0x08 => {
                    r = f(1, 85) + f(0x10, 170);
                    g = f(2, 85) + f(0x20, 170);
                    b = f(4, 85) + f(0x40, 170);
                    a = 127;
                }
                0x80 => {
                    r = 127 + f(1, 43) + f(0x10, 85);
                    g = 127 + f(2, 43) + f(0x20, 85);
                    b = 127 + f(4, 43) + f(0x40, 85);
                    a = 255;
                }
                _ => {
                    r = f(1, 43) + f(0x10, 85);
                    g = f(2, 43) + f(0x20, 85);
                    b = f(4, 43) + f(0x40, 85);
                    a = 255;
                }
            }
        }
        c.c8[i] = [r as u8, g as u8, b as u8, a as u8];
    }
    c
}

/// BT.601 limited-range Y'CrCb to full-range RGB, ffmpeg's `YUV_TO_RGB*_CCIR` in integers.
fn ycc_to_rgb(y: i32, cr: i32, cb: i32) -> [u8; 3] {
    let cb = cb - 128;
    let cr = cr - 128;
    let one_half = 1 << 15;
    let fix = |x: f64| (x * f64::from(1 << 16) + 0.5) as i32;
    let r_add = fix(1.402 * 255.0 / 224.0) * cr + one_half;
    let g_add = -fix(0.34414 * 255.0 / 224.0) * cb - fix(0.71414 * 255.0 / 224.0) * cr + one_half;
    let b_add = fix(1.772 * 255.0 / 224.0) * cb + one_half;
    let yy = (y - 16) * fix(255.0 / 219.0);
    let c = |v: i32| (v >> 16).clamp(0, 255) as u8;
    [c(yy + r_add), c(yy + g_add), c(yy + b_add)]
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?])))
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize, // bit position
}

impl Bits<'_> {
    fn get(&mut self, n: usize) -> Option<u32> {
        if self.pos + n > self.d.len() * 8 {
            return None;
        }
        let mut v = 0;
        for _ in 0..n {
            let b = (self.d[self.pos / 8] >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | u32::from(b);
            self.pos += 1;
        }
        Some(v)
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// One pixel-data sub-block's code string, calling `put(count, code)` for each run. Returns the
/// bytes the string took (through its end code and byte alignment; all of `d` if truncated).
fn code_string(d: &[u8], depth: u8, mut put: impl FnMut(usize, u8)) -> usize {
    let mut b = Bits { d, pos: 0 };
    loop {
        let r = (|| -> Option<Option<(usize, u8)>> {
            Some(match depth {
                2 => {
                    let v = b.get(2)?;
                    if v != 0 {
                        Some((1, v as u8))
                    } else if b.get(1)? == 1 {
                        let n = b.get(3)? as usize + 3;
                        Some((n, b.get(2)? as u8))
                    } else if b.get(1)? == 1 {
                        Some((1, 0))
                    } else {
                        match b.get(2)? {
                            0 => None,
                            1 => Some((2, 0)),
                            2 => {
                                let n = b.get(4)? as usize + 12;
                                Some((n, b.get(2)? as u8))
                            }
                            _ => {
                                let n = b.get(8)? as usize + 29;
                                Some((n, b.get(2)? as u8))
                            }
                        }
                    }
                }
                4 => {
                    let v = b.get(4)?;
                    if v != 0 {
                        Some((1, v as u8))
                    } else if b.get(1)? == 0 {
                        let n = b.get(3)? as usize;
                        if n == 0 { None } else { Some((n + 2, 0)) }
                    } else if b.get(1)? == 0 {
                        let n = b.get(2)? as usize + 4;
                        Some((n, b.get(4)? as u8))
                    } else {
                        match b.get(2)? {
                            0 => Some((1, 0)),
                            1 => Some((2, 0)),
                            2 => {
                                let n = b.get(4)? as usize + 9;
                                Some((n, b.get(4)? as u8))
                            }
                            _ => {
                                let n = b.get(8)? as usize + 25;
                                Some((n, b.get(4)? as u8))
                            }
                        }
                    }
                }
                _ => {
                    let v = b.get(8)?;
                    if v != 0 {
                        Some((1, v as u8))
                    } else if b.get(1)? == 0 {
                        let n = b.get(7)? as usize;
                        if n == 0 { None } else { Some((n, 0)) }
                    } else {
                        let n = b.get(7)? as usize;
                        Some((n, b.get(8)? as u8))
                    }
                }
            })
        })();
        match r {
            Some(Some((n, c))) => put(n, c),
            Some(None) => {
                b.align();
                return (b.pos / 8).min(d.len());
            }
            None => return d.len(),
        }
    }
}

impl DvbSubDecoder {
    /// A decoder with a 720x576 display and no state.
    pub fn new() -> DvbSubDecoder {
        DvbSubDecoder {
            params: CodecParameters::new(CodecId::DvbSub),
            regions: HashMap::new(),
            cluts: HashMap::new(),
            page: Vec::new(),
            timeout: 0,
            page_state: 0,
            display: (720, 576),
            ready: VecDeque::new(),
            pts: None,
        }
    }

    /// Feeds one PES payload (or Matroska block). Segments inside that are damaged are dropped;
    /// the first such error is returned after the rest were processed.
    pub fn push_pes(&mut self, data: &[u8]) -> Result<()> {
        let mut at = if data.len() >= 2 && data[0] == 0x20 && data[1] == 0x00 { 2 } else { 0 };
        let mut first = None;
        while at + 6 <= data.len() && data[at] == 0x0F {
            let kind = data[at + 1];
            let len = be16(data, at + 4).unwrap_or(0);
            let end = at + 6 + len;
            let Some(body) = data.get(at + 6..end) else {
                first.get_or_insert(Error::corrupt("dvbsub: segment past the packet"));
                break;
            };
            let r = match kind {
                SEG_PAGE => self.page_comp(body),
                SEG_REGION => self.region_comp(body),
                SEG_CLUT => self.clut_def(body),
                SEG_OBJECT => self.object_data(body),
                SEG_DISPLAY => self.display_def(body),
                SEG_END => self.end_set(),
                _ => Ok(()),
            };
            if let Err(e) = r {
                first.get_or_insert(e);
            }
            at = end;
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The next finished display set.
    pub fn take_cue(&mut self) -> Option<DvbCue> {
        self.ready.pop_front()
    }

    fn display_def(&mut self, b: &[u8]) -> Result<()> {
        let w = be16(b, 1).ok_or_else(|| Error::corrupt("dvbsub: short display definition"))? as u32 + 1;
        let h = be16(b, 3).ok_or_else(|| Error::corrupt("dvbsub: short display definition"))? as u32 + 1;
        if w > MAX_DIMENSION || h > MAX_DIMENSION {
            return Err(Error::corrupt("dvbsub: display past the dimension limit"));
        }
        self.display = (w, h);
        Ok(())
    }

    fn page_comp(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 2 {
            return Err(Error::corrupt("dvbsub: short page composition"));
        }
        self.timeout = b[0];
        self.page_state = (b[1] >> 2) & 3;
        if self.page_state != 0 {
            // Acquisition point / mode change: nothing from before is kept.
            self.regions.clear();
            self.cluts.clear();
        }
        self.page.clear();
        let mut at = 2;
        while at + 6 <= b.len() {
            let x = be16(b, at + 2).unwrap_or(0) as u32;
            let y = be16(b, at + 4).unwrap_or(0) as u32;
            self.page.push((b[at], x, y));
            at += 6;
        }
        Ok(())
    }

    fn region_comp(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 10 {
            return Err(Error::corrupt("dvbsub: short region composition"));
        }
        let id = b[0];
        let fill = b[1] & 0x08 != 0;
        let w = be16(b, 2).unwrap() as u32;
        let h = be16(b, 4).unwrap() as u32;
        if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
            return Err(Error::corrupt("dvbsub: region size out of range"));
        }
        let depth = match (b[6] >> 2) & 7 {
            1 => 2,
            2 => 4,
            3 => 8,
            _ => return Err(Error::corrupt("dvbsub: bad region depth")),
        };
        let clut = b[7];
        let bg = match depth {
            2 => (b[9] >> 2) & 3,
            4 => b[9] >> 4,
            _ => b[8],
        };
        let mut pix = vec![0u8; (w * h) as usize];
        if fill {
            pix.fill(bg);
        }
        let mut objects = Vec::new();
        let mut at = 10;
        while at + 6 <= b.len() {
            let oid = be16(b, at).unwrap() as u16;
            let kind = b[at + 2] >> 6;
            let x = (be16(b, at + 2).unwrap() & 0xFFF) as u32;
            let y = (be16(b, at + 4).unwrap() & 0xFFF) as u32;
            objects.push((oid, x, y));
            at += if kind == 1 || kind == 2 { 8 } else { 6 };
        }
        self.regions.insert(id, Region { width: w, height: h, depth, clut, pix, objects });
        Ok(())
    }

    fn clut_def(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 2 {
            return Err(Error::corrupt("dvbsub: short CLUT"));
        }
        let clut = self.cluts.entry(b[0]).or_insert_with(default_clut);
        let mut at = 2;
        while at + 2 <= b.len() {
            let id = usize::from(b[at]);
            let flags = b[at + 1];
            at += 2;
            let (y, cr, cb, t);
            if flags & 1 != 0 {
                let Some(e) = b.get(at..at + 4) else { break };
                y = i32::from(e[0]);
                cr = i32::from(e[1]);
                cb = i32::from(e[2]);
                t = i32::from(e[3]);
                at += 4;
            } else {
                let Some(e) = b.get(at..at + 2) else { break };
                y = i32::from(e[0] & 0xFC);
                cr = i32::from((((e[0] & 3) << 2) | (e[1] >> 6)) << 4);
                cb = i32::from((e[1] << 2) & 0xF0);
                t = i32::from((e[1] << 6) & 0xC0);
                at += 2;
            }
            let rgba = if y == 0 {
                [0, 0, 0, 0]
            } else {
                let rgb = ycc_to_rgb(y, cr, cb);
                [rgb[0], rgb[1], rgb[2], (255 - t) as u8]
            };
            if flags & 0x80 != 0 && id < 4 {
                clut.c2[id] = rgba;
            }
            if flags & 0x40 != 0 && id < 16 {
                clut.c4[id] = rgba;
            }
            if flags & 0x20 != 0 {
                clut.c8[id] = rgba;
            }
        }
        Ok(())
    }

    fn object_data(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 3 {
            return Err(Error::corrupt("dvbsub: short object"));
        }
        let oid = be16(b, 0).unwrap() as u16;
        let method = (b[2] >> 2) & 3;
        if method != 0 {
            return Ok(()); // character-coded objects are not bitmaps
        }
        let top_len = be16(b, 3).ok_or_else(|| Error::corrupt("dvbsub: short object"))?;
        let bot_len = be16(b, 5).ok_or_else(|| Error::corrupt("dvbsub: short object"))?;
        let top = b.get(7..7 + top_len).ok_or_else(|| Error::corrupt("dvbsub: field past object"))?;
        let bot = if bot_len == 0 {
            top
        } else {
            b.get(7 + top_len..7 + top_len + bot_len)
                .ok_or_else(|| Error::corrupt("dvbsub: field past object"))?
        };
        for region in self.regions.values_mut() {
            let placed: Vec<(u32, u32)> = region
                .objects
                .iter()
                .filter(|o| o.0 == oid)
                .map(|o| (o.1, o.2))
                .collect();
            for (x, y) in placed {
                paint_field(region, top, x, y, 0);
                paint_field(region, bot, x, y, 1);
            }
        }
        Ok(())
    }

    fn end_set(&mut self) -> Result<()> {
        let (cw, ch) = self.display;
        let stride = cw as usize * 4;
        let mut px = vec![0u8; stride * ch as usize];
        let mut shown = Vec::new();
        let def = default_clut();
        for &(rid, rx, ry) in &self.page {
            let Some(r) = self.regions.get(&rid) else { continue };
            let clut = self.cluts.get(&r.clut).unwrap_or(&def);
            shown.push((rx, ry, r.width, r.height));
            for row in 0..r.height {
                let ty = ry + row;
                if ty >= ch {
                    break;
                }
                for col in 0..r.width {
                    let tx = rx + col;
                    if tx >= cw {
                        break;
                    }
                    let code = usize::from(r.pix[(row * r.width + col) as usize]);
                    let c = match r.depth {
                        2 => clut.c2[code & 3],
                        4 => clut.c4[code & 15],
                        _ => clut.c8[code],
                    };
                    let o = ty as usize * stride + tx as usize * 4;
                    px[o..o + 4].copy_from_slice(&c);
                }
            }
        }
        let frame = VideoFrame::try_new(PixelFormat::Rgba8, cw, ch, vec![Plane::new(px, stride)])?;
        if self.ready.len() >= MAX_PENDING {
            self.ready.pop_front();
        }
        let mut frame = frame;
        frame.pts = self.pts;
        self.ready.push_back(DvbCue {
            frame,
            timeout_ms: u32::from(self.timeout) * 1000,
            empty: shown.is_empty(),
            page_state: self.page_state,
            regions: shown,
        });
        Ok(())
    }
}

/// Paints the sub-blocks of one field of an object into `region` at (`x`, `y`), starting on
/// line `field` (0 top, 1 bottom) and stepping two lines.
fn paint_field(region: &mut Region, data: &[u8], x: u32, y: u32, field: u32) {
    let (w, h) = (region.width as usize, region.height as usize);
    let mut row = (y + field) as usize;
    let mut col = x as usize;
    let mut m24 = [0u8, 7, 8, 15];
    let mut m28 = [0u8, 0x77, 0x88, 0xFF];
    let mut m48: [u8; 16] = std::array::from_fn(|i| (i * 0x11) as u8);
    let mut at = 0;
    while at < data.len() {
        let kind = data[at];
        at += 1;
        match kind {
            0x10..=0x12 => {
                let src_depth = match kind {
                    0x10 => 2,
                    0x11 => 4,
                    _ => 8,
                };
                let depth = region.depth;
                let (m24c, m28c, m48c) = (m24, m28, m48);
                let mapped = |c: u8| -> u8 {
                    match (src_depth, depth) {
                        (2, 4) => m24c[usize::from(c & 3)],
                        (2, 8) => m28c[usize::from(c & 3)],
                        (4, 8) => m48c[usize::from(c & 15)],
                        _ => c,
                    }
                };
                let used = code_string(&data[at..], src_depth, |n, c| {
                    if src_depth > depth || row >= h {
                        return;
                    }
                    let c = mapped(c);
                    for _ in 0..n {
                        if col < w {
                            region.pix[row * w + col] = c;
                        }
                        col += 1;
                    }
                });
                let end = at + used;
                at = end;
            }
            0x20 => {
                if let Some(t) = data.get(at..at + 2) {
                    m24 = [t[0] >> 4, t[0] & 15, t[1] >> 4, t[1] & 15];
                }
                at += 2;
            }
            0x21 => {
                if let Some(t) = data.get(at..at + 4) {
                    m28 = [t[0], t[1], t[2], t[3]];
                }
                at += 4;
            }
            0x22 => {
                if let Some(t) = data.get(at..at + 16) {
                    m48.copy_from_slice(t);
                }
                at += 16;
            }
            0xF0 => {
                row += 2;
                col = x as usize;
            }
            _ => return,
        }
    }
}

impl Decoder for DvbSubDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &ec_core::Packet) -> Result<()> {
        self.pts = packet.pts.map(|t| Timestamp::new(t, packet.time_base));
        self.push_pes(&packet.data)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.take_cue() {
            Some(c) => Ok(Frame::Video(c.frame)),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) {
        *self = DvbSubDecoder::new();
    }
}

/// A DVB subtitle decoder as the family's registry hands one out.
pub fn make_decoder(_params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(DvbSubDecoder::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0x0F, kind, 0, 1];
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn set(depth_code: u8, string_type: u8, string: &[u8]) -> Vec<u8> {
        let mut p = vec![0x20, 0x00];
        p.extend(seg(SEG_PAGE, &[5, 0x08, 0, 0, 0, 0, 0, 0]));
        // region 0: 4x1, depth per `depth_code` (1: 2 bit, 2: 4 bit, 3: 8 bit), clut 0, object 7 at (0,0)
        p.extend(seg(SEG_REGION, &[0, 0, 0, 4, 0, 1, depth_code << 2, 0, 0, 0, 0, 7, 0, 0, 0, 0]));
        let mut obj = vec![0, 7, 0];
        let field = [&[string_type][..], string, &[0xF0]].concat();
        obj.extend_from_slice(&(field.len() as u16).to_be_bytes());
        obj.extend_from_slice(&[0, 0]);
        obj.extend_from_slice(&field);
        p.extend(seg(SEG_OBJECT, &obj));
        p.extend(seg(SEG_END, &[]));
        p.push(0xFF);
        p
    }

    fn row(d: &mut DvbSubDecoder) -> Vec<[u8; 4]> {
        let c = d.take_cue().expect("a display set");
        c.frame.planes[0].data[..16].chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
    }

    #[test]
    fn two_bit_runs_use_the_default_clut() {
        let mut d = DvbSubDecoder::new();
        // pixels 1,1,1,2 then end of string.
        d.push_pes(&set(1, 0x10, &[0x56, 0x00])).unwrap();
        let r = row(&mut d);
        assert_eq!(r, vec![[255, 255, 255, 255], [255, 255, 255, 255], [255, 255, 255, 255], [0, 0, 0, 255]]);
    }

    #[test]
    fn four_and_eight_bit_strings_decode_runs() {
        let mut d = DvbSubDecoder::new();
        // 4-bit: pixel 1, then run of 3 zeros (0000 0 011 -> n=3, run n+2=5?): use 0,0,001 -> 3 px.
        // bits: 0001 | 0000 0 001 | end 0000 0 000 -> 0x10, 0x01, 0x00
        d.push_pes(&set(2, 0x11, &[0x10, 0x01, 0x00])).unwrap();
        let r = row(&mut d);
        assert_eq!(r[0], [255, 0, 0, 255]); // 4-bit default entry 1: red? bit0 -> r
        assert_eq!(r[1][3], 0);
        // 8-bit: pixel 9, pixel 9, then end (00 + 0 + 0000000).
        d.push_pes(&set(3, 0x12, &[9, 9, 0, 0])).unwrap();
        let r = row(&mut d);
        assert_eq!(r[0], r[1]);
        assert_ne!(r[0][3], 0);
    }

    #[test]
    fn rubbish_never_panics() {
        let mut x = 0x1234_5678u32;
        let mut d = DvbSubDecoder::new();
        for n in 0..2000 {
            let len = (n % 300) + 1;
            let mut buf: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 24) as u8
                })
                .collect();
            if n % 2 == 0 {
                buf[0] = 0x0F;
                if buf.len() > 1 {
                    buf[1] = 0x10 + (n % 6) as u8;
                }
            }
            let _ = d.push_pes(&buf);
            while d.take_cue().is_some() {}
        }
    }
}
