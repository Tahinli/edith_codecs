//! DTS in AVI: the `0x2001` audio format tag is `CodecId::Dts`.

use std::io::Cursor;
use std::process::Command;

use ec_core::{CodecId, Demuxer, MediaParameters};
use ec_riff::AviDemuxer;

#[test]
fn avi_tag_0x2001_is_dts() {
    let out = std::env::temp_dir().join(format!("ec-riff-dts-{}.avi", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:duration=0.5")
        .args(["-ac", "2", "-c:a", "dca", "-strict", "-2"])
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none_or(|v| v.is_empty() || v == "0"),
            "ffmpeg cannot make a DTS avi"
        );
        eprintln!("skip: no ffmpeg with the dca encoder");
        return;
    }
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(out);
    let d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
    let s = d.streams().iter().find(|s| s.params.codec == CodecId::Dts);
    let s = s.expect("a DTS stream");
    let MediaParameters::Audio(a) = &s.params.media else {
        panic!("not audio");
    };
    assert_eq!((a.sample_rate, a.layout.channel_count()), (48_000, 2));
}
