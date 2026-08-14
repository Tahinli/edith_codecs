//! Every VP9 fixture stream parsed end to end, field-compared against ffprobe.
//!
//! `scripts/gen-bitstream-fixtures.sh` remuxes the container fixtures to IVF and
//! encodes the branch cases (superframes, 4:4:4, tiles). Fixtures are gitignored,
//! so a checkout without them skips rather than fails.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::Error;
use ec_vp9_syntax::{FrameType, Vp9Parser, superframe};

fn bitstreams() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bitstreams")
}

/// The IVF frames of a file, payload only.
fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 32; // DKIF file header
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

fn ffprobe(path: &Path, entries: &str) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(entries)
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end_matches(',').to_string())
        .collect()
}

/// `(width, height, pix_fmt, profile)` as ffprobe sees the stream.
fn probe_stream(path: &Path) -> (u32, u32, String, u8) {
    let line = ffprobe(path, "stream=profile,width,height,pix_fmt")
        .into_iter()
        .next()
        .expect("one video stream");
    let f: Vec<&str> = line.split(',').collect();
    let profile = f[0]
        .rsplit(' ')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("unexpected VP9 profile {:?}", f[0]));
    (
        f[1].parse().unwrap(),
        f[2].parse().unwrap(),
        f[3].to_string(),
        profile,
    )
}

/// `bit_depth, subsampling_x, subsampling_y` implied by an ffmpeg pixel format.
fn pix_fmt_geometry(pix_fmt: &str) -> (u8, u8, u8) {
    let depth = if pix_fmt.contains("10le") {
        10
    } else if pix_fmt.contains("12le") {
        12
    } else {
        8
    };
    let (sx, sy) = if pix_fmt.starts_with("yuv444") {
        (0, 0)
    } else if pix_fmt.starts_with("yuv422") {
        (1, 0)
    } else {
        (1, 1)
    };
    (depth, sx, sy)
}

fn fixtures() -> Vec<PathBuf> {
    let dir = bitstreams();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "ivf")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("vp9-"))
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_fixture_matches_ffprobe() {
    let files = fixtures();
    if files.is_empty() {
        eprintln!(
            "skipped: no fixtures/bitstreams/vp9-*.ivf — run scripts/gen-bitstream-fixtures.sh"
        );
        return;
    }
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let (width, height, pix_fmt, profile) = probe_stream(&path);
        let (bit_depth, sx, sy) = pix_fmt_geometry(&pix_fmt);
        let ffprobe_keys: Vec<bool> = ffprobe(&path, "frame=key_frame")
            .iter()
            .map(|l| l == "1")
            .collect();

        let data = std::fs::read(&path).unwrap();
        let mut parser = Vp9Parser::new();
        let mut shown = Vec::new();
        let mut coded = 0usize;
        let mut superframes = 0usize;
        let mut show_existing = 0usize;
        for chunk in ivf_frames(&data) {
            let frames = superframe::split(chunk)
                .unwrap_or_else(|e| panic!("{name}: superframe split: {e}"));
            if frames.len() > 1 {
                superframes += 1;
            }
            for frame in frames {
                let h = parser
                    .parse_frame(frame)
                    .unwrap_or_else(|e| panic!("{name}: frame {coded}: {e}"));
                coded += 1;
                if h.show_existing_frame {
                    show_existing += 1;
                    shown.push(false);
                    continue;
                }
                assert_eq!(h.profile, profile, "{name}: profile");
                assert_eq!(h.bit_depth, bit_depth, "{name}: bit depth ({pix_fmt})");
                assert_eq!(
                    (h.subsampling_x, h.subsampling_y),
                    (sx, sy),
                    "{name}: subsampling"
                );
                assert_eq!((h.width, h.height), (width, height), "{name}: coded size");
                assert_eq!(
                    (h.render_width, h.render_height),
                    (width, height),
                    "{name}: render size"
                );
                assert!(
                    h.header_size_in_bytes > 0,
                    "{name}: empty compressed header"
                );
                assert!(
                    (h.uncompressed_header_size as usize) < frame.len(),
                    "{name}: header longer than the frame"
                );
                if name.contains("tiles") {
                    // -tile-columns 2 asks libvpx for 2^2 tile columns, which
                    // 1280 samples (20 superblocks) is just wide enough to allow.
                    assert_eq!(h.tile_info.cols_log2, 2, "{name}: tile columns");
                }
                if h.show_frame {
                    shown.push(h.frame_type == FrameType::Key);
                }
            }
        }

        assert_eq!(
            shown.len(),
            ffprobe_keys.len(),
            "{name}: shown frame count ({coded} coded, {superframes} superframes, {show_existing} show_existing)"
        );
        assert_eq!(shown, ffprobe_keys, "{name}: key frame flags");
        println!(
            "{name}: {coded} coded / {} shown, {superframes} superframes, {show_existing} show_existing, \
             profile {profile}, {bit_depth}-bit, {width}x{height}",
            shown.len()
        );
    }
}

