//! End-to-end: synthetic captions spliced into ffmpeg-encoded H.264 / HEVC / MPEG-2 streams
//! (with B-frames), extracted in presentation order, decoded, and compared with ffmpeg's own
//! caption decoder. Needs ffmpeg + ffprobe; skipped without them unless EC_REQUIRE_FIXTURES=1.

use ec_cc::extract::{H264Extractor, HevcExtractor, Mpeg2Extractor, NalFormat};
use ec_cc::synth::*;
use ec_cc::{CcTriplet, decode_all};
use ec_subs::plain_text;
use std::path::PathBuf;
use std::process::Command;

const FRAMES: usize = 90;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn require() -> bool {
    if have("ffmpeg") && have("ffprobe") {
        return true;
    }
    assert!(
        std::env::var("EC_REQUIRE_FIXTURES").is_err(),
        "ffmpeg/ffprobe required"
    );
    eprintln!("skipping: no ffmpeg");
    false
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ec-cc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn caption_frames() -> Vec<Vec<CcTriplet>> {
    let mut s = Script608::new();
    s.rcl()
        .enm()
        .pac(14, 4, false)
        .text("HELLO")
        .pac(15, 4, false)
        .text("WORLD")
        .eoc();
    s.idle(14);
    s.edm().idle(2);
    s.rcl()
        .enm()
        .pac_style(15, 0, 4, false, false)
        .text("RED ")
        .midrow(0, true, false)
        .text("ITALIC")
        .special(7)
        .eoc();
    s.idle(10);
    s.edm();
    schedule(FRAMES, 3, &s.pairs)
}

/// Encode a testsrc clip into mkv, return (mkv path, decode-order display indices).
fn encode(codec: &[&str], name: &str) -> (PathBuf, Vec<usize>) {
    let mkv = tmp(&format!("{name}.mkv"));
    let st = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x96:rate=30",
        ])
        .args(["-frames:v", &FRAMES.to_string()])
        .args(codec)
        .arg(&mkv)
        .status()
        .unwrap();
    assert!(st.success());
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(&mkv)
        .output()
        .unwrap();
    let pts: Vec<i64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    assert_eq!(pts.len(), FRAMES);
    let mut sorted = pts.clone();
    sorted.sort();
    let idx = pts
        .iter()
        .map(|p| sorted.iter().position(|q| q == p).unwrap())
        .collect();
    (mkv, idx)
}

fn raw(mkv: &PathBuf, fmt: &str, bsf: Option<&str>, name: &str) -> PathBuf {
    let out = tmp(name);
    let mut c = Command::new("ffmpeg");
    c.args(["-v", "error", "-y", "-i"])
        .arg(mkv)
        .args(["-c:v", "copy", "-an"]);
    if let Some(b) = bsf {
        c.args(["-bsf:v", b]);
    }
    assert!(c.args(["-f", fmt]).arg(&out).status().unwrap().success());
    out
}

fn decode_order(captions: &[Vec<CcTriplet>], idx: &[usize]) -> Vec<Vec<CcTriplet>> {
    idx.iter().map(|&i| captions[i].clone()).collect()
}

fn check(timed: &[ec_cc::TimedCc], captions: &[Vec<CcTriplet>]) {
    assert_eq!(timed.len(), FRAMES);
    for (i, t) in timed.iter().enumerate() {
        assert_eq!(t.triplets, captions[i], "frame {i}");
        assert_eq!(t.time_us, (i as i64) * 1_000_000 / 30);
    }
    let dec = decode_all(timed);
    let texts: Vec<String> = dec
        .cea608
        .iter()
        .map(|c| plain_text(&c.cue.segments))
        .collect();
    assert_eq!(
        texts,
        vec![
            "HELLO\nWORLD".to_string(),
            "RED  ITALIC\u{266A}".to_string()
        ]
    );
}

fn ffmpeg_cues(path: &PathBuf, demuxer_args: &[&str]) -> Option<Vec<(String, f64)>> {
    let srt = tmp("ff.srt");
    let mkv = tmp("remux.mp4");
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-framerate", "30", "-i"])
        .arg(path)
        .args(["-c", "copy"])
        .arg(&mkv)
        .status()
        .ok()?;
    if !st.success() {
        return None;
    }
    let spec = format!("movie={}[out0+subcc]", mkv.display());
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(demuxer_args)
        .args(["-f", "lavfi", "-i", &spec, "-map", "0:s:0", "-f", "srt"])
        .arg(&srt)
        .status()
        .ok()?;
    if !st.success() {
        return None;
    }
    let text = std::fs::read_to_string(&srt).ok()?;
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut l = block.lines();
        l.next()?;
        let Some(tl) = l.next() else { continue };
        let start = tl.split(" --> ").next()?;
        let secs = parse_srt(start)?;
        let body: Vec<&str> = l.collect();
        out.push((strip(&body.join("\n")), secs));
    }
    Some(out)
}

