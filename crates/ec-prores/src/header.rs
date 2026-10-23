//! Frame and picture headers (RDD 36 section 5).

use ec_core::error::{Error, Result};
use ec_core::frame::Interlace;

/// Largest picture side accepted (ProRes goes to 8K in practice).
pub const MAX_DIM: u32 = 16384;
/// Largest picture area accepted, in luma samples.
pub const MAX_AREA: u64 = 8192 * 4608;

/// What the frame header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    /// Visible width.
    pub width: u32,
    /// Visible height.
    pub height: u32,
    /// Header `version` field (0 or 1).
    pub version: u16,
    /// Four-byte encoder id (`Lavc`, `apl0`, ...).
    pub creator: [u8; 4],
    /// True for 4:4:4 (profiles 4444 / 4444 XQ), false for 4:2:2.
    pub is_444: bool,
    /// Interlace of the coded frame.
    pub interlace: Interlace,
    /// 0 = no alpha, 8 or 16 = coded alpha depth.
    pub alpha_bits: u8,
    /// Colour primaries / transfer / matrix code points as signalled.
    pub primaries: u8,
    /// Transfer characteristics code point.
    pub transfer: u8,
    /// Matrix coefficients code point.
    pub matrix: u8,
    /// The frame carried its own luma quantisation matrix.
    pub custom_luma_qmat: bool,
    /// The frame carried its own chroma quantisation matrix.
    pub custom_chroma_qmat: bool,
    /// `log2` of the nominal macroblocks per slice (0..=3) of the first picture.
    pub log2_slice_mbs: u8,
}

pub struct Header {
    pub info: FrameInfo,
    pub qmat_luma: [u8; 64],
    pub qmat_chroma: [u8; 64],
    /// Offset of the first picture header inside the frame data.
    pub pictures_at: usize,
    /// End of the frame (from the frame size field).
    pub end: usize,
}

pub fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]))
}
pub fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(at)?, *d.get(at + 1)?, *d.get(at + 2)?, *d.get(at + 3)?]))
}

pub fn parse_frame(data: &[u8]) -> Result<Header> {
    let size = be32(data, 0).ok_or_else(|| Error::corrupt("ProRes frame: shorter than the size field"))? as usize;
    if data.get(4..8) != Some(b"icpf") {
        return Err(Error::corrupt("ProRes frame: missing 'icpf' tag"));
    }
    if size < 28 || size > data.len() {
        return Err(Error::corrupt(format!(
            "ProRes frame: size field {size} against {} bytes of data",
            data.len()
        )));
    }
    let d = &data[8..size];
    let hdr_size = be16(d, 0).ok_or_else(|| Error::corrupt("ProRes frame header: truncated"))? as usize;
    if hdr_size < 20 || hdr_size > d.len() {
        return Err(Error::corrupt(format!("ProRes frame header: size {hdr_size}")));
    }
    let version = be16(d, 2).unwrap_or(0);
    if version > 1 {
        return Err(Error::unsupported(
            format!("ProRes bitstream version {version}"),
            "only versions 0 and 1 are defined",
        ));
    }
    let width = u32::from(be16(d, 8).unwrap_or(0));
    let height = u32::from(be16(d, 10).unwrap_or(0));
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM || u64::from(width) * u64::from(height) > MAX_AREA {
        return Err(Error::corrupt(format!("ProRes frame header: picture size {width}x{height}")));
    }
    let chroma_factor = d[12] >> 6;
    let is_444 = match chroma_factor {
        2 => false,
        3 => true,
        c => {
            return Err(Error::unsupported(
                format!("ProRes chroma format {c}"),
                "only 4:2:2 (2) and 4:4:4 (3) are defined",
            ));
        }
    };
    let interlace = match (d[12] >> 2) & 3 {
        0 => Interlace::Progressive,
        1 => Interlace::TopFieldFirst,
        2 => Interlace::BottomFieldFirst,
        t => return Err(Error::corrupt(format!("ProRes frame header: frame_type {t}"))),
    };
    let alpha_bits = match d[17] & 0xf {
        0 => 0,
        1 => 8,
        2 => 16,
        a => {
            return Err(Error::unsupported(
                format!("ProRes alpha_channel_type {a}"),
                "only none (0), 8-bit (1) and 16-bit (2) are defined",
            ));
        }
    };
    if alpha_bits != 0 && !is_444 {
        return Err(Error::corrupt("ProRes frame header: alpha channel in a 4:2:2 frame"));
    }
    let flags = d[19];
    let mut qmat_luma = [4u8; 64];
    let mut qmat_chroma = [4u8; 64];
    let mut p = 20;
    let need = |p: usize| -> Result<()> {
        if p + 64 > hdr_size {
            Err(Error::corrupt("ProRes frame header: quantisation matrix past the header"))
        } else {
            Ok(())
        }
    };
    let custom_luma_qmat = flags & 2 != 0;
    if custom_luma_qmat {
        need(p)?;
        qmat_luma.copy_from_slice(&d[p..p + 64]);
        p += 64;
    }
    let custom_chroma_qmat = flags & 1 != 0;
    if custom_chroma_qmat {
        need(p)?;
        qmat_chroma.copy_from_slice(&d[p..p + 64]);
    } else {
        // RDD 36: an absent chroma matrix repeats the luma one.
        qmat_chroma = qmat_luma;
    }
    Ok(Header {
        info: FrameInfo {
            width,
            height,
            version,
            creator: [d[4], d[5], d[6], d[7]],
            is_444,
            interlace,
            alpha_bits,
            primaries: d[14],
            transfer: d[15],
            matrix: d[16],
            custom_luma_qmat,
            custom_chroma_qmat,
            log2_slice_mbs: 0,
        },
        qmat_luma,
        qmat_chroma,
        pictures_at: 8 + hdr_size,
        end: size,
    })
}

/// One picture header plus its slice table.
pub struct Picture {
    pub slice_count: usize,
    pub log2_slice_mbs: u32,
    /// Offset (in the frame) of the slice size table.
    pub table_at: usize,
    /// Offset of the first slice.
    pub data_at: usize,
    /// End of this picture.
    pub end: usize,
}

pub fn parse_picture(frame: &[u8], at: usize, end: usize) -> Result<Picture> {
    let p = frame.get(at..end).ok_or_else(|| Error::corrupt("ProRes picture: starts past the frame"))?;
    if p.len() < 8 {
        return Err(Error::corrupt("ProRes picture header: truncated"));
    }
    let hdr = usize::from(p[0] >> 3);
    let pic_size = be32(p, 1).unwrap_or(0) as usize;
    let slice_count = usize::from(be16(p, 5).unwrap_or(0));
    let log2 = u32::from(p[7] >> 4);
    if hdr < 8 || hdr > p.len() {
        return Err(Error::corrupt(format!("ProRes picture header: size {hdr}")));
    }
    if log2 > 3 || p[7] & 0xf != 0 {
        return Err(Error::unsupported(
            format!("ProRes slice size byte {:#x}", p[7]),
            "slices are 1, 2, 4 or 8 macroblocks wide and one macroblock tall",
        ));
    }
    if pic_size < hdr + slice_count * 2 || pic_size > p.len() {
        return Err(Error::corrupt(format!(
            "ProRes picture header: picture size {pic_size} (header {hdr}, {slice_count} slices, {} bytes left)",
            p.len()
        )));
    }
    Ok(Picture {
        slice_count,
        log2_slice_mbs: log2,
        table_at: at + hdr,
        data_at: at + hdr + slice_count * 2,
        end: at + pic_size,
    })
}
