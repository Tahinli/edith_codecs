//! The RFC 1951 block loop.

use std::sync::OnceLock;

use crate::Error;
use crate::bits::Bits;
use crate::huffman::Table;

/// First-level table width for the literal/length and code-length alphabets.
const LIT_ROOT: u32 = 9;
/// Distances have thirty symbols; a narrower root keeps the table cache-warm.
const DIST_ROOT: u32 = 6;

/// Match length per literal/length symbol 257..=285, plus its extra-bit count.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Match distance per distance symbol 0..=29, plus its extra-bit count.
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order dynamic blocks list their code-length code lengths in.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// The fixed literal/length and distance trees of RFC 1951 §3.2.6, built once.
fn fixed_tables() -> &'static (Table, Table) {
    static FIXED: OnceLock<(Table, Table)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let mut lit = [0u16; 288];
        for (sym, l) in lit.iter_mut().enumerate() {
            *l = match sym {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        // All thirty-two distance slots are five bits wide; 30 and 31 decode to
        // symbols that no valid stream uses, and are refused below.
        let dist = [5u16; 32];
        (
            Table::new(&lit, LIT_ROOT).expect("fixed literal tree is well formed"),
            Table::new(&dist, DIST_ROOT).expect("fixed distance tree is well formed"),
        )
    })
}

/// Decode every block of a raw DEFLATE stream into `out`, stopping at the final
/// one; returns the byte offset just past the stream.
pub(crate) fn blocks(bits: &mut Bits, out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => stored(bits, out, limit)?,
            1 => {
                let (lit, dist) = fixed_tables();
                compressed(bits, out, limit, lit, dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(bits)?;
                compressed(bits, out, limit, &lit, &dist)?;
            }
            _ => return Err(Error::corrupt("reserved DEFLATE block type 3")),
        }
        if last {
            return Ok(bits.align());
        }
    }
}

/// §3.2.4 stored block: byte-aligned, length and its complement, then raw bytes.
fn stored(bits: &mut Bits, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    let pos = bits.align();
    let src = bits.src();
    let header = src.get(pos..pos + 4).ok_or(Error::Truncated)?;
    let len = u16::from_le_bytes([header[0], header[1]]);
    let nlen = u16::from_le_bytes([header[2], header[3]]);
    if len != !nlen {
        return Err(Error::corrupt(
            "stored block length does not match its complement",
        ));
    }
    let len = len as usize;
    let body = src.get(pos + 4..pos + 4 + len).ok_or(Error::Truncated)?;
    reserve(out, len, limit)?;
    out.extend_from_slice(body);
    bits.seek(pos + 4 + len);
    Ok(())
}

/// §3.2.7 dynamic block header: the code-length code, then the two trees it codes.
fn dynamic_tables(bits: &mut Bits) -> Result<(Table, Table), Error> {
    let hlit = bits.take(5)? as usize + 257;
    let hdist = bits.take(5)? as usize + 1;
    let hclen = bits.take(4)? as usize + 4;

    let mut cl_lengths = [0u16; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(hclen) {
        cl_lengths[slot] = bits.take(3)? as u16;
    }
    let cl_table = Table::new(&cl_lengths, LIT_ROOT)?;

    let mut lengths = vec![0u16; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let (value, run) = match cl_table.decode(bits)? {
            sym @ 0..=15 => (sym, 1),
            16 => {
                let prev = *lengths
                    .get(i.wrapping_sub(1))
                    .ok_or_else(|| Error::corrupt("code-length repeat with nothing to repeat"))?;
                (prev, 3 + bits.take(2)? as usize)
            }
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return Err(Error::corrupt("code-length symbol above 18")),
        };
        if i + run > lengths.len() {
            return Err(Error::corrupt("code-length run past the end of the tables"));
        }
        lengths[i..i + run].fill(value);
        i += run;
    }

    let lit = Table::new(&lengths[..hlit], LIT_ROOT)?;
    let dist = Table::new(&lengths[hlit..], DIST_ROOT)?;
    Ok((lit, dist))
}

/// The literal/match loop shared by fixed and dynamic blocks.
fn compressed(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Table,
    dist: &Table,
) -> Result<(), Error> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => {
                if out.len() == limit {
                    return Err(Error::LimitExceeded { limit });
                }
                out.push(sym as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let index = sym as usize - 257;
                let length = LEN_BASE[index] as usize + bits.take(LEN_EXTRA[index])? as usize;
                let dsym = dist.decode(bits)? as usize;
                if dsym >= DIST_BASE.len() {
                    return Err(Error::corrupt("distance symbol 30 or 31"));
                }
                let distance = DIST_BASE[dsym] as usize + bits.take(DIST_EXTRA[dsym])? as usize;
                if distance > out.len() {
                    return Err(Error::corrupt("match distance reaches before the output"));
                }
                reserve(out, length, limit)?;
                copy_match(out, distance, length);
            }
            _ => return Err(Error::corrupt("literal/length symbol 286 or 287")),
        }
    }
}

/// LZ77 back-reference.
///
/// An overlapping match — `distance` one is how DEFLATE spells a run of the
/// same byte — is defined byte by byte, but it does not have to be *copied*
/// byte by byte: each pass copies everything already settled, so the window
/// doubles and a 258-byte run costs eight `memcpy`s rather than 258 pushes.
#[inline]
fn copy_match(out: &mut Vec<u8>, distance: usize, length: usize) {
    let end = out.len();
    out.resize(end + length, 0);
    let (head, tail) = out.split_at_mut(end);
    let pattern = &head[end - distance..];
    if distance >= length {
        tail.copy_from_slice(&pattern[..length]);
        return;
    }
    tail[..distance].copy_from_slice(pattern);
    let mut filled = distance;
    while filled < length {
        let take = filled.min(length - filled);
        let (done, rest) = tail.split_at_mut(filled);
        rest[..take].copy_from_slice(&done[..take]);
        filled += take;
    }
}

/// The ceiling, checked before every write: a crafted stream must fail, not
/// take all the memory there is.
#[inline]
fn reserve(out: &mut Vec<u8>, extra: usize, limit: usize) -> Result<(), Error> {
    if out.len() + extra > limit {
        return Err(Error::LimitExceeded { limit });
    }
    out.reserve(extra);
    Ok(())
}
