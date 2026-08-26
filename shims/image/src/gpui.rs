//! The half of the `image` surface that exists only because gpui consumes it.
//!
//! edith's window toolkit decodes pictures through the same `image` crate the
//! engine does, and it reaches for animation: `Frame`, `Delay`, the
//! `AnimationDecoder` trait, and the per-format decoder types under
//! `image::codecs`. None of that belongs to a still-image decoder — it is a
//! consumer's shape — so it lives behind the `gpui` cargo feature, the way
//! `chrono` keeps its `serde` impls behind one. A build that does not draw
//! windows never compiles a line of it.
//!
//! Everything here is served by [`ec_image`]; nothing decodes twice.

use std::io::Read;
use std::time::Duration;

use crate::{DynamicImage, ImageError, ImageFormat, ImageResult, RgbaImage};

/// How long one frame of an animation is shown, as a rational number of
/// milliseconds — upstream's `image::Delay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delay {
    numer: u32,
    denom: u32,
}

impl Delay {
    /// A delay of `numer / denom` milliseconds.
    pub fn from_numer_denom_ms(numer: u32, denom: u32) -> Delay {
        Delay { numer, denom }
    }

    /// The delay as the pair it was built from.
    pub fn numer_denom_ms(self) -> (u32, u32) {
        (self.numer, self.denom)
    }
}

impl From<Delay> for Duration {
    fn from(delay: Delay) -> Duration {
        if delay.denom == 0 {
            return Duration::ZERO;
        }
        Duration::from_nanos(u64::from(delay.numer) * 1_000_000 / u64::from(delay.denom))
    }
}

/// One picture of an animation: the whole canvas at that point, and how long
/// it is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    buffer: RgbaImage,
    delay: Delay,
}

impl Frame {
    /// A single frame shown for no particular length of time — what a still
    /// picture becomes when a caller wants frames.
    pub fn new(buffer: RgbaImage) -> Frame {
        Frame {
            buffer,
            delay: Delay::from_numer_denom_ms(0, 1),
        }
    }

    /// A frame with its own delay.
    pub fn from_parts(buffer: RgbaImage, delay: Delay) -> Frame {
        Frame { buffer, delay }
    }

    /// The samples.
    pub fn buffer(&self) -> &RgbaImage {
        &self.buffer
    }

    /// The samples, to be edited in place — callers swap R and B here.
    pub fn buffer_mut(&mut self) -> &mut RgbaImage {
        &mut self.buffer
    }

    /// How long this frame is shown.
    pub fn delay(&self) -> Delay {
        self.delay
    }
}

/// The frames of an animation, as an iterator — upstream's `image::Frames`.
///
/// Upstream streams them; these decoders composite the whole animation before
/// the first frame is handed out, because every format we serve carries frames
/// that reference their predecessors anyway.
pub struct Frames {
    frames: std::vec::IntoIter<ImageResult<Frame>>,
}

impl Iterator for Frames {
    type Item = ImageResult<Frame>;

    fn next(&mut self) -> Option<ImageResult<Frame>> {
        self.frames.next()
    }
}

impl Frames {
    fn of(frames: Vec<ImageResult<Frame>>) -> Frames {
        Frames {
            frames: frames.into_iter(),
        }
    }
}

/// A decoder that can yield more than one picture.
pub trait AnimationDecoder {
    /// Every frame, composited.
    fn into_frames(self) -> Frames;
}

/// A decoder of one still picture, as far as [`DynamicImage::from_decoder`] is
/// concerned.
pub trait ImageDecoder {
    /// Decode the whole picture.
    fn into_image(self) -> ImageResult<DynamicImage>;
}

impl DynamicImage {
    /// The picture a decoder holds.
    pub fn from_decoder(decoder: impl ImageDecoder) -> ImageResult<DynamicImage> {
        decoder.into_image()
    }

    /// The image as 8-bit RGBA, by value.
    pub fn into_rgba8(self) -> RgbaImage {
        match self {
            DynamicImage::ImageRgba8(buffer) => buffer,
            other => other.to_rgba8(),
        }
    }
}

/// The format `data` starts with, as `image::guess_format`.
///
/// Upstream errors on unrecognised bytes rather than returning `None`, and
/// gpui branches on that error, so this keeps the shape.
pub fn guess_format(data: &[u8]) -> ImageResult<ImageFormat> {
    crate::format_of(data).ok_or_else(|| ImageError::Unsupported("unrecognised signature".into()))
}

