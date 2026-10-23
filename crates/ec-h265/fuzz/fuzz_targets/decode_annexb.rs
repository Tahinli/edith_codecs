#![no_main]

// Untrusted entry: a raw Annex B byte stream through the full stateful Decoder
// (parameter sets, POC, RPS, DPB, slice decode, in-loop filters, output).
// Errors are expected for malformed input; only panics, hangs and OOM count.
//
// Caps, so one iteration stays in the milliseconds and the fuzzer cannot OOM:
// - an SPS larger than MAX_WIDTH x MAX_HEIGHT luma is not fed to the decoder;
// - an SPS asking for a DPB deeper than MAX_DPB_PICTURES is not fed either;
// - at most MAX_PICTURES pictures and MAX_SLICES slice segments are fed: the stream
//   is cut at the first NAL unit past either limit;
// - the decoder runs on one thread, so the time goes to the input, not to scheduling.

use ec_h265::Decoder;
use ec_h265_syntax::{NalUnitType, Sps, split_annex_b};
use libfuzzer_sys::fuzz_target;

/// Largest luma picture fed to the decoder.
const MAX_WIDTH: u32 = 64;
const MAX_HEIGHT: u32 = 64;
/// Pictures the DPB may hold (`sps_max_dec_pic_buffering_minus1 + 1`).
const MAX_DPB_PICTURES: u32 = 6;
/// Pictures fed per input.
const MAX_PICTURES: u32 = 8;
/// Slice NAL units fed per input, first or not.
const MAX_SLICES: u32 = 16;

/// Whether an SPS payload is within the size and DPB limits above.
fn sps_within_limits(rbsp: &[u8]) -> bool {
    match Sps::parse(rbsp) {
        Ok(sps) => {
            sps.pic_width_in_luma_samples <= MAX_WIDTH
                && sps.pic_height_in_luma_samples <= MAX_HEIGHT
                && sps.max_dec_pic_buffering_minus1 < MAX_DPB_PICTURES
        }
        Err(_) => true,
    }
}

fuzz_target!(|data: &[u8]| {
    let mut decoder = Decoder::new();
    decoder.set_threads(1);
    let mut pictures = 0u32;
    let mut slices = 0u32;
    for nal in split_annex_b(data) {
        if nal.header.nal_type == NalUnitType::Sps && !sps_within_limits(&nal.rbsp()) {
            continue;
        }
        // first_slice_segment_in_pic_flag is the first bit of a slice's RBSP.
        if nal.header.nal_type.is_vcl() {
            slices += 1;
            if slices > MAX_SLICES {
                break;
            }
            if nal.payload.first().is_some_and(|b| b & 0x80 != 0) {
                pictures += 1;
                if pictures > MAX_PICTURES {
                    break;
                }
            }
        }
        let mut bytes = nal.header.to_bytes().to_vec();
        bytes.extend_from_slice(nal.payload);
        let _ = decoder.push_nal(&bytes);
        while let Some(frame) = decoder.next_frame() {
            std::hint::black_box(frame);
        }
    }
    let _ = decoder.flush();
    while let Some(frame) = decoder.next_frame() {
        std::hint::black_box(frame);
    }
});
