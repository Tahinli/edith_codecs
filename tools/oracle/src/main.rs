//! `oracle` — ffmpeg-backed comparison harness for the edith_codecs family.
//!
//! ffmpeg/ffprobe are ORACLE tooling: they are driven here through `std::process`
//! pipes so no `ec-*` crate ever gains a media dependency, sync or async.
//!
//! Subcommands:
//!   `bit-exact <a> <b>`                        byte-identical check
//!   `audio-compare <ours.raw> <ref-file>`      per-channel correlation + RMS delta
//!   `video-compare <ours.raw> <ref-file>`      per-frame PSNR, min/mean
//!
//! `<ours.raw>` is our decoder's raw output: interleaved f32 LE for audio, raw
//! planar/packed frames in `--pix-fmt` order for video. The reference is any file
//! ffmpeg can decode; it is decoded to the same raw layout and compared.
//!
//! stdout is TSV (every line `type<TAB>...`), stderr is the human summary.
//! Exit code: 0 = PASS, 1 = FAIL, 2 = usage/IO error.

use std::collections::HashMap;
use std::process::{Command, Stdio};

const USAGE: &str = "\
oracle — comparison harness (ffmpeg is test tooling only)

  oracle bit-exact <a> <b>
  oracle audio-compare <ours.f32le.raw> <ref-file> [--channels N] [--min-corr F] [--max-rms F]
  oracle video-compare <ours.raw> <ref-file> [--pix-fmt FMT] [--size WxH] [--min-psnr DB]

Defaults: --min-corr 0.999  --max-rms 1e-3  --min-psnr 40
Missing --channels/--pix-fmt/--size are probed from <ref-file> with ffprobe.
";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let rc = match argv.first().map(String::as_str) {
        Some("bit-exact") => run(cmd_bit_exact(&argv[1..])),
        Some("audio-compare") => run(cmd_audio_compare(&argv[1..])),
        Some("video-compare") => run(cmd_video_compare(&argv[1..])),
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            0
        }
        _ => {
            eprint!("{USAGE}");
            2
        }
    };
    std::process::exit(rc);
}

fn run(r: Result<bool, String>) -> i32 {
    match r {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("oracle: {e}");
            2
        }
    }
}

// ------------------------------------------------------------------ args ---

struct Args {
    pos: Vec<String>,
    flags: HashMap<String, String>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Self, String> {
        let mut pos = Vec::new();
        let mut flags = HashMap::new();
        let mut it = argv.iter();
        while let Some(a) = it.next() {
            if let Some(key) = a.strip_prefix("--") {
                let v = it
                    .next()
                    .ok_or_else(|| format!("flag --{key} needs a value"))?;
                flags.insert(key.to_string(), v.clone());
            } else {
                pos.push(a.clone());
            }
        }
        Ok(Self { pos, flags })
    }

    fn num<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.flags.get(key) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("bad --{key} value: {v}")),
        }
    }
}

// --------------------------------------------------------------- ffmpeg ----

/// Decode `input` with ffmpeg and return the raw bytes it writes to stdout.
/// stderr is inherited so a large error log can never deadlock the pipe.
fn ffmpeg_raw(input: &str, extra: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i", input])
        .args(extra)
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("spawning ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg failed on {input} ({})", out.status));
    }
    if out.stdout.is_empty() {
        return Err(format!("ffmpeg produced no data for {input}"));
    }
    Ok(out.stdout)
}

/// `ffprobe -show_entries stream=<entries>` as a key -> value map (first stream wins).
fn ffprobe(input: &str, select: &str, entries: &str) -> Result<HashMap<String, String>, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", select, "-show_entries"])
        .arg(format!("stream={entries}"))
        .args(["-of", "default=nw=1"])
        .arg(input)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("spawning ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe failed on {input} ({})", out.status));
    }
    let mut map = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((k, v)) = line.split_once('=')
            && v != "N/A"
        {
            map.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
    }
    Ok(map)
}

fn read_file(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))
}

// ------------------------------------------------------------ bit-exact ----

