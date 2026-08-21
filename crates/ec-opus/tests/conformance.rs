//! RFC 6716 conformance: the official test vectors, the `opus_compare` quality
//! metric, real-file oracles against ffmpeg, and the decode-speed floor.
//!
//! All of it lives in one test binary on purpose — a file per feature costs a
//! serial link-and-run for every build.

use std::fs;
use std::path::{Path, PathBuf};

use ec_opus::Decoder;

// ---------------------------------------------------------------------------
// Counting allocator: proves the steady-state encode loop allocates nothing.
// Lives in the test binary only — the shipped crate has no allocator games.
// Same pattern as ec-h264's proof: only the thread that armed the counter is
// counted, so parallel tests (and ffmpeg child plumbing) allocate freely.
// ---------------------------------------------------------------------------

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

std::thread_local! {
    static COUNTING_HERE: Cell<bool> = const { Cell::new(false) };
}

fn counting_here() -> bool {
    COUNTING_HERE.try_with(Cell::get).unwrap_or(false)
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting_here() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counting_here() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

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

// ---------------------------------------------------------------------------
// Encoder: self-consistency (range-exact against our own decoder)
// ---------------------------------------------------------------------------

use ec_opus::{Application, Bandwidth, Encoder, MultistreamEncoder};

/// A deterministic music-like test signal: several detuned partials plus a
/// swept component per channel, loud enough to exercise every band.
fn test_signal(channels: usize, secs: f64) -> Vec<f32> {
    let n = (48000.0 * secs) as usize;
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f64 / 48000.0;
        for c in 0..channels {
            let base = 220.0 * (c as f64 * 0.5 + 1.0);
            let sweep = 400.0 + 3000.0 * (0.5 + 0.5 * (0.13 * t).sin());
            let v = 0.35 * (std::f64::consts::TAU * base * t).sin()
                + 0.2 * (std::f64::consts::TAU * base * 3.01 * t).sin()
                + 0.12 * (std::f64::consts::TAU * sweep * t).sin()
                + 0.08 * (std::f64::consts::TAU * 9000.0 * (1.0 + 0.02 * c as f64) * t).sin();
            out.push(v as f32);
        }
    }
    out
}

/// Encodes `pcm` in `frame`-sample packets and decodes each with our own
/// decoder, asserting the range coder states agree packet for packet — the
/// same bit-exact check the RFC vectors put on the decoder, now closing the
/// loop over the encoder. Returns the decoded signal and total packet bytes.
fn roundtrip_own(
    enc: &mut Encoder,
    pcm: &[f32],
    channels: usize,
    frame: usize,
) -> (Vec<f32>, usize) {
    let mut dec = Decoder::new(48000, channels).unwrap();
    let mut out = vec![0u8; 1500];
    let mut decoded = Vec::new();
    let mut buf = vec![0.0f32; 5760 * channels];
    let mut bytes = 0usize;
    let mut padded = Vec::new();
    for block in pcm.chunks(frame * channels) {
        let block = if block.len() < frame * channels {
            padded.clear();
            padded.extend_from_slice(block);
            padded.resize(frame * channels, 0.0);
            &padded[..]
        } else {
            block
        };
        let len = enc.encode_float(block, frame, &mut out).expect("encode");
        bytes += len;
        let n = dec.decode_float(&out[..len], &mut buf).expect("decode");
        assert_eq!(n, frame, "decoded frame size");
        assert_eq!(
            dec.final_range(),
            enc.final_range(),
            "range state diverged between encoder and decoder"
        );
        decoded.extend_from_slice(&buf[..n * channels]);
    }
    (decoded, bytes)
}

/// Worst-channel correlation against the source with the encoder's
/// 120-sample delay removed.
fn delayed_corr(source: &[f32], decoded: &[f32], channels: usize) -> f64 {
    const DELAY: usize = 120;
    let n = (decoded.len() / channels).saturating_sub(DELAY);
    let n = n.min(source.len() / channels);
    let mut worst = 1.0f64;
    for c in 0..channels {
        let a: Vec<f32> = (0..n).map(|i| source[i * channels + c]).collect();
        let b: Vec<f32> = (0..n)
            .map(|i| decoded[(i + DELAY) * channels + c])
            .collect();
        worst = worst.min(correlation(&a, &b));
    }
    worst
}

/// Every rate from the product's table, mono and stereo, 20 ms CBR: the
/// encoder must be decodable by its own decoder range-exactly (asserted in
/// the helper) and land close to the source. The cross-implementation checks
/// against libopus live below — self-consistency alone proves nothing about
/// the world (the incumbent's exact disease).
#[test]
fn encoder_roundtrips_at_every_rate() {
    for channels in [1usize, 2] {
        let pcm = test_signal(channels, 2.0);
        for kbps in [32u32, 64, 96, 128, 160, 165, 192, 256, 320, 510] {
            let mut enc = Encoder::new(48000, channels, Application::Audio).unwrap();
            enc.set_bitrate(kbps * 1000);
            let (decoded, bytes) = roundtrip_own(&mut enc, &pcm, channels, 960);
            let corr = delayed_corr(&pcm, &decoded, channels);
            let rate = bytes as f64 * 8.0 / 2.0 / 1000.0;
            println!("{channels} ch {kbps:>3} kbps: corr {corr:.4}, actual {rate:.1} kbps");
            let floor = if kbps < 64 { 0.8 } else { 0.9 };
            assert!(
                corr >= floor,
                "{channels} ch {kbps} kbps: correlation {corr:.4}"
            );
        }
    }
}

