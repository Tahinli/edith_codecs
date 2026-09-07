//! Encode-side profiling driver: the BD gate's own ladder, without ffmpeg's
//! decode side or the reference encoders, so `perf record` attributes only
//! our encoder. Also the byte-exactness gate for encoder-internal rewrites:
//! it prints each point's stream length and a hash of its bytes.
//!
//! Usage: `enc_probe <clip> <width> <height> <frames> [q,q,..]`

use std::process::Command;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (clip, width, height, frames) = (
        a[0].clone(),
        a[1].parse::<usize>().unwrap(),
        a[2].parse::<usize>().unwrap(),
        a[3].parse::<usize>().unwrap(),
    );
    let qs: Vec<u8> = a
        .get(4)
        .map_or_else(|| vec![150, 120, 90, 60], |s| s.split(',').map(|q| q.parse().unwrap()).collect());

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", &clip, "-frames:v", &frames.to_string()])
        // lane-census: the BD gate's native rows CROP, they do not scale, so
        // the census can ask for the gate's own recipe (`EC_ENC_VF`).
        .args([
            "-vf",
            &std::env::var("EC_ENC_VF").unwrap_or_else(|_| format!("scale={width}:{height}")),
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg failed to run");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let (luma, chroma) = (width * height, width * height / 4);
    let frame_len = luma + 2 * chroma;
    let source: Vec<ec_av1::encode::Picture> = (0..frames)
        .map(|i| {
            let b = &out.stdout[i * frame_len..][..frame_len];
            ec_av1::encode::Picture {
                width,
                height,
                y: b[..luma].iter().map(|&v| u16::from(v)).collect(),
                u: b[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                v: b[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
            }
        })
        .collect();

    let start = std::time::Instant::now();
    for q in qs {
        let e = ec_av1::encode::encode_sequence(&source, q, 0.5).unwrap();
        // FNV-1a over the stream: a byte-exactness witness, not a checksum
        // with any other job.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in &e.stream {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        println!("q={q} bytes={} fnv1a={h:016x}", e.stream.len());
        // lane-census: keep the stream itself when asked, so `syntax_census`
        // can read the very bytes this point measured.
        if let Ok(dir) = std::env::var("EC_ENC_OUT") {
            std::fs::write(format!("{dir}/ours-q{q}.obu"), &e.stream).expect("stream out");
        }
        if ec_av1::encode::census_on() {
            let (kinds, luma_rank, chroma_rank) = ec_av1::encode::take_census();
            for (name, n) in ec_av1::encode::CENSUS_KINDS.iter().zip(kinds) {
                println!("  census {name}: {n}");
            }
            let pct = |h: &[usize]| {
                let total: usize = h.iter().sum::<usize>().max(1);
                h.iter()
                    .map(|n| format!("{:.1}%", 100.0 * *n as f64 / total as f64))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            println!("  census luma SAD rank of RD winner: {}", pct(&luma_rank));
            println!("  census chroma SAD rank of RD winner: {}", pct(&chroma_rank));
        }
    }
    println!("wall {:.3}s", start.elapsed().as_secs_f64());
}