/// Decode bytes whose format the caller already knows.
///
/// The format argument only *asserts* what the bytes are; every decoder here
/// is chosen by signature, so a mismatch surfaces as a decode error rather
/// than as a wrong picture.
pub fn load_from_memory_with_format(data: &[u8], format: ImageFormat) -> ImageResult<DynamicImage> {
    match crate::format_of(data) {
        Some(found) if found == format => crate::load_from_memory(data),
        Some(found) => Err(ImageError::Decoding(format!(
            "asked for {format:?}, the bytes are {found:?}"
        ))),
        None => Err(ImageError::Unsupported("unrecognised signature".into())),
    }
}

/// Read a decoder's whole input, the way upstream's decoders do before they
/// parse anything.
fn slurp(mut reader: impl Read) -> ImageResult<Vec<u8>> {
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;
    Ok(data)
}

/// Every frame of an animation, as [`Frame`]s.
fn frames_of(data: &[u8]) -> Vec<ImageResult<Frame>> {
    match ec_image::decode_animation(data) {
        Err(e) => vec![Err(e.into())],
        Ok(frames) => frames
            .into_iter()
            .map(|frame| {
                let (w, h) = (frame.image.width, frame.image.height);
                let buffer = RgbaImage::from_raw(w, h, frame.image.to_rgba8())
                    .ok_or_else(|| ImageError::Decoding("frame plane is short".into()))?;
                // `Delay` counts milliseconds; the decoder counts seconds.
                Ok(Frame::from_parts(
                    buffer,
                    Delay::from_numer_denom_ms(
                        frame.delay_num.saturating_mul(1000),
                        frame.delay_den.max(1),
                    ),
                ))
            })
            .collect(),
    }
}

/// The per-format decoder types, under the path upstream puts them.
pub mod codecs {
    /// GIF.
    pub mod gif {
        use super::super::{AnimationDecoder, Frames, ImageDecoder, frames_of, slurp};
        use crate::{DynamicImage, ImageResult};

        /// A GIF decoder over bytes, as `image::codecs::gif::GifDecoder`.
        pub struct GifDecoder {
            data: Vec<u8>,
        }

        impl GifDecoder {
            /// Take the bytes; nothing is parsed until a picture is asked for.
            pub fn new(reader: impl std::io::Read) -> ImageResult<GifDecoder> {
                Ok(GifDecoder {
                    data: slurp(reader)?,
                })
            }
        }

        impl AnimationDecoder for GifDecoder {
            fn into_frames(self) -> Frames {
                Frames::of(frames_of(&self.data))
            }
        }

        impl ImageDecoder for GifDecoder {
            fn into_image(self) -> ImageResult<DynamicImage> {
                crate::load_from_memory(&self.data)
            }
        }
    }

    /// WebP.
    pub mod webp {
        use super::super::{AnimationDecoder, Frames, ImageDecoder, frames_of, slurp};
        use crate::{DynamicImage, ImageResult, Rgba};

        /// A WebP decoder over bytes, as `image::codecs::webp::WebPDecoder`.
        pub struct WebPDecoder {
            data: Vec<u8>,
        }

        impl WebPDecoder {
            /// Take the bytes; nothing is parsed until a picture is asked for.
            pub fn new(reader: impl std::io::Read) -> ImageResult<WebPDecoder> {
                Ok(WebPDecoder {
                    data: slurp(reader)?,
                })
            }

            /// Whether the file carries more than one picture.
            pub fn has_animation(&self) -> bool {
                ec_image::webp::is_animated(&self.data)
            }

            /// The colour an animation's transparent areas are composited
            /// over.
            ///
            /// Frames come out with their own alpha rather than flattened
            /// onto a colour, which is what a caller asking for a fully
            /// transparent background wants; any other colour is refused
            /// rather than silently ignored.
            pub fn set_background_color(&mut self, colour: Rgba) -> ImageResult<()> {
                match colour.0 {
                    [_, _, _, 0] => Ok(()),
                    _ => Err(crate::ImageError::Unsupported(
                        "compositing animation frames onto an opaque background".into(),
                    )),
                }
            }
        }

        impl AnimationDecoder for WebPDecoder {
            fn into_frames(self) -> Frames {
                Frames::of(frames_of(&self.data))
            }
        }

        impl ImageDecoder for WebPDecoder {
            fn into_image(self) -> ImageResult<DynamicImage> {
                crate::load_from_memory(&self.data)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImageBuffer, Rgba};
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills")
    }

    fn bytes(name: &str) -> Option<Vec<u8>> {
        let path = fixtures().join(name);
        if !path.exists() {
            eprintln!("skipped: {name} not generated; run scripts/gen-still-fixtures.sh");
            return None;
        }
        Some(std::fs::read(path).expect("read fixture"))
    }

