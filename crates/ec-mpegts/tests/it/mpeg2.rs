//! MPEG-1/2 video in transport streams: exposed as `CodecId::Mpeg2Video`, one
//! packet per coded picture, parameters from the sequence header. Fixtures come from
//! ffmpeg's synthetic testsrc2 and skip (fail under `EC_REQUIRE_FIXTURES`) without it.

use crate::common::*;
use ec_core::registry::Demuxer;
use ec_core::{CodecId, PixelFormat, TimeBase};

fn pictures(d: &[u8]) -> Vec<u8> {
    // picture_coding_type of every picture start code.
    d.windows(6)
        .filter(|w| w[..4] == [0, 0, 1, 0])
        .map(|w| (w[5] >> 3) & 7)
        .collect()
}

fn check(codec: &str, name: &str, mpeg2: bool) {
    let Some(ts) = fixture(
        name,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=352x288:rate=25",
            "-frames:v",
            "12",
            "-c:v",
            codec,
            "-g",
            "6",
            "-bf",
            "2",
            "-f",
            "mpegts",
        ],
    ) else {
        return;
    };
    let es = fixture_dir().join(format!("{name}.m2v"));
    if let Err(e) = ffmpeg(
        &["-i", ts.to_str().unwrap(), "-c", "copy", "-f", codec],
        &es,
    ) {
        assert!(!required(), "es extraction failed: {e}");
        return;
    }
    let want = std::fs::read(&es).unwrap();
    let mut d = open(&ts);
    assert_eq!(d.streams().len(), 1);
    let s = &d.streams()[0];
    assert_eq!(s.params.codec, CodecId::Mpeg2Video);
    let v = s.params.video().unwrap();
    assert_eq!((v.width, v.height), (352, 288));
    assert_eq!(v.frame_rate, Some(TimeBase::new(25, 1)));
    assert_eq!(v.format, Some(PixelFormat::I420));
    let ex = s.params.extradata.clone().unwrap();
    assert_eq!(ex[..4], [0, 0, 1, 0xB3]);
    assert_eq!(ex.windows(4).any(|w| w == [0, 0, 1, 0xB5]), mpeg2);
    assert!(d.tracks()[0].refusal.is_none());

    let pkts = read_all(&mut d);
    assert_eq!(pkts.len(), 12, "one packet per picture");
    let got: Vec<u8> = pkts.iter().flat_map(|p| p.data.clone()).collect();
    assert_eq!(pictures(&got), pictures(&want));
    assert_eq!(got, want, "packet data equals ffmpeg's elementary stream");
    for p in &pkts {
        assert_eq!(pictures(&p.data).len(), 1);
        assert_eq!(p.key, pictures(&p.data)[0] == 1 || p.data[3] == 0xB3);
        assert!(p.pts.is_some());
    }
    assert!(pkts[0].key);
    assert!(pkts.iter().filter(|p| p.key).count() >= 2);
    // Presentation order: sorted pts are strictly increasing by one frame.
    let mut pts: Vec<i64> = pkts.iter().map(|p| p.pts.unwrap()).collect();
    pts.sort_unstable();
    assert!(pts.windows(2).all(|w| w[1] - w[0] == 3600), "{pts:?}");
    // Decode order: the dts that are given never go backwards.
    let dts: Vec<i64> = pkts.iter().filter_map(|p| p.dts).collect();
    assert!(dts.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn mpeg2video_in_ts() {
    check("mpeg2video", "m2v.ts", true);
}

#[test]
fn mpeg1video_in_ts() {
    check("mpeg1video", "m1v.ts", false);
}
