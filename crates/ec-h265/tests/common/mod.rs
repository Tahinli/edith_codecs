//! Shared fixtures for the integration tests.
//!
//! Each test binary compiles this module and uses part of it.
#![allow(dead_code)]

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
            let ring = if (row * row + col * col) % 977 < 60 {
                60
            } else {
                0
            };
            let texture = ((row * 7 + col * 13) % 17) as i32 * 3;
            y[row * w + col] = (16 + ramp + diag / 2 + ring + texture).clamp(0, 255) as u8;
        }
    }
    let mut cb = vec![0u8; cw * ch];
    let mut cr = vec![0u8; cw * ch];
    for row in 0..ch {
        for col in 0..cw {
            cb[row * cw + col] = (128 + (col as i32 * 60 / (cw as i32)) - 30).clamp(0, 255) as u8;
            cr[row * cw + col] = (128 + (row as i32 * 60 / (ch as i32)) - 30).clamp(0, 255) as u8;
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

/// A picture with the statistics of camera video rather than of a noise
/// generator: smooth gradients, a few hard edges, and detail that decays with
/// distance from them.
///
/// The speed bar this family carries was measured on real 1080p footage, so the
/// fixture the bar is asserted against has to look like footage. The textured
/// [`test_frame`] above is the opposite fixture — worst case for the residual
/// coder — and both numbers are worth printing.
pub fn natural_frame(width: u32, height: u32, phase: u32) -> VideoFrame {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    for row in 0..h {
        for col in 0..w {
            let fx = col as f32 / w as f32;
            let fy = row as f32 / h as f32;
            // Two soft gradients, a horizon and a couple of objects.
            let sky = 200.0 - 90.0 * fy;
            let ground = 60.0 + 40.0 * fx;
            let mut value = if fy < 0.55 { sky } else { ground };
            let dx = fx - 0.3;
            let dy = fy - 0.65;
            if dx * dx + dy * dy < 0.02 {
                value = 150.0 - 40.0 * fx;
            }
            if (fx - 0.72).abs() < 0.06 && fy > 0.35 {
                value = 40.0;
            }
            // Sensor grain, small and local.
            let grain = (((row * 31 + col * 17 + phase as usize) % 7) as f32) - 3.0;
            y[row * w + col] = (value + grain).clamp(0.0, 255.0) as u8;
        }
    }
    let mut cb = vec![0u8; cw * ch];
    let mut cr = vec![0u8; cw * ch];
    for row in 0..ch {
        for col in 0..cw {
            let fy = row as f32 / ch as f32;
            cb[row * cw + col] = (140.0 - 30.0 * fy) as u8;
            cr[row * cw + col] = (110.0 + 25.0 * fy) as u8;
        }
    }
    VideoFrame::try_new(
        PixelFormat::I420,
        width,
        height,
        vec![Plane::new(y, w), Plane::new(cb, cw), Plane::new(cr, cw)],
    )
    .expect("natural frame")
}
