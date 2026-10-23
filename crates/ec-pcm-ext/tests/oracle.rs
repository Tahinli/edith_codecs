//! Bit-exact oracle: fixtures are generated with ffmpeg from synthetic lavfi
//! sources, decoded here and by ffmpeg (`-f s16le`), and must match exactly.
//!
//! Without ffmpeg the tests skip loudly; with `EC_REQUIRE_FIXTURES=1` a
//! missing ffmpeg is a failure.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Demuxer, MediaParameters};
use ec_core::frame::{ChannelLayout, SampleFormat};
use ec_core::packet::Buf;
use ec_pcm_ext::PcmExtDecoder;

fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg").arg("-version").output().is_ok();
    if !ok {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but ffmpeg is not installed"
        );
        eprintln!("ffmpeg absent - skipping");
    }
    ok
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ec-pcm-ext-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("tmp");
    d
}

/// Synthetic source: `kind` 0 = tones with a chirp, 1 = noise.
fn source(kind: u32, channels: u32, rate: u32) -> String {
    match (kind, channels) {
        (0, 1) => format!(
            "aevalsrc='0.6*sin(2*PI*440*t)+0.25*sin(2*PI*1234*t*t)':s={rate}:d=1.3"
        ),
        (0, _) => format!(
            "aevalsrc='0.6*sin(2*PI*440*t)+0.25*sin(2*PI*1234*t*t)|0.5*sin(2*PI*660*t)':s={rate}:d=1.3"
        ),
        (_, 1) => format!("anoisesrc=d=1.3:r={rate}:a=0.9:seed=7"),
        _ => format!(
            "anoisesrc=d=1.3:r={rate}:a=0.9:seed=7[a];anoisesrc=d=1.3:r={rate}:a=0.5:seed=9[b];[a][b]amerge"
        ),
    }
}

fn make(dir: &Path, name: &str, codec: &str, kind: u32, channels: u32, rate: u32) -> PathBuf {
    let out = dir.join(name);
    let src = source(kind, channels, rate);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-nostdin", "-y", "-v", "error", "-f", "lavfi"]);
    if src.contains(';') {
        cmd.args(["-i", "anullsrc=d=0.01", "-filter_complex"]).arg(&src);
    } else {
        cmd.arg("-i").arg(&src);
    }
    cmd.args(["-ac", &channels.to_string(), "-c:a", codec]).arg(&out);
    let o = cmd.output().expect("ffmpeg");
    assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
    out
}

fn ffmpeg_pcm(path: &Path) -> Vec<u8> {
    let o = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s16le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

fn params(codec: CodecId, channels: usize, rate: u32, extra: &[u8]) -> CodecParameters {
    let mut p = CodecParameters::new(codec);
    p.media = MediaParameters::Audio(AudioParameters {
        sample_rate: rate,
        layout: ChannelLayout::from_count(channels),
        format: Some(SampleFormat::S16),
        bits_per_sample: Some(16),
    });
    if !extra.is_empty() {
        p.extradata = Some(Buf::copy_from_slice(extra));
    }
    p
}

fn to_bytes(s: &[i16]) -> Vec<u8> {
    s.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn decode_demuxer(mut d: impl Demuxer, want: CodecId) -> Vec<u8> {
    let st = d
        .streams()
        .iter()
        .find(|s| s.params.codec == want)
        .unwrap_or_else(|| panic!("no {want:?} stream"))
        .clone();
    let mut dec = PcmExtDecoder::from_parameters(st.params.clone()).expect("decoder");
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream != st.index => {}
            Ok(p) => out.extend(to_bytes(dec.decode(&p.data).expect("decode"))),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{e}"),
        }
    }
    out
}

fn decode_wav(path: &Path) -> Vec<u8> {
    let mut r = ec_riff::WavReader::new(BufReader::new(File::open(path).expect("open"))).expect("wav");
    let c = r.compressed().expect("compressed wav").clone();
    let spec = r.spec();
    let codec = ec_riff::WavCompressed::codec_of(c.tag).expect("codec");
    let mut dec = PcmExtDecoder::from_parameters(params(
        codec,
        usize::from(spec.channels),
        spec.sample_rate,
        &c.extra,
    ))
    .expect("decoder");
    let data = r.read_data_bytes().expect("data");
    let step = match c.tag {
        0x0006 | 0x0007 => 4096,
        _ => usize::from(c.block_align),
    };
    let mut out = Vec::new();
    for chunk in data.chunks(step) {
        out.extend(to_bytes(dec.decode(chunk).expect("decode")));
    }
    out
}

