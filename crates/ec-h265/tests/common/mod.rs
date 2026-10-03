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

/// Write `bytes` to `path` and PROVE the file on disk is exactly that long.
///
/// lane-h26xpinloud: the encode gate's sites used `std::fs::write(..).ok()?`
/// and [`write_au`] used `File::create` + `write_all(..).expect(..)`, then all
/// three handed `path` to ffmpeg. Checking the `Result` closes only the ERROR
/// path. An Ok-but-short write -- a filesystem that reports success and stores
/// fewer bytes (ENOSPC on a full mount, EFBIG under a size cap, a quota, a
/// failed final flush) -- left ffmpeg decoding a truncated stream, and the
/// failure surfaced as a plausible-looking wrong pixel or a shifted PSNR
/// number, not as a write failure. MEASURED: 1234 bytes written through a path
/// that LOOKS like a stream file in a scratch directory but whose filesystem
/// accepts every byte and stores none returns `Ok`, and the file on disk is 0
/// bytes. So the length is checked here once, from the same `bytes.len()` the
/// writer used, and any shortfall carries the path, both counts, and the OS
/// error -- or, for the write that reported success and left a short file, the
/// plain statement that there was no OS error, which is exactly the case that
/// used to print nothing.
///
/// This is the ec-av1 `dumpio::LoudDump` idea replicated per crate: there is no
/// shared test-support crate here and none is being added for one function.
/// Returns the failure LINE rather than a bare bool, so the bail callers and the
/// panic caller all end on one format that cannot drift apart. The success path
/// is byte-identical to the old write: same bytes, same path, truncate then
/// write.
pub fn pin_loud(site: &str, path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let expected = bytes.len();
    if let Err(e) = std::fs::write(path, bytes) {
        // The count here is the file's REAL length, not what the writer had
        // confirmed in hand: under a size cap the kernel stores everything up
        // to the cap and only THEN returns the error, and the write confirms
        // none of it.
        let on_disk = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        return Err(short_line(
            site,
            path,
            on_disk,
            expected,
            &format!("the write failed: {e}"),
        ));
    }
    match std::fs::metadata(path) {
        Ok(m) if m.len() as usize == expected => Ok(()),
        Ok(m) => Err(short_line(
            site,
            path,
            m.len() as usize,
            expected,
            "the write reported success and the file on disk is still not that long (no OS error)",
        )),
        Err(e) => Err(short_line(
            site,
            path,
            0,
            expected,
            &format!("the written file could not be stat'ed: {e}"),
        )),
    }
}

/// The one failure line every short pin ends on: site, path, the bytes the file
/// holds vs the bytes owed, and the reason.
fn short_line(
    site: &str,
    path: &std::path::Path,
    on_disk: usize,
    expected: usize,
    why: &str,
) -> String {
    format!(
        "ec-h265 test pin SHORT [{}] {}: file holds {on_disk} of {expected} bytes: {why}",
        site,
        path.display()
    )
}

/// A pin whose shortfall is reported and then BAILED on, for a caller that
/// returns `None` today. Prints the same line [`pin_loud`] returns, so `None`
/// from here never reads as a plain "ffmpeg said no".
pub fn pin_or_bail(site: &str, path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    pin_loud(site, path, bytes)
        .map_err(|why| eprintln!("{why}"))
        .ok()
}

/// [`pin_loud`] with the shortfall PANICKING, for a caller that fails loudly
/// today. The panic message IS the failure line, so the two callers of this
/// shape cannot drift apart either.
pub fn pin_or_panic(site: &str, path: &std::path::Path, bytes: &[u8]) {
    if let Err(why) = pin_loud(site, path, bytes) {
        panic!("{why}");
    }
}
