//! Compatibility shim carrying the `image` name and version, over
//! [`ec_image`].
//!
//! Only the surface a caller in this family actually consumes is here. edith's
//! engine uses exactly:
//!
//! ```text
//! image::ImageReader::open(path)?.with_guessed_format()?.decode()?.to_rgb8()
//! image::ImageReader::open(path)?.with_guessed_format()?.into_dimensions()?
//! image::open(path)?.to_rgb8().get_pixel(x, y).0        (engine test suite)
//! ```
//!
//! so `ImageReader`, `DynamicImage` and the three 8-bit buffer types are the
//! whole shim. Anything else the upstream crate exposes is deliberately
//! absent, so a new use shows up as a compile error rather than as silently
//! different behaviour.
//!
//! **Not covered, deliberately:** the *app* crate's `RgbaImage::from_raw` /
//! `Frame::new` pair feeds `gpui::RenderImage`, which is typed against the
//! real `image` crate gpui itself depends on. Those two call sites cannot be
//! served by a shim — a `[patch.crates-io]` swap would retype gpui's own
//! dependency as well. The engine must therefore take this crate by rename
//! (`image = { package = "ec-image-shim", path = ... }`) or the app must keep
//! the real crate; the swap is not a blanket patch. See the S40 report.
//!
//! Behaviour differences from `image` 0.25.10, all consequences of the decoder
//! underneath and none of them reached by the surface above:
//!
//! - Only PNG, JPEG and WebP decode — the formats edith's `is_image` admits.
//! - EXIF orientation is parsed and exposed, not applied, exactly as upstream.
//! - JPEG samples may differ from upstream's by up to 4 counts of 255, an
//!   IDCT-rounding difference the format explicitly permits; PNG and lossless
//!   WebP are pixel-exact.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;
use std::path::Path;

#[cfg(feature = "gpui")]
pub mod gpui;
// The names gpui writes as `image::...`; upstream has them at the crate root,
// so the feature's module is flattened into it.
#[cfg(feature = "gpui")]
pub use gpui::{
    AnimationDecoder, Delay, Frame, Frames, ImageDecoder, codecs, guess_format,
    load_from_memory_with_format,
};
/// Upstream's one generic buffer type. Every caller behind the `gpui` feature
/// instantiates it at RGBA8, so that is what the name means here.
#[cfg(feature = "gpui")]
pub type ImageBuffer = RgbaImage;

/// Failure of a decode, as `image::ImageError`.
#[derive(Debug)]
pub enum ImageError {
    /// The bytes are not a valid image of their format.
    Decoding(String),
    /// The format, or something in it, is not supported.
    Unsupported(String),
    /// Underlying I/O failure.
    IoError(std::io::Error),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Decoding(m) => write!(f, "Format error decoding image: {m}"),
            ImageError::Unsupported(m) => write!(f, "The image format is not supported: {m}"),
            ImageError::IoError(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImageError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> ImageError {
        ImageError::IoError(e)
    }
}

impl From<ec_image::Error> for ImageError {
    fn from(e: ec_image::Error) -> ImageError {
        match e {
            ec_image::Error::Io(e) => ImageError::IoError(e),
            ec_image::Error::Unsupported { what, why } => {
                ImageError::Unsupported(format!("{what}: {why}"))
            }
            other => ImageError::Decoding(other.to_string()),
        }
    }
}

/// The result of a decode, as `image::ImageResult`.
pub type ImageResult<T> = Result<T, ImageError>;

/// The formats this build decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    WebP,
    /// GIF.
    #[cfg(feature = "gpui")]
    Gif,
    /// Windows bitmap.
    #[cfg(feature = "gpui")]
    Bmp,
    /// TIFF.
    #[cfg(feature = "gpui")]
    Tiff,
}

/// One 8-bit greyscale pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Luma(pub [u8; 1]);
/// One 8-bit R,G,B pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);
/// One 8-bit R,G,B,A pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

/// Generates the three concrete buffer types the surface uses. Upstream has
/// one generic `ImageBuffer`; three concrete types cover every consumed call
/// without dragging a pixel-trait hierarchy in behind them.
macro_rules! buffer {
    ($name:ident, $pixel:ident, $channels:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name {
            width: u32,
            height: u32,
            data: Vec<u8>,
        }

        impl $name {
            /// A buffer over `data`, or `None` when it is not
            #[doc = concat!(stringify!($channels), " bytes per pixel.")]
            pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Option<$name> {
                let needed = (width as usize)
                    .checked_mul(height as usize)?
                    .checked_mul($channels)?;
                (data.len() >= needed).then_some($name {
                    width,
                    height,
                    data,
                })
            }

            /// `(width, height)`.
            pub fn dimensions(&self) -> (u32, u32) {
                (self.width, self.height)
            }

            /// Width in pixels.
            pub fn width(&self) -> u32 {
                self.width
            }

            /// Height in pixels.
            pub fn height(&self) -> u32 {
                self.height
            }

            /// The pixel at `(x, y)`; panics when it is outside the image, as
            /// upstream's does.
            pub fn get_pixel(&self, x: u32, y: u32) -> $pixel {
                assert!(
                    x < self.width && y < self.height,
                    "({x}, {y}) is outside a {}x{} image",
                    self.width,
                    self.height
                );
                let at = ((y as usize) * (self.width as usize) + x as usize) * $channels;
                let mut pixel = [0u8; $channels];
                pixel.copy_from_slice(&self.data[at..at + $channels]);
                $pixel(pixel)
            }

            /// The samples, packed.
            pub fn as_raw(&self) -> &Vec<u8> {
                &self.data
            }

            /// The samples, taken by value.
            pub fn into_raw(self) -> Vec<u8> {
                self.data
            }

            /// The samples as a slice.
            pub fn into_vec(self) -> Vec<u8> {
                self.data
            }
        }

        impl std::ops::Deref for $name {
            type Target = [u8];
            fn deref(&self) -> &[u8] {
                &self.data
            }
        }

        impl std::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut [u8] {
                &mut self.data
            }
        }
    };
}

