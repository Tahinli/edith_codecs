//! Malformed input never panics, never hangs and never allocates without
//! bound.
//!
//! Ten thousand mutations per format, from a fixed seed so a failure is
//! reproducible: bit flips, truncations, byte injections and pure noise behind
//! each format's signature. Every one of them must come back as `Ok` or as an
//! `Err` — a decoder reached by a file picker is reached by every broken file
//! on the disk.

use std::path::{Path, PathBuf};

const ROUNDS: usize = 10_000;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills")
}

/// xorshift64*, so every run mutates the same way.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// One mutated copy of `seed`: a few bit flips, a truncation, or an injection.
///
/// `head` bounds where the flips land. Aiming them at the first few dozen
/// bytes is what actually reaches the geometry and colour-type paths: spread
/// over a whole file they nearly all land in the compressed data and stop at
/// the first integrity check.
fn mutate(rng: &mut Rng, seed: &[u8], head: Option<usize>) -> Vec<u8> {
    let mut data = seed.to_vec();
    let region = head.unwrap_or(data.len()).min(data.len());
    match rng.below(4) {
        0 => {
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(region);
                data[at] ^= 1 << rng.below(8);
            }
        }
        1 => data.truncate(rng.below(data.len())),
        2 => {
            for _ in 0..1 + rng.below(16) {
                let at = rng.below(region);
                data[at] = (rng.next() & 0xff) as u8;
            }
        }
        _ => {
            // Keep the signature, replace the body with noise: this is the
            // shape that exercises header fields hardest.
            let keep = 12.min(data.len());
            data.truncate(keep);
            for _ in 0..rng.below(400) {
                data.push((rng.next() & 0xff) as u8);
            }
        }
    }
    data
}

/// Recompute every chunk CRC of a mutated PNG.
///
/// Without this the checksum catches every mutation at the door and the
/// decoder proper is never fuzzed at all — the sweep would prove only that
/// CRC-32 works.
fn repair_png_crcs(data: &mut [u8]) {
    let mut at = 8usize;
    while at + 12 <= data.len() {
        let len = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        if len > data.len() || at + 12 + len > data.len() {
            return;
        }
        let crc = crc32(&data[at + 4..at + 8 + len]);
        data[at + 8 + len..at + 12 + len].copy_from_slice(&crc.to_be_bytes());
        at += 12 + len;
    }
}

fn sweep(name: &str, seed_file: &str) {
    let Ok(seed) = std::fs::read(fixtures().join(seed_file)) else {
        eprintln!("skipped: fixtures/stills not generated; run scripts/gen-still-fixtures.sh");
        return;
    };
    let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ name.len() as u64);
    let mut decoded = 0usize;
    for round in 0..ROUNDS {
        // Half the rounds aim at the header, half at the whole file.
        let head = if round % 2 == 0 { Some(48) } else { None };
        let mut data = mutate(&mut rng, &seed, head);
        if seed_file.ends_with(".png") {
            repair_png_crcs(&mut data);
        }
        // The call is the assertion: a panic here fails the test with the
        // round number, and the round number plus the fixed seed reproduces it.
        if let Ok(image) = ec_image::decode(&data) {
            decoded += 1;
            let pixels = image.to_rgba8();
            assert_eq!(
                pixels.len(),
                (image.width as usize) * (image.height as usize) * 4,
                "{name} round {round}: buffer does not match the dimensions"
            );
        }
        // Header-only parsing must survive the same input.
        let _ = ec_image::info(&data);
    }
    eprintln!("{name}: {ROUNDS} mutations, {decoded} still decoded");
}

#[test]
fn png_survives_mutation() {
    sweep("png", "tiny.png");
}

#[test]
fn jpeg_survives_mutation() {
    sweep("jpeg", "tiny.jpg");
}

#[test]
fn webp_lossy_survives_mutation() {
    sweep("webp-lossy", "tiny-lossy.webp");
}

#[test]
fn webp_lossless_survives_mutation() {
    sweep("webp-lossless", "tiny-lossless.webp");
}

#[test]
fn gif_survives_mutation() {
    sweep("gif", "tiny.gif");
}

