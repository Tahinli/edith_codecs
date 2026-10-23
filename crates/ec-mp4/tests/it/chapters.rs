//! Chapters written by `Mp4Muxer::set_chapters`, read back by ffprobe: the starts
//! (to the millisecond) and the titles must be the ones given, whether ffprobe
//! reads them from the `chpl` box or from the QuickTime chapter track.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, Demuxer, MediaParameters, Muxer, Packet,
    StreamInfo, TimeBase,
};
use ec_mp4::Mp4Muxer;

/// `seconds` of silent AAC (one 1024-sample packet a step), with the chapters set.
fn audio_file(chapters: &[(u64, String)], seconds: u32) -> Vec<u8> {
    let mut params = CodecParameters::new(CodecId::Aac);
    params.extradata = Some(Buf::from(vec![0x12, 0x10]));
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: 44_100,
        layout: ChannelLayout::from_count(2),
        ..Default::default()
    });
    let mut muxer = Mp4Muxer::new(Cursor::new(Vec::new())).unwrap();
    muxer.set_chapters(chapters).unwrap();
    let stream = muxer
        .add_stream(StreamInfo::new(0, TimeBase::from_rate(44_100), params))
        .unwrap();
    for i in 0..seconds * 44_100 / 1024 {
        let mut packet = Packet::new(stream, TimeBase::from_rate(44_100), vec![0xAA, 0x55, i as u8]);
        packet.dts = Some(i64::from(i) * 1024);
        packet.duration = Some(1024);
        packet.flags.keyframe = true;
        muxer.write_packet(&packet).unwrap();
    }
    muxer.finish().unwrap();
    muxer.into_inner().into_inner()
}

/// `(start in ms, title)` per chapter, as ffprobe reads them from the file.
fn ffprobe_chapters(bytes: &[u8], name: &str) -> Vec<(i64, String)> {
    let dir = std::env::temp_dir().join(format!("ec-mp4-chapters-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_chapters", "-of", "default"])
        .arg(&path)
        .output()
        .expect("ffprobe on PATH");
    assert!(out.status.success(), "ffprobe failed: {:?}", out.stderr);
    let text = String::from_utf8_lossy(&out.stdout);
    let mut chapters = Vec::new();
    let mut start = None;
    let mut title = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("start_time=") {
            start = Some((value.parse::<f64>().unwrap() * 1000.0).round() as i64);
        } else if let Some(value) = line.strip_prefix("TAG:title=") {
            title = Some(value.to_string());
        } else if line == "[/CHAPTER]" {
            chapters.push((start.take().unwrap(), title.take().unwrap_or_default()));
        }
    }
    let _ = std::fs::remove_file(&path);
    chapters
}

fn expect(chapters: &[(u64, &str)]) -> Vec<(i64, String)> {
    chapters
        .iter()
        .map(|(ns, title)| (((ns + 500_000) / 1_000_000) as i64, title.to_string()))
        .collect()
}

#[test]
fn ffprobe_reads_the_chapters_back_with_their_starts_and_titles() {
    let given = [
        (0u64, "Intro"),
        (5_000_000_000, "Middle, with ünïcode"),
        (12_345_000_000, "Ending"),
    ];
    let chapters: Vec<(u64, String)> = given.iter().map(|(n, t)| (*n, t.to_string())).collect();
    let bytes = audio_file(&chapters, 15);
    assert_eq!(ffprobe_chapters(&bytes, "three.m4a"), expect(&given));
}

#[test]
fn a_first_chapter_that_is_not_at_zero_keeps_its_start() {
    let given = [(1_500_000_000u64, "Cold open"), (4_000_000_000, "Credits")];
    let chapters: Vec<(u64, String)> = given.iter().map(|(n, t)| (*n, t.to_string())).collect();
    let bytes = audio_file(&chapters, 6);
    assert_eq!(ffprobe_chapters(&bytes, "offset.m4a"), expect(&given));
}

#[test]
fn a_file_without_chapters_has_none_for_ffprobe() {
    let bytes = audio_file(&[], 3);
    assert!(ffprobe_chapters(&bytes, "none.m4a").is_empty());
}

/// The chapter track is a stream the demuxer walks past like any other `text` track: the file
/// still opens, and its one audio track reads back with every sample.
#[test]
fn the_demuxer_reads_a_chaptered_file_as_it_always_did() {
    let chapters = vec![(0u64, "A".to_string()), (500_000_000, "B".to_string())];
    let bytes = audio_file(&chapters, 2);
    let mut demuxer = ec_mp4::Mp4Demuxer::new(Cursor::new(bytes)).expect("the file opens");
    let audio = demuxer
        .streams()
        .iter()
        .filter(|s| s.params.codec == CodecId::Aac)
        .count();
    assert_eq!(audio, 1, "one AAC track");
    let mut packets = 0;
    loop {
        match demuxer.next_packet() {
            Ok(_) => packets += 1,
            Err(ec_core::Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(packets, 2 * 44_100 / 1024, "every audio packet reads back");
}

/// Past 255 chapters the `chpl` box is left out, and the chapter track carries every one:
/// ffprobe lists all 300, with the starts and titles given.
#[test]
fn ffprobe_lists_all_300_chapters_when_chpl_cannot_hold_them() {
    let given: Vec<(u64, String)> = (0..300u64)
        .map(|i| (i * 100_000_000, format!("Chapter {i}")))
        .collect();
    let bytes = audio_file(&given, 31);
    let expected: Vec<(i64, String)> = given
        .iter()
        .map(|(ns, t)| (((ns + 500_000) / 1_000_000) as i64, t.clone()))
        .collect();
    assert_eq!(ffprobe_chapters(&bytes, "three_hundred.m4a"), expected);
}
