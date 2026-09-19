//! Minimal IVF demuxing for the ec-vp9 test fixtures (the DKIF
//! container; only what the witnesses need).

/// One IVF frame: (pts, payload).
pub struct IvfFrame {
    /// Presentation timestamp from the IVF frame header.
    #[allow(dead_code)]
    pub pts: u64,
    /// The compressed VP9 payload (possibly a superframe chunk).
    #[allow(dead_code)]
    pub data: Vec<u8>,
}

/// The frame chunk's VP9 uncompressed-header `show_frame` bit lives
/// behind the marker/profile bits; the witnesses only need a presence
/// census, so this walks enough of the header to find it (spec 6.2).
#[allow(dead_code)]
pub fn show_frame(data: &[u8]) -> bool {
    // Bit layout: 2 marker, 2 profile, 1 show_existing, 1 frame_type,
    // 1 show_frame (bit 7 of byte 0, LSB-first).
    (data[0] >> 7) & 1 == 1
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
        let pts = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
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

/// Root of the workspace checkout this test runs in (crates/ec-vp9/../..).
#[allow(dead_code)] // shared by every test binary that does `mod ivf`
pub fn fixture_dir() -> Option<std::path::PathBuf> {
    let bitstreams =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bitstreams");
    if bitstreams.is_dir() {
        Some(bitstreams)
    } else {
        None
    }
}
