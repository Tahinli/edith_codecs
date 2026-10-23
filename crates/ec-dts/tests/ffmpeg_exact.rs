#![allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of)]
//! Sample-for-sample equality with ffmpeg's DTS decoder (integer path,
//! `-flags +bitexact`) on the committed fixtures in `fixtures/`, and the
//! stream-layout, header and DTS-HD behaviours that follow from them.
//!
//! The fixtures are made with ffmpeg's own DTS encoder (`-c:a dca -strict
//! -2`): stereo and mono tones, a mono sweep at 32 kHz, 5.1 tones at 44.1
//! kHz and 5.1 noise with ADPCM at 48 kHz. Each is decoded here and by
//! ffmpeg and every 24-bit sample must be equal. ffmpeg is test tooling and
//! is driven through `std::process`; without it the comparison skips loudly.
//! The fixtures themselves are required: a missing one fails.
//!
//! The 14-bit and little-endian stream layouts are derived from the same
//! fixtures here and must decode to identical samples.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Decoder, Error};
use ec_dts::{DtsDecoder, Options, StreamFormat};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn fixture_names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(fixtures())
        .expect("fixtures dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".dts"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no DTS fixtures");
    v
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Split a BE16 elementary stream into its core frames.
fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let Some((s, _)) = ec_dts::find_sync(&data[pos..]) else {
            break;
        };
        pos += s;
        let Ok(size) = ec_dts::frame_size(&data[pos..]) else {
            break;
        };
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

/// Decode a sequence of frames to interleaved 24-bit integers.
fn decode_ours(frames: &[Vec<u8>]) -> (Vec<i32>, usize, u32) {
    let mut dec = DtsDecoder::new();
    let mut out = Vec::new();
    let mut channels = 0;
    let mut rate = 0;
    for f in frames {
        let a = dec.decode_frame(f).expect("decode");
        channels = a.layout.channel_count();
        rate = a.rate;
        for c in a.data[0].chunks_exact(4) {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            out.push((v as f64 * 8388608.0).round() as i32);
        }
    }
    (out, channels, rate)
}

