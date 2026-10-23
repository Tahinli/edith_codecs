//! The two codec configurations whose channel count and output rate a sample
//! entry does not carry honestly: an AAC `AudioSpecificConfig` (ISO/IEC
//! 14496-3 §1.6.2.1) and an AC-3 `dac3` box (ETSI TS 102 366 annex F).
//!
//! The entry's own `channelcount` is fixed at 2 by the spec whatever the stream
//! carries, and its rate is the core's for HE-AAC. Edith reads both from these
//! bytes, and so does ffprobe, so the demuxer does too.

/// `samplingFrequencyIndex` to Hz, ISO/IEC 14496-3 table 1.18.
const FREQUENCIES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// A most-significant-bit-first reader. `None` once it runs out, which is a
/// truncated configuration and not a panic.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> Option<u32> {
        let mut out = 0u32;
        for _ in 0..n {
            let byte = *self.bytes.get(self.at / 8)?;
            out = (out << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(out)
    }
}

/// The output sample rate and channel count an AAC `AudioSpecificConfig` states.
///
/// For HE-AAC (SBR, object type 5) and HE-AAC v2 (PS, 29) the explicit extension
/// rate is the output rate, the core's being half of it; a PS stream is stereo
/// whatever its mono core says. `None` where the config names no layout
/// (`channelConfiguration` 0 or 8..=15, a program config element or a reserved
/// value) or is truncated: the caller keeps the sample entry's answer then.
pub(crate) fn aac_output(asc: &[u8]) -> Option<(u32, u8)> {
    let mut bits = Bits { bytes: asc, at: 0 };
    let mut object = bits.take(5)?;
    if object == 31 {
        object = 32 + bits.take(6)?;
    }
    let rate = match bits.take(4)? {
        15 => bits.take(24)?,
        index => *FREQUENCIES.get(index as usize)?,
    };
    let config = bits.take(4)?;
    let mut channels = match config {
        1..=6 => config as u8,
        7 => 8,
        _ => return None,
    };
    // SBR (5) and PS (29) carry the extension frequency next; a PS stream's
    // core is mono, its output stereo.
    let mut out = rate;
    if object == 5 || object == 29 {
        let index = bits.take(4)?;
        out = match index {
            15 => bits.take(24)?,
            i => *FREQUENCIES.get(i as usize)?,
        };
    }
    if object == 29 {
        channels = 2;
    }
    Some((out, channels))
}

/// The channel count an AC-3 `dac3` box states: the audio coding mode's full
/// channels, plus the LFE channel when `lfeon` is set (ETSI TS 102 366 table
/// 4.5). `None` for a box too short to hold its three bytes.
pub(crate) fn ac3_channels(dac3: &[u8]) -> Option<u8> {
    let body = dac3.get(..3)?;
    // fscod(2) bsid(5) bsmod(3) acmod(3) lfeon(1) bit_rate_code(5) reserved(5):
    // acmod spans bits 10..13 and lfeon bit 13, counted from the box's first bit.
    let acmod = (body[1] >> 3) & 0x07;
    let lfe = (body[1] >> 2) & 1;
    let full = match acmod {
        0 => 2,
        1 => 1,
        2 => 2,
        3 | 4 => 3,
        5 | 6 => 4,
        _ => 5,
    };
    Some(full + lfe)
}

/// The sampling frequency and channel count a `ddts` box states (ETSI TS 102 114
/// annex E): the 32-bit frequency first, and the speaker-activity mask of the
/// `ChannelLayout` field, each set bit standing for one speaker or a pair.
/// `None` for a box too short to hold the mask or one that names no speaker.
pub(crate) fn dts_config(ddts: &[u8]) -> Option<(u32, u8)> {
    const WEIGHTS: [u8; 16] = [1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2, 2, 1, 2, 1, 2];
    let rate = u32::from_be_bytes(ddts.get(..4)?.try_into().ok()?);
    // After the three 32/8-bit fields (13 bytes): 8 bits of frame duration,
    // stream construction and core LFE, then 24 of core layout, size,
    // downmix and representation, then the 16-bit mask.
    let mask = u16::from_be_bytes(ddts.get(17..19)?.try_into().ok()?);
    let channels: u8 = (0..16)
        .filter(|bit| mask >> bit & 1 == 1)
        .map(|bit| WEIGHTS[bit])
        .sum();
    (channels > 0).then_some((rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_lc_config_states_its_own_rate_and_layout() {
        // AAC-LC (2), 48 kHz (index 3), stereo (2): 0x11 0x90.
        assert_eq!(aac_output(&[0x11, 0x90]), Some((48_000, 2)));
        // 5.1 at 44.1 kHz: AOT 2, index 4, config 6 -> 0001 0010 0011 0000.
        assert_eq!(aac_output(&[0x12, 0x30]), Some((44_100, 6)));
        // 7.1 at 48 kHz: config 7.
        assert_eq!(aac_output(&[0x11, 0xb8]), Some((48_000, 8)));
    }

    #[test]
    fn he_aac_states_its_extension_rate_as_the_output() {
        // SBR (5), core 24 kHz (index 6), stereo, extension 48 kHz (index 3).
        assert_eq!(aac_output(&[0x2b, 0x11, 0x80]), Some((48_000, 2)));
    }

    #[test]
    fn a_program_config_or_a_truncated_config_states_no_layout() {
        assert_eq!(aac_output(&[0x11, 0x80]), None, "channelConfiguration 0");
        assert_eq!(aac_output(&[0x11]), None, "truncated");
    }

    #[test]
    fn ac3_channels_follow_the_coding_mode_and_the_lfe_bit() {
        // acmod 7 (3/2) with LFE: the 5.1 a Blu-ray AC-3 track carries.
        assert_eq!(ac3_channels(&[0x00, 0x3c, 0x00]), Some(6));
        // acmod 2 (stereo), no LFE.
        assert_eq!(ac3_channels(&[0x00, 0x10, 0x00]), Some(2));
        assert_eq!(ac3_channels(&[0x00]), None);
    }

    #[test]
    fn dts_config_counts_the_speaker_mask() {
        let mut ddts = vec![0u8; 20];
        ddts[..4].copy_from_slice(&48_000u32.to_be_bytes());
        // C + L/R + Ls/Rs + LFE: 5.1.
        ddts[17..19].copy_from_slice(&0x000Fu16.to_be_bytes());
        assert_eq!(dts_config(&ddts), Some((48_000, 6)));
        ddts[17..19].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(dts_config(&ddts), None);
        assert_eq!(dts_config(&ddts[..10]), None);
    }
}
