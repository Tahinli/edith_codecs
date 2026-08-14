//! Bit-exactness against the JVT conformance bitstreams.
//!
//! Each vector ships with the reference decoder's output, so the oracle here is
//! the specification's own, not another decoder: the first coded picture of the
//! stream is decoded and compared sample by sample with the first frame of the
//! `.yuv` companion file.
//!
//! The vectors are fetched by `scripts/fetch-vectors.sh` and are not committed;
//! when they are absent the test reports what it skipped instead of passing
//! silently.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ec_core::error::Error;
use ec_core::frame::Frame;
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder};
use ec_core::timebase::TimeBase;
use ec_h264::H264Decoder;

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vectors/h264-jvt")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("fixtures/vectors/h264-jvt"))
}

/// Decode the first picture of `stream` and return it as planar I420 bytes.
fn decode_first_frame(stream: &Path) -> Result<(Vec<u8>, u32, u32), Error> {
    let data = std::fs::read(stream)?;
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264))?;
    // The whole file goes in as one packet: the decoder finds the picture
    // boundaries itself, and the first one is complete before any later slice
    // can be refused.
    let result = decoder.send_packet(&Packet::new(0, TimeBase::new(1, 30), data));
    let frame = match decoder.receive_frame() {
        Ok(frame) => frame,
        Err(_) => return Err(result.err().unwrap_or(Error::NeedMore)),
    };
    let Frame::Video(frame) = frame else {
        return Err(Error::corrupt("H.264 decoder produced an audio frame"));
    };
    let mut out = Vec::new();
    for (index, plane) in frame.planes.iter().enumerate() {
        let (width, height) = if index == 0 {
            (frame.width as usize, frame.height as usize)
        } else {
            (
                (frame.width as usize).div_ceil(2),
                (frame.height as usize).div_ceil(2),
            )
        };
        for row in 0..height {
            let start = row * plane.stride;
            out.extend_from_slice(&plane.data[start..start + width]);
        }
    }
    Ok((out, frame.width, frame.height))
}

/// Every vector directory the fetch script has populated, in name order.
fn all_vectors(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Locate `<dir>/<name>` and the reference YUV beside it.
fn vector_files(dir: &Path, name: &str) -> Option<(PathBuf, PathBuf)> {
    let folder = dir.join(name);
    let mut bitstream = None;
    let mut reference = None;
    for entry in std::fs::read_dir(&folder).ok()? {
        let path = entry.ok()?.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("264") | Some("jsv") | Some("26l") | Some("jvt") | Some("h264") => {
                bitstream = Some(path)
            }
            // The reference decoder output ships as .yuv or, for the MW
            // vectors, as _rec.qcif; both are planar I420 of the coded size.
            Some("yuv") | Some("qcif") => reference = Some(path),
            _ => {}
        }
    }
    Some((bitstream?, reference?))
}

#[test]
fn first_picture_is_bit_exact_against_the_jvt_reference() {
    let dir = vectors_dir();
    if !dir.is_dir() {
        eprintln!(
            "skipped: {} is absent (run scripts/fetch-vectors.sh)",
            dir.display()
        );
        return;
    }
    let mut table = String::new();
    let mut exact = 0usize;
    let mut mismatched = 0usize;
    let mut refused = 0usize;
    let mut broken = Vec::new();
    for name in all_vectors(&dir) {
        let Some((bitstream, reference)) = vector_files(&dir, &name) else {
            let _ = writeln!(table, "{name:<16} no bitstream/reference pair");
            continue;
        };
        match decode_first_frame(&bitstream) {
            Ok((frame, width, height)) => {
                let expected_len = frame.len();
                let reference = std::fs::read(&reference).expect("reference YUV readable");
                if reference.len() < expected_len {
                    let _ = writeln!(
                        table,
                        "{name:<16} reference is shorter than one {width}x{height} frame"
                    );
                    continue;
                }
                let mismatches = frame
                    .iter()
                    .zip(&reference[..expected_len])
                    .filter(|(a, b)| a != b)
                    .count();
                if mismatches == 0 {
                    exact += 1;
                    let _ = writeln!(table, "{name:<16} {width}x{height} bit-exact");
                } else {
                    mismatched += 1;
                    let first = frame
                        .iter()
                        .zip(&reference[..expected_len])
                        .position(|(a, b)| a != b)
                        .unwrap();
                    let _ = writeln!(
                        table,
                        "{name:<16} {width}x{height} MISMATCH {mismatches}/{expected_len} bytes, first at {first}"
                    );
                }
            }
            // A stream outside this release's scope must say so by name; a
            // Corrupt or NeedMore verdict on a conformant stream is a bug in
            // this decoder, not a capability statement.
            Err(err @ Error::Unsupported { .. }) => {
                refused += 1;
                let _ = writeln!(table, "{name:<16} refused: {err}");
            }
            Err(err) => {
                broken.push(format!("{name}: {err}"));
                let _ = writeln!(table, "{name:<16} FAILED: {err}");
            }
        }
    }
    eprintln!("{table}");
    assert!(
        exact >= 5,
        "fewer than five vectors decoded bit-exact:\n{table}"
    );
    assert_eq!(
        mismatched, 0,
        "some decoded vectors are not bit-exact:\n{table}"
    );
    assert!(
        broken.is_empty(),
        "vectors failed with something other than a named refusal: {broken:#?}\n{table}"
    );
    eprintln!("{exact} bit-exact, {refused} refused by name");
}

