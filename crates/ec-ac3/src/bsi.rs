//! `bsi()` — AC-3 bit stream information (A/52 §5.3.2), including the
//! alternate syntax of Annex D that `bsid == 6` streams carry.

use ec_core::{BitReader, Error, Result};

use crate::tables;

/// Audio coding mode (Table 5.8): how many channels there are and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Acmod {
    /// 1+1: two completely independent programs (dual mono).
    DualMono,
    /// 1/0: centre only.
    Mono,
    /// 2/0: left, right.
    #[default]
    Stereo,
    /// 3/0: left, centre, right.
    Surround3_0,
    /// 2/1: left, right, mono surround.
    Surround2_1,
    /// 3/1: left, centre, right, mono surround.
    Surround3_1,
    /// 2/2: left, right, left surround, right surround.
    Surround2_2,
    /// 3/2: left, centre, right, left surround, right surround.
    Surround3_2,
}

impl Acmod {
    /// The raw 3-bit code.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// From the raw 3-bit code; anything wider is masked to 3 bits.
    pub fn from_code(code: u8) -> Acmod {
        match code & 7 {
            0 => Acmod::DualMono,
            1 => Acmod::Mono,
            2 => Acmod::Stereo,
            3 => Acmod::Surround3_0,
            4 => Acmod::Surround2_1,
            5 => Acmod::Surround3_1,
            6 => Acmod::Surround2_2,
            _ => Acmod::Surround3_2,
        }
    }

    /// Full-bandwidth channels, excluding the LFE.
    pub fn nfchans(self) -> usize {
        tables::NFCHANS[self.code() as usize]
    }

    /// True when a centre channel is coded (acmod 3, 5, 7).
    pub fn has_center(self) -> bool {
        matches!(self.code(), 3 | 5 | 7)
    }

    /// Number of surround channels coded: 0, 1 (mono surround) or 2.
    pub fn surround_channels(self) -> usize {
        match self.code() {
            4 | 5 => 1,
            6 | 7 => 2,
            _ => 0,
        }
    }
}

/// Centre mix level in linear gain (Table 5.9). The reserved code takes the
/// middle value, as §7.8.2 instructs.
pub fn center_mix_level(cmixlev: u8) -> f32 {
    match cmixlev {
        0 => 0.707,
        2 => 0.500,
        _ => 0.595,
    }
}

/// Surround mix level in linear gain (Table 5.10). The reserved code takes the
/// middle value, as §7.8.2 instructs.
pub fn surround_mix_level(surmixlev: u8) -> f32 {
    match surmixlev {
        0 => 0.707,
        2 => 0.0,
        _ => 0.500,
    }
}

/// Everything `bsi()` carries. Optional fields are [`None`] when their
/// presence bit was clear, which is the difference between "the encoder said
/// 0 dB" and "the encoder said nothing".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bsi {
    /// Bit stream identification: 8 for standard AC-3, 6 for the Annex D
    /// alternate syntax, 16 for E-AC-3 (which uses [`crate::eac3::bsi`]).
    pub bsid: u8,
    /// Bit stream mode (Table 5.7).
    pub bsmod: u8,
    /// Audio coding mode.
    pub acmod: Acmod,
    /// Centre mix level code, present with three front channels.
    pub cmixlev: Option<u8>,
    /// Surround mix level code, present with a surround channel.
    pub surmixlev: Option<u8>,
    /// Dolby Surround mode, present in 2/0.
    pub dsurmod: Option<u8>,
    /// LFE channel present.
    pub lfeon: bool,
    /// Dialogue normalisation, in -dB below full scale (1..=31).
    pub dialnorm: u8,
    /// Heavy compression gain word.
    pub compr: Option<u8>,
    /// Language code (deprecated by the standard, still carried).
    pub langcod: Option<u8>,
    /// Mixing level in dB SPL, when audio production info is present.
    pub mixlevel: Option<u8>,
    /// Room type, when audio production info is present.
    pub roomtyp: Option<u8>,
    /// Second program's dialogue normalisation, in 1+1 mode.
    pub dialnorm2: Option<u8>,
    /// Second program's heavy compression gain word, in 1+1 mode.
    pub compr2: Option<u8>,
    /// Copyright bit.
    pub copyrightb: bool,
    /// Original bit stream bit.
    pub origbs: bool,
    /// Preferred stereo downmix mode (Annex D `dmixmod`).
    pub dmixmod: Option<u8>,
    /// Lt/Rt centre and surround mix levels (Annex D).
    pub ltrt_mixlev: Option<(u8, u8)>,
    /// Lo/Ro centre and surround mix levels (Annex D).
    pub loro_mixlev: Option<(u8, u8)>,
    /// Dolby Surround EX mode (Annex D `dsurexmod`).
    pub dsurexmod: Option<u8>,
    /// Full-bandwidth channels, excluding the LFE.
    pub nfchans: usize,
    /// Coded channels including the LFE — what a decoder hands out natively.
    pub channels: usize,
}

