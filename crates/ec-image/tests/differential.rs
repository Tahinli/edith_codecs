//! Differential tests against the incumbent `image` crate over the generated
//! corpus (`scripts/gen-still-fixtures.sh`).
//!
//! The bars differ by format because the formats differ: PNG and VP8L are
//! lossless, so anything short of pixel-exact agreement is a bug. JPEG's IDCT
//! rounding is explicitly implementation-defined, so the bar there is a
//! per-sample delta plus a correlation, both reported. WebP lossy is measured
//! against the *source picture*, so the question is whether this decoder
//! reconstructs it at least as faithfully as the incumbent does.

use image::GenericImageView;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills")
}

/// Every fixture with one of these extensions, sorted for a stable report.
fn corpus(extension: &str) -> Vec<PathBuf> {
    let dir = fixtures();
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == extension))
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("source"))
        })
        .collect();
    found.sort();
    found
}

fn skip(what: &str) -> bool {
    eprintln!("skipped: fixtures/stills not generated ({what}); run scripts/gen-still-fixtures.sh");
    true
}

/// Largest per-sample difference and Pearson correlation between two buffers.
fn compare(ours: &[u8], theirs: &[u8]) -> (u32, f64) {
    assert_eq!(ours.len(), theirs.len(), "sample counts differ");
    let mut max = 0u32;
    let n = ours.len() as f64;
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (&a, &b) in ours.iter().zip(theirs) {
        max = max.max(u32::from(a.abs_diff(b)));
        let (a, b) = (f64::from(a), f64::from(b));
        sa += a;
        sb += b;
        saa += a * a;
        sbb += b * b;
        sab += a * b;
    }
    let cov = sab - sa * sb / n;
    let va = (saa - sa * sa / n).max(0.0);
    let vb = (sbb - sb * sb / n).max(0.0);
    let corr = if va * vb > 0.0 {
        cov / (va * vb).sqrt()
    } else {
        1.0
    };
    (max, corr)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn png_decodes_pixel_exactly() {
    let files = corpus("png");
    if files.is_empty() && skip("png") {
        return;
    }
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let theirs = image::load_from_memory(&bytes).expect("incumbent decode");
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{}",
            path.display()
        );
        if ours.pixels.bit_depth() == 16 {
            // Compared at 16 bits so the comparison is not laundered through
            // two different 16-to-8 rounding conventions.
            let a = ours.to_rgba16();
            let b = theirs.to_rgba16().into_raw();
            assert_eq!(a, b, "{} (16-bit samples)", path.display());
        } else {
            let a = ours.to_rgba8();
            let b = theirs.to_rgba8().into_raw();
            assert_eq!(a, b, "{} (8-bit samples)", path.display());
        }
    }
}

#[test]
fn webp_lossless_decodes_pixel_exactly() {
    let files: Vec<PathBuf> = corpus("webp")
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("lossless"))
        .collect();
    if files.is_empty() && skip("webp") {
        return;
    }
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let theirs = image::load_from_memory(&bytes).expect("incumbent decode");
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{}",
            path.display()
        );
        assert_eq!(
            ours.to_rgba8(),
            theirs.to_rgba8().into_raw(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn jpeg_matches_the_incumbent_within_rounding() {
    let files = corpus("jpg");
    if files.is_empty() && skip("jpg") {
        return;
    }
    let mut worst = 0u32;
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let theirs = image::load_from_memory(&bytes).expect("incumbent decode");
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{}",
            path.display()
        );
        let (max, corr) = compare(&ours.to_rgb8(), &theirs.to_rgb8().into_raw());
        let db = psnr(&ours.to_rgb8(), &theirs.to_rgb8().into_raw());
        eprintln!(
            "{:<22} max delta {max:>3}  corr {corr:.6}  psnr {db:.1} dB",
            path.file_name().unwrap().to_string_lossy()
        );
        worst = worst.max(max);
        // Five counts of 255 is where this decoder's IDCT and colour rounding
        // sit against jpeg-decoder's; both are legal readings of T.81, and the
        // gap is uniform noise rather than structure, which is what the
        // correlation and the PSNR are there to show. Grayscale, which skips
        // the colour matrix and the upsampler, agrees within one count.
        assert!(max <= 5, "{}: max sample delta {max}", path.display());
        assert!(corr > 0.9995, "{}: correlation {corr}", path.display());
        assert!(
            db > 48.0,
            "{}: {db:.1} dB against the incumbent decode",
            path.display()
        );
    }
    eprintln!("jpeg worst per-sample delta vs incumbent: {worst}");
}

