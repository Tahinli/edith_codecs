//! The Ogg page checksum.
//!
//! CRC-32 with the Ethernet polynomial `0x04c11db7`, but — unlike almost every
//! other CRC-32 in use — with no bit reflection, no initial value and no final
//! inversion. Feeding a byte stream through the reflected variant every other
//! format uses is the classic way to produce pages a real player rejects, so
//! this is its own small module with its own table.

/// Bit-per-bit table for the unreflected polynomial.
const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            r = match r & 0x8000_0000 {
                0 => r << 1,
                _ => (r << 1) ^ 0x04c1_1db7,
            };
            bit += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = build_table();

/// Checksum of `chunks` concatenated — the page header (with its checksum field
/// zeroed) followed by the page body.
pub fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = 0u32;
    for chunk in chunks {
        for &byte in *chunk {
            crc = (crc << 8) ^ TABLE[(((crc >> 24) as u8) ^ byte) as usize];
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_vectors() {
        // Nothing checksums to zero, and the catalogue check value for this
        // variant (poly 04C11DB7, init 0, no reflection, no final xor) is
        // 0x89a1897f over the ASCII digits — the value that separates it from
        // the reflected CRC-32 every zip file uses.
        assert_eq!(crc32(&[]), 0);
        assert_eq!(crc32(&[b"123456789"]), 0x89a1_897f);
        // Chunking must not change the answer — the muxer feeds header and body
        // as two slices, the demuxer as three.
        let all = b"OggS\x00\x02longer body bytes";
        assert_eq!(crc32(&[all]), crc32(&[&all[..4], &all[4..7], &all[7..]]));
    }
}
