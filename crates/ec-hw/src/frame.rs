//! Decoded frames: what comes out of a decoder, and the two ways to get at the
//! pixels — DRM PRIME export (zero copy) and mapped readback.

use std::sync::{Arc, Mutex};

use ec_core::color::{ColorDescription, ContentLight};
use ec_va::sys::{VA_FOURCC_NV12, VA_FOURCC_P010};
use ec_va::{Image, MappedImage, PrimeSurface, Surface};

use crate::error::{Error, Result};
use crate::pool::PooledSurface;

/// A stream's colour metadata, resolved from its own headers: VUI tags for the
/// primaries/transfer/matrix triplet, and — for HEVC — the two HDR prefix SEI
/// messages for the peak the mastering grade and the content itself declared.
/// No container tier here: ec-hw only ever sees the elementary stream, so a
/// caller that also holds the container's tags resolves those over this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour {
    /// Matrix/transfer/range, resolved from the bitstream's own VUI.
    pub description: ColorDescription,
    /// HDR peak: MaxCLL/MaxFALL and the mastering display's luminance, when
    /// the stream carried them (HEVC prefix SEI only).
    pub light: ContentLight,
}

/// The readback image a decoder reuses across frames.
///
/// One `vaCreateImage`/`vaDestroyImage` pair per frame costs ~20 ms at 1080p on
/// this driver — more than the decode — because each one allocates and frees a
/// 3 MB driver buffer. The image is therefore created once per stream and kept
/// here, shared by every frame that stream produces.
pub(crate) type ImageCache = Arc<Mutex<Option<Image>>>;

/// One decoded picture, in display order.
///
/// Holding a frame holds its surface out of the decoder's pool, so a consumer
/// that keeps every frame will starve the decoder — by design: that is the back
/// pressure a zero-copy pipeline needs.
pub struct Frame {
    surface: Arc<PooledSurface>,
    images: ImageCache,
    /// Presentation timestamp carried through from the access unit.
    pub timestamp: i64,
    /// Size the picture is displayed at, after cropping.
    pub display_size: (u32, u32),
    /// Size the picture is coded at (a multiple of the codec's block size).
    pub coded_size: (u32, u32),
    /// Luma bit depth: 8 for NV12 surfaces, 10 for P010.
    pub bit_depth: u8,
    /// This stream's colour metadata, when the codec's decoder resolves one.
    pub colour: Option<Colour>,
}

/// 8-bit planar 4:2:0, the currency edith's software path speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I420 {
    /// Luma plane, `width * height`.
    pub y: Vec<u8>,
    /// Cb plane, `(width + 1) / 2 * ((height + 1) / 2)`.
    pub u: Vec<u8>,
    /// Cr plane, same size as `u`.
    pub v: Vec<u8>,
    /// Width in luma samples.
    pub width: u32,
    /// Height in luma samples.
    pub height: u32,
}

/// 10-bit planar 4:2:0, samples right-aligned in the low 10 bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I420_16 {
    /// Luma plane.
    pub y: Vec<u16>,
    /// Cb plane.
    pub u: Vec<u16>,
    /// Cr plane.
    pub v: Vec<u16>,
    /// Width in luma samples.
    pub width: u32,
    /// Height in luma samples.
    pub height: u32,
}

impl Frame {
    pub(crate) fn new(
        surface: Arc<PooledSurface>,
        images: ImageCache,
        timestamp: i64,
        display_size: (u32, u32),
        coded_size: (u32, u32),
        bit_depth: u8,
        colour: Option<Colour>,
    ) -> Frame {
        Frame {
            surface,
            images,
            timestamp,
            display_size,
            coded_size,
            bit_depth,
            colour,
        }
    }

    /// The surface this frame lives on.
    pub fn surface(&self) -> &Arc<Surface> {
        &self.surface
    }

    /// This frame's colour metadata, when the codec's decoder resolves one.
    pub fn colour(&self) -> Option<Colour> {
        self.colour
    }

