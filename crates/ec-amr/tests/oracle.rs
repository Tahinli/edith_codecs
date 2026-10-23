//! AMR-NB oracle: synthetic sources are encoded with ffmpeg's libopencore_amrnb (every mode,
//! DTX on/off), then decoded by this crate and by ffmpeg. Against libopencore_amrnb (the
//! reference this port follows) the output must be bit-exact; against ffmpeg's native `amrnb`
//! decoder the SNR is reported and must stay high.
//!
//! The fixtures are generated on the fly with ffmpeg from lavfi sources. Without ffmpeg (or
//! without libopencore) the tests skip, unless `EC_REQUIRE_FIXTURES=1`, which makes that a failure.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_amr::nb::bitstream::frame_bytes;
use ec_amr::nb::decoder::AmrNbDecoder;

const MODES: [(u32, &str); 8] = [
    (4750, "mr475"),
    (5150, "mr515"),
    (5900, "mr59"),
    (6700, "mr67"),
    (7400, "mr74"),
    (7950, "mr795"),
    (10200, "mr102"),
    (12200, "mr122"),
];

const SPEECH: &str = "0.3*(sin(2*PI*140*t)+0.5*sin(2*PI*280*t)+0.3*sin(2*PI*420*t)+0.2*sin(2*PI*1100*t))*(0.5+0.5*sin(2*PI*3*t))";

fn sources() -> Vec<(&'static str, String)> {
    vec![
        ("sweep", "aevalsrc=0.5*sin(2*PI*(200+400*t)*t):s=8000:d=5".into()),
        ("noise", "anoisesrc=d=5:c=pink:r=8000:a=0.3".into()),
        ("speech", format!("aevalsrc={SPEECH}:s=8000:d=5")),
        (
            "gaps",
            format!(
                "aevalsrc=if(between(mod(t\\,4)\\,1\\,2.5)\\,{SPEECH}\\,0.001*(random(0)-0.5)):s=8000:d=12"
            ),
        ),
    ]
}

fn require() -> bool {
    std::env::var("EC_REQUIRE_FIXTURES").map(|v| v == "1").unwrap_or(false)
}

fn workdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ec-amr-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-h", "decoder=libopencore_amrnb"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("libopencore_amrnb"))
        .unwrap_or(false)
}

