#![allow(clippy::needless_range_loop, clippy::field_reassign_with_default, clippy::too_many_arguments, clippy::type_complexity)]
//! Byte-exact comparison with ffmpeg's mpeg2video / mpeg1video decoder on
//! streams ffmpeg's own encoder produces from synthetic sources.

mod common;
use common::*;

struct Case {
    tag: &'static str,
    size: &'static str,
    frames: u32,
    src: &'static str,
    enc: &'static [&'static str],
    pix: &'static str,
}

const CASES: &[Case] = &[
    Case { tag: "prog_i", size: "176x144", frames: 3, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "1", "-qscale:v", "3"], pix: "yuv420p" },
    Case { tag: "prog_ip", size: "176x144", frames: 12, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "0", "-qscale:v", "4"], pix: "yuv420p" },
    Case { tag: "prog_ipb", size: "176x144", frames: 14, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "9", "-bf", "2", "-qscale:v", "4"], pix: "yuv420p" },
    Case { tag: "mpeg1_ipb", size: "176x144", frames: 14, src: "testsrc2", enc: &["-c:v", "mpeg1video", "-g", "9", "-bf", "2", "-qscale:v", "4"], pix: "yuv420p" },
    Case { tag: "mpeg1_noise", size: "176x144", frames: 10, src: "testsrc2,noise=alls=40:allf=t", enc: &["-c:v", "mpeg1video", "-g", "5", "-bf", "2", "-qscale:v", "2"], pix: "yuv420p" },
    Case { tag: "noise_q1", size: "176x144", frames: 8, src: "testsrc2,noise=alls=60:allf=t", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "1"], pix: "yuv420p" },
    Case { tag: "noise_q31", size: "176x144", frames: 8, src: "testsrc2,noise=alls=60:allf=t", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "31"], pix: "yuv420p" },
    Case { tag: "ilace_tff", size: "352x288", frames: 10, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-qscale:v", "4", "-flags", "+ildct+ilme", "-top", "1"], pix: "yuv420p" },
    Case { tag: "ilace_bff", size: "352x288", frames: 10, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-qscale:v", "4", "-flags", "+ildct+ilme", "-top", "0"], pix: "yuv420p" },
    Case { tag: "ilace_ildct_only", size: "352x288", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "4", "-flags", "+ildct", "-top", "1"], pix: "yuv420p" },
    Case { tag: "altscan", size: "352x288", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "4", "-alternate_scan", "1", "-flags", "+ildct+ilme", "-top", "1"], pix: "yuv420p" },
    Case { tag: "intra_vlc", size: "176x144", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "3", "-intra_vlc", "1"], pix: "yuv420p" },
    Case { tag: "nonlinear", size: "176x144", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "6", "-non_linear_quant", "1", "-qmax", "28"], pix: "yuv420p" },
    Case { tag: "dc8", size: "176x144", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "3", "-bf", "1", "-qscale:v", "3", "-intra_dc_precision", "0"], pix: "yuv420p" },
    Case { tag: "dc9", size: "176x144", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "3", "-bf", "1", "-qscale:v", "3", "-intra_dc_precision", "1"], pix: "yuv420p" },
    Case { tag: "dc10", size: "176x144", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "3", "-bf", "1", "-qscale:v", "3", "-intra_dc_precision", "2"], pix: "yuv420p" },
    Case { tag: "dc11", size: "176x144", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "3", "-bf", "1", "-qscale:v", "3", "-intra_dc_precision", "3"], pix: "yuv420p" },
    Case { tag: "all_tools", size: "352x288", frames: 10, src: "testsrc2,noise=alls=20:allf=t", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-qscale:v", "5", "-alternate_scan", "1", "-intra_vlc", "1", "-non_linear_quant", "1", "-qmax", "28", "-intra_dc_precision", "2", "-flags", "+ildct+ilme", "-top", "0"], pix: "yuv420p" },
    Case { tag: "p422", size: "176x144", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "3"], pix: "yuv422p" },
    Case { tag: "p422_ilace", size: "352x288", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-qscale:v", "3", "-flags", "+ildct+ilme", "-top", "1"], pix: "yuv422p" },
    Case { tag: "p422_intra", size: "352x288", frames: 4, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "1", "-qscale:v", "2", "-intra_vlc", "1", "-intra_dc_precision", "3"], pix: "yuv422p" },
    Case { tag: "sd_pal", size: "720x576", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-b:v", "6M", "-flags", "+ildct+ilme", "-top", "1"], pix: "yuv420p" },
    Case { tag: "sd_ntsc", size: "720x480", frames: 8, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-b:v", "6M", "-flags", "+ildct+ilme", "-top", "0"], pix: "yuv420p" },
    Case { tag: "hd_1080i", size: "1920x1080", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "6", "-bf", "2", "-b:v", "20M", "-flags", "+ildct+ilme", "-top", "1"], pix: "yuv420p" },
    Case { tag: "hd_1080p_422", size: "1920x1080", frames: 4, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "4", "-bf", "1", "-b:v", "40M"], pix: "yuv422p" },
    Case { tag: "mandel", size: "352x288", frames: 8, src: "mandelbrot", enc: &["-c:v", "mpeg2video", "-g", "5", "-bf", "2", "-qscale:v", "3", "-flags", "+ildct+ilme"], pix: "yuv420p" },
    Case { tag: "odd_size", size: "200x120", frames: 6, src: "testsrc2", enc: &["-c:v", "mpeg2video", "-g", "3", "-bf", "1", "-qscale:v", "4"], pix: "yuv420p" },
    Case { tag: "mv_big", size: "352x288", frames: 10, src: "testsrc2,scroll=horizontal=0.05:vertical=0.03", enc: &["-c:v", "mpeg2video", "-g", "10", "-bf", "2", "-qscale:v", "4", "-me_range", "64", "-flags", "+ildct+ilme"], pix: "yuv420p" },
    Case { tag: "mpeg1_big", size: "352x288", frames: 10, src: "testsrc2", enc: &["-c:v", "mpeg1video", "-g", "10", "-bf", "2", "-b:v", "1500k"], pix: "yuv420p" },
];

