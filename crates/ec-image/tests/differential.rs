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
            if name.contains("animated") {
                continue; // refused by name; `an_animated_webp_is_refused_by_name` covers it
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
