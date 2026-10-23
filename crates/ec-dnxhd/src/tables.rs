//! Compression-id table and the variable-length code lookups built from it.

pub(crate) const FLAG_INTERLACED: u16 = 1;
pub(crate) const FLAG_MBAFF: u16 = 2;
#[allow(dead_code)]
pub(crate) const FLAG_444: u16 = 4;

/// One compression id (CID): frame geometry and the code tables it uses.
/// `width`, `height` and `coding_unit_size` are 0 for the DNxHR ids (variable);
/// `bit_depth` is 0 when the frame header states it.
pub(crate) struct CidEntry {
    pub cid: u32,
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
    pub coding_unit_size: u32,
    pub flags: u16,
    pub bit_depth: u32,
    pub eob_index: u32,
    pub luma_weight: &'static [u8],
    pub chroma_weight: &'static [u8],
    pub dc_codes: &'static [u8],
    pub dc_bits: &'static [u8],
    pub ac_codes: &'static [u16],
    pub ac_bits: &'static [u8],
    pub ac_info: &'static [u8],
    pub run_codes: &'static [u16],
    pub run_bits: &'static [u8],
    pub run: &'static [u8],
}

include!("tables_gen.rs");

pub(crate) fn cid_entry(cid: u32) -> Option<&'static CidEntry> {
    CID_TABLE.iter().find(|e| e.cid == cid)
}

/// Zigzag scan: coefficient index in scan order -> position in the 8x8 block.
pub(crate) const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Two-level prefix-code lookup. A first-level entry is `symbol << 8 | length`
/// (length 0: no such code), or `offset << 8 | 0x80 | sub_bits` pointing at a
/// second-level table for codes longer than the first-level width.
pub(crate) struct Vlc {
    pub bits: u32,
    pub table: Vec<u32>,
}

const PRIMARY_BITS: u32 = 9;

impl Vlc {
    /// `lens[i]` is the length of code `i` (0: absent), `codes(i)` its bits,
    /// `symbols(i)` the value it stands for.
    fn build(lens: &[u8], codes: impl Fn(usize) -> u32, symbols: impl Fn(usize) -> u32) -> Vlc {
        let maxl = lens.iter().copied().max().unwrap_or(1).max(1) as u32;
        let bits = maxl.min(PRIMARY_BITS);
        let mut table = vec![0u32; 1 << bits];
        // Second-level width per first-level prefix.
        let mut sub = vec![0u32; 1 << bits];
        for (i, &l) in lens.iter().enumerate() {
            let l = l as u32;
            if l > bits {
                let pre = (codes(i) >> (l - bits)) as usize;
                sub[pre] = sub[pre].max(l - bits);
            }
        }
        for (pre, &sb) in sub.iter().enumerate() {
            if sb > 0 {
                table[pre] = (table.len() as u32) << 8 | 0x80 | sb;
                let n = table.len();
                table.resize(n + (1 << sb), 0);
            }
        }
        for (i, &l) in lens.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as u32;
            if l <= bits {
                let start = (codes(i) << (bits - l)) as usize;
                for e in &mut table[start..start + (1usize << (bits - l))] {
                    *e = symbols(i) << 8 | l;
                }
            } else {
                let pre = (codes(i) >> (l - bits)) as usize;
                let sb = sub[pre];
                let off = (table[pre] >> 8) as usize;
                let low = codes(i) & ((1 << (l - bits)) - 1);
                let start = off + ((low << (sb - (l - bits))) as usize);
                for e in &mut table[start..start + (1usize << (sb - (l - bits)))] {
                    *e = symbols(i) << 8 | l;
                }
            }
        }
        Vlc { bits, table }
    }

    /// Decode one symbol; `None` for a bit pattern that is no code.
    #[inline(always)]
    pub fn get(&self, r: &mut crate::bits::BitReader) -> Option<u32> {
        let w = r.peek32();
        let mut e = self.table[(w >> (32 - self.bits)) as usize];
        if e & 0x80 != 0 {
            let sb = e & 0x7f;
            let idx = (e >> 8) as usize + ((w << self.bits) >> (32 - sb)) as usize;
            e = self.table[idx];
        }
        let l = e & 0xff;
        if l == 0 {
            return None;
        }
        r.skip(l);
        Some(e >> 8)
    }
}

/// Everything the macroblock decoder needs for one CID at one bit depth.
pub(crate) struct CodeSet {
    pub cid: &'static CidEntry,
    pub bit_depth: u32,
    pub ac: Vlc,
    pub dc: Vlc,
    pub run: Vlc,
}

impl CodeSet {
    pub fn new(cid: &'static CidEntry, bit_depth: u32) -> CodeSet {
        let ndc = if bit_depth > 8 { 14 } else { 12 };
        CodeSet {
            cid,
            bit_depth,
            ac: Vlc::build(cid.ac_bits, |i| cid.ac_codes[i] as u32, |i| i as u32),
            dc: Vlc::build(&cid.dc_bits[..ndc], |i| cid.dc_codes[i] as u32, |i| i as u32),
            run: Vlc::build(cid.run_bits, |i| cid.run_codes[i] as u32, |i| cid.run[i] as u32),
        }
    }
}