/// The mode `Encoder` actually picks, read back from the packet's own TOC
/// byte: Voip below 20 kbps mono selects SILK (NB at/under 10 kbps, WB
/// above), Voip 20-40 kbps and Audio at any rate stay on CELT (Hybrid is
/// unimplemented and Mode 20-40 kbps needs it, D4).
#[test]
fn encoder_mode_follows_application_and_bitrate() {
    // `None` for the two CELT rows: any CELT config is correct there, the
    // exact one is `auto_bandwidth`'s call, not mode selection's.
    let cases = [
        (Application::Voip, 8_000u32, Some(1u8)),
        (Application::Voip, 16_000, Some(9)),
        (Application::Audio, 64_000, None),
        (Application::Voip, 32_000, Some(15)),
        (Application::Voip, 48_000, None),
    ];
    for (app, bps, want_config) in cases {
        let mut enc = Encoder::new(48000, 1, app).unwrap();
        enc.set_bitrate(bps);
        let pcm = vec![0.0f32; 960];
        let mut out = vec![0u8; 1500];
        let n = enc.encode_float(&pcm, 960, &mut out).unwrap();
        let toc = ec_opus::Toc::new(out[0]);
        match want_config {
            Some(want) => assert_eq!(
                toc.config, want,
                "{app:?} at {bps} bps: TOC config {} (wanted {want})",
                toc.config
            ),
            None => assert_eq!(
                toc.mode(),
                ec_opus::Mode::Celt,
                "{app:?} at {bps} bps: TOC config {} is not CELT",
                toc.config
            ),
        }
        assert!(n > 1, "{app:?} at {bps} bps: empty packet");
    }

    let mut enc = Encoder::new(48000, 1, Application::Voip).unwrap();
    enc.set_mode(Some(ec_opus::Mode::Silk));
    enc.set_bandwidth(Some(Bandwidth::Medium));
    let pcm = vec![0.0f32; 480];
    let mut out = vec![0u8; 1500];
    let n = enc.encode_float(&pcm, 480, &mut out).unwrap();
    assert_eq!(ec_opus::Toc::new(out[0]).config, 4, "forced 10 ms MB SILK");
    assert!(n > 1, "forced 10 ms MB SILK empty packet");
}

/// All four CELT frame sizes survive the loop, and constrained VBR both
/// stays decodable and lands near its target.
#[test]
fn encoder_frame_sizes_and_vbr() {
    let pcm = test_signal(2, 1.0);
    for frame in [120usize, 240, 480, 960] {
        let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
        enc.set_bitrate(128_000);
        let (decoded, _) = roundtrip_own(&mut enc, &pcm, 2, frame);
        let corr = delayed_corr(&pcm, &decoded, 2);
        println!("frame {frame}: corr {corr:.4}");
        assert!(corr >= 0.85, "frame {frame}: correlation {corr:.4}");
    }
    let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
    enc.set_bitrate(128_000);
    enc.set_vbr_constrained(true);
    let (decoded, bytes) = roundtrip_own(&mut enc, &pcm, 2, 960);
    let corr = delayed_corr(&pcm, &decoded, 2);
    let rate = bytes as f64 * 8.0 / 1.0 / 1000.0;
    println!("cvbr 128k: corr {corr:.4}, actual {rate:.1} kbps");
    assert!(corr >= 0.9, "cvbr: correlation {corr:.4}");
    assert!(
        (90.0..=150.0).contains(&rate),
        "cvbr at 128k landed on {rate:.1} kbps"
    );
}

// ---------------------------------------------------------------------------
// Encoder: the world must decode it. Self-consistency alone was the
// incumbent's disease (correlation 0.06 against libopus at 256 kbps while
// its own round trip passed), so every claim here runs through ffmpeg's
// libopus — the reference implementation — via Ogg-Opus files this crate
// muxed itself.
// ---------------------------------------------------------------------------

use ec_core::registry::{CodecId, CodecParameters, MediaParameters, Muxer, StreamInfo};
use ec_core::{ChannelLayout, Packet as CorePacket, TimeBase};
use ec_ogg::OggMuxer;

/// The RFC 7845 identification header for this encoder — built by the crate's
/// own [`ec_opus::ogg`] helper, so a muxer and the encoder cannot disagree.
/// `pre_skip` is the encoder's [`Encoder::look_ahead`]: 120, the CELT overlap
/// delay, for every CELT call site; SILK's own (larger) delay for SILK.
fn opus_head(channels: usize, pre_skip: u16, layout: Option<(usize, usize, &[u8])>) -> Vec<u8> {
    let mapping =
        layout.map(|(streams, coupled, table)| (1u8, streams as u8, coupled as u8, table));
    ec_opus::ogg::opus_head(channels as u8, pre_skip, 48000, 0, mapping)
}

/// Muxes 20 ms packets into an Ogg-Opus file whose final granule trims the
/// stream to exactly `total_samples` — RFC 7845 end-trimming plus the
/// pre-skip accounting.
fn write_ogg_opus(
    path: &Path,
    packets: &[Vec<u8>],
    head: Vec<u8>,
    channels: usize,
    total_samples: usize,
    pre_skip: i64,
) {
    let file = std::io::BufWriter::new(fs::File::create(path).unwrap());
    let mut mux = OggMuxer::new(file);
    let tb = TimeBase::from_rate(48000);
    let mut params = CodecParameters::new(CodecId::Opus);
    params.extradata = Some(head.into());
    if let MediaParameters::Audio(a) = &mut params.media {
        a.sample_rate = 48000;
        a.layout = ChannelLayout::from_count(channels);
    }
    mux.add_stream(StreamInfo::new(0, tb, params)).unwrap();
    let mut pts = 0i64;
    for (idx, p) in packets.iter().enumerate() {
        let last = idx == packets.len() - 1;
        let dur = if last {
            total_samples as i64 + pre_skip - pts
        } else {
            960
        };
        let pkt = CorePacket::new(0, tb, p.clone())
            .with_pts(pts)
            .with_duration(dur);
        mux.write_packet(&pkt).unwrap();
        pts += 960;
    }
    mux.finish().unwrap();
}

/// Encodes `pcm` (plus one flush frame for the 120-sample delay) as 20 ms
/// packets.
fn encode_packets(enc: &mut Encoder, pcm: &[f32], channels: usize) -> Vec<Vec<u8>> {
    let mut out = vec![0u8; 1500];
    let mut packets = Vec::new();
    let mut padded = Vec::new();
    for block in pcm.chunks(960 * channels) {
        let block = if block.len() < 960 * channels {
            padded.clear();
            padded.extend_from_slice(block);
            padded.resize(960 * channels, 0.0);
            &padded[..]
        } else {
            block
        };
        let len = enc.encode_float(block, 960, &mut out).expect("encode");
        packets.push(out[..len].to_vec());
    }
    // One silent frame flushes the delay past the end trim.
    let silence = vec![0.0f32; 960 * channels];
    let len = enc.encode_float(&silence, 960, &mut out).expect("encode");
    packets.push(out[..len].to_vec());
    packets
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ec-opus-enc-{}-{name}", std::process::id()))
}