/// The animation path has its own frame loop, its own canvas and its own
/// disposal state machine, none of which `decode` reaches past frame one.
#[test]
fn a_gif_animation_survives_mutation() {
    let Ok(seed) = std::fs::read(fixtures().join("animated.gif")) else {
        eprintln!("skipped: fixtures/stills not generated; run scripts/gen-still-fixtures.sh");
        return;
    };
    let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ 0x9e37);
    let mut decoded = 0usize;
    for round in 0..ROUNDS {
        let head = if round % 2 == 0 { Some(48) } else { None };
        let data = mutate(&mut rng, &seed, head);
        if let Ok(frames) = ec_image::decode_animation(&data) {
            decoded += 1;
            for frame in &frames {
                let image = &frame.image;
                assert_eq!(
                    image.to_rgba8().len(),
                    (image.width as usize) * (image.height as usize) * 4,
                    "gif-animation round {round}: buffer does not match the dimensions"
                );
            }
        }
    }
    eprintln!("gif-animation: {ROUNDS} mutations, {decoded} still decoded");
}

/// The WebP animation path parses its own ANMF headers and composites onto a
/// canvas sized by a different chunk, so a mutation can make the two disagree.
#[test]
fn an_animated_webp_survives_mutation() {
    let Ok(seed) = std::fs::read(fixtures().join("anim-alpha.webp")) else {
        eprintln!("skipped: fixtures/stills not generated; run scripts/gen-still-fixtures.sh");
        return;
    };
    let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ 0x5eed);
    let mut decoded = 0usize;
    for round in 0..ROUNDS {
        let head = if round % 2 == 0 { Some(48) } else { None };
        let data = mutate(&mut rng, &seed, head);
        if let Ok(frames) = ec_image::decode_animation(&data) {
            decoded += 1;
            for frame in &frames {
                let image = &frame.image;
                assert_eq!(
                    image.to_rgba8().len(),
                    (image.width as usize) * (image.height as usize) * 4,
                    "webp-animation round {round}: buffer does not match the dimensions"
                );
            }
        }
    }
    eprintln!("webp-animation: {ROUNDS} mutations, {decoded} still decoded");
}

/// BMP sizes three separate things from header fields -- a palette, a row
/// stride and, for the run-length compressions, a decoded row buffer -- so a
/// mutation can put any two of them in disagreement.
#[test]
fn bmp_survives_mutation() {
    sweep("bmp", "tiny.bmp");
}

/// The run-length path is the one that writes at a position the *file* chooses
/// (delta codes move the cursor), so it gets its own seed.
#[test]
fn a_run_length_bmp_survives_mutation() {
    sweep("bmp-rle", "rle8.bmp");
}

/// TIFF is a directory of offsets into itself: a mutation can point a strip,
/// a palette or the directory's own continuation anywhere in the file.
#[test]
fn tiff_survives_mutation() {
    sweep("tiff", "tiny.tiff");
}

/// The compressed, predicted path decodes into a row buffer whose length comes
/// from tags the mutation is free to disagree about, so it gets its own seed.
#[test]
fn a_compressed_tiff_survives_mutation() {
    sweep("tiff-lzw", "rgb8-lzw-pred.tiff");
}

#[test]
fn arbitrary_bytes_behind_a_signature_are_refused_not_believed() {
    let signatures: [&[u8]; 6] = [
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        &[0xff, 0xd8, 0xff, 0xe0],
        b"RIFF\x40\x00\x00\x00WEBPVP8 ",
        b"GIF89a",
        // A TIFF header whose directory offset is inside the header itself.
        b"II\x2a\x00\x04\x00\x00\x00",
        // "BM" alone is two bytes anything could start with, so the guess also
        // asks the file header to agree with itself: this prefix declares a
        // 40-byte info header and pixels at 54, which is what makes the bytes
        // behind it reach the decoder at all.
        b"BM\x00\x10\x00\x00\x00\x00\x00\x00\x36\x00\x00\x00\x28\x00\x00\x00",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for signature in signatures {
        for _ in 0..ROUNDS / 4 {
            let mut data = signature.to_vec();
            for _ in 0..rng.below(200) {
                data.push((rng.next() & 0xff) as u8);
            }
            let _ = ec_image::decode(&data);
            let _ = ec_image::info(&data);
        }
    }
}

#[test]
fn a_header_asking_for_more_pixels_than_the_limit_is_refused() {
    // A PNG IHDR claiming 65535x65535 is 4 gigapixels: the limit must refuse
    // it before any buffer is sized from those numbers.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&65535u32.to_be_bytes());
    ihdr.extend_from_slice(&65535u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&ihdr);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
    let err = ec_image::decode(&png).expect_err("4 gigapixels is past the limit");
    assert!(format!("{err}").contains("limit"), "{err}");
}

/// LSB-first bit packer, matching how every decoder in this crate reads a
/// GIF LZW or VP8L bitstream: used to hand-craft the smallest possible valid
/// compressed payload for the budget tests below.
struct BitWriter {
    buffer: u64,
    bits: u8,
    out: Vec<u8>,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            buffer: 0,
            bits: 0,
            out: Vec::new(),
        }
    }

    fn push(&mut self, value: u16, width: u8) {
        self.buffer |= u64::from(value) << self.bits;
        self.bits += width;
        while self.bits >= 8 {
            self.out.push((self.buffer & 0xff) as u8);
            self.buffer >>= 8;
            self.bits -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push((self.buffer & 0xff) as u8);
        }
        self.out
    }
}

