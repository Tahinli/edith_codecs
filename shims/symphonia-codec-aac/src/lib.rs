//! Compatibility shim: the `symphonia-codec-aac` 0.6 surface edith consumes,
//! served by [`ec_aac`].
//!
//! One difference from the incumbent, and it is the reason this exists:
//! **multichannel decodes**. The incumbent refuses anything wider than stereo
//! outright (`aac/mod.rs:93`, "aac: aac too complex"), which is exactly the 5.1
//! and 7.1 tracks of a film — edith routes those to a second AAC crate to get
//! around it. [`ec_aac`] decodes mono through 7.1 in film channel order, so
//! there is nothing to route around.
//!
//! Written from the signatures edith calls (`AacDecoder::try_new`, then the
//! `AudioDecoder` trait) and the crate's published documentation; no symphonia
//! source was read.

#![forbid(unsafe_code)]

use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, GenericAudioBufferRef,
    well_known::CODEC_ID_AAC,
};
use symphonia_core::packet::Packet;
use symphonia_core::{Error, Result};

/// An AAC-LC decoder for one track.
pub struct AacDecoder {
    inner: ec_aac::AacDecoder,
    params: AudioCodecParameters,
    out: Vec<f32>,
    channels: usize,
}

impl AacDecoder {
    /// A decoder for the track `params` describes.
    ///
    /// The `extra_data` is the `AudioSpecificConfig`; a track with none (raw
    /// ADTS, which states its configuration in every frame) is accepted and
    /// configures itself from the first frame.
    pub fn try_new(
        params: &AudioCodecParameters,
        _options: &AudioDecoderOptions,
    ) -> Result<AacDecoder> {
        if params.codec != CODEC_ID_AAC {
            return Err(Error::Unsupported(format!(
                "codec id {} is not AAC",
                params.codec.0
            )));
        }
        let inner = match params.extra_data.as_deref() {
            Some(asc) => ec_aac::AacDecoder::with_config_bytes(asc)?,
            None => ec_aac::AacDecoder::new(),
        };
        let channels = params.channels.map_or(2, |c| c.count()).max(1) as usize;
        Ok(AacDecoder {
            inner,
            params: params.clone(),
            out: Vec::new(),
            channels,
        })
    }
}

impl AudioDecoder for AacDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        let audio = self.inner.decode(&packet.data, Some(packet.pts.value()))?;
        self.channels = usize::from(audio.channels).max(1);
        self.out.clear();
        self.out.extend_from_slice(&audio.samples);
        Ok(GenericAudioBufferRef::new(&self.out, self.channels))
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn reset(&mut self) {
        self.inner = match self.params.extra_data.as_deref() {
            Some(asc) => ec_aac::AacDecoder::with_config_bytes(asc).unwrap_or_default(),
            None => ec_aac::AacDecoder::new(),
        };
    }
}
