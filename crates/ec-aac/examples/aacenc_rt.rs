//! AAC-LC encode speed relative to realtime, which is how the crate's encode
//! perf claim is measured: a deterministic tonal-plus-transient program, ten
//! seconds, at the seat's rates (stereo 256 kbit/s, 5.1 512 kbit/s), the
//! shortest wall of several alternating repetitions.
//!
//! ```text
//! cargo run --release -p ec-aac --example aacenc_rt
//! ```

use std::time::Instant;

use ec_aac::{AacEncoder, AacEncoderConfig};

/// Deterministic tonal stacks with periodic noise bursts: enough tonality for
/// the spreading function to bite, enough attacks to exercise the short-block
/// path.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / 16777216.0) * 2.0 - 1.0
    }
}

fn channel(f0: f64, seed: u64, rate: u32, secs: usize, burst_period: f64) -> Vec<f32> {
    let mut noise = Lcg(seed);
    let mut out = Vec::with_capacity(secs * rate as usize);
    for i in 0..secs * rate as usize {
        let t = i as f64 / rate as f64;
        let frac = (t % burst_period) / burst_period;
        let burst = (-frac * 18.0_f64).exp() as f32;
        let stack = (f0.sin() * 0.5
            + (f0 * 3.0).sin() * 0.15
            + (f0 * 5.0 + 0.31 * (6.0 * std::f64::consts::TAU * t).sin()).sin() * 0.08)
            as f32
            * (0.7 + 0.3 * (0.5 * std::f64::consts::TAU * t).sin()) as f32;
        out.push(stack + burst * noise.next_f32() * 0.3);
    }
    out
}

fn program(channels: usize, rate: u32, secs: usize) -> Vec<f32> {
    let stereo = [0x5EED_1234u64, 0xC0FF_EE01];
    let five_one = [0x5EED_1234, 0xC0FF_EE01, 0xABCD_0001, 0xABCD_0002, 0xABCD_0003, 0xABCD_0004];
    let f0s = [220.0f64, 277.0, 330.0, 55.0, 196.0, 246.9];
    let periods = [0.9f64, 0.8, 1.1, 1.3, 0.7, 0.85];
    let seeds: &[u64] = if channels == 2 {
        &stereo
    } else {
        &five_one
    };
    let chans: Vec<Vec<f32>> = (0..channels)
        .map(|c| channel(f0s[c], seeds[c], rate, secs, periods[c]))
        .collect();
    let mut pcm = Vec::with_capacity(chans[0].len() * channels);
    for i in 0..chans[0].len() {
        for c in &chans {
            pcm.push(c[i]);
        }
    }
    pcm
}

fn encode(pcm: &[f32], channels: usize, kbps: u32) -> usize {
    let mut enc = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: kbps * 1_000,
        ..Default::default()
    });
    enc.push_pcm(pcm, channels as u16, 48_000).expect("pcm accepted");
    enc.finish();
    let mut bytes = 0usize;
    while let Ok(p) = enc.next_packet() {
        bytes += p.data.len();
    }
    bytes
}

fn main() {
    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    for (channels, kbps) in [(2usize, 256u32), (6, 512)] {
        let pcm = program(channels, 48_000, 10);
        let secs = pcm.len() as f64 / (48_000.0 * channels as f64);
        let _ = encode(&pcm, channels, kbps); // warm
        let mut walls = Vec::with_capacity(reps);
        for _ in 0..reps {
            let t0 = Instant::now();
            let bytes = encode(&pcm, channels, kbps);
            walls.push((t0.elapsed().as_secs_f64(), bytes));
        }
        walls.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let (wall, bytes) = walls[0];
        println!(
            "{channels}ch {kbps}k: min {wall:.4}s = {:.1}x realtime ({} B, {} reps)",
            secs / wall,
            bytes,
            reps
        );
    }
}
