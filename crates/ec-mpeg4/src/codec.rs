//! The [`ec_core::registry::Decoder`] entry path and a byte-stream API.

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, PixelFormat, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::{TimeBase, Timestamp};

use crate::core::{Core, PictureInfo};

/// MPEG-4 Part 2 Visual (Simple and Advanced Simple profile) and H.263 baseline
/// decoder behind the codec registry's packet/frame contract.
///
/// One packet is one access unit (as stored by AVI, MP4/3GP and Matroska); a
/// DivX/XviD "packed bitstream" packet that carries a P-VOP followed by a
/// B-VOP is unpacked. The decoder's `extradata` is the VOL header (an `esds`
/// DecoderSpecificInfo, or the `00 00 01 B0 ...` sequence in AVI).
///
/// Pictures come back in display order. Unless the stream sets `low_delay`,
/// the decoder holds back one reference picture: the first `receive_frame`
/// answer arrives after the second I/P picture was decoded, and
/// [`Decoder::flush`](ec_core::registry::Decoder::flush) drains the last one.
/// B pictures are returned immediately. Timestamps travel with their picture.
pub struct Mpeg4Decoder {
    params: CodecParameters,
    core: Core,
    eof: bool,
    last_info: Option<PictureInfo>,
}

impl Mpeg4Decoder {
    pub fn new(params: CodecParameters) -> Result<Mpeg4Decoder> {
        if params.codec == CodecId::MsMpeg4V1 {
            return Err(Error::unsupported(
                "MS-MPEG4 version 1",
                "only versions 2 and 3 are implemented".to_string(),
            ));
        }
        let ms_version = match params.codec {
            CodecId::MsMpeg4V2 => 2,
            CodecId::MsMpeg4V3 => 3,
            _ => 0,
        };
        if params.codec != CodecId::Mpeg4Part2 && ms_version == 0 {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements MPEG-4 Part 2 / H.263 only",
            ));
        }
        let mut d = Mpeg4Decoder {
            params,
            core: Core::new(),
            eof: false,
            last_info: None,
        };
        if ms_version != 0 {
            d.core.ms_version = ms_version;
            d.core.have_vol = true;
            d.core.vol.low_delay = true;
            d.core.vol.progressive_sequence = true;
            if let MediaParameters::Video(v) = &d.params.media
                && v.width > 0
                && v.height > 0
            {
                let (w, h) = (v.width as usize, v.height as usize);
                d.core.vol.width = w as u32;
                d.core.vol.height = h as u32;
                d.core.set_size(w, h);
            }
            d.core.xvid_idct = d.core.force_xvid_idct.unwrap_or(false);
        } else if let Some(extra) = d.params.extradata.clone() {
            // Headers only; a refusal here resurfaces with the first picture.
            let _ = d.core.parse_config(&extra);
            d.refresh_params();
        }
        Ok(d)
    }

    /// Container FourCC (`XVID`, `DIVX`, `DX50`, ...). FFmpeg uses it to pick
    /// bug workarounds for encoders that do not identify themselves in user data.
    pub fn set_fourcc(&mut self, tag: [u8; 4]) {
        self.core.tag = tag;
    }

    /// Force the IDCT: `Some(true)` the XviD IDCT, `Some(false)` the simple IDCT.
    /// `None` (default) follows FFmpeg's `-idct auto`: XviD IDCT when the stream
    /// identifies as XviD, simple IDCT otherwise.
    pub fn set_xvid_idct(&mut self, force: Option<bool>) {
        self.core.force_xvid_idct = force;
    }

    /// Decode one packet's bytes. `pts` is attached to the picture decoded from it.
    pub fn send_bytes(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        self.eof = false;
        let r = if self.core.ms_version != 0 {
            self.core.decode_msmpeg4_packet(data, pts)
        } else {
            self.core.decode_packet(data, pts)
        };
        self.refresh_params();
        r
    }

    /// Release the delayed reference picture.
    pub fn flush_stream(&mut self) -> Result<()> {
        self.core.flush();
        self.eof = true;
        Ok(())
    }

    /// Take the next decoded picture with its [`PictureInfo`].
    pub fn pop_picture(&mut self) -> Option<(VideoFrame, PictureInfo)> {
        self.core.out.pop_front().map(|o| (o.frame, o.info))
    }

    /// [`PictureInfo`] of the frame last returned by `receive_frame`.
    pub fn last_picture_info(&self) -> Option<&PictureInfo> {
        self.last_info.as_ref()
    }

    /// Whether the stream sets MPEG-4 `low_delay` (no picture reordering delay).
    pub fn low_delay(&self) -> bool {
        self.core.vol.low_delay
    }

    fn refresh_params(&mut self) {
        if !self.core.have_vol || self.core.w == 0 {
            return;
        }
        let c = &self.core;
        if let MediaParameters::Video(v) = &mut self.params.media {
            v.width = c.w as u32;
            v.height = c.h as u32;
            v.format = Some(PixelFormat::I420);
            if !c.is_h263 && c.vol.sar.0 > 0 && c.vol.sar.1 > 0 && c.vol.sar.0 != c.vol.sar.1 {
                v.sample_aspect_ratio = Some(TimeBase::new(c.vol.sar.0 as i64, c.vol.sar.1 as i64));
            }
            if !c.is_h263 && c.vol.time_inc_res > 0 && c.vol.fixed_vop_rate_den > 0 {
                v.frame_rate = Some(TimeBase::new(
                    c.vol.time_inc_res as i64,
                    c.vol.fixed_vop_rate_den as i64,
                ));
            }
        }
    }
}

impl ec_core::registry::Decoder for Mpeg4Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let pts = packet
            .pts
            .map(|ticks| Timestamp::new(ticks, packet.time_base));
        self.send_bytes(&packet.data, pts)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.core.out.pop_front() {
            Some(o) => {
                self.last_info = Some(o.info);
                Ok(Frame::Video(o.frame))
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