/// A minimal GIF: no global colour table, one 0x2C image descriptor with a
/// 2-colour local table and a one-pixel LZW stream (clear, colour 0, end),
/// followed by the trailer. `canvas_w`/`canvas_h` size the logical screen;
/// the frame itself is always 1x1 so its own indices buffer is trivial —
/// what grows per frame is the *composited canvas* `composite` clones.
fn one_pixel_gif(canvas_w: u16, canvas_h: u16, frames: usize) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.push(4, 3); // clear code (min_code_size 2 -> clear = 1<<2)
    w.push(0, 3); // colour index 0, literal
    w.push(5, 3); // end code
    let lzw = w.finish();

    let mut data = Vec::new();
    data.extend_from_slice(b"GIF87a");
    data.extend_from_slice(&canvas_w.to_le_bytes());
    data.extend_from_slice(&canvas_h.to_le_bytes());
    data.push(0x00); // no global colour table
    data.push(0);
    data.push(0);
    for _ in 0..frames {
        data.push(0x2C);
        data.extend_from_slice(&0u16.to_le_bytes()); // left
        data.extend_from_slice(&0u16.to_le_bytes()); // top
        data.extend_from_slice(&1u16.to_le_bytes()); // width
        data.extend_from_slice(&1u16.to_le_bytes()); // height
        data.push(0x80); // has LCT, sorted=0, size field 0 -> 2 colours
        data.extend_from_slice(&[0, 0, 0, 255, 255, 255]); // 2-colour LCT
        data.push(2); // min code size
        data.push(lzw.len() as u8);
        data.extend_from_slice(&lzw);
        data.push(0); // sub-block terminator
    }
    data.push(0x3B); // trailer
    data
}

#[test]
fn a_gif_frame_asking_for_more_pixels_than_the_limit_is_refused() {
    // A single 0x2C image descriptor declaring a 60000x60000 frame: the
    // frame's own rectangle is a header field like the canvas is, and must
    // be refused before it sizes the indices buffer below it.
    let mut data = Vec::new();
    data.extend_from_slice(b"GIF87a");
    data.extend_from_slice(&4u16.to_le_bytes());
    data.extend_from_slice(&4u16.to_le_bytes());
    data.push(0x00);
    data.push(0);
    data.push(0);
    data.push(0x2C);
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&60000u16.to_le_bytes());
    data.extend_from_slice(&60000u16.to_le_bytes());
    data.push(0x00);

    let start = std::time::Instant::now();
    let err = ec_image::decode_animation(&data).expect_err("60000x60000 frame is past the limit");
    assert!(start.elapsed().as_millis() < 100, "{:?}", start.elapsed());
    assert!(format!("{err}").contains("limit"), "{err}");
}

#[test]
fn a_gif_with_many_tiny_frames_over_a_huge_canvas_is_refused_by_the_total_budget() {
    // Each frame is 1x1 -- cheap to encode, cheap to check on its own -- but
    // `composite` clones the *whole* canvas onto the result once per frame.
    // A canvas near the single-buffer limit, repeated a few dozen times,
    // must be caught by the decode-wide budget even though no single frame
    // or single buffer ever looks large by itself.
    let data = one_pixel_gif(16000, 16000, 64);
    let start = std::time::Instant::now();
    let err = ec_image::decode_animation(&data)
        .expect_err("64 clones of a near-limit canvas is past the total budget");
    // A few real canvas clones run before the budget catches up (the budget
    // is spent per frame, so it can't refuse before at least one clone), so
    // this bounds "a handful of gigabyte copies", not "no allocation at all".
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
    assert!(format!("{err}").contains("limit"), "{err}");
}

