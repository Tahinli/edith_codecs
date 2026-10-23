//! The sample entries this writer puts in an mp4, each muxed from a real ffmpeg
//! file and held to three things. ffprobe reads our file with nothing on stderr
//! and states the source's codec, channels, rate and duration. ffmpeg decodes
//! our file to the same samples as the source, since the packets are the same
//! bytes. This crate's own demuxer reads our file back to the same packets with
//! the same timestamps and keyframe flags.
//!
//! A case is skipped, not failed, where ffmpeg or the encoder it needs is not
//! built in, the way the other ffmpeg-backed tests skip.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Demuxer, Error, Muxer, Packet};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

/// One sample entry under test, and the ffmpeg encode that makes its source.
struct Case {
    /// The sample entry name as the file states it.
    entry: &'static str,
    /// The `codec_name` ffprobe reports for it.
    ffprobe: &'static str,
    /// The encoder ffmpeg must have built in for the case to run.
    encoder: &'static str,
    /// The `-map` selector of the stream the case checks: `0:v:0` or `0:a:0`.
    map: &'static str,
    /// The arguments that make the source: its input and its encode.
    args: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        entry: "avc1",
        ffprobe: "h264",
        encoder: "libx264",
        map: "0:v:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "veryfast",
        ],
    },
    Case {
        entry: "hvc1",
        ffprobe: "hevc",
        encoder: "libx265",
        map: "0:v:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=1",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p",
            "-x265-params",
            "log-level=error",
            "-tag:v",
            "hvc1",
        ],
    },
    Case {
        entry: "av01",
        ffprobe: "av1",
        encoder: "libaom-av1",
        map: "0:v:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=1",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-b:v",
            "0",
            "-crf",
            "45",
            "-pix_fmt",
            "yuv420p",
        ],
    },
    Case {
        entry: "mp4a",
        ffprobe: "aac",
        encoder: "aac",
        map: "0:a:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
        ],
    },
    Case {
        entry: "ac-3",
        ffprobe: "ac3",
        encoder: "ac3",
        map: "0:a:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-ac",
            "6",
            "-c:a",
            "ac3",
            "-b:a",
            "384k",
        ],
    },
    Case {
        entry: "ec-3",
        ffprobe: "eac3",
        encoder: "eac3",
        map: "0:a:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-ac",
            "6",
            "-c:a",
            "eac3",
            "-b:a",
            "384k",
        ],
    },
    Case {
        entry: "alac",
        ffprobe: "alac",
        encoder: "alac",
        map: "0:a:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=1.5",
            "-ac",
            "2",
            "-c:a",
            "alac",
        ],
    },
    Case {
        entry: "Opus",
        ffprobe: "opus",
        encoder: "libopus",
        map: "0:a:0",
        args: &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-ac",
            "2",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
        ],
    },
];

/// A work directory for this test alone: keyed by the entry and the test's own
/// name, since two tests can share an entry and run in parallel.
fn work(case: &Case) -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let dir = std::env::temp_dir()
        .join("ec-mp4-mux-entries")
        .join(case.entry.to_ascii_lowercase())
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test work directory");
    dir
}

/// Whether `tool` runs at all.
fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether ffmpeg was built with `encoder`.
fn has_encoder(encoder: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(" {encoder} ")))
}

/// Makes the source for `case` at `path`, or says why it cannot. `false` is a
/// skip.
fn make_source(case: &Case, path: &Path) -> bool {
    if !has_encoder(case.encoder) {
        eprintln!(
            "skip {}: ffmpeg has no {} encoder",
            case.entry, case.encoder
        );
        return false;
    }
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-y"])
        .args(case.args)
        .args(["-f", "mp4"])
        .arg(path)
        .status()
        .expect("ffmpeg runs")
        .success();
    assert!(ok, "ffmpeg could not make the {} source", case.entry);
    true
}

/// Every packet of `path`, and the streams it declares.
fn read_all(path: &Path) -> (Vec<ec_core::StreamInfo>, Vec<Packet>) {
    let mut demuxer = Mp4Demuxer::new(BufReader::new(
        File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ))
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let streams = demuxer.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(Error::Eof) => break,
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
    (streams, packets)
}

