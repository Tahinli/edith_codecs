//! Matroska `Colour/MasteringMetadata` (primaries, white point, luminances) and
//! `MaxCLL`/`MaxFALL`, written on request and read back, with ffprobe's word
//! for the values. Source: ffmpeg libx265 Main 10, synthetic.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::process::Command;

use ec_core::{ContentLight, Demuxer, Error, MasteringPrimaries, Muxer, Packet, StreamInfo};
use ec_matroska::{MatroskaDemuxer, MatroskaMuxer};
use ec_mp4::Mp4Demuxer;

fn light() -> ContentLight {
    ContentLight {
        max_cll: Some(1000.0),
        max_fall: Some(400.0),
        mastering_max: Some(1000.0),
        mastering_min: Some(0.005),
        mastering_primaries: Some(MasteringPrimaries::P3_D65),
    }
}

fn video_light(path: &Path) -> ContentLight {
    let d = MatroskaDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap();
    match &d.streams()[0].params.media {
        ec_core::MediaParameters::Video(v) => v.light,
        _ => panic!("video"),
    }
}

fn mux(dst: &Path, s: &[StreamInfo], p: &[Packet], light: Option<ContentLight>) {
    let mut m = MatroskaMuxer::new(BufWriter::new(File::create(dst).unwrap()));
    for st in s {
        m.add_stream(st.clone()).unwrap();
    }
    if let Some(l) = light {
        m.set_content_light(0, l).unwrap();
    }
    for pk in p {
        m.write_packet(pk).unwrap();
    }
    m.finish().unwrap();
}

#[test]
fn hevc_main10_mastering_metadata_in_mkv() {
    let enc = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output();
    if !enc.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(" libx265 "))
        || Command::new("ffprobe").arg("-version").output().is_err()
    {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES set but no ffmpeg with libx265"
        );
        eprintln!("skip: no ffmpeg/ffprobe/libx265");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ec-mkv-hdr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=1"])
            .args(["-c:v", "libx265", "-pix_fmt", "yuv420p10le", "-tag:v", "hvc1"])
            .args(["-x265-params", "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc"])
            .arg(&src)
            .status()
            .unwrap()
            .success()
    );
    let mut d = Mp4Demuxer::new(BufReader::new(File::open(&src).unwrap())).unwrap();
    let streams = d.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => packets.push(p),
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }

    // Nothing is written unless asked: no mastering element at all.
    let plain = dir.join("plain.mkv");
    mux(&plain, &streams, &packets, None);
    assert!(!std::fs::read(&plain).unwrap().windows(2).any(|w| w == [0x55, 0xD0]));
    assert_eq!(video_light(&plain), ContentLight::default());

    let dst = dir.join("hdr.mkv");
    mux(&dst, &streams, &packets, Some(light()));
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-show_frames", "-read_intervals", "%+#1", "-of", "json"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    let json = String::from_utf8_lossy(&out.stdout);
    for want in [
        "Mastering display metadata",
        "Content light level metadata",
        "\"max_content\": 1000",
        "\"max_average\": 400",
    ] {
        assert!(json.contains(want), "ffprobe lacks {want}:\n{json}");
    }
    // mkv carries the chromaticities as floats, so ffprobe's rationals are
    // approximate; compare as numbers.
    let num = |key: &str| -> f64 {
        let at = json.find(&format!("\"{key}\": \"")).unwrap_or_else(|| panic!("no {key}"));
        let rest = &json[at + key.len() + 5..];
        let frac = &rest[..rest.find('"').unwrap()];
        let (n, d) = frac.split_once('/').unwrap();
        n.parse::<f64>().unwrap() / d.parse::<f64>().unwrap()
    };
    for (key, want) in [
        ("red_x", 0.68),
        ("red_y", 0.32),
        ("green_x", 0.265),
        ("green_y", 0.69),
        ("blue_x", 0.15),
        ("blue_y", 0.06),
        ("white_point_x", 0.3127),
        ("white_point_y", 0.329),
        ("min_luminance", 0.005),
        ("max_luminance", 1000.0),
    ] {
        let got = num(key);
        assert!((got - want).abs() < 1e-6 * want.max(1.0), "{key}: {got} != {want}");
    }
    // Round trip through this crate's demuxer, exactly.
    assert_eq!(video_light(&dst), light());
    let _ = std::fs::remove_dir_all(&dir);
}
