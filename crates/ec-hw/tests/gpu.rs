//! Live GPU tests: every one of these talks to the real VA-API driver.
//!
//! They are not `#[ignore]`d — a hardware crate whose tests do not touch the
//! hardware tests nothing — but they *skip* (print and return) when there is no
//! VA display or when the fixtures have not been generated. That keeps a
//! checkout without a GPU green while making this machine's run meaningful.
//!
//! The oracle is ffmpeg's software decoder: every fixture is decoded here and
//! by `ffmpeg -f rawvideo`, and the two are compared frame by frame.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ec_hw::{
    Codec, Decoder, EncCodec, Encoder, EncoderConfig, Frame, FrameMetadata, I420, RateControlMode,
};
use ec_va::Display;

/// Frames compared per fixture: enough to cover several GOPs and any
/// reordering, few enough to keep the suite under a minute.
const FRAMES: usize = 24;

fn bitstreams() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bitstreams")
}

fn display() -> Option<Arc<Display>> {
    match Display::open() {
        Ok(display) => Some(display),
        Err(e) => {
            eprintln!("skipped: no VA display ({e})");
            None
        }
    }
}

fn fixtures(pattern: &str) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(bitstreams()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(pattern))
        })
        .collect();
    out.sort();
    out
}

/// The software reference: ffmpeg decoding the same file to raw planes.
struct Reference {
    child: std::process::Child,
    frame_bytes: usize,
    ten_bit: bool,
}

impl Reference {
    fn open(path: &Path, width: u32, height: u32, ten_bit: bool) -> Option<Reference> {
        let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
        let child = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(path)
            // `-fps_mode passthrough`: without it ffmpeg conforms its output to
            // the container's nominal frame rate, duplicating or dropping
            // pictures on a film whose real cadence differs — which shows up
            // here as everything being one frame out from some point onward.
            .args([
                "-map", "0:v:0", "-fps_mode", "passthrough", "-pix_fmt", pix, "-f", "rawvideo", "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let samples = (width as usize * height as usize * 3).div_ceil(2);
        Some(Reference {
            child,
            frame_bytes: if ten_bit { samples * 2 } else { samples },
            ten_bit,
        })
    }

    /// The next reference frame, as samples at the stream's own bit depth.
    ///
    /// A 10-bit stream is compared at 10 bits rather than converted down:
    /// `ffmpeg -pix_fmt yuv420p` on a 10-bit source dithers, which would hide a
    /// real difference behind a fake 53 dB one (measured, not assumed).
    fn next(&mut self, width: u32, height: u32) -> Option<Planes> {
        let stdout = self.child.stdout.as_mut()?;
        let mut buf = vec![0u8; self.frame_bytes];
        stdout.read_exact(&mut buf).ok()?;
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (y_len, c_len) = (w * h, cw * ch);
        if self.ten_bit {
            let read = |slice: &[u8]| -> Vec<u16> {
                slice
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect()
            };
            Some(Planes {
                y: read(&buf[..y_len * 2]),
                u: read(&buf[y_len * 2..(y_len + c_len) * 2]),
                v: read(&buf[(y_len + c_len) * 2..(y_len + 2 * c_len) * 2]),
                peak: 1023.0,
            })
        } else {
            let read = |slice: &[u8]| -> Vec<u16> { slice.iter().map(|&v| u16::from(v)).collect() };
            Some(Planes {
                y: read(&buf[..y_len]),
                u: read(&buf[y_len..y_len + c_len]),
                v: read(&buf[y_len + c_len..y_len + 2 * c_len]),
                peak: 255.0,
            })
        }
    }
}

impl Drop for Reference {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Three planes at whatever bit depth the stream carries.
struct Planes {
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    peak: f64,
}

impl Planes {
    fn from_8bit(f: &I420) -> Planes {
        let read = |v: &[u8]| -> Vec<u16> { v.iter().map(|&s| u16::from(s)).collect() };
        Planes {
            y: read(&f.y),
            u: read(&f.u),
            v: read(&f.v),
            peak: 255.0,
        }
    }

    fn from_16bit(f: &ec_hw::I420_16) -> Planes {
        Planes {
            y: f.y.clone(),
            u: f.u.clone(),
            v: f.v.clone(),
            peak: 1023.0,
        }
    }
}

/// Peak signal to noise ratio over the three planes, in dB; `f64::INFINITY`
/// when the frames are identical.
fn psnr(a: &Planes, b: &Planes) -> f64 {
    let mut sum = 0f64;
    let mut n = 0usize;
    for (x, y) in [(&a.y, &b.y), (&a.u, &b.u), (&a.v, &b.v)] {
        for (p, q) in x.iter().zip(y.iter()) {
            let d = f64::from(*p) - f64::from(*q);
            sum += d * d;
        }
        n += x.len().min(y.len());
    }
    if n == 0 {
        return 0.0;
    }
    let mse = sum / n as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (a.peak * a.peak / mse).log10()
}

/// Feed a whole elementary stream to a decoder, collecting frames.
///
/// H.264 and HEVC are fed access unit by access unit, which is what a demuxer
/// hands over; VP9 and AV1 arrive as IVF frames.
fn decode_stream(
    decoder: &mut Decoder,
    codec: Codec,
    data: &[u8],
    limit: usize,
    mut on_frame: impl FnMut(Frame),
) -> Result<usize, ec_hw::Error> {
    let mut count = 0usize;
    let units: Vec<&[u8]> = match codec {
        Codec::H264 | Codec::H265 => split_access_units(data, codec),
        Codec::Vp9 | Codec::Av1 => split_ivf(data),
    };
    for (i, unit) in units.iter().enumerate() {
        decoder.decode(unit, i as i64)?;
        while let Some(frame) = decoder.next_frame() {
            count += 1;
            on_frame(frame);
            if count >= limit {
                return Ok(count);
            }
        }
    }
    decoder.flush()?;
    while let Some(frame) = decoder.next_frame() {
        count += 1;
        on_frame(frame);
        if count >= limit {
            break;
        }
    }
    Ok(count)
}

/// Split an Annex B stream into access units: a new one starts at the first
/// VCL NAL of a picture, i.e. at an AUD, a parameter set, or a slice whose
/// first-slice flag is set after a previous slice.
fn split_access_units(data: &[u8], codec: Codec) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut seen_slice = false;
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i] != 0 || data[i + 1] != 0 || data[i + 2] != 1 {
            i += 1;
            continue;
        }
        let nal = i + 3;
        let (is_vcl, first_slice) = match codec {
            Codec::H264 => {
                let t = data[nal] & 0x1f;
                let first = data.get(nal + 1).is_some_and(|&b| b & 0x80 != 0);
                ((1..=5).contains(&t), first)
            }
            _ => {
                let t = (data[nal] >> 1) & 0x3f;
                let first = data.get(nal + 2).is_some_and(|&b| b & 0x80 != 0);
                (t <= 31, first)
            }
        };
        if is_vcl {
            if seen_slice && first_slice {
                starts.push(i);
                seen_slice = false;
            }
            if first_slice {
                seen_slice = true;
            }
        }
        i = nal + 1;
    }
    let mut out = Vec::new();
    let mut prev = 0usize;
    for &start in &starts {
        out.push(&data[prev..start]);
        prev = start;
    }
    if prev < data.len() {
        out.push(&data[prev..]);
    }
    out
}