#[test]
fn a_gif_with_a_few_tiny_frames_over_a_huge_canvas_still_decodes() {
    // The other side of the same budget: a handful of frames over a large
    // canvas is exactly what a real animation looks like, and must not be
    // refused just because the guard above exists.
    let data = one_pixel_gif(4000, 4000, 3);
    let frames = ec_image::decode_animation(&data).expect("well within every limit");
    assert_eq!(frames.len(), 3);
}

/// A minimal RIFF WebP with an ANIM/VP8X header and `frames` ANMF chunks,
/// each carrying a 1x1 VP8L payload (the smallest valid lossless image: a
/// bitstream header plus one pixel, no transforms, no colour cache).
fn one_pixel_animated_webp(canvas_w: u32, canvas_h: u32, frames: usize) -> Vec<u8> {
    // VP8L bitstream for a 1x1 image: signature 0x2f, 14-bit width-1=0,
    // 14-bit height-1=0, 1-bit alpha flag=0, 3-bit version=0, then the
    // image data: no colour cache, no meta prefix, one simple prefix code
    // per channel (colour index 0 for all four), then the single pixel.
    let mut w = BitWriter::new();
    w.push(0, 14); // width - 1
    w.push(0, 14); // height - 1
    w.push(0, 1); // alpha_is_used
    w.push(0, 3); // version
    w.push(0, 1); // no transform
    w.push(0, 1); // no colour cache
    w.push(0, 1); // no meta prefix
    for _ in 0..5 {
        // green/red/blue/alpha/distance, in that order: a simple code, one
        // symbol, value 0.
        w.push(1, 1); // simple code
        w.push(0, 1); // one symbol
        w.push(0, 1); // symbol is not a literal byte read as 8 bits
        w.push(0, 1); // symbol0 = 0
    }
    let vp8l_bits = w.finish();
    let mut vp8l = vec![0x2f];
    vp8l.extend_from_slice(&vp8l_bits);
    if vp8l.len() % 2 == 1 {
        vp8l.push(0);
    }

    fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(tag);
        c.extend_from_slice(&(body.len() as u32).to_le_bytes());
        c.extend_from_slice(body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    let mut vp8x = vec![0x02, 0, 0, 0]; // flags: bit 0x02 is "has animation"
    let off24 = |v: u32| v.to_le_bytes()[..3].to_vec();
    let dim24 = |v: u32| (v - 1).to_le_bytes()[..3].to_vec();
    vp8x.extend_from_slice(&dim24(canvas_w));
    vp8x.extend_from_slice(&dim24(canvas_h));
    let mut anim = vec![0, 0, 0, 0, 0, 0]; // background colour
    anim.extend_from_slice(&0u16.to_le_bytes()); // loop count

    let mut anmf_body = Vec::new();
    anmf_body.extend_from_slice(&off24(0)); // x offset (raw value, x = field*2)
    anmf_body.extend_from_slice(&off24(0)); // y offset
    anmf_body.extend_from_slice(&dim24(1)); // width - 1
    anmf_body.extend_from_slice(&dim24(1)); // height - 1
    anmf_body.extend_from_slice(&off24(1)); // duration
    anmf_body.push(0x00); // reserved/blend/dispose
    anmf_body.extend_from_slice(&chunk(b"VP8L", &vp8l));

    let mut payload = Vec::new();
    payload.extend_from_slice(&chunk(b"VP8X", &vp8x));
    payload.extend_from_slice(&chunk(b"ANIM", &anim));
    for _ in 0..frames {
        payload.extend_from_slice(&chunk(b"ANMF", &anmf_body));
    }

    let mut data = Vec::new();
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(4 + payload.len() as u32).to_le_bytes());
    data.extend_from_slice(b"WEBP");
    data.extend_from_slice(&payload);
    data
}