#[test]
fn webp_lossy_reconstructs_at_least_as_well_as_the_incumbent() {
    let files: Vec<PathBuf> = corpus("webp")
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("lossy"))
        .collect();
    if files.is_empty() && skip("webp") {
        return;
    }
    let mut shortfalls: Vec<String> = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = if name.starts_with("odd") {
            fixtures().join("source-odd.png")
        } else if name.contains("alpha") {
            fixtures().join("source-alpha.png")
        } else {
            fixtures().join("source.png")
        };
        let reference = image::open(&source).expect("source picture").to_rgb8();
        let bytes = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let theirs = image::load_from_memory(&bytes).expect("incumbent decode");
        let ours_rgb = ours.to_rgb8();
        let theirs_rgb = theirs.to_rgb8().into_raw();
        let mine = psnr(&ours_rgb, reference.as_raw());
        let incumbent = psnr(&theirs_rgb, reference.as_raw());
        let (max, corr) = compare(&ours_rgb, &theirs_rgb);
        eprintln!(
            "{name:<22} psnr {mine:.2} dB (incumbent {incumbent:.2} dB)  \
             max delta {max}  corr {corr:.6}"
        );
        if mine < incumbent - 0.1 {
            shortfalls.push(format!(
                "{name}: {mine:.2} dB against the source, incumbent {incumbent:.2} dB"
            ));
        }
        if ours.pixels.has_alpha() {
            let a: Vec<u8> = ours.to_rgba8().chunks_exact(4).map(|p| p[3]).collect();
            let b: Vec<u8> = theirs
                .to_rgba8()
                .into_raw()
                .chunks_exact(4)
                .map(|p| p[3])
                .collect();
            assert_eq!(a, b, "{name}: alpha plane is lossless and must match");
        }
    }
    assert!(shortfalls.is_empty(), "{}", shortfalls.join("\n"));
}

