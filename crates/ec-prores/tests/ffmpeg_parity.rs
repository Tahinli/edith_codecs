//! Byte-exact comparison with ffmpeg's `prores` decoder on streams ffmpeg's own
//! `prores_ks` / `prores_aw` encoders produce from synthetic sources, via .mov and ec-mp4.

mod common;
use common::*;

struct Case {
    tag: &'static str,
    size: &'static str,
    frames: u32,
    src: &'static str,
    enc: &'static [&'static str],
    /// Encoder input pixel format.
    pix: &'static str,
    /// What ffmpeg decodes to / we are compared in.
    out: &'static str,
}

macro_rules! c {
    ($tag:expr, $size:expr, $n:expr, $src:expr, [$($e:expr),*], $pix:expr, $out:expr) => {
        Case { tag: $tag, size: $size, frames: $n, src: $src, enc: &["-c:v", $($e),*], pix: $pix, out: $out }
    };
}

const P422: &str = "yuv422p10le";
const P444: &str = "yuv444p10le";
const PA: &str = "yuva444p10le";
const O422: &str = "yuv422p10le";
const O444: &str = "yuv444p12le";
const OA: &str = "yuva444p12le";

const CASES: &[Case] = &[
    c!("ks_proxy", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "0"], P422, O422),
    c!("ks_lt", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "1"], P422, O422),
    c!("ks_std", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "2"], P422, O422),
    c!("ks_hq", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "3"], P422, O422),
    c!("ks_4444", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "4"], P444, O444),
    c!("ks_4444xq", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "5"], P444, O444),
    c!("ks_4444_alpha16", "96x64", 2, "testsrc2,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='mod(X*13+Y*7,1024)'", ["prores_ks", "-profile", "4", "-alpha_bits", "16"], PA, OA),
    c!("ks_4444_alpha8", "96x64", 2, "testsrc2,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,40),1023,mod(X*5,1024))'", ["prores_ks", "-profile", "4", "-alpha_bits", "8"], PA, OA),
    c!("ks_4444xq_alpha", "100x70", 2, "testsrc2,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='255*mod(floor(X/9)+floor(Y/7),2)*4'", ["prores_ks", "-profile", "5"], PA, OA),
    c!("aw_proxy", "96x64", 2, "testsrc2", ["prores_aw", "-profile", "0"], P422, O422),
    c!("aw_lt", "96x64", 2, "testsrc2", ["prores_aw", "-profile", "1"], P422, O422),
    c!("aw_std", "96x64", 2, "testsrc2", ["prores_aw", "-profile", "2"], P422, O422),
    c!("aw_hq", "96x64", 2, "testsrc2", ["prores_aw", "-profile", "3"], P422, O422),
    c!("aw_4444", "96x64", 2, "testsrc2", ["prores_aw", "-profile", "4"], P444, O444),
    c!("odd_size", "101x67", 2, "testsrc2", ["prores_ks", "-profile", "3"], P422, O422),
    c!("odd_size_444", "99x35", 2, "mandelbrot", ["prores_ks", "-profile", "4"], P444, O444),
    c!("tiny", "16x16", 2, "testsrc2", ["prores_ks", "-profile", "2"], P422, O422),
    c!("one_px_over", "17x17", 2, "testsrc2", ["prores_ks", "-profile", "2"], P422, O422),
    c!("noise_hq", "160x96", 3, "testsrc2,noise=alls=80:allf=t", ["prores_ks", "-profile", "3"], P422, O422),
    c!("noise_4444", "160x96", 3, "testsrc2,noise=alls=80:allf=t", ["prores_ks", "-profile", "4"], P444, O444),
    c!("noise_xq", "160x96", 3, "testsrc2,noise=alls=100:allf=t", ["prores_ks", "-profile", "5"], P444, O444),
    c!("saturated", "160x96", 2, "testsrc2,eq=contrast=3:brightness=0.2", ["prores_ks", "-profile", "3"], P422, O422),
    c!("flat", "64x48", 2, "color=c=gray", ["prores_ks", "-profile", "3"], P422, O422),
    c!("black", "64x48", 2, "color=c=black", ["prores_ks", "-profile", "0"], P422, O422),
    c!("white_xq", "64x48", 2, "color=c=white", ["prores_ks", "-profile", "5"], P444, O444),
    // Slice sizes.
    c!("mbs1", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "3", "-mbs_per_slice", "1"], P422, O422),
    c!("mbs2", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "3", "-mbs_per_slice", "2"], P422, O422),
    c!("mbs4", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "3", "-mbs_per_slice", "4"], P422, O422),
    c!("mbs8", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "3", "-mbs_per_slice", "8"], P422, O422),
    c!("mbs1_444", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "4", "-mbs_per_slice", "1"], P444, O444),
    c!("mbs4_444", "176x96", 2, "testsrc2", ["prores_ks", "-profile", "4", "-mbs_per_slice", "4"], P444, O444),
    c!("mbs3_width", "208x64", 2, "testsrc2", ["prores_ks", "-profile", "3", "-mbs_per_slice", "8"], P422, O422),
    // Quantisation: matrices and quantiser index.
    c!("qm_proxy", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "2", "-quant_mat", "proxy"], P422, O422),
    c!("qm_lt", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "2", "-quant_mat", "lt"], P422, O422),
    c!("qm_std", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "3", "-quant_mat", "standard"], P422, O422),
    c!("qm_hq", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "0", "-quant_mat", "hq"], P422, O422),
    c!("qm_default", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "3", "-quant_mat", "default"], P422, O422),
    c!("qm_4444", "96x64", 2, "testsrc2", ["prores_ks", "-profile", "4", "-quant_mat", "proxy"], P444, O444),
    c!("q1", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "3", "-qscale:v", "1"], P422, O422),
    c!("q4", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "3", "-qscale:v", "4"], P422, O422),
    c!("q16", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "3", "-qscale:v", "16"], P422, O422),
    c!("q31", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "3", "-qscale:v", "31"], P422, O422),
    c!("q1_444", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "4", "-qscale:v", "1"], P444, O444),
    c!("bpm_small", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "3", "-bits_per_mb", "300"], P422, O422),
    c!("bpm_big", "96x64", 2, "testsrc2,noise=alls=40:allf=t", ["prores_ks", "-profile", "5", "-bits_per_mb", "4000"], P444, O444),
    // Interlaced.
    c!("ilace_tff", "128x96", 3, "testsrc2", ["prores_ks", "-profile", "3", "-flags", "+ildct", "-top", "1"], P422, O422),
    c!("ilace_bff", "128x96", 3, "testsrc2", ["prores_ks", "-profile", "3", "-flags", "+ildct", "-top", "0"], P422, O422),
    c!("ilace_444", "128x96", 2, "testsrc2", ["prores_ks", "-profile", "4", "-flags", "+ildct", "-top", "1"], P444, O444),
    c!("ilace_odd", "98x70", 2, "testsrc2", ["prores_ks", "-profile", "2", "-flags", "+ildct", "-top", "1"], P422, O422),
    c!("ilace_alpha", "96x64", 2, "testsrc2,format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='mod(X*13+Y*7,1024)'", ["prores_ks", "-profile", "4", "-flags", "+ildct", "-top", "0"], PA, OA),
    c!("ilace_mbs2", "128x96", 2, "testsrc2", ["prores_ks", "-profile", "1", "-flags", "+ildct", "-mbs_per_slice", "2"], P422, O422),
    // Bigger.
    c!("sd_pal", "720x576", 2, "testsrc2", ["prores_ks", "-profile", "3", "-flags", "+ildct", "-top", "1"], P422, O422),
    c!("hd_1080", "1920x1080", 2, "testsrc2,noise=alls=10:allf=t", ["prores_ks", "-profile", "3"], P422, O422),
    c!("hd_1080_4444", "1920x1080", 1, "mandelbrot", ["prores_ks", "-profile", "4"], P444, O444),
];