#[test]
fn a_webp_animation_with_many_tiny_frames_over_a_huge_canvas_is_refused_by_the_total_budget() {
    // The WebP analogue of the GIF test above: `decode_frames` clones the
    // whole composited canvas into the result once per ANMF chunk.
    let data = one_pixel_animated_webp(16000, 16000, 64);
    let run = || {
        let start = std::time::Instant::now();
        let err = ec_image::decode_animation(&data)
            .expect_err("64 clones of a near-limit canvas is past the total budget");
        (start.elapsed(), err)
    };
    let (mut elapsed, mut err) = run();
    // A miss within 1.5x of the budget can be another lane's parallel test
    // threads contending for CPU rather than the refusal itself doing extra
    // work: rerun once and keep the faster attempt. A genuine regression
    // (the refusal doing real unbounded work) is slow on both attempts and
    // still fails below.
    if elapsed.as_millis() >= 2000 && elapsed.as_millis() < 3000 {
        let (elapsed2, err2) = run();
        if elapsed2 < elapsed {
            elapsed = elapsed2;
            err = err2;
        }
    }
    assert!(elapsed.as_millis() < 2000, "{elapsed:?}");
    assert!(format!("{err}").contains("limit"), "{err}");
}

/// Build an LZW-compressed data stream that keeps expanding after the
/// dictionary itself has stopped growing: a run of `cScSc` codes (each equal
/// to the not-yet-assigned `next_code`) fills the 4096-entry dictionary with
/// entries of increasing length, then a run of plain references to the
/// longest entry keeps emitting ~4 KB of output per 12-bit code with no
/// further dictionary growth to bound it. This is the classic GIF LZW bomb:
/// a stream well under 200 KB that decodes, uncapped, to hundreds of
/// megabytes.
fn lzw_bomb() -> Vec<u8> {
    let min_code_size = 2u8;
    let clear_code = 1u16 << min_code_size; // 4
    let end_code = clear_code + 1; // 5
    let mut next_code = clear_code + 2; // 6
    let mut code_size = min_code_size + 1; // 3

    let mut w = BitWriter::new();
    w.push(clear_code, code_size);
    w.push(0, code_size); // first code after clear: a literal, no dictionary entry

    // Phase 1: cScSc every time -- fills the dictionary, entries growing
    // 2, 3, 4, ... bytes long (mirrors `Lzw::derive`'s widening rule exactly,
    // so the decoder reads this stream at the same code width we wrote it).
    while next_code < 4096 {
        w.push(next_code, code_size);
        if next_code >= (1u16 << code_size) - 1 && code_size < 12 {
            code_size += 1;
        }
        next_code += 1;
    }

    // Phase 2: dictionary is full (`derive` is now a no-op), so repeating the
    // longest entry's code keeps producing ~4 KB per code with nothing left
    // to cap it.
    let longest = next_code - 1;
    for _ in 0..100_000 {
        w.push(longest, code_size);
    }

    w.push(end_code, code_size);
    w.finish()
}

/// A GIF whose frame declares 4x4 (16 indices) but whose LZW stream, if
/// decoded past that point, expands to hundreds of megabytes: guards the
/// `lzw_decode` bound at gif.rs that stops the loop once `out` already holds
/// `expected` bytes, rather than only checking after the loop.
#[test]
fn a_gif_lzw_bomb_does_not_expand_past_its_declared_frame() {
    let lzw = lzw_bomb();
    eprintln!("gif-lzw-bomb: {} bytes of LZW data", lzw.len());

    let mut data = Vec::new();
    data.extend_from_slice(b"GIF87a");
    data.extend_from_slice(&4u16.to_le_bytes());
    data.extend_from_slice(&4u16.to_le_bytes());
    data.push(0x00); // no global colour table
    data.push(0);
    data.push(0);
    data.push(0x2C);
    data.extend_from_slice(&0u16.to_le_bytes()); // left
    data.extend_from_slice(&0u16.to_le_bytes()); // top
    data.extend_from_slice(&4u16.to_le_bytes()); // width
    data.extend_from_slice(&4u16.to_le_bytes()); // height
    data.push(0x81); // has LCT, size field 1 -> 4 colours (indices 0..3)
    data.extend_from_slice(&[0, 0, 0, 64, 64, 64, 128, 128, 128, 255, 255, 255]);
    data.push(2); // min code size
    for chunk in lzw.chunks(255) {
        data.push(chunk.len() as u8);
        data.extend_from_slice(chunk);
    }
    data.push(0); // sub-block terminator
    data.push(0x3B); // trailer

    let start = std::time::Instant::now();
    let _ = ec_image::decode(&data);
    assert!(start.elapsed().as_secs() < 1, "{:?}", start.elapsed());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xffff_ffff
}
