//! Motion JPEG video decoder.
//!
//! A packet is one JPEG (or two, for field-coded MJPEG-A / AVI interlaced video). It is decoded
//! straight to planar YUV at the file's own subsampling by [`ec_image::jpeg::decode_yuv`]: no
//! chroma upsampling, no RGB round trip. The planes are byte-identical to libavcodec's `mjpeg`
//! decoder (`yuvj420p`, `yuvj422p`, `yuvj444p`, `gray`).
//!
//! Frames are always tagged full range (JFIF), whatever the container says: MJPEG is full range.
//!
//! What this handles beyond a still JPEG: packets with no DHT (AVI1/MJPG, which rely on the
//! Annex K tables), restart markers, progressive frames, and two-field packets, which are woven
//! into one frame and flagged interlaced.
//!
//! Refused by name: 12-bit precision, arithmetic/lossless/hierarchical JPEG, CMYK, RGB-coded
//! frames, and sampling layouts with no pixel format (4:4:0, 4:1:1, 4:1:0).
#![forbid(unsafe_code)]

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Frame, Interlace, PixelFormat, Plane, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::Timestamp;
use ec_image::Limits;
use ec_image::jpeg::{Subsampling, YuvImage};

/// Largest picture accepted by default (matches the still decoder's order of magnitude).
const DEFAULT_LIMITS: Limits = Limits {
    max_pixels: 16384 * 16384,
    max_alloc: 1 << 30,
    max_total_alloc: 1 << 31,
};

/// Motion JPEG decoder behind the codec registry's packet/frame contract.
pub struct MjpegDecoder {
    params: CodecParameters,
    field_order: Interlace,
    limits: Limits,
    out: VecDeque<VideoFrame>,
    eof: bool,
}

impl MjpegDecoder {
    /// A decoder for `params` (`CodecId::Mjpeg`). The container's coded height steers
    /// two-field detection: a packet holding two JPEGs of half that height is one interlaced
    /// frame (libavcodec's rule); a packet whose JPEG already has the container's height is
    /// decoded as its first image only.
    pub fn new(params: CodecParameters) -> Result<MjpegDecoder> {
        if params.codec != CodecId::Mjpeg {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements Motion JPEG only",
            ));
        }
        Ok(MjpegDecoder {
            params,
            field_order: Interlace::TopFieldFirst,
            limits: DEFAULT_LIMITS,
            out: VecDeque::new(),
            eof: false,
        })
    }

    /// Which field the first JPEG of a two-field packet is. Default top field first (QuickTime
    /// `mjpa`, Matroska); libavcodec's AVI path (`MJPG` tag) takes the first image as the
    /// bottom field, so an AVI caller passes [`Interlace::BottomFieldFirst`] (or reads a
    /// container `fiel` atom).
    pub fn set_field_order(&mut self, order: Interlace) {
        self.field_order = if order.is_interlaced() {
            order
        } else {
            Interlace::TopFieldFirst
        };
    }

    /// Replace the size limits applied to every picture.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Decode one packet to one frame.
    pub fn decode(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<VideoFrame> {
        let container_height = match &self.params.media {
            MediaParameters::Video(v) => v.height,
            _ => 0,
        };
        let container_color = match &self.params.media {
            MediaParameters::Video(v) => v.color,
            _ => ColorInfo::default(),
        };
        let mut frame = decode_frame(data, container_height, self.field_order, self.limits)?;
        frame.pts = pts;
        // JFIF is BT.601 and full range. A container's matrix is kept when it states one;
        // its range claim is not: MJPEG is full range.
        frame.color = ColorInfo {
            primaries: container_color.primaries,
            transfer: container_color.transfer,
            matrix: if container_color.matrix == 2 {
                5
            } else {
                container_color.matrix
            },
            full_range: true,
        };
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = frame.width;
            if !frame.interlace.is_interlaced() || v.height == 0 {
                v.height = frame.height;
            }
            v.format = Some(frame.format);
        }
        Ok(frame)
    }
}

fn format_of(s: Subsampling) -> PixelFormat {
    match s {
        Subsampling::Gray => PixelFormat::Gray8,
        Subsampling::Yuv420 => PixelFormat::I420,
        Subsampling::Yuv422 => PixelFormat::I422,
        Subsampling::Yuv444 => PixelFormat::I444,
    }
}

/// Decode one MJPEG packet. `container_height` is the coded height the container states
/// (0 when unknown); `first_field` names the first image's field for two-field packets.
pub fn decode_frame(
    data: &[u8],
    container_height: u32,
    first_field: Interlace,
    limits: Limits,
) -> Result<VideoFrame> {
    let converted;
    let data = if is_mjpegb(data) {
        converted = mjpegb_to_jpeg(data)?;
        &converted[..]
    } else {
        data
    };
    let first = ec_image::jpeg::decode_yuv(data, limits)?;
    let rest = &data[first.end.min(data.len())..];
    let second_at = rest.windows(2).position(|w| w == [0xff, 0xd8]);
    let two_fields =
        second_at.is_some() && (container_height == 0 || container_height != first.height);
    if let (true, Some(at)) = (two_fields, second_at) {
        let second = ec_image::jpeg::decode_yuv(&rest[at..], limits)?;
        if second.width != first.width
            || second.height != first.height
            || second.subsampling != first.subsampling
        {
            return Err(Error::corrupt(
                "MJPEG: the two fields differ in size or layout",
            ));
        }
        return weave(first, second, first_field);
    }
    let format = format_of(first.subsampling);
    let (width, height) = (first.width, first.height);
    let planes = first
        .planes
        .into_iter()
        .map(|p| Plane::new(p.data, p.width))
        .collect();
    let mut f = VideoFrame::try_new(format, width, height, planes)?;
    f.color = jfif_color();
    f.interlace = Interlace::Progressive;
    Ok(f)
}

