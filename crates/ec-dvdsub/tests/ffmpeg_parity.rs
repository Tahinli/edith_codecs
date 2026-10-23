//! Our subpicture decode against ffmpeg's (sub2video frames: `[0:s:0]` out as raw RGBA).
//!
//! Fixtures are tiny files made by `scripts/gen-bitmap-sub-fixtures.sh` from a synthetic
//! 720x576 PGS source (four cues). ffmpeg is the oracle; without it the tests skip loudly, and
//! with `EC_REQUIRE_FIXTURES=1` a missing ffmpeg fails.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Command;

use ec_core::{Demuxer, Error, Frame, Decoder};
use ec_dvdsub::{DvdSubDecoder, parse_idx, sub_file_units};

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

/// Compares our cues (frame, start s, end s) with ffmpeg's non-blank frames and their ends.
fn compare(ours: &[(Vec<u8>, f64, Option<f64>)], o: &Oracle, what: &str) {
    compare_shifted(ours, o, what, false)
}

/// `shifted`: our times are the oracle's plus one constant (same cues, different muxing).
fn compare_shifted(ours: &[(Vec<u8>, f64, Option<f64>)], o: &Oracle, what: &str, shifted: bool) {
    let off = match shifted {
        true => {
            let first = (0..o.frames.len()).find(|&i| !blank(&o.frames[i].1)).unwrap();
            ours[0].1 - o.frames[first].0
        }
        false => 0.0,
    };
    let shown: Vec<usize> = (0..o.frames.len()).filter(|&i| !blank(&o.frames[i].1)).collect();
    assert_eq!(shown.len(), ours.len(), "{what}: cue count");
    assert!(ours.len() >= 4, "{what}: expected the four fixture cues");
    for (k, &i) in shown.iter().enumerate() {
        let (px, start, end) = &ours[k];
        let (t, theirs) = &o.frames[i];
        assert!((start - off - t).abs() < 0.002, "{what}: cue {k} start {start} vs ffmpeg {t}");
        if let (Some(e), Some((te, _))) = (end, o.frames.get(i + 1)) {
            assert!((e - off - te).abs() < 0.015, "{what}: cue {k} end {e} vs ffmpeg {te}");
        }
        let mut diff = 0;
        for (a, b) in px.chunks(4).zip(theirs.chunks(4)) {
            if a[3] != b[3] || (a[3] != 0 && a[..3] != b[..3]) {
                diff += 1;
            }
        }
        assert_eq!(diff, 0, "{what}: cue {k} differs in {diff} pixels");
    }
}

fn run_packets(
    d: &mut dyn Demuxer,
    stream: u32,
    dec: &mut DvdSubDecoder,
) -> Vec<(Vec<u8>, f64, Option<f64>)> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream == stream => {
                let base = p.time_base;
                let pts = p.pts.expect("subtitle packets carry pts") as f64 * base.num() as f64
                    / base.den() as f64;
                dec.send_packet(&p).unwrap();
                let c = dec.take_cue().unwrap();
                let s = pts + c.start_90k as f64 / 90_000.0;
                let e = c.end_90k.map(|e| pts + e as f64 / 90_000.0);
                let Frame::Video(_) = Frame::Video(c.frame.clone()) else { unreachable!() };
                out.push((c.frame.planes[0].data.to_vec(), s, e));
            }
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    out
}

#[test]
fn mkv_vobsub_matches_ffmpeg() {
    let path = data("dvd.mkv");
    let Some(o) = oracle(&path, 720, 576) else { return };
    let mut d = ec_matroska::MatroskaDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let info = &d.streams()[0];
    let extra = info.params.extradata.as_deref().map(<[u8]>::to_vec);
    assert_eq!(info.params.codec, ec_core::CodecId::DvdSub);
    let idx = parse_idx(extra.as_deref().expect("S_VOBSUB CodecPrivate"));
    assert_eq!(idx.size, Some((720, 576)));
    let mut dec = DvdSubDecoder::new(extra.as_deref());
    let ours = run_packets(&mut d, 0, &mut dec);
    compare(&ours, &o, "mkv");
}

fn mkv_oracle() -> Option<Oracle> {
    oracle(&data("dvd.mkv"), 720, 576)
}

#[test]
fn vob_private_stream_1_matches_ffmpeg() {
    let Some(o) = mkv_oracle() else { return };
    let path = data("dvd.vob");
    let mut d = ec_mpegts::PsDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let sub = d
        .streams()
        .iter()
        .position(|s| s.params.codec == ec_core::CodecId::DvdSub)
        .expect("the VOB's subpicture stream is found") as u32;
    let mut dec = DvdSubDecoder::new(None); // no IFO: the default palette, which is what was encoded
    let ours = run_packets(&mut d, sub, &mut dec);
    compare_shifted(&ours, &o, "vob", true);
}

#[test]
fn mp4s_esds_matches_ffmpeg() {
    let Some(o) = mkv_oracle() else { return };
    let path = data("dvd.mp4");
    let mut d = ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let sub = d
        .streams()
        .iter()
        .position(|s| s.params.codec == ec_core::CodecId::DvdSub)
        .expect("the mp4s track is a DvdSub stream") as u32;
    let extra = d.streams()[sub as usize].params.extradata.as_deref().map(<[u8]>::to_vec);
    assert_eq!(parse_idx(extra.as_deref().unwrap()).size, Some((720, 576)));
    let mut dec = DvdSubDecoder::new(extra.as_deref());
    let ours = run_packets(&mut d, sub, &mut dec);
    compare_shifted(&ours, &o, "mp4", true);
}

#[test]
fn idx_sub_pair_matches_ffmpeg() {
    let Some(o) = mkv_oracle() else { return };
    let idx = parse_idx(&std::fs::read(data("dvd.idx")).unwrap());
    assert_eq!(idx.size, Some((720, 576)));
    assert_eq!(idx.languages, vec![("en".to_string(), 0)]);
    assert_eq!(idx.entries.len(), 4);
    assert_eq!(idx.entries[1].1, 2000);
    let sub = std::fs::read(data("dvd.sub")).unwrap();
    let units = sub_file_units(&sub, 0);
    assert_eq!(units.len(), 4);
    // The idx timestamps (ms) are the units' PTS.
    for (u, e) in units.iter().zip(&idx.entries) {
        assert_eq!(u.1 * 1000 / 90_000, e.1);
    }
    let text = std::fs::read(data("dvd.idx")).unwrap();
    let mut dec = DvdSubDecoder::new(Some(&text));
    let mut ours = Vec::new();
    for (unit, pts) in &units {
        dec.push_unit(unit, Some(ec_core::Timestamp::new(*pts, ec_core::TimeBase::try_new(1, 90_000).unwrap())))
            .unwrap();
        let c = dec.take_cue().unwrap();
        let t = *pts as f64 / 90_000.0;
        ours.push((
            c.frame.planes[0].data.to_vec(),
            t + c.start_90k as f64 / 90_000.0,
            c.end_90k.map(|e| t + e as f64 / 90_000.0),
        ));
    }
    compare_shifted(&ours, &o, "idx/sub", true);
}
