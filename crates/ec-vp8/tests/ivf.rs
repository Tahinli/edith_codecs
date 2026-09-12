//! Minimal IVF demuxing for the ec-vp8 test fixtures (RFC's companion
//! container; only what the witnesses need).

/// One IVF frame: (pts, payload).
pub struct IvfFrame {
    /// Presentation timestamp from the IVF frame header.
    #[allow(dead_code)]
    pub pts: u64,
    /// The compressed VP8 frame payload.
    pub data: Vec<u8>,
}

/// Parse an IVF file into (fourcc, width, height, frames).
pub fn parse_ivf(bytes: &[u8]) -> (&str, u16, u16, Vec<IvfFrame>) {
    assert_eq!(&bytes[0..4], b"DKIF", "not an IVF file");
    let hdr_len = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let fourcc = std::str::from_utf8(&bytes[8..12]).unwrap();
    let width = u16::from_le_bytes([bytes[12], bytes[13]]);
    let height = u16::from_le_bytes([bytes[14], bytes[15]]);
    let mut pos = hdr_len;
    let mut frames = Vec::new();
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let pts = u64::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
            bytes[pos + 8],
            bytes[pos + 9],
            bytes[pos + 10],
            bytes[pos + 11],
        ]);
        pos += 12;
        if pos + sz > bytes.len() {
            break;
        }
        frames.push(IvfFrame {
            pts,
            data: bytes[pos..pos + sz].to_vec(),
        });
        pos += sz;
    }
    (fourcc, width, height, frames)
}

/// Extract the VP8 payload from a lossy WebP file (RIFF/WEBP with a VP8
/// chunk) — the same VP8 key-frame bytes in a different container.
pub fn webp_vp8_chunk(bytes: &[u8]) -> Vec<u8> {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let tag = &bytes[pos..pos + 4];
        let sz = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        if tag == b"VP8 " {
            return bytes[pos + 8..pos + 8 + sz].to_vec();
        }
        pos += 8 + sz + (sz & 1); // chunks are word-aligned
    }
    panic!("no VP8 chunk in webp");
}

/// Root of the workspace checkout this test runs in (crates/ec-vp8/../..).
pub fn fixture_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vp8");
    if dir.is_dir() { Some(dir) } else { None }
}
