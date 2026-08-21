//! MP3 — MPEG-1/2/2.5 Layer III — decoding and encoding for the edith_codecs
//! family.
//!
//! Three ways in, smallest first:
//!
//! - [`Mp3Reader`] over a byte stream: push bytes, take frames. It skips ID3v2
//!   and the Xing/gapless/VBRI header frame, resyncs over junk, and keeps the bit
//!   reservoir across frames.
//! - [`Mp3Decoder`], the [`ec_core::Decoder`] contract, for a container that
//!   hands out one Layer III frame per packet.
//! - [`Mp3Encoder`]: interleaved PCM in, CBR Layer III frames out, with a
//!   Xing/Info header carrying the encoder delay and padding — so a decoder
//!   that honours it hands back exactly the samples that went in, with no
//!   leading or trailing silence.
//!
//! **Where the constants came from.** Layer III's Huffman code tables and its
//! 512-tap polyphase window are normative data no formula produces. Rather
//! than copy them out of an existing implementation, `scripts/mp3-tables/`
//! *measures* them: it writes legal frames whose main-data bits it chooses,
//! decodes them with the oracle, and reads the answer back through a model of the
//! decode chain. Every code tree is walked to a leaf, checked for a Kraft sum
//! of exactly 1 and for complete, duplicate-free coverage of its value grid;
//! the window is least-squares fitted to a residual of 1.6e-6. The measured
//! scalefactor band widths agreed with the published MPEG-1 layouts exactly,
//! which is the cross-check that the rig itself was right.
//!
//! Everything else — side info, the bit reservoir, requantisation, stereo
//! coupling, the block windows, the psychoacoustic model — is written from the
//! standards (ISO/IEC 11172-3 and 13818-3).
//!
//! Free-format streams are refused by name rather than guessed at, since their
//! frame length is only knowable by scanning to the next sync word.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod decode;
pub mod encode;
pub mod header;

mod filterbank;
mod huffman;
mod huffman_tables;
mod tables;
mod window;

pub use decode::{DecodedFrame, Mp3Decode, Mp3Reader};
pub use encode::{Mp3Encode, Mp3Encoder, Mp3EncoderConfig};
pub use header::{ChannelMode, FrameHeader, Version};

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

/// The [`ec_core::Decoder`] seat for MP3: one packet is one or more Layer III
/// frames, which is how Matroska, MP4 and the family's own probe deliver them.
#[derive(Debug)]
pub struct Mp3Decoder {
    params: CodecParameters,
    reader: Mp3Reader,
    pending: Vec<AudioFrame>,
    drained: bool,
}

impl Mp3Decoder {
    /// A decoder for a stream of Layer III packets.
    pub fn new(params: CodecParameters) -> Result<Mp3Decoder> {
        Ok(Mp3Decoder {
            params,
            reader: Mp3Reader::new(),
            pending: Vec::new(),
            drained: false,
        })
    }

    // Drains every complete frame the reservoir will yield. `NeedMore`/`Eof`
    // just mean "nothing left to take yet" and end the loop quietly; any
    // other error (corrupt CRC, unsupported layer) is real and must reach
    // the caller rather than vanish here.
    fn take(&mut self) -> Result<()> {
        loop {
            let frame = match self.reader.next_frame() {
                Ok(frame) => frame,
                Err(e) if e.is_need_more() || e.is_eof() => return Ok(()),
                Err(e) => return Err(e),
            };
            if frame.samples.is_empty() {
                continue;
            }
            let layout = ChannelLayout::from_count(frame.channels);
            let mut bytes = Vec::with_capacity(frame.samples.len() * 4);
            for sample in &frame.samples {
                bytes.extend_from_slice(&sample.to_ne_bytes());
            }
            let samples = frame.samples.len() / frame.channels;
            if let MediaParameters::Audio(audio) = &mut self.params.media {
                audio.sample_rate = frame.sample_rate;
                audio.layout = layout.clone();
                audio.format = Some(SampleFormat::F32);
            }
            if let Ok(audio) = AudioFrame::try_new(
                SampleFormat::F32,
                false,
                layout,
                frame.sample_rate,
                samples,
                vec![Buf::from(bytes)],
            ) {
                self.pending.push(audio);
            }
        }
    }
}

impl Decoder for Mp3Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.reader.push(packet.data.as_ref());
        self.take()
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        if self.pending.is_empty() {
            return Err(if self.drained {
                Error::Eof
            } else {
                Error::NeedMore
            });
        }
        Ok(Frame::Audio(self.pending.remove(0)))
    }

    fn flush(&mut self) -> Result<()> {
        self.take()?;
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.reader.reset();
        self.pending.clear();
        self.drained = false;
    }
}

/// Codec parameters for a Layer III stream of this shape.
pub fn codec_parameters(sample_rate: u32, channels: usize) -> CodecParameters {
    let mut params = CodecParameters::new(CodecId::Mp3);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate,
        layout: ChannelLayout::from_count(channels),
        format: Some(SampleFormat::F32),
        bits_per_sample: None,
    });
    params
}