fn generate(c: &Case) -> Option<std::path::PathBuf> {
    let dir = scratch(c.tag);
    let out = dir.join("in.m2v");
    let (base, chain) = match c.src.split_once(',') {
        Some((b, rest)) => (b, format!(",{rest}")),
        None => (c.src, String::new()),
    };
    let lavfi = format!("{base}=size={}:rate=25{chain}", c.size);
    let frames = c.frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &lavfi, "-frames:v", &frames, "-pix_fmt", c.pix];
    args.extend_from_slice(c.enc);
    let fmt = if c.enc.contains(&"mpeg1video") { "mpeg1video" } else { "mpeg2video" };
    args.extend_from_slice(&["-f", fmt, out.to_str().unwrap()]);
    if std::env::var_os("EC_VERBOSE").is_some() { eprintln!("ffmpeg {args:?}"); }
    if run(&args) { Some(out) } else { None }
}

#[test]
fn parity_matrix() {
    if !have_ffmpeg() {
        assert!(!require(), "ffmpeg missing");
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let only = std::env::var("EC_MPEG2_CASE").ok();
    let mut failures = Vec::new();
    for c in CASES {
        if only.as_deref().is_some_and(|o| o != c.tag) {
            continue;
        }
        let Some(es) = generate(c) else {
            assert!(!require(), "{}: ffmpeg cannot encode", c.tag);
            eprintln!("SKIP {}", c.tag);
            continue;
        };
        match compare(c.tag, &es, c.pix, &[]) {
            Ok(n) => eprintln!("OK   {} ({n} frames)", c.tag),
            Err(e) => {
                eprintln!("FAIL {}: {e}", c.tag);
                failures.push(c.tag);
            }
        }
    }
    assert!(failures.is_empty(), "mismatches: {failures:?}");
}
