//! 4:2:2 AV1 streams, decoded by `ec_av1` and by ffmpeg's `libdav1d`, compared
//! sample for sample. Each stream is encoded by ffmpeg's `libaom-av1` from a
//! synthetic source (mandelbrot plus temporal noise), so no media is committed.
//!
//! Cases cover 8 and 10 bit, CDEF and loop restoration on and off, and odd
//! sizes (176x96, 200x120, 330x190) next to the 352x288 clip that first showed
//! the right-edge chroma defect (8 bit, CDEF and LR off, frame 3).
//!
//! The test SKIPs when ffmpeg lacks the `libaom-av1` encoder or the `libdav1d`
//! decoder.

use ec_av1::encode::Picture;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FRAMES: usize = 6;

struct Case {
    width: usize,
    height: usize,
    bit_depth: u8,
    /// `true` keeps libaom's default CDEF and loop restoration. `false` turns
    /// both off through `-aom-params`, which isolates reconstruction.
    filters: bool,
    /// Noise seed; odd seeds use testsrc2, even seeds mandelbrot.
    seed: u32,
}

impl Case {
    fn name(&self) -> String {
        format!(
            "422_{}x{}_{}bit_{}_s{}",
            self.width,
            self.height,
            self.bit_depth,
            if self.filters { "cdeflr_on" } else { "cdeflr_off" },
            self.seed
        )
    }

    fn pix_fmt(&self) -> &'static str {
        match self.bit_depth {
            8 => "yuv422p",
            10 => "yuv422p10le",
            _ => "yuv422p12le",
        }
    }

    fn bytes_per_sample(&self) -> usize {
        if self.bit_depth == 8 { 1 } else { 2 }
    }
}

fn cases() -> Vec<Case> {
    let mut v = vec![
        Case { width: 352, height: 288, bit_depth: 8, filters: false, seed: 0 },
        Case { width: 352, height: 288, bit_depth: 8, filters: true, seed: 0 },
    ];
    for (w, h) in [(176, 96), (200, 120), (330, 190)] {
        for bit_depth in [8, 10] {
            for filters in [false, true] {
                v.push(Case { width: w, height: h, bit_depth, filters, seed: 0 });
            }
        }
    }
    // Small and odd frames with loop restoration on, all depths, varied content.
    let full = std::env::var_os("EC_422_SWEEP").is_some();
    let sizes: &[(usize, usize)] =
        &[(64, 64), (72, 40), (160, 96), (192, 120), (256, 120), (330, 190)];
    for &(w, h) in sizes {
        for bit_depth in [8, 10, 12] {
            let seeds: &[u32] = if full { &[1, 2, 3, 4, 5] } else { &[1, 2, 4] };
            for &seed in seeds {
                // Keep the default run modest: 64x64 everywhere, the rest only in the sweep.
                if full || (w, h) == (64, 64) || (bit_depth == 10 && seed == 2 && h == 120) {
                    v.push(Case { width: w, height: h, bit_depth, filters: true, seed });
                }
            }
        }
    }
    v
}

