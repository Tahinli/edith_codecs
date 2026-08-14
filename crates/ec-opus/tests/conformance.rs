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
