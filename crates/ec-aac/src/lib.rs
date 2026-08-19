//! AAC-LC: decoder (mono through 7.1) and encoder, in safe Rust.
//!
//! # Where the tables come from
//!
//! The Huffman codebooks, the scalefactor codebook and the scalefactor-band
//! offsets in [`tables`] were **derived**, not transcribed: `scripts/derive-aac-tables.py`
//! synthesises access units whose spectral payload is a chosen bit string, has a
//! reference decoder decode them, and reads the quantised spectrum back through
//! the inverse MDCT.  Walking the binary tree of probes enumerates each codebook
//! in about `2M` frames for `M` codewords.  Every table came out with a Kraft
//! sum of exactly 1 and the entry count ISO/IEC 14496-3 prescribes, which is
//! what makes the derivation self-checking rather than a transcription to be
//! trusted.
//!
//! # What is here and what is not
//!
//! AAC-LC (audio object type 2) is complete: all four window sequences, both
//! window shapes, TNS, pulse data, M/S and intensity stereo, PNS, and SCE, CPE,
//! LFE, DSE, PCE and FIL elements.  HE-AAC's SBR and Parametric Stereo are
//! **not** implemented; a stream that signals them decodes to its AAC-LC core,
//! at the core sample rate, and says so through [`AacDecoder::sbr_support`] and
//! [`AacDecoder::output_sample_rate`] rather than silently claiming the
//! extension rate.  See `sbr_is_reported_not_silently_upsampled` in the tests.

#![forbid(unsafe_code)]

mod config;
mod decode;
mod encode;
mod huffman;
mod sbr_bands;
mod sbr_payload;
mod sbr_tables;
pub mod tables;

pub use config::{
    AOT_AAC_LC, AOT_PS, AOT_SBR, AdtsHeader, AudioSpecificConfig, ProgramConfig,
    audio_specific_config_bytes, channels_for_config, config_for_channels, is_adts, parse_adts,
    parse_audio_specific_config, parse_program_config, sample_rate_for_index, sf_index_for_rate,
    write_adts_header, write_audio_specific_config,
};
pub use decode::{FRAME_LEN, WindowSequence};
pub use ec_core::{Error, Result};
pub use encode::{AacEncoder, AacEncoderConfig, EncodedPacket, WindowShape};
pub use tables::SAMPLE_RATES;

use decode::BlockDecoder;
use ec_core::BitReader;

/// What this decoder does with a High Efficiency stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbrSupport {
    /// The stream is plain AAC-LC: nothing is missing.
    NotSignalled,
    /// SBR (and possibly PS) is signalled and is **not** reconstructed. The
    /// AAC-LC core decodes, at the core rate, half the signalled bandwidth.
    CoreOnly,
}

/// One decoded frame: interleaved `f32` in film channel order.
#[derive(Clone, Debug, Default)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples, `frames() * channels` of them.
    pub samples: Vec<f32>,
    pub pts: Option<i64>,
}

impl DecodedAudio {
    /// Sample frames in this block.
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / usize::from(self.channels)
        }
    }
}

/// Bitstream element order to the film order `FL, FR, FC, LFE, BL, BR, SL, SR`
/// that every downmix in this family folds (ISO/IEC 14496-3 tbl 1.19: the
/// elements arrive centre first, LFE last).
fn film_order(channels: usize) -> &'static [usize] {
    match channels {
        3 => &[1, 2, 0],
        4 => &[1, 2, 0, 3],
        5 => &[1, 2, 0, 3, 4],
        6 => &[1, 2, 0, 5, 3, 4],
        8 => &[1, 2, 0, 7, 5, 6, 3, 4],
        _ => &[0, 1, 2, 3, 4, 5, 6, 7],
    }
}

/// An AAC-LC decoder.
pub struct AacDecoder {
    block: BlockDecoder,
    config: Option<AudioSpecificConfig>,
    sample_rate: u32,
    channels: u16,
}

impl AacDecoder {
    /// A decoder with no configuration yet: it will take its parameters from
    /// the first ADTS header it sees.
    pub fn new() -> AacDecoder {
        AacDecoder {
            block: BlockDecoder::new(3),
            config: None,
            sample_rate: 0,
            channels: 0,
        }
    }

