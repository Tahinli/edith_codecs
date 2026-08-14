//! CAVLC parsing (Rec. ITU-T H.264 clause 9.2): the residual block layer, and
//! the variable length code lookups the rest of the syntax needs.
//!
//! Lookups are a linear scan over the transcribed table. That is the slowest
//! possible implementation and the only one that can be read against the
//! specification's own tables line by line; the scan is safe precisely because
//! every table is a prefix code, which `tables::tests` asserts.

// Clippy's needless_range_loop asks for iterators where this file
// transcribes the specification's own `for i` / `for j` formulas; the
// index is the point.
#![allow(clippy::needless_range_loop)]

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

use crate::tables::{
    COEFF_TOKEN_CHROMA_DC, COEFF_TOKEN_NC0, COEFF_TOKEN_NC2, COEFF_TOKEN_NC4, RUN_BEFORE,
    TOTAL_ZEROS_4X4, TOTAL_ZEROS_CHROMA_DC, Vlc,
};

/// The longest code in any CAVLC table (`coeff_token`, Table 9-5).
const MAX_CODE_BITS: u32 = 16;

/// Peek up to [`MAX_CODE_BITS`] bits, zero padded past the end of the buffer.
///
/// Zero padding cannot decode into a wrong symbol: the match is only accepted
/// when the buffer really holds the code's bits (see [`read_vlc`]).
fn peek_padded(r: &mut BitReader<'_>) -> Result<(u32, u32)> {
    let available = r.bits_remaining().min(MAX_CODE_BITS as u64) as u32;
    let value = r.peek_bits(available)?;
    Ok((value << (MAX_CODE_BITS - available), available))
}

/// Read one variable length code and return its `(a, b)` symbol columns.
pub fn read_vlc(r: &mut BitReader<'_>, table: &[Vlc], name: &str) -> Result<(u8, u8)> {
    let (window, available) = peek_padded(r)?;
    for &(a, b, len, code) in table {
        if window >> (MAX_CODE_BITS - len as u32) == code as u32 {
            if available < len as u32 {
                return Err(Error::NeedMore);
            }
            r.skip_bits(len as u64)?;
            return Ok((a, b));
        }
    }
    if available < MAX_CODE_BITS {
        // The buffer ran out before any code could be completed; more data
        // would decide it, so this is the streaming contract, not corruption.
        return Err(Error::NeedMore);
    }
    Err(Error::corrupt(format!(
        "H.264 CAVLC: no {name} code matches {window:016b}"
    )))
}

/// `coeff_token` (clause 9.2.1), returning `(TrailingOnes, TotalCoeff)`.
///
/// `nc` is the predictor `nC` of clause 9.2.1: a count derived from the
/// neighbouring blocks for luma and chroma AC, and the constant -1 for the
/// 4:2:0 chroma DC block.
pub fn coeff_token(r: &mut BitReader<'_>, nc: i32) -> Result<(u8, u8)> {
    match nc {
        -1 => read_vlc(r, COEFF_TOKEN_CHROMA_DC, "coeff_token (chroma DC)"),
        -2 => Err(Error::unsupported(
            "H.264 4:2:2 chroma DC coeff_token",
            "only ChromaArrayType 1 (4:2:0) is decoded",
        )),
        n if n < 0 => Err(Error::corrupt(format!("H.264 CAVLC: nC = {n}"))),
        0 | 1 => read_vlc(r, COEFF_TOKEN_NC0, "coeff_token (0 <= nC < 2)"),
        2 | 3 => read_vlc(r, COEFF_TOKEN_NC2, "coeff_token (2 <= nC < 4)"),
        4..=7 => read_vlc(r, COEFF_TOKEN_NC4, "coeff_token (4 <= nC < 8)"),
        _ => {
            // 8 <= nC: a six bit fixed length code. TotalCoeff and TrailingOnes
            // pack as 4 * (TotalCoeff - 1) + TrailingOnes, with 000011 standing
            // in for the otherwise unrepresentable TotalCoeff == 0.
            let code = r.read_bits(6)?;
            if code == 3 {
                return Ok((0, 0));
            }
            let total_coeff = (code / 4 + 1) as u8;
            let trailing_ones = (code % 4) as u8;
            if trailing_ones > total_coeff {
                return Err(Error::corrupt(format!(
                    "H.264 CAVLC: coeff_token FLC {code:06b} has more trailing ones than coefficients"
                )));
            }
            Ok((trailing_ones, total_coeff))
        }
    }
}

