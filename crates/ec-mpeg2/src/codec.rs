//! The [`ec_core::registry::Decoder`] entry path and a byte-stream API.

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::{TimeBase, Timestamp};

use crate::decoder::{Core, PictureInfo};
use crate::header::SequenceInfo;

/// MPEG-1/2 video decoder behind the codec registry's packet/frame contract.
///
/// Packets are elementary-stream bytes: one or more whole pictures with the
/// headers in front of them (what a TS/PS demuxer or an `.m2v` splitter
/// produces). Pictures come back in display order; reordering delay is drained
/// by [`Decoder::flush`](ec_core::registry::Decoder::flush).
pub struct Mpeg2Decoder {
    params: CodecParameters,
    core: Core,
    eof: bool,
    last_info: Option<PictureInfo>,
}

impl Mpeg2Decoder {
    /// A decoder for `params`. `extradata`, when present, is parsed as
    /// elementary-stream headers (sequence header and extensions with start
    /// codes), so the picture size is known before the first packet.
    pub fn new(params: CodecParameters) -> Result<Mpeg2Decoder> {
        if params.codec != CodecId::Mpeg2Video {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements MPEG-1/2 video only",
            ));
        }
        let mut d = Mpeg2Decoder {
            params,
            core: Core::new(),
            eof: false,
            last_info: None,
        };
        if let Some(extra) = d.params.extradata.clone() {
            // Headers only; a refusal here resurfaces with the first picture.
            let _ = d.core.push_complete(&extra, None);
            d.refresh_params();
        }
        Ok(d)
    }

    /// Feed arbitrarily chunked elementary-stream bytes. A unit is decoded once
    /// the next start code has arrived; [`Mpeg2Decoder::flush_stream`] ends the
    /// stream. `pts` belongs to the first picture that starts in `data`.
    pub fn send_bytes(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        self.eof = false;
        let r = self.core.push(data, pts);
        self.refresh_params();
        r
    }

    /// Decode what is buffered and release the delayed pictures.
    pub fn flush_stream(&mut self) -> Result<()> {
        let r = self.core.flush();
        self.eof = true;
        self.refresh_params();
        r
    }

    /// Take the next decoded picture with its [`PictureInfo`].
    pub fn pop_picture(&mut self) -> Option<(VideoFrame, PictureInfo)> {
        self.core.out.pop_front()
    }

    /// The current sequence header/extension, once seen.
    pub fn sequence(&self) -> Option<&SequenceInfo> {
        self.core.seq.as_ref()
    }

    /// [`PictureInfo`] of the frame last returned by `receive_frame`.
    pub fn last_picture_info(&self) -> Option<&PictureInfo> {
        self.last_info.as_ref()
    }

    /// Pictures dropped because their forward reference was missing (the
    /// leading B pictures of an open GOP after a seek).
    pub fn dropped_pictures(&self) -> u64 {
        self.core.dropped
    }

    fn refresh_params(&mut self) {
        let Some(seq) = self.core.seq.as_ref() else {
            return;
        };
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = seq.width;
            v.height = seq.height;
            v.format = Some(if seq.chroma_format == 2 {
                ec_core::frame::PixelFormat::I422
            } else {
                ec_core::frame::PixelFormat::I420
            });
            v.frame_rate = seq.frame_rate().map(|(n, d)| TimeBase::new(n as i64, d as i64));
            // Display aspect ratio -> sample aspect ratio.
            let dar = match seq.aspect_ratio_code {
                2 => Some((4i64, 3i64)),
                3 => Some((16, 9)),
                4 => Some((221, 100)),
                _ => None,
            };
            v.sample_aspect_ratio = if seq.mpeg2 {
                dar.map(|(n, d)| {
                    let (num, den) = (n * seq.height as i64, d * seq.width as i64);
                    let g = gcd(num, den).max(1);
                    TimeBase::new(num / g, den / g)
                })
            } else {
                None
            };
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl ec_core::registry::Decoder for Mpeg2Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.eof = false;
        let pts = packet
            .pts
            .map(|ticks| Timestamp::new(ticks, packet.time_base));
        let r = self.core.push_complete(&packet.data, pts);
        self.refresh_params();
        r
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.core.out.pop_front() {
            Some((f, info)) => {
                self.last_info = Some(info);
                Ok(Frame::Video(f))
            }
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.flush_stream()
    }

    fn reset(&mut self) {
        self.core.reset();
        self.eof = false;
        self.last_info = None;
    }
}
