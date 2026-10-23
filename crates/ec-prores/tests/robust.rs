//! Robustness: truncated, corrupted and hostile frames must never panic, and refusals must name
//! what they refused.

mod common;
use common::*;
use ec_core::Error;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Small frames: progressive 422, interlaced 422, 4444 with alpha.
fn frames() -> Option<Vec<Vec<u8>>> {
    static CACHE: std::sync::OnceLock<Option<Vec<Vec<u8>>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(make_frames).clone()
}

fn make_frames() -> Option<Vec<Vec<u8>>> {
    if !ffmpeg_or_skip() {
        return None;
    }
    let dir = scratch("robust");
    let mut out = Vec::new();
    for (i, (prof, fmt, extra)) in [
        ("3", "yuv422p10le", ""),
        ("1", "yuv422p10le", "+ildct"),
        ("4", "yuva444p10le", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let mov = dir.join(format!("r{i}.mov"));
        let mut a = vec![
            "-f", "lavfi", "-i", "testsrc2=size=100x70:rate=25,noise=alls=20:allf=t", "-frames:v", "1",
            "-c:v", "prores_ks", "-profile", prof, "-pix_fmt", fmt,
        ];
        if !extra.is_empty() {
            a.extend(["-flags", extra]);
        }
        a.push(mov.to_str().unwrap());
        assert!(run(&a));
        out.push(packets(&std::fs::read(&mov).unwrap()).remove(0));
    }
    let _ = std::fs::remove_dir_all(dir);
    Some(out)
}

#[test]
fn truncations_never_panic() {
    let Some(fs) = frames() else { return };
    for f in &fs {
        let mut d = decoder(2);
        assert!(d.decode(f).is_ok());
        for n in (0..f.len()).step_by(7).chain(0..40) {
            let _ = d.decode(&f[..n.min(f.len())]);
        }
    }
}

#[test]
fn corruptions_never_panic() {
    let Some(fs) = frames() else { return };
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for f in &fs {
        let mut d = decoder(3);
        for round in 0..400 {
            let mut g = f.clone();
            let flips = 1 + rng.below(8);
            for _ in 0..flips {
                // Half the hits land in the headers, where the structure lives.
                let at = if round % 2 == 0 { rng.below(g.len().min(200)) } else { rng.below(g.len()) };
                g[at] ^= 1 << rng.below(8);
            }
            let _ = d.decode(&g);
        }
    }
}

#[test]
fn garbage_is_refused_by_name() {
    let mut d = decoder(1);
    let mut rng = Rng(7);
    for len in [0usize, 1, 7, 8, 27, 28, 200, 5000] {
        let g: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        assert!(d.decode(&g).is_err());
    }
    // Right magic, nonsense after it.
    let mut g = vec![0u8; 300];
    g[..4].copy_from_slice(&300u32.to_be_bytes());
    g[4..8].copy_from_slice(b"icpf");
    match d.decode(&g) {
        Err(Error::Unsupported { .. } | Error::Corrupt { .. }) => {}
        other => panic!("expected a named refusal, got {:?}", other.map(|f| f.width)),
    }
}

#[test]
fn hostile_dimensions_are_bounded() {
    let Some(fs) = frames() else { return };
    let mut g = fs[0].clone();
    // width/height at frame-header bytes 8..12 (after size+icpf: offset 8+8).
    g[16..20].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let t = std::time::Instant::now();
    assert!(decoder(1).decode(&g).is_err());
    assert!(t.elapsed().as_secs() < 5);
    g[16..20].copy_from_slice(&[0, 0, 0, 0]);
    assert!(decoder(1).decode(&g).is_err());
}

#[test]
fn wrong_codec_is_unsupported() {
    use ec_core::registry::{CodecId, CodecParameters};
    match ec_prores::ProResDecoder::new(CodecParameters::new(CodecId::H264)) {
        Err(Error::Unsupported { .. }) => {}
        _ => panic!("expected Unsupported"),
    }
}

/// `EC_SEED_DIR=crates/ec-prores/fuzz/corpus/decode_prores cargo test -p ec-prores --test robust -- --ignored dump_seeds`
#[test]
#[ignore]
fn dump_seeds() {
    let dir = std::env::var("EC_SEED_DIR").expect("EC_SEED_DIR");
    std::fs::create_dir_all(&dir).unwrap();
    let Some(fs) = frames() else { return };
    for (i, f) in fs.iter().enumerate() {
        std::fs::write(format!("{dir}/seed{i}.bin"), f).unwrap();
    }
}
