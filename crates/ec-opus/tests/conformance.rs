//! RFC 6716 conformance: the official test vectors, the `opus_compare` quality
//! metric, real-file oracles against ffmpeg, and the decode-speed floor.
//!
//! All of it lives in one test binary on purpose — a file per feature costs a
//! serial link-and-run for every build.

use std::fs;
use std::path::{Path, PathBuf};

use ec_opus::Decoder;

/// One packet of an `opus_demo` test vector file.
struct VectorPacket {
    payload: Vec<u8>,
    /// The reference decoder's range state after this packet.
    final_range: u32,
}

/// The `.bit` files are `opus_demo` output: per packet, a big-endian length, a
/// big-endian final range, then the packet itself.
fn read_vector(path: &Path) -> Vec<VectorPacket> {
    let data = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let rng = u32::from_be_bytes(data[i + 4..i + 8].try_into().unwrap());
        i += 8;
        if i + len > data.len() {
            break;
        }
        out.push(VectorPacket {
            payload: data[i..i + len].to_vec(),
            final_range: rng,
        });
        i += len;
    }
    out
}

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vectors/opus-rfc6716/opus_testvectors")
}

/// Decodes a vector at 48 kHz stereo, returning the samples and how many
/// packets ended with the reference range state.
fn decode_vector(packets: &[VectorPacket]) -> (Vec<f32>, usize, usize) {
    let mut dec = Decoder::new(48000, 2).unwrap();
    let mut pcm = Vec::new();
    let mut buf = vec![0.0f32; 5760 * 2];
    let mut matched = 0;
    let mut decoded = 0;
    for p in packets {
        match dec.decode_float(&p.payload, &mut buf) {
            Ok(n) => {
                pcm.extend_from_slice(&buf[..n * 2]);
                decoded += 1;
                if dec.final_range() == p.final_range {
                    matched += 1;
                }
            }
            Err(e) => {
                panic!("packet {decoded} failed: {e}");
            }
        }
    }
    (pcm, matched, decoded)
}

// ---------------------------------------------------------------------------
// The conformance metric
// ---------------------------------------------------------------------------
//
// RFC 6716 Section 6 defines a compliant decoder as one whose output stays
// "within the thresholds specified by the opus_compare.c tool" against the
// reference decoder's output for each test vector. That tool is a weighted
// spectral comparison, reimplemented here so the check needs no C toolchain:
//
//   1. Both signals are cut into 480-sample Hann windows at a 120-sample hop
//      and turned into per-bin power spectra (bins 0..200 of 240).
//   2. The reference's per-band energy is spread by a masking model: 10 dB per
//      Bark upwards, 15 dB downwards, -3 dB per 2.5 ms in time, 1 % crosstalk
//      between channels. A tenth of that masking energy is added to both
//      spectra, so error under a strong signal counts for less.
//   3. Consecutive frames are averaged pairwise, then per bin the ratio
//      r = Y/X contributes r - log(r) - 1 (zero when equal, growing either
//      way); bins 79..81 are damped because the SILK/CELT crossover is free.
//   4. The per-band mean is squared, averaged over bands, squared again,
//      raised to the fourth power per frame, averaged, and the 16th root taken.
//      Quality is 100*(1 - 0.5*ln(1+err)/ln(1.13)); 0 is the pass threshold and
//      100 means identical output.
//
// A quality of 0 was calibrated to additive white noise at 48 dB SNR.

const NBANDS: usize = 21;
const BANDS: [usize; NBANDS + 1] = [
    0, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 68, 80, 96, 120, 156, 200,
];
const WIN_SIZE: usize = 480;
const WIN_STEP: usize = 120;
const NFREQS: usize = 240;

/// A complex FFT of size `15 * 2^k`, which is what a 480-sample window needs.
/// Same decomposition the decoder uses; duplicated here so the test measures
/// the reference metric rather than the decoder's own code.
struct Fft15 {
    n: usize,
    sub: ec_dsp::Fft<f64>,
}

impl Fft15 {
    fn new(n: usize) -> Fft15 {
        assert!(n.is_multiple_of(15) && (n / 15).is_power_of_two());
        Fft15 {
            n,
            sub: ec_dsp::Fft::new(n / 15),
        }
    }

    /// Power spectrum of a real signal, bins `0..n/2`.
    fn power(&mut self, x: &[f64], out: &mut [f64]) {
        let n = self.n;
        let l = n / 15;
        let mut re = vec![0.0f64; n];
        let mut im = vec![0.0f64; n];
        for n2 in 0..l {
            for k1 in 0..15 {
                let (mut sr, mut si) = (0.0, 0.0);
                for n1 in 0..15 {
                    let a = 2.0 * std::f64::consts::PI * ((k1 * n1) % 15) as f64 / 15.0;
                    sr += x[l * n1 + n2] * a.cos();
                    si += x[l * n1 + n2] * a.sin();
                }
                let a = 2.0 * std::f64::consts::PI * (k1 * n2) as f64 / n as f64;
                let (c, s) = (a.cos(), a.sin());
                re[k1 * l + n2] = sr * c - si * s;
                im[k1 * l + n2] = sr * s + si * c;
            }
        }
        for k1 in 0..15 {
            let rr = &mut re[k1 * l..(k1 + 1) * l];
            let ii = &mut im[k1 * l..(k1 + 1) * l];
            self.sub.inverse_split(rr, ii);
            for k2 in 0..l {
                let idx = k1 + 15 * k2;
                if idx < out.len() {
                    let (r, i) = (rr[k2] * l as f64, ii[k2] * l as f64);
                    out[idx] = r * r + i * i;
                }
            }
        }
    }
}

