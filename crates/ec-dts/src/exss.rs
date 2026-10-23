//! DTS-HD extension substream (EXSS) header (ETSI TS 102 114 annex B/C of
//! the DTS-HD addendum, as carried in the substream header of every HD
//! frame). Only the fields a core decoder needs to know what else rides in
//! the frame are kept: which components exist and how big they are.

use crate::bits::Bits;
use crate::header::SYNC_EXSS;
use ec_core::{Error, Result};

/// Core component present in the asset (`nuCoreExtensionMask` bit).
pub const EXSS_CORE: u32 = 0x010;
/// XBR (extended bit rate) component.
pub const EXSS_XBR: u32 = 0x020;
/// XXCH (more than 6.1 channels) component.
pub const EXSS_XXCH: u32 = 0x040;
/// X96 (96 kHz extension) component.
pub const EXSS_X96: u32 = 0x080;
/// LBR (low bit rate) component.
pub const EXSS_LBR: u32 = 0x100;
/// XLL (lossless) component.
pub const EXSS_XLL: u32 = 0x200;

/// One audio asset of an extension substream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExssAsset {
    /// Offset of the asset data from the start of the substream.
    pub offset: usize,
    /// Size of the asset data in bytes.
    pub size: usize,
    /// Audio asset identifier.
    pub index: u8,
    /// PCM bit resolution, when static metadata is present.
    pub pcm_bit_res: u8,
    /// Maximum sample rate in Hz, when static metadata is present.
    pub max_sample_rate: u32,
    /// Total channels of the asset, when static metadata is present.
    pub nchannels_total: u32,
    /// Coding mode (0 multi-component, 1 lossless, 2 LBR, 3 auxiliary).
    pub coding_mode: u8,
    /// Component bit mask, see the `EXSS_*` constants.
    pub extension_mask: u32,
    /// Offset of the core component from the start of the substream.
    pub core_offset: usize,
    /// Size of the core component.
    pub core_size: usize,
    /// Size of the XBR component.
    pub xbr_size: usize,
    /// Size of the XXCH component.
    pub xxch_size: usize,
    /// Offset of the X96 component from the start of the substream.
    pub x96_offset: usize,
    /// Size of the X96 component.
    pub x96_size: usize,
    /// Size of the LBR component.
    pub lbr_size: usize,
    /// Offset of the XLL component from the start of the substream.
    pub xll_offset: usize,
    /// Size of the XLL component.
    pub xll_size: usize,
}

/// A parsed extension substream header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExssInfo {
    /// Extension substream index (0-3).
    pub index: u8,
    /// Total substream size in bytes, header included.
    pub size: usize,
    /// Header size in bytes.
    pub header_size: usize,
    /// Audio assets in order.
    pub assets: Vec<ExssAsset>,
}

impl ExssInfo {
    /// Whether any asset carries an XLL (lossless) component.
    pub fn has_xll(&self) -> bool {
        self.assets.iter().any(|a| a.extension_mask & EXSS_XLL != 0)
    }
    /// Whether any asset carries an LBR component.
    pub fn has_lbr(&self) -> bool {
        self.assets.iter().any(|a| a.extension_mask & EXSS_LBR != 0)
    }
    /// Whether any asset carries a core component.
    pub fn has_core(&self) -> bool {
        self.assets.iter().any(|a| a.extension_mask & EXSS_CORE != 0)
    }
}

const SAMPLING_FREQS: [u32; 16] = [
    8000, 16000, 32000, 64000, 128000, 22050, 44100, 88200, 176400, 352800, 12000, 24000, 48000,
    96000, 192000, 384000,
];

fn count_chs_for_mask(mask: u32) -> u32 {
    ((mask & 0xffff) | ((mask & 0xae66) << 16)).count_ones()
}

struct Static {
    present: bool,
    mix_metadata: bool,
    nmixoutconfigs: usize,
    nmixoutchs: [u32; 4],
    exss_size_nbits: u32,
}

fn bad(what: &str) -> Error {
    Error::corrupt(format!("DTS-HD EXSS: {what}"))
}

