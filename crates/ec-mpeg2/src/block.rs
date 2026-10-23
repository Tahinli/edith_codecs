//! Coefficient decoding and dequantisation (7.2, 7.4).

use crate::bits::Bits;
use crate::tables::{COEF_EOB, COEF_ESC, Tables};

/// Parameters shared by all blocks of a macroblock.
pub(crate) struct BlockParams<'a> {
    pub(crate) mpeg1: bool,
    pub(crate) qscale: i32,
    /// Quantiser matrix for this block class, raster order.
    pub(crate) matrix: &'a [u8; 64],
    pub(crate) scan: &'a [u8; 64],
    /// Use table B.15 (intra_vlc_format) — intra blocks only.
    pub(crate) alt_vlc: bool,
}

#[inline(always)]
fn sat(v: i32) -> i32 {
    v.clamp(-2048, 2047)
}

/// Read escape run/level. Returns (run, signed level) or `None` when invalid.
#[inline(always)]
fn escape(b: &mut Bits, mpeg1: bool) -> Option<(usize, i32)> {
    let run = b.read(6) as usize;
    if mpeg1 {
        let l = b.read(8) as i32;
        let level = if l == 0 {
            b.read(8) as i32
        } else if l == 128 {
            b.read(8) as i32 - 256
        } else if l > 128 {
            l - 256
        } else {
            l
        };
        Some((run, level))
    } else {
        let mut level = b.read(12) as i32;
        if level & 0x800 != 0 {
            level -= 4096;
        }
        if level == 0 || level == -2048 {
            return None;
        }
        Some((run, level))
    }
}

/// Decode an intra block. `dc` is the reconstructed DC coefficient (already
/// scaled). Returns the scan index of the last non-zero coefficient.
pub(crate) fn decode_intra(
    b: &mut Bits,
    t: &Tables,
    p: &BlockParams,
    blk: &mut [i16; 64],
    dc: i32,
) -> Option<usize> {
    let tbl = if p.alt_vlc { &t.coef1 } else { &t.coef0 };
    let dc = sat_dc(dc);
    blk[0] = dc as i16;
    let mut mismatch = dc ^ 1;
    let mut i = 1usize;
    let mut last = 0usize;
    loop {
        let e = tbl.table[b.peek(16) as usize];
        let len = e >> 16;
        if len == 0 {
            return None;
        }
        b.skip(len);
        let v = e & 0xFFFF;
        let (run, level) = if v == COEF_EOB {
            break;
        } else if v == COEF_ESC {
            escape(b, p.mpeg1)?
        } else {
            let lvl = (v & 0xFF) as i32;
            let neg = b.read1();
            ((v >> 8) as usize, if neg { -lvl } else { lvl })
        };
        i += run;
        if i > 63 {
            return None;
        }
        let j = p.scan[i] as usize;
        let mag = level.abs();
        let mut val = if p.mpeg1 {
            let v = (mag * p.qscale * p.matrix[j] as i32) >> 3;
            (v - 1) | 1
        } else {
            (mag * p.qscale * p.matrix[j] as i32) >> 4
        };
        if level < 0 {
            val = -val;
        }
        val = sat(val);
        blk[j] = val as i16;
        mismatch ^= val;
        last = i;
        i += 1;
        if i > 64 {
            return None;
        }
    }
    if !p.mpeg1
        && mismatch & 1 != 0 {
            blk[63] = (blk[63] as i32 ^ 1) as i16;
            last = last.max(63);
        }
    Some(last)
}

#[inline(always)]
fn sat_dc(dc: i32) -> i32 {
    dc.clamp(-2048, 2047)
}

/// Decode a non-intra block; returns the scan index of the last coefficient.
pub(crate) fn decode_inter(
    b: &mut Bits,
    t: &Tables,
    p: &BlockParams,
    blk: &mut [i16; 64],
) -> Option<usize> {
    let tbl = &t.coef0;
    let mut mismatch = 1i32;
    let mut i = 0usize;
    let mut last = 0usize;
    let mut first = true;
    loop {
        let (run, level);
        if first && b.peek(1) == 1 {
            b.skip(1);
            let neg = b.read1();
            run = 0;
            level = if neg { -1 } else { 1 };
        } else {
            let e = tbl.table[b.peek(16) as usize];
            let len = e >> 16;
            if len == 0 {
                return None;
            }
            b.skip(len);
            let v = e & 0xFFFF;
            if v == COEF_EOB {
                break;
            } else if v == COEF_ESC {
                let (r, l) = escape(b, p.mpeg1)?;
                run = r;
                level = l;
            } else {
                let lvl = (v & 0xFF) as i32;
                let neg = b.read1();
                run = (v >> 8) as usize;
                level = if neg { -lvl } else { lvl };
            }
        }
        first = false;
        i += run;
        if i > 63 {
            return None;
        }
        let j = p.scan[i] as usize;
        let mag = level.abs();
        let mut val = if p.mpeg1 {
            let v = ((mag * 2 + 1) * p.qscale * p.matrix[j] as i32) >> 4;
            (v - 1) | 1
        } else {
            ((mag * 2 + 1) * p.qscale * p.matrix[j] as i32) >> 5
        };
        if level < 0 {
            val = -val;
        }
        val = sat(val);
        blk[j] = val as i16;
        mismatch ^= val;
        last = i;
        i += 1;
        if i > 64 {
            return None;
        }
    }
    if !p.mpeg1 && mismatch & 1 != 0 {
        blk[63] = (blk[63] as i32 ^ 1) as i16;
        last = last.max(63);
    }
    Some(last)
}
