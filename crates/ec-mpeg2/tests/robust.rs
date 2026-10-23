//! Robustness: truncated, corrupted and hostile streams must never panic and
//! must keep the output queue bounded.
#![allow(clippy::needless_range_loop, clippy::field_reassign_with_default, clippy::too_many_arguments, clippy::type_complexity)]

mod common;
use common::synth::{SynthCfg, generate};
use common::*;
use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg2::Mpeg2Decoder;

const MAX_QUEUE: usize = 64;

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

fn streams() -> Vec<Vec<u8>> {
    [(1u64, false, false), (2, true, false), (3, false, true), (4, true, true)]
        .into_iter()
        .map(|(seed, fields, p422)| {
            generate(&SynthCfg {
                mb_w: 3,
                mb_h: 2,
                chroma422: p422,
                progressive_sequence: false,
                frames: 5,
                seed,
                fields,
                heavy: fields,
                load_matrices: seed % 2 == 0,
            })
        })
        .collect()
}

/// Decode `data` in chunks of `chunk`; return the frame count. Errors are ignored.
fn run(data: &[u8], chunk: usize) -> usize {
    let mut dec = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap();
    let mut n = 0;
    for c in data.chunks(chunk.max(1)) {
        let _ = dec.send_bytes(c, None);
        while dec.pop_picture().is_some() {
            n += 1;
        }
    }
    let _ = dec.flush_stream();
    while dec.pop_picture().is_some() {
        n += 1;
    }
    n
}

/// Like `run` but never drains until the end: the queue must stay bounded.
fn run_undrained(data: &[u8], chunk: usize) {
    let mut dec = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap();
    for c in data.chunks(chunk.max(1)) {
        let _ = dec.send_bytes(c, None);
    }
    let _ = dec.flush_stream();
    let mut n = 0;
    while dec.pop_picture().is_some() {
        n += 1;
    }
    assert!(n <= MAX_QUEUE, "queued {n} frames");
}

#[test]
fn truncation_prefixes() {
    for es in streams() {
        assert!(run(&es, es.len()) > 0, "clean stream decodes nothing");
        let step = (es.len() / 40).max(1);
        for cut in (0..es.len()).step_by(step).chain([es.len() - 1, es.len() - 2]) {
            run(&es[..cut], 97);
            run_undrained(&es[..cut], usize::MAX);
        }
    }
}

#[test]
fn chunking_is_invariant() {
    for es in streams() {
        let mk = |chunk: usize| {
            let a = decode_ours(&es, chunk).expect("clean stream");
            a.iter().map(|(f, _)| frame_bytes(f)).collect::<Vec<_>>()
        };
        let whole = mk(es.len());
        assert!(!whole.is_empty());
        assert_eq!(whole, mk(1), "1-byte chunks differ");
        assert_eq!(whole, mk(7), "7-byte chunks differ");
    }
}

#[test]
fn random_corruptions() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let all = streams();
    for i in 0..300 {
        let mut es = all[i % all.len()].clone();
        for _ in 0..1 + rng.below(6) {
            if es.is_empty() {
                break;
            }
            let p = rng.below(es.len());
            match rng.below(4) {
                0 => es[p] ^= 1 << rng.below(8),
                1 => es[p] = rng.next() as u8,
                2 => {
                    let n = 1 + rng.below(8);
                    es.drain(p..(p + n).min(es.len()));
                }
                _ => {
                    for _ in 0..1 + rng.below(8) {
                        es.insert(p, rng.next() as u8);
                    }
                }
            }
        }
        run(&es, 1 + rng.below(300));
        run_undrained(&es, usize::MAX);
    }
}

#[test]
fn garbage_is_refused() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for len in [0usize, 1, 3, 4, 100, 5000, 200_000] {
        let g: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        run(&g, 1000);
        run_undrained(&g, usize::MAX);
    }
    // Garbage dense with start codes.
    let mut g = Vec::new();
    for _ in 0..2000 {
        g.extend_from_slice(&[0, 0, 1, rng.next() as u8]);
        for _ in 0..rng.below(20) {
            g.push(rng.next() as u8);
        }
    }
    run(&g, 333);
    run_undrained(&g, usize::MAX);
}

