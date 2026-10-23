//! DTS in Matroska: `A_DTS` (and its `/EXPRESS` and `/LOSSLESS` spellings) is
//! `CodecId::Dts` with the track's rate and channels, the frames come out
//! byte-identical to what ffmpeg copies, and a remux writes `A_DTS` back.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use ec_core::{CodecId, Demuxer, MediaParameters, Muxer};
use ec_matroska::{MatroskaDemuxer, MatroskaMuxer};

fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some_and(|v| !v.is_empty() && v != "0")
}

/// A DTS stream muxed by ffmpeg into `ext`'s container, plus the raw frames.
fn ffmpeg_dts(ext: &str, channels: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir: PathBuf = std::env::temp_dir().join("ec-matroska-dts");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(format!("{}-{channels}.{ext}", std::process::id()));
    let raw = dir.join(format!("{}-{channels}.dts", std::process::id()));
    let run = |args: &[&str]| {
        Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let ok = run(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=0.5",
        "-ac",
        channels,
        "-c:a",
        "dca",
        "-strict",
        "-2",
        out.to_str().unwrap(),
    ]) && run(&[
        "-i",
        out.to_str().unwrap(),
        "-c",
        "copy",
        raw.to_str().unwrap(),
    ]);
    if !ok {
        assert!(!required(), "ffmpeg cannot make a DTS fixture");
        eprintln!("skip: no ffmpeg with the dca encoder");
        return None;
    }
    let pair = (std::fs::read(&out).unwrap(), std::fs::read(&raw).unwrap());
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(raw);
    Some(pair)
}

fn demux_all(bytes: Vec<u8>) -> (MatroskaDemuxer<Cursor<Vec<u8>>>, Vec<ec_core::Packet>) {
    let mut d = MatroskaDemuxer::new(Cursor::new(bytes)).unwrap();
    let mut pkts = Vec::new();
    while let Ok(p) = d.next_packet() {
        pkts.push(p);
    }
    (d, pkts)
}

fn check(channels: &str, want: usize) {
    let Some((mkv, raw)) = ffmpeg_dts("mkv", channels) else {
        return;
    };
    let (d, pkts) = demux_all(mkv);
    let s = &d.streams()[0];
    assert_eq!(s.params.codec, CodecId::Dts);
    assert_eq!(d.track_codec_id(0), Some("A_DTS"));
    let MediaParameters::Audio(a) = &s.params.media else {
        panic!("not audio");
    };
    assert_eq!((a.sample_rate, a.layout.channel_count()), (48_000, want));
    let ours: Vec<u8> = pkts.iter().flat_map(|p| p.data.to_vec()).collect();
    assert_eq!(ours, raw, "frames differ from ffmpeg's copy");

    // Remux: the muxer names the track A_DTS and the frames survive.
    let mut out = Cursor::new(Vec::new());
    {
        let mut m = MatroskaMuxer::new(&mut out);
        m.add_stream(s.clone()).unwrap();
        for p in &pkts {
            m.write_packet(p).unwrap();
        }
        m.finish().unwrap();
    }
    let (d2, pkts2) = demux_all(out.into_inner());
    assert_eq!(d2.streams()[0].params.codec, CodecId::Dts);
    assert_eq!(d2.track_codec_id(0), Some("A_DTS"));
    let again: Vec<u8> = pkts2.iter().flat_map(|p| p.data.to_vec()).collect();
    assert_eq!(again, raw);
}

#[test]
fn a_dts_stereo() {
    check("2", 2);
}

#[test]
fn a_dts_51() {
    check("6", 6);
}
