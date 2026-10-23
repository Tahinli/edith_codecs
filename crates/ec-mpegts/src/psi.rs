//! Program Specific Information: section reassembly, CRC32/MPEG-2, PAT and PMT.

/// Longest section a PSI table may have (12-bit length + 3 header bytes).
pub(crate) const MAX_SECTION: usize = 4096;

/// CRC-32/MPEG-2 (poly 0x04C11DB7, init all ones, no reflection, no final xor).
pub(crate) fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Reassembles one PID's sections.
#[derive(Debug, Default)]
pub(crate) struct SectionBuf {
    buf: Vec<u8>,
    active: bool,
}

impl SectionBuf {
    pub(crate) fn reset(&mut self) {
        self.buf.clear();
        self.active = false;
    }

    /// Feeds one packet payload; complete, CRC-checked sections go to `out`.
    /// Returns the number of sections dropped for a bad CRC or length.
    pub(crate) fn push(&mut self, pusi: bool, payload: &[u8], out: &mut Vec<Vec<u8>>) -> u32 {
        let mut bad = 0;
        let mut data = payload;
        if pusi {
            let Some((&ptr, rest)) = data.split_first() else {
                return 0;
            };
            let ptr = usize::from(ptr);
            if ptr > rest.len() {
                self.reset();
                return 0;
            }
            // The tail of the section in flight, then sections starting here.
            if self.active {
                self.buf.extend_from_slice(&rest[..ptr]);
                bad += self.drain(out);
            }
            self.reset();
            data = &rest[ptr..];
            self.active = true;
        } else if !self.active {
            return 0;
        }
        self.buf.extend_from_slice(data);
        bad += self.drain(out);
        if self.buf.len() > MAX_SECTION {
            self.reset();
        }
        bad
    }

    /// Pulls every whole section off the front of the buffer.
    fn drain(&mut self, out: &mut Vec<Vec<u8>>) -> u32 {
        let mut bad = 0;
        loop {
            if self.buf.len() < 3 {
                return bad;
            }
            if self.buf[0] == 0xFF {
                // Stuffing: nothing else in this payload.
                self.reset();
                return bad;
            }
            let len = 3 + ((usize::from(self.buf[1] & 0x0f) << 8) | usize::from(self.buf[2]));
            if len > MAX_SECTION {
                self.reset();
                return bad + 1;
            }
            if self.buf.len() < len {
                return bad;
            }
            let section: Vec<u8> = self.buf.drain(..len).collect();
            let long_form = section[1] & 0x80 != 0;
            if long_form && (len < 12 || crc32_mpeg(&section) != 0) {
                bad += 1;
            } else {
                out.push(section);
            }
            if self.buf.is_empty() {
                self.active = false;
                return bad;
            }
        }
    }
}

/// One program as the PAT lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatEntry {
    /// `program_number` (never 0, which names the NIT).
    pub program_number: u16,
    /// PID carrying this program's PMT.
    pub pmt_pid: u16,
}

/// Parses a PAT section (already CRC-checked). `None` when it is not a PAT.
pub(crate) fn parse_pat(section: &[u8]) -> Option<(u8, Vec<PatEntry>)> {
    if *section.first()? != 0x00 || section.len() < 12 {
        return None;
    }
    let current = section[5] & 1 != 0;
    if !current {
        return None;
    }
    let version = (section[5] >> 1) & 0x1f;
    let body = section.get(8..section.len() - 4)?;
    let mut out = Vec::new();
    for e in body.as_chunks::<4>().0.iter().take(1024) {
        let program_number = u16::from_be_bytes([e[0], e[1]]);
        let pid = (u16::from(e[2] & 0x1f) << 8) | u16::from(e[3]);
        if program_number != 0 {
            out.push(PatEntry {
                program_number,
                pmt_pid: pid,
            });
        }
    }
    Some((version, out))
}

/// One elementary stream of a PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsEntry {
    /// `stream_type`.
    pub stream_type: u8,
    /// Elementary PID.
    pub pid: u16,
    /// `ES_info` descriptors as `(tag, body)`.
    pub descriptors: Vec<(u8, Vec<u8>)>,
}

impl EsEntry {
    /// Body of the first descriptor with `tag`.
    pub fn descriptor(&self, tag: u8) -> Option<&[u8]> {
        self.descriptors
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, b)| b.as_slice())
    }

    /// Registration descriptor `format_identifier`, when present.
    pub fn registration(&self) -> Option<[u8; 4]> {
        self.descriptor(0x05)
            .and_then(|b| b.get(..4))
            .map(|b| [b[0], b[1], b[2], b[3]])
    }

    /// ISO 639 language code of the first language descriptor.
    pub fn language(&self) -> Option<String> {
        let b = self.descriptor(0x0A)?.get(..3)?;
        if b.iter().all(u8::is_ascii_alphabetic) {
            Some(String::from_utf8_lossy(b).to_ascii_lowercase())
        } else {
            None
        }
    }
}

