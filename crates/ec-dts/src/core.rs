//! The core decoder: primary audio coding header and subframes (ETSI TS 102
//! 114 section 5), subband ADPCM, high frequency VQ, joint intensity, LFE,
//! the XCH extension and the 32-band synthesis bank.
//!
//! The arithmetic is the integer formulation (24-bit subband samples,
//! Q23 filter coefficients, 64-bit accumulators) so a decode is exact and
//! reproducible: the same bits always give the same 24-bit samples.

use crate::bits::Bits;
use crate::dct::{imdct_half_32, imdct_half_64};
use crate::header::{FrameHeader, BIT_RATES, CHANNELS};
use crate::huff::{books, Books};
use crate::math::{clip23, mul, mul23, norm};
use crate::tables_data::*;
use ec_core::{Error, Result};

pub(crate) const CH: usize = 7;
const SUBBANDS: usize = 32;
const ADPCM: usize = 4;
const STRIDE: usize = ADPCM + 128;
const LFE_HISTORY: usize = 8;
const ABITS_MAX: i32 = 26;
const SSF_SAMPLES: usize = 8;

/// Speaker slots, in the order of the standard's speaker mask.
pub(crate) const SPK_C: usize = 0;
pub(crate) const SPK_L: usize = 1;
pub(crate) const SPK_R: usize = 2;
pub(crate) const SPK_LS: usize = 3;
pub(crate) const SPK_RS: usize = 4;
pub(crate) const SPK_LFE: usize = 5;
pub(crate) const SPK_CS: usize = 6;

const MASK_C: u32 = 1 << SPK_C;
const MASK_L: u32 = 1 << SPK_L;
const MASK_R: u32 = 1 << SPK_R;
const MASK_LS: u32 = 1 << SPK_LS;
const MASK_RS: u32 = 1 << SPK_RS;
pub(crate) const MASK_LFE: u32 = 1 << SPK_LFE;
pub(crate) const MASK_CS: u32 = 1 << SPK_CS;

/// Primary channel to speaker, by `AMODE`.
const PRM_CH_TO_SPKR: [[i8; 5]; 10] = [
    [0, -1, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [0, 1, 2, -1, -1],
    [1, 2, 6, -1, -1],
    [0, 1, 2, 6, -1],
    [1, 2, 3, 4, -1],
    [0, 1, 2, 3, 4],
];

const AMODE_MASK: [u32; 10] = [
    MASK_C,
    MASK_L | MASK_R,
    MASK_L | MASK_R,
    MASK_L | MASK_R,
    MASK_L | MASK_R,
    MASK_L | MASK_R | MASK_C,
    MASK_L | MASK_R | MASK_CS,
    MASK_L | MASK_R | MASK_C | MASK_CS,
    MASK_L | MASK_R | MASK_LS | MASK_RS,
    MASK_L | MASK_R | MASK_C | MASK_LS | MASK_RS,
];

const BLOCK_CODE_NBITS: [u32; 7] = [7, 10, 12, 13, 15, 17, 19];
const QUANT_INDEX_SEL_NBITS: [u32; 10] = [1, 2, 2, 2, 2, 3, 3, 3, 3, 3];
const QUANT_INDEX_GROUP_SIZE: [u8; 10] = [1, 3, 3, 3, 3, 7, 7, 7, 7, 7];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    Core,
    Xch,
}

/// Synthesis filter history of one channel.
struct Dsp {
    hist1: Vec<i32>,
    hist2: [i32; 64],
    offset: usize,
}

impl Dsp {
    fn new() -> Dsp {
        Dsp {
            hist1: vec![0; 1024],
            hist2: [0; 64],
            offset: 0,
        }
    }
    fn clear(&mut self) {
        self.hist1.iter_mut().for_each(|x| *x = 0);
        self.hist2 = [0; 64];
        self.offset = 0;
    }
}

/// What the extension search and parse found, for the frame info.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extensions {
    /// XCH (6.1 centre surround) was present and decoded.
    pub xch: bool,
    /// X96 (96 kHz) was present and decoded.
    pub x96: bool,
    /// An XXCH extension was signalled and ignored.
    pub xxch_ignored: bool,
}

/// One decoded core frame: planar 24-bit samples per speaker slot.
pub(crate) struct Decoded {
    pub nsamples: usize,
    pub sample_rate: u32,
    pub ch_mask: u32,
    pub out: Vec<Vec<i32>>,
    pub ext: Extensions,
}

pub(crate) struct Core {
    books: &'static Books,
    // Bit stream header
    crc_present: bool,
    npcmblocks: usize,
    frame_size: usize,
    audio_mode: usize,
    sample_rate: u32,
    bit_rate: u32,
    drc_present: bool,
    ts_present: bool,
    aux_present: bool,
    ext_audio_type: u8,
    ext_audio_present: bool,
    sync_ssf: bool,
    lfe_present: usize,
    predictor_history: bool,
    filter_perfect: bool,
    es_format: bool,
    sumdiff_front: bool,
    sumdiff_surround: bool,
    // Primary audio coding header
    nsubframes: usize,
    nchannels: usize,
    ch_mask: u32,
    nsubbands: [usize; CH],
    subband_vq_start: [usize; CH],
    joint_intensity_index: [usize; CH],
    transition_mode_sel: [usize; CH],
    scale_factor_sel: [usize; CH],
    bit_allocation_sel: [usize; CH],
    quant_index_sel: [[u8; 10]; CH],
    scale_factor_adj: [[i32; 10]; CH],
    // Side information
    nsubsubframes: [usize; 16],
    prediction_mode: [[bool; 64]; CH],
    prediction_vq_index: [[u16; 64]; CH],
    bit_allocation: [[i32; 64]; CH],
    transition_mode: [[[u8; SUBBANDS]; CH]; 16],
    scale_factors: [[[i32; 2]; SUBBANDS]; CH],
    joint_scale_sel: [usize; CH],
    joint_scale_factors: [[i32; 64]; CH],
    // Sample buffers: [ch][band][ADPCM history + 128 samples]
    sb: Vec<i32>,
    lfe_samples: Vec<i32>,
    // X96 extension
    x96_sb: Vec<i32>,
    x96_rand: u32,
    x96_rev_no: u32,
    x96_crc_present: bool,
    x96_nchannels: usize,
    x96_high_res: bool,
    x96_subband_start: usize,
    // Synthesis state
    dsp: Vec<Dsp>,
    filter_mode: u8,
    lfe_x96_hist: i32,
    ext: Extensions,
}

#[inline]
fn sbi(ch: usize, band: usize) -> usize {
    (ch * SUBBANDS + band) * STRIDE
}

