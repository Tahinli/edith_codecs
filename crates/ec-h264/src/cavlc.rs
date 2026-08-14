//! CAVLC residual decoding (spec 9.2).

use ec_core::error::{Error, Result};

use crate::bits::BitCursor;
use crate::tables::{
    COEFF_TOKEN_CHROMA_DC, COEFF_TOKEN_NC0, COEFF_TOKEN_NC2, COEFF_TOKEN_NC4, CoeffTokenEntry,
    RUN_BEFORE, TOTAL_ZEROS_4X4, TOTAL_ZEROS_CHROMA_DC, VlcEntry,
};

/// Decode one code from a `(len, code, value)` VLC list. The cursor's peek is
/// zero-padded past the end of data, so a truncated stream either matches a
/// code longer than what remains (caught by `skip`) or matches nothing.
#[inline]
fn read_vlc(r: &mut BitCursor<'_>, table: &[VlcEntry]) -> Result<u8> {
    let peek = r.peek16();
    for &(len, code, value) in table {
        if peek >> (16 - len) == u32::from(code) {
            r.skip(u32::from(len))?;
            return Ok(value);
        }
    }
    if r.bits_remaining() < 16 {
        return Err(Error::NeedMore);
    }
    Err(Error::corrupt("no matching VLC code"))
}

/// Decode `coeff_token` (spec 9.2.1) returning `(total_coeff, trailing_ones)`.
#[inline]
fn read_coeff_token(r: &mut BitCursor<'_>, nc: i32) -> Result<(u8, u8)> {
    if nc >= 8 {
        // 6-bit fixed-length code.
        let v = r.read_bits(6)?;
        return Ok(if v == 3 {
            (0, 0)
        } else {
            ((v / 4 + 1) as u8, (v % 4) as u8)
        });
    }
    let table: &[CoeffTokenEntry] = match nc {
        -1 => COEFF_TOKEN_CHROMA_DC,
        0..=1 => COEFF_TOKEN_NC0,
        2..=3 => COEFF_TOKEN_NC2,
        4..=7 => COEFF_TOKEN_NC4,
        _ => {
            return Err(Error::unsupported(
                "CAVLC nC = -2",
                "nC -2 is the 4:2:2 chroma DC coeff_token table (9.2.1); only \
                 4:2:0 chroma DC (nC -1) is decoded",
            ));
        }
    };
    let peek = r.peek16();
    for &(len, code, tc, t1) in table {
        if peek >> (16 - len) == u32::from(code) {
            r.skip(u32::from(len))?;
            return Ok((tc, t1));
        }
    }
    if r.bits_remaining() < 16 {
        return Err(Error::NeedMore);
    }
    Err(Error::corrupt("no matching coeff_token code"))
}

/// `level_prefix` (spec 9.2.2.1): leading zeros before the first 1 bit.
/// Capped: baseline/main constrain it to <= 15, other profiles report
/// <= 11 + bit depth; beyond 25 zeros the stream is broken.
#[inline]
fn read_level_prefix(r: &mut BitCursor<'_>) -> Result<u32> {
    r.read_prefix_zeros(25)
}