fn run(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-y"]).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Encode `src` (lavfi) to a .amr file with libopencore_amrnb.
fn encode(dir: &Path, name: &str, src: &str, bitrate: u32, dtx: bool) -> Option<PathBuf> {
    let path = dir.join(format!("{name}-{bitrate}-{}.amr", dtx as u8));
    let br = bitrate.to_string();
    let p = path.to_str().unwrap();
    run(&[
        "-f", "lavfi", "-i", src, "-ar", "8000", "-ac", "1", "-c:a", "libopencore_amrnb", "-b:a", &br, "-dtx",
        if dtx { "1" } else { "0" }, "-f", "amr", p,
    ])?;
    Some(path)
}

fn ffmpeg_decode(path: &Path, codec: &str) -> Vec<i16> {
    let out = run(&["-c:a", codec, "-i", path.to_str().unwrap(), "-f", "s16le", "-"])
        .unwrap_or_else(|| panic!("ffmpeg -c:a {codec} failed on {}", path.display()));
    out.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// Split a `.amr` file into frames (ToC byte + payload).
fn parse_amr(data: &[u8]) -> Vec<Vec<u8>> {
    let mut pos = if data.starts_with(b"#!AMR\n") { 6 } else { 0 };
    let mut frames = Vec::new();
    while pos < data.len() {
        let ft = (data[pos] >> 3) & 15;
        let Some(n) = frame_bytes(ft) else { break };
        if pos + 1 + n > data.len() {
            break;
        }
        frames.push(data[pos..pos + 1 + n].to_vec());
        pos += 1 + n;
    }
    frames
}

fn write_amr(path: &Path, frames: &[Vec<u8>]) {
    let mut d = b"#!AMR\n".to_vec();
    for f in frames {
        d.extend_from_slice(f);
    }
    std::fs::write(path, d).unwrap();
}

fn decode_all(frames: &[Vec<u8>]) -> Vec<i16> {
    let mut dec = AmrNbDecoder::new();
    let mut out = Vec::new();
    for f in frames {
        out.extend_from_slice(&dec.decode(f).expect("decode"));
    }
    out
}

fn snr_db(reference: &[i16], test: &[i16]) -> f64 {
    let n = reference.len().min(test.len());
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 0..n {
        let r = reference[i] as f64;
        sig += r * r;
        let d = r - test[i] as f64;
        err += d * d;
    }
    if err == 0.0 {
        return 200.0;
    }
    10.0 * (sig.max(1.0) / err).log10()
}

fn first_diff(a: &[i16], b: &[i16]) -> Option<usize> {
    (0..a.len().min(b.len())).find(|&i| a[i] != b[i]).or((a.len() != b.len()).then_some(a.len().min(b.len())))
}

fn setup() -> Option<PathBuf> {
    if !ffmpeg_ok() {
        assert!(!require(), "EC_REQUIRE_FIXTURES=1 but ffmpeg with libopencore_amrnb is unavailable");
        eprintln!("skipping: ffmpeg/libopencore_amrnb not available");
        return None;
    }
    Some(workdir())
}

#[test]
fn all_modes_bit_exact_vs_libopencore() {
    let Some(dir) = setup() else { return };
    let mut checked = 0;
    for dtx in [false, true] {
        for (rate, label) in MODES {
            for (sname, src) in sources() {
                if dtx && sname != "gaps" && sname != "speech" {
                    continue;
                }
                let path = encode(&dir, sname, &src, rate, dtx).expect("encode");
                let frames = parse_amr(&std::fs::read(&path).unwrap());
                assert!(frames.len() > 100);
                let sids = frames.iter().filter(|f| (f[0] >> 3) & 15 == 8).count();
                let nodata = frames.iter().filter(|f| (f[0] >> 3) & 15 == 15).count();
                let want = ffmpeg_decode(&path, "libopencore_amrnb");
                let got = decode_all(&frames);
                assert_eq!(
                    first_diff(&got, &want),
                    None,
                    "{label} {sname} dtx={dtx}: differs from libopencore_amrnb ({} vs {} samples)",
                    got.len(),
                    want.len()
                );
                let native = ffmpeg_decode(&path, "amrnb");
                let snr = snr_db(&native, &got);
                eprintln!(
                    "{label:6} {sname:7} dtx={} frames={} sid={sids} nodata={nodata}: bit-exact, SNR vs native amrnb = {snr:.1} dB",
                    dtx as u8,
                    frames.len()
                );
                if sids == 0 {
                    assert!(snr > 15.0, "{label} {sname}: SNR vs native {snr:.1} dB");
                }
                checked += 1;
            }
        }
    }
    assert!(checked >= 8 * 6);
}

/// Deterministic xorshift for corruption patterns.
fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn lost_and_bit_flipped_frames_vs_libopencore() {
    let Some(dir) = setup() else { return };
    let mut seed = 0x1234_5678_9abc_def1u64;
    for (rate, label) in MODES {
        let (sname, src) = &sources()[2];
        let path = encode(&dir, sname, src, rate, false).expect("encode");
        let mut frames = parse_amr(&std::fs::read(&path).unwrap());
        for f in frames.iter_mut() {
            match rng(&mut seed) % 10 {
                0 => *f = vec![15 << 3 | 4], // NO_DATA
                1 | 2 if f.len() > 2 => {
                    let i = 1 + (rng(&mut seed) as usize % (f.len() - 1));
                    f[i] ^= 1 << (rng(&mut seed) % 8);
                }
                _ => {}
            }
        }
        let cpath = dir.join(format!("corrupt-{label}.amr"));
        write_amr(&cpath, &frames);
        let want = ffmpeg_decode(&cpath, "libopencore_amrnb");
        let got = decode_all(&frames);
        assert_eq!(first_diff(&got, &want), None, "{label}: corrupted stream differs from libopencore_amrnb");
    }
}

#[test]
fn garbage_never_panics() {
    let mut dec = AmrNbDecoder::new();
    let mut seed = 0xdead_beef_cafe_f00du64;
    for i in 0..4000usize {
        let len = (rng(&mut seed) % 40) as usize;
        let mut f: Vec<u8> = (0..len).map(|_| rng(&mut seed) as u8).collect();
        if i % 3 == 0 && !f.is_empty() {
            f[0] = ((rng(&mut seed) % 16) as u8) << 3 | 4;
        }
        let pcm = dec.decode(&f).expect("decode never errors");
        assert_eq!(pcm.len(), 160);
    }
    // empty and truncated well-formed frames
    assert!(dec.decode(&[]).is_ok());
    assert!(dec.decode(&[7 << 3 | 4, 1, 2]).is_ok());
    assert!(dec.decode(&[8 << 3 | 4]).is_ok());
    // decoder keeps working afterwards
    assert_eq!(dec.decode(&[15 << 3 | 4]).unwrap().len(), 160);
}
