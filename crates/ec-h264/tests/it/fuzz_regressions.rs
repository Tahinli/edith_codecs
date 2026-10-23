//! Replays of inputs that the fuzzer found to crash the H.264 decoder. Each input is replayed
//! with no harness caps, so a regression shows up here even if the fuzz harness would skip it.

use ec_h264_syntax::{AnnexBIter, Sps};

/// `decode_annexb` panic: the SPS crop offsets are unbounded ue(v) codes, and `crop_x * l`
/// overflowed u32. That panics in debug builds (overflow checks, as the fuzzer builds), and
/// wraps silently in release, so this replay is only meaningful under `cargo test` (debug).
/// The parse must return: a valid SPS, or a corrupt-data refusal, never a panic.
#[test]
fn sps_crop_offset_overflow_is_refused() {
    let data = include_bytes!("../data/fuzz/decode_annexb-sps-crop-overflow.264");
    let sps_nal = AnnexBIter::new(data)
        .find(|nal| nal[0] == 0x67)
        .expect("fixture carries an SPS NAL unit");
    match Sps::parse(&sps_nal[1..]) {
        Ok(sps) => assert!(sps.width > 0, "a parsed SPS has a positive width"),
        Err(err) => assert!(
            matches!(err, ec_core::Error::Corrupt { .. }),
            "expected a corrupt-data refusal, got {err}"
        ),
    }
}

/// `decode_avcc` panic: `dequant_4x4` multiplied untrusted coefficient levels by the level scale in
/// i32 (transform.rs:44), which overflows for out-of-range levels (a debug-build panic). The
/// transform arithmetic now wraps; the packet must decode or be refused, never panic.
#[test]
fn avcc_coefficient_scaling_does_not_overflow() {
    let data = include_bytes!("../data/fuzz/decode_avcc-transform-overflow.bin");
    let Some((&split, rest)) = data.split_first() else {
        panic!("fixture is empty");
    };
    let split = usize::from(split) % (rest.len() + 1);
    let (extradata, packet) = rest.split_at(split);
    let mut params = ec_core::registry::CodecParameters::new(ec_core::registry::CodecId::H264);
    params.extradata = Some(extradata.to_vec().into());
    let Ok(mut decoder) = ec_h264::H264Decoder::new(params) else {
        return;
    };
    let packet = ec_core::Packet::new(0, ec_core::TimeBase::new(1, 25), packet.to_vec());
    let _ = ec_core::registry::Decoder::send_packet(&mut decoder, &packet);
    let _ = ec_core::registry::Decoder::flush(&mut decoder);
    while let Ok(frame) = ec_core::registry::Decoder::receive_frame(&mut decoder) {
        std::hint::black_box(frame);
    }
}

/// `decode_annexb` panic: PPS and slice header arithmetic on parsed ue(v)/se(v) values
/// (`read_ue()? + 1`, `26 + read_se()?`, `pic_init_qp + slice_qp_delta`) overflowed i32/u32 on
/// out-of-range codes (a debug-build panic). These now saturate and are range-checked later.
#[test]
fn pps_parameter_arithmetic_does_not_overflow() {
    let data = include_bytes!("../data/fuzz/decode_annexb-pps-qp-overflow.264");
    let mut decoder = ec_h264::Decoder::new();
    for nal in ec_h264_syntax::AnnexBIter::new(data) {
        let _ = decoder.push_nal(nal);
    }
    let _ = decoder.flush();
    while let Some(frame) = decoder.next_frame() {
        std::hint::black_box(frame);
    }
}

/// `decode_annexb` panic: `(map_height) * 2` in the SPS height computation overflowed u32 for a
/// huge `pic_height_in_map_units_minus1`. The arithmetic now saturates and the size check refuses it.
#[test]
fn sps_height_arithmetic_does_not_overflow() {
    let data = include_bytes!("../data/fuzz/decode_annexb-sps-height-overflow.264");
    let mut decoder = ec_h264::Decoder::new();
    for nal in ec_h264_syntax::AnnexBIter::new(data) {
        let _ = decoder.push_nal(nal);
    }
    let _ = decoder.flush();
    while let Some(frame) = decoder.next_frame() {
        std::hint::black_box(frame);
    }
}
