//! DTS core framing for raw `.dts` files and DTS-in-WAV: just enough of the
//! core substream header to find frame boundaries, the sample rate and the
//! channel count. Decoding is `ec-dts`'s business.

/// The four core-substream sync words, as the first four bytes of a frame:
/// 16-bit big endian, 16-bit little endian, 14-bit big endian, 14-bit little
/// endian.
const SYNCS: [[u8; 4]; 4] = [
    [0x7F, 0xFE, 0x80, 0x01],
    [0xFE, 0x7F, 0x01, 0x80],
    [0x1F, 0xFF, 0xE8, 0x00],
    [0xFF, 0x1F, 0x00, 0xE8],
];

/// Extension-substream sync word (DTS-HD), big endian.
const EXSS_SYNC: [u8; 4] = [0x64, 0x58, 0x20, 0x25];

const RATES: [u32; 16] = [
    0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0,
];
const CHANNELS: [u8; 16] = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8];

/// What a core frame header states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Core {
    /// Frame size in the stream's own storage (14-bit streams are stored in
    /// 16-bit words, so this is the 16/14 larger figure).
    pub bytes: usize,
    /// PCM samples per channel.
    pub samples: u32,
    pub rate: u32,
    /// Channel count including the LFE.
    pub channels: u8,
}

/// Which of the four sync words `b` opens with.
pub(crate) fn sync_kind(b: &[u8]) -> Option<usize> {
    let head = b.get(..4)?;
    SYNCS.iter().position(|s| s == head)
}

/// The core frame header at the start of `b`.
pub(crate) fn core_header(b: &[u8]) -> Option<Core> {
    let kind = sync_kind(b)?;
    let raw = b.get(..16)?;
    let mut w = [0u8; 16];
    // Little-endian variants swap each byte pair.
    for i in 0..8 {
        if kind & 1 == 1 {
            w[2 * i] = raw[2 * i + 1];
            w[2 * i + 1] = raw[2 * i];
        } else {
            w[2 * i] = raw[2 * i];
            w[2 * i + 1] = raw[2 * i + 1];
        }
    }
    let packed: [u8; 16] = if kind >= 2 {
        // 14-bit words in 16-bit containers: keep the low 14 bits of each.
        let mut out = [0u8; 16];
        let mut acc = 0u32;
        let mut nbits = 0;
        let mut n = 0;
        for i in 0..8 {
            let v = u32::from(u16::from_be_bytes([w[2 * i], w[2 * i + 1]]) & 0x3FFF);
            acc = (acc << 14) | v;
            nbits += 14;
            while nbits >= 8 && n < 16 {
                out[n] = (acc >> (nbits - 8)) as u8;
                n += 1;
                nbits -= 8;
            }
            acc &= (1 << nbits) - 1;
        }
        out
    } else {
        w
    };
    if packed[..4] != [0x7F, 0xFE, 0x80, 0x01] {
        return None;
    }
    let bits = u128::from_be_bytes(packed);
    let field = |at: u32, n: u32| ((bits >> (128 - at - n)) & ((1u128 << n) - 1)) as usize;
    // Frame type 1 (normal frames only), deficit sample count 5, CRC flag 1.
    if field(32, 1) != 1 || field(33, 5) != 31 {
        return None;
    }
    let nblks = field(39, 7);
    let fsize = field(46, 14);
    let amode = field(60, 6);
    let sfreq = field(66, 4);
    let lff = field(85, 2);
    if nblks < 5 || fsize < 95 || amode > 15 || lff == 3 {
        return None;
    }
    let rate = RATES[sfreq];
    if rate == 0 {
        return None;
    }
    let size = fsize + 1;
    Some(Core {
        bytes: if kind >= 2 { size * 8 / 7 } else { size },
        samples: (nblks as u32 + 1) * 32,
        rate,
        channels: CHANNELS[amode] + u8::from(lff != 0),
    })
}

