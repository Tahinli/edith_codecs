//! Parametric Stereo (ISO/IEC 14496-3 subpart 8): the `ps_data()` parser.
//!
//! PS rides inside an SBR extension payload (`bs_extension_id` 2, see
//! [`crate::sbr_payload`]). Its state persists across frames: a frame whose
//! header bit is clear keeps the previous frame's stereo band configuration,
//! and the delta-coded parameters refer back to the previous envelope. The
//! structure follows FFmpeg's `aacps_common.c` (`ff_ps_read_data`), including
//! its handling of stale fields, so that the decoded parameters match the
//! reference decoder frame for frame.

use crate::ps_huff;
use ec_core::{BitReader, Error, Result};
use std::sync::OnceLock;

/// Envelopes per frame, at most (`PS_MAX_NUM_ENV`, plus one fake envelope).
pub const MAX_ENV: usize = 5;
/// Stereo parameter bands, at most (34-band mode).
pub const MAX_PAR: usize = 34;

/// Envelope count per frame class (FIXFIX, VARVAR table of `bs_num_env`).
const NUM_ENV_TAB: [[usize; 4]; 2] = [[0, 1, 2, 4], [1, 2, 3, 4]];
/// IID/ICC parameter band count per `iid_mode`/`icc_mode`.
const NR_IIDICC_PAR_TAB: [usize; 6] = [10, 20, 34, 10, 20, 34];
/// IPD/OPD parameter band count per `iid_mode`.
const NR_IIDOPD_PAR_TAB: [usize; 6] = [5, 11, 17, 5, 11, 17];

/// Huffman table indices into [`ps_huff::TABLES`].
const HUFF_IID_DF1: usize = 0;
const HUFF_IID_DT1: usize = 1;
const HUFF_IID_DF0: usize = 2;
const HUFF_IID_DT0: usize = 3;
const HUFF_ICC_DF: usize = 4;
const HUFF_ICC_DT: usize = 5;
const HUFF_IPD_DF: usize = 6;
const HUFF_IPD_DT: usize = 7;
const HUFF_OPD_DF: usize = 8;
const HUFF_OPD_DT: usize = 9;

/// `huff_iid[2 * dt + iid_quant]`: which IID table a delta/quant pair selects.
const HUFF_IID: [usize; 4] = [HUFF_IID_DF0, HUFF_IID_DF1, HUFF_IID_DT0, HUFF_IID_DT1];

/// The persistent PS parser state (FFmpeg's `PSCommonContext`).
#[derive(Clone, Debug, PartialEq)]
pub struct PsCommon {
    /// A PS header has been parsed and the stereo processing is active.
    pub start: bool,
    pub enable_iid: bool,
    pub iid_quant: bool,
    pub nr_iid_par: usize,
    pub nr_ipdopd_par: usize,
    pub enable_icc: bool,
    pub icc_mode: usize,
    pub nr_icc_par: usize,
    pub enable_ext: bool,
    pub frame_class: usize,
    pub num_env_old: usize,
    pub num_env: usize,
    pub enable_ipdopd: bool,
    /// Envelope borders in QMF slots; entry 0 is -1 and the last is 31.
    pub border_position: [i32; MAX_ENV + 1],
    pub iid_par: [[i8; MAX_PAR]; MAX_ENV],
    pub icc_par: [[i8; MAX_PAR]; MAX_ENV],
    pub ipd_par: [[i8; MAX_PAR]; MAX_ENV],
    pub opd_par: [[i8; MAX_PAR]; MAX_ENV],
    pub is34bands: bool,
    pub is34bands_old: bool,
}

impl Default for PsCommon {
    fn default() -> PsCommon {
        PsCommon {
            start: false,
            enable_iid: false,
            iid_quant: false,
            nr_iid_par: 0,
            nr_ipdopd_par: 0,
            enable_icc: false,
            icc_mode: 0,
            nr_icc_par: 0,
            enable_ext: false,
            frame_class: 0,
            num_env_old: 0,
            num_env: 0,
            enable_ipdopd: false,
            border_position: [0; MAX_ENV + 1],
            iid_par: [[0; MAX_PAR]; MAX_ENV],
            icc_par: [[0; MAX_PAR]; MAX_ENV],
            ipd_par: [[0; MAX_PAR]; MAX_ENV],
            opd_par: [[0; MAX_PAR]; MAX_ENV],
            is34bands: false,
            is34bands_old: false,
        }
    }
}