/// `level_prefix` (clause 9.2.2.1): the number of zero bits before the next 1.
fn level_prefix(r: &mut BitReader<'_>) -> Result<u32> {
    let mut zeros = 0u32;
    while !r.read_bit()? {
        zeros += 1;
        // 15 is the largest prefix any profile in this decoder's scope emits;
        // the cap keeps a corrupt stream from scanning to the end of the slice.
        if zeros > 31 {
            return Err(Error::corrupt("H.264 CAVLC: level_prefix beyond 31 zeros"));
        }
    }
    Ok(zeros)
}

/// `residual_block_cavlc(coeffLevel, startIdx, endIdx, maxNumCoeff)`
/// (clause 9.2), writing levels into `coeff_level` in scan order.
///
/// Returns `TotalCoeff`, which the caller stores for the `nC` prediction of the
/// blocks that follow.
pub fn residual_block_cavlc(
    r: &mut BitReader<'_>,
    coeff_level: &mut [i32],
    start_idx: usize,
    end_idx: usize,
    max_num_coeff: usize,
    nc: i32,
) -> Result<u8> {
    debug_assert!(coeff_level.len() >= max_num_coeff);
    let (trailing_ones, total_coeff) = coeff_token(r, nc)?;
    let (trailing_ones, total_coeff) = (trailing_ones as usize, total_coeff as usize);
    if total_coeff > max_num_coeff {
        return Err(Error::corrupt(format!(
            "H.264 CAVLC: TotalCoeff {total_coeff} exceeds maxNumCoeff {max_num_coeff}"
        )));
    }
    if total_coeff == 0 {
        return Ok(0);
    }

    // 9.2.2: levels, decoded from the highest frequency downwards.
    let mut level_val = [0i32; 16];
    let mut suffix_length = if total_coeff > 10 && trailing_ones < 3 {
        1
    } else {
        0
    };
    for i in 0..total_coeff {
        if i < trailing_ones {
            let trailing_ones_sign_flag = r.read_bit()?;
            level_val[i] = if trailing_ones_sign_flag { -1 } else { 1 };
            continue;
        }
        let prefix = level_prefix(r)?;
        let level_suffix_size = if prefix == 14 && suffix_length == 0 {
            4
        } else if prefix >= 15 {
            prefix - 3
        } else {
            suffix_length
        };
        let mut level_code = (prefix.min(15) << suffix_length) as i64;
        if level_suffix_size > 0 {
            level_code += r.read_bits(level_suffix_size)? as i64;
        }
        if prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if prefix >= 16 {
            level_code += (1i64 << (prefix - 3)) - 4096;
        }
        if i == trailing_ones && trailing_ones < 3 {
            level_code += 2;
        }
        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        level_val[i] = i32::try_from(level)
            .map_err(|_| Error::corrupt(format!("H.264 CAVLC: level {level} out of range")))?;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level_val[i].unsigned_abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // 9.2.3: the run of zeros before each level.
    let mut zeros_left = if total_coeff < end_idx - start_idx + 1 {
        let table = if max_num_coeff == 4 {
            TOTAL_ZEROS_CHROMA_DC[total_coeff - 1]
        } else {
            TOTAL_ZEROS_4X4[total_coeff - 1]
        };
        read_vlc(r, table, "total_zeros")?.0 as i32
    } else {
        0
    };
    let mut run_val = [0i32; 16];
    for run in run_val.iter_mut().take(total_coeff.saturating_sub(1)) {
        *run = if zeros_left > 0 {
            let table = RUN_BEFORE[(zeros_left.min(7) - 1) as usize];
            read_vlc(r, table, "run_before")?.0 as i32
        } else {
            0
        };
        zeros_left -= *run;
        if zeros_left < 0 {
            return Err(Error::corrupt("H.264 CAVLC: run_before exceeds zerosLeft"));
        }
    }
    run_val[total_coeff - 1] = zeros_left;

    // 9.2.4: place the levels at their scan positions.
    let mut coeff_num: i32 = -1;
    for i in (0..total_coeff).rev() {
        coeff_num += run_val[i] + 1;
        let index = start_idx + coeff_num as usize;
        if index > end_idx || index >= coeff_level.len() {
            return Err(Error::corrupt(format!(
                "H.264 CAVLC: coefficient index {index} past endIdx {end_idx}"
            )));
        }
        coeff_level[index] = level_val[i];
    }
    Ok(total_coeff as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::bitio::BitWriter;

    #[test]
    fn coeff_token_fixed_length_column() {
        // 8 <= nC uses the six bit code of Table 9-5's last column.
        for (code, expect) in [
            (0b000011u32, (0u8, 0u8)),
            (0b000000, (0, 1)),
            (0b000001, (1, 1)),
            (0b000110, (2, 2)),
            (0b111111, (3, 16)),
        ] {
            let mut w = BitWriter::new();
            w.write_bits(code, 6);
            w.align_to_byte();
            let bytes = w.into_bytes();
            assert_eq!(coeff_token(&mut BitReader::new(&bytes), 8).unwrap(), expect);
        }
    }

    #[test]
    fn coeff_token_selects_the_table_by_nc() {
        // The same bits mean different things in each column of Table 9-5:
        // '1' is (0, 0) for nC < 2 but (1, 1) in the chroma DC column, and
        // '10' is (1, 1) for 2 <= nC < 4.
        let one = [0b1000_0000u8];
        assert_eq!(coeff_token(&mut BitReader::new(&one), 0).unwrap(), (0, 0));
        assert_eq!(coeff_token(&mut BitReader::new(&one), -1).unwrap(), (1, 1));
        assert_eq!(coeff_token(&mut BitReader::new(&one), 2).unwrap(), (1, 1));
        // '11' is (0, 0) for 2 <= nC < 4; '1111' is (0, 0) for 4 <= nC < 8.
        let two = [0b1100_0000u8];
        assert_eq!(coeff_token(&mut BitReader::new(&two), 2).unwrap(), (0, 0));
        let four = [0b1111_0000u8];
        assert_eq!(coeff_token(&mut BitReader::new(&four), 4).unwrap(), (0, 0));
        assert!(coeff_token(&mut BitReader::new(&one), -2).is_err());
    }

    /// A block assembled by hand from clause 9.2, to be decoded back into the
    /// coefficients it was built from: levels 1, 3, -1, -1, 1 at scan positions
    /// 0, 1, 2, 4, 5.
    ///
    /// Reading the levels from the highest frequency down, as the syntax does,
    /// gives TrailingOnes = 3 (the +1, -1, -1 at positions 5, 4, 2),
    /// TotalCoeff = 5, total_zeros = 1 and one run of one zero.
    #[test]
    fn residual_block_levels_and_runs() {
        let mut w = BitWriter::new();
        w.write_bits(0b0000100, 7); // coeff_token: TrailingOnes 3, TotalCoeff 5
        w.write_bit(false); // trailing_ones_sign_flag: +1 at position 5
        w.write_bit(true); // -1 at position 4
        w.write_bit(true); // -1 at position 2
        // Position 1, level 3: levelCode 4 with suffixLength 0 is level_prefix 4
        // and no suffix.
        w.write_bits(0b00001, 5);
        // Position 0, level 1: suffixLength is 1 by now, so level_prefix 0 and a
        // zero suffix bit.
        w.write_bits(0b10, 2);
        w.write_bits(0b0100, 4); // total_zeros = 1 for tzVlcIndex 5
        w.write_bits(0b1, 1); // run_before = 0 before position 5
        w.write_bits(0b0, 1); // run_before = 1 before position 4
        w.align_to_byte();
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        let mut level = [0i32; 16];
        let total = residual_block_cavlc(&mut r, &mut level, 0, 15, 16, 0).unwrap();
        assert_eq!(total, 5);
        assert_eq!(&level[..8], &[1, 3, -1, 0, -1, 1, 0, 0]);
    }

    #[test]
    fn empty_block_reads_one_code() {
        let bytes = [0b1000_0000u8];
        let mut r = BitReader::new(&bytes);
        let mut level = [0i32; 16];
        assert_eq!(
            residual_block_cavlc(&mut r, &mut level, 0, 15, 16, 0).unwrap(),
            0
        );
        assert_eq!(r.bit_position(), 1);
        assert!(level.iter().all(|&v| v == 0));
    }

    #[test]
    fn truncated_block_is_need_more_not_a_panic() {
        let mut level = [0i32; 16];
        // A coeff_token that needs more bits than the buffer holds.
        let bytes = [0b0000_0000u8];
        let err = residual_block_cavlc(&mut BitReader::new(&bytes), &mut level, 0, 15, 16, 0)
            .unwrap_err();
        assert!(err.is_need_more(), "{err}");
        assert!(
            residual_block_cavlc(&mut BitReader::new(&[]), &mut level, 0, 15, 16, 0)
                .unwrap_err()
                .is_need_more()
        );
    }
}
