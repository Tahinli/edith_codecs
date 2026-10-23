//! Uncompressed video in QuickTime (`raw `, `2vuy`, `yuv2`, `v210`, `v410`, `b16g`), compared
//! with ffmpeg's decode of the same .mov. Media is synthetic. Without ffmpeg the tests skip
//! unless `EC_REQUIRE_FIXTURES` is set.

use std::path::PathBuf;
use std::process::Command;

use ec_core::frame::Frame;
use ec_core::registry::{CodecId, Decoder, Demuxer};
use ec_riff::RawVideoDecoder;
use crate::common::frame_bytes;

fn oracle() -> bool {
    if Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "ffmpeg missing and EC_REQUIRE_FIXTURES is set"
    );
    eprintln!("SKIP: no ffmpeg");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-riff-mov-{}-{tag}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(args)
        .output()
        .expect("ffmpeg");
    assert!(
        o.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout
}

#[test]
fn mov_raw_layouts_match_ffmpeg() {
    if !oracle() {
        return;
    }
    // (tag, size, ffmpeg codec, source pix_fmt, expected sample entry, pix_fmt we compare in)
    for (tag, size, codec, pix, entry, ff) in [
        ("rgb24", "98x66", "rawvideo", "rgb24", b"raw ", "rgb24"),
        ("rgb24odd", "97x65", "rawvideo", "rgb24", b"raw ", "rgb24"),
        ("argb", "98x66", "rawvideo", "argb", b"raw ", "bgra"),
        ("rgb555", "98x66", "rawvideo", "rgb555be", b"raw ", "bgr24"),
        (
            "rgb555odd",
            "97x65",
            "rawvideo",
            "rgb555be",
            b"raw ",
            "bgr24",
        ),
        ("uyvy", "98x66", "rawvideo", "uyvy422", b"2vuy", "yuv422p"),
        (
            "uyvyodd", "97x65", "rawvideo", "uyvy422", b"2vuy", "yuv422p",
        ),
        ("yuvs", "98x66", "rawvideo", "yuyv422", b"yuvs", "yuv422p"),
        (
            "yuvsodd", "97x65", "rawvideo", "yuyv422", b"yuvs", "yuv422p",
        ),
        ("yuv2", "98x66", "rawvideo", "yuyv422", b"yuv2", "yuv422p"),
        (
            "yuv2odd", "97x65", "rawvideo", "yuyv422", b"yuv2", "yuv422p",
        ),
        (
            "v210",
            "98x66",
            "v210",
            "yuv422p10le",
            b"v210",
            "yuv422p10le",
        ),
        (
            "v210odd",
            "97x65",
            "v210",
            "yuv422p10le",
            b"v210",
            "yuv422p10le",
        ),
        (
            "v410",
            "98x66",
            "v410",
            "yuv444p10le",
            b"v410",
            "yuv444p10le",
        ),
        ("b16g", "98x66", "rawvideo", "gray16be", b"b16g", "gray16le"),
    ] {
        eprintln!("case {tag}");
        let vtag = std::str::from_utf8(entry).unwrap();
        let path = scratch(tag).join("v.mov");
        let src = format!("testsrc2=size={size}:rate=25:duration=2");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &src,
            "-c:v",
            codec,
            "-pix_fmt",
            pix,
            "-vtag",
            vtag,
            "-f",
            "mov",
            path.to_str().unwrap(),
        ]);
        let want = ffmpeg(&[
            "-i",
            path.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            ff,
            "-",
        ]);
        let mut d =
            ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()))
                .unwrap();
        let p = d.streams()[0].params.clone();
        assert_eq!(p.codec, CodecId::RawVideo, "{tag}");
        let mut dec = RawVideoDecoder::new(p).unwrap();
        let mut got = Vec::new();
        let mut n = 0;
        while let Ok(pk) = d.next_packet() {
            dec.send_packet(&pk).unwrap();
            while let Ok(Frame::Video(f)) = dec.receive_frame() {
                got.extend(frame_bytes(&f));
                n += 1;
            }
        }
        assert_eq!(n, 50, "{tag}");
        assert_eq!(got.len(), want.len(), "{tag}: size");
        assert!(got == want, "{tag}: decoded frames differ from ffmpeg");
    }
}
