//! Encoder quality floor (lane opusq): a music-like signal (chords, noise, transients) must
//! round-trip through the encoder and the decoder above a per-bitrate SNR floor. The floors sit
//! about 0.1-0.5 dB under what the encoder measures after the continuous allocation trim and the
//! analysis-driven leak boost; libopus (CELT, restricted-lowdelay, CBR) measures within ~1 dB
//! above these on the same signal shape, so a regression of the allocation shows here.
use ec_opus::{Application, Decoder, Encoder, MultistreamDecoder, MultistreamEncoder};

const FRAME: usize = 960;

/// Hides a constant chunk size from clippy's `as_chunks` lint.
fn channels_of(n: usize) -> usize {
    std::hint::black_box(n)
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    }
}

/// `channels` interleaved channels, `secs` seconds at 48 kHz.
fn music(channels: usize, secs: f32, seed: u64) -> Vec<f32> {
    let n = (48000.0 * secs) as usize;
    let mut rng = Lcg(seed);
    let notes = [220.0f32, 277.2, 329.6, 440.0, 554.4, 659.3];
    let mut out = vec![0f32; n * channels];
    for c in 0..channels {
        let phases: Vec<f32> = notes.iter().map(|_| rng.next() * 3.0).collect();
        for t in 0..n {
            let tt = t as f32 / 48000.0;
            let f0 = [1.0f32, 1.122, 1.26, 1.5][(tt as usize) % 4];
            let mut v = 0.0;
            for (k, f) in notes.iter().enumerate() {
                for h in 1..=5 {
                    v += 0.08 / h as f32
                        * (2.0 * core::f32::consts::PI * f * f0 * h as f32 * tt + phases[k]).sin();
                }
            }
            v += 0.004 * rng.next();
            let since = t % 12000;
            if since < 3000 {
                v += 0.15 * rng.next() * (-(since as f32) / 500.0).exp();
            }
            out[t * channels + c] = v * 0.6;
        }
    }
    out
}

fn snr_db(reference: &[f32], decoded: &[f32], channels: usize) -> f32 {
    let n = reference.len().min(decoded.len()) / channels;
    let (lo, hi) = (2000 * channels, n.saturating_sub(2000) * channels);
    let (mut s, mut e) = (0f64, 0f64);
    for i in lo..hi {
        s += f64::from(reference[i]).powi(2);
        e += f64::from(reference[i] - decoded[i]).powi(2);
    }
    (10.0 * (s / e).log10()) as f32
}

fn stereo_snr(kbps: u32) -> f32 {
    let mono = music(1, 1.5, 3);
    let mut pcm = music(2, 1.5, 7);
    // Correlated channels (a common part plus a private part), as in a real mix.
    for (t, f) in pcm.chunks_exact_mut(channels_of(2)).enumerate() {
        let (a, b) = (f[0], f[1]);
        f[0] = 0.8 * mono[t] + 0.2 * a;
        f[1] = 0.6 * mono[t] + 0.4 * b;
    }
    let mut enc = Encoder::new(48000, 2, Application::Audio).unwrap();
    enc.set_bitrate(kbps * 1000);
    enc.set_vbr_constrained(false);
    let skip = enc.look_ahead(FRAME);
    let mut dec = Decoder::new(48000, 2).unwrap();
    let (mut out, mut buf, mut pkt) = (Vec::new(), vec![0f32; 5760 * 2], vec![0u8; 1500]);
    for f in pcm.chunks_exact(channels_of(FRAME * 2)) {
        let n = enc.encode_float(f, FRAME, &mut pkt).unwrap();
        let k = dec.decode_float(&pkt[..n], &mut buf).unwrap();
        out.extend_from_slice(&buf[..k * 2]);
    }
    snr_db(&pcm, &out[skip * 2..], 2)
}

#[test]
fn stereo_snr_floor() {
    for (kbps, floor) in [(64, 17.0), (96, 19.3), (128, 22.0), (256, 26.7)] {
        let snr = stereo_snr(kbps);
        eprintln!("stereo {kbps}k snr {snr:.2}");
        assert!(snr >= floor, "{kbps} kbit/s: {snr:.2} dB < floor {floor}");
    }
}

#[test]
fn surround_5_1_snr_floor() {
    let pcm = music(6, 0.8, 11);
    let mut enc = MultistreamEncoder::surround(48000, 6, Application::Audio).unwrap();
    enc.set_bitrate(256_000);
    enc.set_vbr_constrained(false);
    let skip = enc.look_ahead(FRAME);
    let table = ec_opus::ogg::default_mapping(6).unwrap().3;
    let mut dec = MultistreamDecoder::with_rate(48000, 4, 2, &table);
    let (mut out, mut buf, mut pkt) = (Vec::new(), vec![0f32; 5760 * 6], vec![0u8; 4000]);
    for f in pcm.chunks_exact(channels_of(FRAME * 6)) {
        let n = enc.encode_float(f, FRAME, &mut pkt).unwrap();
        let k = dec.decode_float(&pkt[..n], &mut buf).unwrap();
        out.extend_from_slice(&buf[..k * 6]);
    }
    let snr = snr_db(&pcm, &out[skip * 6..], 6);
    eprintln!("5.1 256k snr {snr:.2}");
    assert!(snr >= 17.0, "5.1 256 kbit/s: {snr:.2} dB");
}