fn cmd_bit_exact(argv: &[String]) -> Result<bool, String> {
    let a = Args::parse(argv)?;
    if a.pos.len() != 2 {
        return Err("bit-exact needs exactly two files".into());
    }
    let (x, y) = (read_file(&a.pos[0])?, read_file(&a.pos[1])?);
    let first_diff = x.iter().zip(&y).position(|(p, q)| p != q);
    let pass = x.len() == y.len() && first_diff.is_none();

    println!("meta\ta\t{}", a.pos[0]);
    println!("meta\tb\t{}", a.pos[1]);
    println!("meta\tlen_a\t{}", x.len());
    println!("meta\tlen_b\t{}", y.len());
    match first_diff {
        Some(off) => println!("diff\tfirst_offset\t{off}"),
        None => println!("diff\tfirst_offset\t-"),
    }
    println!("verdict\t{}", if pass { "PASS" } else { "FAIL" });
    eprintln!(
        "bit-exact: {} ({} vs {} bytes, first diff {})",
        if pass { "PASS" } else { "FAIL" },
        x.len(),
        y.len(),
        first_diff.map_or("-".into(), |o| o.to_string())
    );
    Ok(pass)
}

// -------------------------------------------------------------- audio ------

fn deinterleave_f32(buf: &[u8], channels: usize) -> Vec<Vec<f64>> {
    let frames = buf.len() / 4 / channels;
    let mut out = vec![Vec::with_capacity(frames); channels];
    for f in 0..frames {
        for (c, ch) in out.iter_mut().enumerate() {
            let i = (f * channels + c) * 4;
            ch.push(f32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as f64);
        }
    }
    out
}

/// Pearson correlation. Two constant signals correlate perfectly iff equal.
fn corr(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (ma, mb) = (
        a[..n].iter().sum::<f64>() / n as f64,
        b[..n].iter().sum::<f64>() / n as f64,
    );
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return if da == db && (ma - mb).abs() < 1e-12 {
            1.0
        } else {
            0.0
        };
    }
    num / (da * db).sqrt()
}

fn rms_delta(a: &[f64], b: &[f64]) -> (f64, f64) {
    let n = a.len().min(b.len());
    if n == 0 {
        return (f64::INFINITY, f64::INFINITY);
    }
    let mut sum = 0.0;
    let mut peak: f64 = 0.0;
    for i in 0..n {
        let d = a[i] - b[i];
        sum += d * d;
        peak = peak.max(d.abs());
    }
    ((sum / n as f64).sqrt(), peak)
}

fn cmd_audio_compare(argv: &[String]) -> Result<bool, String> {
    let a = Args::parse(argv)?;
    if a.pos.len() != 2 {
        return Err("audio-compare needs <ours.raw> <ref-file>".into());
    }
    let (ours_path, ref_path) = (&a.pos[0], &a.pos[1]);
    let min_corr: f64 = a.num("min-corr", 0.999)?;
    let max_rms: f64 = a.num("max-rms", 1e-3)?;

    let probe = ffprobe(ref_path, "a:0", "channels,sample_rate")?;
    let channels: usize = match a.flags.get("channels") {
        Some(v) => v.parse().map_err(|_| "bad --channels".to_string())?,
        None => probe
            .get("channels")
            .ok_or("could not probe channel count; pass --channels")?
            .parse()
            .map_err(|_| "bad probed channel count".to_string())?,
    };
    if channels == 0 {
        return Err("channel count is zero".into());
    }
    let rate = probe
        .get("sample_rate")
        .cloned()
        .unwrap_or_else(|| "-".into());

    let ours = read_file(ours_path)?;
    let reference = ffmpeg_raw(
        ref_path,
        &["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le"],
    )?;
    let (na, nb) = (ours.len() / 4 / channels, reference.len() / 4 / channels);
    let (ca, cb) = (
        deinterleave_f32(&ours, channels),
        deinterleave_f32(&reference, channels),
    );

    println!("meta\tours\t{ours_path}");
    println!("meta\tref\t{ref_path}");
    println!("meta\tchannels\t{channels}");
    println!("meta\tsample_rate\t{rate}");
    println!("meta\tframes_ours\t{na}");
    println!("meta\tframes_ref\t{nb}");

    // A length gap beyond one percent is a decoder bug (dropped/duplicated
    // packets), not a comparison detail — it fails on its own.
    let len_ok = na.abs_diff(nb) as f64 <= 0.01 * na.max(nb) as f64;
    let mut min_c = f64::INFINITY;
    let mut max_r: f64 = 0.0;
    for c in 0..channels {
        let k = corr(&ca[c], &cb[c]);
        let (r, p) = rms_delta(&ca[c], &cb[c]);
        min_c = min_c.min(k);
        max_r = max_r.max(r);
        println!("ch\t{c}\tcorr\t{k:.6}\trms\t{r:.8}\tpeak\t{p:.8}");
    }
    let pass = len_ok && min_c >= min_corr && max_r <= max_rms;
    println!(
        "verdict\t{}\tmin_corr={min_c:.6}\tmax_rms={max_r:.8}\tlen_ok={len_ok}",
        if pass { "PASS" } else { "FAIL" }
    );
    eprintln!(
        "audio-compare: {} — {channels}ch @{rate}, min corr {min_c:.6} (>= {min_corr}), \
         max RMS delta {max_r:.8} (<= {max_rms}), lengths {na}/{nb}",
        if pass { "PASS" } else { "FAIL" }
    );
    Ok(pass)
}

