//! Regression inputs found by the `decode_stream` fuzz target (`crates/ec-truehd/fuzz`).
//!
//! Each input must be refused without a panic. The loop below is the fuzz target's body.

use ec_truehd::{TrueHdDecoder, frame_length};

/// Replays the input that overflowed `7 << lsb_bits` in `recompute_sign_offset`: a refused
/// access unit left `huff_lsbs` above 24, and the next quant-step update shifted it into i32
/// overflow. The stream is corrupt; decoding must return an error or succeed, never panic.
#[test]
fn stale_huff_lsbs_overflow_is_corrupt_not_a_panic() {
    let data = include_bytes!("../data/fuzz/recompute-sign-offset-overflow.bin");
    let mut dec = TrueHdDecoder::new();
    let mut pos = 0;
    while pos < data.len() {
        let Ok(len) = frame_length(&data[pos..]) else {
            break;
        };
        let Some(au) = data.get(pos..pos + len) else {
            break;
        };
        let _ = dec.decode_access_unit(au);
        pos += len;
    }
}
