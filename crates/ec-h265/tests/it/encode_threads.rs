//! The wavefront is deterministic: one clip encodes to the same bytes at 1, 2, 4
//! and 6 worker threads, for both the intra-only stream (every picture an IDR) and
//! the IDR+P stream, and the P stream decodes to the encoder's own reconstruction.
//!
//! Rows are dealt to workers round-robin, and each worker reads the row above only
//! through the snapshots of finished CTBs, so the thread count changes the schedule
//! and nothing else. A test that only checked one thread count would miss a read
//! that reaches past what a worker has imported, so this compares several.

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::Decoder;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};

/// A 320x240 clip of a textured pattern that moves a few samples per picture, so
/// the P pictures find real motion and code both merge and AMVP units.
fn moving_clip(frames: usize, w: usize, h: usize) -> Vec<VideoFrame> {
    let (cw, ch) = (w / 2, h / 2);
    (0..frames)
        .map(|t| {
            let mut y = vec![0u8; w * h];
            for r in 0..h {
                for c in 0..w {
                    let (x, yy) = (c + 3 * t, r + t);
                    // A ramp with a hashed texture, so no two blocks are alike.
                    let mut v = (x * 7 + yy * 5) as u32;
                    v ^= (x as u32).wrapping_mul(0x9e37) ^ (yy as u32).wrapping_mul(0x85eb);
                    y[r * w + c] = (v.wrapping_mul(2_654_435_761) >> 24) as u8;
                }
            }
            let chroma = |scale: usize| -> Vec<u8> {
                (0..cw * ch)
                    .map(|i| {
                        let (c, r) = (i % cw, i / cw);
                        ((c * scale + r * 3 + t * 2) % 251) as u8
                    })
                    .collect()
            };
            VideoFrame::try_new(
                PixelFormat::I420,
                w as u32,
                h as u32,
                vec![
                    Plane::new(y, w),
                    Plane::new(chroma(5), cw),
                    Plane::new(chroma(9), cw),
                ],
            )
            .expect("I420 planes at the stated size")
        })
        .collect()
}

/// The access units of `frames` coded with `threads` workers at `keyint`, and the
/// reconstruction of every picture.
fn encode_at(frames: &[VideoFrame], keyint: usize, threads: usize) -> (Vec<u8>, Vec<u8>) {
    let mut cfg = EncoderConfig::new(320, 240);
    cfg.rate_control = RateControl::ConstantQp(30);
    cfg.keyint = keyint;
    cfg.keep_recon = true;
    cfg.threads = threads;
    let enc = Encoder::new(cfg).expect("320x240 encoder");
    let coded = enc.encode_sequence(frames).expect("sequence encodes");
    let mut stream = Vec::new();
    let mut recon = Vec::new();
    for pic in &coded {
        stream.extend_from_slice(&pic.au);
        if let Some(r) = &pic.recon {
            for p in &r.planes {
                recon.extend_from_slice(&p.data);
            }
        }
    }
    (stream, recon)
}

/// Encode `frames` at 1 thread, then at 2, 4 and 6 threads, and require the same
/// access units and reconstruction every time.
fn assert_identical_across_threads(frames: &[VideoFrame], keyint: usize, label: &str) {
    let (one, recon_one) = encode_at(frames, keyint, 1);
    for threads in [2, 4, 6] {
        let (many, recon_many) = encode_at(frames, keyint, threads);
        assert_eq!(
            one.len(),
            many.len(),
            "{label}, {threads} threads: stream length differs from 1 thread"
        );
        assert!(
            one == many,
            "{label}, {threads} threads: stream bytes differ from 1 thread"
        );
        assert!(
            recon_one == recon_many,
            "{label}, {threads} threads: reconstruction differs from 1 thread"
        );
    }
}

#[test]
fn wavefront_intra_pictures_are_identical_at_1_2_4_and_6_threads() {
    // keyint 1: every picture is an IDR, so only the intra wavefront runs.
    let frames = moving_clip(4, 320, 240);
    assert_identical_across_threads(&frames, 1, "intra-only");
}

#[test]
fn wavefront_p_pictures_are_identical_at_1_2_4_and_6_threads() {
    // keyint 4: IDR pictures plus P pictures, so both wavefronts run.
    let frames = moving_clip(9, 320, 240);
    assert_identical_across_threads(&frames, 4, "IDR+P");
}

#[test]
fn wavefront_p_stream_decodes_to_the_encoder_reconstruction() {
    let frames = moving_clip(9, 320, 240);
    let (stream, recon) = encode_at(&frames, 4, 4);
    let mut dec = Decoder::new();
    dec.decode_annex_b(&stream).expect("the stream decodes");
    dec.flush().expect("the stream flushes");
    let mut decoded = Vec::new();
    while let Some(frame) = dec.next_frame() {
        for plane in [&frame.picture.y, &frame.picture.cb, &frame.picture.cr] {
            decoded.extend(plane.iter().map(|&v| v as u8));
        }
    }
    assert_eq!(decoded.len(), recon.len(), "decoded picture count differs");
    assert!(
        decoded == recon,
        "decoder output differs from the encoder reconstruction"
    );
}
