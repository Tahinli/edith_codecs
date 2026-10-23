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
    /// The program config element the config carries when `channel_config` is 0. Raw
    /// blocks may then omit it, so their speaker layout comes from here.
    pub pce: Vec<PceElement>,
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
                pce: pce.elements,
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
        pce: Vec::new(),
    })
}

/// Serialises the two-byte (plus SBR extension) AudioSpecificConfig.
///
/// `AOT_SBR`/`AOT_PS` are the *explicit* signalling form (§1.6.2.1's "SBR
/// config"): a core `sf_index`/`channel_config`, then the extension
/// (`SBR`-output) `sf_index` and an `extensionAudioObjectType` of AAC-LC
/// before the GASpecificConfig that form always carries -- without those
/// fields a decoder that trusts the object type (rather than only its
/// bare-ADTS implicit per-frame detection) has no extension rate to read and
/// never reports itself as HE-AAC.
pub fn write_audio_specific_config(cfg: &AudioSpecificConfig) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(u32::from(cfg.object_type), 5);
    w.write_bits(u32::from(cfg.sf_index), 4);
    w.write_bits(u32::from(cfg.channel_config), 4);
    if cfg.object_type == AOT_SBR || cfg.object_type == AOT_PS {
        let ext_rate = cfg.extension_sample_rate.unwrap_or(cfg.sample_rate * 2);
        let ext_sf_index = sf_index_for_rate(ext_rate).unwrap_or(3);
        w.write_bits(u32::from(ext_sf_index), 4);
        w.write_bits(u32::from(AOT_AAC_LC), 5); // extensionAudioObjectType
    }
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
        pce: Vec::new(),
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

/// A loudspeaker position, named as FFmpeg's channel masks name it. The derived
/// order is the mask bit order, which is the order FFmpeg writes its output in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(clippy::upper_case_acronyms)] // FFmpeg's and ISO's own speaker names.
pub enum Speaker {
    FL,
    FR,
    FC,
    LFE,
    BL,
    BR,
    FLC,
    FRC,
    BC,
    SL,
    SR,
}

/// One element a program_config_element lists: its syntactic kind (`0` SCE, `1` CPE,
/// `3` LFE, as in the raw data block's `id_syn_ele`), its instance tag, and the
/// speaker of each of its channels. `speakers` is empty when the PCE's layout is
/// one this crate does not name, and the decoder then keeps the bitstream order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PceElement {
    pub kind: u8,
    pub tag: u8,
    pub speakers: Vec<Speaker>,
}

/// What a program_config_element says about the stream.
#[derive(Clone, Debug, Default)]
pub struct ProgramConfig {
    pub sf_index: u8,
    pub channels: u16,
    /// The listed elements, in PCE order (front, side, back, LFE).
    pub elements: Vec<PceElement>,
}

/// Names the speakers of a PCE's elements from the list each sits in.
///
/// Front elements run from the centre outwards: a centre SCE is FC, the outermost
/// front pair is FL/FR, and an inner pair is FLC/FRC. Side and back pairs are SL/SR
/// and BL/BR, a back SCE is BC, and each LFE is LFE. A layout outside those shapes
/// (two centres, a third front pair, two side pairs, ...) names nothing, which the
/// caller treats as an unknown layout.
fn name_speakers(
    front: &[(u8, u8)],
    side: &[(u8, u8)],
    back: &[(u8, u8)],
    lfe: &[u8],
) -> Option<Vec<Vec<Speaker>>> {
    use Speaker::*;
    let mut names: Vec<Vec<Speaker>> = Vec::new();
    // Front, from the centre outwards: walk the list backwards so the last pair is outermost.
    let mut front_names = vec![Vec::new(); front.len()];
    let (mut pairs, mut centres) = (0usize, 0usize);
    for (i, &(kind, _)) in front.iter().enumerate().rev() {
        if kind == 1 {
            front_names[i] = match pairs {
                0 => vec![FL, FR],
                1 => vec![FLC, FRC],
                _ => return None,
            };
            pairs += 1;
        } else {
            if centres == 1 {
                return None;
            }
            centres += 1;
            front_names[i] = vec![FC];
        }
    }
    names.extend(front_names);
    let mut side_names = Vec::new();
    for &(kind, _) in side {
        if kind != 1 || !side_names.is_empty() {
            return None;
        }
        side_names.push(vec![SL, SR]);
    }
    names.extend(side_names);
    let mut back_names = Vec::new();
    for &(kind, _) in back {
        let next = match (kind, back_names.is_empty()) {
            (1, true) => vec![BL, BR],
            (0, true) => vec![BC],
            _ => return None,
        };
        back_names.push(next);
    }
    names.extend(back_names);
    if lfe.len() > 1 {
        return None;
    }
    names.extend(lfe.iter().map(|_| vec![LFE]));
    Some(names)
}

