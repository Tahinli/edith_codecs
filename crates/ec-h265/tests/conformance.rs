//! Conformance: every access unit this encoder writes must decode in ffmpeg,
//! and the picture ffmpeg produces must be *bit-identical* to the encoder's own
//! reconstruction. That equality is the whole point of coding with the in-loop
//! filters off: it turns "the file plays" into "the decoder and the encoder
//! agree on every sample".

mod common;

use common::{natural_frame, test_frame};
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

/// Encode one picture; `ctb` of `None` takes the configured default.
fn encode(width: u32, height: u32, qp: i32, ctb: Option<usize>) -> (VideoFrame, EncodedPicture) {
    encode_with(width, height, qp, ctb, false)
}

fn encode_with(
    width: u32,
    height: u32,
    qp: i32,
    ctb: Option<usize>,
    chroma_mode_search: bool,
) -> (VideoFrame, EncodedPicture) {
    let mut cfg = EncoderConfig::new(width, height);
    cfg.chroma_mode_search = chroma_mode_search;
    cfg.rate_control = RateControl::ConstantQp(qp);
    if let Some(ctb) = ctb {
        cfg.ctb_size = ctb;
    }
    cfg.keep_recon = true;
    cfg.picture_hash = true;
    let encoder = Encoder::new(cfg).expect("encoder");
    let frame = test_frame(width, height, 0);
    let coded = encoder.encode_idr(&frame).expect("encode");
    (frame, coded)
}

/// The chroma mode search is off by default (it loses on screen content, see
/// `EncoderConfig::chroma_mode_search`), so the shapes above never code an
/// `intra_chroma_pred_mode` other than the derived one. This exercises the four
/// explicit modes: the stream has to differ from the derived-only one — proving
/// the search actually fired — and still decode bit-exactly.
#[test]
fn chroma_mode_search_decodes_bit_exactly() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    for &(w, h) in &[(64u32, 64u32), (130, 66), (352, 288)] {
        let (source, coded) = encode_with(w, h, 27, None, true);
        let (_, derived) = encode_with(w, h, 27, None, false);
        let name = format!("chroma-rd-{w}x{h}");
        assert_ne!(
            coded.au, derived.au,
            "{name}: the search never picked a mode other than the derived one, so this decode proves nothing"
        );
        let path = write_au(&name, &coded);
        let decoded = match ffmpeg_decode(&path) {
            Ok(bytes) => bytes,
            Err(e) => panic!("{name}: ffmpeg failed: {e}"),
        };
        let recon = planes_of(coded.recon.as_ref().expect("recon kept"));
        assert_eq!(decoded.len(), recon.len(), "{name}: size mismatch");
        let mismatches = decoded.iter().zip(&recon).filter(|(a, b)| a != b).count();
        assert_eq!(mismatches, 0, "{name}: {mismatches} samples differ");
        let quality = psnr(&planes_of(&source), &recon);
        assert!(quality > 30.0, "{name}: PSNR {quality:.2} dB at QP 27");
    }
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
    // Both tree sizes are coded: the default is 32, and 64 is still offered.
    for &(w, h, ctb) in &[
        (64u32, 64u32, None),
        (130, 66, None),
        (1916, 1080, None),
        (352, 288, None),
        (1916, 1080, Some(64)),
        (130, 66, Some(64)),
    ] {
        let (source, coded) = encode(w, h, 27, ctb);
        let name = format!(
            "shape-{w}x{h}-ctb{}",
            ctb.map_or("default".to_string(), |c| c.to_string())
        );
        let path = write_au(&name, &coded);
        let decoded = match ffmpeg_decode(&path) {
            Ok(bytes) => bytes,
            Err(e) => panic!("{name}: ffmpeg failed: {e}"),
        };
        let recon = planes_of(coded.recon.as_ref().expect("recon kept"));
        assert_eq!(
            decoded.len(),
            recon.len(),
            "{name}: decoded {} bytes, reconstruction {} bytes",
            decoded.len(),
            recon.len()
        );
        let mismatches = decoded.iter().zip(&recon).filter(|(a, b)| a != b).count();
        assert_eq!(mismatches, 0, "{name}: {mismatches} samples differ");
        // And the encode is worth something: PSNR against the source.
        let quality = psnr(&planes_of(&source), &recon);
        assert!(quality > 30.0, "{name}: PSNR {quality:.2} dB at QP 27");
    }
}

