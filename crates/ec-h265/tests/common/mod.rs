//! Shared fixtures for the integration tests.

use ec_core::frame::{PixelFormat, Plane, VideoFrame};

/// A synthetic picture with gradients, edges and texture — enough structure
/// that every intra direction gets used somewhere.
pub fn test_frame(width: u32, height: u32, phase: u32) -> VideoFrame {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    for row in 0..h {
        for col in 0..w {
            let diag = ((row + col + phase as usize) % 64) as i32;
            let ramp = (col * 200 / w) as i32;
            let ring = if (row * row + col * col) % 977 < 60 { 60 } else { 0 };
            let texture = ((row * 7 + col * 13) % 17) as i32 * 3;
            y[row * w + col] = (16 + ramp + diag / 2 + ring + texture).clamp(0, 255) as u8;
        }
    }
    let mut cb = vec![0u8; cw * ch];
    let mut cr = vec![0u8; cw * ch];
    for row in 0..ch {
        for col in 0..cw {
            cb[row * cw + col] =
                (128 + (col as i32 * 60 / (cw as i32)) - 30).clamp(0, 255) as u8;
            cr[row * cw + col] =
                (128 + (row as i32 * 60 / (ch as i32)) - 30).clamp(0, 255) as u8;
        }
    }
    VideoFrame::try_new(
        PixelFormat::I420,
        width,
        height,
        vec![Plane::new(y, w), Plane::new(cb, cw), Plane::new(cr, cw)],
    )
    .expect("test frame")
}

