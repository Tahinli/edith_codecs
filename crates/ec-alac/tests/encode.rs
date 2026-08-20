//! Round-trip the encoder through the decoder it was written to feed, then
//! through a real mp4 mux and (when the oracle is present) ffmpeg/ffprobe.

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_alac::{AlacDecoder, AlacEncoder};
use ec_core::registry::{CodecId, Demuxer, Encoder, Muxer, StreamInfo};
use ec_core::timebase::TimeBase;
use ec_core::Packet;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// A small deterministic PRNG so a "random data" test does not depend on an
/// external crate: xorshift32.
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn range(&mut self, bit_depth: u8) -> i32 {
        let max = 1i64 << (bit_depth - 1);
        ((self.next() as i64 % (2 * max)) - max) as i32
    }
}

/// Encode `samples` (interleaved, `channels` wide) in `frame_length`-sample
/// chunks (the last one short) and decode every packet straight back,
/// asserting bit-exact recovery.
fn round_trip(sample_rate: u32, channels: u8, bit_depth: u8, frame_length: u32, samples: &[i32]) {
    let enc = AlacEncoder::new(sample_rate, channels, bit_depth, frame_length).expect("encoder");
    let mut dec = AlacDecoder::new(*enc.cookie());
    let per_frame = frame_length as usize * channels as usize;
    let mut decoded = Vec::with_capacity(samples.len());
    for chunk in samples.chunks(per_frame) {
        let packet = enc.encode_frame(chunk);
        let out = dec.decode(&packet).expect("decode");
        // decode() shifts into the PCM container; undo that to compare
        // against the true-range input encode_frame took.
        let shift = enc.cookie().container_shift();
        decoded.extend(out.iter().map(|&s| s >> shift));
    }
    assert_eq!(decoded, samples, "{channels}ch {bit_depth}bit round trip");
}

/// One bit field starting at bit `start` of `data`, MSB-first — enough to
/// reach into a compressed mono frame's plan header (which starts at bit 39:
/// 3 element-tag + 16 instance/reserved + 1 partial + 2 bytes_shifted + 1
/// escape + 8 mix_bits + 8 mix_res) without pulling in the decoder's own
/// private bit reader.
fn bits_at(data: &[u8], start: usize, n: usize) -> u32 {
    let mut v = 0u32;
    for i in 0..n {
        let pos = start + i;
        v = (v << 1) | u32::from((data[pos / 8] >> (7 - pos % 8)) & 1);
    }
    v
}

#[test]
fn twenty_four_bit_uses_byte_shift_when_cheaper() {
    // Real 16-bit audio widened to 24 bits with random dither in the low
    // byte: exactly the shape a 24-bit master of 16-bit source material has,
    // and exactly what bytes_shifted exists for — the high bits alone
    // predict well, but folded together with the dither's noise they do not.
    let path = fixtures().join("wav16-stereo-48000.wav");
    if !path.exists() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }
    let mut reader = ec_riff::WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let mut rng = Rng(0xdead_beef);
    let samples: Vec<i32> = reader
        .read_all_i32()
        .expect("pcm")
        .into_iter()
        .map(|s| (s << 8) | (rng.next() & 0xff) as i32)
        .collect();
    let channels = spec.channels as u8;

    round_trip(spec.sample_rate, channels, 24, 4096, &samples);

    let per_frame = 4096 * channels as usize;
    let with_shift = AlacEncoder::new(spec.sample_rate, channels, 24, 4096).expect("encoder");
    let with_bytes: usize = samples.chunks(per_frame).map(|c| with_shift.encode_frame(c).len()).sum();

    // SAFETY: single-threaded test-only env toggle, restored before return.
    unsafe { std::env::set_var("EC_ALAC_NO_SHIFT", "1") };
    let without_shift = AlacEncoder::new(spec.sample_rate, channels, 24, 4096).expect("encoder");
    let without_bytes: usize = samples.chunks(per_frame).map(|c| without_shift.encode_frame(c).len()).sum();
    unsafe { std::env::remove_var("EC_ALAC_NO_SHIFT") };

    eprintln!("24-bit shifted: {with_bytes} bytes, unshifted: {without_bytes} bytes");
    assert!(
        with_bytes < without_bytes,
        "bytes_shifted split ({with_bytes}) should beat forcing bytes_shifted=0 ({without_bytes})"
    );
}

#[test]
fn mode_one_is_chosen_on_smooth_signal() {
    // A pure ramp: its first difference is a near-constant, which mode 1's
    // extra difference squeezes to near zero — cheaper than any order-0
    // filter's residual stream, which stays at the ramp's own value.
    let n = 4096usize;
    let samples: Vec<i32> = (0..n as i32).map(|i| 100 + i * 3).collect();

    unsafe { std::env::set_var("EC_ALAC_MODE1", "1") };
    let enc = AlacEncoder::new(48_000, 1, 16, 4096).expect("encoder");
    let packet = enc.encode_frame(&samples);
    unsafe { std::env::remove_var("EC_ALAC_MODE1") };

    let mode = bits_at(&packet, 39, 4);
    assert_eq!(mode, 1, "mode 1 should win the cost comparison on a pure ramp");

    let mut dec = AlacDecoder::new(*enc.cookie());
    let shift = enc.cookie().container_shift();
    let decoded: Vec<i32> = dec.decode(&packet).expect("decode").iter().map(|&s| s >> shift).collect();
    assert_eq!(decoded, samples, "mode 1 round trip");
}

