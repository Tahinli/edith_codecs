//! Decoded pictures, and the one-access-unit convenience entry point over
//! the stateful [`crate::dec::Decoder`].

use ec_core::error::{Error, Result};

/// A decoded picture: three planes at the SPS's *coded* size (before the
/// conformance window crops it). Samples are 16-bit whatever the bit depth:
/// Main and Main 10 both store one `u16` per sample, and `bit_depth` says how
/// many of its bits are used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPicture {
    /// Coded width.
    pub width: usize,
    /// Coded height.
    pub height: usize,
    /// `BitDepthY`.
    pub bit_depth: u32,
    /// `BitDepthC`; equal to `bit_depth` unless the stream codes chroma at another depth.
    pub bit_depth_c: u32,
    /// `chroma_format_idc`: 0 (4:0:0, `cb` and `cr` empty), 1 (4:2:0), 2 (4:2:2) or 3 (4:4:4).
    pub chroma_format_idc: u32,
    /// Luma, `width * height`.
    pub y: Vec<u16>,
    /// Cb, `width/2 * height/2`.
    pub cb: Vec<u16>,
    /// Cr, `width/2 * height/2`.
    pub cr: Vec<u16>,
}

/// The log2 subsampling of the chroma planes for `chroma_format_idc`, `(sub_x, sub_y)`:
/// 4:2:0 is `(1, 1)`, 4:2:2 is `(1, 0)`, 4:4:4 is `(0, 0)`.
pub fn chroma_shifts(chroma_format_idc: u32) -> (u32, u32) {
    match chroma_format_idc {
        1 => (1, 1),
        2 => (1, 0),
        _ => (0, 0),
    }
}

impl DecodedPicture {
    /// The log2 chroma subsampling of this picture, `(sub_x, sub_y)`.
    pub fn chroma_shifts(&self) -> (u32, u32) {
        chroma_shifts(self.chroma_format_idc)
    }

    /// The chroma plane width and height of this picture, rounded up.
    pub fn chroma_dims(&self) -> (usize, usize) {
        if self.chroma_format_idc == 0 {
            return (0, 0);
        }
        let (sx, sy) = self.chroma_shifts();
        (self.width.div_ceil(1 << sx), self.height.div_ceil(1 << sy))
    }
    /// A picture of mid-grey samples (`1 << (bit_depth - 1)`).
    pub fn grey(width: usize, height: usize, bit_depth: u32) -> DecodedPicture {
        Self::grey_planes(width, height, bit_depth, bit_depth)
    }

    /// A picture of mid-grey samples, each plane at its own bit depth.
    pub fn grey_planes(
        width: usize,
        height: usize,
        bit_depth: u32,
        bit_depth_c: u32,
    ) -> DecodedPicture {
        Self::grey_planes_fmt(width, height, bit_depth, bit_depth_c, 1)
    }

    /// [`Self::grey_planes`] for any `chroma_format_idc`.
    pub fn grey_planes_fmt(
        width: usize,
        height: usize,
        bit_depth: u32,
        bit_depth_c: u32,
        chroma_format_idc: u32,
    ) -> DecodedPicture {
        let mid = 1u16 << (bit_depth - 1);
        let mid_c = 1u16 << (bit_depth_c - 1);
        let (sx, sy) = chroma_shifts(chroma_format_idc);
        let (cw, ch) = (width.div_ceil(1 << sx), height.div_ceil(1 << sy));
        DecodedPicture {
            width,
            height,
            bit_depth,
            bit_depth_c,
            chroma_format_idc,
            y: vec![mid; width * height],
            cb: vec![mid_c; cw * ch],
            cr: vec![mid_c; cw * ch],
        }
    }

    /// The bit depth of component `c` (0 luma, 1 and 2 chroma).
    pub fn bit_depth_of(&self, c: usize) -> u32 {
        if c == 0 {
            self.bit_depth
        } else {
            self.bit_depth_c
        }
    }

    /// The luma plane as 8-bit samples. Only meaningful for 8-bit pictures.
    pub fn y_u8(&self) -> Vec<u8> {
        self.y.iter().map(|&v| v as u8).collect()
    }
}

/// Decode one IDR access unit: VPS/SPS/PPS/slice NALs in Annex-B framing, one
/// intra slice covering the whole picture. A thin wrapper over
/// [`crate::dec::Decoder`], which owns the picture management; anything
/// outside what that decoder can pixel-decode is refused with a named reason.
pub fn decode_idr_au(data: &[u8]) -> Result<DecodedPicture> {
    let mut decoder = crate::dec::Decoder::new();
    decoder.decode_annex_b(data)?;
    decoder.flush()?;
    let frame = decoder.next_frame().ok_or_else(|| {
        Error::corrupt("HEVC decode: no IDR slice found in the access unit".to_string())
    })?;
    Ok(std::sync::Arc::try_unwrap(frame.picture).unwrap_or_else(|pic| (*pic).clone()))
}
