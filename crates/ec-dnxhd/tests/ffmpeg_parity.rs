#![allow(clippy::too_many_arguments, clippy::needless_range_loop, clippy::len_zero)]
//! Byte-exact comparison with ffmpeg's dnxhd decoder on streams ffmpeg's own
//! encoder produces from synthetic sources (and on header-patched variants for
//! the corners the encoder cannot reach: 12-bit samples, MBAFF).

mod common;
use common::*;
use ec_dnxhd::{Chroma, DnxhdDecoder, Picture};
use ec_core::registry::{CodecId, CodecParameters};

struct Case {
    tag: &'static str,
    size: &'static str,
    rate: &'static str,
    src: &'static str,
    pix_in: &'static str,
    enc: &'static [&'static str],
    frames: u32,
    /// Expected compression id.
    cid: u32,
    /// Expected interlace.
    interlaced: bool,
}

const fn c(
    tag: &'static str,
    size: &'static str,
    rate: &'static str,
    src: &'static str,
    pix_in: &'static str,
    enc: &'static [&'static str],
    frames: u32,
    cid: u32,
    interlaced: bool,
) -> Case {
    Case { tag, size, rate, src, pix_in, enc, frames, cid, interlaced }
}

const T: &str = "testsrc2";
const N: &str = "testsrc2,noise=alls=30:allf=t";
const M: &str = "mandelbrot";

fn ff_pix(pic: &Picture) -> &'static str {
    match (pic.info.chroma, pic.info.bit_depth) {
        (Chroma::C422, 8) => "yuv422p",
        (Chroma::C422, 10) => "yuv422p10le",
        (Chroma::C422, _) => "yuv422p12le",
        (Chroma::C444, 10) => "yuv444p10le",
        (Chroma::C444, _) => "yuv444p12le",
    }
}

fn native_bytes(p: &Picture) -> Vec<u8> {
    let mut v = Vec::new();
    for pl in &p.planes {
        for &s in pl {
            if p.info.bit_depth == 8 {
                v.push(s as u8);
            } else {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
    v
}

fn encode(c: &Case, dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = dir.join(format!("{}.mov", c.tag));
    let (base, chain) = match c.src.split_once(',') {
        Some((b, r)) => (b, format!(",{r}")),
        None => (c.src, String::new()),
    };
    let lavfi = format!("{base}=size={}:rate={}{chain},format={}", c.size, c.rate, c.pix_in);
    let n = c.frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &lavfi, "-frames:v", &n, "-c:v", "dnxhd"];
    args.extend_from_slice(c.enc);
    args.push(out.to_str().unwrap());
    ffmpeg(&args).then_some(out)
}

/// ffmpeg's rawvideo decode of `raw` with the given `-cpuflags`.
fn ffmpeg_decode(dir: &std::path::Path, raw: &std::path::Path, pix: &str, cpu: &str) -> Result<Vec<u8>, String> {
    let refp = dir.join(format!("ref-{cpu}.yuv"));
    if !ffmpeg(&[
        "-cpuflags", cpu, "-f", "dnxhd", "-i", raw.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", pix,
        "-fps_mode", "passthrough", refp.to_str().unwrap(),
    ]) {
        return Err("ffmpeg decode failed".into());
    }
    std::fs::read(&refp).map_err(|e| e.to_string())
}

/// (max abs error, PSNR dB over `peak`) between two sample streams of `bytes_per` width.
fn diff_stats(a: &[u8], b: &[u8], bytes_per: usize, peak: f64) -> (u32, f64) {
    let (mut max, mut sse, mut n) = (0u32, 0f64, 0f64);
    for (x, y) in a.chunks(bytes_per).zip(b.chunks(bytes_per)) {
        let (x, y) = if bytes_per == 1 {
            (u32::from(x[0]), u32::from(y[0]))
        } else {
            (u32::from(u16::from_le_bytes([x[0], x[1]])), u32::from(u16::from_le_bytes([y[0], y[1]])))
        };
        let d = x.abs_diff(y);
        max = max.max(d);
        sse += f64::from(d * d);
        n += 1.0;
    }
    let psnr = if sse == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak * n / sse).log10() };
    (max, psnr)
}

