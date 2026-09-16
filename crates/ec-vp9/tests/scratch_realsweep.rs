//! [scratch] Real-content keyframe sweep.
//!
//! The library carries no VP9 content of its own (its one webm, in
//! `~/Downloads`, is VP8), so the keyframe path only ever sees real imagery
//! through a transcode: encode `SWEEP_SRC` as all-intra VP9 with ffmpeg
//! (`-g 1`, so every frame is a keyframe), decode every frame with `ec-vp9`,
//! and diff Y/U/V against ffmpeg's own decode of the same file.
//!
//! ```text
//! SWEEP_SRC=<video> [SWEEP_N=<frames>] \
//!   cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture
//! ```
//!
//! Prints one line per frame plus a `RESULT` line; fails if any frame differs.
mod ivf;

use ec_vp9::decode::Decoder;

#[test]
fn real_content_sweep() {
    let src = std::env::var("SWEEP_SRC").unwrap_or_default();
    let prebuilt = std::env::var("SWEEP_IVF").ok();
    if src.is_empty() && prebuilt.is_none() {
        println!("SKIP: set SWEEP_SRC=<video> [SWEEP_N=<frames>] or SWEEP_IVF=<ivf>");
        return;
    }
    let n: usize = std::env::var("SWEEP_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let dir = std::env::temp_dir().join("ec-vp9-sweep");
    std::fs::create_dir_all(&dir).unwrap();
    let ivf_path = dir.join("allintra.ivf");
    if let Some(p) = &prebuilt {
        std::fs::copy(p, &ivf_path).unwrap();
    } else {
        let enc = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i", &src, "-frames:v"])
            .arg(n.to_string())
            .args([
                "-c:v",
                "libvpx-vp9",
                "-g",
                "1",
                "-deadline",
                "good",
                "-cpu-used",
                "4",
                "-crf",
                "32",
                "-pix_fmt",
                "yuv420p",
            ])
            // `SWEEP_TILE0=1` pins a single tile: the multi-tile path is what
            // ffmpeg picks by default at 1080p+ and it is diagnosed separately.
            .args(if std::env::var_os("SWEEP_TILE0").is_some() {
                vec!["-tile-columns", "0", "-tile-rows", "0"]
            } else {
                vec![]
            })
            .args(["-f", "ivf"])
            .arg(&ivf_path)
            .status()
            .expect("ffmpeg encode");
        assert!(enc.success(), "ffmpeg encode of {src} failed");
    }
    // Decode list as a human would, to prove our YUV probe is honest.
    let raw = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&ivf_path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg decode");
    assert!(raw.status.success(), "ffmpeg decode of the IVF failed");

    let bytes = std::fs::read(&ivf_path).unwrap();
    let (_fourcc, w, h, frames) = ivf::parse_ivf(&bytes);
    let (w, h) = (w as usize, h as usize);
    let ysz = w * h;
    let uvs = (w / 2) * (h / 2);
    assert_eq!(
        raw.stdout.len(),
        frames.len() * (ysz + 2 * uvs),
        "reference frame size mismatch"
    );

    let mut bad = 0usize;
    for (i, frame) in frames.iter().enumerate() {
        let mut dec = Decoder::new();
        let pic = match dec.decode(&frame.data) {
            Ok(Some(p)) => p,
            other => {
                println!("frame {i}: DECODE REFUSED/ERR {other:?}");
                bad += 1;
                continue;
            }
        };
        let base = i * (ysz + 2 * uvs);
        if i == 0 {
            std::fs::write(dir.join("ours_f0.yuv"), {
                let mut v = Vec::with_capacity(ysz + 2 * uvs);
                v.extend_from_slice(&pic.y[..ysz.min(pic.y.len())]);
                v.extend_from_slice(&pic.u);
                v.extend_from_slice(&pic.v);
                v
            })
            .unwrap();
            std::fs::write(dir.join("ff_f0.yuv"), &raw.stdout[..ysz + 2 * uvs]).unwrap();
        }
        let yd = (0..ysz)
            .filter(|&k| pic.y[k] != raw.stdout[base + k])
            .count();
        let ud = (0..uvs)
            .filter(|&k| pic.u[k] != raw.stdout[base + ysz + k])
            .count();
        let vd = (0..uvs)
            .filter(|&k| pic.v[k] != raw.stdout[base + ysz + uvs + k])
            .count();
        println!(
            "frame {i}: {w}x{h} stride {} uv_stride {} Y {yd} U {ud} V {vd}",
            pic.stride, pic.uv_stride
        );
        if yd + ud + vd > 0 {
            bad += 1;
        }
    }
    println!(
        "RESULT {} of {} frames byte-exact (source {})",
        frames.len() - bad,
        frames.len(),
        if src.is_empty() { ivf_path.display().to_string() } else { src.clone() }
    );
    assert_eq!(bad, 0, "{bad} frame(s) differ from ffmpeg");
}