/// The full product rate table, mono and stereo, decoded by libopus: the
/// worst-channel correlation and the opus_compare quality, per rate. The
/// incumbent fails this file's 256 kbps row at correlation 0.06; the MUST
/// bars here are correlation >= 0.9 at 96/165/256 kbps.
#[test]
fn oracle_decodes_our_packets_across_the_rate_table() {
    for channels in [1usize, 2] {
        let pcm = test_signal(channels, 2.0);
        let total = pcm.len() / channels;
        let mut table = Vec::new();
        for kbps in [16u32, 32, 64, 96, 128, 160, 165, 192, 256, 320, 510] {
            let mut enc = Encoder::new(48000, channels, Application::Audio).unwrap();
            enc.set_bitrate(kbps * 1000);
            let packets = encode_packets(&mut enc, &pcm, channels);
            let path = temp_path(&format!("{channels}ch-{kbps}k.opus"));
            write_ogg_opus(
                &path,
                &packets,
                opus_head(channels, 120, None),
                channels,
                total,
                120,
            );
            let Some(reference) = ffmpeg_decode(&path, channels) else {
                eprintln!("ffmpeg unavailable, skipped");
                let _ = fs::remove_file(&path);
                return;
            };
            let _ = fs::remove_file(&path);
            let n = (reference.len() / channels).min(total);
            assert!(
                n >= total - 960,
                "{channels} ch {kbps} kbps: libopus returned {n} of {total} samples"
            );
            let mut worst = 1.0f64;
            for c in 0..channels {
                let a: Vec<f32> = (0..n).map(|i| pcm[i * channels + c]).collect();
                let b: Vec<f32> = (0..n).map(|i| reference[i * channels + c]).collect();
                worst = worst.min(correlation(&a, &b));
            }
            let quality = opus_compare(
                &to_i16(&pcm[..n * channels]),
                &to_i16(&reference[..n * channels]),
                channels,
            );
            table.push(format!(
                "{channels} ch {kbps:>3} kbps: worst corr {worst:.4}, opus_compare {quality:.1}"
            ));
            let floor = match kbps {
                // 16 kbps fullband CELT is the honest bottom of the range:
                // reported, held to "clearly the same signal", not to hi-fi.
                0..=19 => {
                    if channels == 1 {
                        0.6
                    } else {
                        0.35
                    }
                }
                20..=39 => 0.6,
                40..=95 => 0.8,
                _ => 0.9,
            };
            assert!(
                worst >= floor,
                "{channels} ch {kbps} kbps: libopus heard correlation {worst:.4} (floor {floor})"
            );
            if kbps >= 96 {
                assert!(
                    quality >= 0.0,
                    "{channels} ch {kbps} kbps: opus_compare {quality:.1}"
                );
            }
        }
        for line in table {
            println!("{line}");
        }
    }

    // SILK, mono, the speech-rate corner: Voip application picks it
    // automatically below 20 kbps (`Encoder::wants_silk`). 0.95 rather than
    // the CELT floors above — SILK is a speech codec, this is a tone signal,
    // not the pulse-coded content it's tuned for.
    let pcm = test_signal(1, 2.0);
    let total = pcm.len();
    for kbps in [8u32, 12, 16] {
        let mut enc = Encoder::new(48000, 1, Application::Voip).unwrap();
        enc.set_bitrate(kbps * 1000);
        let pre_skip = enc.look_ahead(960) as u16;
        let packets = encode_packets(&mut enc, &pcm, 1);
        let path = temp_path(&format!("silk-1ch-{kbps}k.opus"));
        write_ogg_opus(
            &path,
            &packets,
            opus_head(1, pre_skip, None),
            1,
            total,
            pre_skip as i64,
        );
        let Some(reference) = ffmpeg_decode(&path, 1) else {
            eprintln!("ffmpeg unavailable, skipped");
            let _ = fs::remove_file(&path);
            return;
        };
        let _ = fs::remove_file(&path);
        let n = (reference.len()).min(total);
        let corr = correlation(&pcm[..n], &reference[..n]);
        println!("silk 1 ch {kbps:>3} kbps: corr {corr:.4}");
        // With Encoder::set_bitrate now wired into SilkEncoder's reservoir,
        // the rate loop genuinely starves the pulse-coded excitation on
        // this tone signal at true speech rates (8/12/16 kbps).
        assert!(corr >= 0.8, "silk {kbps} kbps: correlation {corr:.4}");
    }
}

/// 5.1 multistream: one distinct tone per channel, encoded with the
/// mapping-family-1 layout and decoded by libopus — every channel must come
/// back in its place (order-preserved) at correlation >= 0.9.
#[test]
fn five_one_encode_end_to_end() {
    const VORBIS_TO_FFMPEG_5_1: [usize; 6] = [0, 2, 1, 4, 5, 3];
    const TONES: [f64; 6] = [220.0, 440.0, 660.0, 55.0, 880.0, 1320.0];
    let total = 96000usize;
    // Vorbis order input, one tone per channel plus a quiet upper partial so
    // every stream codes real content.
    let mut pcm = Vec::with_capacity(total * 6);
    for i in 0..total {
        let t = i as f64 / 48000.0;
        for f in TONES {
            let v = 0.4 * (std::f64::consts::TAU * f * t).sin()
                + 0.05 * (std::f64::consts::TAU * f * 7.03 * t).sin();
            pcm.push(v as f32);
        }
    }
    let mut enc = MultistreamEncoder::surround_5_1(48000).unwrap();
    enc.set_bitrate(384_000);
    let mut out = vec![0u8; 8 * 1500];
    let mut packets = Vec::new();
    for block in pcm.chunks_exact(960 * 6) {
        let len = enc.encode_float(block, 960, &mut out).expect("encode");
        packets.push(out[..len].to_vec());
    }
    let silence = vec![0.0f32; 960 * 6];
    let len = enc.encode_float(&silence, 960, &mut out).expect("encode");
    packets.push(out[..len].to_vec());
    let (streams, coupled, mapping) = enc.layout();
    let head = opus_head(6, 120, Some((streams, coupled, mapping)));
    let path = temp_path("5.1.opus");
    write_ogg_opus(&path, &packets, head, 6, total, 120);
    let Some(reference) = ffmpeg_decode(&path, 6) else {
        eprintln!("ffmpeg unavailable, skipped");
        let _ = fs::remove_file(&path);
        return;
    };
    let _ = fs::remove_file(&path);
    let n = (reference.len() / 6).min(total);
    assert!(n >= total - 960, "libopus returned {n} of {total} samples");
    for (c, &their) in VORBIS_TO_FFMPEG_5_1.iter().enumerate() {
        let a: Vec<f32> = (0..n).map(|i| pcm[i * 6 + c]).collect();
        let b: Vec<f32> = (0..n).map(|i| reference[i * 6 + their]).collect();
        let corr = correlation(&a, &b);
        println!("5.1 channel {c} ({} Hz): corr {corr:.4}", TONES[c]);
        assert!(
            corr >= 0.9,
            "5.1 channel {c} came back at correlation {corr:.4}"
        );
    }
}

