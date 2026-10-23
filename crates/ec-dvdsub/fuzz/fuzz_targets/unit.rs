#![no_main]

use ec_dvdsub::{DEFAULT_PALETTE, SpuAssembler, decode_spu, parse_idx};
use libfuzzer_sys::fuzz_target;

// One subpicture unit, a palette text and an assembler fed in two cuts: none may panic.
fuzz_target!(|data: &[u8]| {
    let _ = decode_spu(data, &DEFAULT_PALETTE, (720, 576));
    let _ = parse_idx(data);
    let mut a = SpuAssembler::default();
    let cut = data.len() / 2;
    let _ = a.push(&data[..cut], Some(0));
    let _ = a.push(&data[cut..], None);
});