#[test]
fn huge_dimensions_are_bounded() {
    // Sequence header 4095x4095 + sequence_extension adding 3<<12 to both: 16383x16383.
    let mut es = vec![0, 0, 1, 0xB3, 0xFF, 0xFF, 0xFF, 0x13, 0xFF, 0xFF, 0xE0, 0x18];
    // sequence_extension: id=1, profile/level, progressive, chroma 4:2:0, size ext 3/3.
    es.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x8A, 0x00, 0x01, 0x00, 0x00]);
    es.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x8F, 0xFF, 0x01, 0x00, 0x00]);
    // GOP + picture header (I) + a slice, repeated.
    for _ in 0..3 {
        es.extend_from_slice(&[0, 0, 1, 0xB8, 0, 0x08, 0, 0x40]);
        es.extend_from_slice(&[0, 0, 1, 0x00, 0x00, 0x0F, 0xFF, 0xF8]);
        es.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x41, 0x00]);
        es.extend_from_slice(&[0, 0, 1, 0x01, 0x10, 0x00, 0x00, 0x00]);
    }
    let mut dec = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap();
    let r = dec.send_bytes(&es, None);
    let _ = dec.flush_stream();
    let mut n = 0;
    while let Some((f, _)) = dec.pop_picture() {
        assert!(u64::from(f.width) * u64::from(f.height) <= 8192 * 4352);
        n += 1;
    }
    assert!(n <= MAX_QUEUE);
    if let Some(seq) = dec.sequence() {
        eprintln!("sequence {}x{} (send: {:?})", seq.width, seq.height, r.is_ok());
    }
    // Also at the 12-bit maximum without an extension, and with the max-dimension edge.
    for (w, h) in [(4095u32, 4095u32), (8192, 8192), (16383, 1), (1, 16383)] {
        let mut s = vec![0, 0, 1, 0xB3, (w >> 4) as u8, (((w & 15) << 4) | (h >> 8)) as u8, h as u8, 0x13, 0xFF, 0xFF, 0xE0, 0x18];
        s.extend_from_slice(&[0, 0, 1, 0x00, 0x00, 0x0F, 0xFF, 0xF8, 0, 0, 1, 0x01, 0x10, 0, 0]);
        run(&s, usize::MAX);
        run_undrained(&s, 5);
    }
}

/// `decode_mpeg2` panic: a sequence extension changed the MB-row count (9 to 10 at 176x144)
/// without changing the size, so pictures allocated under the old geometry were decoded under
/// the new one and a macroblock row was written past the end of the plane. The decoder must
/// refuse the damage or conceal it and mark the frame corrupt, never panic.
#[test]
fn mb_height_change_without_resize_is_concealed() {
    let data = include_bytes!("data/fuzz/decode_mpeg2-mb-height-change.m2v");
    let mut dec = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap();
    let mut failed = dec.send_bytes(data, None).is_err();
    failed |= dec.flush_stream().is_err();
    let mut corrupt = false;
    while let Some((_, info)) = dec.pop_picture() {
        corrupt |= info.corrupt;
    }
    assert!(failed || corrupt, "damage must surface as an error or a corrupt frame");
}

/// Writes the synthetic streams as fuzz seeds:
/// `EC_SEED_DIR=crates/ec-mpeg2/fuzz/corpus/decode_mpeg2 cargo test --test robust -- --ignored dump_seeds`
#[test]
#[ignore]
fn dump_seeds() {
    let Some(dir) = std::env::var_os("EC_SEED_DIR") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    for (i, s) in streams().iter().enumerate() {
        std::fs::write(std::path::Path::new(&dir).join(format!("syn{i}.m2v")), s).unwrap();
    }
}
