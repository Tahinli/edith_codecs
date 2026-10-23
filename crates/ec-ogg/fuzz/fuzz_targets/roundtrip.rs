#![no_main]

// Mux arbitrary Opus packets, then demux them: the same packets must come back in the same order.
//
// Input: a 4-byte header (page target, channel count, pre-skip seed), then u16-BE-length-prefixed
// packets. Zero-length packets are dropped first, since the reader does not hand out an empty
// audio packet and the comparison would then never match.

use std::io::Cursor;

use ec_core::{Buf, CodecId, CodecParameters, Demuxer, Error, Muxer, Packet, StreamInfo, TimeBase};
use ec_ogg::{OggDemuxer, OggMuxer};
use libfuzzer_sys::fuzz_target;

fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::from(*b"OpusHead");
    head.push(1); // version
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes()); // output gain
    head.push(0); // mapping family
    head
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let page_target = usize::from(u16::from_le_bytes([data[0], data[1]]) % 4096) + 1;
    let channels = (data[2] % 2) + 1;
    let pre_skip = u16::from(data[3]);

    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut at = 4;
    while let Some(len) = data.get(at..at + 2) {
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        at += 2;
        let Some(body) = data.get(at..at + len) else {
            break;
        };
        at += len;
        if !body.is_empty() {
            packets.push(body.to_vec());
        }
    }

    let time_base = TimeBase::from_rate(48_000);
    let mut params = CodecParameters::new(CodecId::Opus);
    params.extradata = Some(Buf::from_vec(opus_head(channels, pre_skip)));
    let info = StreamInfo::new(0, time_base, params);

    let mut muxer = OggMuxer::new(Cursor::new(Vec::new()));
    muxer.add_stream(info).expect("ogg mux: add_stream");
    muxer.set_page_target_bytes(Some(page_target));
    let mut granule = 0i64;
    for data in &packets {
        granule += 960;
        let mut packet = Packet::new(0, time_base, data.clone());
        packet.pts = Some(granule);
        packet.duration = Some(960);
        muxer.write_packet(&packet).expect("ogg mux: write_packet");
    }
    muxer.finish().expect("ogg mux: finish");
    let bytes = muxer.into_inner().into_inner();

    let mut demux = OggDemuxer::open(Cursor::new(bytes)).expect("ogg demux: open own output");
    let mut read = Vec::new();
    loop {
        match demux.next_packet() {
            Ok(packet) => read.push(packet),
            Err(Error::Eof) => break,
            Err(e) => panic!("ogg demux: unexpected error on own output: {e:?}"),
        }
    }
    assert_eq!(read.len(), packets.len(), "packet count changed in the roundtrip");
    for (got, want) in read.iter().zip(&packets) {
        assert_eq!(&got.data[..], &want[..], "packet bytes changed in the roundtrip");
    }
});
