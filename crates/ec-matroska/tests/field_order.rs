//! Field order in the containers: a video track this workspace writes states
//! its scan in an mp4 `fiel` box and in Matroska `FlagInterlaced`/`FieldOrder`,
//! because ffmpeg's HEVC decoder cannot derive it from the bitstream and
//! reports `unknown` without them.
//!
//! Sources are synthetic (ffmpeg `testsrc2`). A case skips where ffmpeg lacks
//! the encoder, unless `EC_REQUIRE_FIXTURES` is set.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Demuxer, Error, Interlace, Muxer, Packet, StreamInfo};
use ec_matroska::{MatroskaDemuxer, MatroskaMuxer};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

fn skip(why: &str) -> bool {
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES set but: {why}"
    );
    eprintln!("skip: {why}");
    true
}

fn source(name: &str, encoder: &str) -> Option<(PathBuf, Vec<StreamInfo>, Vec<Packet>)> {
    let enc = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output();
    let have =
        enc.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(" {encoder} ")));
    if !have {
        skip(&format!("no ffmpeg or no {encoder}"));
        return None;
    }
    let dir = std::env::temp_dir().join(format!("ec-field-order-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=320x240:rate=25:duration=1")
        .args(["-c:v", encoder, "-pix_fmt", "yuv420p", "-f", "mp4"])
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg encode with {encoder}");
    let mut d = Mp4Demuxer::new(BufReader::new(File::open(&src).unwrap())).unwrap();
    let streams = d.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => packets.push(p),
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    Some((dir, streams, packets))
}

fn mux_mp4(dst: &Path, s: &[StreamInfo], p: &[Packet], order: Option<Option<Interlace>>) {
    let mut m = Mp4Muxer::new(BufWriter::new(File::create(dst).unwrap())).unwrap();
    for st in s {
        m.add_stream(st.clone()).unwrap();
    }
    if let Some(o) = order {
        m.set_field_order(0, o).unwrap();
    }
    for pk in p {
        m.write_packet(pk).unwrap();
    }
    m.finish().unwrap();
}

fn mux_mkv(dst: &Path, s: &[StreamInfo], p: &[Packet], order: Option<Option<Interlace>>) {
    let mut m = MatroskaMuxer::new(BufWriter::new(File::create(dst).unwrap()));
    for st in s {
        m.add_stream(st.clone()).unwrap();
    }
    if let Some(o) = order {
        m.set_field_order(0, o).unwrap();
    }
    for pk in p {
        m.write_packet(pk).unwrap();
    }
    m.finish().unwrap();
}

fn probe(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=field_order")
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn mp4_order(path: &Path) -> Option<Interlace> {
    Mp4Demuxer::new(BufReader::new(File::open(path).unwrap()))
        .unwrap()
        .field_order(0)
}

fn mkv_order(path: &Path) -> Option<Interlace> {
    MatroskaDemuxer::new(BufReader::new(File::open(path).unwrap()))
        .unwrap()
        .field_order(0)
}

fn run(name: &str, encoder: &str, unstated: &str) {
    let Some((dir, s, p)) = source(name, encoder) else {
        return;
    };
    let ffprobe = Command::new("ffprobe").arg("-version").output().is_ok();
    if !ffprobe {
        skip("no ffprobe");
        return;
    }
    let cases = [
        (None, "progressive", Some(Interlace::Progressive)),
        (
            Some(Some(Interlace::Progressive)),
            "progressive",
            Some(Interlace::Progressive),
        ),
        (
            Some(Some(Interlace::TopFieldFirst)),
            "tt",
            Some(Interlace::TopFieldFirst),
        ),
        (
            Some(Some(Interlace::BottomFieldFirst)),
            "bb",
            Some(Interlace::BottomFieldFirst),
        ),
        (Some(None), unstated, None),
    ];
    for (set, want, round) in cases {
        let mp4 = dir.join("o.mp4");
        mux_mp4(&mp4, &s, &p, set);
        assert_eq!(probe(&mp4), want, "{name} mp4 {set:?}");
        assert_eq!(mp4_order(&mp4), round, "{name} mp4 round trip {set:?}");
        let mkv = dir.join("o.mkv");
        mux_mkv(&mkv, &s, &p, set);
        assert_eq!(probe(&mkv), want, "{name} mkv {set:?}");
        assert_eq!(mkv_order(&mkv), round, "{name} mkv round trip {set:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn h264_field_order_in_mp4_and_mkv() {
    // ffmpeg's H.264 decoder derives progressive from frame_mbs_only itself.
    run("h264", "libx264", "progressive");
}

#[test]
fn hevc_field_order_in_mp4_and_mkv() {
    run("hevc", "libx265", "unknown");
}
