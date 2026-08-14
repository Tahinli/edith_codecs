//! Enhanced AC-3 (A/52 Annex E): the header, the frame-level strategy fields
//! and the audio block syntax that differ from AC-3.
//!
//! Everything after parsing — bit allocation, mantissas, coupling, the inverse
//! transform — is the AC-3 machinery in [`crate::decode`], because Annex E
//! changes how the parameters are *sent*, not what they mean. Of what Annex E
//! adds on top, the adaptive hybrid transform lives in [`crate::aht`] and
//! spectral extension in this module; enhanced coupling is refused by name
//! until a stream that uses it turns up.

use ec_core::{BitReader, Error, Result};

use crate::bitalloc::{BitAllocParams, DeltaBa};
use crate::bsi::{Acmod, Bsi};
use crate::decode::{CHANNELS, COEFFS, CPL, Core, LFE, Syntax};
use crate::exps::{self, Strategy};

pub mod bsi {
    //! `bsi()` of Annex E, shaped like [`crate::bsi`] so a caller can reach
    //! either one the same way.
    pub use super::{Eac3Bsi, parse, parse_from};
}

/// Blocks per syncframe by `numblkscod` (Table E2.4).
const BLOCKS: [usize; 4] = [1, 2, 3, 6];

/// Reduced sample rates by `fscod2` (Table E2.3).
const HALF_RATE: [u32; 4] = [24_000, 22_050, 16_000, 0];

/// Default coupling banding structure, `defcplbndstrc[]` (Table E2.12).
const DEFAULT_CPLBNDSTRC: [bool; 18] = [
    false, false, false, false, false, false, false, false, true, false, true, true, false, true,
    true, true, true, true,
];

/// Default spectral extension banding structure, `defspxbndstrc[]` (Table
/// E2.11): every other sub-band from 8 upwards joins the band below it.
const DEFAULT_SPXBNDSTRC: [bool; 18] = [
    false, false, false, false, false, false, false, false, true, false, true, false, true, false,
    true, false, true, false,
];

/// Spectral extension attenuation, `spxattentab[]` (Table E3.14). Only the
/// first three taps of the symmetric 5-tap notch are stored.
const SPXATTENTAB: [[f32; 3]; 32] = [
    [0.9548416, 0.9117225, 0.8705506],
    [0.9117225, 0.8312379, 0.7578583],
    [0.8705506, 0.7578583, 0.659754],
    [0.8312379, 0.6909564, 0.5743492],
    [0.7937005, 0.6299605, 0.5],
    [0.7578583, 0.5743492, 0.4352753],
    [0.7236346, 0.5236471, 0.3789291],
    [0.6909564, 0.4774208, 0.329877],
    [0.659754, 0.4352753, 0.2871746],
    [0.6299605, 0.3968503, 0.25],
    [0.6015125, 0.3618173, 0.2176376],
    [0.5743492, 0.329877, 0.1894646],
    [0.5484125, 0.3007563, 0.1649385],
    [0.5236471, 0.2742062, 0.1435873],
    [0.5, 0.25, 0.125],
    [0.4774208, 0.2279306, 0.1088188],
    [0.4558612, 0.2078095, 0.0947323],
    [0.4352753, 0.1894646, 0.0824692],
    [0.4156189, 0.1727391, 0.0717936],
    [0.3968503, 0.1574901, 0.0625],
    [0.3789291, 0.1435873, 0.0544094],
    [0.3618173, 0.1309118, 0.0473661],
    [0.3454782, 0.1193552, 0.0412346],
    [0.329877, 0.1088188, 0.0358968],
    [0.3149803, 0.0992126, 0.03125],
    [0.3007563, 0.0904543, 0.0272047],
    [0.2871746, 0.0824692, 0.0236831],
    [0.2742062, 0.0751891, 0.0206173],
    [0.2618235, 0.0685516, 0.0179484],
    [0.25, 0.0625, 0.015625],
    [0.2387104, 0.0569827, 0.0136024],
    [0.2279306, 0.0519524, 0.0118415],
];

/// First transform coefficient of spectral extension sub-band `sbnd`
/// (Table E3.13): 17 sub-bands of 12 coefficients from #25.
fn spx_band_start(sbnd: usize) -> usize {
    25 + 12 * sbnd
}

/// Frame exponent strategy combinations (Table E2.10), as strategy codes.
const FRAME_EXP_STRATEGY: [[u8; 6]; 32] = [
    [1, 0, 0, 0, 0, 0],
    [1, 0, 0, 0, 0, 3],
    [1, 0, 0, 0, 2, 0],
    [1, 0, 0, 0, 3, 3],
    [2, 0, 0, 2, 0, 0],
    [2, 0, 0, 2, 0, 3],
    [2, 0, 0, 3, 2, 0],
    [2, 0, 0, 3, 3, 3],
    [2, 0, 1, 0, 0, 0],
    [2, 0, 2, 0, 0, 3],
    [2, 0, 2, 0, 2, 0],
    [2, 0, 2, 0, 3, 3],
    [2, 0, 3, 2, 0, 0],
    [2, 0, 3, 2, 0, 3],
    [2, 0, 3, 3, 2, 0],
    [2, 0, 3, 3, 3, 3],
    [3, 1, 0, 0, 0, 0],
    [3, 1, 0, 0, 0, 3],
    [3, 2, 0, 0, 2, 0],
    [3, 2, 0, 0, 3, 3],
    [3, 2, 0, 2, 0, 0],
    [3, 2, 0, 2, 0, 3],
    [3, 2, 0, 3, 2, 0],
    [3, 2, 0, 3, 3, 3],
    [3, 3, 1, 0, 0, 0],
    [3, 3, 2, 0, 0, 3],
    [3, 3, 2, 0, 2, 0],
    [3, 3, 2, 0, 3, 3],
    [3, 3, 3, 2, 0, 0],
    [3, 3, 3, 2, 0, 3],
    [3, 3, 3, 3, 2, 0],
    [3, 3, 3, 3, 3, 3],
];