/// Parses a program_config_element, the channel layout a `channelConfiguration` of 0 carries.
pub fn parse_program_config(r: &mut BitReader<'_>) -> Result<ProgramConfig> {
    let _tag = r.read_bits(4)?;
    let _object_type = r.read_bits(2)?;
    let sf_index = r.read_bits(4)? as u8;
    let front_n = r.read_bits(4)? as usize;
    let side_n = r.read_bits(4)? as usize;
    let back_n = r.read_bits(4)? as usize;
    let lfe_n = r.read_bits(2)? as usize;
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
    let read_cpe_tag = |r: &mut BitReader<'_>| -> Result<(u8, u8)> {
        let cpe = r.read_bit()?;
        let tag = r.read_bits(4)? as u8;
        Ok((u8::from(cpe), tag))
    };
    let mut front = Vec::with_capacity(front_n);
    for _ in 0..front_n {
        front.push(read_cpe_tag(r)?);
    }
    let mut side = Vec::with_capacity(side_n);
    for _ in 0..side_n {
        side.push(read_cpe_tag(r)?);
    }
    let mut back = Vec::with_capacity(back_n);
    for _ in 0..back_n {
        back.push(read_cpe_tag(r)?);
    }
    let mut lfe = Vec::with_capacity(lfe_n);
    for _ in 0..lfe_n {
        lfe.push(r.read_bits(4)? as u8);
    }
    for _ in 0..assoc {
        let _tag = r.read_bits(4)?;
    }
    for _ in 0..cc {
        let _is_ind_sw = r.read_bit()?;
        let _tag = r.read_bits(4)?;
    }
    r.align_to_byte();
    let comment = r.read_bits(8)? as u64;
    r.skip_bits(comment * 8)?;

    for &(kind, _) in front.iter().chain(&side).chain(&back) {
        channels += if kind == 1 { 2 } else { 1 };
    }
    channels += lfe.len() as u16;

    // Speakers come back in the listing order (front, side, back, LFE), one entry per element.
    let named = name_speakers(&front, &side, &back, &lfe).unwrap_or_default();
    let listed = front
        .iter()
        .chain(&side)
        .chain(&back)
        .map(|&(kind, tag)| (kind, tag))
        .chain(lfe.iter().map(|&tag| (3, tag)));
    let mut elements = Vec::new();
    for (i, (kind, tag)) in listed.enumerate() {
        // An unnamed layout leaves `named` short, so every element then reads as unknown.
        let speakers = named.get(i).cloned().unwrap_or_default();
        elements.push(PceElement { kind, tag, speakers });
    }
    Ok(ProgramConfig {
        sf_index,
        channels,
        elements,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicit-SBR AudioSpecificConfig (object type 5) round-trips its
    /// core AND extension sample rate through `write`/`parse` -- the write
    /// side that a bare-ADTS ASC needs so a decoder trusting the object type
    /// (rather than implicit per-frame detection) reports HE-AAC at the
    /// doubled rate.
    #[test]
    fn explicit_sbr_config_round_trips() {
        let cfg = AudioSpecificConfig {
            object_type: AOT_SBR,
            sample_rate: 22050,
            sf_index: sf_index_for_rate(22050).unwrap(),
            channels: 1,
            channel_config: 1,
            sbr_present: true,
            ps_present: false,
            extension_sample_rate: Some(44100),
            pce: Vec::new(),
        };
        let bytes = write_audio_specific_config(&cfg);
        let parsed = parse_audio_specific_config(&bytes).expect("parses");
        assert!(parsed.sbr_present, "explicit SBR must round-trip as SBR");
        assert_eq!(parsed.sample_rate, 22050);
        assert_eq!(parsed.extension_sample_rate, Some(44100));
        assert_eq!(parsed.channels, 1);
    }
}
