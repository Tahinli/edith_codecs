//! Decoded frames: what comes out of a decoder, and the two ways to get at the
//! pixels — DRM PRIME export (zero copy) and mapped readback.

use std::rc::Rc;

use ec_core::color::{ColorDescription, ContentLight};
use ec_va::va::{VA_FOURCC_NV12, VA_FOURCC_P010};
use ec_va::{Display, Image, PrimeSurface, Surface, SurfaceExt};

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

/// One decoded picture, in display order.
///
/// Holding a frame holds its surface out of the decoder's pool, so a consumer
/// that keeps every frame will starve the decoder — by design: that is the back
/// pressure a zero-copy pipeline needs.
pub struct Frame {
    surface: Rc<PooledSurface>,
    display: Rc<Display>,
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
    /// The GPU reset epoch this frame was decoded in.
    epoch: u64,
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
    /// Luma bit depth: always 10 for this format.
    pub bit_depth: u8,
}

impl Frame {
    pub(crate) fn new(
        surface: Rc<PooledSurface>,
        display: Rc<Display>,
        timestamp: i64,
        display_size: (u32, u32),
        coded_size: (u32, u32),
        bit_depth: u8,
        colour: Option<Colour>,
    ) -> Frame {
        Frame {
            surface,
            display,
            timestamp,
            display_size,
            coded_size,
            bit_depth,
            colour,
            epoch: crate::reset::gpu_reset_epoch(),
        }
    }

    /// Refuse to touch the surface of a context the GPU has since reset.
    fn check_epoch(&self) -> Result<()> {
        if crate::reset::gpu_reset_epoch() == self.epoch {
            Ok(())
        } else {
            Err(Error::hw_reset(
                "the GPU was reset after this frame was decoded; its surface is lost",
            ))
        }
    }

    /// The surface this frame lives on.
    pub fn surface(&self) -> &Rc<Surface> {
        &self.surface
    }

