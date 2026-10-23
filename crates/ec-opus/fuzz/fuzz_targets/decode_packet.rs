#![no_main]

// Input layout:
//   byte 0      bit 0: channels (0 = mono, 1 = stereo); bits 1..3: output rate index
//               (mod 5: 8, 12, 16, 24, 48 kHz)
//   then        packets, each prefixed by a u16 big-endian length
//               (a length past the end of the input is clamped to what remains)
// All packets are decoded in sequence on one decoder, so state carries across them.

use ec_opus::Decoder;
use libfuzzer_sys::fuzz_target;

const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

fuzz_target!(|data: &[u8]| {
    let Some((&cfg, mut rest)) = data.split_first() else {
        return;
    };
    let channels = 1 + (cfg & 1) as usize;
    let rate = RATES[((cfg >> 1) % 5) as usize];
    let Ok(mut dec) = Decoder::new(rate, channels) else {
        return;
    };
    // 120 ms at 48 kHz is the largest output one packet can produce.
    let mut out = vec![0f32; 5760 * channels];
    while rest.len() >= 2 {
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let (pkt, tail) = rest.split_at(len.min(rest.len()));
        let _ = dec.decode_float(pkt, &mut out);
        rest = tail;
    }
});
