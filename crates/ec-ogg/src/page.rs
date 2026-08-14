//! The page: Ogg's only structural unit (RFC 3533 §6).
//!
//! A page is a 27-byte fixed header, a segment table of 1..=255 lacing values,
//! and a body whose length is the sum of those values. Packets are cut into
//! 255-byte segments; a segment shorter than 255 ends its packet, which is why
//! a packet whose length is an exact multiple of 255 needs a trailing zero-length
//! segment — the single most-missed rule in the format, and the one that makes a
//! decoder wait forever for a packet that already ended.

use ec_core::{Error, Result};

use crate::crc;

/// Capture pattern every page starts with.
pub const CAPTURE: [u8; 4] = *b"OggS";
/// Bytes before the segment table.
pub const HEADER_LEN: usize = 27;
/// Most lacing values one page can carry.
pub const MAX_SEGMENTS: usize = 255;
/// Granule position meaning "no packet ends on this page" (RFC 3533 §6.1).
pub const NO_GRANULE: i64 = -1;

/// `header_type` bits.
const FLAG_CONTINUED: u8 = 0x01;
const FLAG_BOS: u8 = 0x02;
const FLAG_EOS: u8 = 0x04;

/// A page header, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHeader {
    /// The first segment continues a packet started on an earlier page.
    pub continued: bool,
    /// Beginning of stream: this page carries the stream's first packet.
    pub bos: bool,
    /// End of stream: the last page of this logical stream.
    pub eos: bool,
    /// Position of the last packet *completed* on this page, in the mapping's
    /// own units, or [`NO_GRANULE`] when no packet ends here.
    pub granule: i64,
    /// Which logical stream this page belongs to.
    pub serial: u32,
    /// Page counter within the logical stream, from zero.
    pub sequence: u32,
    /// Lacing values, in order.
    pub segments: Vec<u8>,
}

impl PageHeader {
    /// Total body length: the sum of the lacing values.
    pub fn body_len(&self) -> usize {
        self.segments.iter().map(|&s| usize::from(s)).sum()
    }

    /// Decode the fixed part of a header. `buf` must hold at least
    /// [`HEADER_LEN`] bytes; the segment table is read by the caller, which
    /// knows how much more input it can supply.
    ///
    /// Returns the header with an empty segment table plus the segment count.
    pub fn parse_fixed(buf: &[u8]) -> Result<(PageHeader, usize)> {
        if buf.len() < HEADER_LEN {
            return Err(Error::NeedMore);
        }
        if buf[..4] != CAPTURE {
            return Err(Error::corrupt("Ogg page: no OggS capture pattern"));
        }
        if buf[4] != 0 {
            return Err(Error::unsupported(
                format!("Ogg stream structure version {}", buf[4]),
                "only version 0 is defined",
            ));
        }
        let flags = buf[5];
        let header = PageHeader {
            continued: flags & FLAG_CONTINUED != 0,
            bos: flags & FLAG_BOS != 0,
            eos: flags & FLAG_EOS != 0,
            granule: i64::from_le_bytes(buf[6..14].try_into().unwrap()),
            serial: u32::from_le_bytes(buf[14..18].try_into().unwrap()),
            sequence: u32::from_le_bytes(buf[18..22].try_into().unwrap()),
            segments: Vec::new(),
        };
        Ok((header, usize::from(buf[26])))
    }

    /// The checksum stored in a fixed header.
    pub fn stored_crc(buf: &[u8]) -> u32 {
        u32::from_le_bytes(buf[22..26].try_into().unwrap())
    }

