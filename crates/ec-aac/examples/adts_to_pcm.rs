//! Decodes an ADTS file (argv[1]) to raw interleaved f32le PCM on stdout, for
//! the patch-map probe scripts (`scripts/aac-tables/sbrpatchmap.py`) to read
//! our own decoder's output through the exact same FFT/comb-reading code as
//! the reference decoder's, with no format translation in between.
use std::io::Write;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: adts_to_pcm <file.aac>");
    let data = std::fs::read(&path).expect("file readable");
    // Plain `AacDecoder::new()` never enables SBR (it only turns on from an
    // explicit AudioSpecificConfig's `sbr_present`, per `with_config`) --
    // the probe streams here are raw same-rate-SBR ADTS with no ASC, so
    // build one directly rather than parsing bytes we don't have.
    let same_rate_sbr = std::env::var("EC_AAC_SBR_SAME_RATE").is_ok();
    let channels: u16 = std::env::var("EC_AAC_SBR_CHANNELS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // Round-32: built via the SAME write_audio_specific_config +
    // with_config_bytes round trip `wrap_sbr` uses (AOT_SBR, core rate
    // 22050 = ADTS sf_index 7's SAMPLE_RATES entry, extension 44100) so this
    // path and the reference's wrap_sbr-wrapped mp4 read byte-identical ASC
    // semantics -- the prior hand-built config (object_type=2/AAC-LC,
    // sample_rate=44100 for what is actually the 22050 Hz *core*) diverged
    // from wrap_sbr's ASC, which is why the sanity gate (ours vs
    // build_patches) was failing independent of any patch-map bug.
    let mut decoder = if same_rate_sbr {
        let asc = ec_aac::write_audio_specific_config(&ec_aac::AudioSpecificConfig {
            object_type: ec_aac::AOT_SBR,
            sample_rate: 22050,
            sf_index: 7,
            channels,
            channel_config: channels as u8,
            sbr_present: true,
            ps_present: false,
            extension_sample_rate: Some(44100),
        });
        ec_aac::AacDecoder::with_config_bytes(&asc).expect("asc parses")
    } else {
        ec_aac::AacDecoder::new()
    };
    let mut at = 0usize;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while at + 7 <= data.len() {
        let header = match ec_aac::parse_adts(&data[at..]) {
            Ok(h) => h,
            Err(_) => break,
        };
        let end = (at + header.frame_length).min(data.len());
        if let Ok(frame) = decoder.decode(&data[at..end], None) {
            for v in &frame.samples {
                out.write_all(&v.to_le_bytes()).expect("stdout writable");
            }
        }
        at = end;
    }
}
