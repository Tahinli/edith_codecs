//! Codec identities, stream parameters and the decoder trait.

use crate::Result;
use crate::packet::Packet;

/// Per-media-type parameters, as a track carries them.
#[derive(Debug, Clone, PartialEq)]
pub enum CodecParameters {
    /// An audio track.
    Audio(audio::AudioCodecParameters),
    /// A video track. Nothing in this family decodes picture through the
    /// symphonia surface, so it carries only what a listing needs.
    Video(VideoCodecParameters),
    /// A subtitle track.
    Subtitle(SubtitleCodecParameters),
}

/// What a video track states about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoCodecParameters {
    /// Codec identity, in this shim's own numbering.
    pub codec: u32,
    /// Coded width.
    pub width: Option<u16>,
    /// Coded height.
    pub height: Option<u16>,
}

/// What a subtitle track states about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubtitleCodecParameters {
    /// Codec identity, in this shim's own numbering.
    pub codec: u32,
    /// Setup bytes (the ASS header, for instance).
    pub extra_data: Option<Box<[u8]>>,
}

/// A codec's name, as a registry reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecInfo {
    /// Short lowercase name, e.g. `"aac"`.
    pub short_name: &'static str,
    /// Human-readable name.
    pub long_name: &'static str,
}

/// One entry of a codec registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecDescriptor {
    /// The identity this entry serves.
    pub id: audio::AudioCodecId,
    /// Its names.
    pub info: CodecInfo,
}

/// A registry's answer for one audio codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDecoderRegistration {
    /// The codec this registration decodes.
    pub codec: CodecDescriptor,
}

/// Audio codecs, parameters and decoders.
pub mod audio {
    use super::*;

    /// A codec identity. The numbering is this shim's own; only equality
    /// against the [`well_known`] constants is meaningful.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct AudioCodecId(pub u32);

    /// The identities edith names.
    pub mod well_known {
        use super::AudioCodecId;

        /// MPEG-4 AAC.
        pub const CODEC_ID_AAC: AudioCodecId = AudioCodecId(0x1000);
        /// Xiph Opus.
        pub const CODEC_ID_OPUS: AudioCodecId = AudioCodecId(0x1001);
        /// Xiph Vorbis.
        pub const CODEC_ID_VORBIS: AudioCodecId = AudioCodecId(0x1002);
        /// Xiph FLAC.
        pub const CODEC_ID_FLAC: AudioCodecId = AudioCodecId(0x1003);
        /// MPEG-1/2 Layer III.
        pub const CODEC_ID_MP3: AudioCodecId = AudioCodecId(0x1004);
        /// Apple Lossless.
        pub const CODEC_ID_ALAC: AudioCodecId = AudioCodecId(0x1005);
        /// Dolby AC-3.
        pub const CODEC_ID_AC3: AudioCodecId = AudioCodecId(0x1006);
        /// Dolby Digital Plus.
        pub const CODEC_ID_EAC3: AudioCodecId = AudioCodecId(0x1007);
        /// Dolby TrueHD. No decoder is registered for it, which is how a
        /// caller learns the track cannot be played.
        pub const CODEC_ID_TRUEHD: AudioCodecId = AudioCodecId(0x1008);
        /// DTS and its extensions. No decoder is registered for it either.
        pub const CODEC_ID_DTS: AudioCodecId = AudioCodecId(0x1009);
        /// Unsigned 8-bit PCM.
        pub const CODEC_ID_PCM_U8: AudioCodecId = AudioCodecId(0x1010);
        /// Signed 16-bit little-endian PCM.
        pub const CODEC_ID_PCM_S16LE: AudioCodecId = AudioCodecId(0x1011);
        /// Signed 16-bit big-endian PCM.
        pub const CODEC_ID_PCM_S16BE: AudioCodecId = AudioCodecId(0x1012);
        /// Signed 24-bit little-endian PCM.
        pub const CODEC_ID_PCM_S24LE: AudioCodecId = AudioCodecId(0x1013);
        /// Signed 32-bit little-endian PCM.
        pub const CODEC_ID_PCM_S32LE: AudioCodecId = AudioCodecId(0x1014);
        /// 32-bit float little-endian PCM.
        pub const CODEC_ID_PCM_F32LE: AudioCodecId = AudioCodecId(0x1015);
        /// Anything with no identity of its own here.
        pub const CODEC_ID_NULL: AudioCodecId = AudioCodecId(0);
    }

    /// A channel layout, as far as anything here asks: how many there are.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Channels(pub u32);

    impl Channels {
        /// Channels in the layout.
        pub fn count(&self) -> u32 {
            self.0
        }
    }