/// Parse `bsi()` from the bits that follow `syncinfo()`.
///
/// `data` starts at the first byte after the five `syncinfo()` bytes, which is
/// how every caller has it: `bsi::parse(&frame[5..])`.
pub fn parse(data: &[u8]) -> Result<Bsi> {
    parse_from(&mut BitReader::new(data))
}

/// [`parse`] against a reader the caller keeps, so a decoder can carry on into
/// the first audio block on the same bit position.
pub fn parse_from(r: &mut BitReader<'_>) -> Result<Bsi> {
    let bsid = r.read_bits(5)? as u8;
    if bsid > 10 {
        return Err(Error::unsupported(
            format!("AC-3 bsi: bsid = {bsid}"),
            "bsid above 10 is Enhanced AC-3; parse it with eac3::bsi",
        ));
    }
    let bsmod = r.read_bits(3)? as u8;
    let acmod = Acmod::from_code(r.read_bits(3)? as u8);
    let cmixlev = (acmod.code() & 1 != 0 && acmod.code() != 1)
        .then(|| r.read_bits(2))
        .transpose()?
        .map(|v| v as u8);
    let surmixlev = (acmod.code() & 4 != 0)
        .then(|| r.read_bits(2))
        .transpose()?
        .map(|v| v as u8);
    let dsurmod = (acmod == Acmod::Stereo)
        .then(|| r.read_bits(2))
        .transpose()?
        .map(|v| v as u8);
    let lfeon = r.read_bit()?;
    let dialnorm = r.read_bits(5)? as u8;
    let compr = read_optional(r, 8)?;
    let langcod = read_optional(r, 8)?;
    let (mixlevel, roomtyp) = if r.read_bit()? {
        (Some(r.read_bits(5)? as u8), Some(r.read_bits(2)? as u8))
    } else {
        (None, None)
    };
    let (mut dialnorm2, mut compr2) = (None, None);
    if acmod == Acmod::DualMono {
        dialnorm2 = Some(r.read_bits(5)? as u8);
        compr2 = read_optional(r, 8)?;
        let _langcod2 = read_optional(r, 8)?;
        if r.read_bit()? {
            let _mixlevel2 = r.read_bits(5)?;
            let _roomtyp2 = r.read_bits(2)?;
        }
    }
    let copyrightb = r.read_bit()?;
    let origbs = r.read_bit()?;

    let mut bsi = Bsi {
        bsid,
        bsmod,
        acmod,
        cmixlev,
        surmixlev,
        dsurmod,
        lfeon,
        dialnorm,
        compr,
        langcod,
        mixlevel,
        roomtyp,
        dialnorm2,
        compr2,
        copyrightb,
        origbs,
        dmixmod: None,
        ltrt_mixlev: None,
        loro_mixlev: None,
        dsurexmod: None,
        nfchans: acmod.nfchans(),
        channels: acmod.nfchans() + usize::from(lfeon),
    };

    if bsid == 6 {
        // Annex D: the time code fields are replaced by the extra mix and
        // downmix metadata a modern encoder actually has to say.
        if r.read_bit()? {
            bsi.dmixmod = Some(r.read_bits(2)? as u8);
            bsi.ltrt_mixlev = Some((r.read_bits(3)? as u8, r.read_bits(3)? as u8));
            bsi.loro_mixlev = Some((r.read_bits(3)? as u8, r.read_bits(3)? as u8));
        }
        if r.read_bit()? {
            bsi.dsurexmod = Some(r.read_bits(2)? as u8);
            let _dheadphonmod = r.read_bits(2)?;
            let _adconvtyp = r.read_bit()?;
            let _xbsi2 = r.read_bits(8)?;
            let _encinfo = r.read_bit()?;
        }
    } else {
        if r.read_bit()? {
            let _timecod1 = r.read_bits(14)?;
        }
        if r.read_bit()? {
            let _timecod2 = r.read_bits(14)?;
        }
    }
    if r.read_bit()? {
        let addbsil = r.read_bits(6)? as u64;
        r.skip_bits((addbsil + 1) * 8)?;
    }
    Ok(bsi)
}