#[test]
fn header_dimensions_match_the_incumbent() {
    let mut checked = 0;
    for extension in ["png", "jpg", "webp"] {
        for path in corpus(extension) {
            if path.to_string_lossy().contains("animated") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let ours = ec_image::info(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let theirs = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .unwrap()
                .into_dimensions()
                .unwrap();
            assert_eq!((ours.width, ours.height), theirs, "{}", path.display());
            checked += 1;
        }
    }
    if checked == 0 {
        skip("any");
    }
}

#[test]
fn an_animated_webp_is_refused_by_name() {
    let path = fixtures().join("animated.webp");
    let Ok(bytes) = std::fs::read(&path) else {
        skip("animated.webp");
        return;
    };
    let err = ec_image::decode(&bytes).expect_err("an animation is not a still");
    let message = format!("{err}");
    assert!(message.contains("animated"), "{message}");
}

/// An APP1 EXIF segment carrying orientation 6, spliced after SOI.
///
/// Built here rather than generated: ImageMagick writes no EXIF when the
/// source is a PNG, and a fixture whose name promises a tag it does not carry
/// is worse than no fixture.
fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = b"Exif\0\0II".to_vec();
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());

    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((tiff.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&tiff);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[test]
fn exif_orientation_is_reported_and_not_applied() {
    let Ok(plain) = std::fs::read(fixtures().join("baseline-444.jpg")) else {
        skip("baseline-444.jpg");
        return;
    };
    let bytes = with_exif_orientation(&plain, 6);
    let ours = ec_image::decode(&bytes).unwrap();
    let theirs = image::load_from_memory(&bytes).unwrap();
    assert_eq!(ours.meta.orientation, Some(6), "EXIF tag 0x0112 parsed");
    assert_eq!(ec_image::decode(&plain).unwrap().meta.orientation, None);
    // Not applied: the pixels are still in stored order, exactly as the
    // incumbent leaves them, and the tagged file decodes like the plain one.
    assert_eq!((ours.width, ours.height), theirs.dimensions());
    assert_eq!(ours.to_rgb8(), ec_image::decode(&plain).unwrap().to_rgb8());
}

/// Every fixture decodes, and the failures are reported together rather than
/// one per run — a decoder that breaks on three formats should say so once.
#[test]
fn every_fixture_decodes() {
    let mut failures = Vec::new();
    let mut count = 0;
    for extension in ["png", "jpg", "webp"] {
        for path in corpus(extension) {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.contains("animated") || name.starts_with("anim-") {
                continue; // an animation is not a still; the animation tests cover it
            }
            let bytes = std::fs::read(&path).unwrap();
            count += 1;
            match ec_image::decode(&bytes) {
                Ok(image) => {
                    let (w, h) = (image.width as usize, image.height as usize);
                    assert_eq!(
                        image.to_rgba8().len(),
                        w * h * 4,
                        "{name}: buffer is not width x height"
                    );
                }
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
    }
    if count == 0 {
        skip("any");
        return;
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn gif_decodes_pixel_exactly() {
    let files = corpus("gif");
    if files.is_empty() && skip("gif") {
        return;
    }
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let theirs = image::load_from_memory(&bytes).expect("incumbent decode");
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{}",
            path.display()
        );
        assert_eq!(
            ours.to_rgba8(),
            theirs.to_rgba8().into_raw(),
            "{} (first frame)",
            path.file_name().unwrap().to_string_lossy()
        );
        eprintln!(
            "{:<22} {}x{} exact",
            path.file_name().unwrap().to_string_lossy(),
            ours.width,
            ours.height
        );
    }
}

#[test]
fn a_gif_animation_composites_frame_for_frame_like_the_incumbent() {
    use image::AnimationDecoder;

    let path = fixtures().join("animated.gif");
    if !path.exists() && skip("animated.gif") {
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let ours = ec_image::decode_animation(&bytes).expect("our animation decode");
    let theirs: Vec<image::Frame> =
        image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))
            .expect("incumbent decoder")
            .into_frames()
            .collect::<Result<Vec<_>, _>>()
            .expect("incumbent frames");

    assert_eq!(ours.len(), theirs.len(), "frame count");
    assert!(ours.len() > 1, "the fixture is not an animation");
    for (i, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        let (num, den) = theirs.delay().numer_denom_ms();
        // Both sides quote the delay in their own units; compare in
        // milliseconds, which is what a renderer actually waits.
        let ours_ms = f64::from(ours.delay_num) * 1000.0 / f64::from(ours.delay_den);
        let theirs_ms = f64::from(num) / f64::from(den);
        assert!(
            (ours_ms - theirs_ms).abs() < 0.5,
            "frame {i}: {ours_ms} ms against the incumbent's {theirs_ms} ms"
        );
        assert_eq!(
            (ours.image.width, ours.image.height),
            theirs.buffer().dimensions(),
            "frame {i} is not the whole canvas"
        );
        assert_eq!(
            ours.image.to_rgba8(),
            theirs.buffer().as_raw().clone(),
            "frame {i} samples"
        );
        eprintln!("frame {i}: {ours_ms:.0} ms, exact");
    }
}

#[test]
fn a_still_of_any_format_is_one_frame_of_animation() {
    let path = fixtures().join("rgb8.png");
    if !path.exists() && skip("rgb8.png") {
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let frames = ec_image::decode_animation(&bytes).expect("decode");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].delay_num, 0);
    assert_eq!(
        frames[0].image.to_rgba8(),
        ec_image::decode(&bytes).unwrap().to_rgba8()
    );
}

#[test]
fn an_animated_webp_composites_frame_for_frame_like_the_incumbent() {
    one_animated_webp("animated.webp");
}

fn one_animated_webp(fixture: &str) {
    use image::AnimationDecoder;

    let path = fixtures().join(fixture);
    let Ok(bytes) = std::fs::read(&path) else {
        skip(fixture);
        return;
    };
    assert!(
        ec_image::webp::is_animated(&bytes),
        "the fixture is not an animation"
    );
    let ours = ec_image::decode_animation(&bytes).expect("our animation decode");
    let theirs: Vec<image::Frame> =
        image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&bytes))
            .expect("incumbent decoder")
            .into_frames()
            .collect::<Result<Vec<_>, _>>()
            .expect("incumbent frames");

    assert_eq!(ours.len(), theirs.len(), "frame count");
    assert!(ours.len() > 1, "the fixture is not an animation");
    for (i, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        let (num, den) = theirs.delay().numer_denom_ms();
        let ours_ms = f64::from(ours.delay_num) * 1000.0 / f64::from(ours.delay_den);
        let theirs_ms = f64::from(num) / f64::from(den);
        assert!(
            (ours_ms - theirs_ms).abs() < 0.5,
            "frame {i}: {ours_ms} ms against the incumbent's {theirs_ms} ms"
        );
        assert_eq!(
            (ours.image.width, ours.image.height),
            theirs.buffer().dimensions(),
            "frame {i} is not the whole canvas"
        );
        // The frames are lossy VP8, so the payload is only as exact as the two
        // VP8 decoders agree; the bar here is on the compositing, which is
        // exact arithmetic over whatever the payload decoded to.
        let a = ours.image.to_rgba8();
        let b = theirs.buffer().as_raw();
        let max = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| x.abs_diff(y))
            .max()
            .unwrap_or(0);
        let psnr = psnr(&a, b);
        eprintln!("{fixture} frame {i}: {ours_ms:.0} ms, max delta {max}, {psnr:.1} dB");
        assert!(
            max <= 5 && psnr > 48.0,
            "{fixture} frame {i}: max delta {max}, {psnr:.1} dB"
        );
    }
}

