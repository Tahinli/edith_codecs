//! The BD gate's source arming, shared with `examples/enc_probe` (lane-probe).
//!
//! The probe used to build its own ffmpeg command line and dropped the gate's
//! `-ss` seek, so it read film B's black leader and every census taken through
//! it was probe-relative (59 kB where the gate's own ladder point coded 89 kB
//! of the same title). One definition now serves both: the gate's
//! `clip_frames_vf` and the probe both call [`source`], so the two can only
//! differ in the arguments they pass, which
//! `encode::tests::the_probe_arms_the_gates_source` pins.

use crate::encode::Picture;
use std::process::Command;

/// The video stream's coded size, from ffprobe.
#[must_use]
pub fn dims(clip: &str) -> Option<(usize, usize)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(clip)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut f = text.trim().split(',');
    Some((
        f.next()?.trim().parse().ok()?,
        f.next()?.trim().parse().ok()?,
    ))
}

/// The native BD gate's window on `clip`: a whole number of 128-wide
/// superblocks, at most 1920x1024, out of the MIDDLE of the coded frame, with
/// even offsets for 4:2:0 -- as an ffmpeg `crop` filter plus its size.
///
/// This lives here because the probe used to be handed the crop by hand, and
/// a hand-written `crop=1920:1024:960:568` (a 2160-high frame that film B,
/// coded 3840x1608, is not) put the instrument on different pixels than the
/// gate: 60 kB where the gate's own q=150 point coded 90 kB.
///
/// # Panics
/// When ffprobe cannot read the size, or the frame is under one superblock.
#[must_use]
pub fn gate_crop(clip: &str) -> (String, usize, usize) {
    let (nw, nh) = dims(clip).expect("ffprobe gave no size");
    let cw = nw.min(1920) / 128 * 128;
    let ch = nh.min(1024) / 128 * 128;
    assert!(
        cw >= 128 && ch >= 128,
        "{nw}x{nh} is smaller than a superblock"
    );
    let (x, y) = ((nw - cw) / 2 & !1, (nh - ch) / 2 & !1);
    (format!("crop={cw}:{ch}:{x}:{y}"), cw, ch)
}

/// `frames` pictures of `clip` from `skip` through the filter chain `vf`, as
/// planar 8-bit 4:2:0 at `width`x`height` -- the BD gate's loader recipe.
///
/// # Panics
/// When ffmpeg cannot be run, fails, or delivers a different number of frames.
#[must_use]
pub fn source(
    clip: &str,
    skip: &str,
    vf: &str,
    width: usize,
    height: usize,
    frames: usize,
) -> Vec<Picture> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", skip, "-i", clip])
        .args(["-frames:v", &frames.to_string()])
        .args(["-vf", vf])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg failed to run");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (luma, chroma) = (width * height, width * height / 4);
    let frame_len = luma + 2 * chroma;
    assert_eq!(
        out.stdout.len(),
        frame_len * frames,
        "expected {frames} 4:2:0 frames"
    );
    (0..frames)
        .map(|i| {
            let b = &out.stdout[i * frame_len..][..frame_len];
            Picture {
                width,
                height,
                y: b[..luma].iter().map(|&v| u16::from(v)).collect(),
                u: b[luma..luma + chroma]
                    .iter()
                    .map(|&v| u16::from(v))
                    .collect(),
                v: b[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
            }
        })
        .collect()
}

/// Runs a test-harness child (`aomenc`, `ffmpeg`) with `input` on its stdin
/// and all three pipes drained concurrently, returning the child's
/// [`std::process::Output`] (status, stdout, stderr) for the caller to
/// assert on.
///
/// The ONE spawn path for every test that feeds a child on stdin
/// (lane-av1-pipedrain). Writing the input INLINE before
/// `wait_with_output()` deadlocks as soon as the child's stdout pipe buffer
/// (~64 KiB) fills before the last input byte is written: the child blocks
/// in write(2), the test blocks in `write_all`, both at 0% CPU until the
/// harness timeout -- measured at 45 minutes on a 1.1 MB fixture decoding
/// to 150 MB of raw frames (lane-t900 r10), and four aomenc gates still sat
/// on the inline pattern after that fix. Here a writer thread owns stdin
/// while the parent drains stdout/stderr, so a multi-MB y4m always meets a
/// reader, never a blocked writer.
///
/// A child that exits early (bad flags, refused input) breaks the writer's
/// pipe: the EPIPE from `write_all` is swallowed on purpose, and the
/// child's own exit status and stderr -- which the caller asserts on --
/// carry the real diagnosis.
///
/// # Panics
/// When the child cannot be spawned or reaped, or the writer thread panics.
#[cfg(test)]
pub(crate) fn run_with_stdin(cmd: &mut Command, input: &[u8]) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("encoder failed to start");
    let mut stdin = child.stdin.take().expect("encoder stdin");
    let payload = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let out = child.wait_with_output().expect("encoder failed to run");
    writer.join().expect("encoder stdin writer thread");
    out
}