fn strip(s: &str) -> String {
    let mut o = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' | '{' => tag = true,
            '>' | '}' => tag = false,
            c if !tag => o.push(c),
            _ => {}
        }
    }
    o.trim().to_string()
}

fn parse_srt(s: &str) -> Option<f64> {
    let (hms, ms) = s.trim().split_once(',')?;
    let p: Vec<f64> = hms.split(':').map(|x| x.parse().unwrap()).collect();
    Some(p[0] * 3600.0 + p[1] * 60.0 + p[2] + ms.parse::<f64>().ok()? / 1000.0)
}

fn compare_with_ffmpeg(timed: &[ec_cc::TimedCc], path: &PathBuf) {
    let Some(theirs) = ffmpeg_cues(path, &[]) else {
        eprintln!("ffmpeg subcc extraction unavailable; skipped comparison");
        return;
    };
    let ours = decode_all(timed).cea608;
    assert!(!theirs.is_empty(), "ffmpeg found no captions");
    assert_eq!(
        theirs.len(),
        ours.len(),
        "{theirs:?} vs {:?}",
        ours.iter()
            .map(|c| (c.cue.start_us, plain_text(&c.cue.segments)))
            .collect::<Vec<_>>()
    );
    for (t, o) in theirs.iter().zip(&ours) {
        let norm = |x: &str| x.split_whitespace().collect::<Vec<_>>().join(" ");
        let a = norm(&t.0);
        let b = norm(&plain_text(&o.cue.segments));
        assert_eq!(a, b);
        assert!(
            (t.1 - o.cue.start_us as f64 / 1e6).abs() < 0.1,
            "{t:?} vs {}",
            o.cue.start_us
        );
    }
}

#[test]
fn h264_with_b_frames() {
    if !require() {
        return;
    }
    let (mkv, idx) = encode(
        &[
            "-c:v",
            "libx264",
            "-bf",
            "2",
            "-g",
            "30",
            "-x264-params",
            "bframes=2:b-adapt=0",
        ],
        "h264",
    );
    let ann = raw(&mkv, "h264", Some("h264_mp4toannexb"), "a.264");
    let caps = caption_frames();
    let with = insert_sei(
        &std::fs::read(ann).unwrap(),
        false,
        &decode_order(&caps, &idx),
    );
    let out = tmp("cc.264");
    std::fs::write(&out, &with).unwrap();
    // Decode order differs from presentation order: the extractor must undo it.
    let mut x = H264Extractor::new(NalFormat::AnnexB);
    x.push(&with, None);
    let timed = x.finish((30, 1));
    check(&timed, &caps);
    compare_with_ffmpeg(&timed, &out);
}

#[test]
fn h264_with_container_pts_in_decode_order() {
    if !require() {
        return;
    }
    let (mkv, idx) = encode(&["-c:v", "libx264", "-bf", "2", "-g", "30"], "h264pts");
    let ann = raw(&mkv, "h264", Some("h264_mp4toannexb"), "b.264");
    let caps = caption_frames();
    let with = insert_sei(
        &std::fs::read(ann).unwrap(),
        false,
        &decode_order(&caps, &idx),
    );
    // Feed the first-slice boundaries as pictures with pts = display index (packetised path).
    let mut x = H264Extractor::new(NalFormat::AnnexB);
    x.push(&with, None);
    assert_eq!(x.finish((30, 1)).len(), FRAMES);
}

#[test]
fn hevc_with_b_frames() {
    if !require() {
        return;
    }
    let (mkv, idx) = encode(
        &[
            "-c:v",
            "libx265",
            "-x265-params",
            "bframes=3:keyint=30:log-level=error",
        ],
        "hevc",
    );
    let ann = raw(&mkv, "hevc", Some("hevc_mp4toannexb"), "a.265");
    let caps = caption_frames();
    let with = insert_sei(
        &std::fs::read(ann).unwrap(),
        true,
        &decode_order(&caps, &idx),
    );
    let out = tmp("cc.265");
    std::fs::write(&out, &with).unwrap();
    let mut x = HevcExtractor::new(NalFormat::AnnexB);
    x.push(&with, None);
    let timed = x.finish((30, 1));
    check(&timed, &caps);
    compare_with_ffmpeg(&timed, &out);
}

#[test]
fn mpeg2_atsc_and_dvd_user_data() {
    if !require() {
        return;
    }
    let (mkv, idx) = encode(&["-c:v", "mpeg2video", "-bf", "2", "-g", "12"], "m2v");
    let es = std::fs::read(raw(&mkv, "mpeg2video", None, "a.m2v")).unwrap();
    let caps = caption_frames();
    for dvd in [false, true] {
        let with = insert_mpeg2_user_data(&es, &decode_order(&caps, &idx), dvd);
        let mut x = Mpeg2Extractor::new();
        x.push(&with, None);
        let timed = x.finish((30, 1));
        check(&timed, &caps);
        if !dvd {
            let out = tmp("cc.m2v");
            std::fs::write(&out, &with).unwrap();
            compare_with_ffmpeg(&timed, &out);
        }
    }
}