    /// Serialize header and segment table, checksum included, and append the
    /// body — the exact bytes of one page.
    pub fn write_page(&self, body: &[u8], out: &mut Vec<u8>) {
        debug_assert_eq!(self.body_len(), body.len());
        debug_assert!(!self.segments.is_empty() && self.segments.len() <= MAX_SEGMENTS);
        let start = out.len();
        out.extend_from_slice(&CAPTURE);
        out.push(0);
        let mut flags = 0u8;
        if self.continued {
            flags |= FLAG_CONTINUED;
        }
        if self.bos {
            flags |= FLAG_BOS;
        }
        if self.eos {
            flags |= FLAG_EOS;
        }
        out.push(flags);
        out.extend_from_slice(&self.granule.to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // checksum, filled in below
        out.push(self.segments.len() as u8);
        out.extend_from_slice(&self.segments);
        out.extend_from_slice(body);
        // The checksum covers the whole page with its own field read as zero.
        let sum = crc::crc32(&[&out[start..]]);
        out[start + 22..start + 26].copy_from_slice(&sum.to_le_bytes());
    }
}

/// The lacing values a packet of `len` bytes becomes: `len / 255` values of 255
/// then the remainder — including the explicit zero that terminates a packet
/// whose length is a multiple of 255.
pub fn lacing(len: usize) -> impl Iterator<Item = u8> {
    let full = len / 255;
    (0..=full).map(move |i| match i < full {
        true => 255,
        false => (len % 255) as u8,
    })
}

/// The three Xiph header packets, laced the way `CodecParameters::extradata`
/// carries them: two length prefixes in the 255-plus-remainder form, then the
/// packets back to back, all behind a leading count of `2`.
///
/// This is the shape ffmpeg calls "Xiph extradata" and the one Vorbis, Theora
/// and Speex setup data travels in outside of Ogg. [`None`] when a header is
/// too long to describe (the format has no escape beyond a 255 run) or when the
/// packet count is not the two-prefixed-plus-one this layout can express.
pub fn xiph_lace(packets: &[&[u8]]) -> Option<Vec<u8>> {
    if packets.len() < 2 || packets.len() > 256 {
        return None;
    }
    let mut out = vec![(packets.len() - 1) as u8];
    for packet in &packets[..packets.len() - 1] {
        out.extend(lacing(packet.len()));
    }
    for packet in packets {
        out.extend_from_slice(packet);
    }
    Some(out)
}

/// The inverse of [`xiph_lace`]: split laced extradata back into packets.
pub fn xiph_unlace(data: &[u8]) -> Result<Vec<&[u8]>> {
    let short = || Error::corrupt("Xiph extradata: truncated lacing");
    let count = usize::from(*data.first().ok_or_else(short)?) + 1;
    let mut at = 1;
    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count - 1 {
        let mut len = 0usize;
        loop {
            let lace = usize::from(*data.get(at).ok_or_else(short)?);
            at += 1;
            len += lace;
            if lace < 255 {
                break;
            }
        }
        lengths.push(len);
    }
    let mut out = Vec::with_capacity(count);
    for len in lengths {
        let end = at.checked_add(len).ok_or_else(short)?;
        out.push(data.get(at..end).ok_or_else(short)?);
        at = end;
    }
    // The last packet runs to the end: its length is the one the layout does
    // not store.
    out.push(&data[at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lacing_terminates_multiples_of_255() {
        assert_eq!(lacing(0).collect::<Vec<_>>(), vec![0]);
        assert_eq!(lacing(1).collect::<Vec<_>>(), vec![1]);
        assert_eq!(lacing(254).collect::<Vec<_>>(), vec![254]);
        // The rule that matters: 255 is *not* one segment of 255, it is a 255
        // and a 0, or the packet never ends.
        assert_eq!(lacing(255).collect::<Vec<_>>(), vec![255, 0]);
        assert_eq!(lacing(256).collect::<Vec<_>>(), vec![255, 1]);
        assert_eq!(lacing(510).collect::<Vec<_>>(), vec![255, 255, 0]);
        for len in [0, 1, 255, 256, 700, 65_535] {
            let sum: usize = lacing(len).map(usize::from).sum();
            assert_eq!(sum, len, "lacing must sum back to the packet length");
        }
    }

    #[test]
    fn page_round_trips_through_its_own_checksum() {
        let body: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        let header = PageHeader {
            continued: true,
            bos: false,
            eos: true,
            granule: 123_456,
            serial: 0xdead_beef,
            sequence: 7,
            segments: lacing(body.len()).collect(),
        };
        let mut bytes = Vec::new();
        header.write_page(&body, &mut bytes);

        let (parsed, nsegs) = PageHeader::parse_fixed(&bytes).unwrap();
        let segments = bytes[HEADER_LEN..HEADER_LEN + nsegs].to_vec();
        let parsed = PageHeader { segments, ..parsed };
        assert_eq!(parsed, header);
        assert_eq!(parsed.body_len(), body.len());

        // The stored checksum verifies with its own field zeroed.
        let stored = PageHeader::stored_crc(&bytes);
        let mut zeroed = bytes.clone();
        zeroed[22..26].fill(0);
        assert_eq!(crc::crc32(&[&zeroed]), stored);
        // ...and a single flipped body byte breaks it.
        let mut damaged = zeroed.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0x01;
        assert_ne!(crc::crc32(&[&damaged]), stored);

        assert!(PageHeader::parse_fixed(&bytes[..10]).is_err());
        assert!(PageHeader::parse_fixed(b"NotAnOggPageHeaderAtAll....").is_err());
    }

    #[test]
    fn xiph_lacing_round_trips() {
        let ident = [1u8; 30];
        let comment = [2u8; 300];
        let setup = [3u8; 4000];
        let packets = [&ident[..], &comment[..], &setup[..]];
        let laced = xiph_lace(&packets).unwrap();
        assert_eq!(laced[0], 2, "count byte is packets - 1");
        let back = xiph_unlace(&laced).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0], &ident[..]);
        assert_eq!(back[1], &comment[..]);
        assert_eq!(back[2], &setup[..]);
        assert!(xiph_lace(&[&ident[..]]).is_none());
        assert!(xiph_unlace(&[]).is_err());
        assert!(xiph_unlace(&[2, 255]).is_err());
    }
}