    /// Everything an audio decoder needs before its first packet.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct AudioCodecParameters {
        /// Which codec.
        pub codec: AudioCodecId,
        /// Sample rate in Hz.
        pub sample_rate: Option<u32>,
        /// Channel layout.
        pub channels: Option<Channels>,
        /// Coded bit depth, when the container states one.
        pub bits_per_sample: Option<u32>,
        /// Codec setup bytes: the AudioSpecificConfig, `OpusHead`, STREAMINFO.
        pub extra_data: Option<Box<[u8]>>,
    }

    impl AudioCodecParameters {
        /// Empty parameters, filled in by the `with_*` builders.
        pub fn new() -> AudioCodecParameters {
            AudioCodecParameters::default()
        }

        /// Name the codec.
        pub fn for_codec(&mut self, codec: AudioCodecId) -> &mut AudioCodecParameters {
            self.codec = codec;
            self
        }

        /// State the sample rate.
        pub fn with_sample_rate(&mut self, rate: u32) -> &mut AudioCodecParameters {
            self.sample_rate = Some(rate);
            self
        }

        /// State the channel layout.
        pub fn with_channels(&mut self, channels: Channels) -> &mut AudioCodecParameters {
            self.channels = Some(channels);
            self
        }

        /// State the bit depth.
        pub fn with_bits_per_sample(&mut self, bits: u32) -> &mut AudioCodecParameters {
            self.bits_per_sample = Some(bits);
            self
        }

        /// State the codec setup bytes.
        pub fn with_extra_data(&mut self, data: Box<[u8]>) -> &mut AudioCodecParameters {
            self.extra_data = Some(data);
            self
        }
    }

    /// Options a decoder is built with. Nothing here has any, which is what
    /// `Default` means for it.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct AudioDecoderOptions {
        /// Verify the decoded audio against a checksum where the format has
        /// one. Not implemented by any decoder in this family.
        pub verify: bool,
    }

    /// A decoded block, borrowed from the decoder until its next call.
    #[derive(Debug, Clone, Copy)]
    pub struct GenericAudioBufferRef<'a> {
        samples: &'a [f32],
        channels: usize,
    }

    impl<'a> GenericAudioBufferRef<'a> {
        /// Wrap interleaved samples.
        pub fn new(samples: &'a [f32], channels: usize) -> GenericAudioBufferRef<'a> {
            GenericAudioBufferRef { samples, channels }
        }

        /// Channels in the block.
        pub fn spec_channels(&self) -> usize {
            self.channels
        }

        /// Sample frames in the block.
        pub fn frames(&self) -> usize {
            match self.channels {
                0 => 0,
                n => self.samples.len() / n,
            }
        }

        /// Copy the block into `out`, interleaved, replacing what was there.
        pub fn copy_to_vec_interleaved<S: FromSample>(&self, out: &mut Vec<S>) {
            out.clear();
            out.extend(self.samples.iter().map(|&s| S::from_sample(s)));
        }
    }

    /// A sample type [`GenericAudioBufferRef::copy_to_vec_interleaved`] can
    /// convert to.
    pub trait FromSample {
        /// Convert one normalised `f32` sample.
        fn from_sample(s: f32) -> Self;
    }

    impl FromSample for f32 {
        fn from_sample(s: f32) -> f32 {
            s
        }
    }

    impl FromSample for i16 {
        fn from_sample(s: f32) -> i16 {
            (s.clamp(-1.0, 1.0) * 32767.0) as i16
        }
    }

    impl FromSample for i32 {
        fn from_sample(s: f32) -> i32 {
            (f64::from(s.clamp(-1.0, 1.0)) * 2147483647.0) as i32
        }
    }

    /// A decoder: packets in, one block out per packet.
    pub trait AudioDecoder: Send {
        /// Decode one packet.
        fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>>;

        /// The parameters this decoder was built with.
        fn codec_params(&self) -> &AudioCodecParameters;

        /// Drop state left over from before a seek.
        fn reset(&mut self);
    }
}

impl CodecParameters {
    /// The audio half, when this is an audio track.
    pub fn audio(&self) -> Option<&audio::AudioCodecParameters> {
        match self {
            CodecParameters::Audio(p) => Some(p),
            _ => None,
        }
    }
}