fn decode_ffmpeg(path: &Path) -> Vec<i32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-flags", "+bitexact", "-i"])
        .arg(path)
        .args(["-f", "s32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 8)
        .collect()
}

/// Re-pack canonical frames into another stream layout.
fn to_layout(frame: &[u8], format: StreamFormat) -> Vec<u8> {
    match format {
        StreamFormat::Be16 => frame.to_vec(),
        StreamFormat::Le16 => {
            let mut v = frame.to_vec();
            for c in v.chunks_exact_mut(2) {
                c.swap(0, 1);
            }
            v
        }
        StreamFormat::Be14 | StreamFormat::Le14 => {
            let mut out = Vec::new();
            let mut acc: u32 = 0;
            let mut n = 0u32;
            let push = |w14: u16, out: &mut Vec<u8>| {
                // The two top bits of each word extend the 14-bit value's sign.
                let w = if w14 & 0x2000 != 0 { w14 | 0xC000 } else { w14 };
                if format == StreamFormat::Be14 {
                    out.extend_from_slice(&w.to_be_bytes());
                } else {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            };
            for &b in frame {
                acc = (acc << 8) | b as u32;
                n += 8;
                while n >= 14 {
                    push(((acc >> (n - 14)) & 0x3FFF) as u16, &mut out);
                    n -= 14;
                }
                acc &= (1 << n) - 1;
            }
            if n > 0 {
                push(((acc << (14 - n)) & 0x3FFF) as u16, &mut out);
            }
            out
        }
    }
}

#[test]
fn fixtures_are_bit_exact_against_ffmpeg() {
    let ff = have_ffmpeg();
    if !ff {
        eprintln!("skip comparison: ffmpeg not on PATH");
    }
    for name in fixture_names() {
        let path = fixtures().join(&name);
        let data = std::fs::read(&path).unwrap();
        let fr: Vec<Vec<u8>> = frames(&data).iter().map(|f| f.to_vec()).collect();
        assert!(fr.len() > 5, "{name}: frames");
        let (ours, channels, rate) = decode_ours(&fr);
        assert!(!ours.is_empty());
        let peak = ours.iter().map(|v| v.abs()).max().unwrap();
        assert!(peak > 1000, "{name}: decoded silence (peak {peak})");
        if ff {
            let reference = decode_ffmpeg(&path);
            assert_eq!(ours.len(), reference.len(), "{name}: sample count");
            let bad = ours.iter().zip(&reference).position(|(a, b)| a != b);
            assert_eq!(
                bad,
                None,
                "{name}: first mismatch at sample {bad:?} (channel {:?})",
                bad.map(|i| i % channels)
            );
            eprintln!(
                "{name}: {channels} ch, {rate} Hz, {} samples/ch, bit-exact vs ffmpeg",
                ours.len() / channels
            );
        }
    }
}

#[test]
fn stream_layouts_decode_identically() {
    let ff = have_ffmpeg();
    let tmp = std::env::temp_dir().join(format!("ec-dts-layouts-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    for name in fixture_names() {
        let data = std::fs::read(fixtures().join(&name)).unwrap();
        let base: Vec<Vec<u8>> = frames(&data).iter().map(|f| f.to_vec()).collect();
        let (want, _, _) = decode_ours(&base);
        for fmt in [StreamFormat::Le16, StreamFormat::Be14, StreamFormat::Le14] {
            let conv: Vec<Vec<u8>> = base.iter().map(|f| to_layout(f, fmt)).collect();
            // Header API sees through the layout.
            let h0 = ec_dts::parse_frame_header(&base[0]).unwrap();
            let h1 = ec_dts::parse_frame_header(&conv[0]).unwrap();
            assert_eq!(h1.format, fmt, "{name} {fmt:?}");
            assert_eq!(
                (h0.sample_rate, h0.channels, h0.samples_per_frame, h0.frame_size),
                (h1.sample_rate, h1.channels, h1.samples_per_frame, h1.frame_size),
                "{name} {fmt:?}"
            );
            assert_eq!(conv[0].len(), h1.stream_frame_size, "{name} {fmt:?}: stored size");
            let (got, _, _) = decode_ours(&conv);
            assert_eq!(got, want, "{name} {fmt:?}: samples differ from BE16");
            if ff {
                let p = tmp.join(format!("{name}.{fmt:?}.dts"));
                std::fs::write(&p, conv.concat()).unwrap();
                let reference = decode_ffmpeg(&p);
                let n = want.len().min(reference.len());
                assert!(n > want.len() / 2, "{name} {fmt:?}: ffmpeg decoded too little");
                assert_eq!(&reference[..n], &want[..n], "{name} {fmt:?}: vs ffmpeg");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn header_api_states_the_stream() {
    let expect = [
        ("noise-51-adpcm-48k.dts", 48_000, 6, 1),
        ("sweep-mono-32k.dts", 32_000, 1, 0),
        ("tone-stereo-48k.dts", 48_000, 2, 1),
        ("tones-51-44k.dts", 44_100, 6, 1),
    ];
    for (name, rate, channels, lfe) in expect {
        let data = std::fs::read(fixtures().join(name)).unwrap();
        let h = ec_dts::parse_frame_header(&data).unwrap();
        assert_eq!(h.sample_rate, rate, "{name}");
        assert_eq!(h.channels as usize, channels, "{name}");
        assert_eq!(h.samples_per_frame % 256, 0, "{name}");
        assert_eq!(h.format, StreamFormat::Be16);
        assert_eq!(ec_dts::frame_size(&data).unwrap(), h.frame_size);
        let _ = lfe;
        // A frame is exactly followed by the next sync word.
        let next = &data[h.frame_size..];
        assert_eq!(&next[..4], &data[..4], "{name}: next frame");
    }
    // Probe behaviour on junk and short input.
    assert!(matches!(ec_dts::parse_frame_header(&[0x7F, 0xFE]), Err(Error::NeedMore)));
    assert!(matches!(
        ec_dts::parse_frame_header(&[0u8; 32]),
        Err(Error::Corrupt { .. })
    ));
}

/// MSB-first bit writer for the hand-built DTS-HD substream.
struct Bw {
    bytes: Vec<u8>,
    nbits: usize,
}

impl Bw {
    fn new() -> Bw {
        Bw {
            bytes: Vec::new(),
            nbits: 0,
        }
    }
    fn put(&mut self, v: u64, n: usize) {
        for i in (0..n).rev() {
            if self.nbits % 8 == 0 {
                self.bytes.push(0);
            }
            let bit = ((v >> i) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= bit << (7 - self.nbits % 8);
            self.nbits += 1;
        }
    }
    fn align(&mut self) {
        self.nbits = self.bytes.len() * 8;
    }
}

/// An extension substream with one asset whose only component is XLL, built
/// by hand from the header syntax (no static fields, no metadata).
fn exss_with_xll(payload: usize) -> Vec<u8> {
    // Asset descriptor: size 9, index 3, drc 1, dialnorm 1, coding mode 2,
    // xll size 16, xll sync present 1, hd stream id 3 -> 36 bits, 5 bytes.
    let mut d = Bw::new();
    d.put(5 - 1, 9);
    d.put(0, 3);
    d.put(0, 1);
    d.put(0, 1);
    d.put(1, 2); // coding mode 1: lossless without a core component
    d.put(payload as u64 - 1, 16);
    d.put(0, 1);
    d.put(0, 3);
    d.align();
    assert_eq!(d.bytes.len(), 5);
    // Header: sync 32, user 8, index 2, wide 1, hdr size 8, exss size 16,
    // static 1, asset size 16, descriptor, crc 16.
    let hdr_bits: usize = 32 + 8 + 2 + 1 + 8 + 16 + 1 + 16 + 40;
    let hdr_bytes = (hdr_bits + 16).div_ceil(8);
    let total = hdr_bytes + payload;
    let mut w = Bw::new();
    w.put(0x6458_2025, 32);
    w.put(0, 8);
    w.put(0, 2);
    w.put(0, 1);
    w.put(hdr_bytes as u64 - 1, 8);
    w.put(total as u64 - 1, 16);
    w.put(0, 1);
    w.put(payload as u64 - 1, 16);
    for b in &d.bytes {
        w.put(*b as u64, 8);
    }
    w.align();
    w.put(0, 16);
    let mut v = w.bytes;
    assert_eq!(v.len(), hdr_bytes);
    v.extend((0..payload).map(|i| (i * 37) as u8));
    v
}

#[test]
fn dts_hd_substream_is_parsed_and_the_core_is_output() {
    let data = std::fs::read(fixtures().join("tones-51-44k.dts")).unwrap();
    let fr = frames(&data);
    let core = fr[3];
    let hd = exss_with_xll(300);

    let info = ec_dts::exss::parse_exss(&hd).unwrap();
    assert_eq!(info.size, hd.len());
    assert!(info.has_xll());
    assert_eq!(info.assets[0].xll_size, 300);
    assert_eq!(info.assets[0].xll_offset, hd.len() - 300);

    // Core alone vs core with the HD substream behind it (4-byte aligned).
    let mut plain = DtsDecoder::new();
    plain.decode_frame(fr[2]).unwrap();
    let want = plain.decode_frame(core).unwrap();

    let mut with = core.to_vec();
    while with.len() % 4 != 0 {
        with.push(0);
    }
    with.extend_from_slice(&hd);
    let mut dec = DtsDecoder::new();
    let mut prev = fr[2].to_vec();
    while prev.len() % 4 != 0 {
        prev.push(0);
    }
    prev.extend_from_slice(&hd);
    dec.decode_frame(&prev).unwrap();
    let got = dec.decode_frame(&with).unwrap();
    assert_eq!(got.data[0].len(), want.data[0].len());
    assert!(got.data[0] == want.data[0], "HD frame must give the core samples");
    let i = dec.frame_info().unwrap();
    assert!(i.hd_substream && i.xll, "XLL flagged: {i:?}");

    // The strict option names the refusal.
    let mut strict = DtsDecoder::with_options(Options { refuse_xll: true });
    match strict.decode_frame(&with) {
        Err(Error::Unsupported { what, .. }) => assert!(what.contains("XLL"), "{what}"),
        other => panic!("expected an XLL refusal, got {other:?}"),
    }
}

#[test]
fn damaged_streams_never_panic() {
    let data = std::fs::read(fixtures().join("noise-51-adpcm-48k.dts")).unwrap();
    let fr = frames(&data);
    let mut seed = 0x1234_5678u32;
    let mut next = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 8
    };
    let mut dec = DtsDecoder::new();
    let mut errors = 0;
    for round in 0..400 {
        let mut f = fr[round % fr.len()].to_vec();
        let flips = 1 + next() % 8;
        for _ in 0..flips {
            let at = (next() as usize) % f.len();
            f[at] ^= 1 << (next() % 8);
        }
        if round % 7 == 0 {
            f.truncate((next() as usize) % f.len());
        }
        if dec.decode_frame(&f).is_err() {
            errors += 1;
        }
    }
    assert!(errors > 0, "corruption should be noticed at least sometimes");
    // And the decoder still works on a clean frame afterwards.
    dec.reset();
    dec.decode_frame(fr[0]).unwrap();
}