/// The 32x32 default buys wavefront rows (see `perf.rs`) by coding a shallower
/// tree; that trade is only worth taking if the bits it costs are noise.
#[test]
fn the_default_tree_costs_almost_nothing_against_64() {
    let source = natural_frame(1920, 1080, 0);
    let mut coded = Vec::new();
    for ctb in [32usize, 64] {
        let mut cfg = EncoderConfig::new(1920, 1080);
        cfg.rate_control = RateControl::ConstantQp(27);
        cfg.ctb_size = ctb;
        cfg.keep_recon = true;
        let encoder = Encoder::new(cfg).expect("encoder");
        let picture = encoder.encode_idr(&source).expect("encode");
        let quality = psnr(
            &planes_of(&source),
            &planes_of(picture.recon.as_ref().unwrap()),
        );
        coded.push((picture.au.len(), quality));
    }
    let ((bits32, psnr32), (bits64, psnr64)) = (coded[0], coded[1]);
    println!(
        "1080p at QP 27: CTB 32 {bits32} bytes / {psnr32:.2} dB, CTB 64 {bits64} bytes / {psnr64:.2} dB \
         ({:+.1}% bits, {:+.2} dB)",
        (bits32 as f64 / bits64 as f64 - 1.0) * 100.0,
        psnr32 - psnr64
    );
    assert!(
        // Measured 1.0% on this fixture; the bound leaves room for the shape of
        // the picture, not for a change of coding behaviour.
        (bits32 as f64) < bits64 as f64 * 1.03,
        "CTB 32 spent {bits32} bytes against 64's {bits64}"
    );
    assert!(
        (psnr32 - psnr64).abs() < 0.1,
        "CTB 32 landed {psnr32:.2} dB against 64's {psnr64:.2} dB"
    );
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
    let (_, coded) = encode(352, 288, 30, None);
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
    let (_, coded) = encode(1920, 1080, 27, None);
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
        let (source, coded) = encode(256, 144, qp, None);
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

#[test]
fn a_bit_target_lands_near_its_target() {
    // The model picks a QP from bits-per-pixel; a picture that comes out more
    // than a quarter off is coded once more at a corrected QP. What is asserted
    // is the contract that mode offers — near the target, monotonic in it — not
    // a bitrate to the byte, which no single-picture encoder can promise.
    let mut previous = 0usize;
    for target in [400_000u64, 1_200_000, 3_000_000] {
        let mut cfg = EncoderConfig::new(640, 360);
        cfg.rate_control = RateControl::TargetBits(target);
        let encoder = Encoder::new(cfg).expect("encoder");
        let frame = test_frame(640, 360, 5);
        let coded = encoder.encode_idr(&frame).expect("encode");
        let bits = coded.au.len() * 8;
        assert!(
            bits > previous,
            "target {target}: {bits} bits did not grow with the target"
        );
        previous = bits;
        let ratio = bits as f64 / target as f64;
        assert!(
            (0.4..2.5).contains(&ratio),
            "target {target}: got {bits} bits (x{ratio:.2}) at QP {}",
            coded.qp
        );
    }
}

#[test]
fn the_batch_helper_codes_every_picture_as_its_own_access_unit() {
    let cfg = EncoderConfig::new(128, 96);
    let encoder = Encoder::new(cfg).expect("encoder");
    let frames: Vec<VideoFrame> = (0..3).map(|i| test_frame(128, 96, i * 11)).collect();
    let coded = encoder.encode_batch(frames.iter()).expect("batch encode");
    assert_eq!(coded.len(), 3);
    for picture in &coded {
        // Every access unit stands alone: parameter sets, then an IDR slice.
        let nals = ec_h265_syntax::split_annex_b(&picture.au);
        let types: Vec<_> = nals.iter().map(|n| n.header.nal_type).collect();
        assert_eq!(
            &types[..4],
            &[
                ec_h265_syntax::NalUnitType::Vps,
                ec_h265_syntax::NalUnitType::Sps,
                ec_h265_syntax::NalUnitType::Pps,
                ec_h265_syntax::NalUnitType::IdrWRadl,
            ],
            "access unit is not self-contained"
        );
    }
    // Different pictures, different bitstreams.
    assert_ne!(coded[0].au, coded[1].au);
}
