//! `cc_data` triplets and their carriage wrappers.

/// One `cc_data` construct: `cc_valid`, `cc_type` and two data bytes.
///
/// `cc_type` 0 is NTSC line 21 field 1, 1 is field 2, 2 is a DTVCC packet continuation and 3 a
/// DTVCC packet start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CcTriplet {
    /// `cc_valid`.
    pub valid: bool,
    /// `cc_type` (0..=3).
    pub cc_type: u8,
    /// The two payload bytes.
    pub data: [u8; 2],
}

/// Parse a `cc_data()` structure (flags byte first). `None` when `process_cc_data_flag` is clear
/// or the buffer is too short.
pub fn parse_cc_data(b: &[u8]) -> Option<Vec<CcTriplet>> {
    if b.len() < 2 || b[0] & 0x40 == 0 {
        return None;
    }
    let count = (b[0] & 0x1F) as usize;
    let body = b.get(2..2 + count * 3)?;
    Some(
        body.as_chunks::<3>()
            .0
            .iter()
            .map(|c| CcTriplet {
                valid: c[0] & 0x04 != 0,
                cc_type: c[0] & 3,
                data: [c[1], c[2]],
            })
            .collect(),
    )
}

/// Parse the body of an SEI `user_data_registered_itu_t_t35` message (country code first).
/// Accepts ATSC `GA94` (provider 0x0031) and the DIRECTV variant (provider 0x002F).
pub fn parse_t35(p: &[u8]) -> Option<Vec<CcTriplet>> {
    if p.len() < 5 || p[0] != 0xB5 {
        return None;
    }
    let provider = u16::from_be_bytes([p[1], p[2]]);
    match provider {
        0x0031 if p.len() >= 8 && &p[3..7] == b"GA94" && p[7] == 0x03 => parse_cc_data(&p[8..]),
        0x002F if p[3] == 0x03 => parse_cc_data(&p[4..]),
        _ => None,
    }
}

/// Parse MPEG-2 `user_data()` bytes (after the `00 00 01 B2` start code): ATSC `GA94` or the DVD
/// `CC 01 F8` form.
pub fn parse_mpeg2_user_data(p: &[u8]) -> Option<Vec<CcTriplet>> {
    if p.len() >= 5 && &p[..4] == b"GA94" && p[4] == 0x03 {
        return parse_cc_data(&p[5..]);
    }
    if p.len() >= 5 && p[0] == b'C' && p[1] == b'C' && p[2] == 0x01 && p[3] == 0xF8 {
        // DVD: flags byte (bit7 field 1 first, bit6 captions present, bits 5..1 count), then
        // groups of marker (0xFF field 1 / 0xFE field 2) plus two bytes.
        let flags = p[4];
        if flags & 0x40 == 0 {
            return None;
        }
        let count = ((flags >> 1) & 0x1F) as usize;
        let mut out = Vec::new();
        let mut i = 5;
        while i + 3 <= p.len() && out.len() < count.max(1) * 2 && p[i] & 0xFE == 0xFE {
            out.push(CcTriplet {
                valid: true,
                cc_type: if p[i] & 1 == 1 { 0 } else { 1 },
                data: [p[i + 1], p[i + 2]],
            });
            i += 3;
        }
        return Some(out);
    }
    None
}

/// Parse a CDP (SMPTE 334-2 caption distribution packet) and return its `ccdata_section`.
pub fn parse_cdp(b: &[u8]) -> Option<Vec<CcTriplet>> {
    if b.len() < 7 || b[0] != 0x96 || b[1] != 0x69 {
        return None;
    }
    let len = (b[2] as usize).min(b.len());
    let b = &b[..len];
    let mut i = 7;
    while i < b.len() {
        match b[i] {
            0x71 => i += 5,
            0x72 => {
                let count = (b.get(i + 1)? & 0x1F) as usize;
                let body = b.get(i + 2..i + 2 + count * 3)?;
                return Some(
                    body.as_chunks::<3>()
                        .0
                        .iter()
                        .map(|c| CcTriplet {
                            valid: c[0] & 0x04 != 0,
                            cc_type: c[0] & 3,
                            data: [c[1], c[2]],
                        })
                        .collect(),
                );
            }
            0x73 => i += 2 + (b.get(i + 1)? & 0x0F) as usize * 7,
            _ => return None,
        }
    }
    None
}

/// Triplets of a QuickTime `c608` sample: `cdat` boxes carry field 1 pairs, `cdt2` field 2.
pub fn parse_c608_sample(s: &[u8]) -> Vec<CcTriplet> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= s.len() {
        let size = u32::from_be_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]) as usize;
        if size < 8 || i + size > s.len() {
            break;
        }
        let cc_type = match &s[i + 4..i + 8] {
            b"cdat" => Some(0),
            b"cdt2" => Some(1),
            _ => None,
        };
        if let Some(cc_type) = cc_type {
            for pair in s[i + 8..i + size].as_chunks::<2>().0 {
                out.push(CcTriplet {
                    valid: true,
                    cc_type,
                    data: [pair[0], pair[1]],
                });
            }
        }
        i += size;
    }
    out
}

/// Triplets of a QuickTime `c708` sample: `ccdp` boxes hold a CDP each.
pub fn parse_c708_sample(s: &[u8]) -> Vec<CcTriplet> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= s.len() {
        let size = u32::from_be_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]) as usize;
        if size < 8 || i + size > s.len() {
            break;
        }
        if let (b"ccdp", Some(t)) = (&s[i + 4..i + 8], parse_cdp(&s[i + 8..i + size])) {
            out.extend(t);
        }
        i += size;
    }
    out
}

/// Raw `cc_type 0` byte pairs of an elementary caption track (mkv/mp4 tracks that store bare
/// 608 pairs, possibly field-tagged by the caller).
pub fn parse_raw_pairs(s: &[u8], cc_type: u8) -> Vec<CcTriplet> {
    s.as_chunks::<2>()
        .0
        .iter()
        .map(|p| CcTriplet {
            valid: true,
            cc_type,
            data: [p[0], p[1]],
        })
        .collect()
}
