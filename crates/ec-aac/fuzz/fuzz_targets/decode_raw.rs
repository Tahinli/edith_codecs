#![no_main]

// Input layout:
//   byte 0      selects one of the four AudioSpecificConfigs below (mod 4)
//   then        raw_data_block access units, each prefixed by a u16 big-endian length
//               (a length past the end of the input is clamped to what remains)

use ec_aac::AacDecoder;
use libfuzzer_sys::fuzz_target;

// Valid, hand-packed AudioSpecificConfigs (ISO/IEC 14496-3 1.6.2.1).
const CONFIGS: [&[u8]; 4] = [
    &[0x11, 0x88],       // AAC-LC, 48 kHz, mono
    &[0x11, 0x90],       // AAC-LC, 48 kHz, stereo
    &[0x2B, 0x23, 0x10], // HE-AAC (explicit SBR): core 24 kHz, stereo, SBR at 48 kHz
    &[0x11, 0xB0],       // AAC-LC, 48 kHz, 5.1
];

fuzz_target!(|data: &[u8]| {
    let Some((&sel, mut rest)) = data.split_first() else {
        return;
    };
    let asc = CONFIGS[(sel as usize) % CONFIGS.len()];
    // The configs are constants above, so a parse failure is a harness bug, not an input finding.
    let mut dec = AacDecoder::with_config_bytes(asc).expect("built-in AudioSpecificConfig must parse");
    while rest.len() >= 2 {
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let (au, tail) = rest.split_at(len.min(rest.len()));
        let _ = dec.decode(au, None);
        rest = tail;
    }
});