    /// The display the surface was created on.
    pub(crate) fn display(&self) -> &Rc<Display> {
        &self.display
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
        self.check_epoch()?;
        crate::reset::sync_watched(&self.surface)?;
        Ok(self.surface.export()?)
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

        self.with_planes(|image| {
            let (y_plane, y_pitch, uv_plane, uv_pitch) = planes_of(image)?;
            fill_i420(
                &mut out,
                self.bit_depth,
                y_plane,
                y_pitch,
                uv_plane,
                uv_pitch,
            )?;
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
            bit_depth: 10,
        };

        self.with_planes(|image| {
            let (y_plane, y_pitch, uv_plane, uv_pitch) = planes_of(image)?;
            fill_i420_16(
                &mut out,
                self.bit_depth,
                y_plane,
                y_pitch,
                uv_plane,
                uv_pitch,
            )?;
            Ok(out)
        })
    }

    /// Map the surface's pixels and hand the planes to `read`.
    ///
    /// `vaGetImage` into a fresh image, *not* `vaDeriveImage`. (cros-libva's
    /// image borrows its surface, so the per-stream image cache of the C-era
    /// layer cannot be kept; one `vaCreateImage` per readback replaces it.)
    /// Deriving looks cheaper — it is a view rather than a
    /// copy — and it does return the same pixels here (checked row for row),
    /// but the view is device memory across the PCIe bar: reading a 1080p frame
    /// through it costs 48 ms against 10 ms for a driver-side copy into system
    /// memory followed by a normal read (measured, 719-frame loop).
    fn with_planes<R>(&self, read: impl FnOnce(&Image<'_>) -> Result<R>) -> Result<R> {
        self.check_epoch()?;
        crate::reset::sync_watched(&self.surface)?;
        let fourcc = if self.bit_depth > 8 {
            VA_FOURCC_P010
        } else {
            VA_FOURCC_NV12
        };
        let (w, h) = self.coded_size;
        let format = ec_va::surface::image_format(&self.display, fourcc)?;
        let surface: &Surface = &self.surface;
        let image = ec_va::surface::create_image(surface, format, (w, h), (w, h))?;
        read(&image)
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

/// Copy NV12/P010 planes into `out`, after validating every plane against it.
fn fill_i420(
    out: &mut I420,
    bit_depth: u8,
    y_plane: &[u8],
    y_pitch: usize,
    uv_plane: &[u8],
    uv_pitch: usize,
) -> Result<()> {
    let (w, h) = (out.width as usize, out.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let bytes = if bit_depth == 8 { 1 } else { 2 };
    check_plane("luma", y_plane.len(), y_pitch, h, w * bytes)?;
    check_plane("chroma", uv_plane.len(), uv_pitch, ch, cw * 2 * bytes)?;
    let mut row_buf = vec![0u8; uv_pitch.max(y_pitch).max(w * 2)];
    match bit_depth {
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
    Ok(())
}

/// Copy NV12/P010 planes into `out`, after validating every plane against it.
fn fill_i420_16(
    out: &mut I420_16,
    bit_depth: u8,
    y_plane: &[u8],
    y_pitch: usize,
    uv_plane: &[u8],
    uv_pitch: usize,
) -> Result<()> {
    let (w, h) = (out.width as usize, out.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let bytes = if bit_depth == 8 { 1 } else { 2 };
    check_plane("luma", y_plane.len(), y_pitch, h, w * bytes)?;
    check_plane("chroma", uv_plane.len(), uv_pitch, ch, cw * 2 * bytes)?;
    let mut row_buf = vec![0u8; uv_pitch.max(y_pitch).max(w * 2)];
    match bit_depth {
        8 => {
            for row in 0..h {
                let src = row * y_pitch;
                row_buf[..w].copy_from_slice(&y_plane[src..src + w]);
                for (dst, &sample) in out.y[row * w..(row + 1) * w].iter_mut().zip(&row_buf[..w]) {
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
    Ok(())
}

/// Luma and chroma planes of a read-back NV12/P010 image, with their pitches.
fn planes_of<'a>(image: &'a Image<'_>) -> Result<(&'a [u8], usize, &'a [u8], usize)> {
    let data: &[u8] = image.as_ref();
    let va = image.image();
    let plane = |i: usize| -> Result<&[u8]> {
        let off = *va
            .offsets
            .get(i)
            .ok_or_else(|| Error::config("VA image has no such plane"))? as usize;
        data.get(off..)
            .ok_or_else(|| Error::config("VA image plane lies outside the mapping"))
    };
    let y_plane = plane(0)?;
    let uv_plane = plane(1)?;
    let y_pitch = va.pitches[0] as usize;
    let uv_pitch = va.pitches[1] as usize;
    Ok((y_plane, y_pitch, uv_plane, uv_pitch))
}

/// Check that `rows` rows of `row_bytes` bytes at `pitch` fit in a plane of
/// `len` bytes. A lost or half-initialised GPU context can hand back a short
/// or odd mapping; this turns what would be a slice panic into an error.
fn check_plane(name: &str, len: usize, pitch: usize, rows: usize, row_bytes: usize) -> Result<()> {
    if rows == 0 || row_bytes == 0 {
        return Ok(());
    }
    let need = (rows - 1)
        .checked_mul(pitch)
        .and_then(|n| n.checked_add(row_bytes));
    match need {
        Some(need) if pitch >= row_bytes && need <= len => Ok(()),
        _ => Err(Error::config(format!(
            "VA image {name} plane does not cover the frame: {rows} rows x {row_bytes} bytes at pitch {pitch}, mapped {len} bytes"
        ))),
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

    fn blank(w: u32, h: u32) -> I420 {
        I420 {
            y: vec![0; (w * h) as usize],
            u: vec![0; (w.div_ceil(2) * h.div_ceil(2)) as usize],
            v: vec![0; (w.div_ceil(2) * h.div_ceil(2)) as usize],
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_plane_that_covers_the_frame_passes() {
        // 4 rows of 6 bytes at pitch 8: the last row needs no padding.
        assert!(check_plane("luma", 3 * 8 + 6, 8, 4, 6).is_ok());
        assert!(check_plane("luma", 4 * 8, 8, 4, 6).is_ok());
        assert!(check_plane("luma", 0, 8, 0, 6).is_ok());
    }

    #[test]
    fn a_short_or_odd_plane_is_an_error_not_a_panic() {
        assert!(check_plane("luma", 3 * 8 + 5, 8, 4, 6).is_err());
        assert!(check_plane("luma", 0, 8, 1, 6).is_err());
        // Pitch narrower than a row.
        assert!(check_plane("luma", 1000, 4, 2, 6).is_err());
        assert!(check_plane("luma", 1000, 0, 2, 6).is_err());
        // Arithmetic that would overflow.
        assert!(check_plane("luma", usize::MAX, usize::MAX, 3, 6).is_err());
    }

    #[test]
    fn short_mappings_are_refused_on_every_path() {
        let (w, h) = (6u32, 4u32);
        let y = vec![0u8; 8 * 4];
        let uv = vec![0u8; 8 * 2];
        for depth in [8u8, 10] {
            let bytes = if depth == 8 { 1 } else { 2 };
            let (yp, uvp) = (8 * bytes, 8 * bytes);
            let mut out8 = blank(w, h);
            let mut out16 = I420_16 {
                y: vec![0; 24],
                u: vec![0; 6],
                v: vec![0; 6],
                width: w,
                height: h,
                bit_depth: 10,
            };
            // Mapped one row short of a full luma plane, and a chroma plane cut
            // in half: what a lost context can leave behind.
            let y_full = vec![0u8; yp * 4];
            let uv_full = vec![0u8; uvp * 2];
            assert!(fill_i420(&mut out8, depth, &y_full, yp, &uv_full, uvp).is_ok());
            assert!(fill_i420_16(&mut out16, depth, &y_full, yp, &uv_full, uvp).is_ok());
            let short_y = &y_full[..yp * 3];
            let short_uv = &uv_full[..uvp / 2];
            assert!(fill_i420(&mut out8, depth, short_y, yp, &uv_full, uvp).is_err());
            assert!(fill_i420(&mut out8, depth, &y_full, yp, short_uv, uvp).is_err());
            assert!(fill_i420_16(&mut out16, depth, short_y, yp, &uv_full, uvp).is_err());
            assert!(fill_i420_16(&mut out16, depth, &y_full, yp, short_uv, uvp).is_err());
            // An empty mapping and a zero pitch.
            assert!(fill_i420(&mut out8, depth, &[], yp, &[], uvp).is_err());
            assert!(fill_i420(&mut out8, depth, &y_full, 0, &uv_full, uvp).is_err());
        }
        let _ = (y, uv);
    }

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

    /// `fill_i420_16` is the lossless 10-bit readback: samples that collapse
    /// together under 8-bit truncation (`512 >> 2 == 513 >> 2 == 128`) must
    /// survive distinctly. The test feeds the conversion synthetic P010 planes
    /// directly: cros-libva has no `vaPutImage`, so the GPU upload this test
    /// used to do is not available, and the plane conversion is what it checks.
    #[test]
    fn p010_readback_preserves_ten_bits() {
        let (w, h) = (4u32, 4u32);
        // Synthetic P010: 10-bit samples packed into the high bits of each
        // u16, exactly how the decoder's P010 surfaces carry them. 512 and 513
        // both truncate to 128 in 8-bit, so they prove the 16-bit path is not
        // silently losing the low bits.
        let y_samples = [512u16, 513, 1023, 5];
        let y_pitch = w as usize * 2;
        let uv_pitch = w as usize * 2;
        let mut y_plane = vec![0u8; y_pitch * h as usize];
        for row in 0..h as usize {
            for col in 0..w as usize {
                let sample = y_samples[(row * w as usize + col) % y_samples.len()] << 6;
                let o = row * y_pitch + col * 2;
                y_plane[o..o + 2].copy_from_slice(&sample.to_le_bytes());
            }
        }
        let mut uv_plane = vec![0u8; uv_pitch * h as usize / 2];
        let (u, v) = (512u16 << 6, 1023u16 << 6);
        for row in 0..h as usize / 2 {
            for col in 0..w as usize / 2 {
                let o = row * uv_pitch + col * 4;
                uv_plane[o..o + 2].copy_from_slice(&u.to_le_bytes());
                uv_plane[o + 2..o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }

        let mut i16 = I420_16 {
            y: vec![0; (w * h) as usize],
            u: vec![0; (w * h / 4) as usize],
            v: vec![0; (w * h / 4) as usize],
            width: w,
            height: h,
            bit_depth: 10,
        };
        fill_i420_16(&mut i16, 10, &y_plane, y_pitch, &uv_plane, uv_pitch).expect("16-bit fill");
        let mut i8 = I420 {
            y: vec![0; (w * h) as usize],
            u: vec![0; (w * h / 4) as usize],
            v: vec![0; (w * h / 4) as usize],
            width: w,
            height: h,
        };
        fill_i420(&mut i8, 10, &y_plane, y_pitch, &uv_plane, uv_pitch).expect("8-bit fill");

        assert_eq!(i16.width, w);
        assert_eq!(i16.height, h);
        assert_eq!(i16.bit_depth, 10);
        let want_y: Vec<u16> = (0..w as usize * h as usize)
            .map(|i| y_samples[i % y_samples.len()])
            .collect();
        assert_eq!(
            i16.y, want_y,
            "16-bit luma must preserve the 10-bit samples"
        );
        let want_y8: Vec<u8> = want_y.iter().map(|&s| (s >> 2) as u8).collect();
        assert_eq!(i8.y, want_y8, "8-bit luma must be the >> 2 truncation");

        assert_eq!(i16.u, vec![512u16; 4]);
        assert_eq!(i16.v, vec![1023u16; 4]);
        assert_eq!(i8.u, vec![128u8; 4]);
        assert_eq!(i8.v, vec![255u8; 4]);
    }
}