    /// Export the surface as DRM PRIME file descriptors.
    ///
    /// This is the path edith's `engine-hw` takes: the fds are imported into
    /// gbm on the other side, so no pixel ever crosses the PCIe bus. The frame
    /// is synchronised first, because an exported fd carries no fence.
    pub fn export_prime(&self) -> Result<PrimeSurface> {
        self.surface.sync()?;
        Ok(self
            .surface
            .export_prime(ec_va::sys::VA_EXPORT_SURFACE_READ_ONLY)?)
    }

    /// Read the frame back as 8-bit I420, truncating a 10-bit surface.
    ///
    /// Truncation is what the `vh_*` C ABI takes today (one byte per sample);
    /// [`Frame::to_i420_16`] is the lossless path for a caller that wants the
    /// ten bits.
    pub fn to_i420(&self) -> Result<I420> {
        let (w, h) = self.display_size;
        let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
        let (w, h) = (w as usize, h as usize);
        let mut out = I420 {
            y: vec![0; w * h],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
            width: self.display_size.0,
            height: self.display_size.1,
        };

        self.with_planes(|mapped| {
            let y_pitch = mapped.pitch(0).unwrap_or(0) as usize;
            let uv_pitch = mapped.pitch(1).unwrap_or(0) as usize;
            let y_plane = mapped
                .plane(0)
                .ok_or_else(|| Error::config("VA image has no luma plane"))?;
            let uv_plane = mapped
                .plane(1)
                .ok_or_else(|| Error::config("VA image has no chroma plane"))?;

            // Mapped VA memory is uncached (write-combining on this driver), where a
            // byte-at-a-time read runs at a few hundred MB/s. Every plane is
            // therefore pulled out one whole row at a time with `copy_from_slice`
            // and unpacked from ordinary memory afterwards: same result, 10x the
            // throughput (27 ms -> 2.6 ms per 1080p frame, measured).
            let mut row_buf = vec![0u8; (uv_pitch.max(y_pitch)).max(w * 2)];
            match self.bit_depth {
                8 => {
                    for row in 0..h {
                        let src = row * y_pitch;
                        out.y[row * w..(row + 1) * w].copy_from_slice(&y_plane[src..src + w]);
                    }
                    for row in 0..ch {
                        let src = row * uv_pitch;
                        let line = &mut row_buf[..cw * 2];
                        line.copy_from_slice(&uv_plane[src..src + cw * 2]);
                        for col in 0..cw {
                            out.u[row * cw + col] = line[col * 2];
                            out.v[row * cw + col] = line[col * 2 + 1];
                        }
                    }
                }
                _ => {
                    // P010 keeps the sample in the *high* bits of each u16, so the
                    // 8-bit value is the high byte — a shift by 8, not by 2.
                    for row in 0..h {
                        let src = row * y_pitch;
                        let line = &mut row_buf[..w * 2];
                        line.copy_from_slice(&y_plane[src..src + w * 2]);
                        for col in 0..w {
                            out.y[row * w + col] = line[col * 2 + 1];
                        }
                    }
                    for row in 0..ch {
                        let src = row * uv_pitch;
                        let line = &mut row_buf[..cw * 4];
                        line.copy_from_slice(&uv_plane[src..src + cw * 4]);
                        for col in 0..cw {
                            out.u[row * cw + col] = line[col * 4 + 1];
                            out.v[row * cw + col] = line[col * 4 + 3];
                        }
                    }
                }
            }
            Ok(out)
        })
    }

