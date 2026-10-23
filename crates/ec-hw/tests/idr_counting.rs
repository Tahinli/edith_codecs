//! Slice-header counters across IDR pictures, read the way a stateful decoder
//! reads them (GPU; skips without a VA display).
//!
//! An IDR picture restarts `frame_num` at 0 (H.264 7.4.3) and the picture order
//! count with it. A stream that keeps counting across an IDR makes a decoder
//! see a `frame_num` gap, and a stream whose IDRs share an `idr_pic_id` makes two
//! consecutive IDRs indistinguishable. Both are checked here on a stream with an
//! IDR every few pictures, and the stream must decode in order with no gap
//! warnings.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use ec_hw::{EncCodec, Encoder, EncoderConfig, FrameMetadata, I420, RateControlMode};
use ec_va::Display;

const WIDTH: u32 = 384;
const HEIGHT: u32 = 256;
const FPS: u32 = 30;

fn display() -> Option<Rc<Display>> {
    match ec_va::open() {
        Ok(display) => Some(display),
        Err(e) => {
            eprintln!("skipped: no VA display ({e})");
            None
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-hw-idr-{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A moving picture: a bright block on a gradient, shifted each frame.
fn picture(t: u32) -> I420 {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut f = I420 {
        y: vec![0; w * h],
        u: vec![128; cw * ch],
        v: vec![128; cw * ch],
        width: WIDTH,
        height: HEIGHT,
    };
    for y in 0..h {
        for x in 0..w {
            f.y[y * w + x] = ((x * 180 / w) + (y * 60 / h)) as u8;
        }
    }
    let (bx, by) = ((t as usize * 9) % (w - 48), (t as usize * 5) % (h - 48));
    for y in by..by + 48 {
        for x in bx..bx + 48 {
            f.y[y * w + x] = 230;
        }
    }
    f
}

/// Encode `frames` pictures with an IDR every `gop` pictures. Returns the
/// stream and, per picture, whether the encoder called it a key frame.
fn encode(codec: EncCodec, frames: u32, gop: u32) -> Option<(Vec<u8>, Vec<bool>)> {
    let display = display()?;
    let mut config = EncoderConfig::new(codec, WIDTH, HEIGHT);
    config.framerate = (FPS, 1);
    config.gop_size = gop;
    config.rate_control = RateControlMode::ConstantQp { qp: 30 };
    let mut encoder = Encoder::new(&display, config).expect("encoder");
    let mut stream = Vec::new();
    let mut keys = Vec::new();
    for t in 0..frames {
        let coded = encoder
            .encode(
                &picture(t),
                FrameMetadata {
                    timestamp: i64::from(t),
                    force_keyframe: false,
                },
            )
            .expect("encode");
        keys.push(coded.is_keyframe);
        stream.extend_from_slice(&coded.data);
    }
    Some((stream, keys))
}

/// Every slice header field as `(name, value)`, in stream order, from ffmpeg's
/// header trace.
fn trace_headers(path: &Path, format: &str) -> Vec<(String, i64)> {
    let out = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "trace",
            "-hide_banner",
            "-nostats",
            "-f",
            format,
            "-i",
        ])
        .arg(path)
        .args(["-c:v", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"])
        .output()
        .expect("ffmpeg runs");
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|line| {
            // "[trace_headers @ 0x...] 16 frame_num 0000 = 0"
            let (lhs, value) = line.rsplit_once(" = ")?;
            let mut words = lhs.split_whitespace().rev();
            let _bits = words.next()?;
            let name = words.next()?;
            Some((name.to_string(), value.trim().parse().ok()?))
        })
        .collect()
}

fn values(fields: &[(String, i64)], name: &str) -> Vec<i64> {
    fields
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| *v)
        .collect()
}

/// Decode with ffmpeg: stderr must be empty at `-v error`, and at `-v debug` no
/// line may report a frame_num gap. Returns the decoded planes.
fn decode_clean(path: &Path, format: &str, frames: u32) -> Vec<u8> {
    let quiet = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", format, "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        quiet.stderr.is_empty(),
        "ffmpeg errors: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2) as usize;
    assert_eq!(
        quiet.stdout.len(),
        frame_bytes * frames as usize,
        "every picture decodes, in order"
    );

    let debug = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "debug", "-f", format, "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .expect("ffmpeg runs");
    let log = String::from_utf8_lossy(&debug.stderr);
    let gaps: Vec<&str> = log
        .lines()
        .filter(|l| l.to_ascii_lowercase().contains("gap"))
        .collect();
    assert!(gaps.is_empty(), "frame_num gap reported: {gaps:?}");
    quiet.stdout
}

/// Mean squared error of each decoded picture against the source picture with
/// the same index. A misordered or misnumbered stream scrambles the pictures and
/// fails this at QP 30 with a wide margin.
fn picture_errors(decoded: &[u8], frames: u32) -> Vec<f64> {
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2) as usize;
    (0..frames as usize)
        .map(|k| {
            let source = picture(k as u32);
            let mut src = source.y.clone();
            src.extend_from_slice(&source.u);
            src.extend_from_slice(&source.v);
            let out = &decoded[k * frame_bytes..(k + 1) * frame_bytes];
            let sum: f64 = src
                .iter()
                .zip(out)
                .map(|(&a, &b)| {
                    let d = f64::from(a) - f64::from(b);
                    d * d
                })
                .sum();
            sum / src.len() as f64
        })
        .collect()
}

/// PSNR floor for a picture at QP 30 against its source. A picture decoded from
/// the wrong reference or in the wrong order falls far below it.
const MIN_PSNR_DB: f64 = 30.0;

fn assert_pictures_in_order(decoded: &[u8], frames: u32) {
    for (k, mse) in picture_errors(decoded, frames).into_iter().enumerate() {
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10();
        assert!(
            psnr >= MIN_PSNR_DB,
            "picture {k} decodes to {psnr:.1} dB against its source"
        );
    }
}

#[test]
fn h264_frame_num_and_idr_pic_id_restart_at_every_idr() {
    let (frames, gop) = (60u32, 7u32);
    let Some((stream, keys)) = encode(EncCodec::H264, frames, gop) else {
        return;
    };
    let expected_keys: Vec<bool> = (0..frames).map(|t| t % gop == 0).collect();
    assert_eq!(keys, expected_keys, "an IDR every {gop} pictures");

    let path = scratch("h264").join("out.264");
    std::fs::write(&path, &stream).expect("write");
    let fields = trace_headers(&path, "h264");
    let frame_num = values(&fields, "frame_num");
    assert_eq!(frame_num.len(), frames as usize, "one slice per picture");
    // frame_num is (pictures since the IDR) mod MaxFrameNum (16 here), so it
    // returns to 0 at every IDR.
    let expected: Vec<i64> = (0..frames).map(|t| i64::from((t % gop) % 16)).collect();
    assert_eq!(frame_num, expected, "frame_num restarts at each IDR");

    // Consecutive IDRs must differ in idr_pic_id (7.4.3): 0, 1, 0, ...
    let idr_pic_id = values(&fields, "idr_pic_id");
    let idrs = expected_keys.iter().filter(|&&k| k).count();
    let expected_ids: Vec<i64> = (0..idrs as i64).map(|k| k % 2).collect();
    assert_eq!(
        idr_pic_id, expected_ids,
        "idr_pic_id alternates between consecutive IDRs"
    );

    // pic_order_cnt_type 2 writes no POC in the slice; the POC restarts with
    // frame_num, so the decoded order is checked on the pictures.
    assert!(
        values(&fields, "pic_order_cnt_lsb").is_empty(),
        "type 2 writes no POC lsb"
    );
    let decoded = decode_clean(&path, "h264", frames);
    assert_pictures_in_order(&decoded, frames);
}

#[test]
fn h264_intra_only_stream_is_one_idr_per_picture() {
    // GOP 1: every picture is an IDR, so idr_pic_id must alternate on every one.
    let frames = 12u32;
    let Some((stream, keys)) = encode(EncCodec::H264, frames, 1) else {
        return;
    };
    assert!(
        keys.iter().all(|&k| k),
        "every picture is a key frame at GOP 1"
    );
    let path = scratch("h264-gop1").join("out.264");
    std::fs::write(&path, &stream).expect("write");
    let fields = trace_headers(&path, "h264");
    assert!(
        values(&fields, "frame_num").iter().all(|&n| n == 0),
        "IDR frame_num is 0"
    );
    let ids = values(&fields, "idr_pic_id");
    let expected: Vec<i64> = (0..frames as i64).map(|k| k % 2).collect();
    assert_eq!(ids, expected, "idr_pic_id alternates on every IDR");
    let decoded = decode_clean(&path, "h264", frames);
    assert_pictures_in_order(&decoded, frames);
}

#[test]
fn hevc_poc_lsb_restarts_at_every_idr() {
    let (frames, gop) = (60u32, 7u32);
    let Some((stream, keys)) = encode(EncCodec::H265, frames, gop) else {
        return;
    };
    let expected_keys: Vec<bool> = (0..frames).map(|t| t % gop == 0).collect();
    assert_eq!(keys, expected_keys, "an IDR every {gop} pictures");

    let path = scratch("hevc").join("out.265");
    std::fs::write(&path, &stream).expect("write");
    let fields = trace_headers(&path, "hevc");
    let lsb = values(&fields, "slice_pic_order_cnt_lsb");
    let log2_max = values(&fields, "log2_max_pic_order_cnt_lsb_minus4")[0] + 4;
    let max_lsb = 1i64 << log2_max;
    // An IDR carries no POC lsb; every other picture carries its position in
    // the GOP, which restarts at the IDR.
    let expected: Vec<i64> = (0..frames)
        .filter(|t| t % gop != 0)
        .map(|t| i64::from(t % gop) % max_lsb)
        .collect();
    assert_eq!(lsb, expected, "POC lsb restarts at each IDR");
    let decoded = decode_clean(&path, "hevc", frames);
    assert_pictures_in_order(&decoded, frames);
}