/// The superframe path on real bytes, and the `show_existing_frame` path on the
/// reference state those bytes leave behind.
///
/// libvpx's VP9 encoder shows a hidden ALTREF by coding an overlay frame rather
/// than by emitting a `show_existing_frame` header, so no ffmpeg command
/// produces one here — the fixture gives real superframes and real hidden
/// frames, and the eight-bit `show_existing_frame` header is appended by hand
/// against the reference slots the fixture filled. (The AV1 fixtures do carry
/// genuine `show_existing_frame` headers; libaom uses them.)
#[test]
fn altref_fixture_has_superframes_and_hidden_frames() {
    let path = bitstreams().join("vp9-superframe-altref.ivf");
    if !path.exists() {
        eprintln!("skipped: no vp9-superframe-altref.ivf");
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let mut parser = Vp9Parser::new();
    let (mut superframes, mut hidden) = (0, 0);
    for chunk in ivf_frames(&data) {
        let frames = superframe::split(chunk).unwrap();
        if frames.len() > 1 {
            superframes += 1;
        }
        for frame in frames {
            let h = parser.parse_frame(frame).unwrap();
            if !h.show_frame && !h.show_existing_frame {
                hidden += 1;
            }
        }
    }
    assert!(superframes > 0, "libvpx altref did not produce superframes");
    assert!(hidden > 0, "no hidden ALTREF frame");

    // marker 10, profile bits 0 0, show_existing_frame 1, slot 101.
    let h = parser.parse_frame(&[0b1000_1101]).unwrap();
    assert!(h.show_existing_frame);
    assert_eq!(h.frame_to_show_map_idx, 5);
    assert_eq!(h.uncompressed_header_size, 1);
    let slot = parser.reference_slots()[5];
    assert!(slot.valid, "slot 5 was never refreshed by the fixture");
    assert_eq!((h.width, h.height), (slot.width, slot.height));
}

/// Truncation, bit flips and random noise: an error, never a panic.
#[test]
fn fuzz_10k_mutations_never_panics() {
    let seeds: Vec<Vec<u8>> = fixtures()
        .iter()
        .take(3)
        .filter_map(|p| std::fs::read(p).ok())
        .flat_map(|data| {
            ivf_frames(&data)
                .into_iter()
                .take(4)
                .map(|f| f[..f.len().min(512)].to_vec())
                .collect::<Vec<_>>()
        })
        .collect();
    let seeds = if seeds.is_empty() {
        // No fixtures: still exercise the parser against pure noise.
        vec![vec![0x82u8; 128]]
    } else {
        seeds
    };

    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut errors = 0usize;
    for i in 0..10_000 {
        let mut buf = seeds[(rng() as usize) % seeds.len()].clone();
        let mutations = 1 + (rng() as usize) % 8;
        for _ in 0..mutations {
            let at = (rng() as usize) % buf.len();
            buf[at] ^= (rng() % 256) as u8;
        }
        if i % 3 == 0 {
            buf.truncate((rng() as usize) % buf.len().max(1));
        }
        let mut parser = Vp9Parser::new();
        for frame in superframe::split(&buf).unwrap_or_else(|_| vec![&buf]) {
            match parser.parse_frame(frame) {
                Ok(_) => {}
                Err(Error::NeedMore | Error::Corrupt { .. }) => errors += 1,
                Err(e) => panic!("unexpected error kind: {e}"),
            }
        }
    }
    println!("fuzz: 10000 mutations, {errors} rejected");
}
