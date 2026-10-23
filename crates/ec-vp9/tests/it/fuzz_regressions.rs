//! Replays of inputs that the fuzzer found to crash the VP9 decoder. Each input is replayed
//! with no harness caps, so a regression shows up here even if the fuzz harness would skip it.

use ec_core::Error;
use ec_vp9::decode::Decoder;

/// `decode_frame` out-of-memory: a key frame header declares 24580x36610, about 900 M luma
/// samples. The decoder sized its picture buffers from that header before refusing anything.
#[test]
fn oversized_keyframe_header_is_refused_before_allocation() {
    let data = include_bytes!("../data/fuzz/decode_frame-oom-size.bin");
    let err = Decoder::new()
        .decode(data)
        .expect_err("a frame above the VP9 level 6.2 sample limit must be refused");
    assert!(
        matches!(err, Error::Unsupported { .. }),
        "expected an Unsupported refusal, got {err}"
    );
    assert!(err.to_string().contains("24580x"), "refusal should name the frame size: {err}");
}

/// `decode_frame` panic: the frame-area check multiplied the 16-bit header sizes in u32, which
/// overflows at 65536x65536 (debug build panic). The product is now taken in u64. The input is
/// an inter-frame header; the decode must be refused, not panic.
#[test]
fn frame_area_product_does_not_overflow() {
    let data = include_bytes!("../data/fuzz/decode_frame-area-overflow.bin");
    let result = Decoder::new().decode(data);
    assert!(result.is_err(), "a frame with no reference state must be refused, got {result:?}");
}