fn parse_descriptor(r: &mut Bits<'_>, s: &Static, a: &mut ExssAsset) -> Result<()> {
    let descr_pos = r.pos();
    let descr_size = r.get(9) as usize + 1;
    a.index = r.get(3) as u8;
    let mut embedded_stereo = false;
    let mut embedded_6ch = false;
    if s.present {
        if r.get1() != 0 {
            r.skip(4);
        }
        if r.get1() != 0 {
            r.skip(24);
        }
        if r.get1() != 0 {
            let text = r.get(10) as usize + 1;
            if r.left() < text as isize * 8 {
                return Err(bad("text overruns the header"));
            }
            r.skip(text * 8);
        }
        a.pcm_bit_res = r.get(5) as u8 + 1;
        a.max_sample_rate = SAMPLING_FREQS[r.get(4) as usize];
        a.nchannels_total = r.get(8) + 1;
        if r.get1() != 0 {
            if a.nchannels_total > 2 {
                embedded_stereo = r.get1() != 0;
            }
            if a.nchannels_total > 6 {
                embedded_6ch = r.get1() != 0;
            }
            let mut spkr_mask_nbits = 0u32;
            if r.get1() != 0 {
                spkr_mask_nbits = (r.get(2) + 1) << 2;
                r.skip(spkr_mask_nbits as usize);
            }
            let nsets = r.get(3) as usize;
            if nsets != 0 && spkr_mask_nbits == 0 {
                return Err(bad("remapping sets without a speaker mask"));
            }
            let mut nspeakers = [0u32; 8];
            for n in nspeakers.iter_mut().take(nsets) {
                *n = count_chs_for_mask(r.get(spkr_mask_nbits));
            }
            for n in nspeakers.iter().take(nsets) {
                let nch = r.get(5) + 1;
                for _ in 0..*n {
                    let m = r.get(nch);
                    r.skip(m.count_ones() as usize * 5);
                }
            }
        } else {
            r.skip(3);
        }
    }
    // The remainder of the descriptor (DRC, mixing metadata, navigation)
    // is only needed for the component sizes below.
    let drc = r.get1() != 0;
    if drc {
        r.skip(8);
    }
    if r.get1() != 0 {
        r.skip(5);
    }
    if drc && embedded_stereo {
        r.skip(8);
    }
    if s.mix_metadata && r.get1() != 0 {
        r.skip(1 + 6);
        if r.get(2) == 3 {
            r.skip(8);
        } else {
            r.skip(3);
        }
        if r.get1() != 0 {
            for i in 0..s.nmixoutconfigs {
                r.skip(6 * s.nmixoutchs[i] as usize);
            }
        } else {
            r.skip(6 * s.nmixoutconfigs);
        }
        let mut nd = a.nchannels_total;
        if embedded_6ch {
            nd += 6;
        }
        if embedded_stereo {
            nd += 2;
        }
        for i in 0..s.nmixoutconfigs {
            if s.nmixoutchs[i] == 0 {
                return Err(bad("empty mixing configuration"));
            }
            for _ in 0..nd {
                let m = r.get(s.nmixoutchs[i]);
                r.skip(m.count_ones() as usize * 6);
            }
        }
    }
    a.coding_mode = r.get(2) as u8;
    let nb = s.exss_size_nbits;
    match a.coding_mode {
        0 => {
            a.extension_mask = r.get(12);
            if a.extension_mask & EXSS_CORE != 0 {
                a.core_size = r.get(14) as usize + 1;
                if r.get1() != 0 {
                    r.skip(2);
                }
            }
            if a.extension_mask & EXSS_XBR != 0 {
                a.xbr_size = r.get(14) as usize + 1;
            }
            if a.extension_mask & EXSS_XXCH != 0 {
                a.xxch_size = r.get(14) as usize + 1;
            }
            if a.extension_mask & EXSS_X96 != 0 {
                a.x96_size = r.get(12) as usize + 1;
            }
            if a.extension_mask & EXSS_LBR != 0 {
                a.lbr_size = r.get(14) as usize + 1;
                if r.get1() != 0 {
                    r.skip(2);
                }
            }
            if a.extension_mask & EXSS_XLL != 0 {
                parse_xll(r, nb, a);
            }
            if a.extension_mask & 0x400 != 0 {
                r.skip(16);
            }
            if a.extension_mask & 0x800 != 0 {
                r.skip(16);
            }
        }
        1 => {
            a.extension_mask = EXSS_XLL;
            parse_xll(r, nb, a);
        }
        2 => {
            a.extension_mask = EXSS_LBR;
            a.lbr_size = r.get(14) as usize + 1;
            if r.get1() != 0 {
                r.skip(2);
            }
        }
        _ => {
            a.extension_mask = 0;
            r.skip(14 + 8);
            if r.get1() != 0 {
                r.skip(3);
            }
        }
    }
    if a.extension_mask & EXSS_XLL != 0 {
        r.skip(3);
    }
    if !r.seek(descr_pos + descr_size * 8) {
        return Err(bad("read past the end of an asset descriptor"));
    }
    Ok(())
}

