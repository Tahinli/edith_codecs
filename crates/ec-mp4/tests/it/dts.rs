//! DTS in mp4: ffmpeg's `mp4a`/`esds` form (object type 0xA9) and the
//! `dtsc` + `ddts` entry both come out as `CodecId::Dts`, the latter with the
//! rate and channels the `ddts` box states; the writer round-trips `dtsc`.

use std::io::Cursor;
use std::process::Command;

use ec_core::{Buf, CodecId, Demuxer, MediaParameters, Muxer};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some_and(|v| !v.is_empty() && v != "0")
}

fn ffmpeg_mp4(tag: &str) -> Option<Vec<u8>> {
    let out = std::env::temp_dir().join(format!("ec-mp4-dts-{tag}-{}.mp4", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:duration=0.5")
        .args(["-ac", "2", "-c:a", "dca", "-strict", "-2"])
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(!required(), "ffmpeg cannot make a DTS mp4");
        eprintln!("skip: no ffmpeg with the dca encoder");
        return None;
    }
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(out);
    Some(bytes)
}

fn packets(d: &mut Mp4Demuxer<Cursor<Vec<u8>>>) -> Vec<ec_core::Packet> {
    let mut v = Vec::new();
    while let Ok(p) = d.next_packet() {
        v.push(p);
    }
    v
}

fn audio(d: &Mp4Demuxer<Cursor<Vec<u8>>>) -> (u32, usize) {
    let MediaParameters::Audio(a) = &d.streams()[0].params.media else {
        panic!("not audio");
    };
    (a.sample_rate, a.layout.channel_count())
}

/// A `ddts` payload for `rate` Hz and the speaker mask `mask`.
fn ddts(rate: u32, mask: u16) -> Vec<u8> {
    let mut b = vec![0u8; 20];
    b[..4].copy_from_slice(&rate.to_be_bytes());
    b[17..19].copy_from_slice(&mask.to_be_bytes());
    b
}

#[test]
fn ffmpeg_dts_in_mp4a_is_dts() {
    let Some(bytes) = ffmpeg_mp4("a") else {
        return;
    };
    let mut d = Mp4Demuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(d.streams()[0].params.codec, CodecId::Dts);
    assert_eq!(audio(&d), (48_000, 2));
    assert!(packets(&mut d).len() > 20);
}

#[test]
fn dtsc_entry_round_trips_with_ddts_rate_and_channels() {
    let Some(bytes) = ffmpeg_mp4("b") else {
        return;
    };
    let mut d = Mp4Demuxer::new(Cursor::new(bytes)).unwrap();
    let pkts = packets(&mut d);
    let mut info = d.streams()[0].clone();
    // 5.1 at 96 kHz, which the entry's 16-bit rate field cannot state.
    if let MediaParameters::Audio(a) = &mut info.params.media {
        a.sample_rate = 96_000;
        a.layout = ec_core::ChannelLayout::from_count(6);
    }
    info.params.extradata = Some(Buf::from_vec(ddts(96_000, 0x000F)));
    let mut out = Cursor::new(Vec::new());
    {
        let mut m = Mp4Muxer::new(&mut out).unwrap();
        m.add_stream(info).unwrap();
        for p in &pkts {
            m.write_packet(p).unwrap();
        }
        m.finish().unwrap();
    }
    let bytes = out.into_inner();
    assert!(bytes.windows(4).any(|w| w == b"dtsc"));
    assert!(bytes.windows(4).any(|w| w == b"ddts"));
    let mut back = Mp4Demuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(back.streams()[0].params.codec, CodecId::Dts);
    assert_eq!(audio(&back), (96_000, 6));
    assert_eq!(
        back.streams()[0].params.extradata.as_deref(),
        Some(&ddts(96_000, 0x000F)[..])
    );
    let again = packets(&mut back);
    assert_eq!(again.len(), pkts.len());
    assert!(
        again
            .iter()
            .zip(&pkts)
            .all(|(a, b)| a.data.to_vec() == b.data.to_vec())
    );
}