    /// A delay is milliseconds, whatever it is written over.
    #[test]
    fn a_delay_converts_to_the_duration_it_names() {
        let tenth = Delay::from_numer_denom_ms(100, 1);
        assert_eq!(Duration::from(tenth), Duration::from_millis(100));
        // A 1/100 s GIF tick, written as 10/1 ms.
        let tick = Delay::from_numer_denom_ms(10, 1);
        assert_eq!(Duration::from(tick), Duration::from_millis(10));
        // Thirds of a millisecond survive as nanoseconds rather than rounding
        // to zero, which is what a 30 fps animation needs.
        let third = Delay::from_numer_denom_ms(1, 3);
        assert_eq!(Duration::from(third), Duration::from_nanos(333_333));
        assert_eq!(
            Duration::from(Delay::from_numer_denom_ms(1, 0)),
            Duration::ZERO
        );
    }

    /// gpui's animated-GIF path, verbatim: guess the format, take the frames,
    /// swap each frame's red and blue in place, and read the delays back.
    #[test]
    fn the_gpui_gif_sequence_works() {
        let Some(data) = bytes("animated.gif") else {
            return;
        };
        assert_eq!(
            guess_format(&data).expect("a GIF signature"),
            ImageFormat::Gif
        );

        let decoder = codecs::gif::GifDecoder::new(Cursor::new(&data)).expect("decoder");
        let mut frames = Vec::new();
        for frame in decoder.into_frames() {
            let mut frame = frame.expect("frame");
            for pixel in frame.buffer_mut().chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            frames.push(frame);
        }
        assert!(
            frames.len() > 1,
            "an animation decoded to {} frame(s)",
            frames.len()
        );
        let (width, height) = frames[0].buffer().dimensions();
        for frame in &frames {
            assert_eq!(
                frame.buffer().dimensions(),
                (width, height),
                "frames are composited onto one canvas"
            );
            assert_eq!(
                frame.buffer().as_raw().len(),
                (width as usize) * (height as usize) * 4
            );
            assert!(
                Duration::from(frame.delay()) > Duration::ZERO,
                "a frame with no delay would never advance"
            );
        }
    }

    /// gpui's WebP path: a still goes through the decoder as one picture, an
    /// animation through the frame iterator.
    #[test]
    fn the_gpui_webp_sequence_works() {
        let Some(still) = bytes("lossless-alpha.webp") else {
            return;
        };
        let mut decoder = codecs::webp::WebPDecoder::new(Cursor::new(&still)).expect("decoder");
        assert!(!decoder.has_animation());
        assert!(decoder.set_background_color(Rgba([0, 0, 0, 0])).is_ok());
        let mut data = DynamicImage::from_decoder(decoder)
            .expect("decode")
            .into_rgba8();
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let frames = [Frame::new(data)];
        assert_eq!(
            Duration::from(frames[0].delay()),
            Duration::ZERO,
            "a still has no delay"
        );

        let Some(animated) = bytes("animated.webp") else {
            return;
        };
        let decoder = codecs::webp::WebPDecoder::new(Cursor::new(&animated)).expect("decoder");
        assert!(decoder.has_animation(), "an ANIM file read as a still");
        let frames: Vec<Frame> = decoder
            .into_frames()
            .map(|frame| frame.expect("frame"))
            .collect();
        assert!(
            frames.len() > 1,
            "an animation decoded to {} frame(s)",
            frames.len()
        );
    }

    /// gpui's still path for everything else, and the buffer it builds by hand
    /// for rendered SVGs.
    #[test]
    fn the_gpui_still_and_svg_paths_work() {
        let Some(data) = bytes("rgb8.png") else {
            return;
        };
        let format = guess_format(&data).expect("a PNG signature");
        assert_eq!(format, ImageFormat::Png);
        let picture = load_from_memory_with_format(&data, format)
            .expect("decode")
            .into_rgba8();
        assert_eq!(picture.dimensions(), (320, 240));

        // The SVG renderer hands over its own samples; gpui wraps them and
        // swaps the channel order in place.
        let mut buffer = ImageBuffer::from_raw(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("buffer");
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        assert_eq!(buffer.as_raw(), &vec![3, 2, 1, 4, 7, 6, 5, 8]);
        let frame = Frame::new(buffer);
        assert_eq!(frame.buffer().dimensions(), (2, 1));
    }

    /// Bytes of no known format are an error here, not a `None` -- gpui
    /// branches on the error to try its SVG renderer instead.
    #[test]
    fn unknown_bytes_are_an_error_so_the_caller_can_fall_through() {
        assert!(guess_format(b"<svg xmlns=...>").is_err());
        let Some(data) = bytes("rgb8.png") else {
            return;
        };
        // Asserting the wrong format is a decode error, never a wrong picture.
        assert!(load_from_memory_with_format(&data, ImageFormat::Jpeg).is_err());
    }
}
