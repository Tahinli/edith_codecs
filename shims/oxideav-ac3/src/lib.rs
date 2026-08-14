//! `oxideav-ac3` as edith consumes it, over [`ec_ac3`].
//!
//! A shim, not a port: it carries the incumbent's package name and version so
//! the swap is a `[patch.crates-io]` line, and it exposes exactly the items the
//! replica names — the header surface it reads a track's rate and layout out of
//! ([`syncinfo::parse`], [`bsi::parse`], [`eac3::bsi::parse`], `audio.rs:1954`,
//! `2063`) and [`register_codecs`], which is how it gets a decoder at all
//! (`audio.rs:2132`). The header modules are `ec_ac3`'s own: their signatures,
//! their entry offsets (`bsi` after the five `syncinfo()` bytes, `eac3::bsi`
//! after the 16-bit sync word) and their fields already match what the replica
//! writes, so a re-export is the whole adapter.
//!
//! Two differences are owned here rather than at the call site:
//!
//! * **Sample format.** The incumbent hands out S16 little-endian and the
//!   replica reads it as such (`audio.rs:2160`, `chunks_exact(2)` over
//!   `/32768.0`); `ec_ac3` decodes to `f32`. The conversion is
//!   [`Ac3Decoder::receive_frame`], once, for every caller — which is why this
//!   shim writes its own [`oxideav_core::Decoder`] instead of wrapping
//!   `oxideav_core::EcDecoder`, whose job is the frames that need no
//!   conversion.
//! * **Downmix request.** The incumbent takes the wanted channel count on
//!   [`CodecParameters::channels`] and `ec_ac3` takes a [`Downmix`] mode; the
//!   factory translates. `Some(2)` is the A/52 §7.8 stereo fold the replica
//!   asks for on anything wider than mono, `None` the stream's own layout.

#![forbid(unsafe_code)]

pub use ec_ac3::{bsi, eac3, syncinfo};

use ec_ac3::{Downmix, Options};
use oxideav_core::{
    AudioFrame, CodecId, CodecInfo, CodecParameters, CodecRegistry, Decoder, Frame, Packet, Result,
};

/// Register this crate's codecs — `ac3` and `eac3` — into `registry`.
///
/// Both ids reach the same decoder: it dispatches per packet on `bsid`, so the
/// id picks the capability set and not the syntax.
pub fn register_codecs(registry: &mut CodecRegistry) {
    for id in ["ac3", "eac3"] {
        registry.register(CodecInfo::new(CodecId::new(id)).decoder(open));
    }
}

/// A decoder for `params`, folded down to `params.channels`.
fn open(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let options = Options {
        downmix: match params.channels {
            Some(1) => Downmix::Mono,
            Some(2) => Downmix::Stereo,
            _ => Downmix::Native,
        },
        ..Options::default()
    };
    Ok(Box::new(Ac3Decoder {
        id: params.codec_id.clone(),
        inner: ec_ac3::Ac3Decoder::with_options(options),
    }))
}

/// The family's AC-3 decoder: one syncframe in, one S16 little-endian frame
/// out.
struct Ac3Decoder {
    id: CodecId,
    inner: ec_ac3::Ac3Decoder,
}

impl Decoder for Ac3Decoder {
    fn codec_id(&self) -> &CodecId {
        &self.id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        Ok(ec_core::Decoder::send_packet(&mut self.inner, packet)?)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match ec_core::Decoder::receive_frame(&mut self.inner)? {
            ec_core::Frame::Audio(audio) => Ok(Frame::Audio(AudioFrame {
                samples: audio.samples as u32,
                pts: audio.pts.map(|ts| ts.ticks),
                data: audio.data.iter().map(|plane| s16le(plane)).collect(),
            })),
            other => Ok(other.into()),
        }
    }
}

/// Interleaved `f32` bytes as S16 little-endian, which is what the incumbent
/// speaks. Full scale is 32768 in both directions, saturating: the replica's
/// inverse is `/32768.0`, and a sample at or past `+1.0` has to land on
/// `i16::MAX` rather than wrap to silence.
fn s16le(samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() / 2);
    for chunk in samples.chunks_exact(4) {
        let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let scaled = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&scaled.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_saturates_instead_of_wrapping() {
        let pcm: Vec<u8> = [-1.5f32, -1.0, 0.0, 0.5, 1.0, 1.5]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let got: Vec<i16> = s16le(&pcm)
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect();
        assert_eq!(got, [-32768, -32768, 0, 16384, 32767, 32767]);
    }

    #[test]
    fn both_ids_are_registered_and_a_third_is_not() {
        let mut registry = CodecRegistry::new();
        register_codecs(&mut registry);
        assert!(registry.has_decoder(&CodecId::new("ac3")));
        assert!(registry.has_decoder(&CodecId::new("eac3")));
        assert!(!registry.has_decoder(&CodecId::new("truehd")));
    }
}
