//! AudioSpecificConfig (ISO/IEC 14496-3 §1.6.2) and ADTS (§1.A.2) framing.

use ec_core::{BitReader, BitWriter, Error, Result};

use crate::tables::SAMPLE_RATES;

/// AAC Low Complexity.
pub const AOT_AAC_LC: u8 = 2;
/// Spectral Band Replication (HE-AAC v1).
pub const AOT_SBR: u8 = 5;
/// Parametric Stereo (HE-AAC v2).
pub const AOT_PS: u8 = 29;

/// A rate's `samplingFrequencyIndex`, or `None` when AAC cannot carry it.
pub fn sf_index_for_rate(rate: u32) -> Option<u8> {
    SAMPLE_RATES
        .iter()
        .position(|&r| r == rate)
        .map(|i| i as u8)
}

/// The rate a `samplingFrequencyIndex` names; 0 for the escape and reserved
/// values.
pub fn sample_rate_for_index(index: u8) -> u32 {
    SAMPLE_RATES.get(usize::from(index)).copied().unwrap_or(0)
}

/// A decoded AudioSpecificConfig.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSpecificConfig {
    /// Audio object type; 2 is AAC-LC, the only one this decoder codes.
    pub object_type: u8,
    pub sample_rate: u32,
    pub sf_index: u8,
    /// Channels the configuration implies, PCE included.
    pub channels: u16,
    /// `channelConfiguration`; 0 means the layout came from a PCE.
    pub channel_config: u8,
    /// True when the config signals SBR, i.e. HE-AAC.
    pub sbr_present: bool,
    /// True when the config signals Parametric Stereo, i.e. HE-AAC v2.
    pub ps_present: bool,
    /// The rate SBR would output at, when signalled.
    pub extension_sample_rate: Option<u32>,
}

/// Channels implied by `channelConfiguration` (ISO 14496-3 tbl 1.19).
pub fn channels_for_config(config: u8) -> u16 {
    match config {
        1..=6 => u16::from(config),
        7 => 8,
        _ => 0,
    }
}

fn object_type(r: &mut BitReader<'_>) -> Result<u8> {
    let t = r.read_bits(5)? as u8;
    if t == 31 {
        return Ok(32 + r.read_bits(6)? as u8);
    }
    Ok(t)
}

fn sampling_frequency(r: &mut BitReader<'_>) -> Result<(u8, u32)> {
    let index = r.read_bits(4)? as u8;
    if index == 15 {
        return Ok((index, r.read_bits(24)?));
    }
    Ok((index, sample_rate_for_index(index)))
}

/// Parses an AudioSpecificConfig, the `esds`/`CodecPrivate` payload.
pub fn parse_audio_specific_config(data: &[u8]) -> Result<AudioSpecificConfig> {
    let mut r = BitReader::new(data);
    let mut object = object_type(&mut r)?;
    let (mut sf_index, mut rate) = sampling_frequency(&mut r)?;
    let mut channel_config = r.read_bits(4)? as u8;
    let mut sbr = false;
    let mut ps = false;
    let mut ext_rate = None;
    if object == AOT_SBR || object == AOT_PS {
        sbr = true;
        ps = object == AOT_PS;
        let (_, er) = sampling_frequency(&mut r)?;
        ext_rate = Some(er);
        object = object_type(&mut r)?;
        if object == 22 {
            let _ext_channel_config = r.read_bits(4)?;
        }
    } else if object == AOT_AAC_LC {
        // GASpecificConfig; the fields matter only for the PCE that may follow.
        let _frame_length_flag = r.read_bit()?;
        if r.read_bit()? {
            let _core_coder_delay = r.read_bits(14)?;
        }
        let _extension_flag = r.read_bit()?;
        if channel_config == 0 {
            let pce = parse_program_config(&mut r)?;
            channel_config = 0;
            sf_index = pce.sf_index;
            rate = sample_rate_for_index(pce.sf_index);
            return Ok(AudioSpecificConfig {
                object_type: object,
                sample_rate: rate,
                sf_index,
                channels: pce.channels,
                channel_config,
                sbr_present: false,
                ps_present: false,
                extension_sample_rate: None,
            });
        }
        // A backward-compatible SBR signal rides in a sync extension at the end.
        if r.bits_remaining() >= 16 && r.read_bits(11)? == 0x2B7 {
            let ext = object_type(&mut r)?;
            if ext == AOT_SBR && r.read_bit()? {
                sbr = true;
                let (_, er) = sampling_frequency(&mut r)?;
                ext_rate = Some(er);
            }
        }
    }
    if rate == 0 {
        return Err(Error::corrupt("aac: reserved samplingFrequencyIndex"));
    }
    Ok(AudioSpecificConfig {
        object_type: object,
        sample_rate: rate,
        sf_index,
        channels: channels_for_config(channel_config),
        channel_config,
        sbr_present: sbr,
        ps_present: ps,
        extension_sample_rate: ext_rate,
    })
}

/// Serialises the two-byte (plus SBR extension) AudioSpecificConfig.
pub fn write_audio_specific_config(cfg: &AudioSpecificConfig) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(u32::from(cfg.object_type), 5);
    w.write_bits(u32::from(cfg.sf_index), 4);
    w.write_bits(u32::from(cfg.channel_config), 4);
    w.write_bit(false); // frameLengthFlag: 1024 samples
    w.write_bit(false); // dependsOnCoreCoder
    w.write_bit(false); // extensionFlag
    w.align_to_byte();
    w.into_bytes()
}

