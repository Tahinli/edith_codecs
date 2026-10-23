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
//! LFE, DSE, PCE and FIL elements.  HE-AAC v1 (SBR) is reconstructed: the core
//! decodes, its low bands feed the SBR QMF/HF/envelope chain, and the output
//! is at the extension rate.  HE-AAC v2 adds Parametric Stereo (ISO/IEC
//! 14496-3 subpart 8), reconstructed as FFmpeg does: a PS stream decodes to
//! two channels at the extension rate.  A PS element whose PS header has not
//! been decoded yet outputs its mono SBR signal on both channels.  Malformed
//! PS data disables PS for that frame, with the same fallback.

#![forbid(unsafe_code)]

mod config;
mod decode;
mod encode;
mod huffman;
mod sbr_bands;
mod sbr_chain;
mod sbr_env;
mod sbr_hf;
mod sbr_payload;
pub mod sbr_qmf;
mod sbr_qmf_window;
mod ps_data;
mod ps_dsp;
mod ps_huff;
mod sbr_tables;
pub mod tables;
#[doc(hidden)]
pub mod knobs;

pub use config::{
    AOT_AAC_LC, AOT_PS, AOT_SBR, AdtsHeader, AudioSpecificConfig, ProgramConfig,
    audio_specific_config_bytes, channels_for_config, config_for_channels, is_adts, parse_adts,
    parse_audio_specific_config, parse_program_config, sample_rate_for_index, sf_index_for_rate,
    write_adts_header, write_audio_specific_config,
};
pub use decode::{FRAME_LEN, ToolSideInfoRow, WindowSequence, tool_sideinfo_log};
pub use ec_core::{Error, Result};
pub use encode::{AacEncoder, AacEncoderConfig, EncodedPacket, WindowShape};
/// Diagnostic only (round-20): per-(access-unit, channel) side-info rows
/// logged during decode when `EC_AAC_SBR_SIDEINFO_DEBUG` is set.
pub use sbr_chain::{SbrSideInfoRow, hold_call_count, sbr_sideinfo_log};
/// Diagnostic only (round-17 Task 1 conviction check): reads back the
/// per-absolute-QMF-band transmitted signal/noise energy sums
/// `EC_AAC_SBR_NOISE_FRACTION_DEBUG` accumulates during decode.
pub use sbr_env::noise_fraction_table;
pub use tables::SAMPLE_RATES;

use decode::BlockDecoder;
use ec_core::BitReader;

