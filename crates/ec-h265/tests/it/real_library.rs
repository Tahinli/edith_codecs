//! The personal media library, HEVC half: every `hevc` row of
//! `fixtures/real-library-manifest.tsv` is extracted to Annex B, its first
//! `FRAMES` pictures are decoded, and each output frame is compared sample for
//! sample with ffmpeg's own `hevc` decoder at the same pixel format.
//!
//! Every file either decodes bit-exactly, is refused by name (a pixel format or
//! a bit depth outside 4:2:0 8/10-bit, or a decoder refusal that says
//! "unsupported"), or fails. A refusal is reported, not counted as a failure.
//!
//! LOCAL ONLY and ignored: the manifest is generated from the user's own media
//! by `scripts/scan-real-library.sh` and is gitignored, so no other host has it.
//!   cargo test -p ec-h265 --test real_library -- --ignored --nocapture

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ec_h265::dec::Decoder;
use ec_h265::decode::DecodedPicture;
use ec_h265_syntax::{ConformanceWindow, split_annex_b};

/// Pictures compared per file.
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
    let dir = base.join(format!("ec-h265-{tag}-{}", std::process::id()));
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

fn manifest_hevc_entries() -> Vec<Entry> {
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
        if f.len() < 6 || f[2] != "hevc" || !Path::new(f[0]).is_file() {
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

/// The samples of a picture cropped to its conformance window, as the bytes of a
/// planar rawvideo frame: one byte per sample at 8 bits, little-endian u16 above.
fn cropped(pic: &DecodedPicture, win: &ConformanceWindow, bps: usize) -> Vec<u8> {
    let (l, r, t, b) = (
        win.left as usize,
        win.right as usize,
        win.top as usize,
        win.bottom as usize,
    );
    let (w, h) = (pic.width, pic.height);
    // Conformance offsets are in chroma units for 4:2:0: luma is twice as large.
    let planes = [
        (
            &pic.y,
            w,
            h,
            2 * l,
            w.saturating_sub(2 * r),
            2 * t,
            h.saturating_sub(2 * b),
        ),
        (
            &pic.cb,
            w / 2,
            h / 2,
            l,
            (w / 2).saturating_sub(r),
            t,
            (h / 2).saturating_sub(b),
        ),
        (
            &pic.cr,
            w / 2,
            h / 2,
            l,
            (w / 2).saturating_sub(r),
            t,
            (h / 2).saturating_sub(b),
        ),
    ];
    let mut out = Vec::new();
    for (samples, pw, ph, x0, x1, y0, y1) in planes {
        for y in y0..y1.min(ph) {
            for x in x0..x1.min(pw) {
                let s = samples.get(y * pw + x).copied().unwrap_or(0);
                if bps == 1 {
                    out.push(s as u8);
                } else {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
    out
}

/// Where byte `offset` of a cropped frame lies: plane name, x and y in that plane.
fn locate(offset: usize, bps: usize, lw: usize, lh: usize) -> (&'static str, usize, usize) {
    let s = offset / bps;
    let (cw, ch) = (lw / 2, lh / 2);
    if s < lw * lh {
        ("Y", s % lw, s / lw)
    } else if s < lw * lh + cw * ch {
        let c = s - lw * lh;
        ("Cb", c % cw, c / cw)
    } else {
        let c = s - lw * lh - cw * ch;
        ("Cr", c % cw, c / cw)
    }
}

/// Running comparison of decoded frames against the oracle's rawvideo stream.
#[derive(Default)]
struct Compare {
    compared: usize,
    /// First mismatch: "frame N plane P x= y= (poc)".
    first_diff: Option<String>,
    differing_frames: usize,
    oracle_short: bool,
}

/// The oracle's stream and the decoder's frames, compared frame by frame.
struct Sweep {
    out: std::process::ChildStdout,
    child: std::process::Child,
    open: bool,
    cmp: Compare,
    index: usize,
    frame_len: usize,
    bps: usize,
}

impl Sweep {
    fn take(&mut self, pic: &DecodedPicture, win: &ConformanceWindow, poc: i32) {
        let want = cropped(pic, win, self.bps);
        let index = self.index;
        self.index += 1;
        if !self.open {
            return;
        }
        let mut oracle = vec![0u8; want.len()];
        if self.out.read_exact(&mut oracle).is_err() {
            self.open = false;
            self.cmp.oracle_short = true;
            return;
        }
        self.frame_len = want.len();
        self.cmp.compared += 1;
        if want != oracle {
            self.cmp.differing_frames += 1;
            if self.cmp.first_diff.is_none() {
                let lw = pic.width - 2 * (win.left as usize + win.right as usize);
                let lh = pic.height - 2 * (win.top as usize + win.bottom as usize);
                let at = want
                    .iter()
                    .zip(&oracle)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                let (plane, x, y) = locate(at, self.bps, lw, lh);
                self.cmp.first_diff = Some(format!(
                    "frame {index} plane {plane} x={x} y={y} (poc {poc})"
                ));
            }
        }
    }

    /// End of the decoder's output: any frame still in the oracle is missing here.
    fn finish(mut self) -> Compare {
        if self.open {
            let mut probe = vec![0u8; self.frame_len.max(1)];
            if self.frame_len > 0 && self.out.read_exact(&mut probe).is_ok() {
                self.cmp.first_diff.get_or_insert(format!(
                    "decoder produced {} frames, ffmpeg produced more",
                    self.index
                ));
                self.cmp.oracle_short = false;
                self.cmp.differing_frames += 1;
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.cmp
    }
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().chars().take(38).collect())
        .unwrap_or_default()
}

/// Decodes one file and compares it with ffmpeg. `Err` is a refusal or an error,
/// with the text the table prints.
fn sweep_file(entry: &Entry, es: &Path, bit_depth: u32) -> Result<Compare, String> {
    let bps = if bit_depth > 8 { 2 } else { 1 };
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "hevc", "-i"])
        .arg(es)
        .args([
            "-frames:v",
            &FRAMES.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
        ])
        .arg(&entry.pix_fmt)
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let out = child.stdout.take().expect("piped stdout");
    let mut sweep = Sweep {
        out,
        child,
        open: true,
        cmp: Compare::default(),
        index: 0,
        frame_len: 0,
        bps,
    };

    let data = std::fs::read(es).map_err(|e| e.to_string())?;
    let mut dec = Decoder::new();
    let drain = |dec: &mut Decoder, sweep: &mut Sweep| {
        while let Some(frame) = dec.next_frame() {
            sweep.take(&frame.picture, &frame.window, frame.poc);
        }
    };
    let result = (|| -> Result<(), String> {
        for nal in split_annex_b(&data) {
            let mut bytes = nal.header.to_bytes().to_vec();
            bytes.extend_from_slice(nal.payload);
            if let Err(e) = dec.push_nal(&bytes) {
                return Err(format!("{e}"));
            }
            drain(&mut dec, &mut sweep);
        }
        dec.flush().map_err(|e| format!("{e}"))?;
        drain(&mut dec, &mut sweep);
        Ok(())
    })();
    let cmp = sweep.finish();
    result?;
    Ok(cmp)
}

/// Extracts the first `FRAMES` pictures of the video as Annex B.
fn extract(src: &Path, dst: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
            "-frames:v",
            &FRAMES.to_string(),
            "-f",
            "hevc",
        ])
        .arg(dst)
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
#[ignore = "sweeps the PERSONAL media library (fixtures/real-library-manifest.tsv, hevc rows) through ffmpeg; minutes of wall time. Local only: run with --ignored."]
fn real_library_hevc_streams_match_ffmpeg() {
    assert!(have_ffmpeg(), "ffmpeg is required to sweep the library");
    let entries = manifest_hevc_entries();
    assert!(!entries.is_empty(), "the manifest lists no hevc rows");
    let dir = scratch("real-library-hevc");
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
        let es = dir.join("stream.265");
        if !extract(&entry.path, &es) {
            failures.push(format!("{name}: extraction failed"));
            table.push_str(&format!(
                "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} ERROR extraction\n",
                entry.container, "-"
            ));
            continue;
        }
        match sweep_file(entry, &es, bit_depth) {
            Ok(c) if c.differing_frames > 0 => {
                failures.push(format!("{name}: {} frames differ", c.differing_frames));
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} MISMATCH {} of {} frames; first: {}\n",
                    entry.container, c.compared, c.differing_frames, c.compared,
                    c.first_diff.clone().unwrap_or_default()
                ));
            }
            Ok(c) => {
                decoded += 1;
                table.push_str(&format!(
                    "{name:<38} {:<12} {res:>10} {bit_depth:>2}-bit {:>4} bit-exact{}\n",
                    entry.container,
                    c.compared,
                    if c.oracle_short {
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
        "real library hevc: {decoded} bit-exact, {refused} refused by name, {} failed",
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(decoded >= 1, "no real HEVC file was swept end to end");
}