// -------------------------------------------------------------- video ------

/// Raw-frame geometry for a pixel format: total samples, luma samples (0 when the
/// format is packed and has no contiguous luma plane), bytes per sample, max value.
struct PixGeom {
    samples: usize,
    luma: usize,
    bps: usize,
    max: f64,
}

fn pix_geom(pix_fmt: &str, w: usize, h: usize) -> Result<PixGeom, String> {
    let wh = w * h;
    let g = |samples: usize, luma: usize, bps: usize, max: f64| PixGeom {
        samples,
        luma,
        bps,
        max,
    };
    // Semi-planar formats carry their depth inside the name, so match them first.
    match pix_fmt {
        "nv12" | "nv21" => return Ok(g(wh * 3 / 2, wh, 1, 255.0)),
        "p010le" | "p010be" => return Ok(g(wh * 3 / 2, wh, 2, 1023.0)),
        "p016le" | "p016be" => return Ok(g(wh * 3 / 2, wh, 2, 65535.0)),
        _ => {}
    }
    let mut base = pix_fmt;
    let mut bps = 1;
    let mut max = 255.0;
    for (suffix, depth) in [("9", 9u32), ("10", 10), ("12", 12), ("14", 14), ("16", 16)] {
        for endian in ["le", "be"] {
            let s = format!("{suffix}{endian}");
            if let Some(b) = pix_fmt.strip_suffix(&s) {
                base = b;
                bps = 2;
                max = ((1u32 << depth) - 1) as f64;
            }
        }
    }
    let geom = match base {
        "yuv420p" | "yuvj420p" => g(wh * 3 / 2, wh, bps, max),
        "yuv422p" | "yuvj422p" => g(wh * 2, wh, bps, max),
        "yuv444p" | "yuvj444p" | "gbrp" => g(wh * 3, wh, bps, max),
        "gray" => g(wh, wh, bps, max),
        // Packed: no contiguous luma plane, so luma PSNR falls back to all-plane.
        "yuyv422" | "uyvy422" => g(wh * 2, 0, bps, max),
        "rgb24" | "bgr24" => g(wh * 3, 0, bps, max),
        "rgba" | "bgra" | "argb" | "abgr" => g(wh * 4, 0, bps, max),
        _ => return Err(format!("unsupported pix_fmt for raw compare: {pix_fmt}")),
    };
    Ok(geom)
}

fn sample(buf: &[u8], i: usize, bps: usize) -> f64 {
    if bps == 1 {
        buf[i] as f64
    } else {
        u16::from_le_bytes([buf[2 * i], buf[2 * i + 1]]) as f64
    }
}

/// PSNR over `n` samples starting at sample index `off` in both buffers.
fn psnr(a: &[u8], b: &[u8], off: usize, n: usize, bps: usize, max: f64) -> f64 {
    let mut sum = 0.0;
    for i in off..off + n {
        let d = sample(a, i, bps) - sample(b, i, bps);
        sum += d * d;
    }
    let mse = sum / n as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (max * max / mse).log10()
    }
}

