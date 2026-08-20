//! CAVLC writing (spec 9.2): the exact inverse of [`crate::cavlc`].
//!
//! Every code table here is *derived at compile time from the decoder's own
//! tables* — the reverse maps below are `const fn` walks of
//! [`crate::tables`], so an encoder code and the decoder code it must match
//! cannot drift apart. The round-trip test at the bottom re-decodes what this
//! module writes with [`crate::cavlc::residual_block`].

use ec_core::BitWriter;

use crate::tables::{
    CBP_INTER_420, CBP_INTRA_420, COEFF_TOKEN_CHROMA_DC, COEFF_TOKEN_NC0, COEFF_TOKEN_NC2,
    COEFF_TOKEN_NC4, CoeffTokenEntry, RUN_BEFORE, TOTAL_ZEROS_4X4, TOTAL_ZEROS_CHROMA_DC, VlcEntry,
};

/// `(length, code)` of one VLC codeword; length 0 marks a combination the
/// table does not contain.
type Code = (u8, u16);

/// Reverse `coeff_token` table: `[total_coeff][trailing_ones]`.
type CoeffTokenRev = [[Code; 4]; 17];

const fn rev_coeff_token(t: &[CoeffTokenEntry]) -> CoeffTokenRev {
    let mut out = [[(0u8, 0u16); 4]; 17];
    let mut i = 0;
    while i < t.len() {
        let (len, code, tc, t1) = t[i];
        out[tc as usize][t1 as usize] = (len, code);
        i += 1;
    }
    out
}

const fn rev_vlc<const N: usize>(t: &[VlcEntry]) -> [Code; N] {
    let mut out = [(0u8, 0u16); N];
    let mut i = 0;
    while i < t.len() {
        let (len, code, value) = t[i];
        out[value as usize] = (len, code);
        i += 1;
    }
    out
}

const fn rev_vlc_table<const R: usize, const N: usize>(t: &[&[VlcEntry]; R]) -> [[Code; N]; R] {
    let mut out = [[(0u8, 0u16); N]; R];
    let mut i = 0;
    while i < R {
        out[i] = rev_vlc::<N>(t[i]);
        i += 1;
    }
    out
}

static COEFF_TOKEN: [CoeffTokenRev; 3] = [
    rev_coeff_token(COEFF_TOKEN_NC0),
    rev_coeff_token(COEFF_TOKEN_NC2),
    rev_coeff_token(COEFF_TOKEN_NC4),
];
static COEFF_TOKEN_CDC: CoeffTokenRev = rev_coeff_token(COEFF_TOKEN_CHROMA_DC);
static TOTAL_ZEROS: [[Code; 16]; 15] = rev_vlc_table(&TOTAL_ZEROS_4X4);
static TOTAL_ZEROS_CDC: [[Code; 4]; 3] = rev_vlc_table(&TOTAL_ZEROS_CHROMA_DC);
static RUNS: [[Code; 15]; 7] = rev_vlc_table(&RUN_BEFORE);

#[inline]
fn write_code(w: &mut BitWriter, c: Code) {
    debug_assert!(c.0 > 0, "VLC combination absent from the table");
    w.write_bits(u32::from(c.1), u32::from(c.0));
}

/// `coeff_token` (9.2.1). `nc` is the neighbour count, -1 for 4:2:0 chroma DC.
fn write_coeff_token(w: &mut BitWriter, nc: i32, total_coeff: u8, trailing_ones: u8) {
    if nc == -1 {
        write_code(
            w,
            COEFF_TOKEN_CDC[total_coeff as usize][trailing_ones as usize],
        );
        return;
    }
    if nc >= 8 {
        // Fixed-length 6-bit form.
        let v = if total_coeff == 0 {
            3
        } else {
            u32::from(total_coeff - 1) * 4 + u32::from(trailing_ones)
        };
        w.write_bits(v, 6);
        return;
    }
    let table = match nc {
        0..=1 => 0,
        2..=3 => 1,
        _ => 2,
    };
    write_code(
        w,
        COEFF_TOKEN[table][total_coeff as usize][trailing_ones as usize],
    );
}

/// Base value and suffix width of one `level_prefix` under `suffix_length`,
/// read straight off the decoder's reconstruction in [`crate::cavlc`].
#[inline]
fn level_range(prefix: u32, suffix_length: u32) -> (i64, u32) {
    if prefix >= 15 {
        let mut base = (15i64 << suffix_length) + if suffix_length == 0 { 15 } else { 0 };
        if prefix >= 16 {
            base += (1i64 << (prefix - 3)) - 4096;
        }
        (base, prefix - 3)
    } else if prefix == 14 && suffix_length == 0 {
        (14, 4)
    } else {
        ((i64::from(prefix)) << suffix_length, suffix_length)
    }
}

