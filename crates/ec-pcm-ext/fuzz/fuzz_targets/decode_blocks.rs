#![no_main]

use ec_core::frame::ChannelLayout;
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, MediaParameters};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let codec = [
        CodecId::ImaAdpcmWav,
        CodecId::MsAdpcm,
        CodecId::ImaAdpcmQt,
        CodecId::PcmAlaw,
        CodecId::PcmMulaw,
    ][usize::from(sel) % 5];
    let channels = 1 + usize::from(sel >> 3) % 2;
    let mut p = CodecParameters::new(codec);
    p.media = MediaParameters::Audio(AudioParameters {
        sample_rate: 8000,
        layout: ChannelLayout::from_count(channels),
        ..AudioParameters::default()
    });
    if let Ok(mut d) = ec_pcm_ext::PcmExtDecoder::from_parameters(p) {
        let _ = d.decode(rest);
    }
});
