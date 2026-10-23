//! No panic on damaged input: seeded random mutations of the committed fixtures
//! (flips, cuts, inserts, repeats), each opened, read through, seeked, read again.

use std::io::Cursor;
use std::path::PathBuf;

use crate::common::Rng;
use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{TimeBase, Timestamp};
use ec_mpegts::{TsDemuxer, VideoFraming};

fn exercise(bytes: Vec<u8>) {
    let Ok(mut d) = TsDemuxer::new(Cursor::new(bytes)) else {
        return;
    };
    let _ = (d.tracks(), d.programs().len(), d.duration_secs(), d.stats());
    for round in 0..2 {
        if round == 1 {
            d.set_video_framing(VideoFraming::LengthPrefixed);
        }
        while d.next_packet().is_ok() {}
        for s in 0..d.streams().len() as u32 {
            for t in [0i64, 40_000, 1 << 33, -1] {
                for mode in [SeekMode::SyncBefore, SeekMode::SyncAfter, SeekMode::Exact] {
                    let _ = d.seek(s, Timestamp::new(t, TimeBase::new(1, 90_000)), mode);
                    for _ in 0..20 {
                        if d.next_packet().is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn mutated_fixtures_never_panic() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("tiny_"))
        })
        .collect();
    names.sort();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let rounds: usize = std::env::var("EC_ROBUST_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for path in names {
        let good = std::fs::read(&path).unwrap();
        for _ in 0..rounds {
            let mut b = good.clone();
            for _ in 0..1 + rng.below(8) {
                match rng.below(6) {
                    0 => {
                        let i = rng.below(b.len());
                        b[i] ^= 1 << rng.below(8);
                    }
                    1 => {
                        let i = rng.below(b.len());
                        b.truncate(i);
                    }
                    2 => {
                        let i = rng.below(b.len());
                        let n = rng.below(400);
                        let j = (i + n).min(b.len());
                        b.drain(i..j);
                    }
                    3 => {
                        let i = rng.below(b.len());
                        let n = rng.below(300);
                        for _ in 0..n {
                            b.insert(i, rng.next() as u8);
                        }
                    }
                    4 => {
                        let i = rng.below(b.len());
                        let n = rng.below(2000).min(b.len() - i);
                        for x in &mut b[i..i + n] {
                            *x = rng.next() as u8;
                        }
                    }
                    _ => {
                        // Repeat a stretch.
                        let i = rng.below(b.len());
                        let n = rng.below(3000).min(b.len() - i);
                        let chunk = b[i..i + n].to_vec();
                        b.splice(i..i, chunk);
                    }
                }
                if b.is_empty() {
                    break;
                }
            }
            exercise(b);
        }
    }
}

#[test]
fn degenerate_inputs() {
    for len in [0usize, 1, 3, 187, 188, 189, 376, 564, 752, 1000] {
        exercise(vec![0x47; len]);
        exercise(vec![0u8; len]);
    }
    let mut v = Vec::new();
    for _ in 0..50 {
        v.push(0x47);
        v.extend_from_slice(&[0x1F, 0xFF, 0x10]);
        v.extend_from_slice(&[0xFF; 184]);
    }
    exercise(v);
}
