//! EXIF orientation, read from the TIFF block that JPEG, WebP and PNG all carry.
//!
//! The TIFF block is bare in WebP's `EXIF` chunk and in PNG's `eXIf` chunk (PNG 1.5
//! and libwebp both define it that way, and `image` reads it that way). JPEG's APP1
//! segment puts an `Exif\0\0` header in front, which the JPEG reader strips. Only
//! IFD0's orientation tag (0x0112) is read, which is the one the EXIF spec defines.

/// Orientation 1..=8 from a bare TIFF block, or `None` if the block is not a
/// readable TIFF or carries no valid orientation entry.
pub(crate) fn tiff_orientation(tiff: &[u8]) -> Option<u8> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = tiff.get(at..at + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = tiff.get(at..at + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = usize::from(u16_at(ifd)?);
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        // Tag 0x0112, type SHORT (3), one value: the value sits in the first two bytes of the field.
        if u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1 {
            let value = u16_at(entry + 8)?;
            return (1..=8).contains(&value).then_some(value as u8);
        }
    }
    None
}
