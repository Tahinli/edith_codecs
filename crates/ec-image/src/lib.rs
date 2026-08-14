//! Still-image decoding: PNG, JPEG and WebP.
//!
//! One entry point — [`decode`] over bytes, [`open`] over a path — guesses the
//! format from the leading bytes and hands back an [`Image`]: dimensions, a
//! [`Pixels`] buffer in the file's own colour form, and the [`Metadata`] the
//! file declared about it (gamma, sRGB intent, EXIF orientation).
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - Metadata is *parsed and exposed, never applied*. A JPEG that says it was
//!   shot sideways decodes to the pixels as stored, with
//!   [`Metadata::orientation`] set; a PNG's gamma is reported, not rendered.
//!   Rotating or gamma-correcting behind the caller's back would make a
//!   round-trip through this crate lossy in a way no caller asked for.
//! - Sample depth survives: a 16-bit PNG decodes to a 16-bit [`Pixels`]
//!   variant. [`Image::to_rgb8`] and friends convert on demand.
//! - Refusals are [`ec_core::Error::Unsupported`] naming *what* and *why* —
//!   an animated WebP and a CMYK JPEG each say so rather than producing a
//!   plausible-looking wrong picture.
//! - Truncated or corrupt input is an error, never a panic and never an
//!   unbounded allocation: [`Limits`] bounds pixel count before any buffer is
//!   sized from header fields.
//!
//! No async, no unsafe, no external dependencies beyond the family's own
//! [`ec_core`] and [`ec_inflate`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Coefficient positions, macroblock columns and prediction samples are indices
// in every specification these decoders implement; iterating them by index is
// what keeps the code readable against the text it comes from.
#![allow(clippy::needless_range_loop)]

pub mod jpeg;
pub mod png;
mod upsample;
pub mod webp;

use ec_core::{PixelFormat, Plane, VideoFrame};

// The family's error taxonomy is this crate's too, and a caller that has to
// name a decode failure needs it in scope.
pub use ec_core::{Error, Result};

/// The formats this crate decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// Portable Network Graphics (W3C PNG, RFC 2083).
    Png,
    /// JFIF/EXIF JPEG (ITU-T T.81 baseline and progressive).
    Jpeg,
    /// WebP: a RIFF container over VP8 (lossy) or VP8L (lossless).
    WebP,
}

impl ImageFormat {
    /// The format whose signature `data` starts with, if any.
    ///
    /// This is what `with_guessed_format` means: content, not file extension.
    pub fn guess(data: &[u8]) -> Option<ImageFormat> {
        if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            Some(ImageFormat::Png)
        } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(ImageFormat::Jpeg)
        } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
            Some(ImageFormat::WebP)
        } else {
            None
        }
    }

    /// The usual lowercase extension, for diagnostics.
    pub fn extension(&self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
        }
    }
}

/// Bounds applied before any buffer is sized from a header field.
///
/// A three-byte dimension field can ask for a 68-gigapixel allocation; this is
/// the trust boundary that says no first. The default admits everything the
/// formats themselves can express up to 2^28 pixels — comfortably past 8K
/// (33 Mpx) and far below what would exhaust a desktop.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest `width * height` accepted.
    pub max_pixels: u64,
    /// Largest single decompressed buffer accepted, in bytes.
    pub max_alloc: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pixels: 1 << 28,
            max_alloc: 1 << 32,
        }
    }
}

impl Limits {
    /// Check a header's dimensions before they size anything.
    pub fn check(&self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::corrupt(format!("{width}x{height} image")));
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(Error::unsupported(
                format!("{width}x{height} image"),
                format!("{pixels} pixels is past the {} limit", self.max_pixels),
            ));
        }
        Ok(())
    }
}

/// What a file said about its own pixels, parsed but not applied.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metadata {
    /// PNG `gAMA`: the gamma the file was authored at.
    pub gamma: Option<f64>,
    /// PNG `sRGB`: rendering intent 0..=3, meaning the file is sRGB.
    pub srgb_intent: Option<u8>,
    /// EXIF orientation tag 1..=8, from a JPEG APP1 or a WebP `EXIF` chunk.
    ///
    /// Exposed, never applied — the same choice the incumbent `image` crate
    /// makes, so a caller swapping between them sees the same pixels.
    pub orientation: Option<u8>,
}