buffer!(GrayImage, Luma, 1, "An 8-bit greyscale image buffer.");
buffer!(RgbImage, Rgb, 3, "An 8-bit RGB image buffer.");
buffer!(RgbaImage, Rgba, 4, "An 8-bit RGBA image buffer.");

/// A decoded image of whichever colour form its file carried.
#[derive(Debug, Clone, PartialEq)]
pub enum DynamicImage {
    /// 8-bit greyscale.
    ImageLuma8(GrayImage),
    /// 8-bit RGB.
    ImageRgb8(RgbImage),
    /// 8-bit RGBA.
    ImageRgba8(RgbaImage),
}

impl DynamicImage {
    fn from_decoded(image: ec_image::Image) -> DynamicImage {
        let (w, h) = (image.width, image.height);
        match &image.pixels {
            ec_image::Pixels::L8(data) => DynamicImage::ImageLuma8(
                GrayImage::from_raw(w, h, data.clone()).expect("plane sized by the decoder"),
            ),
            ec_image::Pixels::Rgb8(data) => DynamicImage::ImageRgb8(
                RgbImage::from_raw(w, h, data.clone()).expect("plane sized by the decoder"),
            ),
            _ if image.pixels.has_alpha() => DynamicImage::ImageRgba8(
                RgbaImage::from_raw(w, h, image.to_rgba8()).expect("plane sized by the decoder"),
            ),
            _ => DynamicImage::ImageRgb8(
                RgbImage::from_raw(w, h, image.to_rgb8()).expect("plane sized by the decoder"),
            ),
        }
    }

    /// `(width, height)`.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            DynamicImage::ImageLuma8(b) => b.dimensions(),
            DynamicImage::ImageRgb8(b) => b.dimensions(),
            DynamicImage::ImageRgba8(b) => b.dimensions(),
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    /// The image as 8-bit RGB, dropping any alpha channel.
    pub fn to_rgb8(&self) -> RgbImage {
        let (w, h) = self.dimensions();
        let data = match self {
            DynamicImage::ImageRgb8(b) => return b.clone(),
            DynamicImage::ImageLuma8(b) => b.data.iter().flat_map(|&l| [l, l, l]).collect(),
            DynamicImage::ImageRgba8(b) => b
                .data
                .chunks_exact(4)
                .flat_map(|p| p[..3].to_vec())
                .collect(),
        };
        RgbImage::from_raw(w, h, data).expect("converted plane")
    }

    /// The image as 8-bit RGBA; opaque where the source had no alpha.
    pub fn to_rgba8(&self) -> RgbaImage {
        let (w, h) = self.dimensions();
        let data = match self {
            DynamicImage::ImageRgba8(b) => return b.clone(),
            DynamicImage::ImageLuma8(b) => b.data.iter().flat_map(|&l| [l, l, l, 255]).collect(),
            DynamicImage::ImageRgb8(b) => b
                .data
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
        };
        RgbaImage::from_raw(w, h, data).expect("converted plane")
    }

    /// The image as 8-bit greyscale, by the same luma weights upstream uses.
    pub fn to_luma8(&self) -> GrayImage {
        let (w, h) = self.dimensions();
        let data = match self {
            DynamicImage::ImageLuma8(b) => return b.clone(),
            _ => self
                .to_rgb8()
                .data
                .chunks_exact(3)
                .map(|p| {
                    let luma =
                        2126 * u32::from(p[0]) + 7152 * u32::from(p[1]) + 722 * u32::from(p[2]);
                    ((luma + 5000) / 10000).min(255) as u8
                })
                .collect(),
        };
        GrayImage::from_raw(w, h, data).expect("converted plane")
    }
}

/// A decoder that guesses its format from the bytes, as `image::ImageReader`.
///
/// Upstream is generic over any `BufRead + Seek`; this reads the whole file
/// once, because every caller in this family hands it a path and then decodes
/// the whole picture anyway.
#[derive(Debug)]
pub struct ImageReader {
    data: Vec<u8>,
    format: Option<ImageFormat>,
}

