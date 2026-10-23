//! The header probe, EXIF orientation, pixel transforms and PNG encoder, each checked
//! against the `image` crate (a dev-dependency oracle only).
//!
//! Fixtures are built here: EXIF blocks are written by hand (no exiftool on the lanes),
//! and the containers around them come from `image`'s own encoders.

use ec_image::transform::{Layout, apply_orientation};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder};
use std::io::{Cursor, Read};

/// A 7x5 RGB picture whose every pixel is distinct, so any misplaced pixel shows.
fn pattern(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[
                (x * 35) as u8,
                (y * 50) as u8,
                ((x * 7 + y * 13) % 256) as u8,
            ]);
        }
    }
    v
}

/// A TIFF block (II or MM) with one IFD0 entry: orientation `o`.
fn tiff(o: u16, big_endian: bool) -> Vec<u8> {
    let mut t = Vec::new();
    let u16b = |v: u16| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u32b = |v: u32| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    t.extend_from_slice(if big_endian { b"MM" } else { b"II" });
    t.extend_from_slice(&u16b(42));
    t.extend_from_slice(&u32b(8));
    t.extend_from_slice(&u16b(1));
    t.extend_from_slice(&u16b(0x0112));
    t.extend_from_slice(&u16b(3));
    t.extend_from_slice(&u32b(1));
    t.extend_from_slice(&u16b(o));
    t.extend_from_slice(&u16b(0));
    t.extend_from_slice(&u32b(0));
    t
}

