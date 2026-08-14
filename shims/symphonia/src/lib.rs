//! Compatibility shim: the `symphonia` 0.6 umbrella surface edith consumes,
//! served by [`ec_probe`].
//!
//! Two registries, exactly as the incumbent has them:
//! [`default::get_probe`] opens a file and [`default::get_codecs`] builds a
//! decoder for one of its tracks. Underneath, both are `ec_probe`: one sniffing
//! reader over WAV, MP3, FLAC, ADTS, Ogg, mp4 and Matroska, and one decoder
//! seat over the family's codecs.
//!
//! Two behaviours differ from the incumbent, deliberately, because they are
//! defects edith works around today:
//!
//! - **A seek is reliable more than once per reader.** The incumbent's mkv
//!   seek neither clears its queued frames nor recovers its element iterator,
//!   so edith opens a *new reader per seek attempt*
//!   (`crates/engine/src/audio.rs`, `SymTrack::seek_to`). Here every seek is
//!   served on the one reader the file was opened with, and the landing it
//!   reports is the next packet's own timestamp.
//! - **A multichannel AAC track decodes.** The incumbent refuses anything
//!   wider than stereo (`aac: aac too complex`), which is exactly a film's 5.1;
//!   [`ec_aac`] decodes to 7.1, so `make_audio_decoder` does not refuse it.
//!
//! Written from the signatures edith calls and the crate's published
//! documentation; no symphonia source was read (it is MPL-2.0, this family is
//! MIT OR Apache-2.0).

#![forbid(unsafe_code)]

pub use symphonia_core as core;

mod reader;

use symphonia_core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::*,
};
use symphonia_core::codecs::{
    AudioDecoderRegistration, CodecDescriptor, CodecInfo, ec_codec_id, ec_parameters,
};
use symphonia_core::formats::{FormatOptions, FormatReader, probe::Hint};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataOptions;
use symphonia_core::{Error, Result};

pub use reader::ProbeReader;

/// The registries, as the incumbent hands them out.
pub mod default {
    use super::{CodecRegistry, Probe};

    /// The format probe: content-sniffing, extension-hinted.
    pub fn get_probe() -> &'static Probe {
        &Probe
    }

    /// The codec registry.
    pub fn get_codecs() -> &'static CodecRegistry {
        &CodecRegistry
    }
}

/// Opens a media source as whatever it turns out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe;

impl Probe {
    /// Open `source`, using `hint` only where the content says nothing.
    pub fn probe(
        &self,
        hint: &Hint,
        source: MediaSourceStream,
        _format: FormatOptions,
        _metadata: MetadataOptions,
    ) -> Result<Box<dyn FormatReader>> {
        let reader = ec_probe::Reader::new(source.into_inner(), hint.extension())?;
        Ok(Box::new(ProbeReader::new(reader)))
    }
}

/// Which codecs have a decoder behind them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecRegistry;

/// Every codec this registry serves, with the name it reports.
const REGISTERED: &[(AudioCodecId, &str, &str)] = &[
    (CODEC_ID_AAC, "aac", "Advanced Audio Coding"),
    (CODEC_ID_OPUS, "opus", "Opus"),
    (CODEC_ID_VORBIS, "vorbis", "Vorbis"),
    (CODEC_ID_FLAC, "flac", "Free Lossless Audio Codec"),
    (CODEC_ID_MP3, "mp3", "MPEG-1/2 Audio Layer III"),
    (CODEC_ID_ALAC, "alac", "Apple Lossless Audio Codec"),
    (CODEC_ID_AC3, "ac3", "Dolby Digital"),
    (CODEC_ID_EAC3, "eac3", "Dolby Digital Plus"),
    (CODEC_ID_PCM_U8, "pcm_u8", "PCM unsigned 8-bit"),
    (CODEC_ID_PCM_S16LE, "pcm_s16le", "PCM signed 16-bit LE"),
    (CODEC_ID_PCM_S16BE, "pcm_s16be", "PCM signed 16-bit BE"),
    (CODEC_ID_PCM_S24LE, "pcm_s24le", "PCM signed 24-bit LE"),
    (CODEC_ID_PCM_S32LE, "pcm_s32le", "PCM signed 32-bit LE"),
    (CODEC_ID_PCM_F32LE, "pcm_f32le", "PCM float 32-bit LE"),
    // TrueHD and DTS are deliberately absent: nothing in this family decodes
    // them, so a caller asking gets `None` and can say so.
];

impl CodecRegistry {
    /// The registration for `codec`, or [`None`] when nothing decodes it.
    pub fn get_audio_decoder(&self, codec: AudioCodecId) -> Option<AudioDecoderRegistration> {
        REGISTERED
            .iter()
            .find(|(id, ..)| *id == codec)
            .map(|&(id, short_name, long_name)| AudioDecoderRegistration {
                codec: CodecDescriptor {
                    id,
                    info: CodecInfo {
                        short_name,
                        long_name,
                    },
                },
            })
    }

    /// Build a decoder for `params`.
    pub fn make_audio_decoder(
        &self,
        params: &AudioCodecParameters,
        _options: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        if self.get_audio_decoder(params.codec).is_none() {
            let named = ec_codec_id(params.codec).map_or("this codec", |c| c.name());
            return Err(Error::Unsupported(format!(
                "{named}: no decoder for it exists in this family"
            )));
        }
        Ok(Box::new(reader::EcAudioDecoder::new(
            ec_probe::AudioDecoder::new(&ec_parameters(params)?)?,
            params.clone(),
        )))
    }
}
