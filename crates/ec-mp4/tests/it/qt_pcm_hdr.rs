//! QuickTime PCM sound descriptions (`in24`, `in32`, `fl32` with `wave/enda`
//! and `chan`) and the HDR `mdcv`/`clli` boxes, checked against ffmpeg.
//!
//! Sources are synthetic (ffmpeg `sine`, `testsrc2`). A case skips where
//! ffmpeg or an encoder is missing, unless `EC_REQUIRE_FIXTURES` is set.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{ContentLight, Demuxer, Error, MasteringPrimaries, Muxer, Packet, StreamInfo};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

fn skip(why: &str) -> bool {
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES set but: {why}"
    );
    eprintln!("skip: {why}");
    true
}

fn have_ffmpeg(encoder: &str) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output();
    let ok = out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(" {encoder} ")))
        && Command::new("ffprobe").arg("-version").output().is_ok();
    ok || !skip(&format!("no ffmpeg/ffprobe or no {encoder}"))
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ec-qtpcm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn read_all(path: &Path) -> (Vec<StreamInfo>, Vec<Packet>) {
    let mut d = Mp4Demuxer::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let streams = d.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => packets.push(p),
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    (streams, packets)
}

fn remux(dst: &Path, s: &[StreamInfo], p: &[Packet], setup: impl FnOnce(&mut Mp4Muxer<BufWriter<File>>)) {
    let mut m = Mp4Muxer::new(BufWriter::new(File::create(dst).unwrap())).unwrap();
    for st in s {
        m.add_stream(st.clone()).unwrap();
    }
    setup(&mut m);
    for pk in p {
        m.write_packet(pk).unwrap();
    }
    m.finish().unwrap();
}