/// What this decoder does with a High Efficiency stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbrSupport {
    /// The stream is plain AAC-LC: nothing is missing.
    NotSignalled,
    /// SBR is signalled and **is** reconstructed: the decoded audio spans
    /// the full extension bandwidth at the extension sample rate.
    V1,
    /// SBR is signalled but not reconstructed. Not produced by this decoder
    /// any more: Parametric Stereo streams report [`SbrSupport::V1`].
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
        d.block.set_pce(cfg.pce.clone());
        d.sample_rate = cfg.sample_rate;
        d.channels = cfg.channels;
        // SBR (and Parametric Stereo, for HE-AACv2) is reconstructed: the
        // core is decoded first, then the extension-rate chain runs on it.
        if cfg.sbr_present {
            let rate = cfg.extension_sample_rate.unwrap_or(cfg.sample_rate * 2);
            d.block.set_sbr_rate(Some(rate));
            d.block.set_sbr_ps(cfg.ps_present);
        } else if !cfg.ps_present {
            // Plain AAC-LC config: an SBR payload may still follow (implicit signalling).
            d.block.set_implicit_core(cfg.sample_rate);
        }
        if cfg.ps_present {
            // PS output is stereo whatever the core's channel count.
            d.channels = 2;
        }
        d.config = Some(cfg);
        d
    }

    /// Core-only decoding: SBR and PS are never applied. An explicit SBR or PS
    /// config is dropped, and implicit SBR is never armed, so every SBR extension
    /// payload is skipped and the output stays at the core rate.
    pub fn core_only(mut self) -> AacDecoder {
        self.block.set_sbr_rate(None);
        self.block.disable_implicit_sbr();
        if let Some(c) = self.config.as_mut() {
            c.sbr_present = false;
            c.ps_present = false;
            c.extension_sample_rate = None;
        }
        self
    }

    /// A decoder configured from raw AudioSpecificConfig bytes -- an mp4
    /// `esds` payload or a Matroska `CodecPrivate`.
    pub fn with_config_bytes(data: &[u8]) -> Result<AacDecoder> {
        Ok(AacDecoder::with_config(parse_audio_specific_config(data)?))
    }

    /// Successful access units that used perceptual noise substitution.
    pub fn pns_aus(&self) -> usize {
        self.block.pns_aus()
    }

    /// The configuration this decoder was built with, if any.
    pub fn config(&self) -> Option<&AudioSpecificConfig> {
        self.config.as_ref()
    }

    /// Whether the stream asked for SBR, and what became of it. A plain AAC-LC
    /// stream whose frames carry an SBR payload (implicit signalling) reports
    /// [`SbrSupport::V1`] once the first such payload has been decoded; see
    /// [`AacDecoder::sbr_implicit`].
    pub fn sbr_support(&self) -> SbrSupport {
        if self.block.implicit_sbr() {
            return SbrSupport::V1;
        }
        match &self.config {
            Some(c) if c.ps_present || c.sbr_present => SbrSupport::V1,
            _ => SbrSupport::NotSignalled,
        }
    }

    /// Whether SBR was switched on by an implicit payload: a stream that
    /// signals plain AAC-LC (config or ADTS header) but carries an SBR
    /// extension payload, decoded at twice the core rate as ffmpeg and the
    /// ISO reference do. Only ever true once the first such payload has been
    /// decoded, and only for a core rate of at most 24 kHz. Explicit SBR
    /// signalling is reported by [`AacDecoder::sbr_support`] alone.
    pub fn sbr_implicit(&self) -> bool {
        self.block.implicit_sbr()
    }

    /// Whether Parametric Stereo was switched on by an implicit payload: a
    /// stream that signals plain AAC-LC but whose SBR extension carries a PS
    /// extension (the HE-AACv2 form with no explicit signalling). Only true once
    /// such a PS extension has been decoded, so a caller reads it after the
    /// first frames. Implies [`AacDecoder::sbr_implicit`].
    pub fn ps_implicit(&self) -> bool {
        self.block.implicit_ps()
    }

    /// The rate the samples this decoder hands back are actually at: the
    /// extension rate for a reconstructed (V1) HE-AAC or HE-AACv2 stream,
    /// the core rate for plain AAC-LC -- claiming bandwidth that was not
    /// actually rebuilt would be a lie.
    ///
    /// For a decoder built from an ADTS header (no config), this is `None`
    /// until the first frame has been decoded. After that it is the core
    /// rate, or twice it once implicit SBR has switched on, so a caller
    /// should read it again after each packet.
    pub fn output_sample_rate(&self) -> Option<u32> {
        let (core, ext) = match &self.config {
            Some(c) => (c.sample_rate, c.extension_sample_rate),
            None if self.sample_rate != 0 => (self.sample_rate, None),
            None => return None,
        };
        Some(match self.sbr_support() {
            SbrSupport::V1 => ext.unwrap_or(core * 2),
            _ => core,
        })
    }

    /// Decodes one packet: a raw `raw_data_block`, or one or more ADTS frames.
    pub fn decode(&mut self, packet: &[u8], pts: Option<i64>) -> Result<DecodedAudio> {
        let mut planes: Vec<Vec<f32>> = Vec::new();
        if is_adts(packet) {
            let mut at = 0usize;
            while at + 7 <= packet.len() {
                let header = parse_adts(&packet[at..])?;
                let end = (at + header.frame_length).min(packet.len());
                // A truncated frame can end before its header does; clamp so the slice cannot invert.
                let body = &packet[(at + header.header_len).min(end)..end];
                if self.config.is_none() {
                    self.sample_rate = header.sample_rate;
                    self.channels = header.channels;
                    self.block.set_sf_index(header.sf_index);
                    self.block.set_implicit_core(header.sample_rate);
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
        // ffmpeg decodes an HE-AAC mono stream with no PS payload (SBR reconstructed, one
        // plane) as two identical channels ("Treating HE-AAC mono as stereo"). Match it.
        if planes.len() == 1 && self.sbr_support() == SbrSupport::V1 {
            let dup = planes[0].clone();
            planes.push(dup);
        }
        let channels = planes.len();
        if channels == 0 {
            return Ok(DecodedAudio {
                sample_rate: self.output_sample_rate().unwrap_or(self.sample_rate),
                channels: self.channels,
                samples: Vec::new(),
                pts,
            });
        }
        if self.channels == 0 {
            self.channels = channels as u16;
        }
        // A PCE names each plane's speaker, which orders the output; otherwise the
        // channel configuration's bitstream order does.
        let order = self
            .block
            .plane_order(channels)
            .unwrap_or_else(|| film_order(channels).to_vec());
        let frames = planes[0].len();
        let mut samples = vec![0.0f32; frames * channels];
        for (out_ch, &src) in order.iter().take(channels).enumerate() {
            let plane = &planes[src.min(channels - 1)];
            // Planes can differ in length after `merge`; only the first `frames` samples fit.
            for (i, &v) in plane.iter().take(frames).enumerate() {
                samples[i * channels + out_ch] = v;
            }
        }
        Ok(DecodedAudio {
            sample_rate: self.output_sample_rate().unwrap_or(self.sample_rate),
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
