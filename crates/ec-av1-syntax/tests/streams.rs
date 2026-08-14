//! Every AV1 fixture stream parsed end to end, field-compared against ffprobe.
//!
//! `scripts/gen-bitstream-fixtures.sh` remuxes the container fixtures to IVF and
//! encodes the branch cases (4:4:4, monochrome, tiles, hidden ALTREF). Fixtures
//! are gitignored, so a checkout without them skips rather than fails.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_av1_syntax::{Av1Parser, FrameType, ObuKind};
use ec_core::Error;

fn bitstreams() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bitstreams")
}

/// The IVF frames of a file — for AV1, each is one temporal unit.
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

/// `(width, height, pix_fmt, seq_profile)` as ffprobe sees the stream.
fn probe_stream(path: &Path) -> (u32, u32, String, u8) {
    let line = ffprobe(path, "stream=profile,width,height,pix_fmt")
        .into_iter()
        .next()
        .expect("one video stream");
    let f: Vec<&str> = line.split(',').collect();
    let profile = match f[0] {
        "Main" => 0,
        "High" => 1,
        "Professional" => 2,
        other => panic!("unexpected AV1 profile {other:?}"),
    };
    (
        f[1].parse().unwrap(),
        f[2].parse().unwrap(),
        f[3].to_string(),
        profile,
    )
}

/// `bit_depth, mono_chrome, subsampling_x, subsampling_y` from a pixel format.
fn pix_fmt_geometry(pix_fmt: &str) -> (u8, bool, u8, u8) {
    let depth = if pix_fmt.contains("10le") {
        10
    } else if pix_fmt.contains("12le") {
        12
    } else {
        8
    };
    if pix_fmt.starts_with("gray") {
        // Monochrome codes subsampling as 4:2:0 and has no chroma planes.
        return (depth, true, 1, 1);
    }
    let (sx, sy) = if pix_fmt.starts_with("yuv444") {
        (0, 0)
    } else if pix_fmt.starts_with("yuv422") {
        (1, 0)
    } else {
        (1, 1)
    };
    (depth, false, sx, sy)
}

fn fixtures() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(bitstreams()) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "ivf")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("av1-"))
        })
        .collect();
    files.sort();
    files
}

/// One pass over a stream: what the parser saw.
#[derive(Default)]
struct Tally {
    frames: usize,
    shown: Vec<bool>,
    show_existing: usize,
    hidden: usize,
    tiles: usize,
    max_tiles_per_frame: usize,
}

