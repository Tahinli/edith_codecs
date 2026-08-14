//! The three integrity primitives FLAC streams carry: CRC-8 over a frame
//! header, CRC-16 over a whole frame, and the MD5 of the *unencoded* audio in
//! `STREAMINFO`.
//!
//! MD5 is here rather than pulled in as a dependency because the family ships
//! no third-party crates, and because FLAC needs exactly one thing from it: the
//! digest of interleaved little-endian samples.

/// CRC-8, polynomial `x^8 + x^2 + x + 1` (0x07), initial value 0 — the frame
/// header check.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc = CRC8[(crc ^ b) as usize];
    }
    crc
}

/// CRC-16, polynomial `x^16 + x^15 + x^2 + 1` (0x8005), initial value 0 — the
/// frame footer check, computed over the frame including its header.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc = (crc << 8) ^ CRC16[(((crc >> 8) as u8) ^ b) as usize];
    }
    crc
}

const CRC8: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

const CRC16: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// MD5 (RFC 1321), streaming.
#[derive(Debug, Clone)]
pub struct Md5 {
    state: [u32; 4],
    len: u64,
    buf: [u8; 64],
    used: usize,
}

impl Default for Md5 {
    fn default() -> Self {
        Md5::new()
    }
}

impl Md5 {
    /// A fresh digest.
    pub fn new() -> Md5 {
        Md5 {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            len: 0,
            buf: [0; 64],
            used: 0,
        }
    }

    /// Feed bytes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.used > 0 {
            let take = (64 - self.used).min(data.len());
            self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used == 64 {
                let block = self.buf;
                self.compress(&block);
                self.used = 0;
            }
            // The tail below overwrites `used`, so a call that only topped up
            // the buffer has to stop here — otherwise the buffered bytes are
            // silently forgotten.
            if data.is_empty() {
                return;
            }
        }
        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.used = rest.len();
    }

    /// Finish and return the 16-byte digest.
    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_le_bytes());
        debug_assert_eq!(self.used, 0);
        let mut out = [0u8; 16];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(MD5_S[i]));
            a = tmp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

/// MD5 of interleaved samples the way `STREAMINFO` defines it: each sample as
/// `bits_per_sample / 8` little-endian bytes, signed, in channel order.
pub fn md5_of_samples(samples: &[i32], bits_per_sample: u32) -> [u8; 16] {
    let bytes = bits_per_sample.div_ceil(8) as usize;
    let mut md5 = Md5::new();
    // One buffered pass: a per-sample `update` call costs more than the hash.
    let mut buf = Vec::with_capacity(4096 * bytes);
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes()[..bytes]);
        if buf.len() >= 4096 * bytes {
            md5.update(&buf);
            buf.clear();
        }
    }
    md5.update(&buf);
    md5.finish()
}

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

#[rustfmt::skip]
const MD5_K: [u32; 64] = {
    // K[i] = floor(2^32 * abs(sin(i + 1))), spelled out because `sin` is not
    // available in a const context.
    [
        0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee, 0xf57c_0faf, 0x4787_c62a, 0xa830_4613,
        0xfd46_9501, 0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be, 0x6b90_1122, 0xfd98_7193,
        0xa679_438e, 0x49b4_0821, 0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa, 0xd62f_105d,
        0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8, 0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed,
        0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a, 0xfffa_3942, 0x8771_f681, 0x6d9d_6122,
        0xfde5_380c, 0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70, 0x289b_7ec6, 0xeaa1_27fa,
        0xd4ef_3085, 0x0488_1d05, 0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665, 0xf429_2244,
        0x432a_ff97, 0xab94_23a7, 0xfc93_a039, 0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
        0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1, 0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb,
        0xeb86_d391,
    ]
};

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        let cases: [(&str, &str); 5] = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, want) in cases {
            let mut md5 = Md5::new();
            md5.update(input.as_bytes());
            assert_eq!(hex(md5.finish()), want, "md5({input:?})");
        }
    }

    #[test]
    fn md5_is_split_invariant() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let mut whole = Md5::new();
        whole.update(&data);
        let mut split = Md5::new();
        for chunk in data.chunks(37) {
            split.update(chunk);
        }
        assert_eq!(whole.finish(), split.finish());
    }

    #[test]
    fn crcs_match_known_values() {
        // CRC-8/SMBUS and CRC-16/UMTS check values, i.e. of "123456789".
        assert_eq!(crc8(b"123456789"), 0xf4);
        assert_eq!(crc16(b"123456789"), 0xfee8);
        assert_eq!(crc8(&[]), 0);
        assert_eq!(crc16(&[]), 0);
    }
}
