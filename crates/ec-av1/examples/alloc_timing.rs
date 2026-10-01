//! Decode wall time and minor page faults for one `EC_AV1_PLANE_ALLOC` arm.
//!
//! The three closures lane-uballoc had to choose between (see
//! `lanes/av1uballoc.report.md`) are selected by `EC_AV1_PLANE_ALLOC` inside
//! the crate, not by a build flag -- see [`ec_av1`]'s `fresh_plane`. Measuring
//! them that way means all arms share one binary, one allocator state and one
//! machine, which three separate builds cannot do.
//!
//! ```text
//! EC_AV1_PLANE_ALLOC=zeroed cargo run -p ec-av1 --release --example alloc_timing -- s.obu
//! ```
//!
//! One process does ONE decode and prints ONE line, so the harness interleaves
//! arms across processes instead of trusting one process's warm-up history:
//!
//! ```text
//! ALLOC_TIMING arm=zeroed frames=24 wall_ms=1234.567 minflt=61234 majflt=0 hash=8f2a...
//! ```
//!
//! `hash` is FNV-1a over every picture's Y/U/V samples in decode order. It is
//! the cross-arm determinism evidence: identical `hash` on every arm means the
//! closure chosen does not move a single output sample.
use std::time::Instant;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Minor/major faults from `/proc/self/stat` fields 10 and 12, read around the
/// decode. A zeroed-from-the-kernel allocation and an explicit memset differ in
/// how many pages they actually touch, and that is the only cheap proxy for it
/// that does not need `perf`.
fn faults() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Everything before the last ')' is pid + comm, and comm may hold spaces.
    let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
        return (0, 0);
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    // `rest` starts at field 3 (state); minflt is field 10, majflt field 12.
    (
        f.get(7).and_then(|v| v.parse().ok()).unwrap_or(0),
        f.get(9).and_then(|v| v.parse().ok()).unwrap_or(0),
    )
}

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: alloc_timing <stream.obu>");
        std::process::exit(2);
    };
    let arm = std::env::var("EC_AV1_PLANE_ALLOC").unwrap_or_else(|_| "<unset>".into());
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };

    // One warm-up decode is NOT done here on purpose: the plane allocations are
    // freed back to the allocator between frames anyway, and a discarded decode
    // would only hand one arm a warmer arena than another. The harness runs
    // each arm many times and reports the median.
    let (minflt0, majflt0) = faults();
    let t0 = Instant::now();
    let mut frames = 0usize;
    let mut hash = FNV_OFFSET;
    let r = ec_av1::stream::decode_stream_with(&data, |pic, _idx, shown| {
        if !shown {
            return Ok(());
        }
        frames += 1;
        for p in [&pic.y, &pic.u, &pic.v] {
            let mut buf = Vec::with_capacity(p.len() * 2);
            for &s in p.iter() {
                buf.extend_from_slice(&s.to_le_bytes());
            }
            hash = fnv1a(hash, &buf);
        }
        Ok(())
    });
    let wall = t0.elapsed();
    let (minflt1, majflt1) = faults();
    match r {
        Ok(()) => println!(
            "ALLOC_TIMING arm={arm} frames={frames} wall_ms={:.3} minflt={} majflt={} hash={hash:016x}",
            wall.as_secs_f64() * 1e3,
            minflt1.saturating_sub(minflt0),
            majflt1.saturating_sub(majflt0),
        ),
        Err(e) => {
            eprintln!("{path}: decode refused: {e}");
            std::process::exit(3);
        }
    }
}
