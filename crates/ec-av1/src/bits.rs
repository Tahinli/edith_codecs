//! AV1 bit-primitive writers — bit-exact inverses of the readers in
//! [`ec_av1_syntax::obu`], built on the family [`ec_core::BitWriter`].
//!
//! Every function here is the inverse of a named AV1 bit-reading descriptor
//! (spec section 4.10 / 5.9); the parser function it mirrors is named in its
//! docs and roundtripped in the tests below.

use ec_av1_syntax::obu::floor_log2;
use ec_core::BitWriter;

/// `leb128()` writer (spec 4.10.5); inverts [`ec_av1_syntax::obu::read_leb128`].
///
/// Little-endian base-128: each byte carries 7 payload bits and a high
/// continuation bit, set on every byte but the last. A 32-bit value needs at
/// most 5 bytes (the parser accepts up to 8 and rejects anything that does not
/// fit in 32 bits).
pub fn write_leb128(w: &mut BitWriter, value: u32) {
    let mut v = value;
    loop {
        let byte = v & 0x7f;
        v >>= 7;
        let more = v != 0;
        w.write_bits(if more { byte | 0x80 } else { byte }, 8);
        if !more {
            return;
        }
    }
}

/// `uvlc()` writer (spec 4.10.3); inverts [`ec_av1_syntax::obu::read_uvlc`].
///
/// `0` is a single `1` bit; `u32::MAX` is the 32-leading-zero escape the
/// parser returns verbatim; otherwise `lz` zero bits, a `1` bit, then the
/// `lz`-bit suffix, where `lz = floor_log2(value + 1)` and the suffix is
/// `value - (1 << lz) + 1` (so the decoded value is `suffix + (1 << lz) - 1`).
pub fn write_uvlc(w: &mut BitWriter, value: u32) {
    if value == u32::MAX {
        // The spec escape: 32 leading zeros read back as u32::MAX, with no
        // terminating 1 bit.
        w.write_bits64(0, 32);
        return;
    }
    let lz = floor_log2(value + 1);
    w.write_bits64(0, lz);
    w.write_bit(true);
    if lz > 0 {
        let suffix = value - ((1u32 << lz) - 1);
        w.write_bits(suffix, lz);
    }
}

/// `le(n)` writer (spec 4.10.4); inverts [`ec_av1_syntax::obu::read_le`].
///
/// `n`-byte little-endian, least-significant byte first. AV1 reads `le(n)`
/// only at a byte boundary; the writer should be byte aligned when called.
pub fn write_le(w: &mut BitWriter, n: u32, value: u32) {
    for i in 0..n {
        w.write_bits((value >> (i * 8)) & 0xff, 8);
    }
}

/// `su(n)` writer (spec 4.10.6); inverts [`ec_av1_syntax::obu::read_su`].
///
/// `n`-bit two's complement, delegated to [`BitWriter::write_signed`].
pub fn write_su(w: &mut BitWriter, n: u32, value: i32) {
    w.write_signed(value, n);
}

/// `ns(n)` writer (spec 4.10.7); inverts [`ec_av1_syntax::obu::read_ns`].
///
/// Non-symmetric unsigned below `n`: `w = floor_log2(n) + 1` bits, split into a
/// `w - 1`-bit leading part and, for the high range, one extra bit. Values
/// `[0, m)` (with `m = (1 << w) - n`) carry themselves in `w - 1` bits; values
/// `[m, n)` carry `(value + m) >> 1` in `w - 1` bits plus the low bit of
/// `value + m`.
pub fn write_ns(w: &mut BitWriter, n: u32, value: u32) {
    assert!(value < n, "write_ns: value {value} >= n {n}");
    if n <= 1 {
        return;
    }
    let width = floor_log2(n) + 1;
    let m = (1u32 << width) - n;
    if value < m {
        w.write_bits(value, width - 1);
    } else {
        let combined = value + m;
        w.write_bits(combined >> 1, width - 1);
        w.write_bit(combined & 1 != 0);
    }
}

/// `delta_q()` writer (spec 5.9.13); inverts
/// `ec_av1_syntax::frame::read_delta_q`.
///
/// One `delta_coded` bit: `0` when the delta is zero, otherwise `1` followed by
/// `su(7)` holding the signed delta. (The frame module is private, so the test
/// reconstructs the read side from [`ec_av1_syntax::obu::read_su`] plus a bit.)
pub fn write_delta_q(w: &mut BitWriter, delta_q: i32) {
    if delta_q == 0 {
        w.write_bit(false);
    } else {
        w.write_bit(true);
        w.write_signed(delta_q, 7);
    }
}