/// A parsed PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pmt {
    /// `program_number`.
    pub program_number: u16,
    /// `version_number`.
    pub version: u8,
    /// PID that carries the PCR.
    pub pcr_pid: u16,
    /// Elementary streams in PMT order.
    pub streams: Vec<EsEntry>,
}

/// Elementary streams kept per program; a PMT cannot name more than 8191 PIDs, but
/// nothing sane has more than this.
pub(crate) const MAX_ES: usize = 64;

pub(crate) fn descriptors(mut b: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    while b.len() >= 2 && out.len() < 32 {
        let (tag, len) = (b[0], usize::from(b[1]));
        let Some(body) = b.get(2..2 + len) else {
            break;
        };
        out.push((tag, body.to_vec()));
        b = &b[2 + len..];
    }
    out
}

/// Parses a PMT section (already CRC-checked). `None` when it is not a PMT.
pub(crate) fn parse_pmt(section: &[u8]) -> Option<Pmt> {
    if *section.first()? != 0x02 || section.len() < 16 {
        return None;
    }
    if section[5] & 1 == 0 {
        return None;
    }
    let program_number = u16::from_be_bytes([section[3], section[4]]);
    let version = (section[5] >> 1) & 0x1f;
    let pcr_pid = (u16::from(section[8] & 0x1f) << 8) | u16::from(section[9]);
    let info_len = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
    let end = section.len().checked_sub(4)?;
    let mut at = 12usize.checked_add(info_len)?;
    let mut streams = Vec::new();
    while at + 5 <= end && streams.len() < MAX_ES {
        let stream_type = section[at];
        let pid = (u16::from(section[at + 1] & 0x1f) << 8) | u16::from(section[at + 2]);
        let es_len = (usize::from(section[at + 3] & 0x0f) << 8) | usize::from(section[at + 4]);
        let d = section.get(at + 5..(at + 5 + es_len).min(end))?;
        streams.push(EsEntry {
            stream_type,
            pid,
            descriptors: descriptors(d),
        });
        at += 5 + es_len;
    }
    Some(Pmt {
        program_number,
        version,
        pcr_pid,
        streams,
    })
}

#[cfg(test)]
pub(crate) fn build_section(table_id: u8, ext: u16, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![table_id, 0xB0 | ((len >> 8) as u8 & 0x0f), len as u8];
    s.extend_from_slice(&ext.to_be_bytes());
    s.push(0xC1);
    s.extend_from_slice(&[0, 0]);
    s.extend_from_slice(body);
    let crc = crc32_mpeg(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_a_section_with_its_crc_is_zero() {
        let s = build_section(0, 1, &[0, 1, 0xE1, 0x00]);
        assert_eq!(crc32_mpeg(&s), 0);
        let (v, e) = parse_pat(&s).unwrap();
        assert_eq!(v, 0);
        assert_eq!(
            e,
            vec![PatEntry {
                program_number: 1,
                pmt_pid: 0x100
            }]
        );
    }

    #[test]
    fn section_split_over_packets_and_bad_crc() {
        let s = build_section(0, 1, &[0, 1, 0xE1, 0x00]);
        let mut buf = SectionBuf::default();
        let mut out = Vec::new();
        let mut first = vec![0u8];
        first.extend_from_slice(&s[..5]);
        assert_eq!(buf.push(true, &first, &mut out), 0);
        assert!(out.is_empty());
        buf.push(false, &s[5..], &mut out);
        assert_eq!(out.len(), 1);
        let mut bad = s.clone();
        bad[9] ^= 1;
        let mut p = vec![0u8];
        p.extend_from_slice(&bad);
        assert_eq!(buf.push(true, &p, &mut out), 1);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn pmt_parses() {
        let mut body = vec![0xE1, 0x01, 0xF0, 0x00];
        body.extend_from_slice(&[0x1B, 0xE1, 0x01, 0xF0, 0x00]);
        body.extend_from_slice(&[0x06, 0xE1, 0x02, 0xF0, 0x05, 0x6A, 0x01, 0x00, 0x0A, 0x00]);
        let s = build_section(2, 1, &body);
        let pmt = parse_pmt(&s).unwrap();
        assert_eq!(pmt.pcr_pid, 0x101);
        assert_eq!(pmt.streams.len(), 2);
        assert_eq!(pmt.streams[1].descriptors[0].0, 0x6A);
    }
}
