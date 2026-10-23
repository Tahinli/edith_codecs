//! Encodes a raw interleaved f32 little-endian PCM file to ADTS with `AacEncoder`,
//! for the objective encoder comparison in `scripts/aac-encode-score.sh`.
//!
//! ```text
//! cargo run --release -p ec-aac --example aacenc_file -- <in.f32le> <channels> <rate> <kbps> <out.aac>
//! ```

use std::process::ExitCode;

use ec_aac::{AacEncoder, AacEncoderConfig};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        eprintln!("usage: aacenc_file <in.f32le> <channels> <rate> <kbps> <out.aac>");
        return ExitCode::from(2);
    }
    let parse = |s: &str| s.parse::<u32>().ok();
    let (Some(channels), Some(rate), Some(kbps)) = (parse(&args[2]), parse(&args[3]), parse(&args[4]))
    else {
        eprintln!("channels, rate and kbps must be unsigned integers");
        return ExitCode::from(2);
    };
    let raw = match std::fs::read(&args[1]) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("read {}: {e}", args[1]);
            return ExitCode::FAILURE;
        }
    };
    let pcm: Vec<f32> = raw
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("whole f32 samples")))
        .collect();
    let mut enc = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: kbps * 1_000,
        adts: true,
        ..Default::default()
    });
    if let Err(e) = enc.push_pcm(&pcm, channels as u16, rate) {
        eprintln!("push_pcm: {e:?}");
        return ExitCode::FAILURE;
    }
    enc.finish();
    let mut out = Vec::new();
    while let Ok(p) = enc.next_packet() {
        out.extend_from_slice(&p.data);
    }
    if let Err(e) = std::fs::write(&args[5], &out) {
        eprintln!("write {}: {e}", args[5]);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
