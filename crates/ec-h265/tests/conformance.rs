//! Conformance: every access unit this encoder writes must decode in ffmpeg,
//! and the picture ffmpeg produces must be *bit-identical* to the encoder's own
//! reconstruction. That equality is the whole point of coding with the in-loop
//! filters off: it turns "the file plays" into "the decoder and the encoder
//! agree on every sample".

mod common;

use common::test_frame;
use ec_core::frame::VideoFrame;
use ec_h265::encoder::{EncodedPicture, Encoder, EncoderConfig, RateControl};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("ec-h265-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Decode an Annex-B file with ffmpeg into planar 8-bit 4:2:0.
fn ffmpeg_decode(path: &Path) -> Result<Vec<u8>, String> {
    let out = path.with_extension("yuv");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .output()
        .map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err(String::from_utf8_lossy(&status.stderr).to_string());
    }
    let stderr = String::from_utf8_lossy(&status.stderr).to_string();
    if !stderr.trim().is_empty() {
        return Err(format!("ffmpeg complained: {stderr}"));
    }
    std::fs::read(&out).map_err(|e| e.to_string())
}

fn write_au(name: &str, picture: &EncodedPicture) -> PathBuf {
    let path = scratch_dir().join(format!("{name}.265"));
    let mut file = std::fs::File::create(&path).expect("create bitstream");
    file.write_all(&picture.au).expect("write bitstream");
    path
}

fn planes_of(frame: &VideoFrame) -> Vec<u8> {
    let mut out = Vec::new();
    let (w, h) = (frame.width as usize, frame.height as usize);
    for (i, plane) in frame.planes.iter().enumerate() {
        let (pw, ph) = if i == 0 {
            (w, h)
        } else {
            (w.div_ceil(2), h.div_ceil(2))
        };
        for row in 0..ph {
            out.extend_from_slice(&plane.data[row * plane.stride..row * plane.stride + pw]);
        }
    }
    out
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

fn encode(width: u32, height: u32, qp: i32) -> (VideoFrame, EncodedPicture) {
    let mut cfg = EncoderConfig::new(width, height);
    cfg.rate_control = RateControl::ConstantQp(qp);
    cfg.keep_recon = true;
    cfg.picture_hash = true;
    let encoder = Encoder::new(cfg).expect("encoder");
    let frame = test_frame(width, height, 0);
    let coded = encoder.encode_idr(&frame).expect("encode");
    (frame, coded)
}

#[test]
fn ffmpeg_decodes_bit_exactly_at_every_shape() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    // 1920x1080 needs the conformance window on the bottom (1088 coded rows are
    // not used: 1080 is a multiple of 8, but 64x64 CTBs still overhang), 1916
    // needs it on the right, 130x66 exercises a partial CTB in both directions.
    for &(w, h) in &[(64u32, 64u32), (130, 66), (1916, 1080), (352, 288)] {
        let (source, coded) = encode(w, h, 27);
        let path = write_au(&format!("shape-{w}x{h}"), &coded);
        let decoded = match ffmpeg_decode(&path) {
            Ok(bytes) => bytes,
            Err(e) => panic!("{w}x{h}: ffmpeg failed: {e}"),
        };
        let recon = planes_of(coded.recon.as_ref().expect("recon kept"));
        assert_eq!(
            decoded.len(),
            recon.len(),
            "{w}x{h}: decoded {} bytes, reconstruction {} bytes",
            decoded.len(),
            recon.len()
        );
        let mismatches = decoded.iter().zip(&recon).filter(|(a, b)| a != b).count();
        assert_eq!(mismatches, 0, "{w}x{h}: {mismatches} samples differ");
        // And the encode is worth something: PSNR against the source.
        let quality = psnr(&planes_of(&source), &recon);
        assert!(quality > 30.0, "{w}x{h}: PSNR {quality:.2} dB at QP 27");
    }
}

#[test]
fn ffmpeg_verifies_the_decoded_picture_hash() {
    // The MD5 SEI is the encoder's claim about its own reconstruction; ffmpeg's
    // decoder checks that claim against what it decoded when asked to. This is
    // the one oracle that does not depend on this crate being right about
    // anything except the hash itself.
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let (_, coded) = encode(352, 288, 30);
    let path = write_au("hash-352x288", &coded);
    let output = Command::new("ffmpeg")
        .args(["-v", "debug", "-err_detect", "crccheck", "-y", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let log = String::from_utf8_lossy(&output.stderr).to_string();
    let checks: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("Verifying checksum"))
        .collect();
    assert!(!checks.is_empty(), "ffmpeg checked no picture hash: {log}");
    for line in checks {
        assert_eq!(line.matches("correct").count(), 3, "hash mismatch: {line}");
    }
}

#[test]
fn vaapi_hardware_decodes_the_stream() {
    // The other half of "it decodes": a GPU decoder is a different
    // implementation with different tolerances, and it is what edith's HEVC
    // path actually uses. Skipped where there is no render node.
    if !have_ffmpeg() || !Path::new("/dev/dri/renderD128").exists() {
        eprintln!("skipping: no ffmpeg or no VA-API render node");
        return;
    }
    let (_, coded) = encode(1920, 1080, 27);
    let path = write_au("vaapi-1080p", &coded);
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-hwaccel",
            "vaapi",
            "-hwaccel_device",
            "/dev/dri/renderD128",
            "-i",
        ])
        .arg(&path)
        .args(["-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if stderr.contains("Failed to initialise VAAPI")
        || stderr.contains("No VA display found")
        || stderr.contains("Device creation failed")
    {
        eprintln!("skipping: VA-API unavailable here ({})", stderr.trim());
        return;
    }
    assert!(
        output.status.success() && stderr.trim().is_empty(),
        "VA-API decode failed: {stderr}"
    );
}

#[test]
fn quality_rises_as_qp_falls() {
    let mut previous = 0.0;
    for &qp in &[40i32, 32, 27, 22] {
        let (source, coded) = encode(256, 144, qp);
        let recon = planes_of(coded.recon.as_ref().expect("recon"));
        let quality = psnr(&planes_of(&source), &recon);
        assert!(
            quality > previous,
            "QP {qp}: PSNR {quality:.2} dB is not above {previous:.2}"
        );
        previous = quality;
    }
    assert!(
        previous > 40.0,
        "QP 22 should clear 40 dB, got {previous:.2}"
    );
}

#[test]
fn threads_do_not_change_the_bitstream() {
    let mut cfg = EncoderConfig::new(320, 192);
    cfg.rate_control = RateControl::ConstantQp(30);
    cfg.threads = 1;
    let single = Encoder::new(cfg.clone()).unwrap();
    cfg.threads = 8;
    let many = Encoder::new(cfg).unwrap();
    let frame = test_frame(320, 192, 3);
    let a = single.encode_idr(&frame).unwrap();
    let b = many.encode_idr(&frame).unwrap();
    assert_eq!(a.au, b.au, "wavefront changed the bitstream");
}