/// Which substream a frame belongs to (Table E2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    /// Independent substream: decodable on its own.
    Independent,
    /// Dependent substream: channel extensions for the independent one before
    /// it.
    Dependent,
    /// An AC-3 stream carried in Annex E framing.
    Ac3Converted,
    /// Reserved.
    Reserved,
}

impl StreamType {
    fn from_code(code: u32) -> StreamType {
        match code & 3 {
            0 => StreamType::Independent,
            1 => StreamType::Dependent,
            2 => StreamType::Ac3Converted,
            _ => StreamType::Reserved,
        }
    }
}

/// Everything the Annex E `bsi()` states.
#[derive(Debug, Clone, PartialEq)]
pub struct Eac3Bsi {
    /// Substream type.
    pub strmtyp: StreamType,
    /// Substream identifier, 0..=7.
    pub substreamid: u8,
    /// Frame size code: the frame is `(frmsiz + 1) * 2` bytes.
    pub frmsiz: u16,
    /// Frame size in bytes.
    pub frame_size: usize,
    /// Sample rate code; 3 means the reduced rates of `fscod2`.
    pub fscod: u8,
    /// Reduced sample rate code, when `fscod == 3`.
    pub fscod2: Option<u8>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of blocks per syncframe code.
    pub numblkscod: u8,
    /// Blocks in this syncframe: 1, 2, 3 or 6.
    pub nblocks: usize,
    /// Custom channel map of a dependent substream (Table E2.5).
    pub chanmap: Option<u16>,
    /// The fields Annex E shares with AC-3, so one caller can read either.
    pub bsi: Bsi,
    /// Full-bandwidth channels, LFE excluded — mirrors `bsi.nfchans`.
    pub nfchans: usize,
}

/// Parse the Annex E `bsi()` from the bytes after the 16-bit sync word.
///
/// `data` starts at `strmtyp`, which is how a caller has it after matching the
/// sync word: `eac3::bsi::parse(&frame[2..])`.
pub fn parse(data: &[u8]) -> Result<Eac3Bsi> {
    // The frame-size field is relative to the sync word, so a reader that
    // starts after it has to be told where zero was: parse against a prefixed
    // buffer only if the caller kept the sync word. Here the caller did not,
    // so nothing in this function depends on absolute bit position.
    parse_after_syncword(&mut BitReader::new(data))
}

/// [`parse`] against a reader positioned at the sync word.
pub fn parse_from(r: &mut BitReader<'_>) -> Result<Eac3Bsi> {
    let syncword = r.read_bits(16)? as u16;
    if syncword != crate::syncinfo::SYNCWORD {
        return Err(Error::corrupt(format!(
            "E-AC-3 bsi: syncword {syncword:#06x}, expected 0x0b77"
        )));
    }
    parse_after_syncword(r)
}

