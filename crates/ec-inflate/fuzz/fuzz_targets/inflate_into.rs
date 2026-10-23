#![no_main]

use ec_inflate::{Format, inflate_into};
use libfuzzer_sys::fuzz_target;

const LIMIT: usize = 16 << 20;

// Byte 0 bit 0 picks the format. The output buffer starts non-empty, so the
// fuzzer also checks that the prefix survives and the limit counts the whole buffer.
fuzz_target!(|data: &[u8]| {
    let Some((&sel, body)) = data.split_first() else {
        return;
    };
    let format = if sel & 1 == 0 { Format::Raw } else { Format::Zlib };
    let mut out = b"prefix".to_vec();
    let _ = inflate_into(body, &mut out, LIMIT, format);
    assert!(out.starts_with(b"prefix"));
    assert!(out.len() <= LIMIT);
});