/// Run a command, returning false when it or the tool itself fails.
fn run(command: &str, args: &[&str]) -> bool {
    std::process::Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Every quantisation parameter, decoded bit-exactly against ffmpeg.
///
/// The conformance vectors above are coded at a handful of quantisers, which
/// leaves most of the deblocking filter's `alpha`, `beta` and `tC0` tables
/// (Tables 8-16 and 8-17, indexed by `indexA`/`indexB`, that is by QP)
/// unexercised. One all-intra Baseline picture per QP covers all 51 rows.
#[test]
fn every_quantiser_matches_ffmpeg() {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-qp-sweep-{}-{}",
        std::process::id(),
        "every_quantiser_matches_ffmpeg"
    ));
    let _ = std::fs::create_dir_all(&dir);
    if !run(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-version"],
    ) {
        eprintln!("skipped: ffmpeg is not on PATH");
        return;
    }
    let mut table = String::new();
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for qp in 1..=51u32 {
        let stream = dir.join(format!("q{qp}.264"));
        let reference = dir.join(format!("q{qp}.yuv"));
        // Baseline profile is CAVLC, 4:2:0, no 8x8 transform and no scaling
        // matrices: exactly this release's scope, with the deblocking filter on.
        let encoded = run(
            "ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=176x144:rate=1:duration=1",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-profile:v",
                "baseline",
                "-qp",
                &qp.to_string(),
                "-frames:v",
                "1",
                "-f",
                "h264",
                stream.to_str().unwrap(),
            ],
        );
        if !encoded {
            eprintln!("skipped: ffmpeg has no usable libx264 encoder");
            return;
        }
        assert!(
            run(
                "ffmpeg",
                &[
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-i",
                    stream.to_str().unwrap(),
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    reference.to_str().unwrap(),
                ],
            ),
            "ffmpeg could not decode its own QP {qp} output"
        );
        let expected = std::fs::read(&reference).expect("reference readable");
        match decode_first_frame(&stream) {
            Ok((frame, _, _)) => {
                checked += 1;
                let mismatches = frame.iter().zip(&expected).filter(|(a, b)| a != b).count();
                if mismatches != 0 || frame.len() != expected.len() {
                    failures.push(format!("qp {qp}: {mismatches} bytes differ"));
                }
                let _ = writeln!(table, "qp {qp:<3} {mismatches} bytes differ");
            }
            Err(err) => failures.push(format!("qp {qp}: {err}")),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked >= 40, "too few quantisers exercised:\n{table}");
    assert!(failures.is_empty(), "{failures:#?}");
    eprintln!("{checked} quantisers bit-exact against ffmpeg");
}

/// Encode one all-intra Baseline picture with the given extra x264 arguments,
/// decode it both ways and return `(mismatching bytes, width, height)`.
fn ffmpeg_round_trip(dir: &Path, tag: &str, size: &str, extra: &[&str]) -> Result<usize, String> {
    let stream = dir.join(format!("{tag}.264"));
    let reference = dir.join(format!("{tag}.yuv"));
    let source = format!("testsrc=size={size}:rate=1:duration=1");
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &source,
        "-pix_fmt",
        "yuv420p",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-frames:v",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(extra.iter().map(|s| s.to_string()));
    args.extend([
        "-f".into(),
        "h264".into(),
        stream.to_string_lossy().into_owned(),
    ]);
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    if !run("ffmpeg", &refs) {
        return Err(format!("{tag}: ffmpeg could not encode"));
    }
    if !run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            stream.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            reference.to_str().unwrap(),
        ],
    ) {
        return Err(format!("{tag}: ffmpeg could not decode its own output"));
    }
    let expected = std::fs::read(&reference).map_err(|e| format!("{tag}: {e}"))?;
    let (frame, width, height) = decode_first_frame(&stream).map_err(|e| format!("{tag}: {e}"))?;
    if frame.len() != expected.len() {
        return Err(format!(
            "{tag}: {width}x{height} is {} bytes, ffmpeg gave {}",
            frame.len(),
            expected.len()
        ));
    }
    Ok(frame.iter().zip(&expected).filter(|(a, b)| a != b).count())
}

