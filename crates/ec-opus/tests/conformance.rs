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

/// The CELT-only vectors, which need no SILK layer.
const CELT_VECTORS: [&str; 3] = ["testvector01", "testvector07", "testvector11"];

#[test]
fn celt_vectors_match_the_reference_range_state() {
    for name in CELT_VECTORS {
        let path = vectors_dir().join(format!("{name}.bit"));
        if !path.exists() {
            eprintln!("{name}: missing, skipped (run scripts/fetch-vectors.sh)");
            continue;
        }
        let packets = read_vector(&path);
        let (pcm, matched, decoded) = decode_vector(&packets);
        println!(
            "{name}: {matched}/{decoded} packets bit-exact, {} samples",
            pcm.len() / 2
        );
        assert_eq!(
            matched, decoded,
            "{name}: range state diverged from the reference"
        );

        let reference = read_i16(&vectors_dir().join(format!("{name}.dec")));
        let mine = to_i16(&pcm);
        let n = reference.len().min(mine.len());
        let q = opus_compare(&reference[..n], &mine[..n], 2);
        println!("{name}: opus_compare quality {q:.1} %");
        assert!(
            q >= 0.0,
            "{name}: opus_compare quality {q:.1} % (must be >= 0)"
        );
    }
}