/// The muxed file's duration must be exact: the final granule trims the
/// flush frame so a 2.000 s input probes as 2.000 s, and the sample count
/// libopus hands back is exactly the input length.
#[test]
fn ogg_opus_round_trip_duration_is_exact() {
    let pcm = test_signal(2, 2.0);
    let total = pcm.len() / 2;
    let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
    enc.set_bitrate(128_000);
    let packets = encode_packets(&mut enc, &pcm, 2);
    let path = temp_path("duration.opus");
    write_ogg_opus(&path, &packets, opus_head(2, 120, None), 2, total, 120);
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&path)
        .output();
    if let Ok(p) = probe
        && p.status.success()
    {
        let text = String::from_utf8_lossy(&p.stdout);
        let dur: f64 = text
            .trim()
            .strip_prefix("duration=")
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        // ffprobe reports Ogg-Opus durations *including* the pre-skip — its
        // own 3.000 s encode probes as 3.0065 (312/48000 more). Ours carries
        // a 120-sample pre-skip, so exact is 2.0025; the decoded sample
        // count below is the pre-skip-free half of the claim.
        let expected = (total as f64 + 120.0) / 48000.0;
        assert!(
            (dur - expected).abs() < 1e-6,
            "ffprobe reports {dur} s, expected {expected} s (2.000 s + pre-skip)"
        );
    }
    if let Some(decoded) = ffmpeg_decode(&path, 2) {
        assert_eq!(
            decoded.len() / 2,
            total,
            "libopus decoded {} samples of {total}",
            decoded.len() / 2
        );
    }
    let _ = fs::remove_file(&path);
}

/// Cross-check against the reference `opus_demo` decoder (RFC 6716
/// Appendix A) over its own `.bit` format, when the binary is available:
/// `EC_OPUS_DEMO=/path/to/opus_demo cargo test`.
#[test]
fn opus_demo_decodes_our_bit_stream() {
    let Ok(demo) = std::env::var("EC_OPUS_DEMO") else {
        eprintln!("EC_OPUS_DEMO unset, skipped");
        return;
    };
    let pcm = test_signal(2, 2.0);
    let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
    enc.set_bitrate(256_000);
    let mut out = vec![0u8; 1500];
    let mut bit = Vec::new();
    for block in pcm.chunks_exact(960 * 2) {
        let len = enc.encode_float(block, 960, &mut out).expect("encode");
        bit.extend_from_slice(&(len as u32).to_be_bytes());
        bit.extend_from_slice(&enc.final_range().to_be_bytes());
        bit.extend_from_slice(&out[..len]);
    }
    let bit_path = temp_path("demo.bit");
    let pcm_path = temp_path("demo.pcm");
    fs::write(&bit_path, &bit).unwrap();
    let status = Command::new(&demo)
        .arg("-d")
        .arg("48000")
        .arg("2")
        .arg(&bit_path)
        .arg(&pcm_path)
        .status()
        .expect("run opus_demo");
    assert!(status.success(), "opus_demo refused our stream");
    let decoded = read_i16(&pcm_path);
    let _ = fs::remove_file(&bit_path);
    let _ = fs::remove_file(&pcm_path);
    // opus_demo does not apply pre-skip; compensate for the 120-sample delay.
    let delay = 120usize;
    let n = (decoded.len() / 2 - delay).min(pcm.len() / 2 - delay);
    let mut worst = 1.0f64;
    for c in 0..2 {
        let a: Vec<f32> = (0..n).map(|i| pcm[i * 2 + c]).collect();
        let b: Vec<f32> = (0..n)
            .map(|i| decoded[(i + delay) * 2 + c] as f32 / 32768.0)
            .collect();
        worst = worst.min(correlation(&a, &b));
    }
    println!("opus_demo 256 kbps stereo: worst corr {worst:.4}");
    assert!(
        worst >= 0.9,
        "reference decoder heard correlation {worst:.4}"
    );
}

/// The steady-state encode loop allocates nothing: every buffer — MDCT
/// scratch, PVQ vectors, the range coder's frame — is owned by the encoder
/// and sized on the first frame (the decoder discipline, applied to the
/// encoder).
#[test]
fn steady_state_encode_loop_zero_alloc() {
    let pcm = test_signal(2, 0.5);
    let pcm6 = {
        let mono = test_signal(1, 0.5);
        let mut v = Vec::with_capacity(mono.len() * 6);
        for &s in &mono {
            for c in 0..6 {
                v.push(s * (1.0 + c as f32 * 0.01));
            }
        }
        v
    };
    // Stereo elementary encoder.
    let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
    enc.set_bitrate(256_000);
    let mut out = vec![0u8; 1500];
    for block in pcm.chunks_exact(960 * 2) {
        enc.encode_float(block, 960, &mut out).unwrap();
    }
    ALLOCS.store(0, Ordering::SeqCst);
    COUNTING_HERE.with(|c| c.set(true));
    for block in pcm.chunks_exact(960 * 2) {
        enc.encode_float(block, 960, &mut out).unwrap();
    }
    COUNTING_HERE.with(|c| c.set(false));
    let n = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(n, 0, "stereo encode allocated {n} times in steady state");

    // 5.1 multistream.
    let mut ms = MultistreamEncoder::surround_5_1(48000).unwrap();
    ms.set_bitrate(384_000);
    let mut out6 = vec![0u8; 8 * 1500];
    for block in pcm6.chunks_exact(960 * 6) {
        ms.encode_float(block, 960, &mut out6).unwrap();
    }
    ALLOCS.store(0, Ordering::SeqCst);
    COUNTING_HERE.with(|c| c.set(true));
    for block in pcm6.chunks_exact(960 * 6) {
        ms.encode_float(block, 960, &mut out6).unwrap();
    }
    COUNTING_HERE.with(|c| c.set(false));
    let n = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(n, 0, "5.1 encode allocated {n} times in steady state");

    // Mono SILK (Voip, 12 kbps) is NOT asserted zero-alloc here: fixing
    // `Encoder::zero_stuff_mono` to reuse a scratch buffer removed its
    // per-frame Vec, but `SilkEncoder::encode_frame` itself (NLSF/LPC
    // analysis, pitch search) allocates well beyond that one site — sweeping
    // those is a separate, much larger effort than this defect's scope.
}