/// `trailing_bits()` writer (spec 4.8.4).
///
/// A required `1` bit followed by zero bits up to the next byte boundary. AV1
/// ends every OBU that is not a whole number of bytes with this.
pub fn write_trailing_bits(w: &mut BitWriter) {
    w.write_bit(true);
    w.align_to_byte();
}

/// `byte_alignment()` writer (spec 4.8.5).
///
/// Zero bits up to the next byte boundary — [`BitWriter::align_to_byte`] is the
/// exact inverse of the parser's `while !byte_aligned: read_bit()` loop.
pub fn write_byte_alignment(w: &mut BitWriter) {
    w.align_to_byte();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_av1_syntax::obu::{read_le, read_leb128, read_ns, read_su, read_uvlc};
    use ec_core::{BitReader, BitWriter};

    /// Write `f` into a fresh writer, then read it back with `g`.
    fn roundtrip<T, F, G>(f: F, g: G) -> T
    where
        F: FnOnce(&mut BitWriter),
        G: FnOnce(&mut BitReader<'_>) -> ec_core::Result<T>,
    {
        let mut w = BitWriter::new();
        f(&mut w);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        g(&mut r).expect("parser readback failed")
    }

    #[test]
    fn uvlc_roundtrip() {
        for v in 0..4096u32 {
            assert_eq!(roundtrip(|w| write_uvlc(w, v), read_uvlc), v, "uvlc {v}");
        }
        assert_eq!(
            roundtrip(|w| write_uvlc(w, u32::MAX), read_uvlc),
            u32::MAX,
            "uvlc escape"
        );
    }
    #[test]
    fn leb128_roundtrip() {
        // One value at each LEB128 byte-length boundary (1 through 5 bytes),
        // plus the all-ones and zero corners.
        let spread = [
            0u32,
            1,
            127,
            128,
            255,
            256,
            16_383,
            16_384,
            65_535,
            65_536,
            2_097_151,
            2_097_152,
            134_217_727,
            134_217_728,
            268_435_455,
            268_435_456,
            u32::MAX,
        ];
        for v in spread {
            assert_eq!(
                roundtrip(|w| write_leb128(w, v), read_leb128),
                v,
                "leb128 {v}"
            );
        }
    }

    #[test]
    fn ns_roundtrip() {
        for n in 1u32..256 {
            if n <= 64 {
                for v in 0..n {
                    assert_eq!(
                        roundtrip(|w| write_ns(w, n, v), |r| read_ns(r, n)),
                        v,
                        "ns n={n} v={v}"
                    );
                }
            } else {
                for &v in &[0u32, 1, n / 2, n - 2, n - 1] {
                    assert_eq!(
                        roundtrip(|w| write_ns(w, n, v), |r| read_ns(r, n)),
                        v,
                        "ns n={n} v={v}"
                    );
                }
            }
        }
    }

    #[test]
    fn su_roundtrip() {
        for v in -64..64i32 {
            assert_eq!(
                roundtrip(|w| write_su(w, 8, v), |r| read_su(r, 8)),
                v,
                "su {v}"
            );
        }
    }

    #[test]
    fn le_roundtrip() {
        for n in 1u32..=4 {
            let max = u32::MAX >> (32 - 8 * n);
            for &v in &[0u32, 1, 0xAB, 0x1234, 0x123456, 0xDEADBEEF, max] {
                let v = v & max;
                assert_eq!(
                    roundtrip(|w| write_le(w, n, v), |r| read_le(r, n)),
                    v,
                    "le n={n} v={v}"
                );
            }
        }
    }

    #[test]
    fn delta_q_roundtrip() {
        // Mirrors ec_av1_syntax::frame::read_delta_q exactly: a `delta_coded`
        // bit, then su(7) when set. That function is private, so the read side
        // is rebuilt from BitReader::read_bit + obu::read_su.
        for v in -63..=63i32 {
            let got = roundtrip(
                |w| write_delta_q(w, v),
                |r| -> ec_core::Result<i32> {
                    if r.read_bit()? {
                        Ok(read_su(r, 7)?)
                    } else {
                        Ok(0)
                    }
                },
            );
            assert_eq!(got, v, "delta_q {v}");
        }
    }

    #[test]
    fn trailing_bits_and_byte_alignment_are_byte_aligned() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        assert!(!w.is_byte_aligned());
        write_trailing_bits(&mut w);
        assert!(w.is_byte_aligned());

        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        write_byte_alignment(&mut w);
        assert!(w.is_byte_aligned());
    }
}