fn generate(c: &Case) -> std::path::PathBuf {
    let dir = scratch(c.tag);
    let out = dir.join("in.mov");
    let (base, chain) = match c.src.split_once(',') {
        Some((b, rest)) => (b, format!(",{rest}")),
        None => (c.src, String::new()),
    };
    let lavfi = if base.starts_with("color") {
        format!("{}:size={}:rate=25", c.src, c.size)
    } else {
        format!("{base}=size={}:rate=25{chain}", c.size)
    };
    let frames = c.frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &lavfi, "-frames:v", &frames, "-pix_fmt", c.pix];
    args.extend_from_slice(c.enc);
    args.push(out.to_str().unwrap());
    assert!(run(&args), "{}: ffmpeg encode failed", c.tag);
    out
}

#[test]
fn matches_ffmpeg() {
    if !ffmpeg_or_skip() {
        return;
    }
    let mut bad = Vec::new();
    for c in CASES {
        let mov = generate(c);
        let reference = raw_ffmpeg(c.tag, &mov, c.out).unwrap();
        let bytes = std::fs::read(&mov).unwrap();
        for threads in [1usize, 3] {
            match decode_ours(&bytes, threads) {
                Ok(frames) => {
                    let ours = raw_ours(&frames);
                    if let Some(at) = first_diff(&ours, &reference) {
                        bad.push(format!("{} (threads {threads}): first diff at byte {at} (ours {} ref {})", c.tag, ours.len(), reference.len()));
                    }
                }
                Err(e) => bad.push(format!("{} (threads {threads}): {e}", c.tag)),
            }
        }
        let _ = std::fs::remove_dir_all(scratch(c.tag));
    }
    assert!(bad.is_empty(), "mismatches:\n{}", bad.join("\n"));
}