/// One prefix-code table: every codeword as (length, code, parameter value).
struct Vlc {
    codes: Vec<(u32, u32, i32)>,
    max_len: u32,
}

/// The ten PS Huffman tables, with canonical codes assigned from the lengths.
fn vlcs() -> &'static [Vlc] {
    static VLCS: OnceLock<Vec<Vlc>> = OnceLock::new();
    VLCS.get_or_init(|| {
        ps_huff::TABLES
            .iter()
            .zip(ps_huff::OFFSETS)
            .map(|(table, offset)| build_codes(table, offset))
            .collect()
    })
}

/// Assigns the codewords of one table the way FFmpeg does
/// (`ff_vlc_init_from_lengths`): each entry takes the next free slice of the
/// code space in table order, so the codes are not sorted by length. The
/// lengths alone do not fix the codes, which is why sorting them canonically
/// misreads every fine-IID and ICC frame. Every table in the standard is a
/// complete prefix code, which the unit tests check.
fn build_codes(table: &[(u8, u8)], offset: i32) -> Vlc {
    let mut codes = Vec::with_capacity(table.len());
    // Position in the code space, in units of 2^-32 (a 64-bit sum: no overflow).
    let mut next: u64 = 0;
    let mut max_len = 0;
    for &(value, len) in table {
        let len = u32::from(len).clamp(1, 32);
        let code = (next >> (32 - len)) as u32;
        codes.push((len, code, i32::from(value) + offset));
        next += 1u64 << (32 - len);
        max_len = max_len.max(len);
    }
    Vlc { codes, max_len }
}

/// Reads one codeword of table `which` and returns its parameter value.
fn read_vlc(r: &mut BitReader<'_>, which: usize) -> Result<i32> {
    let table = &vlcs()[which];
    let mut code: u32 = 0;
    for len in 1..=table.max_len {
        code = (code << 1) | u32::from(r.read_bit()?);
        if let Some(&(_, _, value)) = table
            .codes
            .iter()
            .find(|&&(l, c, _)| l == len && c == code)
        {
            return Ok(value);
        }
    }
    Err(Error::corrupt("ps: no codeword matches the bitstream"))
}

/// Reads one IID/ICC/IPD/OPD parameter row (FFmpeg's `READ_PAR_DATA`).
///
/// `dt` selects time-differential coding against the previous envelope (the
/// last envelope of the previous frame for `e == 0`), otherwise the values are
/// frequency-differential, accumulated across the bands. `mask` (IPD/OPD) keeps
/// only the low three bits; `invalid` rejects a value the spec does not allow.
#[allow(clippy::too_many_arguments)] // mirrors FFmpeg's READ_PAR_DATA parameters
fn read_par(
    r: &mut BitReader<'_>,
    par: &mut [[i8; MAX_PAR]; MAX_ENV],
    table: usize,
    num: usize,
    e: usize,
    dt: bool,
    num_env_old: usize,
    mask: Option<i32>,
    invalid: impl Fn(i32) -> bool,
) -> Result<()> {
    if dt {
        let e_prev = if e > 0 { e - 1 } else { num_env_old.saturating_sub(1) };
        let prev = par[e_prev];
        for (b, slot) in par[e].iter_mut().enumerate().take(num) {
            let mut val = i32::from(prev[b]) + read_vlc(r, table)?;
            if let Some(m) = mask {
                val &= m;
            }
            *slot = val as i8;
            if invalid(i32::from(*slot)) {
                return Err(Error::corrupt("ps: parameter out of range"));
            }
        }
    } else {
        let mut val = 0i32;
        for slot in par[e].iter_mut().take(num) {
            val += read_vlc(r, table)?;
            if let Some(m) = mask {
                val &= m;
            }
            *slot = val as i8;
            if invalid(i32::from(*slot)) {
                return Err(Error::corrupt("ps: parameter out of range"));
            }
        }
    }
    Ok(())
}

/// Skips `n` bits, stopping at the end of the buffer rather than failing.
fn skip_clamped(r: &mut BitReader<'_>, n: u64) {
    let n = n.min(r.bits_remaining());
    // `n` is clamped to what remains, so the skip cannot run past the buffer.
    let _ = r.skip_bits(n);
}