/// Alpha blending and dispose-to-background, against libwebp itself.
///
/// The `image` crate is not the oracle here: it ignores the ANMF
/// dispose-to-background flag, so the frame after a disposal keeps pixels
/// libwebp clears -- 6912 of 6912 pixels differ on this fixture, and libwebp
/// agrees with us on every one. The goldens are libwebp's own composited
/// frames, written by `scripts/gen-still-fixtures.sh`.
#[test]
fn webp_disposal_and_blending_match_libwebp() {
    let path = fixtures().join("anim-alpha.webp");
    let Ok(bytes) = std::fs::read(&path) else {
        skip("anim-alpha.webp");
        return;
    };
    let goldens: Vec<PathBuf> = (0..)
        .map(|i| fixtures().join(format!("anim-alpha-f{i}.png")))
        .take_while(|p| p.exists())
        .collect();
    if goldens.is_empty() {
        skip("anim-alpha-f0.png (libwebp goldens; Pillow not installed?)");
        return;
    }

    let ours = ec_image::decode_animation(&bytes).expect("our animation decode");
    assert_eq!(ours.len(), goldens.len(), "frame count against libwebp");
    for (i, (ours, golden)) in ours.iter().zip(&goldens).enumerate() {
        let want = ec_image::open(golden).expect("golden");
        assert_eq!(
            (ours.image.width, ours.image.height),
            (want.width, want.height),
            "frame {i} is not the whole canvas"
        );
        assert_eq!(ours.image.to_rgba8(), want.to_rgba8(), "frame {i} samples");
        eprintln!(
            "frame {i}: {} ms, exact against libwebp",
            f64::from(ours.delay_num) * 1000.0 / f64::from(ours.delay_den)
        );
    }
    // The delays come from the same headers, but a player that gets them wrong
    // plays the animation at the wrong speed however exact the pixels are.
    let delays: Vec<u32> = ours.iter().map(|f| f.delay_num).collect();
    assert_eq!(delays, vec![60, 90, 30], "per-frame durations in ms");
}