fn parse_after_syncword(r: &mut BitReader<'_>) -> Result<Eac3Bsi> {
    let strmtyp = StreamType::from_code(r.read_bits(2)?);
    let substreamid = r.read_bits(3)? as u8;
    let frmsiz = r.read_bits(11)? as u16;
    let fscod = r.read_bits(2)? as u8;
    let (fscod2, numblkscod) = if fscod == 3 {
        (Some(r.read_bits(2)? as u8), 3)
    } else {
        (None, r.read_bits(2)? as u8)
    };
    let sample_rate = match fscod2 {
        Some(code) => HALF_RATE[code as usize],
        None => crate::tables::SAMPLE_RATE[fscod as usize],
    };
    if sample_rate == 0 {
        return Err(Error::corrupt("E-AC-3 bsi: reserved sample rate code"));
    }
    let acmod = Acmod::from_code(r.read_bits(3)? as u8);
    let lfeon = r.read_bit()?;
    let bsid = r.read_bits(5)? as u8;
    if !(11..=16).contains(&bsid) {
        return Err(Error::unsupported(
            format!("E-AC-3 bsi: bsid = {bsid}"),
            "Annex E defines bsid 11 to 16; below that the stream is AC-3",
        ));
    }
    let dialnorm = r.read_bits(5)? as u8;
    let compr = read_optional(r, 8)?;
    let (mut dialnorm2, mut compr2) = (None, None);
    if acmod == Acmod::DualMono {
        dialnorm2 = Some(r.read_bits(5)? as u8);
        compr2 = read_optional(r, 8)?;
    }
    let mut chanmap = None;
    if strmtyp == StreamType::Dependent && r.read_bit()? {
        chanmap = Some(r.read_bits(16)? as u16);
    }

    let mut bsi = Bsi {
        bsid,
        bsmod: 0,
        acmod,
        cmixlev: None,
        surmixlev: None,
        dsurmod: None,
        lfeon,
        dialnorm,
        compr,
        langcod: None,
        mixlevel: None,
        roomtyp: None,
        dialnorm2,
        compr2,
        copyrightb: false,
        origbs: false,
        dmixmod: None,
        ltrt_mixlev: None,
        loro_mixlev: None,
        dsurexmod: None,
        nfchans: acmod.nfchans(),
        channels: acmod.nfchans() + usize::from(lfeon),
    };
    let nblocks = BLOCKS[numblkscod as usize];

    // Mixing metadata.
    if r.read_bit()? {
        if acmod.code() > 2 {
            bsi.dmixmod = Some(r.read_bits(2)? as u8);
        }
        if acmod.code() & 1 != 0 && acmod.code() > 2 {
            let ltrtcmixlev = r.read_bits(3)? as u8;
            let lorocmixlev = r.read_bits(3)? as u8;
            bsi.ltrt_mixlev = Some((ltrtcmixlev, 0));
            bsi.loro_mixlev = Some((lorocmixlev, 0));
        }
        if acmod.code() & 4 != 0 {
            let ltrtsurmixlev = r.read_bits(3)? as u8;
            let lorosurmixlev = r.read_bits(3)? as u8;
            bsi.ltrt_mixlev = Some((bsi.ltrt_mixlev.map_or(0, |v| v.0), ltrtsurmixlev));
            bsi.loro_mixlev = Some((bsi.loro_mixlev.map_or(0, |v| v.0), lorosurmixlev));
        }
        if lfeon && r.read_bit()? {
            let _lfemixlevcod = r.read_bits(5)?;
        }
        if strmtyp == StreamType::Independent {
            if r.read_bit()? {
                let _pgmscl = r.read_bits(6)?;
            }
            if acmod == Acmod::DualMono && r.read_bit()? {
                let _pgmscl2 = r.read_bits(6)?;
            }
            if r.read_bit()? {
                let _extpgmscl = r.read_bits(6)?;
            }
            match r.read_bits(2)? {
                1 => {
                    let _premix = r.read_bits(5)?;
                }
                2 => {
                    let _mixdata = r.read_bits(12)?;
                }
                3 => read_mixdata_option4(r, nblocks)?,
                _ => {}
            }
            if acmod.code() < 2 {
                if r.read_bit()? {
                    let _panmean = r.read_bits(8)?;
                    let _paninfo = r.read_bits(6)?;
                }
                if acmod == Acmod::DualMono && r.read_bit()? {
                    let _panmean2 = r.read_bits(8)?;
                    let _paninfo2 = r.read_bits(6)?;
                }
            }
            if r.read_bit()? {
                if numblkscod == 0 {
                    let _blkmixcfginfo = r.read_bits(5)?;
                } else {
                    for _ in 0..nblocks {
                        if r.read_bit()? {
                            let _blkmixcfginfo = r.read_bits(5)?;
                        }
                    }
                }
            }
        }
    }

    // Informational metadata.
    if r.read_bit()? {
        bsi.bsmod = r.read_bits(3)? as u8;
        bsi.copyrightb = r.read_bit()?;
        bsi.origbs = r.read_bit()?;
        if acmod == Acmod::Stereo {
            bsi.dsurmod = Some(r.read_bits(2)? as u8);
            let _dheadphonmod = r.read_bits(2)?;
        }
        if acmod.code() >= 6 {
            bsi.dsurexmod = Some(r.read_bits(2)? as u8);
        }
        if r.read_bit()? {
            bsi.mixlevel = Some(r.read_bits(5)? as u8);
            bsi.roomtyp = Some(r.read_bits(2)? as u8);
            let _adconvtyp = r.read_bit()?;
        }
        if acmod == Acmod::DualMono && r.read_bit()? {
            let _mixlevel2 = r.read_bits(5)?;
            let _roomtyp2 = r.read_bits(2)?;
            let _adconvtyp2 = r.read_bit()?;
        }
        if fscod < 3 {
            let _sourcefscod = r.read_bit()?;
        }
    }
    if strmtyp == StreamType::Independent && numblkscod != 3 {
        let _convsync = r.read_bit()?;
    }
    if strmtyp == StreamType::Ac3Converted {
        let blkid = if numblkscod == 3 { true } else { r.read_bit()? };
        if blkid {
            let _frmsizecod = r.read_bits(6)?;
        }
    }
    if r.read_bit()? {
        let addbsil = r.read_bits(6)? as u64;
        r.skip_bits((addbsil + 1) * 8)?;
    }

    Ok(Eac3Bsi {
        strmtyp,
        substreamid,
        frmsiz,
        frame_size: (usize::from(frmsiz) + 1) * 2,
        fscod,
        fscod2,
        sample_rate,
        numblkscod,
        nblocks,
        chanmap,
        nfchans: bsi.nfchans,
        bsi,
    })
}

fn read_mixdata_option4(r: &mut BitReader<'_>, _nblocks: usize) -> Result<()> {
    let mixdeflen = r.read_bits(5)? as u64;
    let start = r.bit_position();
    if r.read_bit()? {
        let _premixcmpsel = r.read_bit()?;
        let _drcsrc = r.read_bit()?;
        let _premixcmpscl = r.read_bits(3)?;
        for _ in 0..7 {
            if r.read_bit()? {
                let _scale = r.read_bits(4)?;
            }
        }
        if r.read_bit()? {
            for _ in 0..2 {
                if r.read_bit()? {
                    let _aux = r.read_bits(4)?;
                }
            }
        }
    }
    if r.read_bit()? {
        let _spchdat = r.read_bits(5)?;
        if r.read_bit()? {
            let _spchdat1 = r.read_bits(5)?;
            let _spchan1att = r.read_bits(2)?;
            if r.read_bit()? {
                let _spchdat2 = r.read_bits(5)?;
                let _spchan2att = r.read_bits(3)?;
            }
        }
    }
    // The field is a fixed-length block: whatever the sub-fields did not use is
    // padding, so seek to its stated end rather than trusting the parse.
    let used = r.bit_position() - start;
    let total = 8 * (mixdeflen + 2);
    if used > total {
        return Err(Error::corrupt(
            "E-AC-3 bsi: mixdata sub-fields overran mixdeflen",
        ));
    }
    r.skip_bits(total - used)?;
    Ok(())
}