/// A presence bit followed by an `n`-bit value.
fn read_optional(r: &mut BitReader<'_>, n: u32) -> Result<Option<u8>> {
    if r.read_bit()? {
        Ok(Some(r.read_bits(n)? as u8))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    fn write_minimal_bsi(bsid: u8, acmod: u8, lfeon: bool, dialnorm: u8) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(u32::from(bsid), 5);
        w.write_bits(0, 3); // bsmod
        w.write_bits(u32::from(acmod), 3);
        if acmod & 1 != 0 && acmod != 1 {
            w.write_bits(1, 2); // cmixlev
        }
        if acmod & 4 != 0 {
            w.write_bits(0, 2); // surmixlev
        }
        if acmod == 2 {
            w.write_bits(0, 2); // dsurmod
        }
        w.write_bit(lfeon);
        w.write_bits(u32::from(dialnorm), 5);
        w.write_bit(false); // compre
        w.write_bit(false); // langcode
        w.write_bit(false); // audprodie
        w.write_bit(false); // copyrightb
        w.write_bit(false); // origbs
        w.write_bit(false); // timecod1e
        w.write_bit(false); // timecod2e
        w.write_bit(false); // addbsie
        w.align_to_byte();
        w.into_bytes()
    }

    #[test]
    fn parses_a_5_1_header() {
        let bytes = write_minimal_bsi(8, 7, true, 27);
        let bsi = parse(&bytes).unwrap();
        assert_eq!(bsi.acmod, Acmod::Surround3_2);
        assert_eq!(bsi.nfchans, 5);
        assert!(bsi.lfeon);
        assert_eq!(bsi.channels, 6);
        assert_eq!(bsi.dialnorm, 27);
        assert_eq!(bsi.cmixlev, Some(1));
        assert_eq!(bsi.surmixlev, Some(0));
    }

    #[test]
    fn parses_mono_and_refuses_eac3_bsid() {
        let bsi = parse(&write_minimal_bsi(8, 1, false, 31)).unwrap();
        assert_eq!(bsi.acmod, Acmod::Mono);
        assert_eq!(bsi.channels, 1);
        assert!(matches!(
            parse(&write_minimal_bsi(16, 7, true, 27)),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(parse(&[]), Err(Error::NeedMore)));
    }

    #[test]
    fn mix_levels_follow_tables_5_9_and_5_10() {
        assert_eq!(center_mix_level(0), 0.707);
        assert_eq!(center_mix_level(2), 0.500);
        assert_eq!(center_mix_level(3), center_mix_level(1));
        assert_eq!(surround_mix_level(2), 0.0);
    }
}
