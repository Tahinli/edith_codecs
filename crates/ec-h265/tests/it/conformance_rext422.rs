//! HEVC RExt 4:2:2 (Main 4:2:2 10 and the 8-bit generated clips) against ffmpeg.
//!
//! The committed 64x64 stream is decoded by ffmpeg at run time and compared sample for
//! sample. The Sony RExt conformance streams and the generated GOP/intra clips live in the
//! gitignored fixtures (`EC_H265_REXT_DIR`, default `fixtures/vectors/hevc-rext`, with the
//! ffmpeg output of each stream under `ref/` or next to it); a missing fixture is skipped
//! and reported, a present one must decode byte-exactly.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_h265::Decoder;

const YUV422_10: &[u8] = include_bytes!("../data/refusals/yuv422_10.265");

fn decode(data: &[u8]) -> Vec<ec_h265::decode::DecodedPicture> {
    let mut dec = Decoder::new();
    dec.decode_annex_b(data).expect("decode");
    dec.flush().expect("flush");
    let mut out = Vec::new();
    while let Some(f) = dec.next_frame() {
        out.push((*f.picture).clone());
    }
    out
}

/// The first coded-size-cropped `w` x `h` window of every picture, as planar little-endian
/// samples (one byte per sample at 8 bit, two above), the layout of ffmpeg's rawvideo.
fn raw(pictures: &[ec_h265::decode::DecodedPicture], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for p in pictures {
        let wide = p.bit_depth > 8;
        for (plane, pw, cw) in [
            (&p.y, p.width, w),
            (&p.cb, p.width / 2, w / 2),
            (&p.cr, p.width / 2, w / 2),
        ] {
            for y in 0..h {
                for x in 0..cw {
                    let v = plane[y * pw + x];
                    if wide {
                        out.extend_from_slice(&v.to_le_bytes());
                    } else {
                        out.push(v as u8);
                    }
                }
            }
        }
    }
    out
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y)
}

#[test]
fn committed_422_stream_matches_ffmpeg() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skip: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join("ec-h265-rext422");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("yuv422_10.265");
    let reference = dir.join("yuv422_10.yuv");
    std::fs::write(&src, YUV422_10).unwrap();
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv422p10le"])
        .arg(&reference)
        .status()
        .unwrap();
    assert!(st.success());
    let want = std::fs::read(&reference).unwrap();
    let got = raw(&decode(YUV422_10), 64, 64);
    assert_eq!(
        first_diff(&got, &want),
        None,
        "4:2:2 10-bit differs from ffmpeg"
    );
}

fn rext_root() -> PathBuf {
    match std::env::var("EC_H265_REXT_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/hevc-rext"),
    }
}

fn find_stream(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_stream(&p) {
                return Some(f);
            }
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("bin" | "bit")) {
            return Some(p);
        }
    }
    None
}

#[test]
fn rext_422_vectors_are_byte_exact() {
    let root = rext_root();
    // (stream, ffmpeg reference, width, height)
    let mut cases: Vec<(PathBuf, PathBuf, usize, usize)> = Vec::new();
    for (name, w, h) in [
        ("GENERAL_10b_422_RExt_Sony_1", 400, 384),
        ("Main_422_10_A_RExt_Sony_2", 1920, 1080),
        ("Main_422_10_B_RExt_Sony_2", 2560, 1600),
    ] {
        if let Some(s) = find_stream(&root.join(name)) {
            cases.push((s, root.join("ref").join(format!("{name}.yuv")), w, h));
        }
    }
    for name in [
        "i422_8",
        "g422_8",
        "i422_10",
        "g422_10",
        "g422_8_tools",
        "g422_10_tools",
    ] {
        cases.push((
            root.join("gen").join(format!("{name}.265")),
            root.join("gen").join(format!("{name}.yuv")),
            352,
            288,
        ));
    }
    let mut checked = 0;
    for (stream, reference, w, h) in cases {
        let (Ok(data), Ok(want)) = (std::fs::read(&stream), std::fs::read(&reference)) else {
            eprintln!("skip (fixture missing): {}", stream.display());
            continue;
        };
        let got = raw(&decode(&data), w, h);
        assert_eq!(
            first_diff(&got, &want),
            None,
            "{} differs from ffmpeg",
            stream.display()
        );
        checked += 1;
    }
    eprintln!("4:2:2 vectors checked byte-exact: {checked}");
}
