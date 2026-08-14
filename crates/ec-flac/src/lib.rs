//! FLAC (RFC 9639) decoding and encoding for the edith_codecs family.
//!
//! Three ways in, smallest first:
//!
//! - [`decode::FlacReader`] over a whole `.flac` buffer — metadata, frames,
//!   seek table, and [`decode::FlacReader::decode_all`] for the common case.
//! - [`FlacDecoder`], the [`ec_core::Decoder`] contract, for a container that
//!   hands out one FLAC frame per packet.
//! - [`encode::encode`], interleaved samples in, a complete stream out, with
//!   the `STREAMINFO` MD5 filled in so any decoder can prove we were lossless.
//!
//! Decoded samples keep their own bit depth in [`decode::Block`] and
//! [`decode::DecodedStream`]; the PCM views ([`decode::DecodedStream::to_pcm_bytes`],
//! [`FlacDecoder`]) shift them left into an `s16`/`s32` container the way
//! ffmpeg and the family's mixer expect.
//!
//! Nothing here panics on malformed input: truncation is `NeedMore`, a broken
//! stream is `Corrupt`, and both CRCs are checked on every frame.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod checksum;
pub mod decode;
pub mod encode;

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

pub use decode::{Block, DecodedStream, FlacReader, StreamInfo};
pub use encode::{EncoderConfig, encode};

/// The [`ec_core::Decoder`] seat for FLAC: one packet is one FLAC frame, which
/// is how Matroska, Ogg and the family's own probe deliver them.
#[derive(Debug)]
pub struct FlacDecoder {
    params: CodecParameters,
    stream_info: Option<StreamInfo>,
    block: Block,
    pending: Option<AudioFrame>,
    drained: bool,
}

impl FlacDecoder {
    /// A decoder for a stream whose `extradata`, when present, is the 34-byte
    /// `STREAMINFO` payload every container carries for FLAC.
    pub fn new(params: CodecParameters) -> Result<FlacDecoder> {
        let stream_info = match &params.extradata {
            Some(bytes) if bytes.len() >= 34 => Some(StreamInfo::parse(&bytes[..34])?),
            _ => None,
        };
        Ok(FlacDecoder {
            params,
            stream_info,
            block: Block::default(),
            pending: None,
            drained: false,
        })
    }
}

impl Decoder for FlacDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let mut reader = FlacReader::frames(&packet.data, self.stream_info.clone())?;
        if !reader.next_block(&mut self.block)? {
            return Err(Error::NeedMore);
        }
        let header = self.block.header.expect("a decoded block has a header");
        let shift = decode::container_shift(header.bits_per_sample);
        let format = match header.bits_per_sample <= 16 {
            true => SampleFormat::S16,
            false => SampleFormat::S32,
        };
        let samples = self.block.len();
        let interleaved = self.block.to_interleaved(shift);
        let mut bytes = Vec::with_capacity(interleaved.len() * format.bytes_per_sample());
        for s in interleaved {
            match format {
                SampleFormat::S16 => bytes.extend_from_slice(&(s as i16).to_ne_bytes()),
                _ => bytes.extend_from_slice(&s.to_ne_bytes()),
            }
        }
        let layout = ChannelLayout::from_count(self.block.channels.len());
        let mut frame = AudioFrame::try_new(
            format,
            false,
            layout.clone(),
            header.sample_rate,
            samples,
            vec![Buf::from_vec(bytes)],
        )?;
        frame.pts = None;
        self.params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: header.sample_rate,
            layout,
            format: Some(format),
            bits_per_sample: Some(header.bits_per_sample),
        });
        self.pending = Some(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(Frame::Audio(frame)),
            None if self.drained => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.pending = None;
        self.drained = false;
    }
}

/// Codec parameters for a FLAC stream described by its `STREAMINFO`.
pub fn codec_parameters(info: &StreamInfo) -> CodecParameters {
    let mut params = CodecParameters::new(CodecId::Flac);
    params.extradata = Some(Buf::from_vec(info.to_bytes().to_vec()));
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: info.sample_rate,
        layout: ChannelLayout::from_count(usize::from(info.channels)),
        format: Some(match info.bits_per_sample <= 16 {
            true => SampleFormat::S16,
            false => SampleFormat::S32,
        }),
        bits_per_sample: Some(u32::from(info.bits_per_sample)),
    });
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::timebase::TimeBase;

    /// The container path: encode, hand each frame over as its own packet, and
    /// get the same audio back through the family's `Decoder` trait.
    #[test]
    fn decoder_trait_round_trips_packet_by_packet() {
        let samples: Vec<i32> = (0..8000)
            .map(|i| ((i as f64 * 0.05).sin() * 8000.0) as i32)
            .collect();
        let config = EncoderConfig {
            block_size: 1024,
            ..EncoderConfig::default()
        };
        let stream = encode(&config, &samples, 2, 16, 44100).expect("encode");

        let mut reader = FlacReader::new(&stream).expect("open");
        let info = reader.stream_info().expect("streaminfo").clone();
        let mut decoder = FlacDecoder::new(codec_parameters(&info)).expect("decoder");

        // Slice the stream back into frames the way a demuxer would.
        let mut block = Block::default();
        let mut bounds = vec![reader.position()];
        while reader.next_block(&mut block).expect("frame") {
            bounds.push(reader.position());
        }
        let time_base = TimeBase::new(1, 44100);
        let mut got = Vec::new();
        for w in bounds.windows(2) {
            let packet = Packet::new(0, time_base, &stream[w[0]..w[1]]);
            decoder.send_packet(&packet).expect("send");
            let Frame::Audio(frame) = decoder.receive_frame().expect("receive") else {
                panic!("FLAC decoded to a video frame");
            };
            assert_eq!(frame.format, SampleFormat::S16);
            assert_eq!(frame.channels(), 2);
            for chunk in frame.data[0].chunks_exact(2).take(frame.samples * 2) {
                got.push(i16::from_ne_bytes([chunk[0], chunk[1]]) as i32);
            }
        }
        assert_eq!(got, samples);
        assert!(decoder.receive_frame().unwrap_err().is_need_more());
        decoder.flush().unwrap();
        assert!(decoder.receive_frame().unwrap_err().is_eof());
    }
}