#[test]
fn escape_frames_round_trip_random_data() {
    let mut rng = Rng(0x1234_5678);
    for &(channels, bit_depth) in &[(1u8, 16u8), (2, 16), (1, 24), (2, 24)] {
        // 4096 + a short tail, so the last frame is partial.
        let n = 4096 * 2 + 777;
        let samples: Vec<i32> = (0..n * channels as usize).map(|_| rng.range(bit_depth)).collect();
        round_trip(48_000, channels, bit_depth, 4096, &samples);
    }
}

#[test]
fn real_audio_round_trips_and_compresses() {
    let path = fixtures().join("wav16-stereo-48000.wav");
    if !path.exists() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }
    let mut reader = ec_riff::WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let samples = reader.read_all_i32().expect("pcm");
    round_trip(spec.sample_rate, spec.channels as u8, spec.bits_per_sample as u8, 4096, &samples);

    let enc = AlacEncoder::new(spec.sample_rate, spec.channels as u8, spec.bits_per_sample as u8, 4096)
        .expect("encoder");
    let per_frame = 4096 * spec.channels as usize;
    let mut coded_bytes = 0usize;
    for chunk in samples.chunks(per_frame) {
        coded_bytes += enc.encode_frame(chunk).len();
    }
    let raw_bytes = samples.len() * 2;
    let ratio = coded_bytes as f64 / raw_bytes as f64;
    eprintln!("wav16-stereo-48000: {coded_bytes}/{raw_bytes} bytes, ratio {ratio:.4}");
    assert!(ratio <= 0.75, "compression ratio {ratio:.4} > 0.75");
}

#[test]
fn muxed_stream_is_alac_at_the_right_rate_and_ffmpeg_agrees() {
    let path = fixtures().join("wav16-stereo-48000.wav");
    if !path.exists() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }
    let mut reader = ec_riff::WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let samples = reader.read_all_i32().expect("pcm");
    let enc = AlacEncoder::new(spec.sample_rate, spec.channels as u8, spec.bits_per_sample as u8, 4096)
        .expect("encoder");

    let mut mp4 = ec_mp4::Mp4Muxer::new(Cursor::new(Vec::new())).expect("muxer");
    let time_base = TimeBase::new(1, i64::from(spec.sample_rate));
    let stream = mp4
        .add_stream(StreamInfo::new(0, time_base, enc.codec_parameters().clone()))
        .expect("add_stream");

    let per_frame = 4096 * spec.channels as usize;
    let mut pts = 0i64;
    for chunk in samples.chunks(per_frame) {
        let n = chunk.len() / spec.channels as usize;
        let data = enc.encode_frame(chunk);
        let mut packet = Packet::new(stream, time_base, data).with_pts(pts);
        packet.duration = Some(n as i64);
        mp4.write_packet(&packet).expect("write_packet");
        pts += n as i64;
    }
    mp4.finish().expect("finish");
    let bytes = mp4.into_inner().into_inner();

    let dir = std::env::temp_dir().join(format!("ec-alac-mux-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out_path = dir.join("out.m4a");
    std::fs::write(&out_path, &bytes).expect("write m4a");

    // Sanity without an oracle: our own demuxer/decoder reads it back
    // bit-exact against the source PCM.
    let mut demuxer =
        ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(&out_path).expect("open"))).expect("mp4");
    let track = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Alac)
        .expect("alac track")
        .clone();
    let mut dec = AlacDecoder::from_parameters(track.params).expect("cookie");
    let shift = dec.cookie().container_shift();
    let mut decoded = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(p) if p.stream != track.index => continue,
            Ok(p) => decoded.extend(dec.decode(&p.data).expect("decode").iter().map(|&s| s >> shift)),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(decoded, samples, "mux -> demux -> decode round trip");

    // The oracle: ffprobe reads the track back as alac at the right rate and
    // channel count, and ffmpeg's own decode matches the source exactly.
    if Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("ffprobe absent — skipping oracle checks");
        return;
    }
    let probe = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "a:0", "-show_entries",
            "stream=codec_name,sample_rate,channels",
            "-of", "default=noprint_wrappers=1",
        ])
        .arg(&out_path)
        .output()
        .expect("ffprobe");
    let info = String::from_utf8_lossy(&probe.stdout);
    assert!(info.contains("codec_name=alac"), "{info}");
    assert!(info.contains(&format!("sample_rate={}", spec.sample_rate)), "{info}");
    assert!(info.contains(&format!("channels={}", spec.channels)), "{info}");

    let decode = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&out_path)
        .args(["-f", "s16le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(decode.status.success(), "{}", String::from_utf8_lossy(&decode.stderr));
    let want: Vec<i16> = samples.iter().map(|&s| s as i16).collect();
    let got: Vec<i16> = decode
        .stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(got, want, "ffmpeg decode of the muxed file vs source PCM");

    let _ = std::fs::remove_dir_all(&dir);
}

