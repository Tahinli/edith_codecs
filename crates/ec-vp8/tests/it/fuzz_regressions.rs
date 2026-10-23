//! Regression inputs found by the `decode_frame` fuzz target. Each must
//! decode to an error, never a panic.

use ec_vp8::decode::Decoder;

/// Key frame (tag 0x000210, show_frame) with start code 9d 01 2a, width 0x1010
/// and height 0. Found by fuzzing: the zero-height plane reached `extend_plane`
/// with `vis_h == 0` through `make_ref`.
const ZERO_HEIGHT_KEY_FRAME: [u8; 32] = [
    0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x10, 0x10, 0x00, 0x00, 0x00, 0x07, 0x08, 0x85, 0x85,
    0x88, 0x85, 0x84, 0x88, 0x00, 0x28, 0x10, 0x00, 0x60, 0x6b, 0x00, 0xfe, 0xff, 0x31, 0xab,
    0x50, 0x80,
];

#[test]
fn zero_height_key_frame_is_rejected() {
    let mut dec = Decoder::new();
    assert!(dec.decode(&ZERO_HEIGHT_KEY_FRAME).is_err());
    assert!(dec.dimensions().is_none());
}

#[test]
fn zero_width_key_frame_is_rejected() {
    // Same header with width 0 and height 0x0010.
    let mut frame = ZERO_HEIGHT_KEY_FRAME;
    frame[6..10].copy_from_slice(&[0x00, 0x00, 0x10, 0x00]);
    let mut dec = Decoder::new();
    assert!(dec.decode(&frame).is_err());
}

#[test]
fn inter_frame_before_key_frame_is_rejected() {
    // Tag bit 0 set: inter frame, with no key frame decoded yet.
    let mut dec = Decoder::new();
    assert!(dec.decode(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).is_err());
}

/// `decode_ivf` assertion failure in the motion compensation predictor: an inter frame read a
/// reference plane built for another frame size (left behind by a key frame that failed after
/// resizing the decoder). The reference geometry is now checked before any prediction.
#[test]
fn inter_frame_with_reference_of_another_size_is_refused() {
    let data = include_bytes!("../data/fuzz/decode_ivf-ref-size-mismatch.ivf");
    let _ = ec_vp8::stream::decode_stream(data);
}
