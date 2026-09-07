//! Encode-side profiling driver: the BD gate's own ladder, without ffmpeg's
//! decode side or the reference encoders, so `perf record` attributes only
//! our encoder. Also the byte-exactness gate for encoder-internal rewrites:
//! it prints each point's stream length and a hash of its bytes.
//!
//! Usage: `enc_probe <clip> <width|gate> <height> <frames> [q,q,..]`
//!
//! `gate` as the width takes the native BD gate's own window on the clip
//! (`probe::gate_crop`, superblock-aligned centre crop) and ignores the height
//! argument; `EC_ENC_SS` is the gate's seek and `EC_ENC_VF` overrides the
//! filter chain. Without both of those the probe measures different pixels
//! than the gate it reports on.

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let clip = a[0].clone();
    let frames: usize = a[3].parse().unwrap();
    let gate = a[1] == "gate";
    let (width, height) = match gate {
        true => (0, 0),
        false => (a[1].parse().unwrap(), a[2].parse().unwrap()),
    };
    let qs: Vec<u8> = a
        .get(4)
        .map_or_else(|| vec![150, 120, 90, 60], |s| s.split(',').map(|q| q.parse().unwrap()).collect());

    // lane-census/lane-probe: the BD gate's native rows CROP (`EC_ENC_VF`) and
    // SEEK (`EC_ENC_SS`) -- without the seek this probe read film B's black
    // leader and every census off it was probe-relative.
    let (vf, width, height) = match gate {
        true => ec_av1::probe::gate_crop(&clip),
        false => (
            std::env::var("EC_ENC_VF").unwrap_or_else(|_| format!("scale={width}:{height}")),
            width,
            height,
        ),
    };
    let vf = match gate {
        true => std::env::var("EC_ENC_VF").unwrap_or(vf),
        false => vf,
    };
    eprintln!("probe: {width}x{height} vf={vf}");
    let skip = std::env::var("EC_ENC_SS").unwrap_or_else(|_| "0".into());
    let source = ec_av1::probe::source(&clip, &skip, &vf, width, height, frames);

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