#[inline]
fn xbi(ch: usize, band: usize) -> usize {
    (ch * 64 + band) * STRIDE
}

impl Core {
    pub fn new() -> Core {
        Core {
            books: books(),
            crc_present: false,
            npcmblocks: 0,
            frame_size: 0,
            audio_mode: 0,
            sample_rate: 0,
            bit_rate: 0,
            drc_present: false,
            ts_present: false,
            aux_present: false,
            ext_audio_type: 0,
            ext_audio_present: false,
            sync_ssf: false,
            lfe_present: 0,
            predictor_history: false,
            filter_perfect: false,
            es_format: false,
            sumdiff_front: false,
            sumdiff_surround: false,
            nsubframes: 0,
            nchannels: 0,
            ch_mask: 0,
            nsubbands: [0; CH],
            subband_vq_start: [0; CH],
            joint_intensity_index: [0; CH],
            transition_mode_sel: [0; CH],
            scale_factor_sel: [0; CH],
            bit_allocation_sel: [0; CH],
            quant_index_sel: [[0; 10]; CH],
            scale_factor_adj: [[0; 10]; CH],
            nsubsubframes: [0; 16],
            prediction_mode: [[false; 64]; CH],
            prediction_vq_index: [[0; 64]; CH],
            bit_allocation: [[0; 64]; CH],
            transition_mode: [[[0; SUBBANDS]; CH]; 16],
            scale_factors: [[[0; 2]; SUBBANDS]; CH],
            joint_scale_sel: [0; CH],
            joint_scale_factors: [[0; 64]; CH],
            sb: vec![0; CH * SUBBANDS * STRIDE],
            lfe_samples: vec![0; LFE_HISTORY + 64],
            x96_sb: vec![0; CH * 64 * STRIDE],
            x96_rand: 1,
            x96_rev_no: 0,
            x96_crc_present: false,
            x96_nchannels: 0,
            x96_high_res: false,
            x96_subband_start: 0,
            dsp: (0..CH).map(|_| Dsp::new()).collect(),
            filter_mode: 0,
            lfe_x96_hist: 0,
            ext: Extensions::default(),
        }
    }

    /// Drop every carried-over history (ADPCM, LFE, synthesis).
    pub fn reset(&mut self) {
        self.sb.iter_mut().for_each(|x| *x = 0);
        self.x96_sb.iter_mut().for_each(|x| *x = 0);
        self.lfe_samples.iter_mut().for_each(|x| *x = 0);
        for d in &mut self.dsp {
            d.clear();
        }
    }

    fn erase_adpcm_history(&mut self) {
        for ch in 0..CH {
            for band in 0..SUBBANDS {
                let b = sbi(ch, band);
                self.sb[b..b + ADPCM].iter_mut().for_each(|x| *x = 0);
            }
        }
    }

    fn load_header(&mut self, h: &FrameHeader) {
        self.crc_present = h.crc_present;
        self.npcmblocks = h.npcmblocks as usize;
        self.frame_size = h.frame_size;
        self.audio_mode = h.amode as usize;
        self.sample_rate = h.sample_rate;
        self.bit_rate = BIT_RATES[h.bit_rate_code as usize];
        self.drc_present = h.drc_present;
        self.ts_present = h.ts_present;
        self.aux_present = h.aux_present;
        self.ext_audio_type = h.ext_audio_type;
        self.ext_audio_present = h.ext_audio_present;
        self.sync_ssf = h.sync_ssf;
        self.lfe_present = h.lfe as usize;
        self.predictor_history = h.predictor_history;
        self.filter_perfect = h.filter_perfect;
        self.es_format = h.es_format;
        self.sumdiff_front = h.sumdiff_front;
        self.sumdiff_surround = h.sumdiff_surround;
    }

    /// Decode one canonical frame. `canon` starts at the sync word and holds
    /// at least the header; `hdr` is its parsed header.
    pub fn decode(&mut self, canon: &[u8], hdr: &FrameHeader, exss_x96: Option<&[u8]>) -> Result<Decoded> {
        self.load_header(hdr);
        self.ext = Extensions::default();
        let mut r = Bits::new(canon);
        // Skip the header, which the caller parsed: re-read to find its end.
        skip_frame_header(&mut r, hdr);
        if !self.predictor_history {
            self.erase_adpcm_history();
        }
        self.parse_frame_data(&mut r, Header::Core, 0)?;

        // Optional information: time stamp, auxiliary data, extension search.
        if self.ts_present {
            r.skip(32);
        }
        let last_pos = r.pos() / 32;
        let mut xch_pos = 0usize;
        let mut x96_pos = 0usize;
        if self.ext_audio_present {
            let fs = self.frame_size.min(canon.len());
            let mut sync_pos = (fs / 4).min(canon.len() / 4) as isize - 1;
            let rd = |i: isize| -> u32 {
                let o = i as usize * 4;
                canon
                    .get(o..o + 4)
                    .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            };
            let mut w2 = 0u32;
            match self.ext_audio_type {
                0 => {
                    while sync_pos >= last_pos as isize {
                        let w1 = rd(sync_pos);
                        if w1 == 0x5A5A_5A5A {
                            let size = (w2 >> 22) as usize + 1;
                            let dist = self.frame_size as isize - sync_pos * 4;
                            if size >= 96
                                && (size as isize == dist || size as isize - 1 == dist)
                                && (w2 >> 15 & 0x7f) == 0x08
                            {
                                xch_pos = sync_pos as usize * 32 + 49;
                                break;
                            }
                        }
                        sync_pos -= 1;
                        w2 = w1;
                    }
                }
                2 => {
                    while sync_pos >= last_pos as isize {
                        let w1 = rd(sync_pos);
                        if w1 == 0x1D95_F262 {
                            let size = (w2 >> 20) as usize + 1;
                            let dist = self.frame_size as isize - sync_pos * 4;
                            if size >= 96 && size as isize == dist {
                                x96_pos = sync_pos as usize * 32 + 44;
                                break;
                            }
                        }
                        sync_pos -= 1;
                        w2 = w1;
                    }
                }
                6 => self.ext.xxch_ignored = true,
                _ => {}
            }
        }

        if xch_pos != 0 {
            let mut rx = Bits::new(canon);
            if rx.seek(xch_pos) && self.parse_xch_frame(&mut rx).is_ok() {
                self.ext.xch = true;
            } else {
                self.nchannels = CHANNELS[self.audio_mode] as usize;
                self.ch_mask = AMODE_MASK[self.audio_mode];
                if self.lfe_present != 0 {
                    self.ch_mask |= MASK_LFE;
                }
            }
        }
        if let Some(x) = exss_x96 {
            if self.parse_x96_frame_exss(x).is_ok() {
                self.ext.x96 = true;
            }
        } else if x96_pos != 0 {
            let mut rx = Bits::new(canon);
            if rx.seek(x96_pos) && self.parse_x96_frame(&mut rx).is_ok() {
                self.ext.x96 = true;
            }
        }
        self.filter()
    }

