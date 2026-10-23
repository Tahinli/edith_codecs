//! Pixel transforms for packed 8-bit buffers: quarter turns, half turns and
//! mirrors, and the EXIF orientation that combines them.
//!
//! Buffers are row-major, top row first, with no padding: `width * height *
//! bytes_per_pixel` bytes. A turn of a `w x h` image is `h x w`. Every function
//! returns a new buffer; the input is never modified.

use ec_core::{Error, Result};

/// The packed 8-bit pixel layouts the transforms accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Three bytes per pixel: R, G, B.
    Rgb8,
    /// Four bytes per pixel: R, G, B, A.
    Rgba8,
}

impl Layout {
    /// Bytes in one pixel.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Layout::Rgb8 => 3,
            Layout::Rgba8 => 4,
        }
    }
}

/// Turn clockwise by 90 degrees. The result is `height x width`.
pub fn rotate90(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let (w, h) = dims(layout, width, height, pixels)?;
    Ok(map(layout, w, pixels, h, w, |x, y| (y, h - 1 - x)))
}

/// Turn by 180 degrees. The result is `width x height`.
pub fn rotate180(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let (w, h) = dims(layout, width, height, pixels)?;
    Ok(map(layout, w, pixels, w, h, |x, y| (w - 1 - x, h - 1 - y)))
}

/// Turn counter-clockwise by 90 degrees (clockwise by 270). The result is `height x width`.
pub fn rotate270(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let (w, h) = dims(layout, width, height, pixels)?;
    Ok(map(layout, w, pixels, h, w, |x, y| (w - 1 - y, x)))
}

/// Mirror left to right. Dimensions are unchanged.
pub fn flip_horizontal(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let (w, h) = dims(layout, width, height, pixels)?;
    Ok(map(layout, w, pixels, w, h, |x, y| (w - 1 - x, y)))
}

/// Mirror top to bottom. Dimensions are unchanged.
pub fn flip_vertical(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let (w, h) = dims(layout, width, height, pixels)?;
    Ok(map(layout, w, pixels, w, h, |x, y| (x, h - 1 - y)))
}

/// Apply an EXIF orientation (1..=8) so the pixels are upright.
///
/// Returns `(width, height, pixels)` of the upright image. Orientation 1 copies
/// the input unchanged. The mapping is the one `image` applies (its
/// `DynamicImage::apply_orientation`), which follows the EXIF 2.3 table.
pub fn apply_orientation(
    layout: Layout,
    width: u32,
    height: u32,
    pixels: &[u8],
    orientation: u8,
) -> Result<(u32, u32, Vec<u8>)> {
    match orientation {
        1 => Ok((width, height, pixels.to_vec())),
        2 => Ok((
            width,
            height,
            flip_horizontal(layout, width, height, pixels)?,
        )),
        3 => Ok((width, height, rotate180(layout, width, height, pixels)?)),
        4 => Ok((width, height, flip_vertical(layout, width, height, pixels)?)),
        5 => {
            let t = rotate90(layout, width, height, pixels)?;
            Ok((height, width, flip_horizontal(layout, height, width, &t)?))
        }
        6 => Ok((height, width, rotate90(layout, width, height, pixels)?)),
        7 => {
            let t = rotate270(layout, width, height, pixels)?;
            Ok((height, width, flip_horizontal(layout, height, width, &t)?))
        }
        8 => Ok((height, width, rotate270(layout, width, height, pixels)?)),
        other => Err(Error::unsupported(
            "EXIF orientation",
            format!("{other} is not one of 1..=8"),
        )),
    }
}

/// Validate a buffer against its dimensions, returning the dimensions as `usize`.
fn dims(layout: Layout, width: u32, height: u32, pixels: &[u8]) -> Result<(usize, usize)> {
    let (w, h) = (width as usize, height as usize);
    let need = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(layout.bytes_per_pixel()))
        .ok_or_else(|| Error::unsupported("transform", "image size overflows"))?;
    if pixels.len() != need {
        return Err(Error::unsupported(
            "transform",
            format!("{} bytes for {width}x{height} (need {need})", pixels.len()),
        ));
    }
    Ok((w, h))
}

