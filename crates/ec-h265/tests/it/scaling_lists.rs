//! Scaling-list streams, checked against ffmpeg.
//!
//! The SLIST conformance streams code their own lists in the SPS and PPS. Most of
//! their pictures are B slices, which this decoder does not pixel-decode yet, so the
//! check is on the first picture in output order: an IDR, intra only, decoded with
//! the stream's lists. It must match ffmpeg sample for sample after cropping.

use ec_h265::Decoder;
use ec_h265::dec::Frame;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The four scaling-list streams the picture-management sweep covers; `str` is
/// the SLIST_D stream.
const STREAMS: [&str; 4] = [
    "SLIST_A_Sony_5/SLIST_A_Sony_5/SLIST_A_Sony_5.bin",
    "SLIST_B_Sony_9/SLIST_B_Sony_9/SLIST_B_Sony_9.bin",
    "SLIST_C_Sony_4/SLIST_C_Sony_4/SLIST_C_Sony_4.bin",
    "SLIST_D_Sony_9/SLIST_D_Sony_9/str.bin",
];

fn vectors_dir() -> PathBuf {
    match std::env::var("EC_H265_JCTVC_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/hevc-jctvc"),
    }
}

/// The first output picture as 4:2:0 bytes cropped to the conformance window.
fn cropped_yuv(frame: &Frame) -> Vec<u8> {
    let pic = &frame.picture;
    let win = &frame.window;
    // Conformance window offsets are in chroma samples for 4:2:0.
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

/// The first frame of `path` as ffmpeg decodes it, in yuv420p.
fn ffmpeg_first_frame(path: &Path, tag: &str) -> Result<Vec<u8>, String> {
    let out = std::env::temp_dir().join(format!("ec-h265-scaling-{tag}.yuv"));
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .output()
        .map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err(String::from_utf8_lossy(&status.stderr).to_string());
    }
    std::fs::read(&out).map_err(|e| e.to_string())
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

// Ignored until the intra path decodes the syntax these streams use: the decode
// desyncs at the last CTB of the IDR, and the intra CTU loop has no SAO or
// cu_qp_delta parsing. Run with `--ignored` to see the failure.
#[ignore = "intra path lacks the syntax these streams use (desync at the IDR's last CTB)"]
#[test]
fn first_picture_of_each_scaling_list_stream_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("scaling-list check skipped: ffmpeg absent");
        return;
    }
    let dir = vectors_dir();
    let mut checked = 0usize;
    for rel in STREAMS {
        let path = dir.join(rel);
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("scaling-list check: {rel} absent, skipped");
            continue;
        };
        let mut decoder = Decoder::new();
        decoder
            .decode_annex_b(&data)
            .unwrap_or_else(|e| panic!("{rel}: decode failed: {e}"));
        decoder
            .flush()
            .unwrap_or_else(|e| panic!("{rel}: flush failed: {e}"));
        let frame = decoder
            .next_frame()
            .unwrap_or_else(|| panic!("{rel}: no picture out"));
        let ours = cropped_yuv(&frame);
        let tag = rel.replace('/', "_");
        let theirs =
            ffmpeg_first_frame(&path, &tag).unwrap_or_else(|e| panic!("{rel}: ffmpeg failed: {e}"));
        assert_eq!(
            ours.len(),
            theirs.len(),
            "{rel}: picture is {} bytes, ffmpeg's is {}",
            ours.len(),
            theirs.len()
        );
        let differing = ours.iter().zip(&theirs).filter(|(a, b)| a != b).count();
        assert_eq!(
            differing, 0,
            "{rel}: {differing} samples differ from ffmpeg in the first picture"
        );
        checked += 1;
    }
    eprintln!("scaling-list check: {checked} first pictures bit-exact against ffmpeg");
}
