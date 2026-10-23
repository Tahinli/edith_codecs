//! Shared fixtures for the integration tests.
//!
//! Each test binary compiles this module and uses part of it.
#![allow(dead_code)]

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::decode::DecodedPicture;
use ec_h265_syntax::ConformanceWindow;
use ec_h265_syntax::sei::{PictureHash, SeiMessage, decoded_picture_hash_rbsp, parse_sei_rbsp};

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
        let on_disk = std::fs::metadata(path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
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

pub const PLANE_NAMES: [&str; 3] = ["Y", "Cb", "Cr"];

/// Bytes per sample of component `c`: 1 for 8-bit, 2 (little endian) above 8 bits.
pub fn bytes_per_sample(pic: &DecodedPicture, c: usize) -> usize {
    if pic.bit_depth_of(c) > 8 { 2 } else { 1 }
}

/// The samples of a picture cropped to its conformance window, as the bytes a YUV
/// file of that frame holds (planar, 8-bit or little-endian 16-bit).
pub fn cropped_bytes(pic: &DecodedPicture, win: &ConformanceWindow) -> Vec<u8> {
    // The reference YUV holds every plane at the wider sample size.
    let bps = bytes_per_sample(pic, 0).max(bytes_per_sample(pic, 1));
    let (w, h) = (pic.width, pic.height);
    let (cw, ch) = pic.chroma_dims();
    // The conformance window offsets are in chroma units: luma units are SubWidthC and
    // SubHeightC times as large.
    let (sw, sh) = (
        1usize << pic.chroma_shifts().0,
        1usize << pic.chroma_shifts().1,
    );
    let (l, r, t, b) = (
        win.left as usize,
        win.right as usize,
        win.top as usize,
        win.bottom as usize,
    );
    let mut out = Vec::new();
    for (samples, pw, ph, (x0, x1, y0, y1)) in [
        (
            &pic.y,
            w,
            h,
            (
                sw * l,
                w.saturating_sub(sw * r),
                sh * t,
                h.saturating_sub(sh * b),
            ),
        ),
        (
            &pic.cb,
            cw,
            ch,
            (l, cw.saturating_sub(r), t, ch.saturating_sub(b)),
        ),
        (
            &pic.cr,
            cw,
            ch,
            (l, cw.saturating_sub(r), t, ch.saturating_sub(b)),
        ),
    ] {
        for y in y0..y1.min(ph) {
            for x in x0..x1.min(pw) {
                let s = samples.get(y * pw + x).copied().unwrap_or(0);
                if bps == 1 {
                    out.push(s as u8);
                } else {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
    out
}

/// A description of one mismatched plane: its location when the reference is known.
pub fn describe(
    reference: Option<&[u8]>,
    out_index: usize,
    pic: &DecodedPicture,
    poc: i32,
    plane: usize,
) -> String {
    let label = PLANE_NAMES.get(plane).copied().unwrap_or("?");
    match reference {
        None => format!("hash mismatch poc={poc} plane={label}"),
        Some(r) => match first_diff(r, out_index, pic, plane) {
            Ok(Some((x, y))) => format!("poc={poc} plane={label} x={x} y={y}"),
            Ok(None) => format!("hash mismatch poc={poc} plane={label} (no sample differs)"),
            Err(e) => format!("hash mismatch poc={poc} plane={label} ({e})"),
        },
    }
}

/// The first sample (x, y) of `plane` that differs from the reference YUV, where
/// frame `out_index` in output order starts at byte `out_index * frame_bytes`.
pub fn first_diff(
    reference: &[u8],
    out_index: usize,
    pic: &DecodedPicture,
    plane: usize,
) -> Result<Option<(usize, usize)>, String> {
    // The reference YUV holds every plane at the wider sample size.
    let bps = bytes_per_sample(pic, 0).max(bytes_per_sample(pic, 1));
    let dims = plane_dims(pic);
    let luma = dims[0].0 * dims[0].1;
    let chroma = dims[1].0 * dims[1].1;
    let frame_bytes = bps * (luma + 2 * chroma);
    let (offset, samples) = match plane {
        0 => (0, &pic.y),
        1 => (bps * luma, &pic.cb),
        _ => (bps * (luma + chroma), &pic.cr),
    };
    let width = dims[plane].0.max(1);
    let base = out_index * frame_bytes + offset;
    for (i, &s) in samples.iter().enumerate() {
        let at = base + i * bps;
        let value = if bps == 1 {
            reference.get(at).map(|&b| u16::from(b))
        } else {
            reference
                .get(at..at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
        };
        let Some(value) = value else {
            return Err(format!(
                "reference YUV ends inside plane {}",
                PLANE_NAMES[plane]
            ));
        };
        if value != s {
            return Ok(Some((i % width, i / width)));
        }
    }
    Ok(None)
}

/// The 32-bit checksum of one plane of `width` samples a row (D.3.19, hash_type 2).
pub fn plane_checksum(samples: &[u16], width: usize, bit_depth: u32) -> u32 {
    let mut sum: u32 = 0;
    for (i, &s) in samples.iter().enumerate() {
        let (x, y) = (i % width.max(1), i / width.max(1));
        let mask = ((x & 0xff) ^ (y & 0xff) ^ (x >> 8) ^ (y >> 8)) as u32;
        sum = sum.wrapping_add((u32::from(s) & 0xff) ^ mask);
        if bit_depth > 8 {
            sum = sum.wrapping_add((u32::from(s) >> 8) ^ mask);
        }
    }
    sum
}

/// The 16-bit CRC of one plane (D.3.19, hash_type 1): the samples as bytes, low
/// byte first above 8 bits, then 16 zero bits.
pub fn plane_crc(samples: &[u16], bit_depth: u32) -> u16 {
    let mut crc: u32 = 0xffff;
    let mut feed = |byte: u8| {
        for bit in 0..8 {
            let msb = (crc >> 15) & 1;
            let bit_val = u32::from((byte >> (7 - bit)) & 1);
            crc = (((crc << 1) + bit_val) & 0xffff) ^ (msb * 0x1021);
        }
    };
    for &s in samples {
        feed(s as u8);
        if bit_depth > 8 {
            feed((s >> 8) as u8);
        }
    }
    for _ in 0..16 {
        let msb = (crc >> 15) & 1;
        crc = ((crc << 1) & 0xffff) ^ (msb * 0x1021);
    }
    crc as u16
}

/// The MD5 digest of each plane, as the SEI hash defines it. `None` when the
/// sample arrays do not match the picture size.
pub fn plane_digests(pic: &DecodedPicture) -> Option<Vec<[u8; 16]>> {
    let planes = [&pic.y, &pic.cb, &pic.cr];
    let mut bytes: Vec<Vec<u8>> = Vec::new();
    let mut dims: Vec<(usize, usize)> = Vec::new();
    for (c, ((samples, (w, h)), _)) in planes
        .iter()
        .zip(plane_dims(pic))
        .zip(PLANE_NAMES)
        .enumerate()
    {
        if samples.is_empty() {
            continue;
        }
        if samples.len() != w * h {
            return None;
        }
        let bps = bytes_per_sample(pic, c);
        let mut buf = Vec::with_capacity(samples.len() * bps);
        for &s in samples.iter() {
            if bps == 1 {
                buf.push(s as u8);
            } else {
                buf.extend_from_slice(&s.to_le_bytes());
            }
        }
        bytes.push(buf);
        dims.push((w * bytes_per_sample(pic, c), h));
    }
    let refs: Vec<(&[u8], usize, usize, usize)> = bytes
        .iter()
        .zip(&dims)
        .map(|(b, &(row_bytes, h))| (b.as_slice(), row_bytes, row_bytes, h))
        .collect();
    let rbsp = decoded_picture_hash_rbsp(&refs);
    let messages = parse_sei_rbsp(&rbsp).ok()?;
    messages.into_iter().find_map(|m| match m {
        SeiMessage::DecodedPictureHash(PictureHash::Md5(digests)) => Some(digests),
        _ => None,
    })
}

/// Plane sizes as (width, height) for Y, Cb and Cr, at the picture's chroma format.
pub fn plane_dims(pic: &DecodedPicture) -> [(usize, usize); 3] {
    let (cw, ch) = pic.chroma_dims();
    [(pic.width, pic.height), (cw, ch), (cw, ch)]
}

/// Per plane, whether the picture matches the SEI hash of the same kind. An MD5
/// digest is compared with the whole digest; a CRC or checksum with its value.
pub fn plane_matches(pic: &DecodedPicture, expected: &PictureHash) -> Vec<bool> {
    let planes = [&pic.y, &pic.cb, &pic.cr];
    let dims = plane_dims(pic);
    match expected {
        PictureHash::Md5(want) => {
            let got = plane_digests(pic).unwrap_or_default();
            want.iter()
                .enumerate()
                .map(|(i, w)| got.get(i) == Some(w))
                .collect()
        }
        PictureHash::Crc(want) => want
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                planes
                    .get(i)
                    .is_some_and(|p| plane_crc(p, pic.bit_depth_of(i)) == w)
            })
            .collect(),
        PictureHash::Checksum(want) => want
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                planes
                    .get(i)
                    .is_some_and(|p| plane_checksum(p, dims[i].0, pic.bit_depth_of(i)) == w)
            })
            .collect(),
    }
}

/// The digest in a `<name>_md5.txt` file: its first word, as 32 hex digits. That file holds the
/// MD5 of the whole decoded output (every frame, cropped, in output order).
pub fn whole_output_digest(text: &str) -> Option<[u8; 16]> {
    let word = text.split_whitespace().next()?;
    if word.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(word.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