    // 5.3.2 - Primary audio coding header
    fn parse_coding_header(&mut self, r: &mut Bits<'_>, header: Header, xch_base: usize) -> Result<()> {
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated frame"));
        }
        match header {
            Header::Core => {
                self.nsubframes = r.get(4) as usize + 1;
                self.nchannels = r.get(3) as usize + 1;
                if self.nchannels != CHANNELS[self.audio_mode] as usize {
                    return Err(Error::corrupt(
                        "DTS: primary channel count disagrees with AMODE",
                    ));
                }
                self.ch_mask = AMODE_MASK[self.audio_mode];
                if self.lfe_present != 0 {
                    self.ch_mask |= MASK_LFE;
                }
            }
            Header::Xch => {
                self.nchannels = CHANNELS[self.audio_mode] as usize + 1;
                self.ch_mask |= MASK_CS;
            }
        }
        let n = self.nchannels;
        for ch in xch_base..n {
            let v = r.get(5) as usize + 2;
            if v > SUBBANDS {
                return Err(Error::corrupt("DTS: invalid subband activity count"));
            }
            self.nsubbands[ch] = v;
        }
        for ch in xch_base..n {
            self.subband_vq_start[ch] = r.get(5) as usize + 1;
        }
        for ch in xch_base..n {
            let v = r.get(3) as usize;
            if v > n {
                return Err(Error::corrupt("DTS: invalid joint intensity coding index"));
            }
            self.joint_intensity_index[ch] = v;
        }
        for ch in xch_base..n {
            self.transition_mode_sel[ch] = r.get(2) as usize;
        }
        for ch in xch_base..n {
            let v = r.get(3) as usize;
            if v == 7 {
                return Err(Error::corrupt("DTS: invalid scale factor code book"));
            }
            self.scale_factor_sel[ch] = v;
        }
        for ch in xch_base..n {
            let v = r.get(3) as usize;
            if v == 7 {
                return Err(Error::corrupt("DTS: invalid bit allocation quantizer select"));
            }
            self.bit_allocation_sel[ch] = v;
        }
        for k in 0..10 {
            for ch in xch_base..n {
                self.quant_index_sel[ch][k] = r.get(QUANT_INDEX_SEL_NBITS[k]) as u8;
            }
        }
        for k in 0..10 {
            for ch in xch_base..n {
                if self.quant_index_sel[ch][k] < QUANT_INDEX_GROUP_SIZE[k] {
                    self.scale_factor_adj[ch][k] = SCALE_FACTOR_ADJ[r.get(2) as usize] as i32;
                }
            }
        }
        if self.crc_present {
            r.skip(16);
        }
        Ok(())
    }

    fn parse_scale(&self, r: &mut Bits<'_>, scale_index: &mut i32, sel: usize) -> Result<i32> {
        let (table, size): (&[u32], usize) = if sel > 5 {
            (&SCALE_FACTOR_QUANT7, 128)
        } else {
            (&SCALE_FACTOR_QUANT6, 64)
        };
        if sel < 5 {
            *scale_index += self.books.scale_factor[sel].read(r);
        } else {
            *scale_index = r.get(sel as u32 + 1) as i32;
        }
        if (*scale_index as u32) as usize >= size {
            return Err(Error::corrupt("DTS: invalid scale factor index"));
        }
        Ok(table[*scale_index as usize] as i32)
    }

    fn parse_joint_scale(&self, r: &mut Bits<'_>, sel: usize) -> Result<i32> {
        let mut idx = if sel < 5 {
            self.books.scale_factor[sel].read(r)
        } else {
            r.get(sel as u32 + 1) as i32
        };
        idx += 64;
        if idx < 0 || idx as usize >= JOINT_SCALE_FACTORS.len() {
            return Err(Error::corrupt("DTS: invalid joint scale factor index"));
        }
        Ok(JOINT_SCALE_FACTORS[idx as usize] as i32)
    }

    // 5.4.1 - Primary audio coding side information
    fn parse_subframe_header(&mut self, r: &mut Bits<'_>, sf: usize, header: Header, xch_base: usize) -> Result<()> {
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated frame"));
        }
        if header == Header::Core {
            self.nsubsubframes[sf] = r.get(2) as usize + 1;
            r.skip(3);
        }
        let n = self.nchannels;
        for ch in xch_base..n {
            for band in 0..self.nsubbands[ch] {
                self.prediction_mode[ch][band] = r.get1() != 0;
            }
        }
        for ch in xch_base..n {
            for band in 0..self.nsubbands[ch] {
                if self.prediction_mode[ch][band] {
                    self.prediction_vq_index[ch][band] = r.get(12) as u16;
                }
            }
        }
        for ch in xch_base..n {
            let sel = self.bit_allocation_sel[ch];
            for band in 0..self.subband_vq_start[ch] {
                let abits = if sel < 5 {
                    self.books.bit_allocation[sel].read(r)
                } else {
                    r.get(sel as u32 - 1) as i32
                };
                if abits > ABITS_MAX {
                    return Err(Error::corrupt("DTS: invalid bit allocation index"));
                }
                self.bit_allocation[ch][band] = abits;
            }
        }
        for ch in xch_base..n {
            self.transition_mode[sf][ch] = [0; SUBBANDS];
            if self.nsubsubframes[sf] > 1 {
                let sel = self.transition_mode_sel[ch];
                for band in 0..self.subband_vq_start[ch] {
                    if self.bit_allocation[ch][band] != 0 {
                        self.transition_mode[sf][ch][band] =
                            self.books.transition_mode[sel].read(r) as u8;
                    }
                }
            }
        }
        for ch in xch_base..n {
            let sel = self.scale_factor_sel[ch];
            let mut scale_index = 0i32;
            for band in 0..self.subband_vq_start[ch] {
                if self.bit_allocation[ch][band] != 0 {
                    self.scale_factors[ch][band][0] = self.parse_scale(r, &mut scale_index, sel)?;
                    if self.transition_mode[sf][ch][band] != 0 {
                        self.scale_factors[ch][band][1] = self.parse_scale(r, &mut scale_index, sel)?;
                    }
                } else {
                    self.scale_factors[ch][band][0] = 0;
                }
            }
            for band in self.subband_vq_start[ch]..self.nsubbands[ch] {
                self.scale_factors[ch][band][0] = self.parse_scale(r, &mut scale_index, sel)?;
            }
        }
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let v = r.get(3) as usize;
                if v == 7 {
                    return Err(Error::corrupt("DTS: invalid joint scale factor code book"));
                }
                self.joint_scale_sel[ch] = v;
            }
        }
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let src_ch = self.joint_intensity_index[ch] - 1;
                let sel = self.joint_scale_sel[ch];
                for band in self.nsubbands[ch]..self.nsubbands[src_ch] {
                    self.joint_scale_factors[ch][band] = self.parse_joint_scale(r, sel)?;
                }
            }
        }
        if self.drc_present && header == Header::Core {
            r.skip(8);
        }
        if self.crc_present {
            r.skip(16);
        }
        Ok(())
    }

    fn extract_audio(&self, r: &mut Bits<'_>, audio: &mut [i32; 16], abits: usize, ch: usize) -> Result<bool> {
        if abits == 0 {
            audio[..SSF_SAMPLES].iter_mut().for_each(|x| *x = 0);
            return Ok(false);
        }
        if abits <= 10 {
            let sel = self.quant_index_sel[ch][abits - 1];
            if sel < QUANT_INDEX_GROUP_SIZE[abits - 1] {
                let book = &self.books.quant_index[abits - 1][sel as usize];
                for a in audio[..SSF_SAMPLES].iter_mut() {
                    *a = book.read(r);
                }
                return Ok(true);
            }
            if abits <= 7 {
                let nb = BLOCK_CODE_NBITS[abits - 1];
                let mut code1 = r.get(nb) as i64;
                let mut code2 = r.get(nb) as i64;
                let levels = QUANT_LEVELS[abits] as i64;
                let offset = (levels - 1) / 2;
                for n in 0..SSF_SAMPLES / 2 {
                    let div = code1 / levels;
                    audio[n] = (code1 - div * levels - offset) as i32;
                    code1 = div;
                }
                for n in SSF_SAMPLES / 2..SSF_SAMPLES {
                    let div = code2 / levels;
                    audio[n] = (code2 - div * levels - offset) as i32;
                    code2 = div;
                }
                if code1 | code2 != 0 {
                    return Err(Error::corrupt("DTS: failed to decode block code"));
                }
                return Ok(false);
            }
        }
        for a in audio[..SSF_SAMPLES].iter_mut() {
            *a = r.get_signed(abits as u32 - 3);
        }
        Ok(false)
    }

    // 5.5 - Primary audio data arrays
    fn parse_subframe_audio(
        &mut self,
        r: &mut Bits<'_>,
        sf: usize,
        header: Header,
        xch_base: usize,
        sub_pos: &mut usize,
        lfe_pos: &mut usize,
    ) -> Result<()> {
        let nsamples = self.nsubsubframes[sf] * SSF_SAMPLES;
        if *sub_pos + nsamples > self.npcmblocks {
            return Err(Error::corrupt("DTS: subband sample buffer overflow"));
        }
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated frame"));
        }
        let n = self.nchannels;
        let mut audio = [0i32; 16];

        // VQ encoded subbands
        for ch in xch_base..n {
            let mut vq_index = [0usize; SUBBANDS];
            for band in self.subband_vq_start[ch]..self.nsubbands[ch] {
                vq_index[band] = r.get(10) as usize;
            }
            for band in self.subband_vq_start[ch]..self.nsubbands[ch] {
                let coeff = &HIGH_FREQ_VQ[vq_index[band] * 32..vq_index[band] * 32 + 32];
                let scale = self.scale_factors[ch][band][0];
                let base = sbi(ch, band) + ADPCM + *sub_pos;
                for j in 0..nsamples {
                    self.sb[base + j] =
                        clip23(((coeff[j] as i32).wrapping_mul(scale).wrapping_add(1 << 3)) >> 4);
                }
            }
        }

        // Low frequency effect data
        if self.lfe_present != 0 && header == Header::Core {
            let nlfe = 2 * self.lfe_present * self.nsubsubframes[sf];
            for a in audio[..nlfe].iter_mut() {
                *a = r.get_signed(8);
            }
            let index = r.get(8) as usize;
            if index >= SCALE_FACTOR_QUANT7.len() {
                return Err(Error::corrupt("DTS: invalid LFE scale factor index"));
            }
            let scale = SCALE_FACTOR_QUANT7[index] as i32;
            let scale = mul23(4697620, scale);
            for k in 0..nlfe {
                let ofs = *lfe_pos + k;
                if ofs >= self.lfe_samples.len() {
                    return Err(Error::corrupt("DTS: LFE sample overflow"));
                }
                self.lfe_samples[ofs] = clip23(audio[k].wrapping_mul(scale) >> 4);
            }
            *lfe_pos += nlfe;
        }

        // Audio data
        let mut ofs = *sub_pos;
        for ssf in 0..self.nsubsubframes[sf] {
            for ch in xch_base..n {
                if r.left() < 0 {
                    return Err(Error::corrupt("DTS: truncated frame"));
                }
                for band in 0..self.subband_vq_start[ch] {
                    let abits = self.bit_allocation[ch][band] as usize;
                    let huff = self.extract_audio(r, &mut audio, abits, ch)?;
                    let step_size = if self.bit_rate == 3 {
                        LOSSLESS_QUANT[abits]
                    } else {
                        LOSSY_QUANT[abits]
                    } as i32;
                    let trans_ssf = self.transition_mode[sf][ch][band] as usize;
                    let mut scale = if trans_ssf == 0 || ssf < trans_ssf {
                        self.scale_factors[ch][band][0]
                    } else {
                        self.scale_factors[ch][band][1]
                    };
                    if huff {
                        let adj = self.scale_factor_adj[ch][abits - 1] as i64;
                        scale = clip23(((adj * scale as i64) >> 22) as i32);
                    }
                    let base = sbi(ch, band) + ADPCM + ofs;
                    dequantize(&mut self.sb[base..base + SSF_SAMPLES], &audio[..SSF_SAMPLES], step_size, scale, false);
                }
            }
            // DSYNC
            if (ssf == self.nsubsubframes[sf] - 1 || self.sync_ssf) && r.get(16) != 0xffff {
                return Err(Error::corrupt("DTS: DSYNC check failed"));
            }
            ofs += SSF_SAMPLES;
        }

        // Inverse ADPCM
        for ch in xch_base..n {
            for band in 0..self.nsubbands[ch] {
                if self.prediction_mode[ch][band] {
                    let id = self.prediction_vq_index[ch][band] as usize;
                    let base = sbi(ch, band) + ADPCM + *sub_pos;
                    for j in 0..nsamples {
                        let x = predict(id, &self.sb[base + j - ADPCM..base + j]);
                        self.sb[base + j] = clip23(self.sb[base + j].wrapping_add(x));
                    }
                }
            }
        }

        // Joint subband coding
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let src_ch = self.joint_intensity_index[ch] - 1;
                for band in self.nsubbands[ch]..self.nsubbands[src_ch] {
                    let scale = self.joint_scale_factors[ch][band];
                    let sbase = sbi(src_ch, band) + ADPCM + *sub_pos;
                    let dbase = sbi(ch, band) + ADPCM + *sub_pos;
                    for j in 0..nsamples {
                        self.sb[dbase + j] = clip23(mul(self.sb[sbase + j], scale, 17));
                    }
                }
            }
        }

        *sub_pos = ofs;
        Ok(())
    }

    fn parse_frame_data(&mut self, r: &mut Bits<'_>, header: Header, xch_base: usize) -> Result<()> {
        self.parse_coding_header(r, header, xch_base)?;
        let mut sub_pos = 0usize;
        let mut lfe_pos = LFE_HISTORY;
        for sf in 0..self.nsubframes {
            self.parse_subframe_header(r, sf, header, xch_base)?;
            self.parse_subframe_audio(r, sf, header, xch_base, &mut sub_pos, &mut lfe_pos)?;
        }
        if r.over {
            return Err(Error::corrupt("DTS: truncated frame"));
        }
        for ch in xch_base..self.nchannels {
            let mut nsubbands = self.nsubbands[ch];
            if self.joint_intensity_index[ch] != 0 {
                nsubbands = nsubbands.max(self.nsubbands[self.joint_intensity_index[ch] - 1]);
            }
            let np = self.npcmblocks;
            for band in 0..nsubbands {
                let b = sbi(ch, band);
                for k in 0..ADPCM {
                    self.sb[b + k] = self.sb[b + np + k];
                }
            }
            for band in nsubbands..SUBBANDS {
                let b = sbi(ch, band);
                self.sb[b..b + ADPCM + np].iter_mut().for_each(|x| *x = 0);
            }
        }
        Ok(())
    }

    fn erase_x96_adpcm_history(&mut self) {
        for ch in 0..CH {
            for band in 0..64 {
                let b = xbi(ch, band);
                self.x96_sb[b..b + ADPCM].iter_mut().for_each(|x| *x = 0);
            }
        }
    }

    fn rand_x96(&mut self) -> i32 {
        self.x96_rand = self.x96_rand.wrapping_mul(1103515245).wrapping_add(12345);
        (self.x96_rand & 0x7fff_ffff) as i32 - 0x4000_0000
    }

    fn parse_x96_coding_header(&mut self, r: &mut Bits<'_>, exss: bool, xch_base: usize) -> Result<()> {
        let header_pos = r.pos();
        let mut header_size = 0usize;
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated X96 frame"));
        }
        if exss {
            header_size = r.get(7) as usize + 1;
        }
        self.x96_high_res = r.get1() != 0;
        if self.x96_rev_no < 8 {
            self.x96_subband_start = r.get(5) as usize;
            if self.x96_subband_start > 27 {
                return Err(Error::corrupt("DTS: invalid X96 subband start index"));
            }
        } else {
            self.x96_subband_start = SUBBANDS;
        }
        let n = self.x96_nchannels;
        for ch in xch_base..n {
            let v = r.get(6) as usize + 1;
            if v < SUBBANDS {
                return Err(Error::corrupt("DTS: invalid X96 subband activity count"));
            }
            self.nsubbands[ch] = v;
        }
        for ch in xch_base..n {
            let mut v = r.get(3) as usize;
            if v != 0 && xch_base != 0 {
                v += xch_base - 1;
            }
            if v > n {
                return Err(Error::corrupt("DTS: invalid X96 joint intensity coding index"));
            }
            self.joint_intensity_index[ch] = v;
        }
        for ch in xch_base..n {
            let v = r.get(3) as usize;
            if v >= 6 {
                return Err(Error::corrupt("DTS: invalid X96 scale factor code book"));
            }
            self.scale_factor_sel[ch] = v;
        }
        for ch in xch_base..n {
            self.bit_allocation_sel[ch] = r.get(3) as usize;
        }
        let nbooks = 6 + 4 * usize::from(self.x96_high_res);
        for k in 0..nbooks {
            for ch in xch_base..n {
                self.quant_index_sel[ch][k] = r.get(QUANT_INDEX_SEL_NBITS[k]) as u8;
            }
        }
        if exss {
            if !r.seek(header_pos + header_size * 8) {
                return Err(Error::corrupt("DTS: read past the end of an X96 channel set header"));
            }
        } else if self.crc_present {
            r.skip(16);
        }
        Ok(())
    }

    fn parse_x96_subframe_header(&mut self, r: &mut Bits<'_>, xch_base: usize) -> Result<()> {
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated X96 frame"));
        }
        let n = self.x96_nchannels;
        let start = self.x96_subband_start;
        for ch in xch_base..n {
            for band in start..self.nsubbands[ch] {
                self.prediction_mode[ch][band] = r.get1() != 0;
            }
        }
        for ch in xch_base..n {
            for band in start..self.nsubbands[ch] {
                if self.prediction_mode[ch][band] {
                    self.prediction_vq_index[ch][band] = r.get(12) as u16;
                }
            }
        }
        let hr = usize::from(self.x96_high_res);
        for ch in xch_base..n {
            let sel = self.bit_allocation_sel[ch];
            let mut abits = 0i32;
            for band in start..self.nsubbands[ch] {
                if sel < 7 {
                    abits += self.books.quant_index[5 + 2 * hr][sel].read(r);
                } else {
                    abits = r.get(3 + hr as u32) as i32;
                }
                if abits < 0 || abits > 7 + 8 * hr as i32 {
                    return Err(Error::corrupt("DTS: invalid X96 bit allocation index"));
                }
                self.bit_allocation[ch][band] = abits;
            }
        }
        for ch in xch_base..n {
            let sel = self.scale_factor_sel[ch];
            let mut scale_index = 0i32;
            for band in start..self.nsubbands[ch] {
                self.scale_factors[ch][band >> 1][band & 1] = self.parse_scale(r, &mut scale_index, sel)?;
            }
        }
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let v = r.get(3) as usize;
                if v == 7 {
                    return Err(Error::corrupt("DTS: invalid X96 joint scale factor code book"));
                }
                self.joint_scale_sel[ch] = v;
            }
        }
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let src_ch = self.joint_intensity_index[ch] - 1;
                let sel = self.joint_scale_sel[ch];
                for band in self.nsubbands[ch]..self.nsubbands[src_ch] {
                    self.joint_scale_factors[ch][band] = self.parse_joint_scale(r, sel)?;
                }
            }
        }
        if self.crc_present {
            r.skip(16);
        }
        Ok(())
    }

    fn parse_x96_subframe_audio(&mut self, r: &mut Bits<'_>, sf: usize, xch_base: usize, sub_pos: &mut usize) -> Result<()> {
        let nsamples = self.nsubsubframes[sf] * SSF_SAMPLES;
        if *sub_pos + nsamples > self.npcmblocks {
            return Err(Error::corrupt("DTS: X96 subband sample buffer overflow"));
        }
        if r.left() < 0 {
            return Err(Error::corrupt("DTS: truncated X96 frame"));
        }
        let n = self.x96_nchannels;
        let start = self.x96_subband_start;
        let mut audio = [0i32; 16];

        // VQ encoded or unallocated subbands
        for ch in xch_base..n {
            for band in start..self.nsubbands[ch] {
                let scale = self.scale_factors[ch][band >> 1][band & 1];
                let base = xbi(ch, band) + ADPCM + *sub_pos;
                match self.bit_allocation[ch][band] {
                    0 => {
                        if scale <= 1 {
                            self.x96_sb[base..base + nsamples].iter_mut().for_each(|x| *x = 0);
                        } else {
                            for k in 0..nsamples {
                                let rnd = self.rand_x96();
                                self.x96_sb[base + k] = mul(rnd, scale, 31);
                            }
                        }
                    }
                    1 => {
                        let mut at = base;
                        for ssf in 0..self.nsubsubframes[sf].div_ceil(2) {
                            let idx = r.get(10) as usize;
                            let vq = &HIGH_FREQ_VQ[idx * 32..idx * 32 + 32];
                            for k in 0..(nsamples - ssf * 16).min(16) {
                                self.x96_sb[at] =
                                    clip23(((vq[k] as i32).wrapping_mul(scale).wrapping_add(1 << 3)) >> 4);
                                at += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Audio data
        let mut ofs = *sub_pos;
        for ssf in 0..self.nsubsubframes[sf] {
            for ch in xch_base..n {
                if r.left() < 0 {
                    return Err(Error::corrupt("DTS: truncated X96 frame"));
                }
                for band in start..self.nsubbands[ch] {
                    let abits = self.bit_allocation[ch][band] - 1;
                    if abits < 1 {
                        continue;
                    }
                    let abits = abits as usize;
                    self.extract_audio(r, &mut audio, abits, ch)?;
                    let step_size = if self.bit_rate == 3 {
                        LOSSLESS_QUANT[abits]
                    } else {
                        LOSSY_QUANT[abits]
                    } as i32;
                    let scale = self.scale_factors[ch][band >> 1][band & 1];
                    let base = xbi(ch, band) + ADPCM + ofs;
                    dequantize(&mut self.x96_sb[base..base + SSF_SAMPLES], &audio[..SSF_SAMPLES], step_size, scale, false);
                }
            }
            if (ssf == self.nsubsubframes[sf] - 1 || self.sync_ssf) && r.get(16) != 0xffff {
                return Err(Error::corrupt("DTS: X96-DSYNC check failed"));
            }
            ofs += SSF_SAMPLES;
        }

        // Inverse ADPCM
        for ch in xch_base..n {
            for band in start..self.nsubbands[ch] {
                if self.prediction_mode[ch][band] {
                    let id = self.prediction_vq_index[ch][band] as usize;
                    let base = xbi(ch, band) + ADPCM + *sub_pos;
                    for j in 0..nsamples {
                        let x = predict(id, &self.x96_sb[base + j - ADPCM..base + j]);
                        self.x96_sb[base + j] = clip23(self.x96_sb[base + j].wrapping_add(x));
                    }
                }
            }
        }

        // Joint subband coding
        for ch in xch_base..n {
            if self.joint_intensity_index[ch] != 0 {
                let src_ch = self.joint_intensity_index[ch] - 1;
                for band in self.nsubbands[ch]..self.nsubbands[src_ch] {
                    let scale = self.joint_scale_factors[ch][band];
                    let sbase = xbi(src_ch, band) + ADPCM + *sub_pos;
                    let dbase = xbi(ch, band) + ADPCM + *sub_pos;
                    for j in 0..nsamples {
                        self.x96_sb[dbase + j] = clip23(mul(self.x96_sb[sbase + j], scale, 17));
                    }
                }
            }
        }

        *sub_pos = ofs;
        Ok(())
    }

    fn parse_x96_frame_data(&mut self, r: &mut Bits<'_>, exss: bool, xch_base: usize) -> Result<()> {
        self.parse_x96_coding_header(r, exss, xch_base)?;
        let mut sub_pos = 0usize;
        for sf in 0..self.nsubframes {
            self.parse_x96_subframe_header(r, xch_base)?;
            self.parse_x96_subframe_audio(r, sf, xch_base, &mut sub_pos)?;
        }
        if r.over {
            return Err(Error::corrupt("DTS: truncated X96 frame"));
        }
        let np = self.npcmblocks;
        for ch in xch_base..self.x96_nchannels {
            let mut nsubbands = self.nsubbands[ch];
            if self.joint_intensity_index[ch] != 0 {
                nsubbands = nsubbands.max(self.nsubbands[self.joint_intensity_index[ch] - 1]);
            }
            for band in 0..64 {
                let b = xbi(ch, band);
                if band >= self.x96_subband_start && band < nsubbands {
                    for k in 0..ADPCM {
                        self.x96_sb[b + k] = self.x96_sb[b + np + k];
                    }
                } else {
                    self.x96_sb[b..b + ADPCM + np].iter_mut().for_each(|x| *x = 0);
                }
            }
        }
        Ok(())
    }

    fn parse_x96_frame(&mut self, r: &mut Bits<'_>) -> Result<()> {
        self.x96_rev_no = r.get(4);
        if self.x96_rev_no < 1 || self.x96_rev_no > 8 {
            return Err(Error::corrupt("DTS: invalid X96 revision"));
        }
        self.x96_crc_present = false;
        self.x96_nchannels = self.nchannels;
        if !self.predictor_history {
            self.erase_x96_adpcm_history();
        }
        self.parse_x96_frame_data(r, false, 0)
    }

    fn parse_x96_frame_exss(&mut self, data: &[u8]) -> Result<()> {
        let mut r = Bits::new(data);
        let header_pos = r.pos();
        if r.get(32) != 0x1D95_F262 {
            return Err(Error::corrupt("DTS: invalid X96 sync word"));
        }
        let header_size = r.get(6) as usize + 1;
        self.x96_rev_no = r.get(4);
        if self.x96_rev_no < 1 || self.x96_rev_no > 8 {
            return Err(Error::corrupt("DTS: invalid X96 revision"));
        }
        self.x96_crc_present = r.get1() != 0;
        let nchsets = r.get(2) as usize + 1;
        let mut frame_size = [0usize; 4];
        let mut nch = [0usize; 4];
        for fs in frame_size.iter_mut().take(nchsets) {
            *fs = r.get(12) as usize + 1;
        }
        for c in nch.iter_mut().take(nchsets) {
            *c = r.get(3) as usize + 1;
        }
        if !r.seek(header_pos + header_size * 8) {
            return Err(Error::corrupt("DTS: read past the end of the X96 frame header"));
        }
        if !self.predictor_history {
            self.erase_x96_adpcm_history();
        }
        self.x96_nchannels = 0;
        let mut base_ch = 0usize;
        for i in 0..nchsets {
            let pos = r.pos();
            if base_ch + nch[i] <= self.nchannels {
                self.x96_nchannels = base_ch + nch[i];
                self.parse_x96_frame_data(&mut r, true, base_ch)?;
            }
            base_ch += nch[i];
            if !r.seek(pos + frame_size[i] * 8) {
                return Err(Error::corrupt("DTS: read past the end of an X96 channel set"));
            }
        }
        Ok(())
    }

    fn parse_xch_frame(&mut self, r: &mut Bits<'_>) -> Result<()> {
        if self.ch_mask & MASK_CS != 0 {
            return Err(Error::corrupt("DTS: XCH with Cs speaker already present"));
        }
        let base = self.nchannels;
        self.parse_frame_data(r, Header::Xch, base)
    }

    /// Map a primary channel to its speaker slot, -1 when unmapped.
    fn map_prm_ch_to_spkr(&self, ch: usize) -> i32 {
        let pos = CHANNELS[self.audio_mode] as usize;
        if ch < pos {
            return PRM_CH_TO_SPKR[self.audio_mode][ch] as i32;
        }
        if self.ext.xch && ch == pos {
            return SPK_CS as i32;
        }
        -1
    }

    fn filter(&mut self) -> Result<Decoded> {
        let np = self.npcmblocks;
        let x96 = self.ext.x96;
        let x96_nch = if x96 { self.x96_nchannels } else { 0 };
        let nsamples = (np * 32) << usize::from(x96);
        let mut out: Vec<Vec<i32>> = (0..CH).map(|_| Vec::new()).collect();
        for spk in 0..CH {
            if self.ch_mask & (1 << spk) != 0 {
                out[spk] = vec![0; nsamples];
            }
        }
        let mode = u8::from(x96);
        if self.filter_mode != mode {
            for d in &mut self.dsp {
                d.clear();
            }
            self.lfe_x96_hist = 0;
            self.filter_mode = mode;
        }
        let window: &[i32; 512] = if self.filter_perfect {
            &FIR_32_PERFECT
        } else {
            &FIR_32_NONPERFECT
        };
        for ch in 0..self.nchannels {
            let spkr = self.map_prm_ch_to_spkr(ch);
            if spkr < 0 {
                return Err(Error::corrupt("DTS: channel without a speaker"));
            }
            let dsp = &mut self.dsp[ch];
            let o = &mut out[spkr as usize];
            if x96 {
                let mut input = [0i32; 64];
                for j in 0..np {
                    for i in 0..32 {
                        let lo = self.sb[sbi(ch, i) + ADPCM + j];
                        input[i] = if ch < x96_nch {
                            lo.wrapping_add(self.x96_sb[xbi(ch, i) + ADPCM + j])
                        } else {
                            lo
                        };
                    }
                    for i in 32..64 {
                        input[i] = if ch < x96_nch {
                            self.x96_sb[xbi(ch, i) + ADPCM + j]
                        } else {
                            0
                        };
                    }
                    synth_filter_64(dsp, &FIR_64, &mut o[j * 64..j * 64 + 64], &input);
                }
            } else {
                let mut input = [0i32; 32];
                for j in 0..np {
                    for i in 0..SUBBANDS {
                        input[i] = self.sb[sbi(ch, i) + ADPCM + j];
                    }
                    synth_filter_32(dsp, window, &mut o[j * 32..j * 32 + 32], &input);
                }
            }
        }

        // LFE
        if self.lfe_present != 0 {
            let dec128 = self.lfe_present == 1;
            let nlfe = np >> (usize::from(dec128) + 1);
            let mut base = vec![0i32; np * 32];
            if dec128 {
                // The fixed-point reference has no 128x decimation filter;
                // interpolate in floating point and round to the same grid.
                for i in 0..nlfe {
                    for j in 0..64 {
                        let (mut a, mut b) = (0f64, 0f64);
                        for k in 0..4 {
                            let s = self.lfe_samples[LFE_HISTORY + i - k] as f64;
                            a += LFE_FIR_128[j * 4 + k] as f64 * s;
                            b += LFE_FIR_128[255 - j * 4 - k] as f64 * s;
                        }
                        base[128 * i + j] = clip23(a.round() as i32);
                        base[128 * i + 64 + j] = clip23(b.round() as i32);
                    }
                }
            } else {
                for i in 0..nlfe {
                    for j in 0..32 {
                        let (mut a, mut b) = (0i64, 0i64);
                        for k in 0..8 {
                            let s = self.lfe_samples[LFE_HISTORY + i - k] as i64;
                            a += LFE_FIR_64[j * 8 + k] as i64 * s;
                            b += LFE_FIR_64[255 - j * 8 - k] as i64 * s;
                        }
                        base[64 * i + j] = clip23(norm(a, 23));
                        base[64 * i + 32 + j] = clip23(norm(b, 23));
                    }
                }
            }
            let o = &mut out[SPK_LFE];
            if x96 {
                // Smooth the 96 kHz oversampled LFE to attenuate the image
                // between 47.6 and 48 kHz.
                let mut prev = self.lfe_x96_hist;
                for (i, &src) in base.iter().enumerate() {
                    let a = 2097471i64 * src as i64 + 6291137i64 * prev as i64;
                    let b = 6291137i64 * src as i64 + 2097471i64 * prev as i64;
                    prev = src;
                    o[2 * i] = clip23(norm(a, 23));
                    o[2 * i + 1] = clip23(norm(b, 23));
                }
                self.lfe_x96_hist = prev;
            } else {
                o.copy_from_slice(&base);
            }
            for n in (0..LFE_HISTORY).rev() {
                self.lfe_samples[n] = self.lfe_samples[nlfe + n];
            }
        }

        // Undo the embedded XCH downmix of ES mastered material.
        if self.es_format && self.ext.xch && self.audio_mode >= 8 {
            let cs = out[SPK_CS].clone();
            for i in 0..nsamples {
                let c = mul23(cs[i], 5931520);
                out[SPK_LS][i] = out[SPK_LS][i].wrapping_sub(c);
                out[SPK_RS][i] = out[SPK_RS][i].wrapping_sub(c);
            }
        }

        if !self.ext.xch {
            if (self.sumdiff_front && self.audio_mode > 0) || self.audio_mode == 3 {
                butterflies(&mut out, SPK_L, SPK_R);
            }
            if self.sumdiff_surround && self.audio_mode >= 8 {
                butterflies(&mut out, SPK_LS, SPK_RS);
            }
        }

        Ok(Decoded {
            nsamples,
            sample_rate: self.sample_rate << u32::from(x96),
            ch_mask: self.ch_mask,
            out,
            ext: self.ext,
        })
    }
}

fn butterflies(out: &mut [Vec<i32>], a: usize, b: usize) {
    let (x, y) = if a < b {
        let (lo, hi) = out.split_at_mut(b);
        (&mut lo[a], &mut hi[0])
    } else {
        let (lo, hi) = out.split_at_mut(a);
        (&mut hi[0], &mut lo[b])
    };
    for (p, q) in x.iter_mut().zip(y.iter_mut()) {
        let t = p.wrapping_sub(*q);
        *p = p.wrapping_add(*q);
        *q = t;
    }
}

/// Skip over the core frame header the caller already parsed.
fn skip_frame_header(r: &mut Bits<'_>, h: &FrameHeader) {
    // 32 sync, 1 normal, 5 deficit, 1 crc, 7 nblks, 14 fsize, 6 amode,
    // 4 sfreq, 5 rate, 1 reserved, 1 drc, 1 ts, 1 aux, 1 hdcd, 3 ext type,
    // 1 ext present, 1 sync, 2 lfe, 1 predictor history = 88 bits.
    r.skip(88);
    if h.crc_present {
        r.skip(16);
    }
    // 1 filter, 4 rev, 2 copy hist, 3 pcmr, 1+1 sumdiff, 4 dialnorm = 16.
    r.skip(16);
}

/// ff-compatible dequantization: scale to the 22-bit fraction of the 24-bit
/// grid, limiting the combined step and scale factor to 24 significant bits.
fn dequantize(output: &mut [i32], input: &[i32], step_size: i32, scale: i32, residual: bool) {
    let mut step_scale = step_size as i64 * scale as i64;
    let mut shift = 0u32;
    if step_scale > (1 << 23) {
        shift = (63 - (step_scale >> 23).leading_zeros()) + 1;
        step_scale >>= shift;
    }
    for (o, &i) in output.iter_mut().zip(input) {
        let bits = (22 - shift as i32).max(0) as u32;
        let v = clip23(norm(i as i64 * step_scale, bits));
        if residual {
            *o = o.wrapping_add(v);
        } else {
            *o = v;
        }
    }
}

/// Four-tap ADPCM prediction from the four samples before `hist.len()`.
fn predict(pred_vq_index: usize, hist: &[i32]) -> i32 {
    let coeff = &ADPCM_VB[pred_vq_index * 4..pred_vq_index * 4 + 4];
    let mut pred: i64 = 0;
    for i in 0..ADPCM {
        pred += hist[ADPCM - 1 - i] as i64 * coeff[i] as i64;
    }
    clip23(norm(pred, 13))
}

/// One subband sample per band in, 32 PCM samples out.
fn synth_filter_32(dsp: &mut Dsp, window: &[i32; 512], out: &mut [i32], input: &[i32; 32]) {
    let off = dsp.offset;
    imdct_half_32(&mut dsp.hist1[off..off + 32], input);
    let sb = &dsp.hist1;
    for i in 0..16 {
        let mut a: i64 = (dsp.hist2[i] as i64) << 21;
        let mut b: i64 = (dsp.hist2[i + 16] as i64) << 21;
        let mut c: i64 = 0;
        let mut d: i64 = 0;
        let mut j = 0;
        while j < 512 {
            let at = |k: usize| sb[(off + k + j) & 511] as i64;
            a += window[i + j] as i64 * at(i);
            b += window[i + j + 16] as i64 * at(15 - i);
            c += window[i + j + 32] as i64 * at(16 + i);
            d += window[i + j + 48] as i64 * at(31 - i);
            j += 64;
        }
        out[i] = clip23(norm(a, 21));
        out[i + 16] = clip23(norm(b, 21));
        dsp.hist2[i] = norm(c, 21);
        dsp.hist2[i + 16] = norm(d, 21);
    }
    dsp.offset = (off.wrapping_sub(32)) & 511;
}

/// One subband sample per band in, 64 PCM samples out (X96 bank).
fn synth_filter_64(dsp: &mut Dsp, window: &[i32; 1024], out: &mut [i32], input: &[i32; 64]) {
    let off = dsp.offset;
    imdct_half_64(&mut dsp.hist1[off..off + 64], input);
    let sb = &dsp.hist1;
    for i in 0..32 {
        let mut a: i64 = (dsp.hist2[i] as i64) << 20;
        let mut b: i64 = (dsp.hist2[i + 32] as i64) << 20;
        let mut c: i64 = 0;
        let mut d: i64 = 0;
        let mut j = 0;
        while j < 1024 {
            let at = |k: usize| sb[(off + k + j) & 1023] as i64;
            a += window[i + j] as i64 * at(i);
            b += window[i + j + 32] as i64 * at(31 - i);
            c += window[i + j + 64] as i64 * at(32 + i);
            d += window[i + j + 96] as i64 * at(63 - i);
            j += 128;
        }
        out[i] = clip23(norm(a, 20));
        out[i + 32] = clip23(norm(b, 20));
        dsp.hist2[i] = norm(c, 20);
        dsp.hist2[i + 32] = norm(d, 20);
    }
    dsp.offset = off.wrapping_sub(64) & 1023;
}
