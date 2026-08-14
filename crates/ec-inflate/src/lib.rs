//! DEFLATE (RFC 1951) and zlib (RFC 1950) decompression.
//!
//! Two entry points, both one-shot over a complete buffer: [`inflate`] for a
//! raw DEFLATE stream (what PNG's `IDAT` chain and Matroska header stripping
//! hand over) and [`inflate_zlib`] for the same thing inside a zlib wrapper,
//! header checked and Adler-32 verified.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - Every call carries a `limit`, the largest output it may produce. A stream
//!   that would exceed it stops with [`Error::LimitExceeded`]; a compressed
//!   megabyte of zeros is not allowed to become a gigabyte of memory. Pass
//!   `usize::MAX` to mean "no ceiling", and mean it.
//! - Malformed input is an error, never a panic. Truncation is
//!   [`Error::Truncated`], anything else the format forbids is
//!   [`Error::Corrupt`] naming what was wrong.
//! - Decoding is table-driven through a two-level lookup: no Huffman code is
//!   walked bit by bit, and the fixed trees are built once per process.
//!
//! Decoder state lives in one struct threaded through the block loop, so
//! resuming across buffer boundaries stays a change to the entry points rather
//! than a rewrite; today the public API is one-shot because both callers are.
//!
//! No async, no unsafe, no external dependencies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bits;
mod huffman;
mod inflate;

use std::fmt;

use bits::Bits;

/// Everything a compressed stream can get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The stream ended in the middle of a code, a block or the trailer.
    Truncated,
    /// The stream violates RFC 1950/1951.
    Corrupt {
        /// What was wrong, e.g. "over-subscribed Huffman code".
        context: String,
    },
    /// Decompressing further would pass the caller's ceiling.
    LimitExceeded {
        /// The ceiling that was hit, in bytes.
        limit: usize,
    },
    /// The zlib trailer disagrees with the data that was decompressed.
    Adler32Mismatch {
        /// Checksum stored in the trailer.
        expected: u32,
        /// Checksum of the output actually produced.
        actual: u32,
    },
    /// A zlib feature this crate does not implement.
    Unsupported {
        /// The construct that was refused, e.g. "zlib preset dictionary".
        what: String,
        /// Why it is refused.
        why: String,
    },
}

impl Error {
    fn corrupt(context: impl Into<String>) -> Error {
        Error::Corrupt {
            context: context.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "truncated deflate stream"),
            Error::Corrupt { context } => write!(f, "corrupt deflate stream: {context}"),
            Error::LimitExceeded { limit } => {
                write!(f, "decompressed output exceeded the {limit} byte limit")
            }
            Error::Adler32Mismatch { expected, actual } => write!(
                f,
                "zlib adler32 mismatch: stream says {expected:#010x}, output is {actual:#010x}"
            ),
            Error::Unsupported { what, why } => write!(f, "unsupported {what}: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for ec_core::Error {
    fn from(error: Error) -> ec_core::Error {
        match error {
            Error::Truncated => ec_core::Error::NeedMore,
            Error::Unsupported { what, why } => ec_core::Error::Unsupported { what, why },
            other => ec_core::Error::corrupt(other.to_string()),
        }
    }
}

/// Which wrapper, if any, surrounds the DEFLATE data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Bare RFC 1951 blocks.
    Raw,
    /// RFC 1950: two header bytes, the blocks, an Adler-32 trailer.
    Zlib,
}

/// Decompress a raw DEFLATE stream, producing at most `limit` bytes.
pub fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    inflate_into(input, &mut out, limit, Format::Raw)?;
    Ok(out)
}

/// Decompress a zlib stream, producing at most `limit` bytes.
///
/// The two header bytes are checked and the Adler-32 trailer is verified
/// against the output.
pub fn inflate_zlib(input: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    inflate_into(input, &mut out, limit, Format::Zlib)?;
    Ok(out)
}

/// Decompress onto the end of an existing buffer.
///
/// `limit` bounds the total length of `out`, not the bytes added. On failure
/// `out` keeps whatever was decompressed before the error, which is what a
/// caller reporting partial progress wants.
pub fn inflate_into(
    input: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
    format: Format,
) -> Result<(), Error> {
    let start = out.len();
    let body = match format {
        Format::Raw => input,
        Format::Zlib => zlib_header(input)?,
    };
    let mut bits = Bits::new(body);
    let end = inflate::blocks(&mut bits, out, limit)?;
    if format == Format::Zlib {
        let trailer: [u8; 4] = body
            .get(end..)
            .and_then(|rest| rest.get(..4))
            .ok_or(Error::Truncated)?
            .try_into()
            .expect("a four byte slice");
        let expected = u32::from_be_bytes(trailer);
        let actual = adler32(&out[start..]);
        if expected != actual {
            return Err(Error::Adler32Mismatch { expected, actual });
        }
    }
    Ok(())
}

/// Validate the RFC 1950 header and hand back the DEFLATE data behind it.
fn zlib_header(input: &[u8]) -> Result<&[u8], Error> {
    let (&cmf, &flg) = match (input.first(), input.get(1)) {
        (Some(cmf), Some(flg)) => (cmf, flg),
        _ => return Err(Error::Truncated),
    };
    if cmf & 0x0f != 8 {
        return Err(Error::corrupt("zlib compression method is not deflate"));
    }
    if cmf >> 4 > 7 {
        return Err(Error::corrupt("zlib window size above 32 KiB"));
    }
    if !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
        return Err(Error::corrupt("zlib header check bits do not divide by 31"));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Unsupported {
            what: "zlib preset dictionary".into(),
            why: "no caller in this family uses FDICT".into(),
        });
    }
    Ok(&input[2..])
}

/// RFC 1950 §9. Chunked so the sums cannot overflow between reductions.
fn adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler32_matches_the_rfc_example() {
        // zlib's own documented value for "abc" and the empty string.
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"abc"), 0x024d_0127);
        // Long input exercises the chunked reduction.
        assert_eq!(adler32(&vec![0xa5; 100_000]), 0x5afa_d3d6);
    }

    #[test]
    fn zlib_header_is_checked() {
        assert!(matches!(zlib_header(&[]), Err(Error::Truncated)));
        assert!(matches!(zlib_header(&[0x78]), Err(Error::Truncated)));
        assert!(matches!(
            zlib_header(&[0x79, 0x9c]),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            zlib_header(&[0x88, 0x9c]),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            zlib_header(&[0x78, 0x9d]),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            zlib_header(&[0x78, 0xbb]),
            Err(Error::Unsupported { .. })
        ));
        assert_eq!(zlib_header(&[0x78, 0x9c, 1, 2]).unwrap(), &[1, 2]);
    }
}
