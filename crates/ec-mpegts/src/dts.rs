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
