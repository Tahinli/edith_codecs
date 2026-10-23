//! DTS through the probe: a raw `.dts` elementary stream (16-bit big and little
//! endian), and DTS in WAV (the `0x2001` tag ffmpeg writes, and the PCM-tagged
//! stereo file DTS-CD rips use) all open as `CodecId::Dts` with the core's rate
//! and channels, and hand back the stream's bytes frame by frame.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::CodecId;
use ec_core::registry::{MediaType, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};
use ec_probe::{Format, Reader};

fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some_and(|v| !v.is_empty() && v != "0")
}

fn work() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ec-probe-dts-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg(args: &[&str], out: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(out)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A raw DTS stream of `channels` channels, 0.5 s at 48 kHz.
fn raw_dts(channels: &str) -> Option<Vec<u8>> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = work().join(format!("{channels}.dts"));
    let ok = ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.5",
            "-ac",
            channels,
            "-c:a",
            "dca",
            "-strict",
            "-2",
        ],
        &path,
    );
    if !ok {
        assert!(!required(), "ffmpeg cannot make a DTS fixture");
        eprintln!("skip: no ffmpeg with the dca encoder");
        return None;
    }
    std::fs::read(&path).ok()
}

fn wav_pcm_tagged(dts: &[u8], rate: u32) -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(36 + dts.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 4).to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(dts.len() as u32).to_le_bytes());
    v.extend_from_slice(dts);
    v
}

fn read_all(r: &mut Reader) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    while let Ok(p) = r.next_packet() {
        v.push(p.data.to_vec());
    }
    v
}

fn check(bytes: &[u8], file: &str, format: Format, channels: usize, want: &[u8]) {
    let path = work().join(file);
    std::fs::write(&path, bytes).unwrap();
    let mut r = Reader::open(&path).unwrap();
    assert_eq!(r.format(), format);
    let s = r.default_stream(MediaType::Audio).unwrap().clone();
    assert_eq!(s.params.codec, CodecId::Dts);
    let a = s.params.audio().unwrap();
    assert_eq!(
        (a.sample_rate, a.layout.channel_count()),
        (48_000, channels)
    );
    let packets = read_all(&mut r);
    assert!(packets.len() > 20);
    assert_eq!(packets.concat(), want);
    // Every packet is one access unit of 512 samples; a seek lands on a frame.
    let mut r = Reader::open(&path).unwrap();
    let to = Timestamp::new(512 * 10 + 7, TimeBase::from_rate(48_000));
    let landed = r.seek(0, to, SeekMode::SyncBefore).unwrap();
    assert_eq!(landed.ticks, 512 * 10);
    assert_eq!(r.next_packet().unwrap().data.to_vec(), packets[10]);
}

#[test]
fn raw_dts_stereo_and_51() {
    for (ch, n) in [("2", 2), ("6", 6)] {
        let Some(raw) = raw_dts(ch) else {
            return;
        };
        assert_eq!(&raw[..4], &[0x7F, 0xFE, 0x80, 0x01]);
        check(&raw, &format!("raw{ch}.dts"), Format::Dts, n, &raw);
    }
}

#[test]
fn raw_dts_little_endian() {
    let Some(raw) = raw_dts("2") else {
        return;
    };
    let mut le = raw.clone();
    for p in le.as_chunks_mut::<2>().0.iter_mut() {
        p.swap(0, 1);
    }
    check(&le, "le.dts", Format::Dts, 2, &le);
}

#[test]
fn dts_in_wav_pcm_tag_and_2001_tag() {
    let Some(raw) = raw_dts("2") else {
        return;
    };
    check(
        &wav_pcm_tagged(&raw, 48_000),
        "pcm.wav",
        Format::Wav,
        2,
        &raw,
    );
    // Own copy: other tests regenerate the shared 2.dts concurrently.
    let src = work().join("tag2001-src.dts");
    std::fs::write(&src, &raw).unwrap();
    let wav = work().join("tag2001.wav");
    assert!(ffmpeg(
        &["-i", src.to_str().unwrap(), "-c", "copy", "-f", "wav"],
        &wav
    ));
    let bytes = std::fs::read(&wav).unwrap();
    assert_eq!(&bytes[20..22], &[0x01, 0x20], "ffmpeg writes tag 0x2001");
    check(&bytes, "tag2001b.wav", Format::Wav, 2, &raw);
}

#[test]
fn plain_pcm_wav_is_still_pcm() {
    let pcm = vec![0u8; 4 * 4800];
    let path = work().join("plain.wav");
    std::fs::write(&path, wav_pcm_tagged(&pcm, 48_000)).unwrap();
    let r = Reader::open(&path).unwrap();
    assert_eq!(
        r.default_stream(MediaType::Audio).unwrap().params.codec,
        CodecId::PcmS16Le
    );
}
