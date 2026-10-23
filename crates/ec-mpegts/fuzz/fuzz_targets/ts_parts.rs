#![no_main]

use libfuzzer_sys::fuzz_target;

// The pieces below the demuxer: PSI sections, PES headers, Annex-B scanning with the
// avcC/hvcC builders, audio and LPCM frame headers, packet layout detection.
fuzz_target!(|data: &[u8]| {
    let _ = ec_mpegts::is_mpegts(data);
    let _ = ec_mpegts::fuzz_api::psi(data);
    let _ = ec_mpegts::fuzz_api::pes(data);
    let _ = ec_mpegts::fuzz_api::access_unit(data);
    let _ = ec_mpegts::fuzz_api::audio_headers(data);
    let _ = ec_mpegts::fuzz_api::packet_layer(data);
    let _ = ec_mpegts::parse_lpcm_header(data);
});