/// The AudioSpecificConfig an AAC-LC track of this shape needs.
pub fn audio_specific_config_bytes(sample_rate: u32, channels: u16) -> Vec<u8> {
    let sf_index = sf_index_for_rate(sample_rate).unwrap_or(3);
    write_audio_specific_config(&AudioSpecificConfig {
        object_type: AOT_AAC_LC,
        sample_rate,
        sf_index,
        channels,
        channel_config: config_for_channels(channels),
        sbr_present: false,
        ps_present: false,
        extension_sample_rate: None,
    })
}

/// The `channelConfiguration` that carries this many channels.
pub fn config_for_channels(channels: u16) -> u8 {
    match channels {
        1..=6 => channels as u8,
        8 => 7,
        _ => 0,
    }
}

/// What a program_config_element says about the stream.
#[derive(Clone, Debug, Default)]
pub struct ProgramConfig {
    pub sf_index: u8,
    pub channels: u16,
}

/// Parses a program_config_element far enough to count its channels.
pub fn parse_program_config(r: &mut BitReader<'_>) -> Result<ProgramConfig> {
    let _tag = r.read_bits(4)?;
    let _object_type = r.read_bits(2)?;
    let sf_index = r.read_bits(4)? as u8;
    let front = r.read_bits(4)? as usize;
    let side = r.read_bits(4)? as usize;
    let back = r.read_bits(4)? as usize;
    let lfe = r.read_bits(2)? as usize;
    let assoc = r.read_bits(3)? as usize;
    let cc = r.read_bits(4)? as usize;
    if r.read_bit()? {
        let _mono_mixdown = r.read_bits(4)?;
    }
    if r.read_bit()? {
        let _stereo_mixdown = r.read_bits(4)?;
    }
    if r.read_bit()? {
        let _matrix_mixdown = r.read_bits(3)?;
    }
    let mut channels = 0u16;
    for _ in 0..front + side + back {
        channels += if r.read_bit()? { 2 } else { 1 };
        let _tag = r.read_bits(4)?;
    }
    channels += lfe as u16;
    for _ in 0..lfe + assoc {
        let _tag = r.read_bits(4)?;
    }
    for _ in 0..cc {
        let _is_ind_sw = r.read_bit()?;
        let _tag = r.read_bits(4)?;
    }
    r.align_to_byte();
    let comment = r.read_bits(8)? as u64;
    r.skip_bits(comment * 8)?;
    Ok(ProgramConfig { sf_index, channels })
}

pub(crate) fn skip_program_config(r: &mut BitReader<'_>) -> Result<()> {
    parse_program_config(r).map(|_| ())
}

/// An ADTS frame header (§1.A.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdtsHeader {
    pub object_type: u8,
    pub sf_index: u8,
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_config: u8,
    /// Whole frame length in bytes, header included.
    pub frame_length: usize,
    /// Header length in bytes: 7, or 9 when the CRC is present.
    pub header_len: usize,
    pub raw_blocks: u8,
}

/// True when `data` opens on an ADTS syncword.
pub fn is_adts(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xFF && data[1] & 0xF6 == 0xF0
}

/// Parses one ADTS header.
pub fn parse_adts(data: &[u8]) -> Result<AdtsHeader> {
    if data.len() < 7 {
        return Err(Error::NeedMore);
    }
    let mut r = BitReader::new(data);
    if r.read_bits(12)? != 0xFFF {
        return Err(Error::corrupt("aac: no ADTS syncword"));
    }
    let _id = r.read_bit()?;
    let layer = r.read_bits(2)?;
    if layer != 0 {
        return Err(Error::corrupt("aac: ADTS layer is not zero"));
    }
    let protection_absent = r.read_bit()?;
    let object_type = r.read_bits(2)? as u8 + 1;
    let sf_index = r.read_bits(4)? as u8;
    let _private = r.read_bit()?;
    let channel_config = r.read_bits(3)? as u8;
    let _original = r.read_bit()?;
    let _home = r.read_bit()?;
    let _copyright_id = r.read_bit()?;
    let _copyright_start = r.read_bit()?;
    let frame_length = r.read_bits(13)? as usize;
    let _fullness = r.read_bits(11)?;
    let raw_blocks = r.read_bits(2)? as u8 + 1;
    let header_len = if protection_absent { 7 } else { 9 };
    if frame_length < header_len {
        return Err(Error::corrupt("aac: ADTS frame shorter than its header"));
    }
    let sample_rate = sample_rate_for_index(sf_index);
    if sample_rate == 0 {
        return Err(Error::corrupt("aac: reserved samplingFrequencyIndex"));
    }
    Ok(AdtsHeader {
        object_type,
        sf_index,
        sample_rate,
        channels: channels_for_config(channel_config),
        channel_config,
        frame_length,
        header_len,
        raw_blocks,
    })
}

/// Serialises a 7-byte ADTS header (no CRC).
pub fn write_adts_header(header: &AdtsHeader) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(7);
    w.write_bits(0xFFF, 12);
    w.write_bit(false); // MPEG-4
    w.write_bits(0, 2); // layer
    w.write_bit(true); // protection absent
    w.write_bits(u32::from(header.object_type.saturating_sub(1)), 2);
    w.write_bits(u32::from(header.sf_index), 4);
    w.write_bit(false); // private
    w.write_bits(u32::from(header.channel_config), 3);
    w.write_bit(false); // original
    w.write_bit(false); // home
    w.write_bit(false); // copyright id
    w.write_bit(false); // copyright start
    w.write_bits(header.frame_length as u32, 13);
    w.write_bits(0x7FF, 11); // buffer fullness: variable rate
    w.write_bits(u32::from(header.raw_blocks.saturating_sub(1)), 2);
    w.into_bytes()
}
