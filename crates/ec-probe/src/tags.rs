//! The handful of tags a media library actually shows: title, artist, album,
//! date and track number, out of ID3v1, ID3v2 and Vorbis comments.
//!
//! Deliberately small. edith reads no metadata at all today (its symphonia
//! reader passes `MetadataOptions::default()` and never asks for the result),
//! so this exists to keep the door open rather than to be complete: no cover
//! art, no arbitrary user frames, and no mp4 `ilst` — an iTunes-tagged `.m4a`
//! reports the `moov/udta/name` title only. Adding a surface is a decision for
//! whoever needs one, and the parsers below are where it goes.

/// What a file says about itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tags {
    /// Track title.
    pub title: Option<String>,
    /// Performing artist.
    pub artist: Option<String>,
    /// Album (or "content group" in ID3 terms).
    pub album: Option<String>,
    /// Year or full date, as the file wrote it.
    pub date: Option<String>,
    /// Track number within its album.
    pub track: Option<u32>,
}

impl Tags {
    /// True when nothing was found.
    pub fn is_empty(&self) -> bool {
        *self == Tags::default()
    }

    /// Fill any field this does not have from `other`.
    pub(crate) fn merge(&mut self, other: Tags) {
        self.title = self.title.take().or(other.title);
        self.artist = self.artist.take().or(other.artist);
        self.album = self.album.take().or(other.album);
        self.date = self.date.take().or(other.date);
        self.track = self.track.or(other.track);
    }

    fn set(&mut self, key: &str, value: String) {
        if value.is_empty() {
            return;
        }
        let field = match key {
            "TIT2" | "TT2" | "TITLE" => &mut self.title,
            "TPE1" | "TP1" | "ARTIST" => &mut self.artist,
            "TALB" | "TAL" | "ALBUM" => &mut self.album,
            "TDRC" | "TYER" | "TYE" | "DATE" => &mut self.date,
            "TRCK" | "TRK" | "TRACKNUMBER" => {
                // "3/12" is a track number and a total; the number is the part
                // anything displays.
                self.track = value
                    .split(['/', ' '])
                    .next()
                    .and_then(|n| n.trim().parse().ok());
                return;
            }
            _ => return,
        };
        if field.is_none() {
            *field = Some(value);
        }
    }
}

/// Total bytes an ID3v2 tag at the head of a stream occupies, header included.
pub(crate) fn id3v2_len(head: &[u8]) -> Option<u64> {
    if head.len() < 10 || &head[..3] != b"ID3" {
        return None;
    }
    let size = syncsafe(&head[6..10])?;
    // Bit 4 of the flags is a footer, which is another ten bytes.
    let footer = u64::from(head[5] & 0x10 != 0) * 10;
    Some(10 + u64::from(size) + footer)
}

/// A 28-bit syncsafe integer.
fn syncsafe(b: &[u8]) -> Option<u32> {
    let b: [u8; 4] = b.get(..4)?.try_into().ok()?;
    if b.iter().any(|&x| x & 0x80 != 0) {
        return None;
    }
    Some(u32::from(b[0]) << 21 | u32::from(b[1]) << 14 | u32::from(b[2]) << 7 | u32::from(b[3]))
}

/// The text frames of a whole ID3v2 tag (header included).
pub(crate) fn from_id3v2(data: &[u8]) -> Tags {
    let mut tags = Tags::default();
    if id3v2_len(data).is_none() {
        return tags;
    }
    let version = data[3];
    // An unsynchronised or compressed tag is left alone rather than
    // mis-parsed: the audio is what matters here.
    if data[5] & 0x80 != 0 {
        return tags;
    }
    let mut at = 10usize;
    // An extended header sits between the header and the first frame.
    if data[5] & 0x40 != 0 && version >= 3 {
        let len = match version {
            3 => data
                .get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize + 4),
            _ => data.get(at..at + 4).and_then(syncsafe).map(|n| n as usize),
        };
        at += len.unwrap_or(0);
    }
    let (id_len, size_len) = match version {
        2 => (3, 3),
        _ => (4, 4),
    };
    while at + id_len + size_len <= data.len() {
        let id = &data[at..at + id_len];
        if id[0] == 0 {
            break; // padding
        }
        let size = match (version, &data[at + id_len..at + id_len + size_len]) {
            (2, s) => usize::from(s[0]) << 16 | usize::from(s[1]) << 8 | usize::from(s[2]),
            (4, s) => syncsafe(s).unwrap_or(0) as usize,
            (_, s) => u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize,
        };
        let flags = match version {
            2 => 0,
            _ => 2,
        };
        let start = at + id_len + size_len + flags;
        let Some(body) = data.get(start..start + size) else {
            break;
        };
        if id[0] == b'T'
            && let Ok(key) = std::str::from_utf8(id)
        {
            tags.set(key, text(body));
        }
        at = start + size;
    }
    tags
}