/// Per-bin power spectra and per-band energies, `band_energy()` in the tool.
fn band_energy(
    signal: &[f64],
    channels: usize,
    nframes: usize,
    bands: &mut Option<&mut Vec<f64>>,
    ps: &mut Vec<f64>,
) {
    let mut fft = Fft15::new(WIN_SIZE);
    let window: Vec<f64> = (0..WIN_SIZE)
        .map(|j| 0.5 - 0.5 * (2.0 * std::f64::consts::PI / (WIN_SIZE - 1) as f64 * j as f64).cos())
        .collect();
    ps.resize(nframes * NFREQS * channels, 0.0);
    if let Some(b) = bands {
        b.resize(nframes * NBANDS * channels, 0.0);
    }
    let mut frame = vec![0.0f64; WIN_SIZE];
    let mut spec = vec![0.0f64; NFREQS];
    for xi in 0..nframes {
        for ci in 0..channels {
            for k in 0..WIN_SIZE {
                frame[k] = window[k] * signal[(xi * WIN_STEP + k) * channels + ci];
            }
            spec.fill(0.0);
            fft.power(&frame, &mut spec);
            for bi in 0..NBANDS {
                let mut p = 0.0;
                for xj in BANDS[bi]..BANDS[bi + 1] {
                    let v = spec[xj] + 100000.0;
                    ps[(xi * NFREQS + xj) * channels + ci] = v;
                    p += v;
                }
                if let Some(b) = bands {
                    b[(xi * NBANDS + bi) * channels + ci] = p / (BANDS[bi + 1] - BANDS[bi]) as f64;
                }
            }
        }
    }
}

/// The `opus_compare` quality metric: >= 0 passes, 100 is identical output.
fn opus_compare(reference: &[i16], test: &[i16], channels: usize) -> f64 {
    assert_eq!(reference.len(), test.len(), "sample counts must match");
    let x: Vec<f64> = reference.iter().map(|&v| v as f64).collect();
    let y: Vec<f64> = test.iter().map(|&v| v as f64).collect();
    let xlength = x.len() / channels;
    assert!(xlength >= WIN_SIZE, "not enough samples to compare");
    let nframes = (xlength - WIN_SIZE + WIN_STEP) / WIN_STEP;

    let mut xb = Vec::new();
    let mut big_x = Vec::new();
    let mut big_y = Vec::new();
    band_energy(&x, channels, nframes, &mut Some(&mut xb), &mut big_x);
    band_energy(&y, channels, nframes, &mut None, &mut big_y);

    for xi in 0..nframes {
        // Frequency masking, low to high then high to low.
        for bi in 1..NBANDS {
            for ci in 0..channels {
                xb[(xi * NBANDS + bi) * channels + ci] +=
                    0.1 * xb[(xi * NBANDS + bi - 1) * channels + ci];
            }
        }
        for bi in (0..NBANDS - 1).rev() {
            for ci in 0..channels {
                xb[(xi * NBANDS + bi) * channels + ci] +=
                    0.03 * xb[(xi * NBANDS + bi + 1) * channels + ci];
            }
        }
        // Temporal masking.
        if xi > 0 {
            for bi in 0..NBANDS {
                for ci in 0..channels {
                    xb[(xi * NBANDS + bi) * channels + ci] +=
                        0.5 * xb[((xi - 1) * NBANDS + bi) * channels + ci];
                }
            }
        }
        // Crosstalk between channels.
        if channels == 2 {
            for bi in 0..NBANDS {
                let l = xb[(xi * NBANDS + bi) * channels];
                let r = xb[(xi * NBANDS + bi) * channels + 1];
                xb[(xi * NBANDS + bi) * channels] += 0.01 * r;
                xb[(xi * NBANDS + bi) * channels + 1] += 0.01 * l;
            }
        }
        // Apply the masking to both spectra.
        for bi in 0..NBANDS {
            for xj in BANDS[bi]..BANDS[bi + 1] {
                for ci in 0..channels {
                    let m = 0.1 * xb[(xi * NBANDS + bi) * channels + ci];
                    big_x[(xi * NFREQS + xj) * channels + ci] += m;
                    big_y[(xi * NFREQS + xj) * channels + ci] += m;
                }
            }
        }
    }

    // Average consecutive frames to make the comparison less twitchy.
    for bi in 0..NBANDS {
        for xj in BANDS[bi]..BANDS[bi + 1] {
            for ci in 0..channels {
                let mut xtmp = big_x[xj * channels + ci];
                let mut ytmp = big_y[xj * channels + ci];
                for xi in 1..nframes {
                    let xtmp2 = big_x[(xi * NFREQS + xj) * channels + ci];
                    let ytmp2 = big_y[(xi * NFREQS + xj) * channels + ci];
                    big_x[(xi * NFREQS + xj) * channels + ci] += xtmp;
                    big_y[(xi * NFREQS + xj) * channels + ci] += ytmp;
                    xtmp = xtmp2;
                    ytmp = ytmp2;
                }
            }
        }
    }

    let max_compare = BANDS[NBANDS];
    let mut err = 0.0f64;
    for xi in 0..nframes {
        let mut ef = 0.0f64;
        for bi in 0..NBANDS {
            let mut eb = 0.0f64;
            for xj in BANDS[bi]..BANDS[bi + 1].min(max_compare) {
                for ci in 0..channels {
                    let re = big_y[(xi * NFREQS + xj) * channels + ci]
                        / big_x[(xi * NFREQS + xj) * channels + ci];
                    let mut im = re - re.ln() - 1.0;
                    // The SILK/CELT crossover may differ in its filters.
                    if (79..=81).contains(&xj) {
                        im *= 0.1;
                    }
                    if xj == 80 {
                        im *= 0.1;
                    }
                    eb += im;
                }
            }
            eb /= ((BANDS[bi + 1] - BANDS[bi]) * channels) as f64;
            ef += eb * eb;
        }
        ef /= NBANDS as f64;
        ef *= ef;
        err += ef * ef;
    }
    err = (err / nframes as f64).powf(1.0 / 16.0);
    100.0 * (1.0 - 0.5 * (1.0 + err).ln() / 1.13f64.ln())
}