    /// A decoder configured from a parsed AudioSpecificConfig.
    pub fn with_config(cfg: AudioSpecificConfig) -> AacDecoder {
        let mut d = AacDecoder::new();
        d.block.set_sf_index(cfg.sf_index);
        d.sample_rate = cfg.sample_rate;
        d.channels = cfg.channels;
        d.config = Some(cfg);
        d
    }

    /// A decoder configured from raw AudioSpecificConfig bytes -- an mp4
    /// `esds` payload or a Matroska `CodecPrivate`.
    pub fn with_config_bytes(data: &[u8]) -> Result<AacDecoder> {
        Ok(AacDecoder::with_config(parse_audio_specific_config(data)?))
    }

    /// The configuration this decoder was built with, if any.
    pub fn config(&self) -> Option<&AudioSpecificConfig> {
        self.config.as_ref()
    }

    /// Whether the stream asked for SBR, and what became of it.
    pub fn sbr_support(&self) -> SbrSupport {
        match &self.config {
            Some(c) if c.sbr_present || c.ps_present => SbrSupport::CoreOnly,
            _ => SbrSupport::NotSignalled,
        }
    }

    /// The rate the samples this decoder hands back are actually at.
    ///
    /// For an HE-AAC stream this is the **core** rate, not the doubled rate the
    /// configuration advertises: the SBR extension is not reconstructed, so
    /// claiming its rate would be claiming bandwidth that is not there.
    pub fn output_sample_rate(&self) -> Option<u32> {
        self.config.as_ref().map(|c| c.sample_rate)
    }

    /// Decodes one packet: a raw `raw_data_block`, or one or more ADTS frames.
    pub fn decode(&mut self, packet: &[u8], pts: Option<i64>) -> Result<DecodedAudio> {
        let mut planes: Vec<Vec<f32>> = Vec::new();
        if is_adts(packet) {
            let mut at = 0usize;
            while at + 7 <= packet.len() {
                let header = parse_adts(&packet[at..])?;
                let end = (at + header.frame_length).min(packet.len());
                let body = &packet[at + header.header_len..end];
                if self.config.is_none() {
                    self.sample_rate = header.sample_rate;
                    self.channels = header.channels;
                    self.block.set_sf_index(header.sf_index);
                }
                let mut r = BitReader::new(body);
                for _ in 0..header.raw_blocks {
                    let block = self.block.raw_data_block(&mut r)?;
                    merge(&mut planes, block);
                    if r.bits_remaining() < 8 {
                        break;
                    }
                }
                if header.frame_length == 0 {
                    break;
                }
                at += header.frame_length;
            }
        } else {
            let mut r = BitReader::new(packet);
            planes = self.block.raw_data_block(&mut r)?;
        }
        let channels = planes.len();
        if channels == 0 {
            return Ok(DecodedAudio {
                sample_rate: self.sample_rate,
                channels: self.channels,
                samples: Vec::new(),
                pts,
            });
        }
        if self.channels == 0 {
            self.channels = channels as u16;
        }
        let order = film_order(channels);
        let frames = planes[0].len();
        let mut samples = vec![0.0f32; frames * channels];
        for (out_ch, &src) in order.iter().take(channels).enumerate() {
            let plane = &planes[src.min(channels - 1)];
            for (i, &v) in plane.iter().enumerate() {
                samples[i * channels + out_ch] = v;
            }
        }
        Ok(DecodedAudio {
            sample_rate: self.sample_rate,
            channels: channels as u16,
            samples,
            pts,
        })
    }
}

/// Appends a block's planes to the packet's, growing the plane count if a later
/// `raw_data_block` carries more channels than an earlier one.
fn merge(planes: &mut Vec<Vec<f32>>, block: Vec<Vec<f32>>) {
    if planes.is_empty() {
        *planes = block;
        return;
    }
    for (i, plane) in block.into_iter().enumerate() {
        match planes.get_mut(i) {
            Some(dst) => dst.extend_from_slice(&plane),
            None => planes.push(plane),
        }
    }
}

impl Default for AacDecoder {
    fn default() -> AacDecoder {
        AacDecoder::new()
    }
}
