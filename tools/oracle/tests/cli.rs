//! Self-test of the `oracle` CLI itself: drives the built binary the way a codec
//! crate's test will, using ffmpeg as both the "our decoder" stand-in and the
//! reference. Exercises the PASS and the FAIL path of every subcommand.
//!
//! Fixture-gated: fixtures/ is gitignored, so a checkout without
//! `scripts/gen-fixtures.sh` run skips loudly instead of failing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

// Per-test directory: the test binary runs its cases as threads of one process,
// so a pid-only name would let one case delete another's raw files.
fn workdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oracle-selftest-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Decode `input` to a raw file with ffmpeg — stands in for "our decoder's output".
fn decode_raw(input: &Path, out: &Path, extra: &[&str]) {
    let f = std::fs::File::create(out).unwrap();
    let st = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-i"])
        .arg(input)
        .args(extra)
        .arg("-")
        .stdout(Stdio::from(f))
        .status()
        .expect("ffmpeg must be installed to run the oracle self-test");
    assert!(st.success(), "ffmpeg failed decoding {}", input.display());
}

fn oracle(args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_oracle"))
        .args(args)
        .status()
        .expect("oracle binary")
        .code()
        .expect("oracle exited by signal")
}

fn require(paths: &[PathBuf]) -> bool {
    for p in paths {
        if !p.is_file() {
            eprintln!(
                "SKIP: missing fixture {} — run scripts/gen-fixtures.sh",
                p.display()
            );
            return false;
        }
    }
    true
}

const F32: &[&str] = &["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le"];

#[test]
fn bit_exact_passes_on_same_decode_and_fails_on_a_different_one() {
    let fx = fixtures();
    let a = fx.join("audio/flac-stereo-48000.flac");
    let b = fx.join("audio/flac-mono-48000.flac");
    if !require(&[a.clone(), b.clone()]) {
        return;
    }
    let w = workdir("bitexact");
    let (r1, r2, r3) = (w.join("a1.raw"), w.join("a2.raw"), w.join("b.raw"));
    decode_raw(&a, &r1, F32);
    decode_raw(&a, &r2, F32);
    decode_raw(&b, &r3, F32);

    assert_eq!(
        oracle(&["bit-exact", r1.to_str().unwrap(), r2.to_str().unwrap()]),
        0,
        "same file decoded twice must be bit-exact"
    );
    assert_eq!(
        oracle(&["bit-exact", r1.to_str().unwrap(), r3.to_str().unwrap()]),
        1,
        "different fixtures must not be bit-exact"
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn audio_compare_passes_against_its_own_source_and_fails_across_rates() {
    let fx = fixtures();
    let a = fx.join("audio/flac-stereo-48000.flac");
    let b = fx.join("audio/flac-stereo-44100.flac");
    if !require(&[a.clone(), b.clone()]) {
        return;
    }
    let w = workdir("audio");
    let (ra, rb) = (w.join("a.raw"), w.join("b.raw"));
    decode_raw(&a, &ra, F32);
    decode_raw(&b, &rb, F32);

    assert_eq!(
        oracle(&["audio-compare", ra.to_str().unwrap(), a.to_str().unwrap()]),
        0,
        "lossless decode vs its own source must correlate at 1.0"
    );
    // 44.1 kHz samples against a 48 kHz reference: wrong length and wrong phase.
    assert_eq!(
        oracle(&["audio-compare", rb.to_str().unwrap(), a.to_str().unwrap()]),
        1,
        "a different-rate decode must be rejected"
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn video_compare_passes_against_its_own_source_and_fails_on_frame_count() {
    let fx = fixtures();
    let a = fx.join("video/h264-1080p-23.976-8bit.mp4");
    let b = fx.join("video/h264-1080p-60-8bit.mp4");
    if !require(&[a.clone(), b.clone()]) {
        return;
    }
    let w = workdir("video");
    let raw = w.join("a.raw");
    decode_raw(
        &a,
        &raw,
        &["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "yuv420p"],
    );

    assert_eq!(
        oracle(&[
            "video-compare",
            raw.to_str().unwrap(),
            a.to_str().unwrap(),
            "--pix-fmt",
            "yuv420p",
        ]),
        0,
        "decode vs its own source must be lossless (infinite PSNR)"
    );
    assert_eq!(
        oracle(&[
            "video-compare",
            raw.to_str().unwrap(),
            b.to_str().unwrap(),
            "--pix-fmt",
            "yuv420p",
        ]),
        1,
        "48 frames against a 120-frame reference must fail"
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn bad_usage_exits_two() {
    assert_eq!(oracle(&["no-such-subcommand"]), 2);
    assert_eq!(oracle(&["bit-exact", "only-one-arg"]), 2);
}