/// Length of the extension substream (DTS-HD) opening `b`, if one does.
pub(crate) fn exss_len(b: &[u8]) -> Option<usize> {
    if b.get(..4)? != EXSS_SYNC {
        return None;
    }
    let h = b.get(4..12)?;
    // 8 bits user data, 2 bits index, 1 bit header size type, then the
    // header size and the substream size at 8+16 or 12+20 bits.
    let bits = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
    let wide = (bits >> (63 - 10)) & 1 == 1;
    let (hn, sn) = if wide { (12, 20) } else { (8, 16) };
    let size = ((bits >> (64 - 11 - hn - sn)) & ((1 << sn) - 1)) as usize + 1;
    (size >= 8).then_some(size)
}

/// One access unit at the head of `b`: the core frame, plus an extension
/// substream that directly follows it. Returns the core header and the whole
/// unit's length in bytes.
pub(crate) fn access_unit(b: &[u8]) -> Option<(Core, usize)> {
    let core = core_header(b)?;
    let mut len = core.bytes;
    if let Some(rest) = b.get(len..)
        && let Some(ext) = exss_len(rest)
    {
        len += ext;
    }
    Some((core, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core header for stereo 48 kHz, 16 blocks, FSIZE 1023 as 16-bit big endian.
    fn header16() -> [u8; 16] {
        let mut bits: u128 = 0x7FFE_8001u128 << 96;
        let mut put = |at: u32, n: u32, v: u128| bits |= v << (128 - at - n);
        put(32, 1, 1); // normal frame
        put(33, 5, 31);
        put(39, 7, 15); // 16 blocks = 512 samples
        put(46, 14, 1023);
        put(60, 6, 2); // stereo
        put(66, 4, 13); // 48 kHz
        bits.to_be_bytes()
    }

    /// Repack as 14-bit words in 16-bit containers, sign-extended as the
    /// 14-bit format stores them.
    fn to14(b: &[u8; 16]) -> [u8; 16] {
        let bits = u128::from_be_bytes(*b);
        let mut out = [0u8; 16];
        for i in 0..8 {
            let w = ((bits >> (128 - 14 * (i + 1))) & 0x3FFF) as u16;
            let word = if w & 0x2000 != 0 { w | 0xC000 } else { w };
            out[2 * i..2 * i + 2].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn swap(b: &[u8; 16]) -> [u8; 16] {
        let mut o = *b;
        for p in o.as_chunks_mut::<2>().0.iter_mut() {
            p.swap(0, 1);
        }
        o
    }

    #[test]
    fn all_four_sync_forms_state_the_same_core() {
        let h = header16();
        let want = Core {
            bytes: 1024,
            samples: 512,
            rate: 48_000,
            channels: 2,
        };
        assert_eq!(core_header(&h), Some(want));
        assert_eq!(core_header(&swap(&h)), Some(want));
        let h14 = to14(&h);
        assert_eq!(&h14[..4], &[0x1F, 0xFF, 0xE8, 0x00]);
        // 14-bit streams are stored 16/14 larger.
        let want14 = Core {
            bytes: 1024 * 8 / 7,
            ..want
        };
        assert_eq!(core_header(&h14), Some(want14));
        assert_eq!(core_header(&swap(&h14)), Some(want14));
    }

    #[test]
    fn extension_substream_length_is_read_in_both_header_forms() {
        let mut b = EXSS_SYNC.to_vec();
        // user bits 0, index 0, short header (0), header size 16, size 256.
        let narrow = (0x0Fu64 << 16 | 0x00FF) << (64 - 35);
        b.extend_from_slice(&narrow.to_be_bytes());
        assert_eq!(exss_len(&b), Some(256));
        // long header (1): 12-bit header size, 20-bit substream size 4096.
        let wide = (((1u64 << 12) | 0x1F) << 20 | 0x00FFF) << (64 - 43);
        let mut w = EXSS_SYNC.to_vec();
        w.extend_from_slice(&wide.to_be_bytes());
        assert_eq!(exss_len(&w), Some(4096));
        assert_eq!(exss_len(&EXSS_SYNC), None);
    }

    #[test]
    fn junk_is_not_a_header() {
        assert_eq!(core_header(&[0u8; 16]), None);
        assert_eq!(core_header(&header16()[..8]), None);
    }
}
