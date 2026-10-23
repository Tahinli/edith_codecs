//! Replays of inputs that the fuzzer found to crash the Opus decoder. The input is replayed
//! with the same framing as the `decode_packet` harness, and with no harness caps.

use ec_opus::Decoder;

/// `decode_packet` panic: the CELT path calls `tell_frac` after `skip_to_end` on a frame of a
/// few bytes. The bit count then dips below zero, and the unsigned subtraction in
/// `RangeDecoder::tell_frac` panicked in debug builds (wrapped in release). The decode must now
/// finish without panicking, whatever it returns.
#[test]
fn silent_celt_frame_tell_frac_does_not_underflow() {
    let data: &[u8] = include_bytes!("data/fuzz/decode_packet-tell-frac-underflow.bin");
    let Some((&cfg, mut rest)) = data.split_first() else {
        panic!("replay input is empty");
    };
    let channels = 1 + (cfg & 1) as usize;
    const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
    let rate = RATES[((cfg >> 1) % 5) as usize];
    let mut dec = Decoder::new(rate, channels).expect("replay rate and channel count are valid");
    let mut out = vec![0f32; 5760 * channels];
    while rest.len() >= 2 {
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let (pkt, tail) = rest.split_at(len.min(rest.len()));
        let _ = dec.decode_float(pkt, &mut out);
        rest = tail;
    }
}