/// An ID3v2 text frame body: an encoding byte and then the string.
fn text(body: &[u8]) -> String {
    let Some((&encoding, rest)) = body.split_first() else {
        return String::new();
    };
    let s = match encoding {
        // ISO-8859-1: every byte is its own code point.
        0 => rest.iter().map(|&b| char::from(b)).collect(),
        1 | 2 => utf16(rest, encoding == 1),
        _ => String::from_utf8_lossy(rest).into_owned(),
    };
    s.trim_end_matches('\0').trim().to_string()
}

/// UTF-16, big-endian unless a byte-order mark says otherwise.
fn utf16(data: &[u8], bom: bool) -> String {
    let (data, little) = match (bom, data) {
        (true, [0xFF, 0xFE, rest @ ..]) => (rest, true),
        (true, [0xFE, 0xFF, rest @ ..]) => (rest, false),
        _ => (data, false),
    };
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| match little {
            true => u16::from_le_bytes([c[0], c[1]]),
            false => u16::from_be_bytes([c[0], c[1]]),
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// The 128-byte ID3v1 block at the end of a file.
pub(crate) fn from_id3v1(tail: &[u8]) -> Tags {
    let mut tags = Tags::default();
    if tail.len() < 128 || &tail[..3] != b"TAG" {
        return tags;
    }
    let field = |r: std::ops::Range<usize>| -> String {
        tail[r]
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| char::from(b))
            .collect::<String>()
            .trim()
            .to_string()
    };
    tags.set("TIT2", field(3..33));
    tags.set("TPE1", field(33..63));
    tags.set("TALB", field(63..93));
    tags.set("TDRC", field(93..97));
    // A zero separator before the last byte means the track number is there.
    if tail[125] == 0 && tail[126] != 0 {
        tags.track = Some(u32::from(tail[126]));
    }
    tags
}

/// A Vorbis comment block: the FLAC metadata block body, or the Ogg packet
/// with its `\x03vorbis` / `OpusTags` signature already stripped.
pub(crate) fn from_vorbis_comment(data: &[u8]) -> Tags {
    let mut tags = Tags::default();
    let u32_at = |at: usize| -> Option<usize> {
        let b: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b) as usize)
    };
    let Some(vendor) = u32_at(0) else {
        return tags;
    };
    let mut at = 4 + vendor;
    let Some(count) = u32_at(at) else {
        return tags;
    };
    at += 4;
    for _ in 0..count.min(1024) {
        let Some(len) = u32_at(at) else { break };
        at += 4;
        let Some(entry) = data.get(at..at + len) else {
            break;
        };
        at += len;
        let entry = String::from_utf8_lossy(entry);
        if let Some((key, value)) = entry.split_once('=') {
            tags.set(&key.to_ascii_uppercase(), value.to_string());
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ID3v2.3 tag of the shape the oracle writes, and the v1 block behind it.
    #[test]
    fn id3_tags_are_read_in_both_versions() {
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        let mut frames = Vec::new();
        for (id, body) in [
            ("TIT2", "A Tone"),
            ("TPE1", "edith_codecs"),
            ("TRCK", "3/12"),
        ] {
            frames.extend_from_slice(id.as_bytes());
            let text = format!("\u{0}{body}");
            frames.extend_from_slice(&(text.len() as u32).to_be_bytes());
            frames.extend_from_slice(&[0, 0]);
            frames.extend_from_slice(text.as_bytes());
        }
        let size = frames.len() as u32;
        tag.extend_from_slice(&[
            (size >> 21) as u8 & 0x7f,
            (size >> 14) as u8 & 0x7f,
            (size >> 7) as u8 & 0x7f,
            size as u8 & 0x7f,
        ]);
        tag.extend_from_slice(&frames);
        assert_eq!(id3v2_len(&tag), Some(tag.len() as u64));
        let tags = from_id3v2(&tag);
        assert_eq!(tags.title.as_deref(), Some("A Tone"));
        assert_eq!(tags.artist.as_deref(), Some("edith_codecs"));
        assert_eq!(tags.track, Some(3));

        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..3 + 6].copy_from_slice(b"A Tone");
        v1[126] = 7;
        let tags = from_id3v1(&v1);
        assert_eq!(tags.title.as_deref(), Some("A Tone"));
        assert_eq!(tags.track, Some(7));
    }

    #[test]
    fn vorbis_comments_are_case_insensitive_keys() {
        let mut block = Vec::new();
        let vendor = b"ec-probe";
        block.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        block.extend_from_slice(vendor);
        let entries = ["title=A Tone", "Artist=edith_codecs", "TRACKNUMBER=4"];
        block.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in entries {
            block.extend_from_slice(&(e.len() as u32).to_le_bytes());
            block.extend_from_slice(e.as_bytes());
        }
        let tags = from_vorbis_comment(&block);
        assert_eq!(tags.title.as_deref(), Some("A Tone"));
        assert_eq!(tags.artist.as_deref(), Some("edith_codecs"));
        assert_eq!(tags.track, Some(4));
        assert!(!tags.is_empty());
    }
}
