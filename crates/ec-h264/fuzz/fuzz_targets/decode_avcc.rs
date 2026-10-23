#![no_main]

// Untrusted entry: the packet-level H264Decoder as a demuxer drives it.
// Input layout: byte 0 gives the avcC extradata length (modulo the rest),
// the next bytes are the avcC record, and the remainder is one packet
// (length-prefixed NAL units when the record is valid, Annex B otherwise).

use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase};
use ec_h264::H264Decoder;
use libfuzzer_sys::fuzz_target;

/// Harness caps (see lanes/fuzz1hvideo.report.md): at most 512 bytes of avcC
/// record and 4 KiB of packet, so one exec stays small enough to run at a useful rate.
const MAX_EXTRADATA: usize = 512;
const MAX_PACKET: usize = 4096;

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else {
        return;
    };
    let split = usize::from(split) % (rest.len() + 1);
    let (extradata, packet) = rest.split_at(split);
    let extradata = &extradata[..extradata.len().min(MAX_EXTRADATA)];
    let packet = &packet[..packet.len().min(MAX_PACKET)];

    let mut params = CodecParameters::new(CodecId::H264);
    params.extradata = Some(extradata.to_vec().into());
    let Ok(mut decoder) = H264Decoder::new(params) else {
        return;
    };
    let _ = decoder.send_packet(&Packet::new(0, TimeBase::new(1, 25), packet.to_vec()));
    let _ = decoder.flush();
    while let Ok(frame) = decoder.receive_frame() {
        std::hint::black_box(frame);
    }
});