fn decode_avi(path: &Path, want: CodecId) -> Vec<u8> {
    let d = ec_riff::AviDemuxer::new(BufReader::new(File::open(path).expect("open"))).expect("avi");
    decode_demuxer(d, want)
}

fn decode_mov(path: &Path, want: CodecId) -> Vec<u8> {
    let d = ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mov");
    decode_demuxer(d, want)
}

fn check(name: &str, got: Vec<u8>, path: &Path) {
    let want = ffmpeg_pcm(path);
    assert_eq!(got.len(), want.len(), "{name}: byte count");
    let bad = got.chunks(2).zip(want.chunks(2)).position(|(a, b)| a != b);
    assert_eq!(bad, None, "{name}: first differing sample");
    assert!(!got.is_empty(), "{name}: empty");
}

#[test]
fn every_format_is_bit_exact_against_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tmpdir("oracle");
    let mut n = 0;
    // (ffmpeg encoder, codec id, containers)
    let cases: [(&str, CodecId, &[&str]); 5] = [
        ("adpcm_ima_wav", CodecId::ImaAdpcmWav, &["wav", "avi"]),
        ("adpcm_ms", CodecId::MsAdpcm, &["wav", "avi"]),
        ("adpcm_ima_qt", CodecId::ImaAdpcmQt, &["mov"]),
        ("pcm_alaw", CodecId::PcmAlaw, &["wav", "avi", "mov"]),
        ("pcm_mulaw", CodecId::PcmMulaw, &["wav", "avi", "mov"]),
    ];
    for (enc, codec, containers) in cases {
        for ext in containers {
            for channels in [1u32, 2] {
                for (kind, rate) in [(0u32, 22050u32), (1, 44100), (0, 8000)] {
                    let name = format!("{enc}-{ext}-{channels}ch-{kind}-{rate}.{ext}");
                    let path = make(&dir, &name, enc, kind, channels, rate);
                    let got = match *ext {
                        "wav" => decode_wav(&path),
                        "avi" => decode_avi(&path, codec),
                        _ => decode_mov(&path, codec),
                    };
                    check(&name, got, &path);
                    n += 1;
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("pcm-ext oracle: {n} fixtures bit-exact vs ffmpeg");
}

/// All 256 G.711 code points decode like ffmpeg's tables.
#[test]
fn g711_tables_match_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tmpdir("g711");
    let raw: Vec<u8> = (0..=255u8).collect();
    for (fmt, codec) in [("alaw", CodecId::PcmAlaw), ("mulaw", CodecId::PcmMulaw)] {
        let p = dir.join(format!("all.{fmt}"));
        std::fs::write(&p, &raw).expect("write");
        let o = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-f", fmt, "-ar", "8000", "-ac", "1", "-i"])
            .arg(&p)
            .args(["-f", "s16le", "-"])
            .output()
            .expect("ffmpeg");
        assert!(o.status.success());
        let mut dec = PcmExtDecoder::from_parameters(params(codec, 1, 8000, &[])).expect("dec");
        assert_eq!(to_bytes(dec.decode(&raw).expect("decode")), o.stdout, "{fmt}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Garbage and truncation are errors or short output, never a panic.
#[test]
fn garbage_does_not_panic() {
    let mut seed = 0x1234_5678u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for codec in [
        CodecId::ImaAdpcmWav,
        CodecId::MsAdpcm,
        CodecId::ImaAdpcmQt,
        CodecId::PcmAlaw,
        CodecId::PcmMulaw,
    ] {
        for ch in [1usize, 2, 3] {
            let Ok(mut d) = PcmExtDecoder::from_parameters(params(codec, ch, 8000, &[])) else {
                continue;
            };
            for len in 0..600 {
                let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                let _ = d.decode(&data);
            }
        }
    }
}