    /// Read the frame back as 10-bit I420 (samples in the low 10 bits).
    ///
    /// An 8-bit surface is widened rather than refused, so a caller can hold
    /// one frame type across a mixed-depth timeline.
    pub fn to_i420_16(&self) -> Result<I420_16> {
        let (w, h) = self.display_size;
        let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
        let (w, h) = (w as usize, h as usize);
        let mut out = I420_16 {
            y: vec![0; w * h],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
            width: self.display_size.0,
            height: self.display_size.1,
        };

        self.with_planes(|mapped| {
            let y_pitch = mapped.pitch(0).unwrap_or(0) as usize;
            let uv_pitch = mapped.pitch(1).unwrap_or(0) as usize;
            let y_plane = mapped
                .plane(0)
                .ok_or_else(|| Error::config("VA image has no luma plane"))?;
            let uv_plane = mapped
                .plane(1)
                .ok_or_else(|| Error::config("VA image has no chroma plane"))?;

            // Row at a time, for the reason given in `to_i420`.
            let mut row_buf = vec![0u8; (uv_pitch.max(y_pitch)).max(w * 2)];
            match self.bit_depth {
                8 => {
                    for row in 0..h {
                        let src = row * y_pitch;
                        row_buf[..w].copy_from_slice(&y_plane[src..src + w]);
                        for (dst, &sample) in
                            out.y[row * w..(row + 1) * w].iter_mut().zip(&row_buf[..w])
                        {
                            *dst = u16::from(sample) << 2;
                        }
                    }
                    for row in 0..ch {
                        let src = row * uv_pitch;
                        let line = &mut row_buf[..cw * 2];
                        line.copy_from_slice(&uv_plane[src..src + cw * 2]);
                        for col in 0..cw {
                            out.u[row * cw + col] = u16::from(line[col * 2]) << 2;
                            out.v[row * cw + col] = u16::from(line[col * 2 + 1]) << 2;
                        }
                    }
                }
                _ => {
                    // P010 is 16-bit little endian with the sample in the top ten
                    // bits; the low six are zero padding.
                    for row in 0..h {
                        let src = row * y_pitch;
                        let line = &mut row_buf[..w * 2];
                        line.copy_from_slice(&y_plane[src..src + w * 2]);
                        for col in 0..w {
                            out.y[row * w + col] =
                                u16::from_le_bytes([line[col * 2], line[col * 2 + 1]]) >> 6;
                        }
                    }
                    for row in 0..ch {
                        let src = row * uv_pitch;
                        let line = &mut row_buf[..cw * 4];
                        line.copy_from_slice(&uv_plane[src..src + cw * 4]);
                        for col in 0..cw {
                            out.u[row * cw + col] =
                                u16::from_le_bytes([line[col * 4], line[col * 4 + 1]]) >> 6;
                            out.v[row * cw + col] =
                                u16::from_le_bytes([line[col * 4 + 2], line[col * 4 + 3]]) >> 6;
                        }
                    }
                }
            }
            Ok(out)
        })
    }