/// The identity a family [`ec_core::CodecId`] answers to here.
pub fn audio_codec_id(codec: ec_core::CodecId) -> audio::AudioCodecId {
    use audio::well_known::*;
    use ec_core::CodecId;
    match codec {
        CodecId::Aac => CODEC_ID_AAC,
        CodecId::Opus => CODEC_ID_OPUS,
        CodecId::Vorbis => CODEC_ID_VORBIS,
        CodecId::Flac => CODEC_ID_FLAC,
        CodecId::Mp3 => CODEC_ID_MP3,
        CodecId::Alac => CODEC_ID_ALAC,
        CodecId::Ac3 => CODEC_ID_AC3,
        CodecId::EAc3 => CODEC_ID_EAC3,
        CodecId::TrueHd => CODEC_ID_TRUEHD,
        CodecId::Dts => CODEC_ID_DTS,
        // Each PCM width keeps its own identity: mapping them all to one and
        // back would decode a 24-bit WAVE as 16-bit.
        CodecId::PcmU8 => CODEC_ID_PCM_U8,
        CodecId::PcmS16Le => CODEC_ID_PCM_S16LE,
        CodecId::PcmS16Be => CODEC_ID_PCM_S16BE,
        CodecId::PcmS24Le => CODEC_ID_PCM_S24LE,
        CodecId::PcmS32Le => CODEC_ID_PCM_S32LE,
        CodecId::PcmF32Le => CODEC_ID_PCM_F32LE,
        _ => CODEC_ID_NULL,
    }
}

/// The family codec an identity names, or [`None`] for one nothing here
/// decodes.
pub fn ec_codec_id(id: audio::AudioCodecId) -> Option<ec_core::CodecId> {
    use audio::well_known::*;
    use ec_core::CodecId;
    Some(match id {
        CODEC_ID_AAC => CodecId::Aac,
        CODEC_ID_OPUS => CodecId::Opus,
        CODEC_ID_VORBIS => CodecId::Vorbis,
        CODEC_ID_FLAC => CodecId::Flac,
        CODEC_ID_MP3 => CodecId::Mp3,
        CODEC_ID_ALAC => CodecId::Alac,
        CODEC_ID_AC3 => CodecId::Ac3,
        CODEC_ID_EAC3 => CodecId::EAc3,
        CODEC_ID_TRUEHD => CodecId::TrueHd,
        CODEC_ID_DTS => CodecId::Dts,
        CODEC_ID_PCM_U8 => CodecId::PcmU8,
        CODEC_ID_PCM_S16LE => CodecId::PcmS16Le,
        CODEC_ID_PCM_S16BE => CodecId::PcmS16Be,
        CODEC_ID_PCM_S24LE => CodecId::PcmS24Le,
        CODEC_ID_PCM_S32LE => CodecId::PcmS32Le,
        CODEC_ID_PCM_F32LE => CodecId::PcmF32Le,
        _ => return None,
    })
}

/// Family parameters for what a track states, for handing to a family decoder.
pub fn ec_parameters(
    params: &audio::AudioCodecParameters,
) -> crate::Result<ec_core::CodecParameters> {
    let codec = ec_codec_id(params.codec).ok_or_else(|| {
        crate::Error::Unsupported(format!("no decoder for codec id {}", params.codec.0))
    })?;
    let mut out = ec_core::CodecParameters::new(codec);
    out.extradata = params
        .extra_data
        .as_deref()
        .map(ec_core::packet::Buf::copy_from_slice);
    out.media = ec_core::registry::MediaParameters::Audio(ec_core::registry::AudioParameters {
        sample_rate: params.sample_rate.unwrap_or(48_000),
        layout: ec_core::ChannelLayout::from_count(
            params.channels.map_or(2, |c| c.count()).max(1) as usize
        ),
        format: None,
        bits_per_sample: params.bits_per_sample,
    });
    Ok(out)
}

/// What a track states, from the family's own description of it.
pub fn from_ec_parameters(params: &ec_core::CodecParameters) -> CodecParameters {
    match params.audio() {
        Some(audio) => {
            let mut out = audio::AudioCodecParameters::new();
            out.for_codec(audio_codec_id(params.codec))
                .with_sample_rate(audio.sample_rate)
                .with_channels(audio::Channels(audio.layout.channel_count() as u32));
            if let Some(bits) = audio.bits_per_sample {
                out.with_bits_per_sample(bits);
            }
            if let Some(extra) = &params.extradata {
                out.with_extra_data(extra.as_ref().to_vec().into_boxed_slice());
            }
            CodecParameters::Audio(out)
        }
        None => match params.video() {
            Some(video) => CodecParameters::Video(VideoCodecParameters {
                codec: audio_codec_id(params.codec).0,
                width: Some(video.width as u16),
                height: Some(video.height as u16),
            }),
            None => CodecParameters::Subtitle(SubtitleCodecParameters {
                codec: audio_codec_id(params.codec).0,
                extra_data: params
                    .extradata
                    .as_ref()
                    .map(|b| b.as_ref().to_vec().into_boxed_slice()),
            }),
        },
    }
}