/// Cropping, odd sizes, multiple slices and a disabled loop filter, each
/// against ffmpeg.
///
/// The conformance vectors are all QCIF single-slice: this covers the geometry
/// (a 1080-line picture is coded as 1088 and cropped) and the slice structure
/// that a real stream from a camera or an encoder has.
#[test]
fn geometry_and_slice_structure_match_ffmpeg() {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-geometry-{}-{}",
        std::process::id(),
        "geometry_and_slice_structure_match_ffmpeg"
    ));
    let _ = std::fs::create_dir_all(&dir);
    if !run(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-version"],
    ) {
        eprintln!("skipped: ffmpeg is not on PATH");
        return;
    }
    let cases: &[(&str, &str, &[&str])] = &[
        // 1080 lines are coded as 68 macroblock rows and cropped back to 1080.
        ("hd", "1920x1080", &["-qp", "26"]),
        // A width that is not a multiple of 16 crops horizontally as well.
        ("odd", "1916x1080", &["-qp", "26"]),
        // Four slices per picture: neighbour availability stops at each slice.
        (
            "slices",
            "352x288",
            &["-qp", "24", "-x264-params", "slices=4"],
        ),
        // No loop filter at all.
        (
            "nodeblock",
            "352x288",
            &["-qp", "30", "-x264-params", "no-deblock=1"],
        ),
        // A non-zero chroma QP offset moves the chroma deblocking thresholds.
        (
            "chromaqp",
            "352x288",
            &["-qp", "30", "-x264-params", "chroma_qp_offset=6"],
        ),
        // Very small pictures: one macroblock wide.
        ("tiny", "16x16", &["-qp", "20"]),
    ];
    let mut table = String::new();
    let mut failures = Vec::new();
    for (tag, size, extra) in cases {
        match ffmpeg_round_trip(&dir, tag, size, extra) {
            Ok(0) => {
                let _ = writeln!(table, "{tag:<10} {size:<10} bit-exact");
            }
            Ok(n) => {
                let _ = writeln!(table, "{tag:<10} {size:<10} MISMATCH {n} bytes");
                failures.push(format!("{tag}: {n} bytes differ"));
            }
            Err(err) => {
                let _ = writeln!(table, "{tag:<10} {size:<10} {err}");
                failures.push(err);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The `avcC` entry path: parameter sets out of band, NAL units length
/// prefixed — how an MP4 or Matroska demuxer hands H.264 over.
#[test]
fn avcc_extradata_and_length_prefixed_packets() {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-avcc-{}-{}",
        std::process::id(),
        "avcc_extradata_and_length_prefixed_packets"
    ));
    let _ = std::fs::create_dir_all(&dir);
    if !run(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-version"],
    ) {
        eprintln!("skipped: ffmpeg is not on PATH");
        return;
    }
    let annex_b = dir.join("stream.264");
    assert!(run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=176x144:rate=1:duration=1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-qp",
            "26",
            "-frames:v",
            "1",
            "-f",
            "h264",
            annex_b.to_str().unwrap(),
        ],
    ));
    let data = std::fs::read(&annex_b).unwrap();
    let units: Vec<Vec<u8>> = ec_h264_syntax::annex_b_units(&data)
        .into_iter()
        .map(|u| u.to_vec())
        .collect();
    let sps = units.iter().find(|u| u[0] & 0x1F == 7).expect("an SPS");
    let pps = units.iter().find(|u| u[0] & 0x1F == 8).expect("a PPS");

    // avcC (ISO/IEC 14496-15): version, profile, compatibility, level, then
    // the NAL length size and the parameter sets.
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);

    let mut params = CodecParameters::new(CodecId::H264);
    params.extradata = Some(ec_core::packet::Buf::from_vec(avcc));
    let mut decoder = H264Decoder::new(params).expect("avcC parses");
    assert_eq!(
        decoder.codec_parameters().video().unwrap().width,
        176,
        "the SPS inside the avcC sets the picture size"
    );

    // The slice NAL units, each with a four byte length prefix.
    let mut sample = Vec::new();
    for unit in units.iter().filter(|u| matches!(u[0] & 0x1F, 1 | 5)) {
        sample.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        sample.extend_from_slice(unit);
    }
    decoder
        .send_packet(&Packet::new(0, TimeBase::new(1, 25), sample))
        .expect("length prefixed packet decodes");
    let Frame::Video(frame) = decoder.receive_frame().expect("a frame") else {
        panic!("video expected")
    };
    assert_eq!((frame.width, frame.height), (176, 144));

    // Same picture through the Annex B path: the two entry paths agree.
    let (annex_b_frame, _, _) = decode_first_frame(&annex_b).unwrap();
    let mut avcc_frame = Vec::new();
    for (index, plane) in frame.planes.iter().enumerate() {
        let (w, h) = if index == 0 { (176, 144) } else { (88, 72) };
        for row in 0..h {
            avcc_frame.extend_from_slice(&plane.data[row * plane.stride..row * plane.stride + w]);
        }
    }
    assert_eq!(avcc_frame, annex_b_frame);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every refusal this decoder makes, proved against a stream that really uses
