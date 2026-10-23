//! Layer I/II against ffmpeg's float decoders.
//!
//! Two kinds of stream: real ones from ffmpeg's `mp2` / `mp2fixed` encoders
//! (synthetic sources only: sine, noise, chirp), and random-payload frames with
//! legal headers, which reach every allocation code, every scalefactor-select
//! mode, the joint-stereo bound, CRC-protected frames and Layer I -- things no
//! encoder here produces. The oracle is `ffmpeg -c:a mp2 -i x.mp2 -f f32le`.
//!
//! ffmpeg is needed; absent, the tests skip (fail under `EC_REQUIRE_FIXTURES=1`).
//!
//!   cargo test -p ec-mp3 --release --test mp2_matrix -- --nocapture

use std::path::PathBuf;
use std::process::Command;

use ec_mp3::{FrameHeader, Mp3Reader};

fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some_and(|v| !v.is_empty() && v != "0")
}

fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            !required(),
            "EC_REQUIRE_FIXTURES is set but ffmpeg is missing"
        );
        eprintln!("skip: ffmpeg not available");
    }
    ok
}

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("ec-mp2-matrix");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg_decode(path: &std::path::Path, channels: usize, layer: u8) -> Vec<f32> {
    let decoder = if layer == 1 { "mp1float" } else { "mp2float" };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", decoder, "-i"])
        .arg(path)
        .args(["-ac", &channels.to_string(), "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// ffmpeg's fixed-point decoder (the default `mp2`/`mp1`): s16 out.
fn ffmpeg_decode_s16(path: &std::path::Path, channels: usize, layer: u8) -> Vec<i16> {
    let decoder = if layer == 1 { "mp1" } else { "mp2" };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", decoder, "-i"])
        .arg(path)
        .args(["-ac", &channels.to_string(), "-f", "s16le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success());
    out.stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// `(max LSB difference, fraction of samples equal)` of our float output
/// rounded to s16 against ffmpeg's fixed-point decoder.
fn s16_agreement(got: &[f32], want: &[i16]) -> (i32, f64) {
    let n = got.len().min(want.len());
    let (mut worst, mut equal) = (0i32, 0usize);
    for i in 0..n {
        let g = (got[i] * 32768.0).round().clamp(-32768.0, 32767.0) as i32;
        let d = (g - i32::from(want[i])).abs();
        worst = worst.max(d);
        equal += usize::from(d == 0);
    }
    (worst, equal as f64 / n.max(1) as f64)
}

fn our_decode(bytes: &[u8]) -> (Vec<f32>, u32, usize) {
    let mut reader = Mp3Reader::new();
    reader.push(bytes);
    let mut out = Vec::new();
    let (mut rate, mut ch) = (0, 0);
    while let Ok(frame) = reader.next_frame() {
        rate = frame.sample_rate;
        ch = frame.channels;
        out.extend_from_slice(&frame.samples);
    }
    (out, rate, ch)
}

fn max_err(a: &[f32], b: &[f32]) -> (f32, usize) {
    let n = a.len().min(b.len());
    let mut worst = (0.0f32, 0usize);
    for i in 0..n {
        let e = (a[i] - b[i]).abs();
        if e > worst.0 {
            worst = (e, i);
        }
    }
    worst
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
}

#[test]
fn encoder_streams_match_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let dir = work_dir();
    let sources = [
        "sine=frequency=1000:sample_rate={R}:duration=1.5",
        "anoisesrc=color=pink:amplitude=0.4:sample_rate={R}:duration=1.5:seed=7",
        "aevalsrc=0.5*sin(2*PI*(100+3000*t)*t):s={R}:d=1.5",
    ];
    let cases: &[(&str, u32, u32, &str)] = &[
        ("mp2", 32000, 1, "32k"),
        ("mp2", 44100, 2, "128k"),
        ("mp2", 48000, 2, "384k"),
        ("mp2", 48000, 1, "64k"),
        ("mp2", 44100, 1, "192k"),
        ("mp2", 44100, 2, "64k"),
        ("mp2", 32000, 2, "96k"),
        ("mp2", 16000, 1, "32k"),
        ("mp2", 22050, 2, "64k"),
        ("mp2", 24000, 2, "128k"),
        ("mp2", 24000, 1, "8k"),
        ("mp2fixed", 44100, 2, "192k"),
        ("mp2fixed", 22050, 1, "48k"),
    ];
    let mut worst_all = 0.0f32;
    for (n, &(codec, rate, ch, br)) in cases.iter().enumerate() {
        let src = sources[n % sources.len()].replace("{R}", &rate.to_string());
        let path = dir.join(format!("enc-{n}.mp2"));
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-ac"])
            .arg(ch.to_string())
            .args(["-c:a", codec, "-b:a", br])
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success(), "ffmpeg encode {codec} {rate} {ch} {br}");
        let bytes = std::fs::read(&path).unwrap();
        let want = ffmpeg_decode(&path, ch as usize, 2);
        let (got, r, c) = our_decode(&bytes);
        assert_eq!((r, c), (rate, ch as usize), "{codec} {rate} {ch} {br}");
        assert_eq!(got.len(), want.len(), "{codec} {rate} {ch} {br}: length");
        let (e, at) = max_err(&got, &want);
        let (lsb, same) = s16_agreement(&got, &ffmpeg_decode_s16(&path, ch as usize, 2));
        eprintln!(
            "{codec} {rate} Hz ch={ch} {br}: max abs error {e:.3e} (at {at}); vs fixed-point s16: max {lsb} LSB, {:.2}% identical",
            same * 100.0
        );
        worst_all = worst_all.max(e);
        assert!(e <= 1e-5, "{codec} {rate} {ch} {br}: max error {e} at {at}");
    }
    eprintln!("worst over the encoder matrix: {worst_all:.3e}");
}

/// MSB-first bit writer for building frames.
struct BitW {
    bytes: Vec<u8>,
    bits: usize,
}
impl BitW {
    fn new() -> BitW {
        BitW {
            bytes: Vec::new(),
            bits: 0,
        }
    }
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

/// Quantiser sizes by allocation code for the five Layer II tables, written
/// out again here so that a transcription slip in the crate shows up.
fn l2_rows(table: usize) -> Vec<Vec<u32>> {
    let a = vec![
        0, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
    ];
    let b = vec![
        0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
    ];
    let c = vec![0, 3, 5, 7, 9, 15, 31, 65535];
    let d = vec![0, 3, 5, 65535];
    let e = vec![
        0, 3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
    ];
    let f = vec![0, 3, 5, 9, 15, 31, 63, 127];
    let g = vec![
        0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
    ];
    let h = vec![0, 3, 5, 9];
    let runs: Vec<(&Vec<u32>, usize)> = match table {
        0 => vec![(&a, 3), (&b, 8), (&c, 12), (&d, 4)],
        1 => vec![(&a, 3), (&b, 8), (&c, 12), (&d, 7)],
        2 => vec![(&e, 2), (&f, 6)],
        3 => vec![(&e, 2), (&f, 10)],
        _ => vec![(&g, 4), (&f, 7), (&h, 19)],
    };
    runs.into_iter()
        .flat_map(|(r, n)| std::iter::repeat_n(r.clone(), n))
        .collect()
}

fn l2_table(h: &FrameHeader) -> usize {
    if h.version != ec_mp3::Version::Mpeg1 {
        return 4;
    }
    let ch = h.bitrate_kbps / h.channels() as u32;
    let r = h.sample_rate;
    if (r == 48000 && ch >= 56) || (56..=80).contains(&ch) {
        0
    } else if r != 48000 && ch >= 96 {
        1
    } else if r != 32000 && ch <= 48 {
        2
    } else {
        3
    }
}

/// A legal Layer I/II frame body with random allocations, scalefactor
/// selectors, scalefactors and samples (never the forbidden codes), as many
/// bands filled as fit. `crc` fills in a correct CRC.
fn structured_frame(rng: &mut Rng, h: &FrameHeader, density: u32) -> Option<Vec<u8>> {
    let channels = h.channels();
    let joint = h.mode == ec_mp3::ChannelMode::JointStereo;
    let mut w = BitW::new();
    let rnd = |rng: &mut Rng, n: u32| (rng.next() >> 20) as u32 % n.max(1);
    let (nb_sb, bound);
    let rows: Vec<Vec<u32>>;
    if h.layer == 1 {
        nb_sb = 32;
        rows = Vec::new();
    } else {
        rows = l2_rows(l2_table(h));
        nb_sb = rows.len();
    }
    bound = if joint {
        (4 + 4 * h.mode_ext as usize).min(nb_sb)
    } else {
        nb_sb
    };
    // allocation codes
    let mut alloc = vec![vec![0u32; nb_sb]; 2];
    let width = |sb: usize| -> u32 {
        if h.layer == 1 {
            4
        } else {
            rows[sb].len().trailing_zeros()
        }
    };
    for sb in 0..nb_sb {
        let maxcode = if h.layer == 1 {
            15
        } else {
            rows[sb].len() as u32
        };
        let pick = |rng: &mut Rng| {
            if rnd(rng, 100) < density {
                1 + rnd(rng, maxcode - 1)
            } else {
                0
            }
        };
        for ch in 0..channels {
            alloc[ch][sb] = if sb < bound || ch == 0 {
                pick(rng)
            } else {
                alloc[0][sb]
            };
        }
        if sb >= bound && channels == 2 {
            alloc[1][sb] = alloc[0][sb];
        }
    }
    for sb in 0..nb_sb {
        if sb < bound {
            for a in alloc.iter().take(channels) {
                w.put(a[sb], width(sb));
            }
        } else {
            w.put(alloc[0][sb], width(sb));
        }
    }
    let mut scfsi = vec![vec![0u32; nb_sb]; 2];
    if h.layer == 2 {
        for sb in 0..nb_sb {
            for ch in 0..channels {
                if alloc[ch][sb] != 0 {
                    scfsi[ch][sb] = rnd(rng, 4);
                    w.put(scfsi[ch][sb], 2);
                }
            }
        }
    }
    let protected_bits = w.bits;
    let protected: Vec<u8> = w.bytes.clone();
    for sb in 0..nb_sb {
        for ch in 0..channels {
            if alloc[ch][sb] == 0 {
                continue;
            }
            let n = if h.layer == 1 {
                1
            } else {
                match scfsi[ch][sb] {
                    0 => 3,
                    2 => 1,
                    _ => 2,
                }
            };
            for _ in 0..n {
                w.put(rnd(rng, 63), 6);
            }
        }
    }
    // Layer I: 12 slots of one sample; Layer II: 12 granules of three (grouped).
    for _ in 0..12 {
        for sb in 0..nb_sb {
            let chs = if sb < bound { channels } else { 1 };
            for ch_alloc in alloc.iter().take(chs) {
                let a = ch_alloc[sb];
                if a == 0 {
                    continue;
                }
                if h.layer == 1 {
                    let nb = a + 1;
                    w.put(rnd(rng, (1 << nb) - 1), nb);
                } else {
                    let l = rows[sb][a as usize];
                    match l {
                        3 | 5 | 9 => {
                            let bits = [0, 0, 0, 5, 0, 7, 0, 0, 0, 10][l as usize];
                            w.put(rnd(rng, l * l * l), bits);
                        }
                        _ => {
                            let nb = 32 - l.leading_zeros();
                            for _ in 0..3 {
                                w.put(rnd(rng, l), nb);
                            }
                        }
                    }
                }
            }
        }
    }
    let len = h.frame_len().unwrap();
    let head = if h.crc { 6 } else { 4 };
    if head + w.bytes.len() > len {
        return None;
    }
    let mut frame = h.to_bytes().to_vec();
    if h.crc {
        let c = ec_mp3::header::crc16_bits(&h.to_bytes(), &protected, protected_bits);
        frame.extend_from_slice(&c.to_be_bytes());
    }
    frame.extend_from_slice(&w.bytes);
    frame.resize(len, 0);
    Some(frame)
}

fn random_header(
    rng: &mut Rng,
    layer: u8,
    version: u8,
    mode: u8,
    ext: u8,
    crc: bool,
) -> FrameHeader {
    let br = 1 + (rng.byte() % 14);
    let sr = rng.byte() % 3;
    let b1 = 0xE0 | (version << 3) | ((4 - layer) << 1) | u8::from(!crc);
    FrameHeader::parse(&[0xFF, b1, (br << 4) | (sr << 2), (mode << 6) | (ext << 4)]).unwrap()
}

fn compare_stream(name: &str, frames: &[Vec<u8>], tolerance: f32) -> f32 {
    let dir = work_dir();
    let mut bytes = Vec::new();
    for f in frames {
        bytes.extend_from_slice(f);
    }
    let path = dir.join(format!("{name}.mp2"));
    std::fs::write(&path, &bytes).unwrap();
    let h0 = FrameHeader::parse(&frames[0]).unwrap();
    let ch = h0.channels();
    let want = ffmpeg_decode(&path, ch, h0.layer);
    let (got, _, _) = our_decode(&bytes);
    assert_eq!(got.len(), want.len(), "{name}: length");
    let (e, at) = max_err(&got, &want);
    let (lsb, same) = s16_agreement(&got, &ffmpeg_decode_s16(&path, ch, h0.layer));
    eprintln!(
        "{name}: {} frames, max abs error {e:.3e}; vs fixed-point s16: max {lsb} LSB, {:.2}% identical",
        frames.len(),
        same * 100.0
    );
    if e > tolerance {
        let per = h0.samples_per_frame() * ch;
        for (i, (g, w)) in got.chunks(per).zip(want.chunks(per)).enumerate() {
            let (fe, fat) = max_err(g, w);
            if fe > tolerance {
                eprintln!(
                    "  frame {i}: error {fe:.3e} at {fat} (ours {}, ffmpeg {}); header {h0:?}",
                    g[fat], w[fat]
                );
                break;
            }
        }
    }
    assert!(e <= tolerance, "{name}: max error {e} at {at}");
    e
}

#[test]
fn random_frames_match_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut worst = 0.0f32;
    let mut streams = 0;
    for layer in [2u8, 1] {
        for version in [3u8, 2, 0] {
            for (mode, ext) in [(3u8, 0u8), (0, 0), (2, 0), (1, 0), (1, 1), (1, 2), (1, 3)] {
                for round in 0..8 {
                    let crc = round % 2 == 1;
                    // A header whose frames can at least hold their own allocation fields.
                    let h = loop {
                        let h = random_header(&mut rng, layer, version, mode, ext, crc);
                        if structured_frame(&mut rng, &h, 0).is_some() {
                            break h;
                        }
                    };
                    let mut frames = Vec::new();
                    while frames.len() < 24 {
                        let density = 5 + (rng.byte() as u32 % 90);
                        let mut d = density;
                        loop {
                            if let Some(f) = structured_frame(&mut rng, &h, d) {
                                frames.push(f);
                                break;
                            }
                            d = (d * 2 / 3).max(1);
                        }
                    }
                    let name = format!("rand-l{layer}-v{version}-m{mode}-e{ext}-r{round}");
                    worst = worst.max(compare_stream(&name, &frames, 4e-5));
                    streams += 1;
                }
            }
        }
    }
    eprintln!("{streams} random streams, worst error {worst:.3e}");
}

fn have_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

/// libtwolame writes what the ffmpeg encoder does not: joint stereo with every
/// intensity bound, dual channel, and CRC-protected frames.
#[test]
fn twolame_streams_match_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    if !have_encoder("libtwolame") {
        eprintln!("skip: ffmpeg has no libtwolame");
        return;
    }
    let dir = work_dir();
    let cases: &[(&str, u32, &str, u32, bool)] = &[
        ("joint_stereo", 44100, "64k", 2, false),
        ("joint_stereo", 48000, "96k", 2, true),
        ("joint_stereo", 32000, "128k", 2, false),
        ("joint_stereo", 24000, "48k", 2, true),
        ("stereo", 44100, "192k", 2, true),
        ("stereo", 22050, "64k", 2, false),
        ("dual_channel", 48000, "128k", 2, false),
        ("mono", 44100, "64k", 1, true),
        ("mono", 16000, "32k", 1, false),
        ("joint_stereo", 44100, "384k", 2, true),
        ("joint_stereo", 44100, "112k", 2, false),
        ("mono", 48000, "56k", 1, false),
    ];
    for (n, &(mode, rate, br, ch, crc)) in cases.iter().enumerate() {
        let path = dir.join(format!("tl-{n}.mp2"));
        let src = format!(
            "aevalsrc=0.4*sin(2*PI*(300+1500*t)*t)|0.3*sin(2*PI*700*t)+0.05*random(1):s={rate}:d=2"
        );
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-ac"])
            .arg(ch.to_string())
            .args(["-c:a", "libtwolame", "-mode", mode, "-b:a", br])
            .args(["-error_protection", if crc { "1" } else { "0" }])
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success(), "twolame {mode} {rate} {br}");
        let bytes = std::fs::read(&path).unwrap();
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.crc, crc, "{mode} {rate}: protection bit");
        let want = ffmpeg_decode(&path, ch as usize, 2);
        let (got, r, c) = our_decode(&bytes);
        assert_eq!((r, c), (rate, ch as usize));
        // ffmpeg's demuxer drops leading frames whose header differs from the
        // ones after them (twolame flips the mode between frames): align on the
        // tail, and skip the frame after the cut, whose filterbank history we
        // have and ffmpeg does not.
        let per = 1152 * ch as usize;
        assert!(got.len() >= want.len() && (got.len() - want.len()).is_multiple_of(per));
        let cut = got.len() - want.len();
        let skip = if cut > 0 { per } else { 0 };
        let (e, at) = max_err(&got[cut + skip..], &want[skip..]);
        eprintln!("twolame {mode} {rate} Hz {br} crc={crc}: max abs error {e:.3e}");
        assert!(e <= 1e-5, "twolame {mode} {rate} {br}: {e} at {at}");
    }
}

fn layer2_frame_with_crc() -> Vec<u8> {
    let mut rng = Rng(77);
    // MPEG-1 Layer II, 48 kHz, 128 kbit/s, stereo, CRC on.
    let h = FrameHeader::parse(&[0xFF, 0xFC, 0x84, 0x00]).unwrap();
    loop {
        let f = structured_frame(&mut rng, &h, 40);
        if let Some(f) = f {
            return f;
        }
    }
}

#[test]
fn crc_protects_allocation_and_scfsi() {
    let frame = layer2_frame_with_crc();
    assert!(FrameHeader::parse(&frame).unwrap().crc);
    let mut reader = Mp3Reader::new();
    reader.push(&frame);
    assert!(reader.next_frame().is_ok(), "good CRC decodes");
    // Flip one allocation bit.
    let mut bad = frame.clone();
    bad[7] ^= 0x10;
    let mut reader = Mp3Reader::new();
    reader.push(&bad);
    let err = reader.next_frame().unwrap_err();
    assert!(err.to_string().contains("CRC"), "{err}");
    // A flip in the sample data is not covered by the CRC.
    let mut late = frame.clone();
    let n = late.len() - 3;
    late[n] ^= 0xFF;
    let mut reader = Mp3Reader::new();
    reader.push(&late);
    assert!(reader.next_frame().is_ok());
}

#[test]
fn free_format_layer2_is_refused_by_name() {
    // Bitrate index 0.
    let mut f = vec![0xFF, 0xFD, 0x04, 0x00];
    f.resize(600, 0);
    let mut dec = ec_mp3::Mp3Decode::new();
    let err = dec.decode_frame(&f).unwrap_err();
    assert!(err.to_string().contains("free-format"), "{err}");
}

#[test]
fn decoder_trait_takes_layer2_packets() {
    use ec_core::packet::{Buf, Packet};
    use ec_core::registry::{CodecId, Decoder};
    let mut rng = Rng(5);
    let h = FrameHeader::parse(&[0xFF, 0xFD, 0x94, 0x00]).unwrap(); // 44.1k 128k stereo
    let params = ec_mp3::layer_codec_parameters(2, 44100, 2);
    assert_eq!(params.codec, CodecId::Mp2);
    let mut dec = ec_mp3::Mp3Decoder::new(params).unwrap();
    let mut frames = 0;
    for _ in 0..5 {
        let f = loop {
            if let Some(f) = structured_frame(&mut rng, &h, 30) {
                break f;
            }
        };
        let pkt = Packet::new(
            0,
            ec_core::TimeBase::try_new(1, 44100).unwrap(),
            Buf::from(f),
        );
        dec.send_packet(&pkt).unwrap();
        while let Ok(frame) = dec.receive_frame() {
            let ec_core::frame::Frame::Audio(a) = frame else {
                panic!()
            };
            assert_eq!(a.samples, 1152);
            frames += 1;
        }
    }
    assert_eq!(frames, 5);
}

#[test]
fn damaged_and_truncated_frames_never_panic() {
    let mut rng = Rng(0xDEAD_BEEF);
    for layer in [2u8, 1] {
        for version in [3u8, 2, 0] {
            for mode in [0u8, 1, 3] {
                for _ in 0..40 {
                    let (ext, crc) = (rng.byte() % 4, rng.byte().is_multiple_of(2));
                    let h = random_header(&mut rng, layer, version, mode, ext, crc);
                    let len = h.frame_len().unwrap();
                    let mut f = h.to_bytes().to_vec();
                    f.extend((4..len).map(|_| rng.byte()));
                    // whole, then every truncation point class
                    let mut dec = ec_mp3::Mp3Decode::new();
                    let _ = dec.decode_frame(&f);
                    for cut in [5, 6, 20, len / 2, len - 1] {
                        let _ = dec.decode_frame(&f[..cut.min(len)]);
                        let mut r = Mp3Reader::new();
                        r.push(&f[..cut.min(len)]);
                        while r.next_frame().is_ok() {}
                    }
                }
            }
        }
    }
}
