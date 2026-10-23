//! [`ec_core::registry::Decoder`] over the DV frame decoder, and conversion of a native
//! [`DvPicture`] into a [`VideoFrame`].

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Frame, Plane, PixelFormat, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::{TimeBase, Timestamp};

use crate::profile::Chroma;
use crate::video::{DvPicture, DvVideoDecoder};

/// Pixel format a DV system comes out as through [`DvDecoder`]: 4:2:0 PAL -> `I420`, 4:2:2 ->
/// `I422`, and 4:1:1 -> `I422` by exact horizontal chroma duplication (each native chroma sample
/// is repeated twice; use [`DvPicture`] / [`DvDecoder::decode_native`] for the native 4:1:1
/// planes).
pub fn output_format(c: Chroma) -> PixelFormat {
    match c {
        Chroma::Yuv420 => PixelFormat::I420,
        Chroma::Yuv411 | Chroma::Yuv422 => PixelFormat::I422,
    }
}

/// Convert a native DV picture to a [`VideoFrame`] (4:1:1 is widened to 4:2:2 by duplication).
pub fn to_video_frame(p: &DvPicture) -> VideoFrame {
    let w = p.width as usize;
    let (cw, ch) = (p.chroma_width(), p.chroma_height());
    let format = output_format(p.chroma);
    let mut planes = vec![Plane::new(p.planes[0].clone(), w)];
    for i in 1..3 {
        if p.chroma == Chroma::Yuv411 {
            let mut v = Vec::with_capacity(cw * 2 * ch);
            for row in p.planes[i].chunks_exact(cw) {
                for &s in row {
                    v.push(s);
                    v.push(s);
                }
            }
            planes.push(Plane::new(v, cw * 2));
        } else {
            planes.push(Plane::new(p.planes[i].clone(), cw));
        }
    }
    let mut f = VideoFrame::alloc(format, p.width, p.height);
    f.planes = planes;
    f.interlace = p.interlace;
    f.color = if p.profile.is_hd() {
        ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false }
    } else if p.profile.dsf == 1 {
        ColorInfo { primaries: 5, transfer: 6, matrix: 5, full_range: false }
    } else {
        ColorInfo { primaries: 6, transfer: 6, matrix: 6, full_range: false }
    };
    f
}

/// DV video decoder. Every packet is one whole DV frame (what an AVI/MOV sample or one frame of a
/// raw `.dv` file holds); one packet yields one frame.
pub struct DvDecoder {
    params: CodecParameters,
    inner: DvVideoDecoder,
    out: VecDeque<VideoFrame>,
    eof: bool,
}

impl DvDecoder {
    /// A decoder for `params` (`CodecId::DvVideo`).
    pub fn new(params: CodecParameters) -> Result<DvDecoder> {
        if params.codec != CodecId::DvVideo {
            return Err(Error::unsupported(format!("codec {:?}", params.codec), "this decoder implements DV video only"));
        }
        Ok(DvDecoder { params, inner: DvVideoDecoder::new(), out: VecDeque::new(), eof: false })
    }

    /// Decode one frame to its native planes (4:1:1 stays 4:1:1).
    pub fn decode_native(&mut self, data: &[u8]) -> Result<DvPicture> {
        let p = self.inner.decode(data)?;
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = p.width;
            v.height = p.height;
            v.format = Some(output_format(p.chroma));
            let (n, d) = p.profile.time_base;
            v.frame_rate = Some(TimeBase::new(i64::from(d), i64::from(n)));
            let (sn, sd) = p.profile.sar[usize::from(p.is_16_9)];
            v.sample_aspect_ratio = Some(TimeBase::new(i64::from(sn), i64::from(sd)));
        }
        Ok(p)
    }

    /// Decode one frame to a [`VideoFrame`] (see [`output_format`]).
    pub fn decode(&mut self, data: &[u8]) -> Result<VideoFrame> {
        let p = self.decode_native(data)?;
        Ok(to_video_frame(&p))
    }
}

impl ec_core::registry::Decoder for DvDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.eof = false;
        let mut v = self.decode(&packet.data)?;
        v.pts = packet.pts.map(|t| Timestamp::new(t, packet.time_base));
        self.out.push_back(v);
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
        self.inner.reset();
    }
}