/// Split an IVF file into its frames.
fn split_ivf(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let header = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = header.max(32);
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

/// Decode every named fixture and compare with ffmpeg, frame by frame.
fn compare_against_ffmpeg(files: &[PathBuf], codec: Codec, ten_bit: bool) {
    let Some(display) = display() else { return };
    if files.is_empty() {
        eprintln!("skipped: no fixtures — run scripts/gen-bitstream-fixtures.sh");
        return;
    }
    let mut table = Vec::new();
    for path in files {
        let data = std::fs::read(path).expect("fixture is readable");
        let mut decoder = Decoder::new(&display, codec).expect("decoder opens");
        let mut reference: Option<Reference> = None;
        let mut worst = f64::INFINITY;
        let mut identical = 0usize;
        let mut compared = 0usize;
        let mut mismatched_size = false;

        let result = decode_stream(&mut decoder, codec, &data, FRAMES, |frame| {
            let (w, h) = frame.display_size;
            let reference = reference
                .get_or_insert_with(|| Reference::open(path, w, h, ten_bit).expect("ffmpeg runs"));
            let Some(want) = reference.next(w, h) else {
                return;
            };
            let got = if ten_bit {
                Planes::from_16bit(&frame.to_i420_16().expect("readback"))
            } else {
                Planes::from_8bit(&frame.to_i420().expect("readback"))
            };
            if got.y.len() != want.y.len() {
                mismatched_size = true;
                return;
            }
            let db = psnr(&got, &want);
            if std::env::var_os("EC_HW_DEBUG").is_some() {
                eprintln!("  frame {compared}: {db:.1} dB (ts {})", frame.timestamp);
            }
            if db.is_infinite() {
                identical += 1;
            }
            worst = worst.min(db);
            compared += 1;
        });
        let decoded = result.expect("decode succeeds");
        assert!(
            !mismatched_size,
            "{}: frame size disagrees with ffmpeg",
            path.display()
        );
        assert!(compared > 0, "{}: nothing decoded", path.display());
        table.push(format!(
            "{:<40} {decoded:>3} frames, {identical:>3}/{compared} identical, worst {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            if worst.is_infinite() {
                "exact".to_string()
            } else {
                format!("{worst:.1} dB")
            }
        ));
        assert!(
            worst >= 40.0,
            "{}: worst frame PSNR {worst:.1} dB against ffmpeg",
            path.display()
        );
    }
    for row in table {
        println!("{row}");
    }
}

/// The 4:2:0 fixtures of one kind; the 4:4:4 and monochrome ones are the
/// profiles this GPU does not decode, and have their own test.
fn profile_420(pattern: &str, codec: &str) -> Vec<PathBuf> {
    fixtures(pattern)
        .into_iter()
        .filter(|p| {
            let name = p.to_string_lossy().to_string();
            name.contains(codec) && !name.contains("444") && !name.contains("monochrome")
        })
        .collect()
}

fn eight_bit(pattern: &str, codec: &str) -> Vec<PathBuf> {
    profile_420(pattern, codec)
        .into_iter()
        .filter(|p| !p.to_string_lossy().contains("10bit"))
        .collect()
}

fn ten_bit(pattern: &str, codec: &str) -> Vec<PathBuf> {
    profile_420(pattern, codec)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("10bit"))
        .collect()
}

#[test]
fn h264_matches_ffmpeg() {
    compare_against_ffmpeg(&fixtures(".264"), Codec::H264, false);
}

#[test]
fn hevc_8bit_matches_ffmpeg() {
    compare_against_ffmpeg(&eight_bit(".265", "hevc"), Codec::H265, false);
}

/// The 10-bit path, which is the RT format defect this crate exists to not
/// repeat: a Main10 stream must get a `VA_RT_FORMAT_YUV420_10` context and a
/// P010 surface, and the ten bits must survive to the caller.
#[test]
fn hevc_10bit_decodes_to_p010() {
    let files = ten_bit(".265", "hevc");
    if files.is_empty() {
        eprintln!("skipped: no 10-bit HEVC fixtures");
        return;
    }
    let Some(display) = display() else { return };
    let data = std::fs::read(&files[0]).expect("fixture is readable");
    let mut decoder = Decoder::new(&display, Codec::H265).expect("decoder opens");
    let mut depth = 0u8;
    decode_stream(&mut decoder, Codec::H265, &data, 1, |frame| {
        depth = frame.bit_depth;
    })
    .expect("decode succeeds");
    assert_eq!(depth, 10, "a Main10 stream decoded at {depth} bits");
    let info = decoder.stream_info().expect("the session exists");
    assert_eq!(info.bit_depth, 10);
    assert_eq!(info.profile, ec_va::caps::Profile::HEVCMain10);

    compare_against_ffmpeg(&files, Codec::H265, true);
}

#[test]
fn vp9_matches_ffmpeg() {
    compare_against_ffmpeg(&eight_bit(".ivf", "vp9"), Codec::Vp9, false);
}

#[test]
fn vp9_10bit_matches_ffmpeg() {
    compare_against_ffmpeg(&ten_bit(".ivf", "vp9"), Codec::Vp9, true);
}

#[test]
fn av1_matches_ffmpeg() {
    compare_against_ffmpeg(&eight_bit(".ivf", "av1"), Codec::Av1, false);
}

#[test]
fn av1_10bit_matches_ffmpeg() {
    compare_against_ffmpeg(&ten_bit(".ivf", "av1"), Codec::Av1, true);
}

