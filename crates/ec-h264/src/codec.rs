//! The [`ec_core::registry::Decoder`] entry path: packets in, frames out.
//!
//! [`crate::Decoder`] is the NAL-level surface a bitstream tool wants. This is
//! the surface a *player* wants, and it adds the two things a container brings
//! with it: parameter sets carried out of band in an `avcC` record
//! (ISO/IEC 14496-15 clause 5.3.3.1) and NAL units framed by a length prefix
//! rather than by Annex B start codes. Which framing a packet uses is decided
//! once, by whether `extradata` held an `avcC`.

use std::collections::VecDeque;

use ec_core::BitReader;
use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, PixelFormat};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_core::timebase::Timestamp;
use ec_h264_syntax::AnnexBIter;

use crate::decoder::{Decoder, NalOutcome, OutputOrder};

/// H.264 decoder behind the codec registry's packet/frame contract.
pub struct H264Decoder {
    params: CodecParameters,
    inner: Decoder,
    /// Bytes of the NAL length prefix when the stream is length framed;
    /// `None` means Annex B start codes.
    nal_length_size: Option<usize>,
    frames: VecDeque<Frame>,
    end_of_stream: bool,
}

impl H264Decoder {
    /// A decoder for `params`.
    ///
    /// When `params.extradata` holds an `avcC` record its parameter sets are
    /// read immediately — so [`H264Decoder::codec_parameters`] reports the
    /// picture size before the first packet — and packets are then expected to
    /// carry length-prefixed NAL units; otherwise packets are read as Annex B.
    pub fn new(params: CodecParameters) -> Result<H264Decoder> {
        if params.codec != CodecId::H264 {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements H.264 only",
            ));
        }
        let mut decoder = H264Decoder {
            params,
            inner: Decoder::new(),
            nal_length_size: None,
            frames: VecDeque::new(),
            end_of_stream: false,
        };
        // 7 bytes is the shortest record that can hold the fixed header plus a
        // parameter-set count; version 1 is the only one 14496-15 defines.
        if let Some(extradata) = decoder.params.extradata.clone()
            && extradata.len() >= 7
            && extradata[0] == 1
        {
            decoder.parse_avcc(&extradata)?;
        }
        Ok(decoder)
    }

    /// Choose the order frames come back in.
    ///
    /// The default is display order, which is the order a player presents
    /// pictures in and the only correct one for a stream with B pictures. A
    /// caller that does its own reordering — or that wants the lowest possible
    /// latency and knows the stream never reorders — can ask for decode order.
    pub fn set_output_order(&mut self, order: OutputOrder) {
        self.inner.set_output_order(order);
    }

    /// The order frames come back in.
    pub fn output_order(&self) -> OutputOrder {
        self.inner.output_order()
    }

    /// Read an `avcC` record: the NAL length size and the in-band parameter
    /// sets (ISO/IEC 14496-15 clause 5.3.3.1).
    fn parse_avcc(&mut self, extradata: &[u8]) -> Result<()> {
        let mut r = BitReader::new(extradata);
        // configurationVersion, AVCProfileIndication, profile_compatibility,
        // AVCLevelIndication: all restated by the SPS below.
        r.skip_bits(32)?;
        let length_size_minus_one = r.read_bits(8)? & 0x03;
        self.nal_length_size = Some(length_size_minus_one as usize + 1);
        let num_sps = r.read_bits(8)? & 0x1F;
        for _ in 0..num_sps {
            let len = r.read_bits(16)? as usize;
            let unit = r.read_bytes(len)?.to_vec();
            self.push(&unit)?;
        }
        let num_pps = r.read_bits(8)?;
        for _ in 0..num_pps {
            let len = r.read_bits(16)? as usize;
            let unit = r.read_bytes(len)?.to_vec();
            self.push(&unit)?;
        }
        Ok(())
    }

    /// Split a packet into NAL units by whichever framing this stream uses.
    fn decode_packet_payload(&mut self, data: &[u8]) -> Result<()> {
        let Some(size) = self.nal_length_size else {
            for unit in AnnexBIter::new(data) {
                let unit = unit.to_vec();
                self.push(&unit)?;
            }
            return Ok(());
        };
        let mut offset = 0usize;
        while offset + size <= data.len() {
            let mut length = 0usize;
            for i in 0..size {
                length = (length << 8) | data[offset + i] as usize;
            }
            offset += size;
            let end = offset
                .checked_add(length)
                .filter(|&end| end <= data.len())
                .ok_or_else(|| {
                    Error::corrupt(format!(
                        "H.264: a NAL unit of {length} bytes overruns the packet"
                    ))
                })?;
            let unit = data[offset..end].to_vec();
            self.push(&unit)?;
            offset = end;
        }
        Ok(())
    }

    /// Feed one NAL unit, completing the open picture when it starts a new one.
    fn push(&mut self, unit: &[u8]) -> Result<()> {
        match self.inner.push_nal(unit)? {
            NalOutcome::PictureBoundary => {
                // The NAL was not consumed: it opens the next picture.
                self.finish_picture()?;
                self.inner.push_nal(unit)?;
            }
            NalOutcome::ParameterSet => self.sync_parameters(),
            _ => {}
        }
        Ok(())
    }

    /// Complete the open picture, if there is one, and drain whatever that
    /// made ready for output.
    fn finish_picture(&mut self) -> Result<()> {
        if self.inner.picture_open() {
            self.inner.end_picture()?;
        }
        self.drain();
        Ok(())
    }

    /// Move every frame the decoder has released into the output queue.
    fn drain(&mut self) {
        while let Some(frame) = self.inner.next_frame() {
            self.frames.push_back(Frame::Video(frame));
        }
    }

    /// Publish the active SPS geometry on `params`, the way a demuxer that
    /// only had an `avcC` expects to read it back.
    fn sync_parameters(&mut self) {
        let Some((width, height)) = self.inner.picture_size() else {
            return;
        };
        if let MediaParameters::Video(video) = &mut self.params.media {
            video.width = width;
            video.height = height;
            video.format = Some(PixelFormat::I420);
        }
    }
}