fn cmd_video_compare(argv: &[String]) -> Result<bool, String> {
    let a = Args::parse(argv)?;
    if a.pos.len() != 2 {
        return Err("video-compare needs <ours.raw> <ref-file>".into());
    }
    let (ours_path, ref_path) = (&a.pos[0], &a.pos[1]);
    let min_psnr: f64 = a.num("min-psnr", 40.0)?;

    let probe = ffprobe(ref_path, "v:0", "width,height,pix_fmt")?;
    let pix = match a.flags.get("pix-fmt") {
        Some(v) => v.clone(),
        None => probe
            .get("pix_fmt")
            .cloned()
            .ok_or("could not probe pix_fmt; pass --pix-fmt")?,
    };
    let (w, h) = match a.flags.get("size") {
        Some(s) => {
            let (ws, hs) = s.split_once('x').ok_or("--size must look like 1920x1080")?;
            (
                ws.parse::<usize>().map_err(|_| "bad --size width")?,
                hs.parse::<usize>().map_err(|_| "bad --size height")?,
            )
        }
        None => (
            probe
                .get("width")
                .ok_or("could not probe width; pass --size")?
                .parse()
                .map_err(|_| "bad probed width".to_string())?,
            probe
                .get("height")
                .ok_or("could not probe height; pass --size")?
                .parse()
                .map_err(|_| "bad probed height".to_string())?,
        ),
    };

    let geom = pix_geom(&pix, w, h)?;
    let frame_bytes = geom.samples * geom.bps;
    if frame_bytes == 0 {
        return Err("frame size computed as zero".into());
    }

    let ours = read_file(ours_path)?;
    let reference = ffmpeg_raw(
        ref_path,
        &["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", &pix],
    )?;
    let (fa, fb) = (ours.len() / frame_bytes, reference.len() / frame_bytes);
    let frames = fa.min(fb);

    println!("meta\tours\t{ours_path}");
    println!("meta\tref\t{ref_path}");
    println!("meta\tpix_fmt\t{pix}");
    println!("meta\tdims\t{w}x{h}");
    println!("meta\tframe_bytes\t{frame_bytes}");
    println!("meta\tframes_ours\t{fa}");
    println!("meta\tframes_ref\t{fb}");
    if frames == 0 {
        println!("verdict\tFAIL\tno complete frame in common");
        eprintln!("video-compare: FAIL — no complete frame in common ({fa} vs {fb})");
        return Ok(false);
    }

    let luma_n = if geom.luma == 0 {
        geom.samples
    } else {
        geom.luma
    };
    let mut min_y = f64::INFINITY;
    let mut sum_y = 0.0;
    let mut min_all = f64::INFINITY;
    let mut sum_all = 0.0;
    let mut finite_y = 0usize;
    let mut finite_all = 0usize;
    for f in 0..frames {
        let off = f * geom.samples;
        let py = psnr(&ours, &reference, off, luma_n, geom.bps, geom.max);
        let pa = psnr(&ours, &reference, off, geom.samples, geom.bps, geom.max);
        min_y = min_y.min(py);
        min_all = min_all.min(pa);
        if py.is_finite() {
            sum_y += py;
            finite_y += 1;
        }
        if pa.is_finite() {
            sum_all += pa;
            finite_all += 1;
        }
        println!("frame\t{f}\tpsnr_y\t{py:.4}\tpsnr_all\t{pa:.4}");
    }
    // Mean over finite frames only: identical frames give +inf and would otherwise
    // poison the average of a run that also contains real differences.
    let mean_y = if finite_y == 0 {
        f64::INFINITY
    } else {
        sum_y / finite_y as f64
    };
    let mean_all = if finite_all == 0 {
        f64::INFINITY
    } else {
        sum_all / finite_all as f64
    };
    let len_ok = fa == fb;
    let pass = len_ok && min_y >= min_psnr;
    println!("stats\tpsnr_y\tmin\t{min_y:.4}\tmean\t{mean_y:.4}");
    println!("stats\tpsnr_all\tmin\t{min_all:.4}\tmean\t{mean_all:.4}");
    println!(
        "verdict\t{}\tmin_psnr_y={min_y:.4}\tframes={frames}\tlen_ok={len_ok}",
        if pass { "PASS" } else { "FAIL" }
    );
    eprintln!(
        "video-compare: {} — {frames} frames {w}x{h} {pix}, min luma PSNR {min_y:.4} dB \
         (>= {min_psnr}), mean {mean_y:.4} dB, frame counts {fa}/{fb}",
        if pass { "PASS" } else { "FAIL" }
    );
    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corr_identical_and_inverted() {
        let a = [0.0, 1.0, -1.0, 0.5, -0.25];
        let inv: Vec<f64> = a.iter().map(|x| -x).collect();
        assert!((corr(&a, &a) - 1.0).abs() < 1e-12);
        assert!((corr(&a, &inv) + 1.0).abs() < 1e-12);
        // Silence vs silence correlates; silence vs DC offset does not.
        assert_eq!(corr(&[0.0; 4], &[0.0; 4]), 1.0);
        assert_eq!(corr(&[0.0; 4], &[1.0; 4]), 0.0);
    }

    #[test]
    fn rms_and_peak_delta() {
        let (r, p) = rms_delta(&[0.0, 0.0, 0.0], &[0.0, 0.0, 0.0]);
        assert_eq!((r, p), (0.0, 0.0));
        let (r, p) = rms_delta(&[1.0, -1.0], &[0.0, 0.0]);
        assert!((r - 1.0).abs() < 1e-12 && (p - 1.0).abs() < 1e-12);
    }

    #[test]
    fn psnr_identical_is_infinite_and_differing_is_finite() {
        let a = vec![10u8, 20, 30, 40];
        assert!(psnr(&a, &a, 0, 4, 1, 255.0).is_infinite());
        let b = vec![11u8, 21, 31, 41];
        let p = psnr(&a, &b, 0, 4, 1, 255.0);
        // MSE 1 over 8-bit range: 10*log10(255^2) = 48.13 dB.
        assert!((p - 48.1308).abs() < 1e-3, "psnr={p}");
        // 10-bit path reads little-endian u16 pairs.
        let x = [0u8, 1, 0, 1];
        let y = [1u8, 1, 0, 1];
        assert!(psnr(&x, &y, 0, 2, 2, 1023.0).is_finite());
    }

    #[test]
    fn pix_geom_covers_the_generated_fixture_formats() {
        let (w, h) = (1920, 1080);
        let g = pix_geom("yuv420p", w, h).unwrap();
        assert_eq!(
            (g.samples, g.luma, g.bps, g.max),
            (w * h * 3 / 2, w * h, 1, 255.0)
        );
        let g = pix_geom("yuv420p10le", w, h).unwrap();
        assert_eq!(
            (g.samples, g.luma, g.bps, g.max),
            (w * h * 3 / 2, w * h, 2, 1023.0)
        );
        let g = pix_geom("p010le", w, h).unwrap();
        assert_eq!((g.samples, g.bps, g.max), (w * h * 3 / 2, 2, 1023.0));
        assert_eq!(pix_geom("yuv444p", w, h).unwrap().samples, w * h * 3);
        assert_eq!(pix_geom("yuyv422", w, h).unwrap().luma, 0);
        assert!(pix_geom("no_such_fmt", w, h).is_err());
    }

    #[test]
    fn deinterleave_splits_channels() {
        let mut buf = Vec::new();
        for f in 0..3u32 {
            for c in 0..2u32 {
                buf.extend_from_slice(&((f * 10 + c) as f32).to_le_bytes());
            }
        }
        let ch = deinterleave_f32(&buf, 2);
        assert_eq!(ch[0], vec![0.0, 10.0, 20.0]);
        assert_eq!(ch[1], vec![1.0, 11.0, 21.0]);
    }

    #[test]
    fn args_parse_flags_and_positionals() {
        let argv: Vec<String> = ["a.raw", "b.mp4", "--min-corr", "0.5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = Args::parse(&argv).unwrap();
        assert_eq!(a.pos, ["a.raw", "b.mp4"]);
        assert_eq!(a.num::<f64>("min-corr", 0.999).unwrap(), 0.5);
        assert_eq!(a.num::<f64>("max-rms", 1e-3).unwrap(), 1e-3);
        assert!(Args::parse(&["--dangling".to_string()]).is_err());
    }
}