/// Every "not supported" this crate says has to be a fact about the driver, not
/// a string. This GPU decodes VP9 profiles 0 and 2 and AV1 profiles 0 and 2 —
/// so the 4:4:4 fixtures must come back as a typed refusal naming the profile,
/// and the capability report must agree that the profile is absent.
#[test]
fn unsupported_profiles_are_refused_with_a_reason() {
    let Some(display) = display() else { return };
    let caps = ec_va::CapReport::probe(&display).expect("caps probe");
    for (file, codec, absent) in [
        (
            "vp9-profile1-444.ivf",
            Codec::Vp9,
            ec_va::caps::Profile::VP9Profile1,
        ),
        (
            "av1-profile1-444.ivf",
            Codec::Av1,
            ec_va::caps::Profile::AV1Profile1,
        ),
    ] {
        let path = bitstreams().join(file);
        if !path.exists() {
            eprintln!("skipped: no {file}");
            continue;
        }
        assert!(
            !caps.supports(absent, ec_va::caps::Entrypoint::VLD),
            "{absent:?} is in fact supported — the refusal below would be a bug"
        );
        let data = std::fs::read(&path).expect("fixture is readable");
        let mut decoder = Decoder::new(&display, codec).expect("decoder opens");
        let err = decode_stream(&mut decoder, codec, &data, 1, |_| {})
            .expect_err("a 4:4:4 stream must be refused");
        let message = err.to_string();
        assert!(
            matches!(err, ec_hw::Error::Unsupported { .. }),
            "{file}: expected a typed refusal, got {message}"
        );
        println!("{file}: {message}");
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// A moving synthetic frame: a gradient with a box that travels, so motion
/// compensation has something to find and a static-image encoder cannot cheat.
fn source_frame(width: u32, height: u32, t: u32) -> I420 {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut f = I420 {
        y: vec![0; w * h],
        u: vec![128; cw * ch],
        v: vec![128; cw * ch],
        width,
        height,
    };
    for y in 0..h {
        for x in 0..w {
            f.y[y * w + x] = ((x * 200 / w) + (y * 55 / h)) as u8;
        }
    }
    let (bx, by) = ((t as usize * 7) % (w - 64), (t as usize * 5) % (h - 64));
    for y in by..by + 64 {
        for x in bx..bx + 64 {
            f.y[y * w + x] = 235;
        }
    }
    for y in by / 2..(by + 64) / 2 {
        for x in bx / 2..(bx + 64) / 2 {
            f.u[y * cw + x] = 90;
            f.v[y * cw + x] = 240;
        }
    }
    f
}

/// Encode `frames` synthetic pictures and return the bitstream.
fn encode_stream(
    display: &Arc<Display>,
    codec: EncCodec,
    width: u32,
    height: u32,
    frames: u32,
    allow_av1: bool,
) -> Result<Vec<u8>, ec_hw::Error> {
    let mut config = EncoderConfig::new(codec, width, height);
    config.gop_size = 30;
    config.framerate = (30, 1);
    config.rate_control = RateControlMode::ConstantQp { qp: 26 };
    config.allow_av1 = allow_av1;
    let mut encoder = Encoder::new(display, config)?;
    let mut out = Vec::new();
    for t in 0..frames {
        let coded = encoder.encode(
            &source_frame(width, height, t),
            FrameMetadata {
                timestamp: i64::from(t),
                force_keyframe: false,
            },
        )?;
        assert_eq!(coded.is_keyframe, t == 0, "GOP structure at frame {t}");
        out.extend_from_slice(&coded.data);
    }
    Ok(out)
}

/// Decode a bitstream with ffmpeg and compare with the frames that went in.
fn round_trip(codec: EncCodec, width: u32, height: u32, min_db: f64) {
    let Some(display) = display() else { return };
    let frames = 12u32;
    let stream = match encode_stream(&display, codec, width, height, frames, false) {
        Ok(stream) => stream,
        Err(e) => panic!("{codec:?} encode failed: {e}"),
    };
    assert!(
        stream.len() > 1000,
        "{codec:?}: {} bytes is not a stream",
        stream.len()
    );

    let dir = std::env::temp_dir().join(format!("ec-hw-{codec:?}-{width}x{height}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(match codec {
        EncCodec::H264 => "out.264",
        EncCodec::H265 => "out.265",
        EncCodec::Av1 => "out.obu",
    });
    std::fs::write(&path, &stream).expect("write bitstream");

    // ffprobe first: the conformance window / frame cropping has to survive, so
    // a 1916-wide encode comes back 1916 wide and not 1920.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("ffprobe runs");
    let dims = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    assert_eq!(
        dims,
        format!("{width},{height}"),
        "{codec:?}: ffprobe sees {dims}"
    );

    let mut reference = Reference::open(&path, width, height, false).expect("ffmpeg runs");
    let mut worst = f64::INFINITY;
    let mut compared = 0u32;
    for t in 0..frames {
        let Some(got) = reference.next(width, height) else {
            break;
        };
        let want = Planes::from_8bit(&source_frame(width, height, t));
        let db = psnr(&got, &want);
        worst = worst.min(db);
        compared += 1;
    }
    assert_eq!(
        compared, frames,
        "{codec:?}: ffmpeg decoded {compared} of {frames}"
    );
    println!(
        "{codec:?} {width}x{height}: {compared} frames, {} bytes, worst {worst:.1} dB vs source",
        stream.len()
    );
    assert!(
        worst >= min_db,
        "{codec:?}: worst frame {worst:.1} dB against the source"
    );
}

#[test]
fn h264_encode_round_trips_through_ffmpeg() {
    round_trip(EncCodec::H264, 640, 480, 30.0);
}

#[test]
fn hevc_encode_round_trips_through_ffmpeg() {
    round_trip(EncCodec::H265, 640, 480, 30.0);
}

/// 1916x1080 is the shape that broke the incumbent HEVC encoder: the coded size
/// is 1920x1088, and only the conformance window brings it back.
#[test]
fn hevc_encode_honours_the_conformance_window() {
    round_trip(EncCodec::H265, 1916, 1080, 30.0);
}

/// What this crate encodes, its own hardware decoder must decode.
#[test]
fn our_encoder_and_our_decoder_agree() {
    let Some(display) = display() else { return };
    for (enc, dec) in [(EncCodec::H264, Codec::H264), (EncCodec::H265, Codec::H265)] {
        let stream = encode_stream(&display, enc, 640, 480, 12, false).expect("encode");
        let mut decoder = Decoder::new(&display, dec).expect("decoder opens");
        let mut worst = f64::INFINITY;
        let mut count = 0u32;
        decode_stream(&mut decoder, dec, &stream, 12, |frame| {
            let got = Planes::from_8bit(&frame.to_i420().expect("readback"));
            let want = Planes::from_8bit(&source_frame(640, 480, count));
            worst = worst.min(psnr(&got, &want));
            count += 1;
        })
        .expect("decode");
        assert_eq!(count, 12, "{enc:?}: decoded {count} of 12 frames");
        println!("{enc:?} -> our decoder: {count} frames, worst {worst:.1} dB vs source");
        assert!(worst >= 30.0, "{enc:?}: worst {worst:.1} dB");
    }
}

/// A decoded surface goes straight into an encoder with no `to_i420`
/// read-back, and what comes out is not measurably different from the
/// CPU-path baseline (decode -> `to_i420` -> `encode`) of the same frames.
///
/// 1920x1080 is the size on purpose: H.264's 16-block and HEVC's 32-block
/// rounding both land on the same 1920x1088 coded size for it, so the
/// decoder's surfaces satisfy `encode_frame`'s coded-size check against
/// either target codec without a second fixture.
#[test]
fn encode_frame_zero_copy_matches_the_cpu_path() {
    let Some(display) = display() else { return };
    let (width, height) = (1920u32, 1080u32);
    let Some(path) = fixtures(".264")
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("h264-1080p-23.976-8bit.264"))
    else {
        eprintln!("skipped: no h264-1080p fixture — run scripts/gen-bitstream-fixtures.sh");
        return;
    };
    let data = std::fs::read(&path).expect("fixture is readable");
    let frames = 10usize;

    for (target, dec_codec) in [(EncCodec::H264, Codec::H264), (EncCodec::H265, Codec::H265)] {
        let mut cfg = EncoderConfig::new(target, width, height);
        cfg.gop_size = 30;
        cfg.rate_control = RateControlMode::ConstantQp { qp: 22 };
        let mut zero_copy = Encoder::new(&display, cfg).expect("zero-copy encoder opens");
        let mut cpu_path = Encoder::new(&display, cfg).expect("cpu-path encoder opens");
        let mut zc_stream = Vec::new();
        let mut cpu_stream = Vec::new();
        let mut count = 0i64;

        let mut decoder = Decoder::new(&display, Codec::H264).expect("decoder opens");
        decode_stream(&mut decoder, Codec::H264, &data, frames, |frame| {
            let meta = FrameMetadata {
                timestamp: count,
                force_keyframe: false,
            };
            // The zero-copy path never reads the surface back to the CPU: the
            // only `to_i420` call in this loop is the CPU-path baseline's.
            let zc = zero_copy
                .encode_frame(&frame, meta)
                .expect("encode_frame (zero copy)");
            zc_stream.extend_from_slice(&zc.data);
            let i420 = frame.to_i420().expect("readback for the CPU-path baseline");
            let cpu = cpu_path.encode(&i420, meta).expect("encode (CPU path)");
            cpu_stream.extend_from_slice(&cpu.data);
            count += 1;
        })
        .expect("decode");
        assert_eq!(count as usize, frames, "{target:?}: decoded {count} of {frames}");

        let ext = match target {
            EncCodec::H264 => "264",
            EncCodec::H265 => "265",
            EncCodec::Av1 => unreachable!("AV1 is not in this test's target list"),
        };
        let dir = std::env::temp_dir().join("ec-hw-encode-frame-zero-copy");
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (name, bytes) in [("zc", &zc_stream), ("cpu", &cpu_stream)] {
            std::fs::write(dir.join(format!("{name}.{ext}")), bytes).expect("write bitstream");
        }

        let mut zc_decoder = Decoder::new(&display, dec_codec).expect("zc decoder opens");
        let mut cpu_decoder = Decoder::new(&display, dec_codec).expect("cpu decoder opens");
        let mut zc_frames = Vec::new();
        let mut cpu_frames = Vec::new();
        decode_stream(&mut zc_decoder, dec_codec, &zc_stream, frames, |f| {
            zc_frames.push(Planes::from_8bit(&f.to_i420().expect("readback")))
        })
        .expect("decode zero-copy stream");
        decode_stream(&mut cpu_decoder, dec_codec, &cpu_stream, frames, |f| {
            cpu_frames.push(Planes::from_8bit(&f.to_i420().expect("readback")))
        })
        .expect("decode CPU-path stream");
        assert_eq!(zc_frames.len(), frames, "{target:?}: zero-copy stream frame count");
        assert_eq!(cpu_frames.len(), frames, "{target:?}: CPU-path stream frame count");

        let worst = zc_frames
            .iter()
            .zip(&cpu_frames)
            .map(|(a, b)| psnr(a, b))
            .fold(f64::INFINITY, f64::min);
        println!("{target:?} encode_frame vs CPU path: worst {worst:.1} dB");
        assert!(worst >= 35.0, "{target:?}: worst {worst:.1} dB vs the CPU path");
    }
}

/// `EncoderConfig::colour` reaches the VUI of both codecs' packed SPS: an
/// export with a colour description survives ffprobe reading it back, and
/// leaving it unset keeps today's behaviour (no `video_signal_type`,
/// deterministically — two encodes of the same picture with no colour set
/// come back byte for byte identical).
#[test]
fn colour_description_reaches_the_vui() {
    let Some(display) = display() else { return };
    let (width, height) = (640u32, 480u32);
    let dir = std::env::temp_dir().join("ec-hw-colour-vui");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let encode_one = |colour: Option<(u8, u8, u8, bool)>, name: &str| -> (PathBuf, Vec<u8>) {
        let mut cfg = EncoderConfig::new(EncCodec::H265, width, height);
        cfg.rate_control = RateControlMode::ConstantQp { qp: 26 };
        if let Some((primaries, transfer, matrix, full_range)) = colour {
            cfg = cfg.colour(primaries, transfer, matrix, full_range);
        }
        let mut enc = Encoder::new(&display, cfg).expect("encoder opens");
        let coded = enc
            .encode(
                &source_frame(width, height, 0),
                FrameMetadata {
                    timestamp: 0,
                    force_keyframe: true,
                },
            )
            .expect("encode");
        let path = dir.join(name);
        std::fs::write(&path, &coded.data).expect("write bitstream");
        (path, coded.data)
    };
    let probe = |path: &Path| -> String {
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_primaries,color_transfer,color_space",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .expect("ffprobe runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // Pinned default case: no colour set, twice, must match exactly.
    let (default_path, default_bytes) = encode_one(None, "default-a.265");
    let (_, default_bytes2) = encode_one(None, "default-b.265");
    assert_eq!(
        default_bytes, default_bytes2,
        "no colour set: two encodes of the same picture must be byte-identical"
    );
    let default_probe = probe(&default_path);
    println!("no colour set: ffprobe sees `{default_probe}`");
    assert!(
        !default_probe.contains("bt2020"),
        "no colour set should not read back as BT.2020: `{default_probe}`"
    );

    // BT.2020 / PQ / BT.2020 non-constant luminance (H.273 9/16/9).
    let (bt2020_path, _) = encode_one(Some((9, 16, 9, false)), "bt2020.265");
    // ffprobe's csv reorders these as color_space,color_transfer,color_primaries
    // regardless of the -show_entries order given above.
    let got = probe(&bt2020_path);
    assert_eq!(got, "bt2020nc,smpte2084,bt2020", "ffprobe on the BT.2020 stream");
}

/// AV1 encoding is opt-in, and this test does not turn it on.
///
/// It was probed once, live, at the driver's own minimum encode size
/// (radeonsi reports 320x128 for AV1 `EncSlice`) on 2026-08-14. The submission
/// took the GPU down:
///
/// ```text
/// amdgpu: The CS has cancelled because the context is lost.
///         This context is guilty of a hard recovery.
/// ```
///
/// The process was aborted by the driver, not by this crate — there is no error
/// to return from a context that no longer exists. So the probe is *recorded*
/// rather than repeated: what this test asserts is the guard that keeps the
/// path off unless a caller asks for it by name, which is the only defence a
/// library has against a driver bug of that class.
#[test]
fn av1_encode_is_opt_in() {
    let Some(display) = display() else { return };
    let refused = Encoder::new(&display, EncoderConfig::new(EncCodec::Av1, 320, 128));
    assert!(
        matches!(refused, Err(ec_hw::Error::Unsupported { .. })),
        "AV1 encoding must be refused without the explicit opt-in"
    );
    // With the opt-in the encoder builds — the parameters, the surface pool and
    // the context are all real; only `encode()` is left untested here.
    let mut config = EncoderConfig::new(EncCodec::Av1, 320, 128);
    config.allow_av1 = true;
    match Encoder::new(&display, config) {
        Ok(encoder) => println!(
            "AV1 encode: opt-in encoder built at {:?}, submission deliberately not run",
            encoder.coded_size()
        ),
        Err(e) => println!("AV1 encode: refused by the driver, typed error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// The four incumbent defects, as tests
// ---------------------------------------------------------------------------

/// A `frame_num` gap is normal, not corruption.
///
/// Dropping a reference picture from the middle of a GOP is what a seek, a lost
/// packet or an encoder that skipped a picture all look like. The incumbent
/// stack treated it as an error and fell back to software for the rest of the
/// timeline; here the missing pictures are inferred (8.2.5.2) and decoding
/// carries on in hardware.
#[test]
fn h264_frame_num_gaps_are_synthesised() {
    let Some(display) = display() else { return };
    let path = bitstreams().join("h264-1080p-23.976-8bit.264");
    if !path.exists() {
        eprintln!("skipped: no H.264 fixture");
        return;
    }
    let data = std::fs::read(&path).expect("fixture is readable");
    let units = split_access_units(&data, Codec::H264);
    assert!(
        units.len() > 8,
        "fixture is too short to drop a picture from"
    );

    // Drop two reference pictures from the middle of the first GOP.
    let mut kept: Vec<&[u8]> = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        if (3..=5).contains(&i) {
            continue;
        }
        kept.push(unit);
    }

    let mut decoder = Decoder::new(&display, Codec::H264).expect("decoder opens");
    let mut frames = 0usize;
    for (i, unit) in kept.iter().enumerate().take(20) {
        if let Err(e) = decoder.decode(unit, i as i64) {
            panic!(
                "unit {i} failed after {} gap frames: {e}",
                decoder.gap_frames_synthesized()
            );
        }
        while let Some(frame) = decoder.next_frame() {
            // The frame is real: reading it back must work, not just arrive.
            let planes = frame.to_i420().expect("readback");
            assert_eq!(planes.y.len(), 1920 * 1080);
            frames += 1;
        }
    }
    decoder.flush().expect("flush");
    while decoder.next_frame().is_some() {
        frames += 1;
    }
    let synthesised = decoder.gap_frames_synthesized();
    println!("frame_num gap: {frames} frames decoded, {synthesised} pictures inferred");
    assert!(synthesised > 0, "the gap was not detected");
    assert!(frames >= 10, "only {frames} frames survived the gap");
}

/// VmRSS in kilobytes, from `/proc/self/status`.
fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next().map(|n| n.to_string()))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Decode and read back for longer than any export, and watch the resident set.
///
/// The incumbent leaked ~3 MB per frame by mapping a VA image and never
/// unmapping it: a 719-frame export lost 2 GB. Every mapping here is a guard
/// that unmaps on drop, and this is the test that says so out loud.
#[test]
fn long_decode_does_not_grow_the_resident_set() {
    // VmRSS is a process-wide number and the harness runs every test in this
    // binary as a thread of one process: a sibling allocating between the
    // baseline and the end reading shows up here as a 200 MB "leak". The
    // measurement therefore runs in a child process holding only this test.
    if std::env::var_os("EC_HW_RSS_CHILD").is_none() {
        let status = Command::new(std::env::current_exe().expect("test binary path"))
            .args(["--exact", "long_decode_does_not_grow_the_resident_set"])
            .env("EC_HW_RSS_CHILD", "1")
            .status()
            .expect("re-run this test in its own process");
        assert!(status.success(), "isolated RSS run failed");
        return;
    }
    let Some(display) = display() else { return };
    let path = bitstreams().join("h264-1080p-23.976-8bit.264");
    if !path.exists() {
        eprintln!("skipped: no H.264 fixture");
        return;
    }
    let data = std::fs::read(&path).expect("fixture is readable");
    let units = split_access_units(&data, Codec::H264);
    let mut decoder = Decoder::new(&display, Codec::H264).expect("decoder opens");

    // Warm up first: the first pictures allocate the surface pool and the
    // readback image, and that growth is not a leak.
    let mut decoded = 0usize;
    let mut baseline = 0u64;
    let mut readback_ms = 0f64;
    let mut pending: Vec<Frame> = Vec::new();
    let target = 719usize;
    'outer: for pass in 0..16 {
        if pass > 0 {
            decoder.reset();
        }
        for (i, unit) in units.iter().enumerate() {
            decoder.decode(unit, i as i64).expect("decode");
            while let Some(frame) = decoder.next_frame() {
                // Hold a few frames before reading any back: a decoder that is
                // read one frame per submission spends its time waiting for the
                // GPU, which measures latency rather than throughput.
                pending.push(frame);
                if pending.len() < 4 {
                    continue;
                }
                let start = std::time::Instant::now();
                for frame in pending.drain(..) {
                    let planes = frame.to_i420().expect("readback");
                    std::hint::black_box(&planes);
                    decoded += 1;
                }
                readback_ms += start.elapsed().as_secs_f64() * 1000.0;
                if decoded == 60 {
                    baseline = rss_kb();
                }
                if decoded >= target {
                    break 'outer;
                }
            }
        }
    }
    decoder.flush().expect("flush");
    let end = rss_kb();
    let delta_mb = (end.saturating_sub(baseline)) as f64 / 1024.0;
    println!(
        "{decoded} frames decoded and read back: RSS {baseline} -> {end} kB (delta {delta_mb:.1} MB), \
         decode and read back {:.2} ms/frame at 1920x1080",
        readback_ms / decoded as f64
    );
    assert!(decoded >= target, "only {decoded} frames decoded");
    assert!(
        delta_mb < 50.0,
        "resident set grew {delta_mb:.1} MB over {decoded} frames"
    );
}

/// The zero-copy path: a decoded surface exports as DRM PRIME file descriptors.
///
/// This is what edith's `engine-hw` imports into gbm, so the fds and the plane
/// layout matter as much as the pixels do.
#[test]
fn frames_export_as_dma_buf() {
    let Some(display) = display() else { return };
    let path = bitstreams().join("h264-1080p-23.976-8bit.264");
    if !path.exists() {
        eprintln!("skipped: no H.264 fixture");
        return;
    }
    let data = std::fs::read(&path).expect("fixture is readable");
    let mut decoder = Decoder::new(&display, Codec::H264).expect("decoder opens");
    let mut exported = 0usize;
    decode_stream(&mut decoder, Codec::H264, &data, 2, |frame| {
        let prime = frame.export_prime().expect("export");
        assert_eq!(prime.fourcc, ec_va::sys::VA_FOURCC_NV12);
        assert_eq!((prime.width, prime.height), frame.coded_size);
        assert!(!prime.objects.is_empty(), "no DRM objects exported");
        assert!(!prime.layers.is_empty(), "no layers exported");
        let planes: usize = prime.layers.iter().map(|l| l.planes.len()).sum();
        assert!(
            planes >= 2,
            "NV12 needs a luma and a chroma plane, got {planes}"
        );
        exported += 1;
    })
    .expect("decode");
    assert!(exported >= 1, "nothing was exported");
    println!("DRM PRIME export: {exported} frames");
}

// ---------------------------------------------------------------------------
// His library
// ---------------------------------------------------------------------------

/// Turn any container into an elementary stream ffmpeg and this crate agree on.
///
/// `frames` bounds the extraction: piping a two-hour 4K film through memory to
/// compare its first five hundred pictures is several gigabytes of nothing.
fn elementary(path: &Path, codec: Codec, frames: usize) -> Option<Vec<u8>> {
    let args: &[&str] = match codec {
        Codec::H264 => &["-bsf:v", "h264_mp4toannexb", "-f", "h264"],
        Codec::H265 => &["-bsf:v", "hevc_mp4toannexb", "-f", "hevc"],
        Codec::Vp9 | Codec::Av1 => &["-f", "ivf"],
    };
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-c:v", "copy"])
        // A few more coded pictures than frames wanted: reordering means the
        // last few packets have not been shown yet.
        .args(["-frames:v", &(frames + 16).to_string()])
        .args(args)
        .arg("-")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (!out.stdout.is_empty()).then_some(out.stdout)
}

/// Decode 500 frames of five files from the real library and compare with
/// ffmpeg, per file.
///
/// Fixtures are 5-second synthetic clips at round resolutions; his library is
/// 1038-high HEVC, 792-high AV1 and hour-long H.264 screen captures. A claim
/// that has not met those is a claim about fixtures.
#[test]
fn real_library_spot_check() {
    let Some(display) = display() else { return };
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!(
            "skipped: no fixtures/real-library-manifest.tsv — run scripts/scan-real-library.sh"
        );
        return;
    };

    // One file per (codec, bit depth) the hardware path serves, five in all.
    let mut wanted: Vec<(String, Codec, bool)> = Vec::new();
    let mut seen: Vec<(String, u8)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let (path, vcodec, depth) = (f[0].to_string(), f[2], f[6].parse::<u8>().unwrap_or(8));
        let codec = match vcodec {
            "h264" => Codec::H264,
            "hevc" => Codec::H265,
            "vp9" => Codec::Vp9,
            "av1" => Codec::Av1,
            _ => continue,
        };
        let key = (vcodec.to_string(), depth);
        if seen.iter().filter(|k| **k == key).count() >= 2 || wanted.len() >= 5 {
            continue;
        }
        if !Path::new(&path).exists() {
            continue;
        }
        seen.push(key);
        wanted.push((path, codec, depth > 8));
    }
    if wanted.is_empty() {
        eprintln!("skipped: no files from the manifest are present");
        return;
    }

    // 500 frames per file by default; a diagnostic run can ask for fewer.
    let limit: usize = std::env::var("EC_HW_SWEEP_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let mut table = Vec::new();
    for (path, codec, ten_bit) in &wanted {
        let path = Path::new(path);
        let Some(data) = elementary(path, *codec, limit) else {
            table.push(format!("{:<52} SKIP (no elementary stream)", short(path)));
            continue;
        };
        let mut decoder = Decoder::new(&display, *codec).expect("decoder opens");
        let mut reference: Option<Reference> = None;
        let mut worst = f64::INFINITY;
        let mut compared = 0usize;
        let mut failed: Option<String> = None;
        let result = decode_stream(&mut decoder, *codec, &data, limit, |frame| {
            let (w, h) = frame.display_size;
            let reference = reference
                .get_or_insert_with(|| Reference::open(path, w, h, *ten_bit).expect("ffmpeg runs"));
            let Some(want) = reference.next(w, h) else {
                return;
            };
            let got = if *ten_bit {
                Planes::from_16bit(&frame.to_i420_16().expect("readback"))
            } else {
                Planes::from_8bit(&frame.to_i420().expect("readback"))
            };
            if got.y.len() != want.y.len() {
                failed = Some("frame size disagrees with ffmpeg".to_string());
                return;
            }
            let db = psnr(&got, &want);
            if std::env::var_os("EC_HW_DEBUG").is_some() {
                eprintln!("  {} frame {compared}: {db:.1} dB", short(path));
            }
            worst = worst.min(db);
            compared += 1;
        });
        match result {
            Ok(decoded) => table.push(format!(
                "{:<52} {decoded:>4} frames, {compared} compared, worst {}",
                short(path),
                if worst.is_infinite() {
                    "exact".to_string()
                } else {
                    format!("{worst:.1} dB")
                }
            )),
            Err(e) => failed = Some(e.to_string()),
        }
        if let Some(reason) = failed {
            table.push(format!("{:<52} FAIL {reason}", short(path)));
        }
        assert!(compared > 0, "{}: nothing decoded", short(path));
        assert!(
            worst >= 40.0,
            "{}: worst frame {worst:.1} dB against ffmpeg",
            short(path)
        );
    }
    for row in table {
        println!("{row}");
    }
}

/// A library path without the directory tree, so a report can carry it.
fn short(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// PSNR of one plane, in dB (`f64::INFINITY` for an exact match).
fn plane_psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    let n = a.len().min(b.len());
    let mse: f64 = a[..n]
        .iter()
        .zip(&b[..n])
        .map(|(&p, &q)| {
            let d = f64::from(p) - f64::from(q);
            d * d
        })
        .sum::<f64>()
        / n as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

/// P010 correctness on the real 4K HDR film, plane by plane.
///
/// `real_library_spot_check` already pins the whole-frame PSNR at >= 40 dB
/// for a mix of files; this isolates Y from U/V on this specific 10-bit film,
/// against both `yuv420p10le` (the lossless comparison) and `yuv420p` (the
/// truncating `to_i420` path, which rounds ffmpeg's dithered 8-bit output
/// against this crate's un-dithered right-shift -- a real but small gap, not
/// a bug). A collapsed U/V PSNR here (a grey frame) would mean the chroma
/// plane is being read at the wrong stride/offset; it is not, on this file.
#[test]
fn ten_bit_4k_chroma_is_not_grey() {
    let Some(display) = display() else { return };
    let path = Path::new(
        "/home/tahinli/Downloads/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE\
         /Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE.mkv",
    );
    if !path.exists() {
        eprintln!("skipped: film not present");
        return;
    }
    let frames = 3usize;
    let Some(data) = elementary(path, Codec::H265, frames) else {
        eprintln!("skipped: no elementary stream");
        return;
    };
    let mut decoder = Decoder::new(&display, Codec::H265).expect("decoder opens");
    let mut ref10: Option<Reference> = None;
    let mut ref8: Option<Reference> = None;
    let mut compared = 0usize;
    decode_stream(&mut decoder, Codec::H265, &data, frames, |frame| {
        let (w, h) = frame.display_size;
        let ref10 = ref10.get_or_insert_with(|| Reference::open(path, w, h, true).expect("ffmpeg runs"));
        let want10 = ref10.next(w, h).expect("ffmpeg has a frame");
        let got16 = Planes::from_16bit(&frame.to_i420_16().expect("readback"));
        let y_db = plane_psnr(&got16.y, &want10.y, 1023.0);
        let uv_db = plane_psnr(
            &[got16.u.as_slice(), got16.v.as_slice()].concat(),
            &[want10.u.as_slice(), want10.v.as_slice()].concat(),
            1023.0,
        );
        println!("frame {compared} to_i420_16 vs yuv420p10le: Y {y_db:.1} dB, UV {uv_db:.1} dB");
        assert!(y_db >= 50.0, "frame {compared}: Y PSNR collapsed to {y_db:.1} dB");
        assert!(
            uv_db >= 50.0,
            "frame {compared}: chroma PSNR collapsed to {uv_db:.1} dB (grey frame)"
        );

        let ref8 = ref8.get_or_insert_with(|| Reference::open(path, w, h, false).expect("ffmpeg runs"));
        let want8 = ref8.next(w, h).expect("ffmpeg has a frame");
        let got8 = Planes::from_8bit(&frame.to_i420().expect("readback"));
        let y_db8 = plane_psnr(&got8.y, &want8.y, 255.0);
        let uv_db8 = plane_psnr(
            &[got8.u.as_slice(), got8.v.as_slice()].concat(),
            &[want8.u.as_slice(), want8.v.as_slice()].concat(),
            255.0,
        );
        println!("frame {compared} to_i420 vs yuv420p: Y {y_db8:.1} dB, UV {uv_db8:.1} dB");
        // ffmpeg dithers its 8-bit downconversion; this crate truncates. Both
        // are legitimate 10-to-8 roundings, so the floor here is the
        // dithering noise floor, not the lossless bound above.
        assert!(y_db8 >= 30.0, "frame {compared}: 8-bit Y PSNR {y_db8:.1} dB");
        assert!(
            uv_db8 >= 30.0,
            "frame {compared}: 8-bit chroma PSNR collapsed to {uv_db8:.1} dB (grey frame)"
        );
        compared += 1;
    })
    .expect("decode");
    assert_eq!(compared, frames, "not all frames were compared");
}

/// Per-frame cost of the 4K 10-bit read-back path, isolated from decode
/// submission itself.
///
/// The consumer's `hw_decode::the_hdr_film_renders_tone_mapped` measured
/// 1352.5 ms/frame against a 39 ms (24 fps) budget on this film. Decode
/// correctness on it is already pinned by `real_library_spot_check` (bit-exact
/// against ffmpeg), so this isolates which stage of the frame path -- decode,
/// 8-bit readback, 10-bit readback or DRM PRIME export -- the time goes to.
#[test]
fn ten_bit_4k_frame_path_is_realtime() {
    let Some(display) = display() else { return };
    let path = Path::new(
        "/home/tahinli/Downloads/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE\
         /Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE.mkv",
    );
    if !path.exists() {
        eprintln!("skipped: film not present");
        return;
    }
    let limit = 120usize;
    let Some(data) = elementary(path, Codec::H265, limit) else {
        eprintln!("skipped: no elementary stream");
        return;
    };
    let units = split_access_units(&data, Codec::H265);
    assert!(units.len() > limit, "film clip too short for {limit} frames");

    #[derive(Clone, Copy)]
    enum Op {
        DecodeOnly,
        ToI420,
        ToI420_16,
        ExportPrime,
    }
    fn apply(op: Op, frame: Frame) {
        match op {
            Op::DecodeOnly => drop(frame),
            Op::ToI420 => {
                frame.to_i420().expect("readback");
            }
            Op::ToI420_16 => {
                frame.to_i420_16().expect("readback");
            }
            Op::ExportPrime => {
                frame.export_prime().expect("export");
            }
        }
    }

    println!(
        "{:<22} {:>10} {:>10} {:>10}",
        "stage", "median ms", "min ms", "max ms"
    );
    let mut got_to_i420_16 = None;
    for (name, op) in [
        ("decode-only", Op::DecodeOnly),
        ("decode+to_i420", Op::ToI420),
        ("decode+to_i420_16", Op::ToI420_16),
        ("decode+export_prime", Op::ExportPrime),
    ] {
        let mut decoder = Decoder::new(&display, Codec::H265).expect("decoder opens");
        let mut durations: Vec<Duration> = Vec::new();
        // Hold a few frames before reading, as the resident-set test does:
        // reading one frame per submission measures latency, not throughput,
        // and lets decode run ahead of read-back the way a real player does.
        let mut pending: Vec<Frame> = Vec::new();
        let mut t_prev = Instant::now();
        'outer: for (i, unit) in units.iter().enumerate() {
            decoder.decode(unit, i as i64).expect("decode");
            while let Some(frame) = decoder.next_frame() {
                pending.push(frame);
                if pending.len() < 4 {
                    continue;
                }
                for frame in pending.drain(..) {
                    apply(op, frame);
                    let now = Instant::now();
                    durations.push(now.duration_since(t_prev));
                    t_prev = now;
                }
                if durations.len() >= limit {
                    break 'outer;
                }
            }
        }
        decoder.flush().expect("flush");
        while durations.len() < limit {
            let Some(frame) = decoder.next_frame() else {
                break;
            };
            apply(op, frame);
            let now = Instant::now();
            durations.push(now.duration_since(t_prev));
            t_prev = now;
        }
        assert!(
            durations.len() >= 24,
            "{name}: only {} frames decoded",
            durations.len()
        );
        durations.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let median = ms(durations[durations.len() / 2]);
        let min = ms(durations[0]);
        let max = ms(durations[durations.len() - 1]);
        println!("{name:<22} {median:>10.2} {min:>10.2} {max:>10.2}");
        if name == "decode+to_i420_16" {
            got_to_i420_16 = Some(median);
        }
    }
    let got = got_to_i420_16.expect("to_i420_16 phase ran");
    println!("target: decode+to_i420_16 <= 39.00 ms/frame median (24 fps budget); got {got:.2}");
}

/// A CRA GOP boundary stores its RASL leading pictures *after* the CRA in
/// decode order, even though they display before it (they predict from
/// pictures the CRA's own random-access point does not have). A decoder
/// started fresh at that CRA — what a seek does — must therefore drop those
/// RASL pictures per H.265 8.1.3's `NoRaslOutputFlag`, or its output shifts by
/// however many RASL pictures the GOP has, handing back the wrong picture for
/// every display index at or after the seek target.
#[test]
fn hevc_seek_matches_linear_mkv() {
    let Some(display) = display() else { return };
    let path = bitstreams().join("test_hevc.mkv");
    let Some(data) = elementary(&path, Codec::H265, 45) else {
        eprintln!("skipped: no test_hevc.mkv fixture");
        return;
    };
    let units = split_access_units(&data, Codec::H265);

    // Linear decode: every picture from the start, frame 30 kept for
    // comparison (the file's second CRA, per `test_hevc.mkv`'s 30-picture
    // GOPs).
    const TARGET: usize = 30;
    let mut decoder = Decoder::new(&display, Codec::H265).expect("decoder opens");
    let mut linear_frame_30 = None;
    let mut index = 0usize;
    decode_stream(&mut decoder, Codec::H265, &data, 45, |frame| {
        if index == TARGET {
            linear_frame_30 = Some(frame.to_i420().expect("readback"));
        }
        index += 1;
    })
    .expect("linear decode");
    let linear_frame_30 = linear_frame_30.expect("linear decode reached frame 30");

    // Find the access unit whose first VCL NAL is the CRA at this GOP
    // boundary, and feed the decoder from there on, exactly as a seek would:
    // nothing from before it goes in.
    let cra_at = units
        .iter()
        .position(|au| starts_with_cra(au))
        .expect("a CRA access unit exists");

    // A seek reuses the parameter sets a decoder already learned; a fresh one
    // has none, so it is primed the same way `ec_hw::Decoder::reset` expects a
    // caller to: decode the leading VPS/SPS/PPS (their IDR picture is thrown
    // away) and reset picture state before feeding the seek target.
    let mut seek_decoder = Decoder::new(&display, Codec::H265).expect("decoder opens");
    seek_decoder.decode(units[0], 0).expect("prime parameter sets");
    seek_decoder.reset();
    let mut seeked_frame = None;
    for (i, unit) in units[cra_at..].iter().enumerate() {
        seek_decoder.decode(unit, i as i64).expect("seek decode");
        if let Some(frame) = seek_decoder.next_frame() {
            seeked_frame = Some(frame.to_i420().expect("readback"));
            break;
        }
    }
    if seeked_frame.is_none() {
        seek_decoder.flush().expect("flush");
        seeked_frame = seek_decoder
            .next_frame()
            .map(|f| f.to_i420().expect("readback"));
    }
    let seeked_frame = seeked_frame.expect("a frame came out after the seek");

    assert_eq!(
        seeked_frame, linear_frame_30,
        "seeking to the CRA before frame 30 handed back a different picture \
         than a linear decode of frame 30 — RASL pictures were not dropped"
    );
}

/// True when an access unit's first VCL NAL is an HEVC CRA (`nal_type` 21).
/// A CRA's access unit may open with its own VPS/SPS/PPS, so this skips past
/// non-VCL NALs to the first one that carries a slice.
fn starts_with_cra(au: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let t = (au[i + 3] >> 1) & 0x3f;
            if t <= 31 {
                return t == 21;
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    false
}