/// Decode one CAVLC residual block (spec 9.2) into `coeff[0..max_num_coeff]`
/// in scan order. Returns TotalCoeff. `nc` selects the coeff_token VLC:
/// the averaged neighbour count for luma/chroma-AC blocks, -1 for 4:2:0
/// chroma DC.
///
/// `coeff` is fully zeroed first; the caller hands the same stack array every
/// time — no allocation.
// The spec's level/run algorithms couple the loop index to state transitions
// (`i == trailing_ones`, positions counted down); ranges mirror clause 9.2.
#[allow(clippy::needless_range_loop)]
pub fn residual_block(
    r: &mut BitCursor<'_>,
    coeff: &mut [i32; 16],
    max_num_coeff: usize,
    nc: i32,
) -> Result<u8> {
    debug_assert!(max_num_coeff <= 16);
    *coeff = [0; 16];
    let (total_coeff, trailing_ones) = read_coeff_token(r, nc)?;
    let total_coeff = total_coeff as usize;
    let trailing_ones = trailing_ones as usize;
    if total_coeff == 0 {
        return Ok(0);
    }
    if total_coeff > max_num_coeff {
        return Err(Error::corrupt("TotalCoeff exceeds maxNumCoeff"));
    }

    // 9.2.2: levels, highest frequency first.
    let mut level = [0i32; 16];
    for l in level.iter_mut().take(trailing_ones) {
        *l = if r.read_bit()? { -1 } else { 1 };
    }
    let mut suffix_length: u32 = u32::from(total_coeff > 10 && trailing_ones < 3);
    for i in trailing_ones..total_coeff {
        let level_prefix = read_level_prefix(r)?;
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let mut level_code = (level_prefix.min(15) << suffix_length) as i64;
        if level_suffix_size > 0 {
            level_code += r.read_bits(level_suffix_size)? as i64;
        }
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1i64 << (level_prefix - 3)) - 4096;
        }
        if i == trailing_ones && trailing_ones < 3 {
            level_code += 2;
        }
        let v = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        level[i] =
            i32::try_from(v).map_err(|_| Error::corrupt("coefficient level out of range"))?;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level[i].unsigned_abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // 9.2.3: runs of zeros.
    let mut zeros_left: i32 = if total_coeff == max_num_coeff {
        0
    } else {
        let tz = if nc == -1 {
            read_vlc(r, TOTAL_ZEROS_CHROMA_DC[total_coeff - 1])?
        } else {
            read_vlc(r, TOTAL_ZEROS_4X4[total_coeff - 1])?
        };
        tz as i32
    };

    // 9.2.4 fused with 9.2.3: place levels from the highest scan position
    // downward, consuming run_before per coefficient.
    let mut coeff_num = zeros_left + total_coeff as i32 - 1;
    for i in 0..total_coeff {
        let run = if i == total_coeff - 1 {
            // Last (lowest-frequency) coefficient absorbs the rest.
            zeros_left
        } else if zeros_left > 0 {
            let idx = (zeros_left.min(7) - 1) as usize;
            read_vlc(r, RUN_BEFORE[idx])? as i32
        } else {
            0
        };
        if run > zeros_left {
            return Err(Error::corrupt("run_before exceeds zerosLeft"));
        }
        let pos = coeff_num as usize;
        if pos >= max_num_coeff {
            return Err(Error::corrupt("coefficient position out of block"));
        }
        coeff[pos] = level[i];
        zeros_left -= run;
        coeff_num -= run + 1;
    }
    Ok(total_coeff as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    fn cursor(data: &[u8]) -> BitCursor<'_> {
        BitCursor::new(data, 0)
    }

    fn reader_of(bits: &str) -> Vec<u8> {
        let mut w = BitWriter::new();
        for c in bits.chars().filter(|c| !c.is_whitespace()) {
            w.write_bit(c == '1');
        }
        // Pad so peeks have room.
        w.align_to_byte();
        w.write_bytes(&[0xFF; 4]);
        w.into_bytes()
    }

    #[test]
    fn zero_coefficients_single_bit() {
        // nC 0: coeff_token (0,0) is "1".
        let data = reader_of("1");
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert_eq!(residual_block(&mut r, &mut c, 16, 0).unwrap(), 0);
        assert_eq!(r.bit_position(), 1);
        assert_eq!(c, [0; 16]);
    }

    /// Worked example: TotalCoeff 3, TrailingOnes 2, one explicit level.
    /// nC=0 coeff_token(2,3) = "0000101"; signs + and -; level_prefix "1"
    /// (level_code 0+2=2 -> level 2); total_zeros (tz index 3) = 1 -> "111";
    /// wait: use total_zeros 0 -> "0101" per tzVlcIndex 3.
    #[test]
    fn trailing_ones_and_one_level() {
        // trailing signs: 0 (+1), 1 (-1); level_prefix "1" -> levelCode 2 -> +2
        // total_zeros = 0 -> "0101" (tz3); no run_before reads (zerosLeft 0).
        let data = reader_of("0000101 0 1 1 0101");
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert_eq!(residual_block(&mut r, &mut c, 16, 0).unwrap(), 3);
        // Levels ordered high frequency -> low: +1, -1, +2 at scan 2,1,0.
        assert_eq!(&c[..4], &[2, -1, 1, 0]);
    }

    /// With total_zeros > 0 the levels spread out by run_before codes.
    #[test]
    fn runs_spread_levels() {
        // nC=0, coeff_token (1,1): "01"; sign 0 -> +1.
        // total_zeros for tzVlcIndex 1: value 2 = "010".
        // Single coefficient: no run_before read, position = 2.
        let data = reader_of("01 0 010");
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert_eq!(residual_block(&mut r, &mut c, 16, 0).unwrap(), 1);
        assert_eq!(c[2], 1);
        assert_eq!(c.iter().filter(|&&x| x != 0).count(), 1);
    }

    #[test]
    fn chroma_dc_block() {
        // nC=-1, coeff_token (1,1) = "1"; sign "1" -> -1; TotalCoeff 1 < 4 so
        // total_zeros chroma (tz index 1): 3 = "000". Position = 3.
        let data = reader_of("1 1 000");
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert_eq!(residual_block(&mut r, &mut c, 4, -1).unwrap(), 1);
        assert_eq!(c[3], -1);
    }

    #[test]
    fn full_block_skips_total_zeros() {
        // nC >= 8 uses the 6-bit FLC. (16, 3) = 63 = "111111".
        // 3 trailing signs then 13 levels with suffix_length starting at 1.
        let mut bits = String::from("111111 000");
        for _ in 0..13 {
            bits.push_str("10"); // level_prefix 0, suffix "0" -> levelCode 0
        }
        // First non-trailing level: t1 == 3 so no +2 adjustment; levelCode 0
        // -> +1 each.
        let data = reader_of(&bits);
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert_eq!(residual_block(&mut r, &mut c, 16, 9).unwrap(), 16);
        assert_eq!(c.iter().filter(|&&x| x != 0).count(), 16);
    }

    #[test]
    fn truncated_is_need_more() {
        let data = [0u8; 1]; // 8 zero bits: valid prefix of a long code
        let mut r = cursor(&data);
        let mut c = [0i32; 16];
        assert!(matches!(
            residual_block(&mut r, &mut c, 16, 0),
            Err(Error::NeedMore)
        ));
    }
}