impl ImageReader {
    /// Open `path` for decoding. The format is not guessed yet.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<ImageReader> {
        Ok(ImageReader {
            data: std::fs::read(path)?,
            format: None,
        })
    }

    /// A reader over bytes already in memory.
    pub fn new(data: Vec<u8>) -> ImageReader {
        ImageReader { data, format: None }
    }

    /// Guess the format from the leading bytes.
    ///
    /// Upstream returns an I/O error here because it may have to read; an
    /// unrecognised signature is not an error at this point in either crate —
    /// it surfaces at `decode`.
    pub fn with_guessed_format(mut self) -> std::io::Result<ImageReader> {
        self.format = format_of(&self.data);
        Ok(self)
    }

    /// The format, once guessed or set.
    pub fn format(&self) -> Option<ImageFormat> {
        self.format
    }

    /// Decode the whole picture.
    pub fn decode(self) -> ImageResult<DynamicImage> {
        Ok(DynamicImage::from_decoded(ec_image::decode(&self.data)?))
    }

    /// The dimensions from the header alone, without decoding the pixels.
    pub fn into_dimensions(self) -> ImageResult<(u32, u32)> {
        let info = ec_image::info(&self.data)?;
        Ok((info.width, info.height))
    }
}

/// The format `data`'s leading bytes name, if this build decodes it.
fn format_of(data: &[u8]) -> Option<ImageFormat> {
    match ec_image::ImageFormat::guess(data)? {
        ec_image::ImageFormat::Png => Some(ImageFormat::Png),
        ec_image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        ec_image::ImageFormat::WebP => Some(ImageFormat::WebP),
        // Only the gpui half of this shim names the formats gpui asks for; a
        // build without it has no variant to answer with.
        #[cfg(feature = "gpui")]
        ec_image::ImageFormat::Gif => Some(ImageFormat::Gif),
        #[cfg(not(feature = "gpui"))]
        ec_image::ImageFormat::Gif => None,
        #[cfg(feature = "gpui")]
        ec_image::ImageFormat::Bmp => Some(ImageFormat::Bmp),
        #[cfg(not(feature = "gpui"))]
        ec_image::ImageFormat::Bmp => None,
        #[cfg(feature = "gpui")]
        ec_image::ImageFormat::Tiff => Some(ImageFormat::Tiff),
        #[cfg(not(feature = "gpui"))]
        ec_image::ImageFormat::Tiff => None,
    }
}

/// Decode the file at `path`.
pub fn open(path: impl AsRef<Path>) -> ImageResult<DynamicImage> {
    let data = std::fs::read(path)?;
    load_from_memory(&data)
}

/// Decode bytes already in memory.
pub fn load_from_memory(data: &[u8]) -> ImageResult<DynamicImage> {
    Ok(DynamicImage::from_decoded(ec_image::decode(data)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills")
    }

    /// The exact sequence edith's engine performs, start to finish.
    #[test]
    fn the_engine_call_sequence_works() {
        let path = fixtures().join("rgb8.png");
        if !path.exists() {
            eprintln!("skipped: fixtures/stills not generated");
            return;
        }
        let reader = ImageReader::open(&path).expect("open");
        let rgb = reader
            .with_guessed_format()
            .expect("guess")
            .decode()
            .map_err(|e| format!("{}: {e}", path.display()))
            .expect("decode")
            .to_rgb8();
        let (width, height) = rgb.dimensions();
        assert_eq!((width, height), (320, 240));
        assert_eq!(rgb.len(), 320 * 240 * 3, "Deref reaches the samples");

        let dimensions = ImageReader::open(&path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .expect("dimensions");
        assert_eq!(dimensions, (320, 240));

        let pixel = open(&path).expect("open").to_rgb8().get_pixel(7, 9).0;
        assert_eq!(pixel.len(), 3);
    }

    #[test]
    fn a_missing_file_is_an_io_error_the_caller_can_propagate() {
        let err = ImageReader::open(fixtures().join("nope.png")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let err: ImageError = err.into();
        assert!(matches!(err, ImageError::IoError(_)), "{err}");
    }

    #[test]
    fn a_file_that_is_not_an_image_fails_at_decode_not_at_guess() {
        let reader = ImageReader::new(b"not a picture at all".to_vec())
            .with_guessed_format()
            .expect("guessing never fails on unknown bytes");
        assert_eq!(reader.format(), None);
        assert!(matches!(reader.decode(), Err(ImageError::Unsupported(_))));
    }

    #[test]
    fn alpha_survives_and_grey_stays_grey() {
        let dir = fixtures();
        if !dir.join("rgba8.png").exists() {
            eprintln!("skipped: fixtures/stills not generated");
            return;
        }
        let rgba = open(dir.join("rgba8.png")).unwrap();
        assert!(matches!(rgba, DynamicImage::ImageRgba8(_)));
        let grey = open(dir.join("gray8.png")).unwrap();
        assert!(matches!(grey, DynamicImage::ImageLuma8(_)));
        // to_rgb8 of a grey image repeats the luma into all three channels.
        let rgb = grey.to_rgb8();
        let p = rgb.get_pixel(3, 3).0;
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }
}