fn read_optional(r: &mut BitReader<'_>, n: u32) -> Result<Option<u8>> {
    if r.read_bit()? {
        Ok(Some(r.read_bits(n)? as u8))
    } else {
        Ok(None)
    }
}

/// The frame-level strategy fields of `audfrm()` (Table E1.3).
#[derive(Debug, Clone)]
struct AudFrm {
    expstre: bool,
    ahte: bool,
    snroffststr: u32,
    transproce: bool,
    blkswe: bool,
    dithflage: bool,
    bamode: bool,
    frmfgaincode: bool,
    dbaflde: bool,
    skipflde: bool,
    spxattene: bool,
    cplstre: [bool; 6],
    cplinu: [bool; 6],
    cplexpstr: [Strategy; 6],
    chexpstr: [[Strategy; 5]; 6],
    lfeexpstr: [Strategy; 6],
    frmcsnroffst: u8,
    frmfsnroffst: u8,
    /// Per-channel spectral extension attenuation, from `spxattene`.
    chinspxatten: [bool; 5],
    spxattencod: [u8; 5],
    /// Which channels use the adaptive hybrid transform this frame.
    cplahtinu: bool,
    chahtinu: [bool; 5],
    lfeahtinu: bool,
}

fn parse_audfrm(r: &mut BitReader<'_>, hdr: &Eac3Bsi) -> Result<AudFrm> {
    let nfchans = hdr.nfchans;
    let nblocks = hdr.nblocks;
    let acmod = hdr.bsi.acmod;
    let (expstre, ahte) = if hdr.numblkscod == 3 {
        (r.read_bit()?, r.read_bit()?)
    } else {
        (true, false)
    };
    let mut frm = AudFrm {
        expstre,
        ahte,
        snroffststr: r.read_bits(2)?,
        transproce: r.read_bit()?,
        blkswe: r.read_bit()?,
        dithflage: r.read_bit()?,
        bamode: r.read_bit()?,
        frmfgaincode: r.read_bit()?,
        dbaflde: r.read_bit()?,
        skipflde: r.read_bit()?,
        spxattene: r.read_bit()?,
        cplstre: [false; 6],
        cplinu: [false; 6],
        cplexpstr: [Strategy::Reuse; 6],
        chexpstr: [[Strategy::Reuse; 5]; 6],
        lfeexpstr: [Strategy::Reuse; 6],
        frmcsnroffst: 0,
        frmfsnroffst: 0,
        chinspxatten: [false; 5],
        spxattencod: [0; 5],
        cplahtinu: false,
        chahtinu: [false; 5],
        lfeahtinu: false,
    };

    frm.cplstre[0] = true;
    if acmod.code() > 1 {
        frm.cplinu[0] = r.read_bit()?;
        for blk in 1..nblocks {
            frm.cplstre[blk] = r.read_bit()?;
            frm.cplinu[blk] = if frm.cplstre[blk] {
                r.read_bit()?
            } else {
                frm.cplinu[blk - 1]
            };
        }
    }

    if frm.expstre {
        for blk in 0..nblocks {
            if frm.cplinu[blk] {
                frm.cplexpstr[blk] = Strategy::from_code(r.read_bits(2)?);
            }
            for ch in 0..nfchans {
                frm.chexpstr[blk][ch] = Strategy::from_code(r.read_bits(2)?);
            }
        }
    } else {
        let ncplblks = frm.cplinu[..nblocks].iter().filter(|&&v| v).count();
        if acmod.code() > 1 && ncplblks > 0 {
            let code = r.read_bits(5)? as usize;
            for blk in 0..nblocks {
                frm.cplexpstr[blk] = Strategy::from_code(u32::from(FRAME_EXP_STRATEGY[code][blk]));
            }
        }
        for ch in 0..nfchans {
            let code = r.read_bits(5)? as usize;
            for blk in 0..nblocks {
                frm.chexpstr[blk][ch] =
                    Strategy::from_code(u32::from(FRAME_EXP_STRATEGY[code][blk]));
            }
        }
    }
    if hdr.bsi.lfeon {
        for blk in 0..nblocks {
            frm.lfeexpstr[blk] = if r.read_bit()? {
                Strategy::D15
            } else {
                Strategy::Reuse
            };
        }
    }
    if hdr.strmtyp == StreamType::Independent {
        let convexpstre = if hdr.numblkscod != 3 {
            r.read_bit()?
        } else {
            true
        };
        if convexpstre {
            for _ in 0..nfchans {
                let _convexpstr = r.read_bits(5)?;
            }
        }
    }
    if frm.ahte {
        // The AHT flags are only sent where a single exponent region covers
        // the frame; counting the regions is how the standard states it.
        let ncplblks = frm.cplinu[..nblocks].iter().filter(|&&v| v).count();
        let ncplregs = (0..nblocks)
            .filter(|&b| frm.cplstre[b] || frm.cplexpstr[b] != Strategy::Reuse)
            .count();
        let cplahtinu = if ncplblks == 6 && ncplregs == 1 {
            r.read_bit()?
        } else {
            false
        };
        let mut chahtinu = [false; 5];
        for (ch, slot) in chahtinu.iter_mut().enumerate().take(nfchans) {
            let nchregs = (0..nblocks)
                .filter(|&b| frm.chexpstr[b][ch] != Strategy::Reuse)
                .count();
            if nchregs == 1 {
                *slot = r.read_bit()?;
            }
        }
        let mut lfeahtinu = false;
        let nlferegs = (0..nblocks)
            .filter(|&b| frm.lfeexpstr[b] != Strategy::Reuse)
            .count();
        if hdr.bsi.lfeon && nlferegs == 1 {
            lfeahtinu = r.read_bit()?;
        }
        frm.cplahtinu = cplahtinu;
        frm.chahtinu = chahtinu;
        frm.lfeahtinu = lfeahtinu;
    }
    if frm.snroffststr == 0 {
        frm.frmcsnroffst = r.read_bits(6)? as u8;
        frm.frmfsnroffst = r.read_bits(4)? as u8;
    }
    if frm.transproce {
        for _ in 0..nfchans {
            if r.read_bit()? {
                let _transprocloc = r.read_bits(10)?;
                let _transproclen = r.read_bits(8)?;
            }
        }
    }
    if frm.spxattene {
        for ch in 0..nfchans {
            frm.chinspxatten[ch] = r.read_bit()?;
            if frm.chinspxatten[ch] {
                frm.spxattencod[ch] = r.read_bits(5)? as u8;
            }
        }
    }
    if hdr.numblkscod != 0 && r.read_bit()? {
        // §E2.3.2.27: the field is sized from the frame size.
        // §E2.3.2.27: (numblks - 1) * (4 + ceil(log2(frmsiz + 1))).
        let words = u32::from(hdr.frmsiz) + 1;
        let nblkstrtbits = (nblocks as u32 - 1) * (4 + words.next_power_of_two().trailing_zeros());
        r.skip_bits(u64::from(nblkstrtbits))?;
    }
    Ok(frm)
}

