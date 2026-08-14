//! The replica's own call sequence, run against the shim.
//!
//! `export.rs:2942-2982` laces three Vorbis headers, describes one stream,
//! opens a concrete muxer, sets the page target, then writes header, packets
//! and trailer — with each packet's *granule* in its `pts`. This drives exactly
//! that, with real Vorbis packets taken out of a fixture, and asks ffmpeg
//! whether the result is the same audio.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::Demuxer as _;
use oxideav_core::{CodecId, CodecParameters, Muxer as _, Packet, StreamInfo, TimeBase};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/vorbis-ogg-stereo-44100.ogg")
}

fn decode_pcm(path: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn writes_a_vorbis_file_the_way_the_replica_does() {
    let source = fixture();
    if !source.exists() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }

    // Packets and setup data out of a real file, standing in for the encoder
    // the replica feeds this from.
    let mut demuxer = ec_ogg::OggDemuxer::open(std::fs::File::open(&source).unwrap()).unwrap();
    let info = demuxer.streams()[0].clone();
    let extradata = info.params.extradata.clone().unwrap();
    let headers = ec_ogg::xiph_unlace(&extradata).unwrap();
    let mut packets = Vec::new();
    while let Ok(packet) = demuxer.next_packet() {
        packets.push(packet);
    }
    assert_eq!(headers.len(), 3, "a Vorbis stream is three headers");

    // From here on, the replica's code, line for line.
    let laced = super_xiph_lace(&headers).expect("the three headers would not lace");
    assert_eq!(&laced[..], &extradata[..], "lacing must round-trip");

    let rate = 44_100u32;
    let time_base = TimeBase::new(1, i64::from(rate));
    let mut params = CodecParameters::audio(CodecId::new("vorbis"));
    params.sample_rate = Some(rate);
    params.channels = Some(2);
    params.extradata = laced;
    let stream = StreamInfo {
        index: 0,
        time_base,
        duration: info.duration,
        start_time: Some(0),
        params,
    };

    let out_path =
        std::env::temp_dir().join(format!("oxideav-ogg-shim-{}.ogg", std::process::id()));
    let file = std::fs::File::create(&out_path).unwrap();
    let mut muxer = oxideav_ogg::mux::open_concrete(Box::new(file), &[stream]).unwrap();
    muxer.set_page_target_bytes(Some(4096));
    muxer.write_header().unwrap();
    for packet in &packets {
        // The replica states the granule position in `pts`; the shim is what
        // knows that is what it means.
        let mut out = Packet::new(0, time_base, packet.data.to_vec());
        out.pts = ec_ogg::granule_of(packet);
        out.duration = packet.duration;
        muxer.write_packet(&out).unwrap();
    }
    muxer.write_trailer().unwrap();

    // ffprobe reads it without complaint, and it is the same audio.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-count_packets",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(&out_path)
        .output()
        .expect("ffprobe");
    let stderr = String::from_utf8_lossy(&probe.stderr);
    assert!(stderr.trim().is_empty(), "ffprobe complained: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        format!("vorbis,{rate},2,{}", packets.len()),
        "ffprobe must see the stream the replica described"
    );
    assert_eq!(
        decode_pcm(&out_path),
        decode_pcm(&source),
        "the shim's file must decode to the samples that went into it"
    );
    let _ = std::fs::remove_file(&out_path);
}

/// Named apart so the call above reads like the replica's line.
fn super_xiph_lace(headers: &[&[u8]]) -> Option<Vec<u8>> {
    oxideav_ogg::mux::xiph_lace(headers)
}