#[test]
fn every_fixture_matches_ffprobe() {
    let files = fixtures();
    if files.is_empty() {
        eprintln!(
            "skipped: no fixtures/bitstreams/av1-*.ivf — run scripts/gen-bitstream-fixtures.sh"
        );
        return;
    }
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let (width, height, pix_fmt, profile) = probe_stream(&path);
        let (bit_depth, mono, sx, sy) = pix_fmt_geometry(&pix_fmt);
        let ffprobe_keys: Vec<bool> = ffprobe(&path, "frame=key_frame")
            .iter()
            .map(|l| l == "1")
            .collect();

        let data = std::fs::read(&path).unwrap();
        let mut parser = Av1Parser::new();
        let mut t = Tally::default();
        let mut sequences = 0usize;

        for (unit, chunk) in ivf_frames(&data).iter().enumerate() {
            let obus = parser
                .parse_temporal_unit(chunk)
                .unwrap_or_else(|e| panic!("{name}: temporal unit {unit}: {e}"));
            for obu in obus {
                match &obu.kind {
                    ObuKind::SequenceHeader(seq) => {
                        sequences += 1;
                        assert_eq!(seq.seq_profile, profile, "{name}: seq_profile");
                        let c = seq.color_config;
                        assert_eq!(c.bit_depth, bit_depth, "{name}: bit depth ({pix_fmt})");
                        assert_eq!(c.mono_chrome, mono, "{name}: mono_chrome");
                        assert_eq!(
                            (c.subsampling_x, c.subsampling_y),
                            (sx, sy),
                            "{name}: subsampling"
                        );
                        assert_eq!(c.num_planes, if mono { 1 } else { 3 }, "{name}: planes");
                        assert_eq!(seq.max_frame_width, width, "{name}: max width");
                        assert_eq!(seq.max_frame_height, height, "{name}: max height");
                    }
                    ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => {
                        t.frames += 1;
                        if h.show_existing_frame {
                            t.show_existing += 1;
                            t.shown.push(false);
                            continue;
                        }
                        assert_eq!(
                            (h.upscaled_width, h.frame_height),
                            (width, height),
                            "{name}: frame size"
                        );
                        assert_eq!(
                            (h.render_width, h.render_height),
                            (width, height),
                            "{name}: render size"
                        );
                        assert!(h.tile_info.cols >= 1 && h.tile_info.rows >= 1);
                        if h.show_frame {
                            t.shown.push(h.frame_type == FrameType::Key);
                        } else {
                            t.hidden += 1;
                        }
                    }
                    _ => {}
                }
                if let ObuKind::Frame(_, tiles) | ObuKind::TileGroup(tiles) = &obu.kind {
                    t.tiles += tiles.len();
                    t.max_tiles_per_frame = t.max_tiles_per_frame.max(tiles.len());
                    for tile in tiles {
                        // Every tile must lie inside the temporal unit it came from.
                        assert!(
                            tile.offset + tile.size <= chunk.len(),
                            "{name}: tile {} runs past the temporal unit",
                            tile.tile_num
                        );
                        assert!(tile.size > 0, "{name}: empty tile");
                    }
                }
            }
        }

        assert!(sequences > 0, "{name}: no sequence header");
        assert_eq!(
            t.shown.len(),
            ffprobe_keys.len(),
            "{name}: shown frame count ({} coded, {} hidden, {} show_existing)",
            t.frames,
            t.hidden,
            t.show_existing
        );
        assert_eq!(t.shown, ffprobe_keys, "{name}: key frame flags");
        if name.contains("tiles") {
            assert_eq!(
                t.max_tiles_per_frame, 4,
                "{name}: -tiles 2x2 should give 4 tiles"
            );
        }
        println!(
            "{name}: {} coded / {} shown, {} hidden, {} show_existing, {} tiles (max {}/frame), \
             profile {profile}, {bit_depth}-bit{}, {width}x{height}",
            t.frames,
            t.shown.len(),
            t.hidden,
            t.show_existing,
            t.tiles,
            t.max_tiles_per_frame,
            if mono { " mono" } else { "" }
        );
    }
}

/// libaom shows its hidden ALTREF with a `show_existing_frame` header; that path
/// resolves a real reference slot, which is the one thing a synthetic header
/// cannot prove.
#[test]
fn altref_fixture_has_hidden_frames_and_show_existing() {
    let path = bitstreams().join("av1-altref.ivf");
    if !path.exists() {
        eprintln!("skipped: no av1-altref.ivf");
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let mut parser = Av1Parser::new();
    let (mut hidden, mut show_existing, mut showable) = (0usize, 0usize, 0usize);
    for chunk in ivf_frames(&data) {
        for obu in parser.parse_temporal_unit(chunk).unwrap() {
            if let ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) = &obu.kind {
                if h.show_existing_frame {
                    show_existing += 1;
                    let slot = parser.reference_slots()[h.frame_to_show_map_idx as usize];
                    assert!(slot.valid, "show_existing_frame names an empty slot");
                    assert_eq!(
                        (h.frame_width, h.frame_height),
                        (slot.frame_width, slot.frame_height)
                    );
                } else if !h.show_frame {
                    hidden += 1;
                    // A hidden frame that is shown later must say so; one that is
                    // only ever a reference need not, and libaom codes both.
                    showable += usize::from(h.showable_frame);
                }
            }
        }
    }
    assert!(hidden > 0, "no hidden ALTREF frame");
    assert!(show_existing > 0, "no show_existing_frame header");
    assert!(showable > 0, "no hidden frame was marked showable");
    println!(
        "av1-altref.ivf: {hidden} hidden ({showable} showable), {show_existing} show_existing"
    );
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
        vec![vec![
            0x0au8, 0x0b, 0x00, 0x00, 0x00, 0x24, 0xcf, 0x7f, 0x0d, 0xbf, 0xff, 0x30, 0x08,
        ]]
    } else {
        seeds
    };

    let mut state = 0x9e37_79b9_7f4a_7c15u64;
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
        let mut parser = Av1Parser::new();
        match parser.parse_temporal_unit(&buf) {
            Ok(_) => {}
            Err(Error::NeedMore | Error::Corrupt { .. }) => errors += 1,
            Err(e) => panic!("unexpected error kind: {e}"),
        }
    }
    println!("fuzz: 10000 mutations, {errors} rejected");
}