/// Decode one Annex E syncframe: header already parsed, reader positioned at
/// `audfrm()`.
pub(crate) fn decode_frame(
    core: &mut Core,
    r: &mut BitReader<'_>,
    hdr: &Eac3Bsi,
    out: &mut Vec<f32>,
) -> Result<()> {
    let frm = parse_audfrm(r, hdr)?;
    // Reduced rates index the hearing threshold table by the full rate they
    // halve, which is what fscod2 numbers.
    let fscod = hdr.fscod2.map_or(hdr.fscod as usize, |c| c as usize);
    core.start_frame(Syntax::Eac3, fscod, &hdr.bsi);
    // §E1.3, end of audfrm(): the coupling coordinates, the spectral extension
    // coordinates and the leak state are all sent afresh in the first block of
    // every frame that uses them.
    core.first_cplco = [true; 5];
    core.first_spxco = [true; 5];
    core.first_cplleak = true;
    core.chinspxatten = frm.chinspxatten;
    core.spxattencod = frm.spxattencod;
    core.aht = [false; CHANNELS];
    core.aht[CPL] = frm.cplahtinu;
    core.aht[LFE] = frm.lfeahtinu;
    core.aht[..5].copy_from_slice(&frm.chahtinu);
    for blk in 0..hdr.nblocks {
        parse_block(core, r, &frm, hdr, blk)?;
        // §3.6: synthesis sits between decoupling and rematrixing — it fills
        // the band above the coded one, which rematrixing never reaches.
        core.decouple();
        if core.spxinu {
            for ch in 0..core.nfchans {
                if core.chinspx[ch] {
                    spectral_extension(core, ch);
                }
            }
        }
        core.rematrix();
        core.apply_gain();
        core.transform(out);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn parse_block(
    core: &mut Core,
    r: &mut BitReader<'_>,
    frm: &AudFrm,
    hdr: &Eac3Bsi,
    blk: usize,
) -> Result<()> {
    let nfchans = core.nfchans;
    let acmod = core.acmod;

    for ch in 0..nfchans {
        core.blksw[ch] = if frm.blkswe { r.read_bit()? } else { false };
    }
    for ch in 0..nfchans {
        core.dithflag[ch] = if frm.dithflage { r.read_bit()? } else { true };
    }
    if r.read_bit()? {
        core.dynrng[0] = core.gain(r.read_bits(8)? as u8);
    } else if blk == 0 {
        core.dynrng[0] = 1.0;
    }
    if acmod == Acmod::DualMono {
        if r.read_bit()? {
            core.dynrng[1] = core.gain(r.read_bits(8)? as u8);
        } else if blk == 0 {
            core.dynrng[1] = 1.0;
        }
    }

    // Spectral extension strategy (§E2.2.4).
    let spxstre = blk == 0 || r.read_bit()?;
    if spxstre {
        core.spxinu = r.read_bit()?;
        if core.spxinu {
            if acmod == Acmod::Mono {
                core.chinspx[0] = true;
            } else {
                for ch in 0..nfchans {
                    core.chinspx[ch] = r.read_bit()?;
                }
            }
            core.spxstrtf = r.read_bits(2)? as usize;
            let spxbegf = r.read_bits(3)? as usize;
            let spxendf = r.read_bits(3)? as usize;
            core.spxbegf = spxbegf;
            core.spx_begin = if spxbegf < 6 {
                spxbegf + 2
            } else {
                spxbegf * 2 - 3
            };
            core.spx_end = if spxendf < 3 {
                spxendf + 5
            } else {
                spxendf * 2 + 3
            };
            if core.spx_end <= core.spx_begin || core.spx_end > 17 {
                return Err(Error::corrupt(format!(
                    "E-AC-3 spectral extension: sub-bands {}..{}",
                    core.spx_begin, core.spx_end
                )));
            }
            if r.read_bit()? {
                for bnd in core.spx_begin + 1..core.spx_end {
                    core.spxbndstrc[bnd] = r.read_bit()?;
                }
            } else if blk == 0 {
                core.spxbndstrc = DEFAULT_SPXBNDSTRC;
            }
            // §3.6.2: sub-bands merge into bands as spxbndstrc says.
            core.nspxbnds = 1;
            core.spxbndsz[0] = 12;
            for bnd in core.spx_begin + 1..core.spx_end {
                if core.spxbndstrc[bnd] {
                    core.spxbndsz[core.nspxbnds - 1] += 12;
                } else {
                    core.spxbndsz[core.nspxbnds] = 12;
                    core.nspxbnds += 1;
                }
            }
        } else {
            core.chinspx = [false; 5];
            core.first_spxco = [true; 5];
        }
    }

    // Spectral extension coordinates.
    if core.spxinu {
        for ch in 0..nfchans {
            if !core.chinspx[ch] {
                core.first_spxco[ch] = true;
                continue;
            }
            let spxcoe = if core.first_spxco[ch] {
                core.first_spxco[ch] = false;
                true
            } else {
                r.read_bit()?
            };
            if !spxcoe {
                continue;
            }
            let spxblnd = r.read_bits(5)? as f32 / 32.0;
            let mstrspxco = r.read_bits(2)? as i32;
            let mut spxmant = spx_band_start(core.spx_begin) as f32;
            let end = spx_band_start(core.spx_end) as f32;
            for bnd in 0..core.nspxbnds {
                let exp = r.read_bits(4)? as i32;
                let mant = r.read_bits(2)? as f32;
                let value = if exp == 15 {
                    mant / 4.0
                } else {
                    (mant + 4.0) / 8.0
                };
                core.spxco[ch][bnd] =
                    value * crate::mantissa::scale((exp + 3 * mstrspxco).min(24) as u8);
                // §3.6.4.2.1: the blend follows the band's frequency midpoint.
                let bandsize = core.spxbndsz[bnd] as f32;
                let nratio = ((spxmant + 0.5 * bandsize) / end - spxblnd).clamp(0.0, 1.0);
                core.nblend[ch][bnd] = nratio.sqrt();
                core.sblend[ch][bnd] = (1.0 - nratio).sqrt();
                spxmant += bandsize;
            }
        }
    }

    // Coupling strategy.
    // `cplstre[blk]` and `cplinu[blk]` were read in audfrm(), not here.
    if frm.cplstre[blk] {
        if frm.cplinu[blk] {
            let ecplinu = r.read_bit()?;
            if ecplinu {
                return Err(Error::unsupported(
                    "E-AC-3 enhanced coupling (ecplinu)",
                    "the amplitude/angle/chaos parameterisation is not \
                     implemented in this build",
                ));
            }
            if acmod == Acmod::Stereo {
                core.chincpl[0] = true;
                core.chincpl[1] = true;
            } else {
                for ch in 0..nfchans {
                    core.chincpl[ch] = r.read_bit()?;
                }
            }
            if acmod == Acmod::Stereo {
                core.phsflginu = r.read_bit()?;
            }
            core.cplbegf = r.read_bits(4)? as usize;
            // §E3.3.1: with spectral extension the coupling end is derived
            // from spxbegf rather than sent, and may go negative.
            let cplendf: i32 = if core.spxinu {
                if core.spxbegf < 6 {
                    core.spxbegf as i32 - 2
                } else {
                    core.spxbegf as i32 * 2 - 7
                }
            } else {
                r.read_bits(4)? as i32
            };
            let ncplsubnd = 3 + cplendf - core.cplbegf as i32;
            if !(1..=18).contains(&ncplsubnd) {
                return Err(Error::corrupt(format!(
                    "E-AC-3 coupling: cplbegf {} with cplendf {cplendf}",
                    core.cplbegf
                )));
            }
            core.ncplsubnd = ncplsubnd as usize;
            core.strtmant[CPL] = 37 + 12 * core.cplbegf;
            core.endmant[CPL] = (37 + 12 * (cplendf + 3)) as usize;
            // §E2.3.3.14: the banding array is indexed by *absolute* coupling
            // sub-band, like the spectral extension one — which is what makes
            // the default of Table E2.12 mean the same thing wherever coupling
            // starts. `cplbndstrce == 0` in the first block using coupling
            // means the default; in a later block it means "reuse".
            if r.read_bit()? {
                for bnd in 1..core.ncplsubnd {
                    core.cplbndstrc[core.cplbegf + bnd] = r.read_bit()?;
                }
            } else if blk == 0 {
                core.cplbndstrc = DEFAULT_CPLBNDSTRC;
            }
            core.cplband[0] = 0;
            core.ncplbnd = 1;
            for sbnd in 1..core.ncplsubnd {
                if !core.cplbndstrc[core.cplbegf + sbnd] {
                    core.ncplbnd += 1;
                }
                core.cplband[sbnd] = core.ncplbnd - 1;
            }
        } else {
            core.chincpl = [false; 5];
            core.phsflginu = false;
        }
    }
    core.cplinu = frm.cplinu[blk];

    // Coupling coordinates. Annex E sends them once per channel per coupling
    // run: the first block of the run has them implicitly.
    if core.cplinu {
        let mut cplcoe = [false; 5];
        for ch in 0..nfchans {
            if !core.chincpl[ch] {
                core.first_cplco[ch] = true;
                continue;
            }
            cplcoe[ch] = if core.first_cplco[ch] {
                core.first_cplco[ch] = false;
                true
            } else {
                r.read_bit()?
            };
            if cplcoe[ch] {
                let mstrcplco = r.read_bits(2)? as i32;
                for bnd in 0..core.ncplbnd {
                    let cplcoexp = r.read_bits(4)? as i32;
                    let cplcomant = r.read_bits(4)? as f32;
                    let mant = if cplcoexp == 15 {
                        cplcomant / 16.0
                    } else {
                        (cplcomant + 16.0) / 32.0
                    };
                    core.cplco[ch][bnd] =
                        mant * crate::mantissa::scale((cplcoexp + 3 * mstrcplco).min(24) as u8);
                }
            }
        }
        if acmod == Acmod::Stereo && core.phsflginu && (cplcoe[0] || cplcoe[1]) {
            for bnd in 0..core.ncplbnd {
                core.phsflg[bnd] = r.read_bit()?;
            }
        }
    }

    // Rematrixing.
    if acmod == Acmod::Stereo {
        let rematstr = blk == 0 || r.read_bit()?;
        if rematstr {
            // §E3.3.2.
            core.nrematbnd = if core.cplinu {
                match core.cplbegf {
                    0 => 2,
                    1 | 2 => 3,
                    _ => 4,
                }
            } else if core.spxinu {
                if core.spxbegf < 2 { 3 } else { 4 }
            } else {
                4
            };
            for bnd in 0..core.nrematbnd {
                core.rematflg[bnd] = r.read_bit()?;
            }
        }
    }

    // Bandwidth codes and exponents.
    core.expstr = [Strategy::Reuse; CHANNELS];
    core.expstr[CPL] = frm.cplexpstr[blk];
    for ch in 0..nfchans {
        core.expstr[ch] = frm.chexpstr[blk][ch];
    }
    core.expstr[LFE] = frm.lfeexpstr[blk];
    for ch in 0..nfchans {
        if core.expstr[ch] != Strategy::Reuse {
            if core.chincpl[ch] && core.cplinu {
                core.endmant[ch] = core.strtmant[CPL];
            } else if core.chinspx[ch] && core.spxinu {
                // §E3.3.3: the coded band ends where synthesis begins.
                core.endmant[ch] = spx_band_start(core.spx_begin);
            } else {
                let chbwcod = r.read_bits(6)? as usize;
                if chbwcod > 60 {
                    return Err(Error::corrupt(format!(
                        "E-AC-3 audblk: chbwcod = {chbwcod} > 60"
                    )));
                }
                core.endmant[ch] = (chbwcod + 12) * 3 + 37;
            }
        }
    }
    if core.cplinu && core.expstr[CPL] != Strategy::Reuse {
        let absexp = (r.read_bits(4)? << 1) as u8;
        let (start, end) = (core.strtmant[CPL], core.endmant[CPL]);
        let ngrps = core.expstr[CPL].coupling_groups(start, end);
        exps::decode(
            r,
            core.expstr[CPL],
            ngrps,
            absexp,
            start,
            &mut core.exps[CPL],
        )?;
    }
    for ch in 0..nfchans {
        if core.expstr[ch] == Strategy::Reuse {
            continue;
        }
        let absexp = r.read_bits(4)? as u8;
        core.exps[ch][0] = absexp;
        let ngrps = core.expstr[ch].fbw_groups(core.endmant[ch]);
        exps::decode(r, core.expstr[ch], ngrps, absexp, 1, &mut core.exps[ch])?;
        let _gainrng = r.read_bits(2)?;
    }
    if core.lfeon && core.expstr[LFE] != Strategy::Reuse {
        let absexp = r.read_bits(4)? as u8;
        core.exps[LFE][0] = absexp;
        exps::decode(r, Strategy::D15, 2, absexp, 1, &mut core.exps[LFE])?;
    }

    // Bit allocation parameters.
    if frm.bamode {
        if r.read_bit()? {
            core.ba = BitAllocParams {
                sdcycod: r.read_bits(2)? as u8,
                fdcycod: r.read_bits(2)? as u8,
                sgaincod: r.read_bits(2)? as u8,
                dbpbcod: r.read_bits(2)? as u8,
                floorcod: r.read_bits(3)? as u8,
            };
        }
    } else {
        core.ba = BitAllocParams {
            sdcycod: 2,
            fdcycod: 1,
            sgaincod: 1,
            dbpbcod: 2,
            floorcod: 7,
        };
    }

    // SNR offsets.
    if frm.snroffststr == 0 {
        core.csnroffst = frm.frmcsnroffst;
        core.fsnroffst = [frm.frmfsnroffst; CHANNELS];
    } else {
        let snroffste = blk == 0 || r.read_bit()?;
        if snroffste {
            core.csnroffst = r.read_bits(6)? as u8;
            if frm.snroffststr == 1 {
                let blkfsnroffst = r.read_bits(4)? as u8;
                core.fsnroffst = [blkfsnroffst; CHANNELS];
            } else {
                if core.cplinu {
                    core.fsnroffst[CPL] = r.read_bits(4)? as u8;
                }
                for ch in 0..nfchans {
                    core.fsnroffst[ch] = r.read_bits(4)? as u8;
                }
                if core.lfeon {
                    core.fsnroffst[LFE] = r.read_bits(4)? as u8;
                }
            }
        }
    }

    // Fast gain codes.
    let fgaincode = frm.frmfgaincode && r.read_bit()?;
    if fgaincode {
        if core.cplinu {
            core.fgaincod[CPL] = r.read_bits(3)? as u8;
        }
        for ch in 0..nfchans {
            core.fgaincod[ch] = r.read_bits(3)? as u8;
        }
        if core.lfeon {
            core.fgaincod[LFE] = r.read_bits(3)? as u8;
        }
    } else {
        core.fgaincod = [4; CHANNELS];
    }
    if hdr.strmtyp == StreamType::Independent && r.read_bit()? {
        let _convsnroffst = r.read_bits(10)?;
    }
    if core.cplinu {
        let cplleake = if core.first_cplleak {
            core.first_cplleak = false;
            true
        } else {
            r.read_bit()?
        };
        if cplleake {
            core.cplleak = (
                ((r.read_bits(3)? as i32) << 8) + 768,
                ((r.read_bits(3)? as i32) << 8) + 768,
            );
        }
    }

    // Delta bit allocation.
    if frm.dbaflde && r.read_bit()? {
        let mut deltbae = [0u32; CHANNELS];
        if core.cplinu {
            deltbae[CPL] = r.read_bits(2)?;
        }
        for ch in 0..nfchans {
            deltbae[ch] = r.read_bits(2)?;
        }
        let mut order = Vec::with_capacity(CHANNELS);
        if core.cplinu {
            order.push(CPL);
        }
        order.extend(0..nfchans);
        for ch in order {
            match deltbae[ch] {
                0 => {}
                1 => {
                    core.deltba_on[ch] = true;
                    let nseg = r.read_bits(3)? as usize + 1;
                    let mut dba = DeltaBa {
                        nseg,
                        ..DeltaBa::default()
                    };
                    for seg in 0..nseg {
                        dba.offset[seg] = r.read_bits(5)? as u8;
                        dba.length[seg] = r.read_bits(4)? as u8;
                        dba.delta[seg] = r.read_bits(3)? as u8;
                    }
                    core.deltba[ch] = dba;
                }
                _ => core.deltba_on[ch] = false,
            }
        }
    }

    if frm.skipflde && r.read_bit()? {
        let skipl = r.read_bits(9)? as u64;
        r.skip_bits(skipl * 8)?;
    }

    core.allocate();
    core.read_mantissas(r, blk)
}

/// §3.6.4: synthesise one channel's high band — translate, blend with noise,
/// and scale to the transmitted envelope.
fn spectral_extension(core: &mut Core, ch: usize) {
    let copystart = spx_band_start(core.spxstrtf);
    let copyend = spx_band_start(core.spx_begin);
    let nbnds = core.nspxbnds;

    // §3.6.4.1: translation, wrapping the copy region as it runs out.
    let mut wrapflag = [false; 18];
    let mut copyindex = copystart;
    let mut insertindex = copyend;
    for bnd in 0..nbnds {
        let bandsize = core.spxbndsz[bnd];
        if copyindex + bandsize > copyend {
            copyindex = copystart;
            wrapflag[bnd] = true;
        }
        for _ in 0..bandsize {
            if copyindex == copyend {
                copyindex = copystart;
            }
            if insertindex >= COEFFS {
                break;
            }
            core.coeffs[ch][insertindex] = core.coeffs[ch][copyindex];
            insertindex += 1;
            copyindex += 1;
        }
    }

    // §3.6.4.2.2: banded RMS energy of what was just translated.
    let mut rms = [0.0f32; 18];
    let mut spxmant = copyend;
    for bnd in 0..nbnds {
        let bandsize = core.spxbndsz[bnd];
        let mut accum = 0.0f32;
        for _ in 0..bandsize {
            if spxmant >= COEFFS {
                break;
            }
            let v = core.coeffs[ch][spxmant];
            accum += v * v;
            spxmant += 1;
        }
        rms[bnd] = (accum / bandsize as f32).sqrt();
    }

    // §3.6.4.2.3: a 5-tap notch across the baseband border and every wrap.
    if core.chinspxatten[ch] {
        let tab = SPXATTENTAB[(core.spxattencod[ch] as usize).min(31)];
        let taps = [tab[0], tab[1], tab[2], tab[1], tab[0]];
        let mut filtbin = copyend.saturating_sub(2);
        for tap in taps {
            if filtbin < COEFFS {
                core.coeffs[ch][filtbin] *= tap;
            }
            filtbin += 1;
        }
        filtbin += core.spxbndsz[0];
        for bnd in 1..nbnds {
            if wrapflag[bnd] {
                filtbin -= 5;
                for tap in taps {
                    if filtbin < COEFFS {
                        core.coeffs[ch][filtbin] *= tap;
                    }
                    filtbin += 1;
                }
            }
            filtbin += core.spxbndsz[bnd];
        }
    }

    // §3.6.4.2.4 and §3.6.4.3: blend in energy-matched noise, then scale to
    // the transmitted envelope.
    let mut spxmant = copyend;
    for bnd in 0..nbnds {
        let bandsize = core.spxbndsz[bnd];
        let nscale = rms[bnd] * core.nblend[ch][bnd];
        let sscale = core.sblend[ch][bnd];
        let coord = core.spxco[ch][bnd] * 32.0;
        for _ in 0..bandsize {
            if spxmant >= COEFFS {
                break;
            }
            let noise = core.spx_noise.unit_variance();
            core.coeffs[ch][spxmant] = (core.coeffs[ch][spxmant] * sscale + noise * nscale) * coord;
            spxmant += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_exponent_strategy_table_is_the_printed_one() {
        // Table E2.10 corners: code 0 is D15 then all reuse, code 31 is D45
        // in every block, and code 16 opens D45, D15.
        assert_eq!(FRAME_EXP_STRATEGY[0], [1, 0, 0, 0, 0, 0]);
        assert_eq!(FRAME_EXP_STRATEGY[31], [3, 3, 3, 3, 3, 3]);
        assert_eq!(FRAME_EXP_STRATEGY[16], [3, 1, 0, 0, 0, 0]);
        // Every combination starts by sending exponents, never by reusing
        // exponents that do not exist yet.
        for row in FRAME_EXP_STRATEGY {
            assert_ne!(row[0], 0);
        }
    }
}