/// A decoded image's samples, in the file's own colour form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pixels {
    /// 8-bit grayscale.
    L8(Vec<u8>),
    /// 8-bit grayscale + alpha.
    La8(Vec<u8>),
    /// 8-bit R,G,B.
    Rgb8(Vec<u8>),
    /// 8-bit R,G,B,A.
    Rgba8(Vec<u8>),
    /// 16-bit grayscale.
    L16(Vec<u16>),
    /// 16-bit grayscale + alpha.
    La16(Vec<u16>),
    /// 16-bit R,G,B.
    Rgb16(Vec<u16>),
    /// 16-bit R,G,B,A.
    Rgba16(Vec<u16>),
}

impl Pixels {
    /// Samples per pixel.
    pub fn channels(&self) -> usize {
        match self {
            Pixels::L8(_) | Pixels::L16(_) => 1,
            Pixels::La8(_) | Pixels::La16(_) => 2,
            Pixels::Rgb8(_) | Pixels::Rgb16(_) => 3,
            Pixels::Rgba8(_) | Pixels::Rgba16(_) => 4,
        }
    }

    /// Bits per sample: 8 or 16.
    pub fn bit_depth(&self) -> u8 {
        match self {
            Pixels::L8(_) | Pixels::La8(_) | Pixels::Rgb8(_) | Pixels::Rgba8(_) => 8,
            _ => 16,
        }
    }

    /// True when the samples carry an alpha channel.
    pub fn has_alpha(&self) -> bool {
        matches!(
            self,
            Pixels::La8(_) | Pixels::Rgba8(_) | Pixels::La16(_) | Pixels::Rgba16(_)
        )
    }
}

/// A decoded still image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The samples.
    pub pixels: Pixels,
    /// What the file declared about them.
    pub meta: Metadata,
}

/// Rounding-correct 16-bit sample down to 8 bits: `round(v * 255 / 65535)`.
fn narrow(v: u16) -> u8 {
    ((u32::from(v) + 128) / 257) as u8
}

/// 8-bit sample widened to 16: `v * 65535 / 255`, exact.
fn widen(v: u8) -> u16 {
    u16::from(v) * 257
}

impl Image {
    /// `(width, height)`.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Packed 8-bit R,G,B, dropping any alpha channel.
    pub fn to_rgb8(&self) -> Vec<u8> {
        let n = (self.width as usize) * (self.height as usize);
        let mut out = Vec::with_capacity(n * 3);
        match &self.pixels {
            Pixels::L8(d) => d.iter().for_each(|&l| out.extend_from_slice(&[l, l, l])),
            Pixels::La8(d) => d
                .chunks_exact(2)
                .for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0]])),
            Pixels::Rgb8(d) => out.extend_from_slice(d),
            Pixels::Rgba8(d) => d
                .chunks_exact(4)
                .for_each(|p| out.extend_from_slice(&p[..3])),
            Pixels::L16(d) => d.iter().for_each(|&l| {
                let l = narrow(l);
                out.extend_from_slice(&[l, l, l]);
            }),
            Pixels::La16(d) => d.chunks_exact(2).for_each(|p| {
                let l = narrow(p[0]);
                out.extend_from_slice(&[l, l, l]);
            }),
            Pixels::Rgb16(d) => d.iter().for_each(|&s| out.push(narrow(s))),
            Pixels::Rgba16(d) => d
                .chunks_exact(4)
                .for_each(|p| out.extend(p[..3].iter().map(|&s| narrow(s)))),
        }
        out
    }

    /// Packed 8-bit R,G,B,A; opaque where the source had no alpha.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let n = (self.width as usize) * (self.height as usize);
        let mut out = Vec::with_capacity(n * 4);
        match &self.pixels {
            Pixels::L8(d) => d
                .iter()
                .for_each(|&l| out.extend_from_slice(&[l, l, l, 255])),
            Pixels::La8(d) => d
                .chunks_exact(2)
                .for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
            Pixels::Rgb8(d) => d
                .chunks_exact(3)
                .for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
            Pixels::Rgba8(d) => out.extend_from_slice(d),
            Pixels::L16(d) => d.iter().for_each(|&l| {
                let l = narrow(l);
                out.extend_from_slice(&[l, l, l, 255]);
            }),
            Pixels::La16(d) => d.chunks_exact(2).for_each(|p| {
                let l = narrow(p[0]);
                out.extend_from_slice(&[l, l, l, narrow(p[1])]);
            }),
            Pixels::Rgb16(d) => d.chunks_exact(3).for_each(|p| {
                out.extend_from_slice(&[narrow(p[0]), narrow(p[1]), narrow(p[2]), 255]);
            }),
            Pixels::Rgba16(d) => d.iter().for_each(|&s| out.push(narrow(s))),
        }
        out
    }

    /// Packed 16-bit R,G,B — 8-bit sources widened exactly (`v * 257`).
    pub fn to_rgb16(&self) -> Vec<u16> {
        match &self.pixels {
            Pixels::Rgb16(d) => d.clone(),
            Pixels::Rgba16(d) => d.chunks_exact(4).flat_map(|p| p[..3].to_vec()).collect(),
            Pixels::L16(d) => d.iter().flat_map(|&l| [l, l, l]).collect(),
            Pixels::La16(d) => d.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0]]).collect(),
            _ => self.to_rgb8().into_iter().map(widen).collect(),
        }
    }

    /// Packed 16-bit R,G,B,A; opaque where the source had no alpha.
    pub fn to_rgba16(&self) -> Vec<u16> {
        match &self.pixels {
            Pixels::Rgba16(d) => d.clone(),
            Pixels::Rgb16(d) => d
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], u16::MAX])
                .collect(),
            Pixels::L16(d) => d.iter().flat_map(|&l| [l, l, l, u16::MAX]).collect(),
            Pixels::La16(d) => d
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            _ => self.to_rgba8().into_iter().map(widen).collect(),
        }
    }

    /// The image as an [`ec_core::VideoFrame`], packed RGB8 or RGBA8.
    ///
    /// The natural bridge for a caller that composes stills and decoded video
    /// in one pipeline: alpha survives when the file had it, rather than being
    /// silently dropped on the way to a frame.
    pub fn to_video_frame(&self) -> Result<VideoFrame> {
        let (format, data) = if self.pixels.has_alpha() {
            (PixelFormat::Rgba8, self.to_rgba8())
        } else {
            (PixelFormat::Rgb8, self.to_rgb8())
        };
        let stride = (self.width as usize) * if self.pixels.has_alpha() { 4 } else { 3 };
        VideoFrame::try_new(
            format,
            self.width,
            self.height,
            vec![Plane::new(data, stride)],
        )
    }
}