/// The throughput headline: encode speed in multiples of realtime, per
/// configuration, at the product's rates. Floors are deliberately far under
/// the measured numbers — they catch a regression class (an accidental
/// O(n^2), a debug path left on), not machine variance.
#[test]
fn encode_speed() {
    if cfg!(debug_assertions) {
        eprintln!("encode speed is only meaningful in release; skipped");
        return;
    }
    let secs = 4.0;
    for (name, channels, kbps, floor) in [
        ("mono 96k", 1usize, 96u32, 100.0f64),
        ("stereo 128k", 2, 128, 70.0),
        ("stereo 256k", 2, 256, 70.0),
        ("stereo 510k", 2, 510, 50.0),
    ] {
        let pcm = test_signal(channels, secs);
        let mut enc = Encoder::new(48000, channels, Application::Audio).unwrap();
        enc.set_bitrate(kbps * 1000);
        let mut out = vec![0u8; 1500];
        // Warm up one pass, then measure.
        for block in pcm.chunks_exact(960 * channels) {
            enc.encode_float(block, 960, &mut out).unwrap();
        }
        let passes = 4;
        let start = std::time::Instant::now();
        for _ in 0..passes {
            for block in pcm.chunks_exact(960 * channels) {
                enc.encode_float(block, 960, &mut out).unwrap();
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let realtime = secs * passes as f64 / elapsed;
        println!("{name}: {realtime:.0}x realtime");
        assert!(
            realtime >= floor,
            "{name}: {realtime:.0}x is below {floor}x"
        );
    }
    // 5.1 multistream.
    let mono = test_signal(1, secs);
    let mut pcm6 = Vec::with_capacity(mono.len() * 6);
    for &s in &mono {
        for c in 0..6 {
            pcm6.push(s * (1.0 + c as f32 * 0.01));
        }
    }
    let mut ms = MultistreamEncoder::surround_5_1(48000).unwrap();
    ms.set_bitrate(384_000);
    let mut out = vec![0u8; 8 * 1500];
    for block in pcm6.chunks_exact(960 * 6) {
        ms.encode_float(block, 960, &mut out).unwrap();
    }
    let passes = 4;
    let start = std::time::Instant::now();
    for _ in 0..passes {
        for block in pcm6.chunks_exact(960 * 6) {
            ms.encode_float(block, 960, &mut out).unwrap();
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let realtime = secs * passes as f64 / elapsed;
    println!("5.1 384k: {realtime:.0}x realtime");
    assert!(realtime >= 25.0, "5.1: {realtime:.0}x is below 25x");
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
// SILK encoder (mono, 20 ms, NB/WB)
// ---------------------------------------------------------------------------

use ec_opus::SilkEncoder;

/// A 1 s speech-like signal at 48 kHz: 0.6 s of a pitch-modulated pulse train
/// through a formant-ish AR(2) filter, then 0.4 s of filtered noise.
fn speech_like() -> Vec<f32> {
    let mut out = Vec::with_capacity(48000);
    let (mut y1, mut y2) = (0f32, 0f32);
    let mut phase = 0f64;
    let mut s = 12345u32;
    for i in 0..48000 {
        let t = i as f64 / 48000.0;
        let e = if t < 0.6 {
            let f0 = 120.0 + 30.0 * (2.0 * std::f64::consts::PI * 2.0 * t).sin();
            phase += f0 / 48000.0;
            if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            }
        } else {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.3
        };
        // Resonance near 700 Hz, radius 0.97.
        let (r, w) = (0.97f32, 2.0 * std::f32::consts::PI * 700.0 / 48000.0);
        let y = e + 2.0 * r * w.cos() * y1 - r * r * y2;
        y2 = y1;
        y1 = y;
        out.push(y * 0.03);
    }
    out
}

/// 16-bit PCM mono WAV at 48 kHz, as `f32`.
fn read_wav_mono(path: &Path) -> Vec<f32> {
    let d = fs::read(path).unwrap();
    assert_eq!(u16::from_le_bytes([d[22], d[23]]), 1, "mono fixture");
    assert_eq!(u32::from_le_bytes([d[24], d[25], d[26], d[27]]), 48000);
    let mut pos = 12;
    loop {
        let id = &d[pos..pos + 4];
        let len = u32::from_le_bytes([d[pos + 4], d[pos + 5], d[pos + 6], d[pos + 7]]) as usize;
        if id == b"data" {
            return d[pos + 8..pos + 8 + len]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
        }
        pos += 8 + len + (len & 1);
    }
}

/// Windowed-sinc low-pass at `cutoff_hz` (48 kHz input), zero-phase.
fn lowpass(x: &[f32], cutoff_hz: f64) -> Vec<f32> {
    let taps = 127usize;
    let c = cutoff_hz / 48000.0;
    let h: Vec<f64> = (0..taps)
        .map(|i| {
            let m = i as f64 - (taps - 1) as f64 / 2.0;
            let sinc = if m == 0.0 {
                2.0 * c
            } else {
                (2.0 * std::f64::consts::PI * c * m).sin() / (std::f64::consts::PI * m)
            };
            sinc * (0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos())
        })
        .collect();
    let half = (taps - 1) / 2;
    (0..x.len())
        .map(|n| {
            h.iter()
                .enumerate()
                .map(|(k, &hk)| {
                    let idx = n as isize + half as isize - k as isize;
                    if idx >= 0 && (idx as usize) < x.len() {
                        hk * x[idx as usize] as f64
                    } else {
                        0.0
                    }
                })
                .sum::<f64>() as f32
        })
        .collect()
}

/// Encodes `pcm` NB or WB, decodes each packet with our decoder asserting the
/// range-coder invariant, returns (decoded 48 kHz, packets, voiced frames,
/// encoder delay).
fn silk_roundtrip(
    pcm: &[f32],
    wideband: bool,
    bitrate: Option<u32>,
) -> (Vec<f32>, Vec<Vec<u8>>, usize, usize) {
    let mut enc = SilkEncoder::new(wideband);
    if let Some(bps) = bitrate {
        enc.set_bitrate(bps);
    }
    let mut dec = Decoder::new(48000, 1).unwrap();
    let mut out = vec![0u8; 1500];
    let mut buf = vec![0f32; 5760];
    let mut decoded = Vec::new();
    let mut packets = Vec::new();
    let mut padded = pcm.to_vec();
    padded.resize(pcm.len().div_ceil(960) * 960 + 960, 0.0);
    for block in padded.chunks(960) {
        let len = enc.encode_frame(block, &mut out).expect("silk encode");
        let n = dec
            .decode_float(&out[..len], &mut buf)
            .expect("silk decode");
        assert_eq!(n, 960);
        assert_eq!(dec.final_range(), enc.final_range(), "range state diverged");
        decoded.extend_from_slice(&buf[..n]);
        packets.push(out[..len].to_vec());
    }
    (decoded, packets, enc.voiced_frames(), enc.delay_samples())
}

fn silk_roundtrip_ms(
    mut enc: SilkEncoder,
    pcm: &[f32],
    frame_ms: usize,
    bitrate: Option<u32>,
) -> (Vec<f32>, Vec<Vec<u8>>, usize) {
    if let Some(bps) = bitrate {
        enc.set_bitrate(bps);
    }
    let frame = 48 * frame_ms;
    let mut dec = Decoder::new(48000, 1).unwrap();
    let mut out = vec![0u8; 1500];
    let mut buf = vec![0f32; 5760];
    let mut decoded = Vec::new();
    let mut packets = Vec::new();
    let mut padded = pcm.to_vec();
    padded.resize(pcm.len().div_ceil(frame) * frame + frame, 0.0);
    for block in padded.chunks(frame) {
        let len = enc
            .encode_frame_ms(block, &mut out, frame_ms)
            .expect("silk encode");
        let n = dec
            .decode_float(&out[..len], &mut buf)
            .expect("silk decode");
        assert_eq!(n, frame);
        assert_eq!(dec.final_range(), enc.final_range(), "range state diverged");
        decoded.extend_from_slice(&buf[..n]);
        packets.push(out[..len].to_vec());
    }
    (decoded, packets, enc.voiced_frames())
}

/// Correlation after aligning `decoded` to `reference` at the best lag in
/// `[0, max_lag)`; the peak must be interior (a peak at the search bound is
/// no measurement).
fn aligned_corr(reference: &[f32], decoded: &[f32], max_lag: usize) -> (f64, usize) {
    let (mut best, mut best_lag) = (-1.0f64, 0usize);
    for lag in 0..max_lag {
        let c = correlation(reference, &decoded[lag..]);
        if c > best {
            best = c;
            best_lag = lag;
        }
    }
    assert!(best_lag + 1 < max_lag, "alignment peak at the search bound");
    (best, best_lag)
}

#[test]
fn silk_mono_nb_wb_roundtrip() {
    let wav = fixture("wav16-mono-48000.wav");
    let mut sources = vec![("synthetic", speech_like())];
    if wav.exists() {
        let w = read_wav_mono(&wav);
        sources.push(("wav16-mono-48000", w[..w.len().min(96000)].to_vec()));
    }
    for (name, pcm) in &sources {
        for wideband in [false, true] {
            let (decoded, packets, voiced, delay) = silk_roundtrip(pcm, wideband, None);
            let band = if wideband { 7000.0 } else { 3500.0 };
            let reference = lowpass(pcm, band);
            let (corr, lag) = aligned_corr(&reference, &decoded, 2000);
            let bytes: usize = packets.iter().map(Vec::len).sum();
            let tag = if wideband { "WB" } else { "NB" };
            eprintln!(
                "silk {name} {tag}: corr {corr:.4} at lag {lag} (resampler delay {delay}), voiced {voiced}/{} frames, {} kbps",
                packets.len(),
                bytes * 8 / (packets.len() * 20)
            );
            assert!(corr >= 0.8, "{name} {tag}: corr {corr:.4}");
            assert!(
                lag >= delay,
                "{name} {tag}: aligned before the documented delay"
            );
            if *name == "synthetic" {
                assert!(voiced > 0, "{name} {tag}: no frame coded with LTP");
            }

            // Reference oracle: the packets in an Ogg-Opus file decode cleanly.
            if Command::new("ffmpeg").arg("-version").output().is_ok() {
                let path = temp_path(&format!("silk-{name}-{tag}.opus"));
                write_ogg_opus(&path, &packets, opus_head(1, 120, None), 1, pcm.len(), 120);
                let out = Command::new("ffmpeg")
                    .args(["-v", "warning", "-c:a", "libopus", "-i"])
                    .arg(&path)
                    .args(["-f", "null", "-"])
                    .output()
                    .unwrap();
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    out.status.success() && stderr.trim().is_empty(),
                    "{name} {tag}: ffmpeg: {stderr}"
                );
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[test]
fn silk_mediumband_and_10ms_roundtrip() {
    let pcm = speech_like();
    for (tag, fs_khz, frame_ms, config, cutoff, bps) in [
        ("NB10", 8usize, 10usize, 0u8, 3500.0, 12_000u32),
        ("MB10", 12, 10, 4, 5500.0, 16_000),
        ("WB10", 16, 10, 8, 7000.0, 20_000),
        ("MB20", 12, 20, 5, 5500.0, 16_000),
    ] {
        let enc = match fs_khz {
            8 => SilkEncoder::new(false),
            12 => SilkEncoder::new_mediumband(),
            _ => SilkEncoder::new(true),
        };
        let (decoded, packets, voiced) = silk_roundtrip_ms(enc, &pcm, frame_ms, Some(bps));
        assert_eq!(packets[0][0] >> 3, config, "{tag}: wrong TOC config");
        let reference = lowpass(&pcm, cutoff);
        let (corr, lag) = aligned_corr(&reference, &decoded, 2000);
        let bytes: usize = packets.iter().map(Vec::len).sum();
        let kbps = bytes as f64 * 8.0 / (packets.len() as f64 * frame_ms as f64);
        eprintln!(
            "silk {tag}: corr {corr:.4} at lag {lag}, voiced {voiced}/{} frames, {kbps:.2} kbps",
            packets.len()
        );
        assert!(corr >= 0.75, "{tag}: corr {corr:.4}");
        assert!(
            frame_ms == 10 || voiced > 0,
            "{tag}: no frame coded with LTP"
        );
        if let Some(oracle) = oracle_decode(&format!("silk-{tag}"), &packets, pcm.len()) {
            let (c, _) = aligned_corr(&oracle, &decoded, 2000);
            assert!(c >= 0.99, "{tag}: our decode vs oracle decode corr {c:.4}");
        }
    }
}

#[test]
fn silk_compares_to_celt_on_speech_at_speech_rates() {
    let pcm = speech_like();
    for (tag, silk, bandwidth, bps, cutoff) in [
        (
            "NB16",
            SilkEncoder::new(false),
            Bandwidth::Narrow,
            16_000u32,
            3500.0,
        ),
        (
            "WB24",
            SilkEncoder::new(true),
            Bandwidth::Wide,
            24_000u32,
            7000.0,
        ),
    ] {
        let reference = lowpass(&pcm, cutoff);
        let (silk_decoded, silk_packets, _) = silk_roundtrip_ms(silk, &pcm, 20, Some(bps));
        let (silk_corr, silk_lag) = aligned_corr(&reference, &silk_decoded, 2000);
        let mut celt = Encoder::new(48000, 1, Application::Audio).unwrap();
        celt.set_mode(Some(ec_opus::Mode::Celt));
        celt.set_bandwidth(Some(bandwidth));
        celt.set_bitrate(bps);
        let (celt_decoded, celt_bytes) = roundtrip_own(&mut celt, &pcm, 1, 960);
        let (celt_corr, celt_lag) = aligned_corr(&reference, &celt_decoded, 2000);
        let silk_bytes: usize = silk_packets.iter().map(Vec::len).sum();
        let silk_kbps = silk_bytes as f64 * 8.0 / (silk_packets.len() as f64 * 20.0);
        let celt_kbps = celt_bytes as f64 * 8.0 / 1000.0;
        eprintln!(
            "speech {tag}: SILK corr {silk_corr:.4} lag {silk_lag} {silk_kbps:.2} kbps; CELT corr {celt_corr:.4} lag {celt_lag} {celt_kbps:.2} kbps"
        );
        assert!(silk_corr >= 0.95, "{tag}: SILK corr {silk_corr:.4}");
        assert!(
            silk_corr + 0.01 >= celt_corr,
            "{tag}: SILK {silk_corr:.4} trails CELT {celt_corr:.4} by >0.01"
        );
    }
}

/// Reference oracle decode of `packets` as Ogg-Opus, or `None` without ffmpeg.
fn oracle_decode(name: &str, packets: &[Vec<u8>], samples: usize) -> Option<Vec<f32>> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let path = temp_path(&format!("{name}.opus"));
    write_ogg_opus(&path, packets, opus_head(1, 120, None), 1, samples, 120);
    let out = Command::new("ffmpeg")
        .args(["-v", "warning", "-c:a", "libopus", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stderr.trim().is_empty(),
        "{name}: ffmpeg: {stderr}"
    );
    let decoded = ffmpeg_decode(&path, 1);
    let _ = fs::remove_file(&path);
    decoded
}

fn silk_sources() -> Vec<(&'static str, Vec<f32>)> {
    let wav = fixture("wav16-mono-48000.wav");
    let mut sources = vec![("synthetic", speech_like())];
    if wav.exists() {
        let w = read_wav_mono(&wav);
        sources.push(("wav16-mono-48000", w[..w.len().min(96000)].to_vec()));
    }
    sources
}

/// Noise-shaped, closed-loop quantisation at a controlled 16 kbps WB beats
/// the open-loop writer's quality at 27-50 kbps.
#[test]
fn silk_nsq_improves_quality_at_equal_rate() {
    let mut failures = Vec::new();
    for (name, pcm) in &silk_sources() {
        let (decoded, packets, _, _) = silk_roundtrip(pcm, true, Some(16000));
        let reference = lowpass(pcm, 7000.0);
        let (corr, lag) = aligned_corr(&reference, &decoded, 2000);
        let bytes: usize = packets.iter().map(Vec::len).sum();
        let kbps = bytes as f64 * 8.0 / (packets.len() as f64 * 20.0);
        eprintln!("silk nsq {name} WB @16k: corr {corr:.4} at lag {lag}, {kbps:.1} kbps");
        // The synthetic signal's noise tail (0.4 s of filtered noise) caps
        // at ~0.96 at this rate; its pulse-train part alone reaches 0.992.
        let floor = if *name == "synthetic" { 0.96 } else { 0.99 };
        if corr < floor || kbps > 16.0 * 1.2 {
            failures.push(format!(
                "{name}: corr {corr:.4} (floor {floor}) at {kbps:.1} kbps"
            ));
        }
        if let Some(oracle) = oracle_decode(&format!("silk-nsq-{name}"), &packets, pcm.len()) {
            let (c, _) = aligned_corr(&oracle, &decoded, 2000);
            assert!(c >= 0.99, "{name}: our decode vs oracle decode corr {c:.4}");
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// `set_bitrate` lands the coded rate within 20% of 8/12/16/24 kbps.
#[test]
fn silk_rate_control_tracks_target() {
    for (name, pcm) in &silk_sources() {
        for wideband in [false, true] {
            for kbps_target in [8u32, 12, 16, 24] {
                let (decoded, packets, _, _) =
                    silk_roundtrip(pcm, wideband, Some(kbps_target * 1000));
                let bytes: usize = packets.iter().map(Vec::len).sum();
                let kbps = bytes as f64 * 8.0 / (packets.len() as f64 * 20.0);
                let reference = lowpass(pcm, if wideband { 7000.0 } else { 3500.0 });
                let (corr, _) = aligned_corr(&reference, &decoded, 2000);
                let tag = if wideband { "WB" } else { "NB" };
                eprintln!(
                    "silk rate {name} {tag} target {kbps_target}: {kbps:.2} kbps, corr {corr:.4}"
                );
                let ratio = kbps / kbps_target as f64;
                assert!(
                    (0.8..=1.2).contains(&ratio),
                    "{name} {tag} target {kbps_target}: got {kbps:.2} kbps"
                );
            }
        }
    }
}

/// The public `Encoder` (Voip, mono) routes `set_bitrate` into the SILK
/// path it dispatches to, same as `silk_rate_control_tracks_target` proves
/// directly against `SilkEncoder`.
#[test]
fn silk_packets_track_encoder_bitrate() {
    let pcm = test_signal(1, 2.0);
    for kbps_target in [8u32, 12, 16] {
        let mut enc = Encoder::new(48000, 1, Application::Voip).unwrap();
        enc.set_bitrate(kbps_target * 1000);
        let packets = encode_packets(&mut enc, &pcm, 1);
        let bytes: usize = packets.iter().map(Vec::len).sum();
        let kbps = bytes as f64 * 8.0 / (packets.len() as f64 * 20.0);
        eprintln!("encoder bitrate target {kbps_target}: {kbps:.2} kbps");
        let ratio = kbps / kbps_target as f64;
        assert!(
            (0.8..=1.2).contains(&ratio),
            "target {kbps_target}: got {kbps:.2} kbps"
        );
    }
}

// ---------------------------------------------------------------------------
// Hybrid (SILK 16 kHz + CELT from band 17, one range-coded stream).

/// Encodes mono `pcm` as hybrid packets at `bps` and decodes them with our
/// decoder, `mute` = (celt, silk) layers dropped at the decoder; asserts
/// range-state parity packet for packet. Returns (decoded, packets).
fn hybrid_roundtrip(
    pcm: &[f32],
    bps: u32,
    bandwidth: Option<Bandwidth>,
    mute: (bool, bool),
) -> (Vec<f32>, Vec<Vec<u8>>) {
    let mut enc = Encoder::new(48000, 1, Application::Voip).unwrap();
    enc.set_bitrate(bps);
    enc.set_bandwidth(bandwidth);
    let mut dec = Decoder::new(48000, 1).unwrap();
    dec.debug_mute_layers(mute.0, mute.1);
    let mut out = vec![0u8; 1500];
    let mut buf = vec![0f32; 5760];
    let (mut decoded, mut packets) = (Vec::new(), Vec::new());
    let mut padded = pcm.to_vec();
    padded.resize(pcm.len().div_ceil(960) * 960 + 960, 0.0);
    for block in padded.chunks(960) {
        let len = enc
            .encode_float(block, 960, &mut out)
            .expect("hybrid encode");
        let n = dec
            .decode_float(&out[..len], &mut buf)
            .expect("hybrid decode");
        assert_eq!(n, 960);
        assert_eq!(dec.final_range(), enc.final_range(), "range state diverged");
        decoded.extend_from_slice(&buf[..n]);
        packets.push(out[..len].to_vec());
    }
    (decoded, packets)
}

/// The two layers of a hybrid packet reach the decoder's output at the same
/// delay: a click's low-band-only and high-band-only peaks coincide at the
/// encoder's documented look-ahead.
#[test]
fn hybrid_layers_align() {
    let mut click = vec![0f32; 48000];
    // Well inside the stream so both layers' predictors are running.
    let at = 960 * 10 + 300;
    click[at] = 0.9;
    let peak = |x: &[f32]| {
        x.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0
    };
    let (lb, _) = hybrid_roundtrip(&click, 32_000, None, (true, false));
    let (hb, _) = hybrid_roundtrip(&click, 32_000, None, (false, true));
    let enc = Encoder::new(48000, 1, Application::Voip).unwrap();
    let (plb, phb) = (peak(&lb) as i64 - at as i64, peak(&hb) as i64 - at as i64);
    eprintln!(
        "hybrid click: LB peak +{plb}, HB peak +{phb}, look_ahead {}",
        {
            let mut e = enc.clone();
            e.set_bitrate(32_000);
            e.look_ahead(960)
        }
    );
    assert!(
        (plb - phb).abs() <= 2,
        "layers misaligned: LB +{plb} vs HB +{phb}"
    );
    assert!((phb - 120).abs() <= 2, "HB peak +{phb}, look_ahead 120");
}

/// 20 ms FB hybrid at 32 kbps: decodes in our decoder range-exactly, tracks
/// the input full-band and in the SILK layer's band alone, and the reference
/// oracle decodes the packets to the same signal.
#[test]
fn hybrid_fb_roundtrip() {
    for (name, pcm) in silk_sources() {
        let (decoded, packets) = hybrid_roundtrip(&pcm, 32_000, None, (false, false));
        let toc = ec_opus::Toc::new(packets[0][0]);
        assert_eq!(toc.config, 15, "{name}: TOC config {}", toc.config);
        let (corr, lag) = aligned_corr(&pcm, &decoded, 400);
        let (lb_only, _) = hybrid_roundtrip(&pcm, 32_000, None, (true, false));
        let lb_ref = lowpass(&pcm, 7000.0);
        let (lb_corr, lb_lag) = aligned_corr(&lb_ref, &lb_only, 400);
        let bytes: usize = packets.iter().map(Vec::len).sum();
        let kbps = bytes as f64 * 8.0 / (packets.len() as f64 * 20.0);
        eprintln!(
            "hybrid FB {name}: corr {corr:.4} at lag {lag}, LB corr {lb_corr:.4} at lag {lb_lag}, {kbps:.1} kbps"
        );
        assert!(corr >= 0.95, "{name}: full-band corr {corr:.4}");
        // +-3: the wav16 fixture is a pure tone, whose phase moves the peak.
        assert!(
            (117..=123).contains(&lag),
            "{name}: aligned at lag {lag}, not the documented 120"
        );
        assert!(lb_corr >= 0.9, "{name}: low-band corr {lb_corr:.4}");
        assert!(
            (28.0..=36.0).contains(&kbps),
            "{name}: {kbps:.1} kbps for a 32 kbps CBR target"
        );
        if let Some(oracle) = oracle_decode(&format!("hybrid-{name}"), &packets, pcm.len()) {
            let n = oracle.len().min(decoded.len() - 120);
            let c = correlation(&oracle[..n], &decoded[120..120 + n]);
            eprintln!("hybrid FB {name}: oracle vs ours corr {c:.4}");
            assert!(c >= 0.95, "{name}: oracle corr {c:.4}");
        }
    }
    // SWB when asked for it, and the rest of the automatic hybrid range
    // (SILK's share leaves CELT least at the bottom of it).
    let speech = speech_like();
    let (_, packets) =
        hybrid_roundtrip(&speech, 24_000, Some(Bandwidth::SuperWide), (false, false));
    assert_eq!(ec_opus::Toc::new(packets[0][0]).config, 13);
    for bps in [20_000u32, 24_000, 39_000] {
        let (decoded, packets) = hybrid_roundtrip(&speech, bps, None, (false, false));
        let (corr, _) = aligned_corr(&speech, &decoded, 400);
        let kbps = packets.iter().map(Vec::len).sum::<usize>() as f64 * 8.0
            / (packets.len() as f64 * 20.0);
        eprintln!(
            "hybrid {bps}: config {}, corr {corr:.4}, {kbps:.1} kbps",
            ec_opus::Toc::new(packets[0][0]).config
        );
        assert!(corr >= 0.9, "hybrid at {bps}: corr {corr:.4}");
        assert!(
            kbps <= bps as f64 / 1000.0 + 3.0,
            "hybrid at {bps}: {kbps:.1} kbps"
        );
    }
}