impl ec_core::registry::Decoder for H264Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.end_of_stream = false;
        // The timestamp belongs to the picture, not to the packet: display
        // order hands pictures back in a different order than they arrive.
        self.inner.set_next_pts(
            packet
                .pts
                .map(|ticks| Timestamp::new(ticks, packet.time_base)),
        );
        self.decode_packet_payload(&packet.data)?;
        // One packet is one access unit, so whatever picture it started is
        // complete once the packet is consumed.
        self.finish_picture()
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.frames.pop_front() {
            Some(frame) => Ok(frame),
            None if self.end_of_stream => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()?;
        self.drain();
        self.end_of_stream = true;
        Ok(())
    }

    fn reset(&mut self) {
        // Parameter sets and the framing decision survive a seek; picture
        // state does not.
        self.inner.reset_pictures();
        self.frames.clear();
        self.end_of_stream = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::TimeBase;
    use ec_core::registry::Decoder as _;

    #[test]
    fn only_h264_parameters_are_accepted() {
        assert!(H264Decoder::new(CodecParameters::new(CodecId::H265)).is_err());
        let decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        assert_eq!(decoder.codec_parameters().codec, CodecId::H264);
        assert!(decoder.nal_length_size.is_none(), "Annex B by default");
    }

    #[test]
    fn a_packet_without_parameter_sets_is_corrupt_not_a_panic() {
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        // A slice NAL with no SPS or PPS in front of it.
        let stream = [0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00];
        let packet = Packet::new(0, TimeBase::new(1, 25), stream.to_vec());
        let err = decoder.send_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
        assert!(decoder.receive_frame().unwrap_err().is_need_more());
    }

    #[test]
    fn flush_then_drain_reports_end_of_stream() {
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        decoder.flush().unwrap();
        assert!(decoder.receive_frame().unwrap_err().is_eof());
        decoder.reset();
        assert!(decoder.receive_frame().unwrap_err().is_need_more());
    }

    /// A truncated `avcC` is a container bug, not a panic: the reader runs out
    /// of bytes and says so.
    #[test]
    fn a_truncated_avcc_is_an_error() {
        let mut params = CodecParameters::new(CodecId::H264);
        params.extradata = Some(ec_core::Buf::from_vec(vec![1, 66, 0, 30, 0xFF, 0xE1, 0x00]));
        assert!(H264Decoder::new(params).is_err());
    }
}
