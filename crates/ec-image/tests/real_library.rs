//! The same differential bars, against real pictures on this machine.
//!
//! Generated fixtures come out of two encoders with default settings; a real
//! library has phone JPEGs with EXIF, screenshots, scans and downloads, and
//! those are the files a user actually opens. The sweep is read-only, prints a
//! per-file table, and skips silently when the directories hold no pictures.

use image::GenericImageView;
use std::path::PathBuf;

/// Where a real library lives on this machine; absent directories are skipped.
fn library() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    ["Videos", "Downloads"]
        .iter()
        .map(|d| PathBuf::from(&home).join(d))
        .filter(|p| p.is_dir())
        .collect()
}

/// Every still under `roots`, three directory levels deep, capped.
fn stills(roots: &[PathBuf], cap: usize) -> Vec<PathBuf> {
    fn walk(dir: &PathBuf, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth - 1, out);
            } else if let Some(extension) = path.extension() {
                let extension = extension.to_string_lossy().to_lowercase();
                if ["png", "jpg", "jpeg", "webp"].contains(&extension.as_str()) {
                    out.push(path);
                }
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        walk(root, 3, &mut found);
    }
    found.sort();
    found.truncate(cap);
    found
}

#[test]
fn real_pictures_decode_like_the_incumbent() {
    let files = stills(&library(), 20);
    if files.is_empty() {
        eprintln!("skipped: no PNG/JPEG/WebP under ~/Videos or ~/Downloads");
        return;
    }
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap();
        let started = std::time::Instant::now();
        let ours = match ec_image::decode(&bytes) {
            Ok(image) => image,
            Err(e) => {
                // The incumbent's verdict decides whether this is our failure
                // or a file neither crate claims to decode.
                if image::load_from_memory(&bytes).is_ok() {
                    failures.push(format!(
                        "{name}: we refuse it, the incumbent decodes it ({e})"
                    ));
                } else {
                    eprintln!("{name:<44} both refuse: {e}");
                }
                continue;
            }
        };
        let ours_ms = started.elapsed().as_secs_f64() * 1000.0;
        let theirs = match image::load_from_memory(&bytes) {
            Ok(image) => image,
            Err(e) => {
                eprintln!("{name:<44} we decode it, the incumbent does not ({e})");
                continue;
            }
        };
        if (ours.width, ours.height) != theirs.dimensions() {
            failures.push(format!(
                "{name}: {}x{} against the incumbent's {:?}",
                ours.width,
                ours.height,
                theirs.dimensions()
            ));
            continue;
        }
        let a = ours.to_rgb8();
        let b = theirs.to_rgb8().into_raw();
        let max = a
            .iter()
            .zip(&b)
            .map(|(&x, &y)| x.abs_diff(y))
            .max()
            .unwrap_or(0);
        let lossless = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        eprintln!(
            "{name:<44} {}x{} max delta {max:>3}  {ours_ms:>7.1} ms",
            ours.width, ours.height
        );
        let bar = if lossless { 0 } else { 8 };
        if u32::from(max) > bar {
            failures.push(format!("{name}: max delta {max}, bar {bar}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} real pictures failed:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

/// Every GIF on this machine, composited frame for frame against the
/// incumbent.
///
/// The still sweep above compares RGB, which a GIF's transparent index makes
/// meaningless; an animation is only decoded correctly if the canvas, the
/// disposal method and the alpha all agree, so this one compares RGBA over
/// every frame of every file.
#[test]
fn real_gifs_animate_like_the_incumbent() {
    use image::AnimationDecoder;

    let files = walk_extension(&library(), "gif", 20);
    if files.is_empty() {
        eprintln!("skipped: no GIF under ~/Videos or ~/Downloads");
        return;
    }
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap();
        let started = std::time::Instant::now();
        let ours = match ec_image::decode_animation(&bytes) {
            Ok(frames) => frames,
            Err(e) => {
                if image::load_from_memory(&bytes).is_ok() {
                    failures.push(format!(
                        "{name}: we refuse it, the incumbent decodes it ({e})"
                    ));
                } else {
                    eprintln!("{name:<44} both refuse: {e}");
                }
                continue;
            }
        };
        let ours_ms = started.elapsed().as_secs_f64() * 1000.0;
        let theirs = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))
            .and_then(|d| d.into_frames().collect::<Result<Vec<_>, _>>());
        let theirs = match theirs {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!("{name:<44} we decode it, the incumbent does not ({e})");
                continue;
            }
        };
        if ours.len() != theirs.len() {
            failures.push(format!(
                "{name}: {} frames against the incumbent's {}",
                ours.len(),
                theirs.len()
            ));
            continue;
        }
        let mut worst = None;
        for (i, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
            if (ours.image.width, ours.image.height) != theirs.buffer().dimensions() {
                worst = Some(format!(
                    "frame {i}: {}x{} against {:?}",
                    ours.image.width,
                    ours.image.height,
                    theirs.buffer().dimensions()
                ));
                break;
            }
            let a = ours.image.to_rgba8();
            let b = theirs.buffer().as_raw();
            let delta = a
                .iter()
                .zip(b)
                .map(|(&x, &y)| x.abs_diff(y))
                .max()
                .unwrap_or(0);
            let (num, den) = theirs.delay().numer_denom_ms();
            let ours_ms = f64::from(ours.delay_num) * 1000.0 / f64::from(ours.delay_den);
            let theirs_ms = f64::from(num) / f64::from(den);
            if delta != 0 {
                worst = Some(format!("frame {i}: max delta {delta}, bar 0"));
                break;
            }
            if (ours_ms - theirs_ms).abs() >= 0.5 {
                worst = Some(format!(
                    "frame {i}: {ours_ms} ms against the incumbent's {theirs_ms} ms"
                ));
                break;
            }
        }
        eprintln!(
            "{name:<44} {}x{} {:>3} frames  {ours_ms:>7.1} ms  {}",
            ours[0].image.width,
            ours[0].image.height,
            ours.len(),
            worst.as_deref().unwrap_or("exact")
        );
        if let Some(worst) = worst {
            failures.push(format!("{name}: {worst}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} real GIFs failed:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

/// Every file under `roots` with the given extension, three levels deep.
fn walk_extension(roots: &[PathBuf], extension: &str, cap: usize) -> Vec<PathBuf> {
    fn walk(dir: &PathBuf, want: &str, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, want, depth - 1, out);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(want))
            {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        walk(root, extension, 3, &mut found);
    }
    found.sort();
    found.truncate(cap);
    found
}