fn jpeg(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
        .write_image(px, w, h, ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn png(w: u32, h: u32, px: &[u8], color: ExtendedColorType) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(px, w, h, color)
        .unwrap();
    out
}

fn webp_lossless(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(px, w, h, ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// JPEG with an APP1 Exif segment spliced in after SOI.
fn jpeg_with_exif(plain: &[u8], o: u16, be: bool) -> Vec<u8> {
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend(tiff(o, be));
    let mut out = plain[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    out.extend(seg);
    out.extend_from_slice(&plain[2..]);
    out
}

/// PNG with an `eXIf` chunk (raw TIFF, no Exif header) placed before the first IDAT.
fn png_with_exif(plain: &[u8], o: u16, be: bool) -> Vec<u8> {
    let idat = plain.windows(4).position(|w| w == b"IDAT").unwrap() - 4;
    let body = tiff(o, be);
    let mut chunk = (body.len() as u32).to_be_bytes().to_vec();
    let mut typed = b"eXIf".to_vec();
    typed.extend(&body);
    chunk.extend(&typed);
    chunk.extend(crc32(&typed).to_be_bytes());
    let mut out = plain[..idat].to_vec();
    out.extend(chunk);
    out.extend_from_slice(&plain[idat..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Lossless WebP with a VP8X header (EXIF flag set) and a bare-TIFF EXIF chunk after the image.
fn webp_with_exif(plain: &[u8], o: u16, be: bool, w: u32, h: u32) -> Vec<u8> {
    let image_chunk = &plain[12..]; // the simple file's single VP8L chunk, with its header
    let exif = tiff(o, be); // bare TIFF: the WebP EXIF chunk has no Exif header
    let mut vp8x = vec![0x08, 0, 0, 0];
    vp8x.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    let mut body = b"WEBP".to_vec();
    body.extend_from_slice(b"VP8X");
    body.extend_from_slice(&10u32.to_le_bytes());
    body.extend(&vp8x);
    body.extend_from_slice(image_chunk);
    body.extend_from_slice(b"EXIF");
    body.extend_from_slice(&(exif.len() as u32).to_le_bytes());
    body.extend(&exif);
    if exif.len() % 2 == 1 {
        body.push(0);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

/// The `image` crate's view: format-level orientation (as its EXIF number) and pixels.
fn image_view(bytes: &[u8]) -> (u8, Vec<u8>, u32, u32) {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .unwrap();
    let mut decoder = reader.into_decoder().unwrap();
    let orientation = decoder.orientation().unwrap().to_exif();
    let mut img = DynamicImage::from_decoder(decoder).unwrap();
    img.apply_orientation(image::metadata::Orientation::from_exif(orientation).unwrap());
    let (w, h) = (img.width(), img.height());
    (orientation, img.to_rgb8().into_raw(), w, h)
}

const W: u32 = 7;
const H: u32 = 5;

fn bytes_close(a: &[u8], b: &[u8], tolerance: u8) -> usize {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > tolerance)
        .count()
}

#[test]
fn every_orientation_is_probed_and_applied_like_image_for_jpeg_png_and_webp() {
    let px = pattern(W, H);
    let plain_jpeg = jpeg(W, H, &px);
    let plain_png = png(W, H, &px, ExtendedColorType::Rgb8);
    let plain_webp = webp_lossless(W, H, &px);
    for o in 1..=8u16 {
        for be in [false, true] {
            let cases = [
                ("jpeg", jpeg_with_exif(&plain_jpeg, o, be), 3u8),
                ("png", png_with_exif(&plain_png, o, be), 0),
                ("webp", webp_with_exif(&plain_webp, o, be, W, H), 0),
            ];
            for (what, bytes, tol) in cases {
                let info = ec_image::info(&bytes).unwrap();
                assert_eq!(
                    info.orientation, o as u8,
                    "{what} info, orientation {o} be={be}"
                );
                let probed = ec_image::probe(&bytes).unwrap();
                assert_eq!(probed, info, "{what} probe matches info");
                let (theirs_o, theirs_px, tw, th) = image_view(&bytes);
                assert_eq!(theirs_o, o as u8, "image reads the same {what} orientation");
                let decoded = ec_image::decode(&bytes).unwrap();
                assert_eq!(
                    decoded.meta.orientation,
                    Some(o as u8),
                    "{what} decode meta"
                );
                let ours = decoded.to_rgb8();
                let (uw, uh, upright) =
                    apply_orientation(Layout::Rgb8, W, H, &ours, o as u8).unwrap();
                assert_eq!((uw, uh), (tw, th), "{what} upright size {o}");
                let wrong = bytes_close(&upright, &theirs_px, tol);
                assert_eq!(
                    wrong, 0,
                    "{what} upright pixels differ from image at orientation {o}"
                );
            }
        }
    }
}

#[test]
fn transforms_match_image_on_random_rgba_for_all_orientations() {
    let (w, h) = (9u32, 4u32);
    let mut s = 0x1234_5678u32;
    let px: Vec<u8> = (0..(w * h * 4))
        .map(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (s >> 16) as u8
        })
        .collect();
    for o in 1..=8u8 {
        let (uw, uh, got) = apply_orientation(Layout::Rgba8, w, h, &px, o).unwrap();
        let mut img = image::RgbaImage::from_raw(w, h, px.clone()).unwrap();
        let mut dynimg = DynamicImage::ImageRgba8(img.clone());
        dynimg.apply_orientation(image::metadata::Orientation::from_exif(o).unwrap());
        img = dynimg.to_rgba8();
        assert_eq!((uw, uh), (img.width(), img.height()), "size at {o}");
        assert_eq!(got, img.into_raw(), "rgba transform at orientation {o}");
    }
}

#[test]
fn gif_and_bmp_report_orientation_one() {
    let px = pattern(W, H);
    let rgba: Vec<u8> = px.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let mut gif = Vec::new();
    image::codecs::gif::GifEncoder::new(&mut gif)
        .encode_frame(image::Frame::new(
            image::RgbaImage::from_raw(W, H, rgba).unwrap(),
        ))
        .unwrap();
    assert_eq!(ec_image::info(&gif).unwrap().orientation, 1);
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp)
        .write_image(&px, W, H, ExtendedColorType::Rgb8)
        .unwrap();
    assert_eq!(ec_image::info(&bmp).unwrap().orientation, 1);
}

#[test]
fn probe_reads_the_header_and_never_the_pixels() {
    // A PNG whose IDAT body is garbage still probes: the header is all that is read.
    let px = pattern(W, H);
    let mut bytes = png(W, H, &px, ExtendedColorType::Rgb8);
    let idat = bytes.windows(4).position(|w| w == b"IDAT").unwrap();
    let body_at = idat + 4;
    let len = u32::from_be_bytes(bytes[idat - 4..idat].try_into().unwrap()) as usize;
    for b in &mut bytes[body_at..body_at + len] {
        *b = 0xa5;
    }
    let info = ec_image::probe(&bytes).unwrap();
    assert_eq!(
        (info.width, info.height, info.format),
        (W, H, ec_image::ImageFormat::Png)
    );
    // The same bytes do not decode.
    assert!(ec_image::decode(&bytes).is_err());
}

#[test]
fn a_prefix_of_a_large_jpeg_probes_to_the_same_answer() {
    let (w, h) = (640u32, 480u32);
    let px: Vec<u8> = pattern(w, h);
    let bytes = jpeg_with_exif(&jpeg(w, h, &px), 6, false);
    let whole = ec_image::info(&bytes).unwrap();
    // Cut far past the header, well inside the 1 MiB window.
    let prefix = ec_image::probe(&bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(prefix, whole);
    // Too short to reach SOF: refused, not guessed.
    assert!(ec_image::probe(&bytes[..10]).is_err());
}

/// Yields `header` and then endless zero bytes, counting what is pulled from it.
struct Endless<'a> {
    header: &'a [u8],
    at: usize,
    pulled: usize,
}

impl Read for Endless<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            let b = if self.at < self.header.len() {
                self.header[self.at]
            } else {
                0
            };
            buf[n] = b;
            self.at += 1;
            n += 1;
        }
        self.pulled += n;
        Ok(n)
    }
}

#[test]
fn probe_reader_stops_at_the_window() {
    let px = pattern(W, H);
    let bytes = png_with_exif(&png(W, H, &px, ExtendedColorType::Rgb8), 8, false);
    let mut endless = Endless {
        header: &bytes,
        at: 0,
        pulled: 0,
    };
    let info = ec_image::probe_reader(&mut endless).unwrap();
    assert_eq!((info.width, info.height, info.orientation), (W, H, 8));
    assert_eq!(
        endless.pulled,
        ec_image::PROBE_LIMIT,
        "exactly one window is read"
    );
}

#[test]
fn a_huge_declared_size_is_reported_without_allocating_it() {
    // A PNG header claiming 2^31-1 by 2^31-1 pixels: the probe reports it and does not decode.
    let px = pattern(W, H);
    let mut bytes = png(W, H, &px, ExtendedColorType::Rgb8);
    bytes[16..20].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
    bytes[20..24].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
    let ihdr_crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&ihdr_crc.to_be_bytes());
    let info = ec_image::probe(&bytes[..33]).unwrap();
    assert_eq!((info.width, info.height), (0x7fff_ffff, 0x7fff_ffff));
}

#[test]
fn png_encoder_round_trips_bit_exact_and_matches_image_decode() {
    let (w, h) = (97u32, 61u32);
    let rgb = pattern(w, h);
    let out = ec_image::png_encode::encode_rgb8(w, h, &rgb).unwrap();
    assert_eq!(ec_image::decode(&out).unwrap().to_rgb8(), rgb);
    assert_eq!(
        image::load_from_memory(&out).unwrap().to_rgb8().into_raw(),
        rgb
    );

    let rgba: Vec<u8> = rgb
        .chunks(3)
        .enumerate()
        .flat_map(|(i, p)| [p[0], p[1], p[2], (i * 37 % 256) as u8])
        .collect();
    let out = ec_image::png_encode::encode_rgba8(w, h, &rgba).unwrap();
    assert_eq!(ec_image::decode(&out).unwrap().to_rgba8(), rgba);
    assert_eq!(
        image::load_from_memory(&out).unwrap().to_rgba8().into_raw(),
        rgba
    );
}

#[test]
fn png_encoder_size_against_image_on_the_still_fixture() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills/source.png");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipped: {} not generated", path.display());
        return;
    };
    let src = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let (w, h) = src.dimensions();
    let ours = ec_image::png_encode::encode_rgb8(w, h, src.as_raw()).unwrap();
    let mut theirs = Vec::new();
    image::codecs::png::PngEncoder::new(&mut theirs)
        .write_image(src.as_raw(), w, h, ExtendedColorType::Rgb8)
        .unwrap();
    eprintln!(
        "png encode {w}x{h} rgb8: ec-image {} bytes, image {} bytes ({:.2}x)",
        ours.len(),
        theirs.len(),
        ours.len() as f64 / theirs.len() as f64
    );
    assert_eq!(
        ec_image::decode(&ours).unwrap().to_rgb8(),
        src.as_raw().as_slice()
    );
}

#[test]
fn probing_arbitrary_mutations_never_panics() {
    let px = pattern(W, H);
    let seeds = [
        jpeg_with_exif(&jpeg(W, H, &px), 6, false),
        png_with_exif(&png(W, H, &px, ExtendedColorType::Rgb8), 6, false),
        webp_with_exif(&webp_lossless(W, H, &px), 6, true, W, H),
    ];
    let mut s = 99u32;
    for seed in &seeds {
        for i in 0..seed.len().min(256) {
            for v in [0u8, 0xff, 0x7f] {
                let mut m = seed.clone();
                m[i] = v;
                let _ = ec_image::probe(&m);
                let _ = ec_image::info(&m);
            }
        }
        for _ in 0..2000 {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let mut m = seed.clone();
            let at = (s >> 8) as usize % m.len();
            m[at] = (s >> 16) as u8;
            m.truncate(((s >> 4) as usize % m.len()).max(1));
            let _ = ec_image::probe(&m);
        }
    }
}