/// `level_prefix` + `level_suffix` (9.2.2.1) for one `levelCode`, choosing the
/// shortest prefix whose range contains it.
fn write_level_code(w: &mut BitWriter, level_code: i64, suffix_length: u32) {
    for prefix in 0..=25u32 {
        let (base, size) = level_range(prefix, suffix_length);
        if level_code >= base && level_code < base + (1i64 << size) {
            w.write_bits64(0, prefix);
            w.write_bit(true);
            if size > 0 {
                w.write_bits64((level_code - base) as u64, size);
            }
            return;
        }
    }
    debug_assert!(false, "level {level_code} outside every level_prefix range");
}

/// Write one CAVLC residual block (9.2) from `coeff[0..max_num_coeff]` in scan
/// order. Returns TotalCoeff, which the caller records as the block's non-zero
/// count for its neighbours.
pub(crate) fn write_residual_block(
    w: &mut BitWriter,
    coeff: &[i32],
    max_num_coeff: usize,
    nc: i32,
) -> u8 {
    debug_assert!(max_num_coeff <= 16);
    // Scan positions of the non-zero levels, lowest frequency first.
    let mut pos = [0usize; 16];
    let mut total_coeff = 0usize;
    for (k, &c) in coeff.iter().take(max_num_coeff).enumerate() {
        if c != 0 {
            pos[total_coeff] = k;
            total_coeff += 1;
        }
    }
    if total_coeff == 0 {
        write_coeff_token(w, nc, 0, 0);
        return 0;
    }
    // Trailing ones: up to three +-1 levels at the high-frequency end.
    let mut trailing_ones = 0usize;
    while trailing_ones < 3
        && trailing_ones < total_coeff
        && coeff[pos[total_coeff - 1 - trailing_ones]].abs() == 1
    {
        trailing_ones += 1;
    }
    write_coeff_token(w, nc, total_coeff as u8, trailing_ones as u8);

    // 9.2.2: levels, highest frequency first.
    for i in 0..trailing_ones {
        w.write_bit(coeff[pos[total_coeff - 1 - i]] < 0);
    }
    let mut suffix_length = u32::from(total_coeff > 10 && trailing_ones < 3);
    for i in trailing_ones..total_coeff {
        let level = i64::from(coeff[pos[total_coeff - 1 - i]]);
        let mut level_code = if level > 0 {
            2 * level - 2
        } else {
            -2 * level - 1
        };
        if i == trailing_ones && trailing_ones < 3 {
            level_code -= 2;
        }
        write_level_code(w, level_code, suffix_length);
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // 9.2.3 / 9.2.4: total_zeros then the runs, highest frequency first.
    let total_zeros = pos[total_coeff - 1] + 1 - total_coeff;
    if total_coeff < max_num_coeff {
        if nc == -1 {
            write_code(w, TOTAL_ZEROS_CDC[total_coeff - 1][total_zeros]);
        } else {
            write_code(w, TOTAL_ZEROS[total_coeff - 1][total_zeros]);
        }
    }
    let mut zeros_left = total_zeros;
    for i in 0..total_coeff - 1 {
        if zeros_left == 0 {
            break;
        }
        let k = total_coeff - 1 - i;
        let run = pos[k] - pos[k - 1] - 1;
        write_code(w, RUNS[zeros_left.min(7) - 1][run]);
        zeros_left -= run;
    }
    total_coeff as u8
}

/// `coded_block_pattern` as me(v) (9.1.2, Table 9-4).
pub(crate) fn write_cbp(w: &mut BitWriter, cbp_luma: u8, cbp_chroma: u8, intra: bool) {
    let cbp = cbp_luma | (cbp_chroma << 4);
    let table: &[u8] = if intra {
        &CBP_INTRA_420
    } else {
        &CBP_INTER_420
    };
    let code = table
        .iter()
        .position(|&v| v == cbp)
        .expect("every 4:2:0 coded_block_pattern has a code number");
    w.write_ue(code as u32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitCursor;
    use crate::cavlc::residual_block;

    fn round_trip(levels: &[i32], max_num_coeff: usize, nc: i32) {
        let mut w = BitWriter::new();
        let tc = write_residual_block(&mut w, levels, max_num_coeff, nc);
        // A stop bit keeps `more_rbsp_data` honest and gives the reader room.
        w.write_bit(true);
        w.align_to_byte();
        let bytes = w.into_bytes();
        let mut r = BitCursor::new(&bytes, 0);
        let mut out = [0i32; 16];
        let got = residual_block(&mut r, &mut out, max_num_coeff, nc).expect("decodes");
        assert_eq!(got, tc, "TotalCoeff mismatch for {levels:?}");
        for (k, &want) in levels.iter().take(max_num_coeff).enumerate() {
            assert_eq!(out[k], want, "position {k} of {levels:?}");
        }
    }

    /// An 8x8 block written as four interleaved 4x4 blocks reads back through
    /// the decoder's 4x4 path into the same 64 coefficients, with the
    /// decoder's reassembly rule (`scan8[4 * i + i4] = scan[i]`).
    #[test]
    fn interleaved_8x8_round_trips() {
        let mut state = 0xC0FF_EE00u32;
        let mut rand = move |m: u32| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 16) % m
        };
        for _ in 0..200 {
            let mut c8 = [0i32; 64];
            for _ in 0..rand(64) {
                let v = rand(70) as i32 + 1;
                c8[rand(64) as usize] = if rand(2) == 0 { v } else { -v };
            }
            let mut w = BitWriter::new();
            let mut tcs = [0u8; 4];
            for j in 0..4 {
                tcs[j] = write_residual_block(
                    &mut w,
                    &super::super::entropy::sub_block_4x4(&c8, j),
                    16,
                    j as i32 * 3,
                );
            }
            w.write_bit(true);
            w.align_to_byte();
            let bytes = w.into_bytes();
            let mut r = BitCursor::new(&bytes, 0);
            let mut got = [0i32; 64];
            for j in 0..4 {
                let mut scan = [0i32; 16];
                let tc = residual_block(&mut r, &mut scan, 16, j as i32 * 3).expect("decodes");
                assert_eq!(tc, tcs[j]);
                for i in 0..16 {
                    got[4 * i + j] = scan[i];
                }
            }
            assert_eq!(got, c8);
        }
    }

    /// Every block this writer emits decodes back to the same levels, over a
    /// spread that covers empty blocks, trailing ones, long runs, the
    /// suffix-length ladder and the escape codes.
    #[test]
    fn residual_blocks_round_trip() {
        let mut state = 0x1234_5678u32;
        let mut rand = move |m: u32| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 16) % m
        };
        for &(max, nc) in &[
            (16usize, 0i32),
            (16, 2),
            (16, 5),
            (16, 9),
            (15, 0),
            (15, 3),
            (4, -1),
        ] {
            for trial in 0..400 {
                let mut levels = [0i32; 16];
                let density = 1 + rand(max as u32);
                for level in levels.iter_mut().take(max) {
                    if rand(max as u32) < density {
                        // Magnitudes spread over the whole ladder including
                        // the 4096-wide escape.
                        let mag = match trial % 5 {
                            0 => 1,
                            1 => 1 + rand(3) as i32,
                            2 => 1 + rand(40) as i32,
                            3 => 1 + rand(600) as i32,
                            _ => 1 + rand(9000) as i32,
                        };
                        *level = if rand(2) == 0 { mag } else { -mag };
                    }
                }
                round_trip(&levels, max, nc);
            }
        }
    }

    /// The all-zero block, the single-coefficient block and a fully populated
    /// block are the boundary cases of 9.2.3 (no total_zeros is written when
    /// TotalCoeff = maxNumCoeff).
    #[test]
    fn boundary_blocks_round_trip() {
        round_trip(&[0; 16], 16, 0);
        let mut one = [0i32; 16];
        one[15] = -1;
        round_trip(&one, 16, 4);
        round_trip(&[1; 16], 16, 0);
        round_trip(&[-3; 16], 16, 8);
        round_trip(&[2, 0, 0, -1], 4, -1);
    }

    /// The coded_block_pattern code numbers invert the decoder's tables.
    #[test]
    fn cbp_codes_invert() {
        for &(luma, chroma) in &[(0u8, 0u8), (15, 2), (3, 1), (5, 0), (15, 0)] {
            for intra in [true, false] {
                let mut w = BitWriter::new();
                write_cbp(&mut w, luma, chroma, intra);
                w.write_bit(true);
                w.align_to_byte();
                let bytes = w.into_bytes();
                let mut r = BitCursor::new(&bytes, 0);
                let code = r.read_ue().unwrap() as usize;
                let table = if intra { CBP_INTRA_420 } else { CBP_INTER_420 };
                assert_eq!(table[code], luma | (chroma << 4));
            }
        }
    }
}