/// Writes `packets` of `streams` to `dst`. Each audio track is given the shift
/// in `shifts` as its priming when `carry_shift` is set, as a caller that carries
/// the source's edit list would; a caller that does not gets none and must rely
/// on the muxer's own priming (an Opus pre-skip).
fn write_out(
    dst: &Path,
    streams: &[ec_core::StreamInfo],
    packets: &[Packet],
    shifts: &[(u32, i64)],
    carry_shift: bool,
) {
    let mut muxer = Mp4Muxer::new(BufWriter::new(File::create(dst).expect("output file")))
        .expect("muxer opens");
    for stream in streams {
        muxer.add_stream(stream.clone()).expect("stream declared");
    }
    if carry_shift {
        for &(stream, shift) in shifts {
            if let Ok(priming) = u32::try_from(shift) {
                muxer.set_audio_delay(stream, priming).expect("delay set");
            }
        }
    }
    for packet in packets {
        muxer.write_packet(packet).expect("packet written");
    }
    muxer.finish().expect("finished");
}

/// `src` written back out through this crate's muxer, with each track's edit list
/// shift carried over.
fn remux(src: &Path, dst: &Path) {
    let mut demuxer =
        Mp4Demuxer::new(BufReader::new(File::open(src).expect("source"))).expect("source demuxes");
    let shifts: Vec<(u32, i64)> = demuxer
        .tracks()
        .iter()
        .filter_map(|t| Some((t.stream?, t.media_time?)))
        .collect();
    let streams = demuxer.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(Error::Eof) => break,
            Err(e) => panic!("{}: {e}", src.display()),
        }
    }
    write_out(dst, &streams, &packets, &shifts, true);
}