impl PsCommon {
    /// Parses one `ps_data()` from `r`, which holds `bits_left` bits of SBR
    /// extension payload after the 2-bit extension id. Returns the bits the
    /// PS data occupies. Malformed data disables PS for the frame (the
    /// stereo state is cleared and the PS payload is skipped), as FFmpeg does:
    /// the caller then reconstructs the mono SBR output on both channels.
    pub fn read_data(&mut self, r: &mut BitReader<'_>, bits_left: u64) -> u64 {
        let mut trial = r.clone();
        match self.parse(&mut trial) {
            Ok(consumed) if consumed <= bits_left => {
                *r = trial;
                consumed
            }
            _ => {
                self.start = false;
                self.iid_par = [[0; MAX_PAR]; MAX_ENV];
                self.icc_par = [[0; MAX_PAR]; MAX_ENV];
                self.ipd_par = [[0; MAX_PAR]; MAX_ENV];
                self.opd_par = [[0; MAX_PAR]; MAX_ENV];
                skip_clamped(r, bits_left);
                bits_left
            }
        }
    }

    /// The body of `ps_data()`; returns the bits consumed, leaving `r` after them.
    fn parse(&mut self, r: &mut BitReader<'_>) -> Result<u64> {
        let start_bit = r.bit_position();
        let header = r.read_bit()?;
        if header {
            self.enable_iid = r.read_bit()?;
            if self.enable_iid {
                let iid_mode = r.read_bits(3)? as usize;
                if iid_mode > 5 {
                    return Err(Error::corrupt("ps: reserved iid_mode"));
                }
                self.nr_iid_par = NR_IIDICC_PAR_TAB[iid_mode];
                self.iid_quant = iid_mode > 2;
                self.nr_ipdopd_par = NR_IIDOPD_PAR_TAB[iid_mode];
            }
            self.enable_icc = r.read_bit()?;
            if self.enable_icc {
                self.icc_mode = r.read_bits(3)? as usize;
                if self.icc_mode > 5 {
                    return Err(Error::corrupt("ps: reserved icc_mode"));
                }
                self.nr_icc_par = NR_IIDICC_PAR_TAB[self.icc_mode];
            }
            self.enable_ext = r.read_bit()?;
        }
        self.frame_class = usize::from(r.read_bit()?);
        self.num_env_old = self.num_env;
        self.num_env = NUM_ENV_TAB[self.frame_class][r.read_bits(2)? as usize];
        self.border_position[0] = -1;
        if self.frame_class == 1 {
            for e in 1..=self.num_env {
                self.border_position[e] = r.read_bits(5)? as i32;
                if self.border_position[e] < self.border_position[e - 1] {
                    return Err(Error::corrupt("ps: envelope borders out of order"));
                }
            }
        } else {
            let log2 = match self.num_env {
                0 | 1 => 0,
                2 | 3 => 1,
                _ => 2,
            };
            for e in 1..=self.num_env {
                self.border_position[e] = ((e * 32) >> log2) as i32 - 1;
            }
        }

        if self.enable_iid {
            let quant = usize::from(self.iid_quant);
            let limit = 7 + 8 * quant as i32;
            for e in 0..self.num_env {
                let dt = r.read_bit()?;
                let table = HUFF_IID[2 * usize::from(dt) + quant];
                read_par(
                    r,
                    &mut self.iid_par,
                    table,
                    self.nr_iid_par,
                    e,
                    dt,
                    self.num_env_old,
                    None,
                    |v| v.abs() > limit,
                )?;
            }
        } else {
            self.iid_par = [[0; MAX_PAR]; MAX_ENV];
        }

        if self.enable_icc {
            for e in 0..self.num_env {
                let dt = r.read_bit()?;
                let table = if dt { HUFF_ICC_DT } else { HUFF_ICC_DF };
                read_par(
                    r,
                    &mut self.icc_par,
                    table,
                    self.nr_icc_par,
                    e,
                    dt,
                    self.num_env_old,
                    None,
                    |v| !(0..=7).contains(&v),
                )?;
            }
        } else {
            self.icc_par = [[0; MAX_PAR]; MAX_ENV];
        }

        if self.enable_ext {
            let mut cnt = i64::from(r.read_bits(4)?);
            if cnt == 15 {
                cnt += i64::from(r.read_bits(8)?);
            }
            cnt *= 8;
            while cnt > 7 {
                let ext_id = r.read_bits(2)?;
                let used = self.read_extension(r, ext_id)?;
                let used = i64::try_from(used).map_err(|_| Error::corrupt("ps: extension size overflow"))?;
                cnt -= 2 + used;
            }
            if cnt < 0 {
                return Err(Error::corrupt("ps: extension overruns its payload"));
            }
            skip_clamped(r, cnt as u64);
        }

        // The spec allows the last envelope to end early; the decoder then
        // holds the last parameter set until the end of the frame (a fake
        // envelope at slot 31), as FFmpeg does.
        if self.num_env == 0 || self.border_position[self.num_env] < 31 {
            let source = if self.num_env > 0 {
                Some(self.num_env - 1)
            } else {
                self.num_env_old.checked_sub(1)
            };
            if let Some(src) = source.filter(|&s| s != self.num_env) {
                let n = self.num_env;
                if self.enable_iid {
                    self.iid_par[n] = self.iid_par[src];
                }
                if self.enable_icc {
                    self.icc_par[n] = self.icc_par[src];
                }
                if self.enable_ipdopd {
                    self.ipd_par[n] = self.ipd_par[src];
                    self.opd_par[n] = self.opd_par[src];
                }
            }
            if self.enable_iid {
                let limit = 7 + 8 * i32::from(self.iid_quant);
                for b in 0..self.nr_iid_par {
                    if i32::from(self.iid_par[self.num_env][b]).abs() > limit {
                        return Err(Error::corrupt("ps: fake envelope iid out of range"));
                    }
                }
            }
            if self.enable_icc {
                for b in 0..self.nr_iid_par {
                    if !(0..=7).contains(&i32::from(self.icc_par[self.num_env][b])) {
                        return Err(Error::corrupt("ps: fake envelope icc out of range"));
                    }
                }
            }
            self.num_env += 1;
            self.border_position[self.num_env] = 31;
        }

        self.is34bands_old = self.is34bands;
        if self.enable_iid || self.enable_icc {
            self.is34bands = (self.enable_iid && self.nr_iid_par == 34)
                || (self.enable_icc && self.nr_icc_par == 34);
        }
        if !self.enable_ipdopd {
            self.ipd_par = [[0; MAX_PAR]; MAX_ENV];
            self.opd_par = [[0; MAX_PAR]; MAX_ENV];
        }
        if header {
            self.start = true;
        }
        Ok(r.bit_position() - start_bit)
    }