/// Whether ffmpeg has both `libaom-av1` (encode) and `libdav1d` (decode).
fn have_tools() -> bool {
    let list = |flag: &str| -> String {
        Command::new("ffmpeg")
            .args(["-hide_banner", flag])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    list("-encoders").contains("libaom-av1") && list("-decoders").contains("libdav1d")
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join(format!("ec-av1-422-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Encodes the case to a raw OBU stream at `path`.
fn encode(case: &Case, path: &Path) {
    let src = if case.seed % 2 == 1 { "testsrc2" } else { "mandelbrot" };
    let source = format!("{src}=size={}x{}:rate=25", case.width, case.height);
    let noise = format!("noise=alls=12:allf=t:all_seed={}", 1000 + case.seed);
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v", "error", "-y", "-f", "lavfi", "-i", &source, "-vf",
        &noise, "-frames:v", &FRAMES.to_string(), "-c:v", "libaom-av1",
        "-cpu-used", if case.seed == 0 { "2" } else { "7" }, "-lag-in-frames", "0", "-crf", "25", "-b:v", "0", "-pix_fmt",
        case.pix_fmt(),
    ]);
    if !case.filters {
        cmd.args(["-aom-params", "enable-cdef=0:enable-restoration=0"]);
    }
    cmd.args(["-f", "obu"]).arg(path);
    let out = cmd.output().expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "{}: ffmpeg encode failed: {}",
        case.name(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Splits one packed rawvideo frame stream into per-plane `u16` samples.
fn split_frames(case: &Case, raw: &[u8]) -> Vec<(Vec<u16>, Vec<u16>, Vec<u16>)> {
    let cw = case.width.div_ceil(2);
    let (luma, chroma) = (case.width * case.height, cw * case.height);
    let bps = case.bytes_per_sample();
    let frame_bytes = (luma + 2 * chroma) * bps;
    assert_eq!(raw.len() % frame_bytes, 0, "{}: ragged oracle output", case.name());
    let sample = |i: usize| -> u16 {
        if bps == 1 {
            u16::from(raw[i])
        } else {
            u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]])
        }
    };
    (0..raw.len() / frame_bytes)
        .map(|f| {
            let base = f * (luma + 2 * chroma);
            let y = (0..luma).map(|i| sample(base + i)).collect();
            let u = (0..chroma).map(|i| sample(base + luma + i)).collect();
            let v = (0..chroma).map(|i| sample(base + luma + chroma + i)).collect();
            (y, u, v)
        })
        .collect()
}

fn dav1d_decode(case: &Case, path: &Path) -> Vec<(Vec<u16>, Vec<u16>, Vec<u16>)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "libdav1d", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", case.pix_fmt(), "-"])
        .stderr(Stdio::piped())
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "{}: libdav1d refused the stream: {}",
        case.name(),
        String::from_utf8_lossy(&out.stderr)
    );
    split_frames(case, &out.stdout)
}

/// Compares every shown frame and returns one line per differing plane.
fn compare(case: &Case, ours: &[Picture], oracle: &[(Vec<u16>, Vec<u16>, Vec<u16>)]) -> Vec<String> {
    let mut report = Vec::new();
    if ours.len() != oracle.len() {
        report.push(format!(
            "frame count: ours {} vs libdav1d {}",
            ours.len(),
            oracle.len()
        ));
    }
    let cw = case.width.div_ceil(2);
    for (i, (pic, (oy, ou, ov))) in ours.iter().zip(oracle).enumerate() {
        for (name, got, want, w) in [
            ("Y", &pic.y, oy, case.width),
            ("U", &pic.u, ou, cw),
            ("V", &pic.v, ov, cw),
        ] {
            let mut count = 0usize;
            let mut first = None;
            for (idx, (a, b)) in got.iter().zip(want.iter()).enumerate() {
                if a != b {
                    count += 1;
                    if first.is_none() {
                        first = Some((idx % w, idx / w, *a, *b));
                    }
                }
            }
            if got.len() != want.len() {
                report.push(format!(
                    "frame {i} plane {name}: length ours {} vs libdav1d {}",
                    got.len(),
                    want.len()
                ));
            } else if let Some((x, y, a, b)) = first {
                report.push(format!(
                    "frame {i} plane {name}: {count} samples differ, first at x={x} y={y} (ours {a}, libdav1d {b})"
                ));
            }
        }
    }
    report
}

#[test]
fn yuv422_matches_dav1d_byte_exact() {
    if !have_tools() {
        eprintln!("SKIP yuv422_matches_dav1d_byte_exact: ffmpeg lacks libaom-av1 or libdav1d");
        return;
    }
    let dir = scratch("cases");
    let mut failures = Vec::new();
    for case in cases() {
        let path = dir.join(format!("{}.obu", case.name()));
        encode(&case, &path);
        let data = std::fs::read(&path).expect("encoded stream");
        let ours = ec_av1::stream::decode_stream(&data)
            .unwrap_or_else(|e| panic!("{}: ec-av1 refused the stream: {e}", case.name()));
        let oracle = dav1d_decode(&case, &path);
        let report = compare(&case, &ours, &oracle);
        if !report.is_empty() {
            failures.push(format!("{}:\n  {}", case.name(), report.join("\n  ")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "4:2:2 decode differs from libdav1d:\n{}",
        failures.join("\n")
    );
}