/// Decode `frames` with ffmpeg (raw dnxhd stream) and with us. Against
/// ffmpeg's C code (`-cpuflags 0`) the bytes must be identical. Against its
/// default x86 SIMD IDCT they must be identical too, unless that IDCT
/// differs from its own C version on overflowing coefficients (noise-heavy
/// 10-bit content); then we ask for max abs error <= 1... measured in `note`.
fn compare(tag: &str, frames: &[Vec<u8>], threads: usize) -> Result<Picture, String> {
    let dir = scratch(&format!("cmp-{tag}"));
    let raw = dir.join("in.dnxhd");
    std::fs::write(&raw, frames.concat()).map_err(|e| e.to_string())?;
    let mut dec = DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).unwrap();
    dec.set_threads(threads);
    let mut ours = Vec::new();
    let mut last = None;
    for f in frames {
        let pic = dec.decode_native(f).map_err(|e| format!("{tag}: decode: {e}"))?;
        ours.extend_from_slice(&native_bytes(&pic));
        last = Some(pic);
    }
    let last = last.ok_or("no frames")?;
    let pix = ff_pix(&last);
    let reference = ffmpeg_decode(&dir, &raw, pix, "0").map_err(|e| format!("{tag}: {e}"))?;
    if reference.len() != ours.len() {
        return Err(format!("{tag}: size {} != ffmpeg {}", ours.len(), reference.len()));
    }
    if let Some(i) = reference.iter().zip(&ours).position(|(a, b)| a != b) {
        let bad = reference.iter().zip(&ours).filter(|(a, b)| a != b).count();
        return Err(format!("{tag}: {bad} bytes differ from ffmpeg's C decoder, first at {i}"));
    }
    // The default build of ffmpeg on this machine (x86 SIMD IDCT). Its 10-bit
    // SIMD IDCT is not bit-identical to its own C code on coefficient sets
    // that overflow 16 bits (saturated noise at very high bit rates), so a
    // handful of samples there differ by a whole code range; we follow the C
    // code. Everywhere else the two agree, and the difference stays below
    // 0.1% of the samples.
    let simd = ffmpeg_decode(&dir, &raw, pix, "all").map_err(|e| format!("{tag}: {e}"))?;
    let bytes_per = if last.info.bit_depth == 8 { 1 } else { 2 };
    if simd != ours {
        let (max, psnr) = diff_stats(&simd, &ours, bytes_per, f64::from((1u32 << last.info.bit_depth) - 1));
        let bad = simd.chunks(bytes_per).zip(ours.chunks(bytes_per)).filter(|(a, b)| a != b).count();
        let frac = bad as f64 / (ours.len() / bytes_per) as f64;
        eprintln!("note {tag}: vs ffmpeg SIMD {bad} samples differ ({frac:.5}), max abs error {max}, PSNR {psnr:.1} dB");
        // 12-bit: ffmpeg's SIMD IDCT rounds differently from its C code by one
        // code value (>= 60 dB PSNR, max abs error 1).
        if !((max <= 1 && psnr >= 60.0) || frac <= 0.001) {
            return Err(format!("{tag}: vs ffmpeg SIMD {bad} samples differ"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(last)
}

fn run_cases(tag: &str, cases: &[Case]) {
    if !can_run() {
        return;
    }
    let dir = scratch(tag);
    let mut failures = Vec::new();
    for case in cases {
        let Some(path) = encode(case, &dir) else {
            failures.push(format!("{}: ffmpeg encode failed", case.tag));
            continue;
        };
        let frames = mov_frames(&path);
        if frames.len() != case.frames as usize {
            failures.push(format!("{}: {} frames", case.tag, frames.len()));
            continue;
        }
        match compare(case.tag, &frames, 3) {
            Ok(pic) => {
                if pic.info.cid != case.cid || pic.info.interlaced != case.interlaced {
                    failures.push(format!("{}: cid {} interlaced {}", case.tag, pic.info.cid, pic.info.interlaced));
                }
            }
            Err(e) => failures.push(e),
        }
        // Single-threaded decode must agree too.
        if let Err(e) = compare(&format!("{}-t1", case.tag), &frames, 1) {
            failures.push(e);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_dnxhd_cid_ffmpeg_can_encode() {
    run_cases("cids", &[
        c("1235", "1920x1080", "24000/1001", T, "yuv422p10le", &["-b:v", "175M"], 3, 1235, false),
        c("1235_50p", "1920x1080", "50", N, "yuv422p10le", &["-b:v", "365M"], 3, 1235, false),
        c("1235_60p", "1920x1080", "60000/1001", M, "yuv422p10le", &["-b:v", "440M"], 2, 1235, false),
        c("1237", "1920x1080", "30000/1001", T, "yuv422p", &["-b:v", "145M"], 3, 1237, false),
        c("1237_noise", "1920x1080", "25", N, "yuv422p", &["-b:v", "120M"], 2, 1237, false),
        c("1238", "1920x1080", "24000/1001", T, "yuv422p", &["-b:v", "175M"], 3, 1238, false),
        c("1238_60p", "1920x1080", "60000/1001", N, "yuv422p", &["-b:v", "440M"], 2, 1238, false),
        c("1241", "1920x1080", "24000/1001", T, "yuv422p10le", &["-b:v", "185M", "-flags", "+ildct"], 3, 1241, true),
        c("1241_30i", "1920x1080", "30000/1001", N, "yuv422p10le", &["-b:v", "220M", "-flags", "+ildct"], 2, 1241, true),
        c("1242", "1920x1080", "24000/1001", T, "yuv422p", &["-b:v", "120M", "-flags", "+ildct"], 3, 1242, true),
        c("1243", "1920x1080", "24000/1001", N, "yuv422p", &["-b:v", "185M", "-flags", "+ildct"], 3, 1243, true),
        c("1244", "1440x1080", "24000/1001", T, "yuv422p", &["-b:v", "120M", "-flags", "+ildct"], 3, 1244, true),
        c("1250", "1280x720", "24000/1001", T, "yuv422p10le", &["-b:v", "90M"], 3, 1250, false),
        c("1250_60p", "1280x720", "60000/1001", N, "yuv422p10le", &["-b:v", "220M"], 3, 1250, false),
        c("1251", "1280x720", "24000/1001", T, "yuv422p", &["-b:v", "90M"], 3, 1251, false),
        c("1251_60p", "1280x720", "60000/1001", M, "yuv422p", &["-b:v", "220M"], 3, 1251, false),
        c("1252", "1280x720", "24000/1001", T, "yuv422p", &["-b:v", "60M"], 3, 1252, false),
        c("1253", "1920x1080", "24000/1001", T, "yuv422p", &["-b:v", "36M"], 3, 1253, false),
        c("1258", "960x720", "24000/1001", T, "yuv422p", &["-b:v", "42M"], 3, 1258, false),
        c("1259", "1440x1080", "24000/1001", T, "yuv422p", &["-b:v", "63M"], 3, 1259, false),
    ]);
}

fn profile_args(name: &str) -> &'static [&'static str] {
    match name {
        "lb" | "dnxhr_lb" => &["-profile:v", "dnxhr_lb"],
        "sq" | "dnxhr_sq" => &["-profile:v", "dnxhr_sq"],
        "hq" | "dnxhr_hq" => &["-profile:v", "dnxhr_hq"],
        "hqx" | "dnxhr_hqx" => &["-profile:v", "dnxhr_hqx"],
        _ => &["-profile:v", "dnxhr_444"],
    }
}

#[test]
fn dnxhr_profiles_at_odd_sizes() {
    let p = profile_args;
    let mut cases = Vec::new();
    for (sz, tagsz) in [("1280x544", "1280x544"), ("322x178", "322x178"), ("258x122", "258x122"), ("256x120", "256x120"), ("1000x300", "1000x300")] {
        let _ = tagsz;
        for (prof, cid, pix) in [
            ("lb", 1274, "yuv422p"),
            ("sq", 1273, "yuv422p"),
            ("hq", 1272, "yuv422p"),
            ("hqx", 1271, "yuv422p10le"),
            ("444", 1270, "yuv444p10le"),
        ] {
            let tag: &'static str = Box::leak(format!("hr_{prof}_{sz}").into_boxed_str());
            let src = if prof == "444" || prof == "hq" { N } else { T };
            let size: &'static str = Box::leak(sz.to_string().into_boxed_str());
            cases.push(c(tag, size, "25", src, pix, p(prof), 3, cid, false));
        }
    }
    run_cases("hr_odd", &cases);
}

#[test]
fn dnxhr_2160p_has_a_long_scan_index() {
    run_cases("uhd", &[
        c("uhd_hq", "3840x2160", "25", T, "yuv422p", &["-profile:v", "dnxhr_hq"], 1, 1272, false),
        c("uhd_444", "3840x2160", "25", N, "yuv444p10le", &["-profile:v", "dnxhr_444"], 1, 1270, false),
    ]);
}

#[test]
fn dnxhr_1080p_all_profiles() {
    run_cases("fhd", &[
        c("fhd_lb", "1920x1080", "25", M, "yuv422p", &["-profile:v", "dnxhr_lb"], 2, 1274, false),
        c("fhd_sq", "1920x1080", "25", M, "yuv422p", &["-profile:v", "dnxhr_sq"], 2, 1273, false),
        c("fhd_hq", "1920x1080", "25", M, "yuv422p", &["-profile:v", "dnxhr_hq"], 2, 1272, false),
        c("fhd_hqx", "1920x1080", "25", M, "yuv422p10le", &["-profile:v", "dnxhr_hqx"], 2, 1271, false),
    ]);
}

/// Patch a header byte in every frame (and every field of an interlaced frame)
/// so the ffmpeg decoder and ours read the same stream the encoder cannot make.
fn patched(frames: &[Vec<u8>], f: impl Fn(&mut [u8])) -> Vec<Vec<u8>> {
    frames
        .iter()
        .map(|fr| {
            let mut fr = fr.clone();
            f(&mut fr);
            fr
        })
        .collect()
}

#[test]
fn twelve_bit_streams_by_patching_the_depth_field() {
    if !can_run() {
        return;
    }
    let dir = scratch("p12");
    let mut failures = Vec::new();
    for (tag, size, pix, prof, src) in [
        ("hqx12", "320x176", "yuv422p10le", "dnxhr_hqx", N),
        ("hqx12_mand", "320x176", "yuv422p10le", "dnxhr_hqx", M),
        ("444_12", "320x176", "yuv444p10le", "dnxhr_444", N),
        ("444_12_t", "322x178", "yuv444p10le", "dnxhr_444", T),
    ] {
        let case = c(tag, size, "25", src, pix, profile_args(prof), 3, 0, false);
        let Some(path) = encode(&case, &dir) else {
            failures.push(format!("{tag}: encode"));
            continue;
        };
        let frames = patched(&mov_frames(&path), |fr| fr[0x21] = (fr[0x21] & 0x1f) | (3 << 5));
        match compare(tag, &frames, 2) {
            Ok(p) => assert_eq!(p.info.bit_depth, 12),
            Err(e) => failures.push(e),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn mbaff_cid_1260_by_patching_the_header() {
    if !can_run() {
        return;
    }
    let dir = scratch("mbaff");
    let mut failures = Vec::new();
    for (tag, src) in [("mbaff_t", T), ("mbaff_n", N)] {
        let case = c(tag, "1440x1080", "25", src, "yuv422p", &["-b:v", "120M", "-flags", "+ildct"], 2, 1244, true);
        let Some(path) = encode(&case, &dir) else {
            failures.push(format!("{tag}: encode"));
            continue;
        };
        let frames = mov_frames(&path);
        // CID 1260 (1440x1080 MBAFF, 417792-byte coding units) from a 1244
        // stream: the macroblock header is 12 bits either way (11-bit qscale,
        // or interlace flag + 10-bit qscale), so the payload stays valid. The
        // first macroblock of every row is flipped to an interlaced one.
        let frames: Vec<Vec<u8>> = frames
            .iter()
            .map(|fr| {
                let unit = fr.len() / 2;
                let mut out = Vec::new();
                for k in 0..2 {
                    let mut u = fr[k * unit..(k + 1) * unit].to_vec();
                    u[0x28..0x2c].copy_from_slice(&1260u32.to_be_bytes());
                    u[6] |= 0x20;
                    let rows = u16::from_be_bytes([u[0x16c], u[0x16d]]) as usize;
                    for r in 0..rows {
                        let off = u32::from_be_bytes(u[0x170 + 4 * r..0x174 + 4 * r].try_into().unwrap()) as usize;
                        u[0x280 + off] |= 0x80;
                    }
                    u.resize(417_792, 0);
                    out.extend_from_slice(&u);
                }
                out
            })
            .collect();
        match compare(tag, &frames, 3) {
            Ok(p) => assert!(p.info.mbaff && p.info.cid == 1260 && p.info.interlaced),
            Err(e) => failures.push(e),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn video_frame_output_formats_and_interlace_flag() {
    if !can_run() {
        return;
    }
    use ec_core::frame::{Interlace, PixelFormat};
    let dir = scratch("vf");
    let mut dec = DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).unwrap();
    for (case, fmt, il) in [
        (c("vf_422", "320x176", "25", T, "yuv422p", &["-profile:v", "dnxhr_hq"], 1, 1272, false), PixelFormat::I422, Interlace::Progressive),
        (c("vf_hqx", "320x176", "25", T, "yuv422p10le", &["-profile:v", "dnxhr_hqx"], 1, 1271, false), PixelFormat::I210, Interlace::Progressive),
        (c("vf_444", "320x176", "25", T, "yuv444p10le", &["-profile:v", "dnxhr_444"], 1, 1270, false), PixelFormat::I410, Interlace::Progressive),
        (c("vf_il", "1920x1080", "25", T, "yuv422p", &["-b:v", "120M", "-flags", "+ildct"], 1, 1242, true), PixelFormat::I422, Interlace::TopFieldFirst),
    ] {
        let path = encode(&case, &dir).unwrap();
        let fr = &mov_frames(&path)[0];
        let f = dec.decode_bytes(fr).unwrap();
        assert_eq!(f.format, fmt, "{}", case.tag);
        assert_eq!(f.interlace, il, "{}", case.tag);
        assert!(!frame_bytes(&f).is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn matroska_carries_dnxhd_too() {
    if !can_run() {
        return;
    }
    let dir = scratch("mkv");
    let mkv = dir.join("a.mkv");
    let mov = dir.join("a.mov");
    let avi = dir.join("a.avi");
    for out in [&mkv, &mov, &avi] {
        assert!(ffmpeg(&[
            "-f", "lavfi", "-i", "testsrc2=size=320x176:rate=25,format=yuv422p", "-frames:v", "2",
            "-c:v", "dnxhd", "-profile:v", "dnxhr_sq", out.to_str().unwrap(),
        ]));
    }
    assert_eq!(mkv_frames(&mkv), mov_frames(&mov));
    assert_eq!(avi_frames(&avi), mov_frames(&mov));
    let _ = std::fs::remove_dir_all(&dir);
}