/// Dimensions and format from a header alone — the whole file is not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// Which format the bytes are in.
    pub format: ImageFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Decode `data`, guessing the format from its leading bytes.
pub fn decode(data: &[u8]) -> Result<Image> {
    decode_with_limits(data, Limits::default())
}

/// [`decode`] with caller-chosen [`Limits`].
pub fn decode_with_limits(data: &[u8], limits: Limits) -> Result<Image> {
    match ImageFormat::guess(data) {
        Some(ImageFormat::Png) => png::decode(data, limits),
        Some(ImageFormat::Jpeg) => jpeg::decode(data, limits),
        Some(ImageFormat::WebP) => webp::decode(data, limits),
        None => Err(Error::unsupported(
            "image",
            "no PNG, JPEG or WebP signature at the start of the data",
        )),
    }
}

/// Dimensions from the header alone, without decoding the pixels.
pub fn info(data: &[u8]) -> Result<Info> {
    match ImageFormat::guess(data) {
        Some(ImageFormat::Png) => png::info(data),
        Some(ImageFormat::Jpeg) => jpeg::info(data),
        Some(ImageFormat::WebP) => webp::info(data),
        None => Err(Error::unsupported(
            "image",
            "no PNG, JPEG or WebP signature at the start of the data",
        )),
    }
}

/// Read and decode the file at `path`.
pub fn open(path: impl AsRef<std::path::Path>) -> Result<Image> {
    let data = std::fs::read(path).map_err(Error::Io)?;
    decode(&data)
}

/// Read the file at `path` and report its header dimensions.
pub fn open_info(path: impl AsRef<std::path::Path>) -> Result<Info> {
    let data = std::fs::read(path).map_err(Error::Io)?;
    info(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_reads_signatures_not_extensions() {
        assert_eq!(
            ImageFormat::guess(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            ImageFormat::guess(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(ImageFormat::Jpeg)
        );
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.push(0);
        assert_eq!(ImageFormat::guess(&webp), Some(ImageFormat::WebP));
        assert_eq!(ImageFormat::guess(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(ImageFormat::guess(b""), None);
    }

    #[test]
    fn sample_conversion_round_trips_through_eight_bits() {
        for v in 0..=255u8 {
            assert_eq!(narrow(widen(v)), v, "{v}");
        }
        assert_eq!(narrow(0), 0);
        assert_eq!(narrow(u16::MAX), 255);
        assert_eq!(narrow(32768), 128);
    }

    #[test]
    fn limits_refuse_before_allocating() {
        let limits = Limits::default();
        assert!(limits.check(1920, 1080).is_ok());
        assert!(limits.check(0, 10).is_err());
        assert!(limits.check(100_000, 100_000).is_err());
    }
}