/// ffprobe's stderr for `path`, with the full-decode check ffmpeg runs over it.
fn ffprobe_stderr(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One ffprobe CSV field list per stream, for `entries` (`stream=...`).
fn probe_lines(path: &Path, entries: &str) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end_matches(',').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn duration(path: &Path) -> f64 {
    probe_lines(path, "format=duration")
        .first()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(f64::NAN)
}

/// The MD5 of every decoded sample of `map` in `path`, which ffmpeg prints as
/// `MD5=...`. Any stderr from the decode is returned too: a clean file says
/// nothing.
fn decoded_md5(path: &Path, map: &str) -> (String, String) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", map, "-f", "md5", "-"])
        .output()
        .expect("ffmpeg runs");
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_case(case: &Case) {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skip {}: ffmpeg or ffprobe is not installed", case.entry);
        return;
    }
    let dir = work(case);
    let src = dir.join("source.mp4");
    let out = dir.join("ours.mp4");
    if !make_source(case, &src) {
        return;
    }
    remux(&src, &out);

    // (a) ffprobe reads our file cleanly and states what the source states.
    let stderr = ffprobe_stderr(&out);
    assert!(stderr.is_empty(), "{}: ffprobe said: {stderr}", case.entry);
    let fields = "stream=codec_name,codec_type,channels,sample_rate";
    assert_eq!(
        probe_lines(&out, fields),
        probe_lines(&src, fields),
        "{}: codec, channels and rate as ffprobe reads them",
        case.entry
    );
    let (ours, theirs) = (duration(&out), duration(&src));
    assert!(
        (ours - theirs).abs() < 0.05,
        "{}: duration {ours} against the source's {theirs}",
        case.entry
    );
    let names = probe_lines(&out, "stream=codec_name");
    assert_eq!(names, vec![case.ffprobe.to_string()], "{}", case.entry);

    // (b) ffmpeg decodes our file to the source's samples, bit for bit.
    let (theirs, their_err) = decoded_md5(&src, case.map);
    let (ours, our_err) = decoded_md5(&out, case.map);
    assert!(
        their_err.is_empty(),
        "{}: source decode: {their_err}",
        case.entry
    );
    assert!(our_err.is_empty(), "{}: our decode: {our_err}", case.entry);
    assert!(theirs.starts_with("MD5="), "{}: no decode", case.entry);
    assert_eq!(ours, theirs, "{}: decoded samples differ", case.entry);

    // (c) this crate's demuxer reads back the packets and timestamps it wrote.
    let (src_streams, src_packets) = read_all(&src);
    let (out_streams, out_packets) = read_all(&out);
    assert_eq!(
        out_streams
            .iter()
            .map(|s| s.params.codec)
            .collect::<Vec<_>>(),
        src_streams
            .iter()
            .map(|s| s.params.codec)
            .collect::<Vec<_>>(),
        "{}: streams",
        case.entry
    );
    assert_eq!(
        out_packets.len(),
        src_packets.len(),
        "{}: packet count",
        case.entry
    );
    assert!(
        !src_packets.is_empty(),
        "{}: source has packets",
        case.entry
    );
    for (i, (a, b)) in src_packets.iter().zip(&out_packets).enumerate() {
        assert_eq!(a.stream, b.stream, "{} packet {i}: stream", case.entry);
        assert_eq!(a.pts, b.pts, "{} packet {i}: pts", case.entry);
        assert_eq!(a.dts, b.dts, "{} packet {i}: dts", case.entry);
        assert_eq!(
            a.duration, b.duration,
            "{} packet {i}: duration",
            case.entry
        );
        assert_eq!(
            a.flags.keyframe, b.flags.keyframe,
            "{} packet {i}: keyframe",
            case.entry
        );
        assert!(a.data == b.data, "{} packet {i}: bytes", case.entry);
    }
}
/// Each sample-entry case by the name its test had, and its index in `CASES`.
const ENTRY_TESTS: &[(&str, usize)] = &[
    ("avc1_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 0),
    ("hvc1_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 1),
    ("av01_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 2),
    ("mp4a_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 3),
    ("ac3_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 4),
    ("ec3_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 5),
    ("alac_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 6),
    ("opus_entry_is_ffprobe_clean_decodes_equal_and_round_trips", 7),
];

#[test]
fn every_entry_is_ffprobe_clean_decodes_equal_and_round_trips() {
    let mut failures = Vec::new();
    for &(name, index) in ENTRY_TESTS {
        let case = &CASES[index];
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(case))).is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

/// An Opus file muxed by a caller that carries no edit list at all: the pre-skip
/// the `OpusHead` states must still come out as the `elst` shift, so the packets
/// keep the timing the source gives them.
#[test]
fn opus_pre_skip_is_written_without_a_caller_asking() {
    let case = &CASES[7];
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skip {}: ffmpeg or ffprobe is not installed", case.entry);
        return;
    }
    let dir = work(case);
    let src = dir.join("source.mp4");
    let out = dir.join("bare.mp4");
    if !make_source(case, &src) {
        return;
    }
    let (streams, packets) = read_all(&src);
    let shifts = Vec::new();
    write_out(&out, &streams, &packets, &shifts, false);
    let (_, out_packets) = read_all(&out);
    assert_eq!(out_packets.len(), packets.len(), "packet count");
    assert_eq!(
        out_packets[0].pts, packets[0].pts,
        "the first pts keeps the pre-skip"
    );
    assert!(
        packets[0].pts.is_some_and(|p| p < 0),
        "the source's first picture is shifted by its pre-skip"
    );
    let stderr = ffprobe_stderr(&out);
    assert!(stderr.is_empty(), "ffprobe said: {stderr}");
}

/// ALAC's cookie handed over bare, without its `alac` box header, is written
/// inside one, so the file is an ALAC track the decoders can read.
#[test]
fn alac_bare_cookie_is_written_inside_an_alac_box() {
    let case = &CASES[6];
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skip {}: ffmpeg or ffprobe is not installed", case.entry);
        return;
    }
    let dir = work(case);
    let src = dir.join("source.mp4");
    let out = dir.join("bare-cookie.mp4");
    if !make_source(case, &src) {
        return;
    }
    let (mut streams, packets) = read_all(&src);
    let boxed = streams[0]
        .params
        .extradata
        .clone()
        .expect("the source's cookie");
    assert_eq!(
        &boxed[4..8],
        b"alac",
        "the demuxer hands the cookie over boxed"
    );
    streams[0].params.extradata = Some(ec_core::Buf::from(&boxed[8..]));
    write_out(&out, &streams, &packets, &[], false);
    let (back, _) = read_all(&out);
    assert_eq!(
        back[0].params.extradata.as_deref(),
        Some(&boxed[..]),
        "cookie"
    );
    let stderr = ffprobe_stderr(&out);
    assert!(stderr.is_empty(), "ffprobe said: {stderr}");
    assert_eq!(
        probe_lines(&out, "stream=codec_name"),
        vec!["alac".to_string()]
    );
}