/// Build a `dst_w x dst_h` buffer where destination pixel `(x, y)` is copied from
/// source pixel `src(x, y)`. The caller has validated `pixels` against `w x h`.
fn map(
    layout: Layout,
    w: usize,
    pixels: &[u8],
    dst_w: usize,
    dst_h: usize,
    src: impl Fn(usize, usize) -> (usize, usize),
) -> Vec<u8> {
    let bpp = layout.bytes_per_pixel();
    let mut out = Vec::with_capacity(dst_w * dst_h * bpp);
    for y in 0..dst_h {
        for x in 0..dst_w {
            let (sx, sy) = src(x, y);
            let at = (sy * w + sx) * bpp;
            out.extend_from_slice(&pixels[at..at + bpp]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3x2 RGB image whose pixel values spell their position: 10*y + x in R.
    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                v.extend_from_slice(&[10 * y + x, 0, 0]);
            }
        }
        v
    }

    #[test]
    fn quarter_turn_moves_the_top_left_pixel_to_the_top_right() {
        let r = rotate90(Layout::Rgb8, 3, 2, &sample()).unwrap();
        // 2 wide, 3 tall: row 0 is the old column 0 read bottom to top.
        let reds: Vec<u8> = r.chunks(3).map(|p| p[0]).collect();
        assert_eq!(reds, vec![10, 0, 11, 1, 12, 2]);
    }

    #[test]
    fn a_full_cycle_is_the_identity() {
        let s = sample();
        let a = rotate90(Layout::Rgb8, 3, 2, &s).unwrap();
        let b = rotate90(Layout::Rgb8, 2, 3, &a).unwrap();
        let c = rotate90(Layout::Rgb8, 3, 2, &b).unwrap();
        let d = rotate90(Layout::Rgb8, 2, 3, &c).unwrap();
        assert_eq!(d, s);
        let half = rotate180(Layout::Rgb8, 3, 2, &s).unwrap();
        assert_eq!(rotate180(Layout::Rgb8, 3, 2, &half).unwrap(), s);
        assert_eq!(
            rotate270(Layout::Rgb8, 3, 2, &s).unwrap(),
            rotate90(Layout::Rgb8, 3, 2, &half).unwrap()
        );
    }

    #[test]
    fn mirrors_keep_dimensions_and_are_involutions() {
        let s = sample();
        let h = flip_horizontal(Layout::Rgb8, 3, 2, &s).unwrap();
        assert_eq!(h[0], 2);
        assert_eq!(flip_horizontal(Layout::Rgb8, 3, 2, &h).unwrap(), s);
        let v = flip_vertical(Layout::Rgba8, 1, 2, &[1, 1, 1, 1, 2, 2, 2, 2]).unwrap();
        assert_eq!(v, vec![2, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn every_orientation_gives_the_right_size_and_is_refused_outside_1_to_8() {
        let s = sample();
        for o in 1..=8u8 {
            let (w, h, px) = apply_orientation(Layout::Rgb8, 3, 2, &s, o).unwrap();
            assert_eq!(px.len(), (w * h * 3) as usize);
            let expect = if o >= 5 { (2, 3) } else { (3, 2) };
            assert_eq!((w, h), expect, "orientation {o}");
        }
        assert!(apply_orientation(Layout::Rgb8, 3, 2, &s, 0).is_err());
        assert!(apply_orientation(Layout::Rgb8, 3, 2, &s, 9).is_err());
    }

    #[test]
    fn a_mismatched_buffer_is_refused() {
        assert!(rotate90(Layout::Rgba8, 3, 2, &sample()).is_err());
    }
}