/// BMP is a lossless container of raw samples, so the only question is whether
/// the two decoders read the same header the same way.
#[test]
fn bmp_decodes_pixel_exactly() {
    let files = corpus("bmp");
    if files.is_empty() && skip("bmp") {
        return;
    }
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.starts_with("embedded-") || name == "os2v2.bmp" {
            continue; // the incumbent refuses both; their own tests cover them
        }
        let data = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        let theirs = image::load_from_memory(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{name}: dimensions differ"
        );
        let (max, corr) = compare(&ours.to_rgba8(), &theirs.to_rgba8().into_raw());
        println!("{name:>16}  max {max}  corr {corr:.6}");
        assert_eq!(max, 0, "{name}: samples differ from the incumbent");
    }
}

/// `BI_PNG` and `BI_JPEG` are a BMP header wrapped around a whole file of
/// another format. The incumbent refuses both, so the oracle is the incumbent
/// decoding the *payload* directly: reading the wrapper must reach the same
/// picture as reading what it wraps.
#[test]
fn a_bmp_wrapping_another_format_decodes_to_that_format() {
    // The JPEG bar is the same 5 the plain JPEG comparison uses: the IDCT is
    // implementation-defined, so the two decoders round differently.
    for (fixture, bar) in [("embedded-png.bmp", 0u32), ("embedded-jpeg.bmp", 5)] {
        let path = fixtures().join(fixture);
        let Ok(data) = std::fs::read(&path) else {
            if skip(fixture) {
                return;
            }
            unreachable!()
        };
        // The payload starts where the file header says the pixels do.
        let offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
        let payload = &data[offset..];
        let ours = ec_image::decode(&data).unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let theirs = image::load_from_memory(payload).unwrap();
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{fixture}: dimensions differ"
        );
        let (max, corr) = compare(&ours.to_rgba8(), &theirs.to_rgba8().into_raw());
        println!("{fixture:>20}  max {max}  corr {corr:.6}");
        assert!(max <= bar, "{fixture}: max delta {max} over the bar {bar}");
        assert_eq!(
            ec_image::info(&data).unwrap().width,
            theirs.dimensions().0,
            "{fixture}: the header-only path disagrees with the pixels"
        );
    }
}

/// The 16-byte OS/2 v2 header, which the incumbent refuses outright
/// ("Unknown bitmap header type (size=16)") and ffmpeg reads. The oracle is
/// therefore the same picture in a header both decoders read: the two files
/// carry identical rows, so the pixels must come out identical too.
#[test]
fn an_os2_v2_header_reads_as_the_same_picture() {
    let short = fixtures().join("os2v2.bmp");
    let long = fixtures().join("os2v1.bmp");
    let (Ok(short), Ok(long)) = (std::fs::read(&short), std::fs::read(&long)) else {
        skip("os2v2.bmp");
        return;
    };
    assert!(
        image::load_from_memory(&short).is_err(),
        "the incumbent now reads a 16-byte header; this test's premise is stale"
    );
    let ours = ec_image::decode(&short).unwrap();
    let reference = image::load_from_memory(&long).unwrap();
    assert_eq!((ours.width, ours.height), reference.dimensions());
    let (max, _) = compare(&ours.to_rgba8(), &reference.to_rgba8().into_raw());
    assert_eq!(
        max, 0,
        "os2v2.bmp: samples differ from the same picture in a v1 header"
    );
}

