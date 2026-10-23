//! Our DVB subtitle decode against ffmpeg's (sub2video frames: `[0:s:0]` out as raw RGBA).
//!
//! Fixtures are tiny files made by `scripts/gen-bitmap-sub-fixtures.sh` from a synthetic
//! 720x576 PGS source (four cues). ffmpeg is the oracle; without it the tests skip loudly, and
//! with `EC_REQUIRE_FIXTURES=1` a missing ffmpeg fails.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Command;

use ec_core::{Decoder, Demuxer, Error};
use ec_dvbsub::DvbSubDecoder;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

struct Oracle {
    /// (pts seconds, rgba) per ffmpeg frame, blank ones included.
    frames: Vec<(f64, Vec<u8>)>,
}

fn oracle(path: &PathBuf, w: usize, h: usize) -> Option<Oracle> {
    let out = Command::new("ffmpeg")
        .args(["-v", "info", "-nostdin", "-canvas_size", &format!("{w}x{h}"), "-i"])
        .arg(path)
        .args([
            "-filter_complex",
            "[0:s:0]showinfo[s]",
            "-map",
            "[s]",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) => {
            assert!(
                std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
                "EC_REQUIRE_FIXTURES=1 but ffmpeg is not installed"
            );
            eprintln!("SKIP: ffmpeg not installed");
            return None;
        }
    };
    assert!(out.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stderr);
    let times: Vec<f64> = text
        .lines()
        .filter(|l| l.contains("showinfo") && l.contains(" n:"))
        .filter_map(|l| l.split("pts_time:").nth(1)?.split_whitespace().next()?.parse().ok())
        .collect();
    let fsize = w * h * 4;
    assert_eq!(out.stdout.len(), times.len() * fsize, "frame count mismatch");
    Some(Oracle {
        frames: times
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, out.stdout[i * fsize..(i + 1) * fsize].to_vec()))
            .collect(),
    })
}

fn blank(f: &[u8]) -> bool {
    f.chunks(4).all(|p| p[3] == 0)
}


/// (rgba, start s, end s) per non-empty display set.
fn run_packets(d: &mut dyn Demuxer, stream: u32) -> Vec<(Vec<u8>, f64, Option<f64>)> {
    let mut dec = DvbSubDecoder::new();
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream == stream => {
                let pts = p.pts.expect("subtitle packets carry pts") as f64 * p.time_base.num() as f64
                    / p.time_base.den() as f64;
                dec.send_packet(&p).unwrap();
                while let Some(c) = dec.take_cue() {
                    if c.empty {
                        continue;
                    }
                    out.push((
                        c.frame.planes[0].data.to_vec(),
                        pts,
                        Some(pts + f64::from(c.timeout_ms) / 1000.0),
                    ));
                }
            }
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    out
}

/// Our non-empty sets against ffmpeg's non-blank frames. Times are compared as offsets from the
/// first cue (the files differ in muxing delay); ends only when the oracle shows a following
/// frame, with ffmpeg's own clear-frame time (it also applies the page timeout).
fn compare(ours: &[(Vec<u8>, f64, Option<f64>)], o: &Oracle, what: &str) {
    let mut shown: Vec<usize> = Vec::new();
    for i in 0..o.frames.len() {
        // sub2video repeats the current picture; one cue is the first frame of a run.
        let repeat = i > 0 && o.frames[i].1 == o.frames[i - 1].1;
        if !blank(&o.frames[i].1) && !repeat {
            shown.push(i);
        }
    }
    assert_eq!(shown.len(), ours.len(), "{what}: cue count");
    assert!(ours.len() >= 4, "{what}: expected the four fixture cues");
    let off = ours[0].1 - o.frames[shown[0]].0;
    for (k, &i) in shown.iter().enumerate() {
        let (px, start, _end) = &ours[k];
        let (t, theirs) = &o.frames[i];
        assert!((start - off - t).abs() < 0.002, "{what}: cue {k} start {start} vs ffmpeg {t}");
        let mut diff = 0;
        let mut maxd = 0i32;
        for (a, b) in px.chunks(4).zip(theirs.chunks(4)) {
            if a[3] != b[3] {
                diff += 1;
            } else if a[3] != 0 {
                let d = (0..3).map(|c| (i32::from(a[c]) - i32::from(b[c])).abs()).max().unwrap();
                maxd = maxd.max(d);
                if d != 0 {
                    diff += 1;
                }
            }
        }
        eprintln!("{what}: cue {k}: {diff} differing pixels, max channel delta {maxd}");
        assert_eq!(diff, 0, "{what}: cue {k} differs in {diff} pixels (max delta {maxd})");
    }
}

#[test]
fn mkv_dvbsub_matches_ffmpeg() {
    let path = data("dvb.mkv");
    let Some(o) = oracle(&path, 720, 576) else { return };
    let mut d = ec_matroska::MatroskaDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    assert_eq!(d.streams()[0].params.codec, ec_core::CodecId::DvbSub);
    let ours = run_packets(&mut d, 0);
    compare(&ours, &o, "mkv");
}

#[test]
fn ts_private_pes_matches_ffmpeg() {
    let path = data("dvb.ts");
    let Some(o) = oracle(&path, 720, 576) else { return };
    let mut d = ec_mpegts::TsDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let sub = d
        .streams()
        .iter()
        .position(|s| s.params.codec == ec_core::CodecId::DvbSub)
        .expect("the 0x06 + subtitling-descriptor stream is a DvbSub stream") as u32;
    let ours = run_packets(&mut d, sub);
    compare(&ours, &o, "ts");
}