/// True for an MJPEG-B (QuickTime `mjpb`) packet: four zero bytes, then `mjpg`.
fn is_mjpegb(data: &[u8]) -> bool {
    data.len() > 40 && data[..4] == [0; 4] && &data[4..8] == b"mjpg"
}

/// Rewrap an MJPEG-B packet as ordinary baseline JPEG (one per field, concatenated).
///
/// A field starts with ten big-endian words: reserved 0, `mjpg`, field size, padded field
/// size, then the offsets (from the field start) of the second field, DQT, DHT, SOF, SOS and the
/// entropy-coded data. The DQT/DHT/SOF/SOS blocks keep their 16-bit length but lose their
/// marker; a zero offset means the block is absent. The scan data carries no FF 00 stuffing. The second field, when present, is a
/// complete field of the same shape at `second_field_offs`.
fn mjpegb_to_jpeg(data: &[u8]) -> Result<Vec<u8>> {
    let bad = |what: &str| Error::corrupt(format!("MJPEG-B: {what}"));
    let word = |d: &[u8], i: usize| -> Result<usize> {
        d.get(i * 4..i * 4 + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| bad("short header"))
    };
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut base = 0usize;
    for pass in 0..2 {
        let f = &data[base..];
        if f.len() < 40 || f[4..8] != *b"mjpg" {
            return Err(bad("bad field header"));
        }
        let size = word(f, 2)?.min(f.len());
        let (second, dqt, dht, sof, sos, sod) = (
            word(f, 4)?,
            word(f, 5)?,
            word(f, 6)?,
            word(f, 7)?,
            word(f, 8)?,
            word(f, 9)?,
        );
        let f = &f[..size];
        out.extend_from_slice(&[0xff, 0xd8]);
        let mut end_of_sos = 0;
        for (off, marker) in [(dqt, 0xdb), (dht, 0xc4), (sof, 0xc0), (sos, 0xda)] {
            if off == 0 {
                continue;
            }
            let len = f
                .get(off..off + 2)
                .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                .ok_or_else(|| bad("block offset past the field"))?;
            let seg = f
                .get(off..off + len)
                .ok_or_else(|| bad("block past the field"))?;
            out.extend_from_slice(&[0xff, marker]);
            out.extend_from_slice(seg);
            if marker == 0xda {
                end_of_sos = off + len;
            }
        }
        if sos == 0 {
            return Err(bad("field without a scan header"));
        }
        let start = if sod >= end_of_sos { sod } else { end_of_sos };
        let scan = f
            .get(start..)
            .ok_or_else(|| bad("scan data past the field"))?;
        // MJPEG-B entropy data is not byte-stuffed; ordinary JPEG needs FF 00 for every FF.
        for &b in scan {
            out.push(b);
            if b == 0xff {
                out.push(0);
            }
        }
        out.extend_from_slice(&[0xff, 0xd9]);
        if pass == 0 {
            if second == 0 || base != 0 || second >= data.len() {
                break;
            }
            base = second;
        }
    }
    Ok(out)
}

fn jfif_color() -> ColorInfo {
    ColorInfo {
        primaries: 2,
        transfer: 2,
        matrix: 5,
        full_range: true,
    }
}

/// Interleave two field images into one frame.
fn weave(first: YuvImage, second: YuvImage, first_field: Interlace) -> Result<VideoFrame> {
    let format = format_of(first.subsampling);
    let height = first
        .height
        .checked_mul(2)
        .ok_or_else(|| Error::corrupt("MJPEG: height overflow"))?;
    let width = first.width;
    let geometry = format.plane_geometry(width, height);
    let first_odd = first_field == Interlace::BottomFieldFirst;
    let mut planes = Vec::with_capacity(geometry.len());
    for (i, (row_bytes, rows)) in geometry.into_iter().enumerate() {
        let (a, b) = (&first.planes[i], &second.planes[i]);
        let mut data = vec![0u8; row_bytes * rows];
        for (k, field) in [a, b].into_iter().enumerate() {
            // The first image goes on the odd lines when it is the bottom field.
            let parity = if (k == 0) == first_odd { 1 } else { 0 };
            for y in 0..field.height {
                let dst = 2 * y + parity;
                if dst >= rows {
                    break;
                }
                data[dst * row_bytes..(dst + 1) * row_bytes]
                    .copy_from_slice(&field.data[y * field.width..y * field.width + row_bytes]);
            }
        }
        planes.push(Plane::new(data, row_bytes));
    }
    let mut f = VideoFrame::try_new(format, width, height, planes)?;
    f.color = jfif_color();
    f.interlace = if first_odd {
        Interlace::BottomFieldFirst
    } else {
        Interlace::TopFieldFirst
    };
    Ok(f)
}

impl ec_core::registry::Decoder for MjpegDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.eof = false;
        let pts = packet
            .pts
            .map(|ticks| Timestamp::new(ticks, packet.time_base));
        let frame = self.decode(&packet.data, pts)?;
        self.out.push_back(frame);
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
