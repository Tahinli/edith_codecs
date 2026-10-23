//! The personal media library, AV1 half: every `av1` row of
//! `fixtures/real-library-manifest.tsv` is extracted as a low-overhead OBU
//! stream, its first `FRAMES` shown pictures are decoded (film grain applied,
//! as the decoder outputs it), and each frame is compared sample for sample with
//! ffmpeg's `libdav1d` decoder at the same pixel format.
//!
//! Every file either decodes bit-exactly, is refused by name (a pixel format
//! outside 4:2:0 8/10-bit, or a decoder refusal that says "unsupported", such as
//! profile 1 or 2), or fails. A refusal is reported, not counted as a failure.
//!
//! LOCAL ONLY and ignored: the manifest is generated from the user's own media
//! by `scripts/scan-real-library.sh` and is gitignored, so no other host has it.
//!   cargo test -p ec-av1 --test real_library -- --ignored --nocapture

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use ec_av1::encode::Picture;

/// Shown pictures compared per file.
const FRAMES: usize = 300;

/// Pixel formats inside the decoder's scope: 4:2:0 at 8 or 10 bits.
const IN_SCOPE_PIX_FMTS: [&str; 2] = ["yuv420p", "yuv420p10le"];

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join(format!("ec-av1-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// One manifest row: the columns the sweep reads.
struct Entry {
    path: PathBuf,
    container: String,
    width: String,
    height: String,
    pix_fmt: String,
}

fn manifest_av1_entries() -> Vec<Entry> {
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv");
    let text = std::fs::read_to_string(&manifest).unwrap_or_else(|e| {
        panic!(
            "{}: {e}. This test sweeps the personal library; generate the manifest with \
             scripts/scan-real-library.sh on the machine that holds the media.",
            manifest.display()
        )
    });
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 6 || f[2] != "av1" || !Path::new(f[0]).is_file() {
            continue;
        }
        out.push(Entry {
            path: PathBuf::from(f[0]),
            container: f[1].to_string(),
            width: f[3].to_string(),
            height: f[4].to_string(),
            pix_fmt: f[5].to_string(),
        });
    }
    out
}

/// The planes of a picture as the bytes of a planar rawvideo frame: one byte per
/// sample at 8 bits, little-endian u16 above.
fn frame_bytes(pic: &Picture, bps: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((pic.y.len() + pic.u.len() + pic.v.len()) * bps);
    for plane in [&pic.y, &pic.u, &pic.v] {
        for &s in plane.iter() {
            if bps == 1 {
                out.push(s as u8);
            } else {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
    out
}

/// Where byte `offset` of a frame lies: plane name, x and y in that plane.
fn locate(offset: usize, bps: usize, pic: &Picture) -> (&'static str, usize, usize) {
    let s = offset / bps;
    let (w, cw) = (pic.width, pic.u.len() / pic.height.div_ceil(2).max(1));
    let luma = pic.y.len();
    if s < luma {
        ("Y", s % w, s / w)
    } else if s < luma + pic.u.len() {
        let c = s - luma;
        ("U", c % cw.max(1), c / cw.max(1))
    } else {
        let c = s - luma - pic.u.len();
        ("V", c % cw.max(1), c / cw.max(1))
    }
}

/// Running comparison of decoded frames against the oracle's rawvideo stream.
struct Sweep {
    out: ChildStdout,
    child: Child,
    open: bool,
    compared: usize,
    differing_frames: usize,
    first_diff: Option<String>,
    oracle_short: bool,
    index: usize,
    frame_len: usize,
    bps: usize,
    max_sample: u16,
}

impl Sweep {
    fn take(&mut self, pic: &Picture) {
        let index = self.index;
        self.index += 1;
        if !self.open || self.index > FRAMES {
            return;
        }
        if let Some(&s) = pic.y.iter().chain(&pic.u).chain(&pic.v).max()
            && s > self.max_sample
        {
            self.differing_frames += 1;
            self.first_diff.get_or_insert(format!(
                "frame {index}: sample {s} above the maximum {}",
                self.max_sample
            ));
        }
        let want = frame_bytes(pic, self.bps);
        self.frame_len = want.len();
        let mut oracle = vec![0u8; want.len()];
        if self.out.read_exact(&mut oracle).is_err() {
            self.open = false;
            self.oracle_short = true;
            return;
        }
        self.compared += 1;
        if want != oracle {
            self.differing_frames += 1;
            if self.first_diff.is_none() {
                let at = want
                    .iter()
                    .zip(&oracle)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                let (plane, x, y) = locate(at, self.bps, pic);
                self.first_diff = Some(format!("frame {index} plane {plane} x={x} y={y}"));
            }
        }
    }

    fn finish(mut self) -> Sweep {
        if self.open && self.frame_len > 0 {
            let mut probe = vec![0u8; self.frame_len];
            if self.out.read_exact(&mut probe).is_ok() {
                self.first_diff.get_or_insert(format!(
                    "decoder produced {} frames, ffmpeg produced more",
                    self.index
                ));
                self.differing_frames += 1;
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self
    }
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().chars().take(38).collect())
        .unwrap_or_default()
}

/// Extracts the first `FRAMES` pictures of the video as a low-overhead OBU stream.
fn extract(src: &Path, dst: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-frames:v",
            &FRAMES.to_string(),
            "-f",
            "obu",
        ])
        .arg(dst)
        .status()
        .is_ok_and(|s| s.success())
}

/// Decodes one OBU stream and compares it with ffmpeg's libdav1d output.
fn sweep_file(es: &Path, pix_fmt: &str, bit_depth: u32) -> Result<Sweep, String> {
    let bps = if bit_depth > 8 { 2 } else { 1 };
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "libdav1d", "-i"])
        .arg(es)
        .args([
            "-frames:v",
            &FRAMES.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let out = child.stdout.take().expect("piped stdout");
    let mut sweep = Sweep {
        out,
        child,
        open: true,
        compared: 0,
        differing_frames: 0,
        first_diff: None,
        oracle_short: false,
        index: 0,
        frame_len: 0,
        bps,
        max_sample: ((1u32 << bit_depth) - 1) as u16,
    };
    let data = std::fs::read(es).map_err(|e| e.to_string())?;
    let result = ec_av1::stream::decode_stream_with(&data, |pic, _decode_index, shown| {
        if shown {
            sweep.take(pic);
        }
        Ok(())
    });
    let sweep = sweep.finish();
    if let Err(e) = result {
        return Err(format!("{e}"));
    }
    Ok(sweep)
}

#[test]
#[ignore = "sweeps the PERSONAL media library (fixtures/real-library-manifest.tsv, av1 rows) through ffmpeg/libdav1d; minutes of wall time. Local only: run with --ignored."]
fn real_library_av1_streams_match_ffmpeg() {
    assert!(have_ffmpeg(), "ffmpeg is required to sweep the library");
    let entries = manifest_av1_entries();
    assert!(!entries.is_empty(), "the manifest lists no av1 rows");
    let dir = scratch("real-library-av1");
    let mut table = String::new();
    let mut decoded = 0usize;
    let mut refused = 0usize;
    let mut failures = Vec::new();

    for entry in &entries {
        let name = short_name(&entry.path);
        let res = format!("{}x{}", entry.width, entry.height);
        let bit_depth: u32 = if entry.pix_fmt == "yuv420p10le" {
            10
        } else {
            8
        };
        if !IN_SCOPE_PIX_FMTS.contains(&entry.pix_fmt.as_str()) {
            refused += 1;
            table.push_str(&format!(
                "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} refused: pixel format {} is outside 4:2:0 8/10-bit\n",
                entry.container, "-", entry.pix_fmt
            ));
            continue;
        }
        let es = dir.join("stream.obu");
        if !extract(&entry.path, &es) {
            failures.push(format!("{name}: extraction failed"));
            table.push_str(&format!(
                "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} ERROR extraction\n",
                entry.container, "-"
            ));
            continue;
        }
        match sweep_file(&es, &entry.pix_fmt, bit_depth) {
            Ok(s) if s.differing_frames > 0 => {
                failures.push(format!("{name}: {} frames differ", s.differing_frames));
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} MISMATCH {} of {} frames; first: {}\n",
                    entry.container, s.compared, s.differing_frames, s.compared,
                    s.first_diff.clone().unwrap_or_default()
                ));
            }
            Ok(s) => {
                decoded += 1;
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} bit-exact{}\n",
                    entry.container,
                    s.compared,
                    if s.oracle_short {
                        " (ffmpeg stopped early)"
                    } else {
                        ""
                    }
                ));
            }
            Err(e) if e.contains("unsupported") => {
                refused += 1;
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} refused: {e}\n",
                    entry.container, "-"
                ));
            }
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} ERROR {e}\n",
                    entry.container, "-"
                ));
            }
        }
        let _ = std::fs::remove_file(&es);
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    eprintln!(
        "real library av1: {decoded} bit-exact, {refused} refused by name, {} failed",
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(decoded >= 1, "no real AV1 file was swept end to end");
}
