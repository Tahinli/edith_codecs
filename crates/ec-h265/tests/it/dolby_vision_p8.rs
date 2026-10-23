//! Dolby Vision profile 8 layout: the HEVC base layer decodes, and the RPU NAL
//! units (type 62) are ignored.
//!
//! The base layer is generated here with ffmpeg's libx265 from a test pattern, so no
//! copyrighted media is involved. Type 62 NAL units with arbitrary payload are then
//! inserted in front of every VCL NAL unit. ec-h265 must decode the injected stream
//! to exactly the pictures it decodes from the clean one, and ffmpeg must decode the
//! injected stream to the same bytes.
//!
//! The test skips when ffmpeg is absent, unless `EC_REQUIRE_FIXTURES` is set.

use ec_h265::Decoder;
use ec_h265::dec::Frame;
use ec_h265_syntax::nal::{escape_rbsp, split_annex_b};
use std::path::Path;
use std::process::Command;

const WIDTH: usize = 352;
const HEIGHT: usize = 288;
const FRAMES: usize = 12;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The base layer: libx265, an IDR every 4 pictures, P pictures in between, no B.
fn encode_base_layer(dir: &Path) -> Result<Vec<u8>, String> {
    let out = dir.join("base.hevc");
    let frames = FRAMES.to_string();
    let source = format!("testsrc2=size={WIDTH}x{HEIGHT}:rate=30");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(&source)
        .arg("-frames:v")
        .arg(&frames)
        .args([
            "-c:v", "libx265", "-preset", "veryfast", "-pix_fmt", "yuv420p",
        ])
        .args([
            "-x265-params",
            "keyint=4:min-keyint=4:bframes=0:scenecut=0:log-level=error",
            "-f",
            "hevc",
        ])
        .arg(&out)
        .output()
        .map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err(String::from_utf8_lossy(&status.stderr).to_string());
    }
    std::fs::read(&out).map_err(|e| e.to_string())
}

/// A type 62 NAL unit with arbitrary payload: zero runs, start-code-like bytes and
/// a trailing zero, escaped as a NAL unit must be. `seed` varies it per slice.
fn rpu_nal(seed: u8) -> Vec<u8> {
    let rbsp = [
        0x19, 0x08, 0x00, 0x00, 0x01, seed, 0x00, 0x00, 0x03, 0xff, 0x80, 0x00,
    ];
    let mut v = vec![62 << 1, 1];
    escape_rbsp(&rbsp, &mut v);
    v
}

/// The stream with a type 62 NAL unit in front of every VCL NAL unit, each with
/// a four-byte start code.
fn inject_rpu(annex_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(annex_b.len() * 2);
    let mut slice = 0u8;
    for n in split_annex_b(annex_b) {
        let mut nal = n.header.to_bytes().to_vec();
        nal.extend_from_slice(n.payload);
        if matches!((nal[0] >> 1) & 0x3f, 0..=21) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&rpu_nal(slice));
            slice = slice.wrapping_add(1);
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&nal);
    }
    out
}

/// Count of NAL units of `nal_type` in an Annex B stream.
fn count_nal_type(annex_b: &[u8], nal_type: u8) -> usize {
    split_annex_b(annex_b)
        .iter()
        .filter(|n| (n.header.to_bytes()[0] >> 1) & 0x3f == nal_type)
        .count()
}

/// One picture as 4:2:0 bytes, cropped to the conformance window.
fn cropped_yuv(frame: &Frame) -> Vec<u8> {
    let pic = &frame.picture;
    let win = &frame.window;
    let (lx0, lx1) = (2 * win.left as usize, pic.width - 2 * win.right as usize);
    let (ly0, ly1) = (2 * win.top as usize, pic.height - 2 * win.bottom as usize);
    let (cx0, cx1) = (win.left as usize, pic.width / 2 - win.right as usize);
    let (cy0, cy1) = (win.top as usize, pic.height / 2 - win.bottom as usize);
    let mut out = Vec::new();
    for row in ly0..ly1 {
        out.extend(
            pic.y[row * pic.width + lx0..row * pic.width + lx1]
                .iter()
                .map(|&v| v as u8),
        );
    }
    let cw = pic.width / 2;
    for plane in [&pic.cb, &pic.cr] {
        for row in cy0..cy1 {
            out.extend(
                plane[row * cw + cx0..row * cw + cx1]
                    .iter()
                    .map(|&v| v as u8),
            );
        }
    }
    out
}

/// Every picture ec-h265 outputs, as one yuv420p byte string, and the picture count.
fn decode_ec(annex_b: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut dec = Decoder::new();
    dec.decode_annex_b(annex_b).map_err(|e| e.to_string())?;
    dec.flush().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut count = 0;
    while let Some(frame) = dec.next_frame() {
        out.extend(cropped_yuv(&frame));
        count += 1;
    }
    Ok((out, count))
}

/// Every picture ffmpeg outputs, as yuv420p.
fn decode_ffmpeg(path: &Path, out: &Path) -> Result<Vec<u8>, String> {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(out)
        .output()
        .map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err(String::from_utf8_lossy(&status.stderr).to_string());
    }
    std::fs::read(out).map_err(|e| e.to_string())
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

#[test]
fn dolby_vision_p8_base_layer_decodes_and_rpu_is_ignored() {
    if !have_ffmpeg() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but ffmpeg is absent"
        );
        eprintln!("dolby vision profile 8 check skipped: ffmpeg absent");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ec-h265-dvp8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let clean = encode_base_layer(&dir).expect("libx265 base layer");
    let injected = inject_rpu(&clean);
    let vcl = split_annex_b(&clean)
        .iter()
        .filter(|n| matches!((n.header.to_bytes()[0] >> 1) & 0x3f, 0..=21))
        .count();
    assert_eq!(
        count_nal_type(&clean, 62),
        0,
        "clean base layer has type 62 NAL units"
    );
    assert_eq!(
        count_nal_type(&injected, 62),
        vcl,
        "one type 62 NAL unit per slice"
    );

    let (ec_clean, n_clean) = decode_ec(&clean).expect("ec-h265 decodes the clean base layer");
    let (ec_injected, n_injected) =
        decode_ec(&injected).expect("ec-h265 decodes the stream with type 62 NAL units");
    assert_eq!(n_clean, FRAMES, "clean stream picture count");
    assert_eq!(n_injected, FRAMES, "injected stream picture count");
    let frame_bytes = WIDTH * HEIGHT * 3 / 2;
    assert_eq!(ec_clean.len(), FRAMES * frame_bytes);
    if let Some(at) = first_difference(&ec_clean, &ec_injected) {
        panic!("type 62 NAL units changed ec-h265's output at byte {at}");
    }

    let injected_path = dir.join("dv-p8.hevc");
    std::fs::write(&injected_path, &injected).expect("write injected stream");
    let ffmpeg = decode_ffmpeg(&injected_path, &dir.join("ffmpeg.yuv"))
        .expect("ffmpeg decodes the injected stream");
    assert_eq!(
        ffmpeg.len(),
        ec_injected.len(),
        "ffmpeg picture bytes differ from ec-h265's"
    );
    if let Some(at) = first_difference(&ec_injected, &ffmpeg) {
        panic!("ec-h265 and ffmpeg differ on the injected stream at byte {at}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