fn parse_xll(r: &mut Bits<'_>, nb: u32, a: &mut ExssAsset) {
    a.xll_size = r.get(nb) as usize + 1;
    if r.get1() != 0 {
        r.skip(4);
        let delay_bits = r.get(5) + 1;
        r.skip(delay_bits as usize);
        r.skip(nb as usize);
    }
}

/// Parse the extension substream header at the start of `data` (the sync
/// word `0x64582025` first).
pub fn parse_exss(data: &[u8]) -> Result<ExssInfo> {
    let mut r = Bits::new(data);
    if data.len() < 12 {
        return Err(Error::NeedMore);
    }
    if r.get(32) != SYNC_EXSS {
        return Err(bad("no substream sync word"));
    }
    r.skip(8);
    let index = r.get(2) as u8;
    let wide = r.get1();
    let header_size = r.get(8 + 4 * wide) as usize + 1;
    let nb = 16 + 4 * wide;
    let size = r.get(nb) as usize + 1;
    if size > data.len() {
        return Err(Error::NeedMore);
    }
    let mut st = Static {
        present: r.get1() != 0,
        mix_metadata: false,
        nmixoutconfigs: 0,
        nmixoutchs: [0; 4],
        exss_size_nbits: nb,
    };
    let mut npresents = 1usize;
    let mut nassets = 1usize;
    if st.present {
        r.skip(2 + 3);
        if r.get1() != 0 {
            r.skip(36);
        }
        npresents = r.get(3) as usize + 1;
        nassets = r.get(3) as usize + 1;
        let mut masks = [0u32; 8];
        for m in masks.iter_mut().take(npresents) {
            *m = r.get(index as u32 + 1);
        }
        for m in masks.iter().take(npresents) {
            r.skip(m.count_ones() as usize * 8);
        }
        st.mix_metadata = r.get1() != 0;
        if st.mix_metadata {
            r.skip(2);
            let nbits = (r.get(2) + 1) << 2;
            st.nmixoutconfigs = r.get(2) as usize + 1;
            for i in 0..st.nmixoutconfigs {
                st.nmixoutchs[i] = count_chs_for_mask(r.get(nbits));
            }
        }
    }
    let _ = npresents;
    let mut assets = vec![ExssAsset::default(); nassets];
    let mut offset = header_size;
    for a in assets.iter_mut() {
        a.offset = offset;
        a.size = r.get(nb) as usize + 1;
        offset += a.size;
        if offset > size {
            return Err(bad("asset out of bounds"));
        }
    }
    for a in assets.iter_mut() {
        parse_descriptor(&mut r, &st, a)?;
        let mut offs = a.offset;
        let mut left = a.size;
        let mut take = |present: bool, sz: usize| -> Result<usize> {
            if !present {
                return Ok(0);
            }
            if sz > left {
                return Err(bad("invalid extension size in an asset descriptor"));
            }
            let o = offs;
            offs += sz;
            left -= sz;
            Ok(o)
        };
        a.core_offset = take(a.extension_mask & EXSS_CORE != 0, a.core_size)?;
        take(a.extension_mask & EXSS_XBR != 0, a.xbr_size)?;
        take(a.extension_mask & EXSS_XXCH != 0, a.xxch_size)?;
        a.x96_offset = take(a.extension_mask & EXSS_X96 != 0, a.x96_size)?;
        take(a.extension_mask & EXSS_LBR != 0, a.lbr_size)?;
        a.xll_offset = take(a.extension_mask & EXSS_XLL != 0, a.xll_size)?;
    }
    if r.over {
        return Err(bad("header overruns the substream"));
    }
    Ok(ExssInfo {
        index,
        size,
        header_size,
        assets,
    })
}