    /// One `ps_extension()` element of id `ext_id`; only id 0 carries data
    /// (the IPD/OPD phase parameters). Other ids are skipped by the caller.
    fn read_extension(&mut self, r: &mut BitReader<'_>, ext_id: u32) -> Result<u64> {
        if ext_id != 0 {
            return Ok(0);
        }
        let start = r.bit_position();
        self.enable_ipdopd = r.read_bit()?;
        if self.enable_ipdopd {
            for e in 0..self.num_env {
                let dt = r.read_bit()?;
                let table = if dt { HUFF_IPD_DT } else { HUFF_IPD_DF };
                read_par(
                    r,
                    &mut self.ipd_par,
                    table,
                    self.nr_ipdopd_par,
                    e,
                    dt,
                    self.num_env_old,
                    Some(0x07),
                    |_| false,
                )?;
                let dt = r.read_bit()?;
                let table = if dt { HUFF_OPD_DT } else { HUFF_OPD_DF };
                read_par(
                    r,
                    &mut self.opd_par,
                    table,
                    self.nr_ipdopd_par,
                    e,
                    dt,
                    self.num_env_old,
                    Some(0x07),
                    |_| false,
                )?;
            }
        }
        r.skip_bits(1)?; // reserved_ps
        Ok(r.bit_position() - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ps_codebook_is_a_complete_prefix_code() {
        // Kraft sum exactly 1: the tables are complete, so every bit string
        // decodes to some codeword and the canonical assignment is consistent.
        for (i, table) in ps_huff::TABLES.iter().enumerate() {
            let kraft: f64 = table.iter().map(|&(_, len)| 0.5f64.powi(i32::from(len))).sum();
            assert!((kraft - 1.0).abs() < 1e-12, "table {i} Kraft sum {kraft}");
            // Codeword counts of FFmpeg's aacps_huff_tabs, in table order.
            const SIZES: [usize; 10] = [61, 61, 29, 29, 15, 15, 8, 8, 8, 8];
            assert_eq!(table.len(), SIZES[i], "table {i} size");
        }
    }

    #[test]
    fn codes_follow_table_order_as_ffmpeg_assigns_them() {
        // FFmpeg takes each entry's code as the next free slice of the code
        // space in table order. The first two IID fine entries are (value -2,
        // length 4) and (value +2, length 4), so they get codes 0000 and 0001,
        // not the canonical order a length sort would give.
        let v = build_codes(ps_huff::TABLES[HUFF_IID_DF1], ps_huff::OFFSETS[HUFF_IID_DF1]);
        assert_eq!(v.codes[0], (4, 0b0000, -2));
        assert_eq!(v.codes[1], (4, 0b0001, 2));
        assert_eq!(v.codes[2], (3, 0b001, -1));
    }

    #[test]
    fn codes_are_prefix_free_and_values_cover_the_range() {
        for (i, table) in ps_huff::TABLES.iter().enumerate() {
            let v = build_codes(table, ps_huff::OFFSETS[i]);
            for (a, &(la, ca, _)) in v.codes.iter().enumerate() {
                for &(lb, cb, _) in v.codes.iter().skip(a + 1) {
                    let (short, long, cs, cl) = if la <= lb { (la, lb, ca, cb) } else { (lb, la, cb, ca) };
                    assert!(cl >> (long - short) != cs, "table {i} codes collide");
                }
            }
        }
    }

    #[test]
    fn a_zero_delta_iid_stream_round_trips_through_read_data() {
        // Header on, IID on (mode 0: 10 bands, fine quant off), ICC off, no
        // extension, FIXFIX, one envelope, IID df0 with all-zero deltas.
        // df0 symbol for delta 0 is 14 (value 0 after the -14 offset) with a
        // one-bit code: a stream of 1s for each of 10 bands.
        let mut bits: Vec<bool> = Vec::new();
        let push = |bits: &mut Vec<bool>, v: u32, n: u32| {
            for i in (0..n).rev() {
                bits.push((v >> i) & 1 == 1);
            }
        };
        push(&mut bits, 1, 1); // header
        push(&mut bits, 1, 1); // enable_iid
        push(&mut bits, 0, 3); // iid_mode 0
        push(&mut bits, 0, 1); // enable_icc
        push(&mut bits, 0, 1); // enable_ext
        push(&mut bits, 0, 1); // frame_class FIXFIX
        push(&mut bits, 1, 2); // num_env index 1 -> 1 envelope
        push(&mut bits, 0, 1); // dt = 0 for the envelope
        let code_for_zero = {
            let v = build_codes(ps_huff::TABLES[HUFF_IID_DF0], ps_huff::OFFSETS[HUFF_IID_DF0]);
            *v.codes.iter().find(|c| c.2 == 0).expect("zero is a codeword")
        };
        for _ in 0..10 {
            push(&mut bits, code_for_zero.1, code_for_zero.0);
        }
        let mut bytes = vec![0u8; bits.len().div_ceil(8) + 1];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let mut r = BitReader::new(&bytes);
        let mut ps = PsCommon::default();
        let used = ps.read_data(&mut r, bits.len() as u64 + 16);
        assert_eq!(used, bits.len() as u64, "consumes exactly the coded bits");
        assert!(ps.start);
        assert_eq!(ps.num_env, 1);
        assert_eq!(ps.nr_iid_par, 10);
        assert!(ps.iid_par[0][..10].iter().all(|&v| v == 0));
    }

    #[test]
    fn malformed_ps_clears_the_stereo_state_and_skips_its_payload() {
        // iid_mode 6 is reserved: the frame's PS is dropped, bits skipped.
        let bytes = [0b1111_0110u8, 0, 0, 0];
        let mut r = BitReader::new(&bytes);
        let mut ps = PsCommon { start: true, ..PsCommon::default() };
        let used = ps.read_data(&mut r, 32);
        assert_eq!(used, 32);
        assert!(!ps.start);
    }
}
