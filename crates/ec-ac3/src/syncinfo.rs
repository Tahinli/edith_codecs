//! `syncinfo()` — the five bytes an AC-3 syncframe opens with (A/52 §5.3.1).

use ec_core::{BitReader, Error, Result};

use crate::tables;

/// The AC-3 sync word, `0x0B77`, in the order it appears on the wire.
pub const SYNCWORD: u16 = 0x0B77;

/// Everything `syncinfo()` states, plus the two values every caller derives
/// from it (sample rate and frame size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncInfo {
    /// CRC over the first 5/8 of the frame (§7.10.1). Carried, not checked.
    pub crc1: u16,
    /// Sample rate code (Table 5.6).
    pub fscod: u8,
    /// Frame size code (Table 5.18).
    pub frmsizecod: u8,
    /// Sample rate in Hz, from `fscod`.
    pub sample_rate: u32,
    /// Whole frame length in bytes, from `frmsizecod` and `fscod`.
    pub frame_size: usize,
    /// Nominal bit rate in kbit/s.
    pub bit_rate_kbps: u32,
}

/// Parse `syncinfo()` from the start of a syncframe.
///
/// `data` starts at the sync word. Fewer than 5 bytes is [`Error::NeedMore`];
/// a wrong sync word or a reserved `fscod`/`frmsizecod` is [`Error::Corrupt`],
/// which is what makes this usable as a "is this AC-3?" probe.
pub fn parse(data: &[u8]) -> Result<SyncInfo> {
    parse_from(&mut BitReader::new(data))
}

/// [`parse`] against a reader the caller keeps, so a decoder can carry on into
/// `bsi()` on the same bit position.
pub fn parse_from(r: &mut BitReader<'_>) -> Result<SyncInfo> {
    let syncword = r.read_bits(16)? as u16;
    if syncword != SYNCWORD {
        return Err(Error::corrupt(format!(
            "AC-3 syncinfo: syncword {syncword:#06x}, expected {SYNCWORD:#06x}"
        )));
    }
    let crc1 = r.read_bits(16)? as u16;
    let fscod = r.read_bits(2)? as u8;
    let frmsizecod = r.read_bits(6)? as u8;
    let sample_rate = tables::SAMPLE_RATE[fscod as usize];
    if sample_rate == 0 {
        return Err(Error::corrupt("AC-3 syncinfo: fscod = 3 (reserved)"));
    }
    let Some(words) = tables::FRAME_SIZE_WORDS.get(frmsizecod as usize) else {
        return Err(Error::corrupt(format!(
            "AC-3 syncinfo: frmsizecod = {frmsizecod} (reserved)"
        )));
    };
    Ok(SyncInfo {
        crc1,
        fscod,
        frmsizecod,
        sample_rate,
        frame_size: usize::from(words[fscod as usize]) * 2,
        bit_rate_kbps: tables::BIT_RATE_KBPS[frmsizecod as usize >> 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_48k_448k_header() {
        // syncword, crc1 = 0x1234, fscod = 0 (48 kHz), frmsizecod = 30 (448k).
        let data = [0x0B, 0x77, 0x12, 0x34, 0b0001_1110];
        let si = parse(&data).unwrap();
        assert_eq!(si.crc1, 0x1234);
        assert_eq!(si.sample_rate, 48_000);
        assert_eq!(si.bit_rate_kbps, 448);
        assert_eq!(si.frame_size, 896 * 2);
    }

    #[test]
    fn refuses_a_bad_syncword_and_short_input() {
        assert!(matches!(
            parse(&[0x0B, 0x78, 0, 0, 0]),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(parse(&[0x0B, 0x77, 0]), Err(Error::NeedMore)));
        // fscod = 3 is reserved.
        assert!(matches!(
            parse(&[0x0B, 0x77, 0, 0, 0b1100_0000]),
            Err(Error::Corrupt { .. })
        ));
    }
}
