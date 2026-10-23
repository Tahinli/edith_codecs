//! The ProRes sample entries (`apco apcs apcn apch ap4h ap4x`) read as `CodecId::ProRes`. Each
//! source is a one-frame ffmpeg `prores_ks` mov written at test time; skipped where ffmpeg or the
//! encoder is missing, unless `EC_REQUIRE_FIXTURES` is set.

use std::fs::File;
use std::io::BufReader;
use std::process::Command;

use ec_core::registry::CodecId;
use ec_core::Demuxer;
use ec_mp4::Mp4Demuxer;

fn has_prores_ks() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("prores_ks"))
        .unwrap_or(false)
}

#[test]
fn prores_fourccs_are_recognised() {
    if !has_prores_ks() {
        assert!(std::env::var_os("EC_REQUIRE_FIXTURES").is_none(), "ffmpeg prores_ks required");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ec-mp4-prores-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (profile, fmt, fourcc) in [
        ("0", "yuv422p10le", "apco"),
        ("1", "yuv422p10le", "apcs"),
        ("2", "yuv422p10le", "apcn"),
        ("3", "yuv422p10le", "apch"),
        ("4", "yuv444p10le", "ap4h"),
        ("5", "yuv444p10le", "ap4x"),
    ] {
        let path = dir.join(format!("{fourcc}.mov"));
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", "1"])
            .args(["-c:v", "prores_ks", "-profile", profile, "-pix_fmt", fmt])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok, "ffmpeg {fourcc}");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(4).any(|w| w == fourcc.as_bytes()), "{fourcc} entry present");
        let demuxer = Mp4Demuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
        let streams = demuxer.streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].params.codec, CodecId::ProRes, "{fourcc}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