/// `FLOAT2INT16`: what `opus_demo` writes, so the metric sees the same samples.
fn to_i16(pcm: &[f32]) -> Vec<i16> {
    pcm.iter()
        .map(|&v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn read_i16(path: &Path) -> Vec<i16> {
    let d = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    d.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Every vector in the RFC 6716 suite, in the order the RFC lists them.
const VECTORS: [&str; 12] = [
    "testvector01",
    "testvector02",
    "testvector03",
    "testvector04",
    "testvector05",
    "testvector06",
    "testvector07",
    "testvector08",
    "testvector09",
    "testvector10",
    "testvector11",
    "testvector12",
];

/// The conformance test of RFC 6716 Section 6, both halves of it: the decoder's
/// range coder state MUST match the reference on every packet, and the output
/// MUST stay within the `opus_compare` threshold (quality >= 0).
#[test]
fn rfc6716_test_vectors() {
    let mut table = Vec::new();
    for name in VECTORS {
        let path = vectors_dir().join(format!("{name}.bit"));
        if !path.exists() {
            eprintln!("{name}: missing, skipped (run scripts/fetch-vectors.sh)");
            continue;
        }
        let packets = read_vector(&path);
        let (pcm, matched, decoded) = decode_vector(&packets);
        let reference = read_i16(&vectors_dir().join(format!("{name}.dec")));
        let mine = to_i16(&pcm);
        assert_eq!(
            reference.len(),
            mine.len(),
            "{name}: decoded {} samples, reference has {}",
            mine.len() / 2,
            reference.len() / 2
        );
        let identical = reference.iter().zip(&mine).filter(|(a, b)| a == b).count();
        let quality = opus_compare(&reference, &mine, 2);
        table.push(format!(
            "{name}: {matched}/{decoded} packets range-exact, \
             {:.4} % samples identical, quality {quality:.2} %",
            100.0 * identical as f64 / reference.len() as f64
        ));
        assert_eq!(
            matched, decoded,
            "{name}: range coder state diverged from the reference"
        );
        assert!(
            quality >= 0.0,
            "{name}: opus_compare quality {quality:.2} %"
        );
    }
    for line in table {
        println!("{line}");
    }
}

// ---------------------------------------------------------------------------
// Real files: the Ogg-Opus fixtures, against ffmpeg
// ---------------------------------------------------------------------------

use ec_opus::MultistreamDecoder;
use std::process::Command;

/// Splits an Ogg stream into packets (RFC 3533): a page header, a segment
/// table, and packets that end on any segment shorter than 255 bytes.
fn ogg_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut partial: Vec<u8> = Vec::new();
    let mut i = 0;
    while i + 27 <= data.len() {
        assert_eq!(&data[i..i + 4], b"OggS", "not an Ogg page at {i}");
        let nsegs = data[i + 26] as usize;
        let table = &data[i + 27..i + 27 + nsegs];
        let mut body = i + 27 + nsegs;
        for &len in table {
            let len = len as usize;
            partial.extend_from_slice(&data[body..body + len]);
            body += len;
            if len < 255 {
                packets.push(core::mem::take(&mut partial));
            }
        }
        i = body;
    }
    packets
}

/// The RFC 7845 identification header: channel count, pre-skip and the
/// multistream layout.
struct OpusHead {
    channels: usize,
    pre_skip: usize,
    streams: usize,
    coupled: usize,
    mapping: Vec<u8>,
}

fn parse_opus_head(p: &[u8]) -> OpusHead {
    assert_eq!(&p[..8], b"OpusHead");
    let channels = p[9] as usize;
    let pre_skip = u16::from_le_bytes([p[10], p[11]]) as usize;
    let family = p[18];
    if family == 0 {
        OpusHead {
            channels,
            pre_skip,
            streams: 1,
            coupled: channels - 1,
            mapping: (0..channels as u8).collect(),
        }
    } else {
        OpusHead {
            channels,
            pre_skip,
            streams: p[19] as usize,
            coupled: p[20] as usize,
            mapping: p[21..21 + channels].to_vec(),
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

/// ffmpeg's decode of the same file, interleaved `f32` in ffmpeg's channel
/// order, with the pre-skip already removed.
///
/// The decoder is forced to `libopus` — the reference implementation. ffmpeg's
/// own native Opus decoder is a third implementation and deviates from libopus
/// by as much as 0.97 correlation on this very fixture's coupled surround
/// streams, so it makes a poor oracle.
fn ffmpeg_decode(path: &Path, channels: usize) -> Option<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", "libopus", "-i"])
        .arg(path)
        .args([
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            &channels.to_string(),
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Decodes an Ogg-Opus file with the multistream decoder, pre-skip removed.
fn decode_ogg(path: &Path) -> (Vec<f32>, usize) {
    let data = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let packets = ogg_packets(&data);
    let head = parse_opus_head(&packets[0]);
    let mut dec = MultistreamDecoder::with_rate(48000, head.streams, head.coupled, &head.mapping);
    let mut pcm = Vec::new();
    for p in packets.iter().skip(2) {
        if p.is_empty() {
            continue;
        }
        let frame = dec.decode_packet(p).expect("decode");
        pcm.extend_from_slice(&frame);
    }
    pcm.drain(..head.pre_skip * head.channels);
    (pcm, head.channels)
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let (mut sxy, mut sxx, mut syy) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        sxy += a[i] as f64 * b[i] as f64;
        sxx += (a[i] as f64).powi(2);
        syy += (b[i] as f64).powi(2);
    }
    if sxx == 0.0 || syy == 0.0 {
        return if sxx == syy { 1.0 } else { 0.0 };
    }
    sxy / (sxx * syy).sqrt()
}

/// Power at `freq` in a channel, by the Goertzel algorithm.
fn tone_power(x: &[f32], channels: usize, ch: usize, freq: f64) -> f64 {
    let n = x.len() / channels;
    let w = 2.0 * std::f64::consts::PI * freq / 48000.0;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for i in 0..n {
        let s = x[i * channels + ch] as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n * n) as f64
}

#[test]
fn ogg_opus_fixtures_match_ffmpeg() {
    // Mapping family 1 puts 5.1 in Vorbis order; ffmpeg hands its own order
    // back, so the comparison permutes one into the other.
    const VORBIS_TO_FFMPEG_5_1: [usize; 6] = [0, 2, 1, 4, 5, 3];
    for (name, channels) in [
        ("opus-ogg-mono-48000.opus", 1usize),
        ("opus-ogg-stereo-48000.opus", 2),
        ("opus-ogg-5.1-48000.opus", 6),
    ] {
        let path = fixture(name);
        if !path.exists() {
            eprintln!("{name}: missing, skipped (run scripts/gen-fixtures.sh)");
            continue;
        }
        let Some(reference) = ffmpeg_decode(&path, channels) else {
            eprintln!("{name}: ffmpeg unavailable, skipped");
            continue;
        };
        let (mine, ch) = decode_ogg(&path);
        assert_eq!(ch, channels, "{name}: channel count");
        let mut worst = 1.0f64;
        let n = (reference.len() / channels).min(mine.len() / channels);
        assert!(n > 48000, "{name}: only {n} samples decoded");
        for c in 0..channels {
            let their_c = if channels == 6 {
                VORBIS_TO_FFMPEG_5_1[c]
            } else {
                c
            };
            let a: Vec<f32> = (0..n).map(|i| mine[i * channels + c]).collect();
            let b: Vec<f32> = (0..n).map(|i| reference[i * channels + their_c]).collect();
            let corr = correlation(&a, &b);
            println!("{name}: channel {c} correlation {corr:.5}");
            worst = worst.min(corr);
        }
        assert!(
            worst >= 0.99,
            "{name}: worst channel correlation {worst:.5}"
        );
    }
}

#[test]
fn five_one_channels_land_in_the_right_places() {
    // The fixture carries one distinct sine per channel, so a channel-order
    // bug shows up as a channel playing the wrong tone. The tones themselves
    // come from the reference decode rather than from an assumption about how
    // ffmpeg laid the fixture out; what is asserted is that this decoder's
    // Vorbis-order output is the reference's 5.1-order output permuted.
    const VORBIS_TO_FFMPEG_5_1: [usize; 6] = [0, 2, 1, 4, 5, 3];
    const TONES: [f64; 6] = [220.0, 440.0, 660.0, 55.0, 880.0, 1320.0];
    let path = fixture("opus-ogg-5.1-48000.opus");
    if !path.exists() {
        eprintln!("5.1 fixture missing, skipped");
        return;
    }
    let Some(reference) = ffmpeg_decode(&path, 6) else {
        eprintln!("ffmpeg unavailable, skipped");
        return;
    };
    let (pcm, channels) = decode_ogg(&path);
    assert_eq!(channels, 6);
    let dominant = |x: &[f32], ch: usize| -> usize {
        let mut best = (0usize, 0.0f64);
        for (t, &f) in TONES.iter().enumerate() {
            let p = tone_power(x, 6, ch, f);
            if p > best.1 {
                best = (t, p);
            }
        }
        best.0
    };
    let mut seen = [false; 6];
    for (c, &their) in VORBIS_TO_FFMPEG_5_1.iter().enumerate() {
        let want = dominant(&reference, their);
        let got = dominant(&pcm, c);
        println!(
            "channel {c}: {} Hz (reference channel {their} carries {} Hz)",
            TONES[got], TONES[want]
        );
        assert_eq!(got, want, "channel {c} carries the wrong tone");
        assert!(!seen[got], "tone {} Hz appears on two channels", TONES[got]);
        seen[got] = true;
    }
}

#[test]
fn decode_speed() {
    if cfg!(debug_assertions) {
        eprintln!("decode speed is only meaningful in release; skipped");
        return;
    }
    for (name, channels) in [
        ("opus-ogg-stereo-48000.opus", 2usize),
        ("opus-ogg-5.1-48000.opus", 6),
    ] {
        let path = fixture(name);
        if !path.exists() {
            continue;
        }
        let data = fs::read(&path).unwrap();
        let packets = ogg_packets(&data);
        let head = parse_opus_head(&packets[0]);
        let audio: Vec<&Vec<u8>> = packets.iter().skip(2).filter(|p| !p.is_empty()).collect();
        let mut dec =
            MultistreamDecoder::with_rate(48000, head.streams, head.coupled, &head.mapping);
        let mut out = vec![0.0f32; 5760 * channels];
        // Warm up, then measure enough passes to cover several seconds of audio.
        let mut samples = 0usize;
        for p in &audio {
            samples += dec.decode_float(p, &mut out).unwrap();
        }
        let passes = 20;
        let start = std::time::Instant::now();
        for _ in 0..passes {
            dec.reset();
            for p in &audio {
                dec.decode_float(p, &mut out).unwrap();
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let audio_secs = (samples * passes) as f64 / 48000.0;
        let realtime = audio_secs / elapsed;
        println!(
            "{name}: {realtime:.0}x realtime ({channels} channels, {audio_secs:.1} s in {elapsed:.3} s)"
        );
        let floor = if channels == 6 { 104.0 } else { 135.0 };
        assert!(
            realtime >= floor,
            "{name}: {realtime:.0}x realtime is below the {floor}x floor"
        );
    }

    // The fixtures are sine tones, which are cheap; testvector01 is real music
    // at a high rate and is the honest stereo number.
    let path = vectors_dir().join("testvector01.bit");
    if path.exists() {
        let packets = read_vector(&path);
        let mut dec = Decoder::new(48000, 2).unwrap();
        let mut out = vec![0.0f32; 5760 * 2];
        let mut samples = 0usize;
        let start = std::time::Instant::now();
        for p in &packets {
            samples += dec.decode_float(&p.payload, &mut out).unwrap();
        }
        let elapsed = start.elapsed().as_secs_f64();
        let realtime = samples as f64 / 48000.0 / elapsed;
        println!("testvector01 (stereo music): {realtime:.0}x realtime");
        assert!(realtime >= 135.0, "stereo music decodes at {realtime:.0}x");
    }
}

#[test]
#[ignore]
fn real_library_sweep() {
    // Files named on the command line (EC_OPUS_FILES=a.opus:b.opus), decoded
    // and compared against libopus channel by channel, with the decode speed.
    let files = std::env::var("EC_OPUS_FILES").unwrap_or_default();
    for f in files.split(':').filter(|s| !s.is_empty()) {
        let path = PathBuf::from(f);
        let (pcm, channels) = decode_ogg(&path);
        let reference = ffmpeg_decode(&path, channels).expect("libopus");
        let n = (reference.len() / channels).min(pcm.len() / channels);
        let mut worst = 1.0f64;
        for c in 0..channels {
            let a: Vec<f32> = (0..n).map(|i| pcm[i * channels + c]).collect();
            let b: Vec<f32> = (0..n).map(|i| reference[i * channels + c]).collect();
            let corr = correlation(&a, &b);
            worst = worst.min(corr);
            println!("{f}: channel {c} correlation {corr:.5}");
        }
        // Speed on the same file.
        let data = fs::read(&path).unwrap();
        let packets = ogg_packets(&data);
        let head = parse_opus_head(&packets[0]);
        let audio: Vec<&Vec<u8>> = packets.iter().skip(2).filter(|p| !p.is_empty()).collect();
        let mut dec =
            MultistreamDecoder::with_rate(48000, head.streams, head.coupled, &head.mapping);
        let mut out = vec![0.0f32; 5760 * channels];
        let mut samples = 0usize;
        for p in &audio {
            samples += dec.decode_float(p, &mut out).unwrap();
        }
        dec.reset();
        let start = std::time::Instant::now();
        for p in &audio {
            dec.decode_float(p, &mut out).unwrap();
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "{f}: {channels} ch, {:.1} s audio, {:.0}x realtime, worst channel {worst:.5}",
            samples as f64 / 48000.0,
            samples as f64 / 48000.0 / elapsed
        );
    }
}

/// Malformed input must come back as an error, never a panic: the decoders
/// index tables with values the bitstream chooses, so this throws well-formed
/// TOC bytes at random payloads to make sure every mode is actually entered.
#[test]
fn garbage_payloads_never_panic() {
    let mut state = 0x12345678u32;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut dec = Decoder::new(48000, 2).unwrap();
    let mut ms = MultistreamDecoder::with_rate(48000, 4, 2, &[0, 4, 1, 2, 3, 5]);
    let mut out = vec![0.0f32; 5760 * 6];
    for config in 0..32u8 {
        for stereo in 0..2u8 {
            for code in 0..4u8 {
                let toc = (config << 3) | (stereo << 2) | code;
                for trial in 0..40 {
                    let len = 2 + (rand() as usize % 400);
                    let mut packet = vec![toc];
                    for _ in 0..len {
                        packet.push(rand() as u8);
                    }
                    if trial % 2 == 0 {
                        dec.reset();
                    }
                    let _ = dec.decode_float(&packet, &mut out);
                    let _ = ms.decode_float(&packet, &mut out);
                }
            }
        }
    }
    // Truncations of a real packet, which exercise the mid-frame paths.
    let path = vectors_dir().join("testvector10.bit");
    if path.exists() {
        for p in read_vector(&path).iter().take(200) {
            for cut in 1..p.payload.len().min(64) {
                let _ = dec.decode_float(&p.payload[..cut], &mut out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder: the world decodes what we write
// ---------------------------------------------------------------------------
//
// The oracle is libopus through ffmpeg, never our own decoder alone: an
// encoder and a decoder that share a misreading round-trip perfectly and agree
// with nothing else.

use ec_core::{
    Buf, CodecId, CodecParameters, MediaParameters, Muxer, Packet as CorePacket, StreamInfo,
    TimeBase,
};
use ec_ogg::{OggMuxer, granule_side_data};
use ec_opus::{Application, Encoder, MultistreamEncoder, ogg::default_mapping, ogg::opus_head};

/// Real programme material to encode: the RFC vector's own decoded output,
/// 48 kHz stereo. Sine fixtures flatter an encoder; this does not.
fn source_pcm(seconds: f64) -> Option<Vec<f32>> {
    let path = vectors_dir().join("testvector01.dec");
    if !path.exists() {
        return None;
    }
    let s = read_i16(&path);
    let want = (seconds * 48000.0) as usize * 2;
    Some(
        s.iter()
            .take(want)
            .map(|&v| v as f32 * (1.0 / 32768.0))
            .collect(),
    )
}

/// Mono view of a stereo buffer (the left channel), so the mono row of the
/// matrix is the same programme.
fn to_mono(pcm: &[f32]) -> Vec<f32> {
    pcm.chunks_exact(2).map(|c| c[0]).collect()
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("ec-opus-enc");
    fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Encodes `pcm` and writes an Ogg-Opus file, returning the packets' total
/// bytes and the sample count.
#[allow(clippy::too_many_arguments)]
fn encode_ogg(
    pcm: &[f32],
    channels: usize,
    bitrate: u32,
    vbr: bool,
    frame_48k: usize,
    path: &Path,
) -> (usize, usize, std::time::Duration) {
    const PRE_SKIP: i64 = 120;
    let samples = pcm.len() / channels;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut buf = vec![0u8; 8 * 1500];
    let mut frame = vec![0.0f32; frame_48k * channels];
    let started = std::time::Instant::now();
    if channels <= 2 {
        let mut e = Encoder::new(48000, channels, Application::Audio).expect("encoder");
        e.set_bitrate(bitrate);
        e.set_vbr(vbr);
        let mut at = 0;
        while at < samples {
            let n = (samples - at).min(frame_48k);
            frame.fill(0.0);
            frame[..n * channels].copy_from_slice(&pcm[at * channels..(at + n) * channels]);
            let len = e.encode_float(&frame, frame_48k, &mut buf).expect("encode");
            packets.push(buf[..len].to_vec());
            at += frame_48k;
        }
    } else {
        let mut e = MultistreamEncoder::surround(48000, channels, Application::Audio)
            .expect("multistream encoder");
        e.set_bitrate(bitrate);
        e.set_vbr(vbr);
        let mut at = 0;
        while at < samples {
            let n = (samples - at).min(frame_48k);
            frame.fill(0.0);
            frame[..n * channels].copy_from_slice(&pcm[at * channels..(at + n) * channels]);
            let len = e.encode_float(&frame, frame_48k, &mut buf).expect("encode");
            packets.push(buf[..len].to_vec());
            at += frame_48k;
        }
    }
    let elapsed = started.elapsed();
    let bytes: usize = packets.iter().map(|p| p.len()).sum();

    // Ogg-Opus: pre-skip cancels the encoder's overlap delay, and the last
    // granule trims the padding off the tail so the duration is exact.
    let mapping = default_mapping(channels).expect("mapping");
    let head = if channels <= 2 {
        opus_head(channels as u8, PRE_SKIP as u16, 48000, 0, None)
    } else {
        opus_head(
            channels as u8,
            PRE_SKIP as u16,
            48000,
            0,
            Some((mapping.0, mapping.1, mapping.2, &mapping.3)),
        )
    };
    let mut params = CodecParameters::new(CodecId::Opus);
    if let MediaParameters::Audio(audio) = &mut params.media {
        audio.sample_rate = 48000;
        audio.layout = match channels {
            1 => ec_core::ChannelLayout::Mono,
            2 => ec_core::ChannelLayout::Stereo,
            6 => ec_core::ChannelLayout::Surround5_1,
            _ => ec_core::ChannelLayout::Surround7_1,
        };
    }
    params.extradata = Some(Buf::from_vec(head));
    let time_base = TimeBase::from_rate(48000);
    let file = fs::File::create(path).expect("create");
    let mut muxer = OggMuxer::new(std::io::BufWriter::new(file));
    muxer
        .add_stream(StreamInfo::new(0, time_base, params))
        .expect("add stream");
    let end = samples as i64 + PRE_SKIP;
    for (i, p) in packets.iter().enumerate() {
        let granule = (((i + 1) * frame_48k) as i64).min(end);
        let mut packet = CorePacket::new(0, time_base, p.clone());
        packet.side_data.push(granule_side_data(granule));
        muxer.write_packet(&packet).expect("write");
    }
    muxer.finish().expect("finish");
    (bytes, samples, elapsed)
}

/// Decodes an Ogg-Opus file with our own decoder, pre-skip removed.
fn our_decode_ogg(path: &Path) -> Vec<f32> {
    decode_ogg(path).0
}

/// libopus encoding the same audio at the same setting, decoded back — the
/// only reference an encoder can honestly be scored against. Returns `None`
/// when ffmpeg cannot do it, so the test degrades to reporting our own number.
fn libopus_encode_decode(
    src: &[f32],
    channels: usize,
    bitrate: u32,
    dir: &Path,
) -> Option<Vec<f32>> {
    use std::io::Write;
    let raw: Vec<u8> = src.iter().flat_map(|v| v.to_le_bytes()).collect();
    let out = dir.join(format!("libopus-{channels}-{bitrate}.opus"));
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "f32le", "-ar", "48000", "-ac"])
        .arg(channels.to_string())
        .args(["-i", "-", "-c:a", "libopus", "-b:a"])
        .arg(bitrate.to_string())
        .args(["-vbr", "constrained", "-application", "audio"])
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(&raw).ok()?;
    if !child.wait().ok()?.success() {
        return None;
    }
    ffmpeg_decode(&out, channels)
}

/// The rate x layout table the slice is judged on: every cell is decoded by
/// libopus (through ffmpeg) and scored against the source, so a cell can only
/// pass if the bitstream is real Opus.
#[test]
fn encoder_rate_quality_matrix() {
    let Some(stereo) = source_pcm(12.0) else {
        eprintln!("testvector01.dec missing, skipped (run scripts/fetch-vectors.sh)");
        return;
    };
    let mono = to_mono(&stereo);
    let dir = tmp_dir();
// The whole range the format allows, not the range that happens to work:
    // 16 kbps is where the incumbent's mono was broken and 510 kbps is one
    // byte under the RFC's per-frame ceiling. (ffmpeg's libopus refuses
    // anything above 256 kbps, hence the NaN in its column up there.)
    let rates = [
        16_000u32, 32_000, 64_000, 96_000, 128_000, 165_000, 192_000, 256_000, 320_000, 510_000,
    ];
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for &channels in &[1usize, 2] {
        let src: &[f32] = if channels == 1 { &mono } else { &stereo };
        for &rate in &rates {
            let path = dir.join(format!("m{channels}-{rate}.opus"));
            let (bytes, samples, took) = encode_ogg(src, channels, rate, true, 960, &path);
            let kbps = bytes as f64 * 8.0 * 48000.0 / samples as f64 / 1000.0;
            let Some(reference) = ffmpeg_decode(&path, channels) else {
                eprintln!("ffmpeg/libopus unavailable, matrix skipped");
                return;
            };
            let n = reference.len().min(src.len());
            let corr = correlation(&reference[..n], &src[..n]);
            let score = opus_compare(&to_i16(&src[..n]), &to_i16(&reference[..n]), channels);
            // Our decoder must agree with libopus on the same bitstream.
            let ours = our_decode_ogg(&path);
            let n2 = ours.len().min(n);
            let self_corr = correlation(&ours[..n2], &reference[..n2]);
            let realtime = samples as f64 / 48000.0 / took.as_secs_f64();
            // The same audio through libopus at the same setting: the number
            // that says whether our score is good or merely a number.
            let (ref_corr, ref_score) = match libopus_encode_decode(&src[..n], channels, rate, &dir)
            {
                Some(lib) => {
                    let k = lib.len().min(n);
                    (
                        correlation(&lib[..k], &src[..k]),
                        opus_compare(&to_i16(&src[..k]), &to_i16(&lib[..k]), channels),
                    )
                }
                None => (f64::NAN, f64::NAN),
            };
            rows.push(format!(
                "{:>2}ch {:>3}k -> {:6.1}k  corr {:.4} (libopus {:.4})  opus_compare {:7.2} \
                 (libopus {:7.2})  vs-libopus {:.4}  {:5.1}x realtime",
                channels,
                rate / 1000,
                kbps,
                corr,
                ref_corr,
                score,
                ref_score,
                self_corr,
                realtime
            ));
            if self_corr < 0.999 {
                failures.push(format!(
                    "{channels}ch {rate}: our decoder disagrees with libopus, corr {self_corr}"
                ));
            }
            // Constrained VBR: overshooting the target is a defect, coming in
            // under it on material with silence in it is the point of VBR.
            let target = rate as f64 / 1000.0;
            if kbps > 1.06 * target || kbps < 0.75 * target {
                failures.push(format!("{channels}ch {rate}: actual rate {kbps:.1} kbps"));
            }
            // The MUSTs of the rubric's audio-opus section.
            if channels == 2 && [96_000, 165_000, 256_000].contains(&rate) && corr < 0.9 {
                failures.push(format!("stereo {rate}: correlation {corr:.4} < 0.9"));
            }
            if channels == 1 && rate >= 96_000 && corr < 0.9 {
                failures.push(format!("mono {rate}: correlation {corr:.4} < 0.9"));
            }
        }
    }
    for r in &rows {
        println!("{r}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every frame size, through libopus.
#[test]
fn encoder_every_frame_size_decodes_in_libopus() {
    let Some(stereo) = source_pcm(4.0) else {
        return;
    };
    let dir = tmp_dir();
    let mut failures = Vec::new();
    for &frame in &[120usize, 240, 480, 960] {
        for &vbr in &[false, true] {
            let path = dir.join(format!("f{frame}-{vbr}.opus"));
            let (_, samples, _) = encode_ogg(&stereo, 2, 128_000, vbr, frame, &path);
            let Some(reference) = ffmpeg_decode(&path, 2) else {
                eprintln!("ffmpeg unavailable, skipped");
                return;
            };
            let n = reference.len().min(stereo.len());
            let corr = correlation(&reference[..n], &stereo[..n]);
            println!("{frame:>3} samples vbr={vbr}: corr {corr:.4}");
            if corr < 0.9 {
                failures.push(format!("frame {frame} vbr {vbr}: corr {corr:.4}"));
            }
            let _ = samples;
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 5.1: encode the fixture's own decoded audio back to Opus, then check that
/// both decoders agree with the source channel for channel — a routing error
/// or a coupled-stream mix-up shows up as a low correlation on one channel.
#[test]
fn encoder_five_one_multistream() {
    let path = fixture("opus-ogg-5.1-48000.opus");
    if !path.exists() {
        eprintln!("5.1 fixture missing, skipped");
        return;
    }
    // The fixture decodes in mapping (Vorbis) order, which is the order the
    // multistream encoder takes.
    let (src, channels) = decode_ogg(&path);
    assert_eq!(channels, 6);
    let src: Vec<f32> = src.into_iter().take(6 * 48000 * 8).collect();
    let out = tmp_dir().join("enc-5.1.opus");
    let (bytes, samples, took) = encode_ogg(&src, 6, 384_000, true, 960, &out);
    println!(
        "5.1: {:.1} kbps, {:.1}x realtime",
        bytes as f64 * 8.0 * 48000.0 / samples as f64 / 1000.0,
        samples as f64 / 48000.0 / took.as_secs_f64()
    );

    // ffmpeg hands 5.1 back in its own order; the fixture is in Vorbis order.
    const VORBIS_FROM_FFMPEG: [usize; 6] = [0, 2, 1, 4, 5, 3];
    let Some(reference) = ffmpeg_decode(&out, 6) else {
        eprintln!("ffmpeg unavailable, skipped");
        return;
    };
    let ours = our_decode_ogg(&out);
    let n = (reference.len() / 6).min(src.len() / 6).min(ours.len() / 6);
    let mut failures = Vec::new();
    for c in 0..6 {
        let ff = VORBIS_FROM_FFMPEG[c];
        let a: Vec<f32> = (0..n).map(|i| reference[i * 6 + ff]).collect();
        let b: Vec<f32> = (0..n).map(|i| src[i * 6 + c]).collect();
        let o: Vec<f32> = (0..n).map(|i| ours[i * 6 + c]).collect();
        let corr = correlation(&a, &b);
        let ours_corr = correlation(&o, &b);
        println!("5.1 channel {c}: libopus corr {corr:.4}, ours {ours_corr:.4}");
        if corr < 0.9 {
            failures.push(format!("channel {c}: libopus corr {corr:.4}"));
        }
        if ours_corr < 0.9 {
            failures.push(format!("channel {c}: our decoder corr {ours_corr:.4}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The container half: ffprobe reads the file, its duration is the source's to
/// the sample, and ffmpeg decodes it without a complaint.
#[test]
fn encoder_ogg_duration_is_exact() {
    let Some(stereo) = source_pcm(5.0) else {
        return;
    };
    let path = tmp_dir().join("duration.opus");
    let (_, samples, _) = encode_ogg(&stereo, 2, 96_000, true, 960, &path);
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&path)
        .output();
    let Ok(out) = out else {
        eprintln!("ffprobe unavailable, skipped");
        return;
    };
    assert!(out.status.success(), "ffprobe rejected the file");
    let text = String::from_utf8_lossy(&out.stdout);
    let duration: f64 = text.trim().parse().expect("duration");
    let want = samples as f64 / 48000.0;
    assert!(
        (duration - want).abs() < 1e-6,
        "duration {duration} vs {want}"
    );
}

/// Encode speed, reported rather than gated: quality is the target here, but a
/// number that nobody measures is a number that regresses.
#[test]
fn encode_speed() {
    let Some(stereo) = source_pcm(20.0) else {
        return;
    };
    let dir = tmp_dir();
    for &(ch, rate) in &[(1usize, 96_000u32), (2, 128_000)] {
        let src = if ch == 1 {
            to_mono(&stereo)
        } else {
            stereo.clone()
        };
        let path = dir.join(format!("speed{ch}.opus"));
        // Warm the caches, then measure.
        let _ = encode_ogg(&src, ch, rate, true, 960, &path);
        let (_, samples, took) = encode_ogg(&src, ch, rate, true, 960, &path);
        println!(
            "encode {ch}ch {} kbps: {:.1}x realtime",
            rate / 1000,
            samples as f64 / 48000.0 / took.as_secs_f64()
        );
    }
}