/// TIFF stores raw samples, so like BMP the only question is whether the two
/// decoders read the same tags the same way. The corpus crosses every
/// compression with both byte orders, strips against tiles, interleaved
/// samples against planes, and five sample depths.
#[test]
fn tiff_decodes_pixel_exactly() {
    let mut files = corpus("tiff");
    files.extend(corpus("tif"));
    files.sort();
    if files.is_empty() && skip("tiff") {
        return;
    }
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "palette4.tiff" {
            continue; // the incumbent refuses a 4-bit palette; its own test covers it
        }
        if name == "rgba8-assoc.tiff" {
            continue; // the incumbent ignores ExtraSamples; its own test covers it
        }
        let data = std::fs::read(&path).unwrap();
        let ours = ec_image::decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        let theirs = image::load_from_memory(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (ours.width, ours.height),
            theirs.dimensions(),
            "{name}: dimensions differ"
        );
        let (max, corr) = compare(&ours.to_rgba8(), &theirs.to_rgba8().into_raw());
        println!("{name:>20}  max {max}  corr {corr:.6}");
        assert_eq!(max, 0, "{name}: samples differ from the incumbent");
    }
}

/// TIFF tags its alpha as either straight (ExtraSamples 1) or associated
/// (ExtraSamples 2, premultiplied). The incumbent hands back the file's raw
/// samples either way, so it cannot arbitrate the premultiplied one; the two
/// fixtures carry the same picture at alpha 0 or 255, where premultiplying is
/// lossless, so the associated file must decode to the straight one exactly.
#[test]
fn an_associated_alpha_tiff_reads_unpremultiplied() {
    let (Ok(assoc), Ok(straight)) = (
        std::fs::read(fixtures().join("rgba8-assoc.tiff")),
        std::fs::read(fixtures().join("rgba8.tiff")),
    ) else {
        skip("rgba8-assoc.tiff");
        return;
    };
    let ours = ec_image::decode(&assoc).expect("rgba8-assoc.tiff");
    let reference = ec_image::decode(&straight).expect("rgba8.tiff");
    // Under a zero alpha the colour is gone for good -- premultiplying by zero
    // is not invertible -- so those pixels are compared on their alpha alone.
    let (ours, reference) = (ours.to_rgba8(), reference.to_rgba8());
    for (i, (a, b)) in ours.chunks(4).zip(reference.chunks(4)).enumerate() {
        assert_eq!(a[3], b[3], "rgba8-assoc.tiff: alpha differs at pixel {i}");
        if b[3] != 0 {
            assert_eq!(
                a[..3],
                b[..3],
                "rgba8-assoc.tiff: the premultiplication was not undone at pixel {i}"
            );
        }
    }

    // The incumbent ignores the tag, so its two decodes differ: were that to
    // change, this test would be comparing against nothing.
    let a = image::load_from_memory(&assoc).expect("incumbent rgba8-assoc.tiff");
    let b = image::load_from_memory(&straight).expect("incumbent rgba8.tiff");
    assert_ne!(
        a.to_rgba8().into_raw(),
        b.to_rgba8().into_raw(),
        "the incumbent now honours ExtraSamples; use it as the oracle instead"
    );
}

/// A 4-bit palette, which the incumbent refuses ("Photometric interpretation
/// RGBPalette with bits per sample [4] is unsupported") and ffmpeg reads. The
/// oracle is therefore ffmpeg's own decode of the same file, written to a PNG
/// when the corpus was generated.
#[test]
fn a_four_bit_tiff_palette_reads_as_ffmpeg_reads_it() {
    let (Ok(narrow), Ok(wide)) = (
        std::fs::read(fixtures().join("palette4.tiff")),
        std::fs::read(fixtures().join("palette4-golden.png")),
    ) else {
        skip("palette4.tiff");
        return;
    };
    assert!(
        image::load_from_memory(&narrow).is_err(),
        "the incumbent now reads a 4-bit palette; this test's premise is stale"
    );
    let ours = ec_image::decode(&narrow).unwrap();
    let reference = image::load_from_memory(&wide).unwrap();
    assert_eq!((ours.width, ours.height), reference.dimensions());
    let (max, _) = compare(&ours.to_rgba8(), &reference.to_rgba8().into_raw());
    assert_eq!(max, 0, "palette4.tiff: samples differ from the same palette at 8 bits");
}
