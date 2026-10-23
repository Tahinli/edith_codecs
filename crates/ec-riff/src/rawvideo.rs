//! Uncompressed video in AVI: `BI_RGB` 24/32-bit (bottom-up or, with a negative height,
//! top-down; rows padded to 4 bytes) and the common YUV FourCCs YUY2/YUYV, UYVY, I420/IYUV and
//! YV12. The demuxer keeps the `BITMAPINFOHEADER` as the stream's extradata; [`RawVideoDecoder`]
//! reads the layout from it and turns each packet into an upright [`VideoFrame`]:
//! RGB comes out as `Bgr8` / `Bgra8`, packed 4:2:2 as `I422` and YV12 as `I420` (lossless
//! repacking, byte-equal to ffmpeg's `-pix_fmt` conversion of the same data).

#![allow(clippy::chunks_exact_to_as_chunks)]

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, PixelFormat, Plane, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder};
use ec_core::timebase::Timestamp;

/// How a raw frame is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// 24- or 32-bit BGR(A) DIB rows, padded to 4 bytes.
    Dib {
        bytes_per_pixel: usize,
        top_down: bool,
    },
    Yuy2,
    Uyvy,
    Yvyu,
    I420,
    Yv12,
    Nv12,
    /// 8-bit luma only (`Y800`, `Y8`, `GREY`), rows tight.
    Gray8,
    /// 8-bit palettised DIB (palette follows the header), expanded to `Bgr8`.
    Pal8 {
        top_down: bool,
    },
    /// 15/16-bit BI_RGB (little-endian RGB555), rows padded to 4 bytes, expanded to `Bgr8`.
    Rgb555 {
        top_down: bool,
    },
    /// v210: 10-bit 4:2:2, 6 pixels per 16 bytes, rows padded to 48 pixels.
    V210,
    // The QuickTime layouts below come from the `mov` marker (see `layout_of_mov`).
    /// `raw ` 24-bit: packed RGB, rows tight.
    MovRgb24,
    /// `raw ` 32-bit: packed ARGB, rows tight; comes out as `Bgra8`.
    MovArgb,
    /// `raw ` 16-bit: big-endian RGB555, rows tight; comes out as `Bgr8`.
    MovRgb555Be,
    /// `yuv2`: YUYV with the chroma bytes offset by 128.
    MovYuv2,
    /// `v410`: packed 10-bit 4:4:4 (32-bit LE words U, Y, V), comes out as `I410`.
    MovV410,
    /// `b16g`: 16-bit big-endian gray, comes out as `Gray16` (little-endian).
    MovGray16Be,
}

impl Layout {
    pub(crate) fn format(self) -> PixelFormat {
        match self {
            Layout::Dib {
                bytes_per_pixel: 3, ..
            } => PixelFormat::Bgr8,
            Layout::Dib { .. } => PixelFormat::Bgra8,
            Layout::Yuy2 | Layout::Uyvy | Layout::Yvyu | Layout::MovYuv2 => PixelFormat::I422,
            Layout::I420 | Layout::Yv12 => PixelFormat::I420,
            Layout::Nv12 => PixelFormat::Nv12,
            Layout::Gray8 => PixelFormat::Gray8,
            Layout::Pal8 { .. } | Layout::Rgb555 { .. } | Layout::MovRgb555Be => PixelFormat::Bgr8,
            Layout::V210 => PixelFormat::I210,
            Layout::MovRgb24 => PixelFormat::Rgb8,
            Layout::MovArgb => PixelFormat::Bgra8,
            Layout::MovV410 => PixelFormat::I410,
            Layout::MovGray16Be => PixelFormat::Gray16,
        }
    }
}

/// The marker a QuickTime sample entry's synthesized 40-byte header carries in place of
/// `biSize` ("mov "): width at 4, height at 8, depth at 14, the sample entry FourCC at 16.
pub const MOV_MARKER: [u8; 4] = *b"mov ";

/// The layout a QuickTime raw sample entry (`raw `, `2vuy`, `yuvs`, `yuv2`, `v210`, `v410`, `b16g`) with
/// `depth` bits states.
pub fn layout_of_mov(kind: &[u8; 4], depth: u16) -> Option<Layout> {
    match kind {
        b"raw " => match depth {
            24 => Some(Layout::MovRgb24),
            32 => Some(Layout::MovArgb),
            16 => Some(Layout::MovRgb555Be),
            _ => None,
        },
        b"2vuy" => Some(Layout::Uyvy),
        b"yuvs" => Some(Layout::Yuy2),
        b"yuv2" => Some(Layout::MovYuv2),
        b"v210" => Some(Layout::V210),
        b"v410" => Some(Layout::MovV410),
        b"b16g" => Some(Layout::MovGray16Be),
        _ => None,
    }
}