    /// Map the surface's pixels and hand the planes to `read`.
    ///
    /// `vaGetImage` into an image kept for the life of the stream, *not*
    /// `vaDeriveImage`. Deriving looks cheaper — it is a view rather than a
    /// copy — and it does return the same pixels here (checked row for row),
    /// but the view is device memory across the PCIe bar: reading a 1080p frame
    /// through it costs 48 ms against 10 ms for a driver-side copy into system
    /// memory followed by a normal read (measured, 719-frame loop).
    fn with_planes<R>(&self, read: impl FnOnce(&MappedImage<'_>) -> Result<R>) -> Result<R> {
        self.surface.sync()?;
        let fourcc = if self.bit_depth > 8 {
            VA_FOURCC_P010
        } else {
            VA_FOURCC_NV12
        };
        let (w, h) = self.coded_size;

        let mut slot = match self.images.lock() {
            Ok(slot) => slot,
            // The lock guards a cached image, not an invariant: a panicking
            // reader leaves it perfectly usable.
            Err(poisoned) => poisoned.into_inner(),
        };
        let reusable = slot
            .as_ref()
            .is_some_and(|image| image.fourcc() == fourcc && image.size() == (w, h));
        if !reusable {
            *slot = Some(Image::create(self.surface.display(), fourcc, w, h)?);
        }
        let image = slot
            .as_mut()
            .ok_or_else(|| Error::config("no readback image"))?;
        self.surface.read_into(image, 0, 0, w, h)?;
        let mapped = image.map()?;
        read(&mapped)
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("surface", &self.surface.id())
            .field("timestamp", &self.timestamp)
            .field("display_size", &self.display_size)
            .field("bit_depth", &self.bit_depth)
            .finish()
    }
}

/// Deinterleave an NV12 chroma plane into I420 U and V planes.
///
/// The same conversion the software path in edith expects on the way out of a
/// hardware frame; kept as a free function because an encoder's input path
/// needs it in reverse.
pub fn nv12_to_i420(
    y_plane: &[u8],
    y_pitch: usize,
    uv_plane: &[u8],
    uv_pitch: usize,
    width: usize,
    height: usize,
) -> I420 {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut out = I420 {
        y: vec![0; width * height],
        u: vec![0; cw * ch],
        v: vec![0; cw * ch],
        width: width as u32,
        height: height as u32,
    };
    for row in 0..height {
        let src = row * y_pitch;
        if src + width > y_plane.len() {
            break;
        }
        out.y[row * width..(row + 1) * width].copy_from_slice(&y_plane[src..src + width]);
    }
    for row in 0..ch {
        let src = row * uv_pitch;
        if src + cw * 2 > uv_plane.len() {
            break;
        }
        for col in 0..cw {
            out.u[row * cw + col] = uv_plane[src + col * 2];
            out.v[row * cw + col] = uv_plane[src + col * 2 + 1];
        }
    }
    out
}

/// Interleave I420 chroma into an NV12 buffer, for the encoder input path.
pub fn i420_to_nv12(
    src: &I420,
    y_out: &mut [u8],
    y_pitch: usize,
    uv_out: &mut [u8],
    uv_pitch: usize,
) {
    let (w, h) = (src.width as usize, src.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for row in 0..h {
        let dst = row * y_pitch;
        if dst + w > y_out.len() {
            break;
        }
        y_out[dst..dst + w].copy_from_slice(&src.y[row * w..row * w + w]);
    }
    for row in 0..ch {
        let dst = row * uv_pitch;
        if dst + cw * 2 > uv_out.len() {
            break;
        }
        for col in 0..cw {
            uv_out[dst + col * 2] = src.u[row * cw + col];
            uv_out[dst + col * 2 + 1] = src.v[row * cw + col];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_round_trips_through_i420() {
        let (w, h) = (6usize, 4usize);
        let (y_pitch, uv_pitch) = (8usize, 8usize);
        let mut y = vec![0u8; y_pitch * h];
        let mut uv = vec![0u8; uv_pitch * h / 2];
        for row in 0..h {
            for col in 0..w {
                y[row * y_pitch + col] = (row * w + col) as u8;
            }
        }
        for row in 0..h / 2 {
            for col in 0..w {
                uv[row * uv_pitch + col] = (100 + row * w + col) as u8;
            }
        }

        let i420 = nv12_to_i420(&y, y_pitch, &uv, uv_pitch, w, h);
        assert_eq!(i420.y.len(), w * h);
        assert_eq!(i420.u.len(), (w / 2) * (h / 2));
        assert_eq!(i420.y[0], 0);
        assert_eq!(i420.y[w], w as u8);
        // U takes the even bytes of the interleaved plane, V the odd ones.
        assert_eq!(i420.u[0], 100);
        assert_eq!(i420.v[0], 101);
        assert_eq!(i420.u[1], 102);

        let mut y2 = vec![0u8; y_pitch * h];
        let mut uv2 = vec![0u8; uv_pitch * h / 2];
        i420_to_nv12(&i420, &mut y2, y_pitch, &mut uv2, uv_pitch);
        let back = nv12_to_i420(&y2, y_pitch, &uv2, uv_pitch, w, h);
        assert_eq!(back, i420);
    }
}