/// The first sample entry of the first track with `fourcc`, size field included.
fn entry(path: &Path, fourcc: &[u8; 4]) -> Option<Vec<u8>> {
    let b = std::fs::read(path).unwrap();
    let at = b.windows(4).position(|w| w == fourcc)? - 4;
    let size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    Some(b[at..at + size].to_vec())
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .expect("ffprobe");
    assert!(out.stderr.is_empty(), "ffprobe stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn decoded_md5(path: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "md5", "-"])
        .output()
        .expect("ffmpeg");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn pcm_case(name: &str, codec: &str, channels: u32, fourcc: &[u8; 4], layout_box: bool) {
    if !have_ffmpeg(codec) {
        return;
    }
    let d = dir(name);
    let src = d.join("src.mov");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=d=0.5:sample_rate=48000"])
        .args(["-ac", &channels.to_string(), "-c:a", codec])
        .arg(&src)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg makes {codec}");
    let (streams, packets) = read_all(&src);
    assert_eq!(streams[0].params.codec.name(), codec, "demux names the codec");
    assert_eq!(
        streams[0].params.audio().unwrap().layout.channel_count() as u32,
        channels
    );
    let dst = d.join("out.mov");
    remux(&dst, &streams, &packets, |_| {});

    // ffprobe reads our file clean and states the same thing as the source.
    let show = ["-show_entries", "stream=codec_name,channels,channel_layout,sample_rate,bits_per_raw_sample", "-of", "csv=p=0"];
    assert_eq!(probe(&dst, &show), probe(&src, &show));
    assert!(probe(&dst, &show).starts_with(codec));
    // The sample entry is byte for byte what ffmpeg writes: version 1 fields,
    // wave/frma/enda, chan.
    let ours = entry(&dst, fourcc).expect("our entry");
    let theirs = entry(&src, fourcc).expect("ffmpeg entry");
    assert_eq!(ours, theirs, "{name}: sample entry");
    let _ = layout_box;
    // Same decoded samples, same packet bytes after a demux of our file.
    assert_eq!(decoded_md5(&dst), decoded_md5(&src));
    let (s2, p2) = read_all(&dst);
    assert_eq!(s2[0].params.codec, streams[0].params.codec);
    let cat = |p: &[Packet]| p.iter().flat_map(|p| p.data.to_vec()).collect::<Vec<u8>>();
    assert_eq!(cat(&p2), cat(&packets));
    let _ = std::fs::remove_dir_all(&d);
}

/// Each PCM sample-entry case by the name its test had, with the arguments
/// `pcm_case` takes.
type PcmTest = (&'static str, &'static str, &'static str, u32, &'static [u8; 4], bool);

const PCM_TESTS: &[PcmTest] = &[
    ("pcm_s24le_stereo_mov", "s24le", "pcm_s24le", 2, b"in24", true),
    ("pcm_s24be_stereo_mov", "s24be", "pcm_s24be", 2, b"in24", true),
    ("pcm_s24le_mono_mov", "s24m", "pcm_s24le", 1, b"in24", true),
    ("pcm_s24le_5_1_mov", "s24-51", "pcm_s24le", 6, b"in24", true),
    ("pcm_s24be_5_1_mov", "s24be-51", "pcm_s24be", 6, b"in24", true),
    ("pcm_s32le_stereo_mov", "s32le", "pcm_s32le", 2, b"in32", true),
    ("pcm_s32be_stereo_mov", "s32be", "pcm_s32be", 2, b"in32", true),
    ("pcm_f32le_stereo_mov", "f32le", "pcm_f32le", 2, b"fl32", true),
    ("pcm_f32be_stereo_mov", "f32be", "pcm_f32be", 2, b"fl32", true),
];

#[test]
fn pcm_sample_entries_are_ffprobe_clean_and_byte_equal() {
    let mut failures = Vec::new();
    for &(test, name, codec, channels, fourcc, layout_box) in PCM_TESTS {
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pcm_case(name, codec, channels, fourcc, layout_box)
        }))
        .is_ok();
        if !ok {
            failures.push(test);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

/// A caller that does not ask gets the 16-bit entries byte for byte as before
/// (no `chan`), and one that does gets ffmpeg's.
#[test]
fn sowt_gets_chan_only_when_asked() {
    if !have_ffmpeg("pcm_s16le") {
        return;
    }
    let d = dir("sowt");
    let src = d.join("src.mov");
    assert!(Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=d=0.2:sample_rate=48000", "-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (s, p) = read_all(&src);
    let plain = d.join("plain.mov");
    remux(&plain, &s, &p, |_| {});
    assert!(!entry(&plain, b"sowt").unwrap().windows(4).any(|w| w == b"chan"));
    let with = d.join("with.mov");
    remux(&with, &s, &p, |m| m.set_channel_layout_box(0, true).unwrap());
    assert_eq!(entry(&with, b"sowt"), entry(&src, b"sowt"));
    let _ = std::fs::remove_dir_all(&d);
}

fn light() -> ContentLight {
    ContentLight {
        max_cll: Some(1000.0),
        max_fall: Some(400.0),
        mastering_max: Some(1000.0),
        mastering_min: Some(0.005),
        mastering_primaries: Some(MasteringPrimaries::P3_D65),
    }
}

/// Main 10 HEVC, remuxed with `mdcv` and `clli`.
#[test]
fn hevc_main10_mdcv_clli_in_mp4() {
    if !have_ffmpeg("libx265") {
        return;
    }
    let d = dir("hdr");
    let src = d.join("src.mp4");
    assert!(Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=1"])
        .args(["-c:v", "libx265", "-pix_fmt", "yuv420p10le", "-tag:v", "hvc1"])
        .args(["-x265-params", "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc"])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (s, p) = read_all(&src);

    // Nothing is written unless the caller says so.
    let plain = d.join("plain.mp4");
    remux(&plain, &s, &p, |_| {});
    let bytes = std::fs::read(&plain).unwrap();
    assert!(!bytes.windows(4).any(|w| w == b"mdcv" || w == b"clli"));

    let dst = d.join("hdr.mp4");
    remux(&dst, &s, &p, |m| m.set_content_light(0, light()).unwrap());
    let json = probe(&dst, &["-show_streams", "-show_frames", "-read_intervals", "%+#1", "-of", "json"]);
    for want in [
        "Mastering display metadata",
        "Content light level metadata",
        "\"red_x\": \"34000/50000\"",
        "\"red_y\": \"16000/50000\"",
        "\"green_x\": \"13250/50000\"",
        "\"green_y\": \"34500/50000\"",
        "\"blue_x\": \"7500/50000\"",
        "\"blue_y\": \"3000/50000\"",
        "\"white_point_x\": \"15635/50000\"",
        "\"white_point_y\": \"16450/50000\"",
        "\"min_luminance\": \"50/10000\"",
        "\"max_luminance\": \"10000000/10000\"",
        "\"max_content\": 1000",
        "\"max_average\": 400",
    ] {
        assert!(json.contains(want), "ffprobe lacks {want}:\n{json}");
    }
    // The round trip through this crate's demuxer.
    let (s2, _) = read_all(&dst);
    let got = match &s2[0].params.media {
        ec_core::MediaParameters::Video(v) => v.light,
        _ => panic!("video"),
    };
    assert_eq!(got, light());

    // Luminances only: the BT.2020 primaries an `mdcv` defaults to.
    let lum = d.join("lum.mp4");
    remux(&lum, &s, &p, |m| {
        m.set_content_light(0, ContentLight { mastering_primaries: None, ..light() }).unwrap()
    });
    let (s3, _) = read_all(&lum);
    let got = match &s3[0].params.media {
        ec_core::MediaParameters::Video(v) => v.light,
        _ => panic!("video"),
    };
    assert_eq!(got.mastering_primaries, Some(MasteringPrimaries::BT2020));
    let _ = std::fs::remove_dir_all(&d);
}
