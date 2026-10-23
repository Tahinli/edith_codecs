//! The [`ec_core::registry::Decoder`] entry path and the one-shot API.

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, PixelFormat, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::{TimeBase, Timestamp};

use crate::decoder::{Chroma, Core, Picture, to_picture, to_video_frame};

/// Avid DNxHD / DNxHR decoder. One packet is one frame (both fields of an
/// interlaced frame included), as MOV, Matroska and MXF carry it.
pub struct DnxhdDecoder {
    params: CodecParameters,
    core: Core,
    ready: VecDeque<VideoFrame>,
    eof: bool,
}

impl DnxhdDecoder {
    /// A decoder for `params` (codec must be [`CodecId::Dnxhd`]). It uses up to
    /// six threads, one band of macroblock rows each.
    pub fn new(params: CodecParameters) -> Result<DnxhdDecoder> {
        if params.codec != CodecId::Dnxhd {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements DNxHD/DNxHR only",
            ));
        }
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        Ok(DnxhdDecoder { params, core: Core::new(threads), ready: VecDeque::new(), eof: false })
    }

    /// Limit the worker threads used per frame (1 to 6).
    pub fn set_threads(&mut self, n: usize) {
        self.core.threads = n.clamp(1, 6);
    }

    /// Decode one frame packet into a [`VideoFrame`] (I422, I210, I444 or I410;
    /// 12-bit pictures are narrowed to 10 bits). The interlace field is set for
    /// two-field frames; `pts` follows the packet.
    pub fn decode(&mut self, packet: &Packet) -> Result<VideoFrame> {
        let mut f = self.decode_bytes(&packet.data)?;
        f.pts = packet.pts.map(|t| Timestamp::new(t, packet.time_base));
        Ok(f)
    }

    /// [`DnxhdDecoder::decode`] on raw frame bytes.
    pub fn decode_bytes(&mut self, data: &[u8]) -> Result<VideoFrame> {
        let canvas = self.core.decode(data)?;
        self.refresh_params(&canvas.info);
        to_video_frame(canvas)
    }

    /// Decode a frame to `u16` planes at the coded bit depth (8, 10 or 12),
    /// with the header facts in [`Picture::info`].
    pub fn decode_native(&mut self, data: &[u8]) -> Result<Picture> {
        let canvas = self.core.decode(data)?;
        self.refresh_params(&canvas.info);
        Ok(to_picture(canvas))
    }

    fn refresh_params(&mut self, info: &crate::decoder::FrameInfo) {
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = info.width;
            v.height = info.height;
            v.format = Some(match (info.chroma, info.bit_depth) {
                (Chroma::C422, 8) => PixelFormat::I422,
                (Chroma::C422, _) => PixelFormat::I210,
                (Chroma::C444, 8) => PixelFormat::I444,
                (Chroma::C444, _) => PixelFormat::I410,
            });
            v.sample_aspect_ratio = info
                .sample_aspect_ratio
                .map(|(n, d)| TimeBase::new(i64::from(n), i64::from(d)));
        }
    }
}

impl ec_core::registry::Decoder for DnxhdDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.eof = false;
        let f = self.decode(packet)?;
        self.ready.push_back(f);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.ready.pop_front() {
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
        self.ready.clear();
        self.eof = false;
    }
}
