//! Wraps a bare ADTS stream (the SBR patch-map probes'
//! `scripts/aac-tables/sbrpatchmap.py` output) in an mp4 container carrying
//! an EXPLICIT-SBR AudioSpecificConfig as its `esds` -- the instrument
//! round-30's blocker needed: a bare `.aac` file carries no out-of-band
//! AudioSpecificConfig at all, so a decoder that trusts one (rather than
//! only its bare-ADTS implicit per-frame SBR detection) has nothing to arm
//! HE-AAC output from and locks to core-rate.
//!
//! Matroska was tried first (`MatroskaMuxer`, dev-dep already proven to
//! round-trip `CodecPrivate` byte-for-byte in
//! `crates/ec-aac/tests/sbr_real_library.rs`) but the reference decoder's
//! Matroska demuxer reads the *codec ID string* ("A_AAC" vs.
//! "A_AAC/MPEG4/SBR") for its reported profile, not just the `CodecPrivate`
//! bytes our `MatroskaMuxer` writes generically -- it still upgraded the
//! reported sample rate to the extension rate (proving it DOES parse
//! `CodecPrivate` for that), but kept reporting profile "LC". mp4's `esds`
//! carries the same explicit-SBR AudioSpecificConfig and needs no such
//! sibling string, matching the real `~/Music/Yok - Nikbinler.mp4` fixture's
//! own container exactly.
//!
//! `stream()`/`stream_one_band()`'s probe frames are full ADTS (7-byte
//! header + raw_data_block); this strips the header off each frame and
//! remuxes the raw access units into an mp4 track whose `esds` is
//! `write_audio_specific_config` with `object_type = AOT_SBR` and an
//! explicit `extension_sample_rate`.
//!
//! usage: wrap_sbr_mkv <in.aac> <core_rate> <ext_rate> <channels> <out.mp4>
use std::fs::File;

use ec_aac::{AOT_SBR, AudioSpecificConfig, sf_index_for_rate, write_audio_specific_config};
use ec_core::{
    AudioParameters, ChannelLayout, CodecId, CodecParameters, MediaParameters, Muxer, Packet,
    StreamInfo, TimeBase,
};
use ec_mp4::Mp4Muxer;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        6,
        "usage: wrap_sbr_mkv <in.aac> <core_rate> <ext_rate> <channels> <out.mkv>"
    );
    let data = std::fs::read(&args[1]).expect("input readable");
    let core_rate: u32 = args[2].parse().expect("core_rate is a number");
    let ext_rate: u32 = args[3].parse().expect("ext_rate is a number");
    let channels: u16 = args[4].parse().expect("channels is a number");

    // Strip each ADTS frame down to its raw access unit (no header, no CRC).
    let mut aus = Vec::new();
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let Ok(header) = ec_aac::parse_adts(&data[at..]) else {
            break;
        };
        let end = (at + header.frame_length).min(data.len());
        aus.push(data[at + header.header_len..end].to_vec());
        at = end;
    }
    assert!(!aus.is_empty(), "no ADTS frames found in {}", args[1]);

    let sf_index = sf_index_for_rate(core_rate).expect("core_rate is a valid AAC rate");
    let channel_config = channels as u8;
    let asc = write_audio_specific_config(&AudioSpecificConfig {
        object_type: AOT_SBR,
        sample_rate: core_rate,
        sf_index,
        channels,
        channel_config,
        sbr_present: true,
        ps_present: false,
        extension_sample_rate: Some(ext_rate),
    });

    let time_base = TimeBase::from_rate(core_rate);
    let layout = if channels == 1 {
        ChannelLayout::Mono
    } else {
        ChannelLayout::Stereo
    };
    let mut params = CodecParameters::new(CodecId::Aac);
    params.extradata = Some(asc.into());
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: core_rate,
        layout,
        format: None,
        bits_per_sample: None,
    });
    let mut info = StreamInfo::new(0, time_base, params);
    info.default = true;

    let out = File::create(&args[5]).expect("output creatable");
    let mut muxer = Mp4Muxer::new(out).expect("mp4 muxer opens");
    muxer.add_stream(info).expect("stream declared");
    for (i, au) in aus.iter().enumerate() {
        let pts = i as i64 * 1024;
        let packet = Packet::new(0, time_base, au.as_slice())
            .with_pts(pts)
            .with_duration(1024);
        muxer.write_packet(&packet).expect("packet written");
    }
    muxer.finish().expect("finished");
    eprintln!(
        "wrote {} ({} frames, core={core_rate}Hz ext={ext_rate}Hz ch={channels})",
        args[5],
        aus.len()
    );
}