/// the feature.
///
/// A "not supported" string is a claim about the binary; without a stream that
/// triggers it, it is just as likely to be a bug being hidden.
#[test]
fn refusals_name_a_feature_the_stream_really_uses() {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-refusals-{}-{}",
        std::process::id(),
        "refusals_name_a_feature_the_stream_really_uses"
    ));
    let _ = std::fs::create_dir_all(&dir);
    if !run(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-version"],
    ) {
        eprintln!("skipped: ffmpeg is not on PATH");
        return;
    }
    // (tag, encoder arguments, the words the refusal must contain)
    let cases: &[(&str, &[&str], &str)] = &[
        (
            "cabac",
            &["-profile:v", "main", "-x264-params", "cabac=1"],
            "CABAC",
        ),
        (
            "transform8x8",
            &["-profile:v", "high", "-x264-params", "cabac=0:8x8dct=1"],
            "8x8 transform",
        ),
        (
            "scaling",
            &[
                "-profile:v",
                "high",
                "-x264-params",
                "cabac=0:8x8dct=0:cqm=jvt",
            ],
            "scaling matrices",
        ),
        (
            "yuv422",
            &[
                "-profile:v",
                "high422",
                "-pix_fmt",
                "yuv422p",
                "-x264-params",
                "cabac=0",
            ],
            "chroma_format_idc 2",
        ),
        (
            "high10",
            &[
                "-profile:v",
                "high10",
                "-pix_fmt",
                "yuv420p10le",
                "-x264-params",
                "cabac=0",
            ],
            "10-bit",
        ),
    ];
    let mut table = String::new();
    let mut failures = Vec::new();
    for (tag, extra, expected) in cases {
        let stream = dir.join(format!("{tag}.264"));
        let mut args: Vec<String> = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=176x144:rate=1:duration=1",
            "-c:v",
            "libx264",
            "-qp",
            "26",
            "-frames:v",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The pixel format has to precede the encoder arguments that name it.
        if !extra.contains(&"-pix_fmt") {
            args.extend(["-pix_fmt".into(), "yuv420p".into()]);
        }
        args.extend(extra.iter().map(|s| s.to_string()));
        args.extend([
            "-f".into(),
            "h264".into(),
            stream.to_string_lossy().into_owned(),
        ]);
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if !run("ffmpeg", &refs) {
            let _ = writeln!(table, "{tag:<12} not encodable by this ffmpeg, skipped");
            continue;
        }
        match decode_first_frame(&stream) {
            Err(Error::Unsupported { what, why }) => {
                let message = format!("{what} ({why})");
                if message.contains(expected) {
                    let _ = writeln!(table, "{tag:<12} refused: {message}");
                } else {
                    failures.push(format!("{tag}: refused as {message}, expected {expected}"));
                }
            }
            Err(err) => failures.push(format!("{tag}: {err}")),
            Ok(_) => failures.push(format!("{tag}: decoded a stream it does not support")),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Truncated and corrupted input produces an error, never a panic.
#[test]
fn damaged_streams_never_panic() {
    let dir = vectors_dir();
    let Some((bitstream, _)) = vector_files(&dir, "BA1_Sony_D") else {
        eprintln!("skipped: the JVT vectors are absent");
        return;
    };
    let mut data = std::fs::read(&bitstream).unwrap();
    // The parameter sets and the first coded picture are enough; the point is
    // the decoder's reaction to damage, not the length of the clip.
    data.truncate(4000);
    // Truncation points on a coarse grid, then single-byte corruptions: both
    // are what a damaged file or a mid-stream seek really looks like.
    for cut in (1..data.len()).step_by(397) {
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        let _ = decoder.send_packet(&Packet::new(0, TimeBase::new(1, 25), data[..cut].to_vec()));
        let _ = decoder.flush();
        while decoder.receive_frame().is_ok() {}
    }
    for flip in (0..data.len()).step_by(409) {
        let mut damaged = data.clone();
        damaged[flip] ^= 0xA5;
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        let _ = decoder.send_packet(&Packet::new(0, TimeBase::new(1, 25), damaged));
        while decoder.receive_frame().is_ok() {}
    }
}
