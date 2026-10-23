//! Decode an IVF VP9 file and report throughput and an output digest.
//!
//! Usage: `decode_ivf <file.ivf> [threads]`. `threads` sets the tile-column
//! worker count (default: `EC_VP9_THREADS`, else the available parallelism
//! capped at 6). The digest is FNV-1a over every output sample, so runs at
//! different thread counts can be compared.

use ec_vp9::decode::Decoder;
use std::time::Instant;

fn ivf_frames(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    if data.len() < 32 || &data[0..4] != b"DKIF" {
        return Err("not an IVF file".into());
    }
    let hdr_len = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = hdr_len;
    let mut out = Vec::new();
    while pos + 12 <= data.len() {
        let sz = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        let end = pos.checked_add(sz).filter(|&e| e <= data.len()).ok_or("truncated IVF frame")?;
        out.push(&data[pos..end]);
        pos = end;
    }
    Ok(out)
}

fn fnv1a(mut h: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: decode_ivf <file.ivf> [threads]")?;
    let data = std::fs::read(&path)?;
    let frames = ivf_frames(&data)?;
    let mut dec = Decoder::new();
    if let Some(n) = args.next() {
        dec.set_threads(n.parse()?);
    }
    let mut raw_out = match std::env::var_os("EC_RAW_OUT") {
        Some(p) => Some(std::fs::File::create(p)?),
        None => None,
    };
    let mut shown = 0usize;
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    let start = Instant::now();
    // Decode time alone: the digest and raw output below are harness work, not decoder work.
    let mut decode_secs = 0.0f64;
    for chunk in &frames {
        let t0 = Instant::now();
        let out = dec.decode(chunk)?;
        decode_secs += t0.elapsed().as_secs_f64();
        if let Some(pic) = out {
            shown += 1;
            if let Some(raw) = raw_out.as_mut() {
                use std::io::Write;
                for plane in [&pic.y, &pic.u, &pic.v] {
                    let bytes: Vec<u8> = plane.iter().map(|&s| s as u8).collect();
                    raw.write_all(&bytes)?;
                }
            }
            digest = fnv1a(digest, pic.y.iter().flat_map(|s| s.to_le_bytes()));
            digest = fnv1a(digest, pic.u.iter().flat_map(|s| s.to_le_bytes()));
            digest = fnv1a(digest, pic.v.iter().flat_map(|s| s.to_le_bytes()));
        }
    }
    let secs = start.elapsed().as_secs_f64();
    if ec_vp9::prof::enabled() {
        for (name, t) in ec_vp9::prof::NAMES.iter().zip(ec_vp9::prof::totals()) {
            println!("prof {name} {t:.3}s");
        }
    }
    println!(
        "frames={} shown={} digest={digest:016x} secs={secs:.3} fps={:.2} decode_secs={decode_secs:.3} decode_fps={:.2}",
        frames.len(),
        shown,
        shown as f64 / secs,
        shown as f64 / decode_secs
    );
    Ok(())
}
