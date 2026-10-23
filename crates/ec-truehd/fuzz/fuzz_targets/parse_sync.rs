#![no_main]

use ec_truehd::{AccessUnitHeader, frame_length};
use libfuzzer_sys::fuzz_target;

/// TrueHD major sync word, `0xF8726FBA`.
const MAJOR_SYNC_TRUEHD: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBA];

/// Largest body that keeps the whole access unit within the 12-bit length word
/// (8190 bytes = 4095 words): 8190 - 4-byte AU header - 4-byte sync word.
const MAX_BODY: usize = 8190 - 8;

fuzz_target!(|data: &[u8]| {
    // Raw bytes: the length word and, if it happens to match, the major sync.
    let _ = frame_length(data);
    let _ = AccessUnitHeader::parse(data);

    // Fixup: wrap the input in a valid AU header and TrueHD major sync word, so the
    // mutations land on the sync fields and the directory behind them instead of on
    // the 4 sync bytes. The length word is set to the wrapped buffer's length.
    let body = &data[..data.len().min(MAX_BODY)];
    let mut au = Vec::with_capacity(8 + body.len() + 1);
    au.extend_from_slice(&[0, 0, 0, 0]);
    au.extend_from_slice(&MAJOR_SYNC_TRUEHD);
    au.extend_from_slice(body);
    if au.len() % 2 != 0 {
        au.push(0);
    }
    let Ok(words) = u16::try_from(au.len() / 2) else {
        return;
    };
    au[..2].copy_from_slice(&(words & 0x0FFF).to_be_bytes());
    let _ = AccessUnitHeader::parse(&au);
});