/// The layout a `BITMAPINFOHEADER` (40+ bytes) with FourCC `comp` and `bits` per pixel states.
pub(crate) fn layout_of(comp: &[u8; 4], bits: u16, signed_height: i32) -> Option<Layout> {
    let up: Vec<u8> = comp.iter().map(|b| b.to_ascii_uppercase()).collect();
    match &up[..] {
        b"YUY2" | b"YUYV" | b"YUV2" | b"V422" => Some(Layout::Yuy2),
        b"UYVY" | b"2VUY" => Some(Layout::Uyvy),
        b"I420" | b"IYUV" => Some(Layout::I420),
        b"YV12" => Some(Layout::Yv12),
        b"YVYU" => Some(Layout::Yvyu),
        b"NV12" => Some(Layout::Nv12),
        b"Y800" | b"Y8  " | b"GREY" => Some(Layout::Gray8),
        b"V210" => Some(Layout::V210),
        _ if comp == &[0; 4] || up == b"RAW " || up == b"RGB " || up == b"BGR " => match bits {
            8 => Some(Layout::Pal8 {
                top_down: signed_height < 0,
            }),
            15 | 16 => Some(Layout::Rgb555 {
                top_down: signed_height < 0,
            }),
            24 => Some(Layout::Dib {
                bytes_per_pixel: 3,
                top_down: signed_height < 0,
            }),
            32 => Some(Layout::Dib {
                bytes_per_pixel: 4,
                top_down: signed_height < 0,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Append an RGB555 pixel as B, G, R bytes, widening each 5-bit field by bit replication.
fn push_rgb555(v: &mut Vec<u8>, px: u16) {
    let ex = |x: u16| ((x << 3) | (x >> 2)) as u8;
    v.extend_from_slice(&[ex(px & 31), ex((px >> 5) & 31), ex((px >> 10) & 31)]);
}

/// Raw video decoder (`CodecId::RawVideo`): one packet is one frame.
pub struct RawVideoDecoder {
    params: CodecParameters,
    layout: Layout,
    /// BGR palette entries for [`Layout::Pal8`].
    palette: Vec<[u8; 3]>,
    width: usize,
    height: usize,
    out: VecDeque<VideoFrame>,
    eof: bool,
}

impl RawVideoDecoder {
    /// A decoder for `params`, whose extradata is the stream's `BITMAPINFOHEADER` as the AVI
    /// demuxer leaves it.
    pub fn new(params: CodecParameters) -> Result<RawVideoDecoder> {
        if params.codec != CodecId::RawVideo {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements raw video only",
            ));
        }
        let bih = params
            .extradata
            .as_ref()
            .map(|b| b.to_vec())
            .filter(|b| b.len() >= 40)
            .ok_or_else(|| Error::corrupt("raw video: missing BITMAPINFOHEADER"))?;
        let le = |at: usize| u32::from_le_bytes([bih[at], bih[at + 1], bih[at + 2], bih[at + 3]]);
        let width = le(4) as i32;
        let signed_height = le(8) as i32;
        let bits = u16::from_le_bytes([bih[14], bih[15]]);
        let comp = [bih[16], bih[17], bih[18], bih[19]];
        let layout = if bih[..4] == MOV_MARKER {
            layout_of_mov(&comp, bits)
        } else {
            layout_of(&comp, bits, signed_height)
        }
        .ok_or_else(|| Error::unsupported("raw video layout", "not a carried raw layout"))?;
        if width <= 0
            || signed_height == 0
            || width > 1 << 16
            || signed_height.unsigned_abs() > 1 << 16
        {
            return Err(Error::corrupt("raw video: bad dimensions"));
        }
        Ok(RawVideoDecoder {
            params,
            layout,
            palette: bih[40..]
                .chunks_exact(4)
                .map(|c| [c[0], c[1], c[2]])
                .collect(),
            width: width as usize,
            height: signed_height.unsigned_abs() as usize,
            out: VecDeque::new(),
            eof: false,
        })
    }

    /// Decode one frame's bytes.
    pub fn decode(&self, data: &[u8]) -> Result<VideoFrame> {
        let (w, h) = (self.width, self.height);
        let short = || Error::corrupt("raw video: packet shorter than a frame");
        let cw = w.div_ceil(2);
        let planes = match self.layout {
            Layout::Dib {
                bytes_per_pixel: bpp,
                top_down,
            } => {
                let row = w * bpp;
                let stride = (row + 3) & !3;
                if data.len() < stride * (h - 1) + row {
                    return Err(short());
                }
                let mut v = Vec::with_capacity(row * h);
                for y in 0..h {
                    let src = if top_down { y } else { h - 1 - y };
                    v.extend_from_slice(&data[src * stride..src * stride + row]);
                }
                vec![Plane::new(v, row)]
            }
            Layout::Yuy2 | Layout::Uyvy | Layout::Yvyu | Layout::MovYuv2 => {
                let stride = cw * 4;
                if data.len() < stride * h {
                    return Err(short());
                }
                let (yo, uo, vo) = match self.layout {
                    Layout::Uyvy => (1, 0, 2),
                    Layout::Yvyu => (0, 3, 1),
                    _ => (0, 1, 3),
                };
                let bias = if self.layout == Layout::MovYuv2 {
                    0x80
                } else {
                    0
                };
                let mut y = vec![0u8; w * h];
                let mut u = vec![0u8; cw * h];
                let mut v = vec![0u8; cw * h];
                for r in 0..h {
                    let row = &data[r * stride..(r + 1) * stride];
                    for c in 0..cw {
                        let q = &row[c * 4..c * 4 + 4];
                        y[r * w + 2 * c] = q[yo];
                        if 2 * c + 1 < w {
                            y[r * w + 2 * c + 1] = q[yo + 2];
                        }
                        u[r * cw + c] = q[uo] ^ bias;
                        v[r * cw + c] = q[vo] ^ bias;
                    }
                }
                vec![Plane::new(y, w), Plane::new(u, cw), Plane::new(v, cw)]
            }
            Layout::I420 | Layout::Yv12 => {
                let ch = h.div_ceil(2);
                let (ys, cs) = (w * h, cw * ch);
                if data.len() < ys + 2 * cs {
                    return Err(short());
                }
                let a = data[ys..ys + cs].to_vec();
                let b = data[ys + cs..ys + 2 * cs].to_vec();
                let (u, v) = if self.layout == Layout::I420 {
                    (a, b)
                } else {
                    (b, a)
                };
                vec![
                    Plane::new(data[..ys].to_vec(), w),
                    Plane::new(u, cw),
                    Plane::new(v, cw),
                ]
            }
            Layout::Nv12 => {
                let ch = h.div_ceil(2);
                if data.len() < w * h + 2 * cw * ch {
                    return Err(short());
                }
                vec![
                    Plane::new(data[..w * h].to_vec(), w),
                    Plane::new(data[w * h..w * h + 2 * cw * ch].to_vec(), 2 * cw),
                ]
            }
            Layout::Gray8 | Layout::Pal8 { .. } | Layout::Rgb555 { .. } => {
                let (bpp, top_down) = match self.layout {
                    Layout::Pal8 { top_down } => (1, top_down),
                    Layout::Rgb555 { top_down } => (2, top_down),
                    _ => (1, true),
                };
                let row = w * bpp;
                let stride = if self.layout == Layout::Gray8 {
                    row
                } else {
                    (row + 3) & !3
                };
                if data.len() < stride * (h - 1) + row {
                    return Err(short());
                }
                let mut v = Vec::with_capacity(
                    row * h
                        * if bpp == 1 && self.layout != Layout::Gray8 {
                            3
                        } else {
                            1
                        },
                );
                for y in 0..h {
                    let src = if top_down { y } else { h - 1 - y };
                    let line = &data[src * stride..src * stride + row];
                    match self.layout {
                        Layout::Gray8 => v.extend_from_slice(line),
                        Layout::Pal8 { .. } => {
                            for &i in line {
                                v.extend_from_slice(
                                    self.palette.get(usize::from(i)).unwrap_or(&[0; 3]),
                                );
                            }
                        }
                        _ => {
                            for px in line.chunks_exact(2) {
                                push_rgb555(&mut v, u16::from_le_bytes([px[0], px[1]]));
                            }
                        }
                    }
                }
                let stride_out = if self.layout == Layout::Gray8 { w } else { w * 3 };
                vec![Plane::new(v, stride_out)]
            }
            Layout::V210 => {
                let stride = w.div_ceil(48) * 128;
                if data.len() < stride * h {
                    return Err(short());
                }
                let mut y = vec![0u8; w * h * 2];
                let mut u = vec![0u8; cw * h * 2];
                let mut v = vec![0u8; cw * h * 2];
                for r in 0..h {
                    let row = &data[r * stride..(r + 1) * stride];
                    let put = |p: &mut [u8], at: usize, lim: usize, val: u32| {
                        if at < lim {
                            p[(r * lim + at) * 2..(r * lim + at) * 2 + 2]
                                .copy_from_slice(&(val as u16).to_le_bytes());
                        }
                    };
                    for (g, blk) in row.chunks_exact(16).enumerate() {
                        let wd = |i: usize| {
                            u32::from_le_bytes([
                                blk[4 * i],
                                blk[4 * i + 1],
                                blk[4 * i + 2],
                                blk[4 * i + 3],
                            ])
                        };
                        let f = |x: u32, k: u32| (x >> (10 * k)) & 0x3ff;
                        let (w0, w1, w2, w3) = (wd(0), wd(1), wd(2), wd(3));
                        let ys = [f(w0, 1), f(w1, 0), f(w1, 2), f(w2, 1), f(w3, 0), f(w3, 2)];
                        let us = [f(w0, 0), f(w1, 1), f(w2, 2)];
                        let vs = [f(w0, 2), f(w2, 0), f(w3, 1)];
                        for (i, val) in ys.iter().enumerate() {
                            put(&mut y, g * 6 + i, w, *val);
                        }
                        for i in 0..3 {
                            put(&mut u, g * 3 + i, cw, us[i]);
                            put(&mut v, g * 3 + i, cw, vs[i]);
                        }
                    }
                }
                vec![
                    Plane::new(y, w * 2),
                    Plane::new(u, cw * 2),
                    Plane::new(v, cw * 2),
                ]
            }
            Layout::MovRgb24 => {
                if data.len() < w * h * 3 {
                    return Err(short());
                }
                vec![Plane::new(data[..w * h * 3].to_vec(), w * 3)]
            }
            Layout::MovArgb => {
                if data.len() < w * h * 4 {
                    return Err(short());
                }
                let mut v = Vec::with_capacity(w * h * 4);
                for px in data[..w * h * 4].chunks_exact(4) {
                    v.extend_from_slice(&[px[3], px[2], px[1], px[0]]);
                }
                vec![Plane::new(v, w * 4)]
            }
            Layout::MovRgb555Be => {
                if data.len() < w * h * 2 {
                    return Err(short());
                }
                let mut v = Vec::with_capacity(w * h * 3);
                for px in data[..w * h * 2].chunks_exact(2) {
                    push_rgb555(&mut v, u16::from_be_bytes([px[0], px[1]]));
                }
                vec![Plane::new(v, w * 3)]
            }
            Layout::MovV410 => {
                if data.len() < w * h * 4 {
                    return Err(short());
                }
                let n = w * h;
                let (mut y, mut u, mut v) = (
                    Vec::with_capacity(n * 2),
                    Vec::with_capacity(n * 2),
                    Vec::with_capacity(n * 2),
                );
                for px in data[..n * 4].chunks_exact(4) {
                    let val = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    u.extend_from_slice(&(((val >> 2) & 0x3ff) as u16).to_le_bytes());
                    y.extend_from_slice(&(((val >> 12) & 0x3ff) as u16).to_le_bytes());
                    v.extend_from_slice(&(((val >> 22) & 0x3ff) as u16).to_le_bytes());
                }
                vec![
                    Plane::new(y, w * 2),
                    Plane::new(u, w * 2),
                    Plane::new(v, w * 2),
                ]
            }
            Layout::MovGray16Be => {
                if data.len() < w * h * 2 {
                    return Err(short());
                }
                let v: Vec<u8> = data[..w * h * 2]
                    .chunks_exact(2)
                    .flat_map(|p| [p[1], p[0]])
                    .collect();
                vec![Plane::new(v, w * 2)]
            }
        };
        VideoFrame::try_new(self.layout.format(), w as u32, h as u32, planes)
    }
}

impl Decoder for RawVideoDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.eof = false;
        let mut f = self.decode(&packet.data)?;
        f.pts = packet.pts.map(|t| Timestamp::new(t, packet.time_base));
        self.out.push_back(f);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.out.pop_front() {
            Some(f) => Ok(Frame::Video(f)),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.out.clear();
        self.eof = false;
    }
}
