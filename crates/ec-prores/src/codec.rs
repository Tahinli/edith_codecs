//! The [`ec_core::registry::Decoder`] entry path and a direct frame API.

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, PixelFormat};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::Timestamp;

use crate::frame::{AlphaPlane, DecodedFrame, decode_frame};
use crate::header::FrameInfo;

/// ProRes decoder. Every packet is one whole frame (the `icpf` atom a QuickTime / MP4 sample
/// holds); there is no reordering, so each packet yields exactly one frame.
pub struct ProResDecoder {
    params: CodecParameters,
    out: VecDeque<ec_core::frame::VideoFrame>,
    eof: bool,
    threads: usize,
    last_alpha: Option<AlphaPlane>,
    last_info: Option<FrameInfo>,
}

impl ProResDecoder {
    /// A decoder for `params` (`CodecId::ProRes`). Slice-parallel over the available cores
    /// (up to 6) by default; see [`ProResDecoder::set_threads`].
    pub fn new(params: CodecParameters) -> Result<ProResDecoder> {
        if params.codec != CodecId::ProRes {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements Apple ProRes only",
            ));
        }
        Ok(ProResDecoder {
            params,
            out: VecDeque::new(),
            eof: false,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 6),
            last_alpha: None,
            last_info: None,
        })
    }

    /// Slice-parallel decode with up to `n` threads (clamped to 1..=6). Output is identical for
    /// any value.
    pub fn set_threads(&mut self, n: usize) {
        self.threads = n.clamp(1, 6);
    }

    /// Decode one frame from its bytes (`size`, `icpf`, frame header, pictures). Returns the
    /// picture, the alpha plane (when the stream has one) and the header facts.
    pub fn decode_full(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        let f = decode_frame(data, self.threads)?;
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = f.info.width;
            v.height = f.info.height;
            v.format = Some(f.video.format);
            v.color = f.video.color;
        }
        Ok(f)
    }

    /// Decode one frame and return its picture; alpha, if any, is kept for
    /// [`ProResDecoder::last_alpha`].
    pub fn decode(&mut self, data: &[u8]) -> Result<ec_core::frame::VideoFrame> {
        let f = self.decode_full(data)?;
        self.last_alpha = f.alpha;
        self.last_info = Some(f.info);
        Ok(f.video)
    }

    /// Alpha plane of the frame last decoded by `decode` or `send_packet`.
    pub fn last_alpha(&self) -> Option<&AlphaPlane> {
        self.last_alpha.as_ref()
    }

    /// Header facts of the frame last decoded.
    pub fn last_info(&self) -> Option<&FrameInfo> {
        self.last_info.as_ref()
    }

    /// Pixel format a stream of this kind decodes to.
    pub fn output_format(is_444: bool) -> PixelFormat {
        if is_444 { PixelFormat::I412 } else { PixelFormat::I210 }
    }
}

impl ec_core::registry::Decoder for ProResDecoder {
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
        self.last_alpha = None;
        self.last_info = None;
    }
}
