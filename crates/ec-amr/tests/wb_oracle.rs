//! AMR-WB against ffmpeg's `libopencore_amrwb`, which wraps the fixed-point decoder derived from
//! the 3GPP TS 26.173 reference code: the output must match it sample for sample.
//!
//! Fixtures are made here with ffmpeg's `libvo_amrwbenc` from a synthetic source, one file per
//! mode, as `.awb` and as mp4 (`sawb`). A machine without those ffmpeg codecs skips loudly; with
//! `EC_REQUIRE_FIXTURES=1` that is a failure.

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_amr::container::AmrDemuxer;
use ec_amr::wb::AmrWbDecoder;
use ec_core::registry::{CodecId, Decoder, Demuxer};

const RATES: [u32; 9] = [6600, 8850, 12650, 14250, 15850, 18250, 19850, 23050, 23850];
const PAYLOAD: [usize; 10] = [17, 23, 32, 36, 40, 46, 50, 58, 60, 5];

fn ffmpeg(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn skip(why: &str) -> bool {
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES=1 but {why}"
    );
    eprintln!("SKIP: {why}");
    true
}

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("ec-amr-wb-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SOURCE: &str = "aevalsrc='0.3*sin(2*PI*(200+60*sin(2*PI*0.7*t))*t)*(0.6+0.4*sin(2*PI*2.3*t))\
+0.15*sin(2*PI*1700*t)*sin(2*PI*3*t)+0.04*(random(0)-0.5)':s=16000:d=3";

/// Encode the synthetic source at `rate` into `path` (format chosen by `fmt`).
fn encode(path: &Path, rate: u32, fmt: &str) -> bool {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        SOURCE,
        "-ac",
        "1",
        "-c:a",
        "libvo_amrwbenc",
        "-b:a",
        &rate.to_string(),
        "-f",
        fmt,
        path.to_str().unwrap(),
    ])
    .is_some()
}

/// The reference decode of `path` as s16le.
fn reference(path: &Path) -> Option<Vec<i16>> {
    let raw = ffmpeg(&[
        "-c:a",
        "libopencore_amrwb",
        "-i",
        path.to_str().unwrap(),
        "-f",
        "s16le",
        "-",
    ])?;
    Some(
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect(),
    )
}

fn decode_all(d: &mut dyn Demuxer, codec: CodecId) -> Vec<i16> {
    let stream = d
        .streams()
        .iter()
        .find(|s| s.params.codec == codec)
        .expect("an AMR-WB track")
        .clone();
    assert_eq!(codec, CodecId::AmrWb);
    let mut dec = AmrWbDecoder::from_parameters(stream.params.clone()).unwrap();
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream != stream.index => continue,
            Ok(p) => out.extend_from_slice(dec.decode(&p.data).expect("decode")),
            Err(e) if e.is_eof() => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

fn have_tools() -> bool {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=16000:cl=mono",
        "-t",
        "0.1",
        "-c:a",
        "libvo_amrwbenc",
        "-f",
        "null",
        "-",
    ])
    .is_some()
        && ffmpeg(&["-decoders"])
            .is_some_and(|o| String::from_utf8_lossy(&o).contains("libopencore_amrwb"))
}

#[test]
fn every_mode_matches_the_reference_decoder_in_awb_and_mp4() {
    if !have_tools() && skip("ffmpeg lacks libvo_amrwbenc/libopencore_amrwb") {
        return;
    }
    let dir = Dir::new("modes");
    for rate in RATES {
        let awb = dir.path(&format!("m{rate}.awb"));
        let mp4 = dir.path(&format!("m{rate}.3gp"));
        assert!(encode(&awb, rate, "amr"), "encode {rate} awb");
        assert!(encode(&mp4, rate, "3gp"), "encode {rate} 3gp");
        let want = reference(&awb).expect("reference decode");
        assert!(want.len() >= 48000, "{rate}: {} samples", want.len());

        let mut d = AmrDemuxer::new(BufReader::new(File::open(&awb).unwrap())).unwrap();
        let got = decode_all(&mut d, CodecId::AmrWb);
        assert_eq!(got.len(), want.len(), "{rate} awb length");
        assert!(
            got == want,
            "{rate} awb: first difference at {:?}",
            got.iter().zip(&want).position(|(a, b)| a != b)
        );

        let mut d = ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(&mp4).unwrap())).unwrap();
        let got = decode_all(&mut d, CodecId::AmrWb);
        // the mp4 holds the same frames; ffmpeg may drop encoder delay, so compare the overlap
        assert!(
            got.len() >= want.len() - 640,
            "{rate} mp4 length {} vs {}",
            got.len(),
            want.len()
        );
        assert!(
            got[..want.len().min(got.len())] == want[..want.len().min(got.len())],
            "{rate} mp4 differs"
        );
    }
}

/// Deterministic pseudo-random bytes.
fn lcg(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 33) as u8
}

#[test]
fn comfort_noise_lost_and_corrupted_frames_match_the_reference() {
    if !have_tools() && skip("ffmpeg lacks libvo_amrwbenc/libopencore_amrwb") {
        return;
    }
    let dir = Dir::new("chaos");
    // Real frames of three modes, mixed with SID, lost and corrupted ones (quality bit kept set:
    // the reference wrapper ignores it).
    let mut frames: Vec<Vec<Vec<u8>>> = Vec::new();
    for rate in [6600, 12650, 23850] {
        let awb = dir.path(&format!("s{rate}.awb"));
        assert!(encode(&awb, rate, "amr"));
        let mut d = AmrDemuxer::new(BufReader::new(File::open(&awb).unwrap())).unwrap();
        let mut v = Vec::new();
        while let Ok(p) = d.next_packet() {
            v.push(p.data.to_vec());
        }
        frames.push(v);
    }
    let n = frames.iter().map(Vec::len).min().unwrap();
    let mut s = 0x1234_5678_9abc_def0u64;
    let mut seq: Vec<Vec<u8>> = Vec::new();
    for i in 0..n * 4 {
        let r = lcg(&mut s);
        let mut f = match r % 8 {
            0 | 1 => {
                let mut sid = vec![(9 << 3) | 4];
                sid.extend((0..PAYLOAD[9]).map(|_| lcg(&mut s)));
                sid[5] &= 0xE0;
                sid
            }
            2 => vec![(14 << 3) | 4],
            _ => frames[usize::from(lcg(&mut s)) % 3][i % n].clone(),
        };
        if r.is_multiple_of(5) {
            for b in f.iter_mut().skip(1) {
                *b ^= lcg(&mut s) & lcg(&mut s) & lcg(&mut s);
            }
        }
        seq.push(f);
    }
    let mut file = b"#!AMR-WB\n".to_vec();
    for f in &seq {
        file.extend_from_slice(f);
    }
    let path = dir.path("chaos.awb");
    std::fs::write(&path, &file).unwrap();
    let want = reference(&path).expect("reference decode");
    let mut d = AmrDemuxer::new(Cursor::new(file)).unwrap();
    let got = decode_all(&mut d, CodecId::AmrWb);
    assert_eq!(got.len(), want.len());
    assert!(
        got == want,
        "first difference at {:?}",
        got.iter().zip(&want).position(|(a, b)| a != b)
    );
}

#[test]
fn the_decoder_trait_yields_one_frame_per_packet() {
    use ec_core::frame::Frame;
    use ec_core::packet::{Buf, Packet};
    use ec_core::timebase::TimeBase;
    let mut dec = AmrWbDecoder::new();
    // mode 0 frame of zero bits, then a NO_DATA frame
    let mut data = vec![0x04u8];
    data.extend([0u8; 17]);
    let mut p = Packet::new(0, TimeBase::from_rate(16000), Buf::from_vec(data));
    p.pts = Some(0);
    dec.send_packet(&p).unwrap();
    match dec.receive_frame().unwrap() {
        Frame::Audio(a) => assert_eq!(a.samples, 320),
        _ => panic!("audio expected"),
    }
    assert!(dec.receive_frame().is_err());
    let p = Packet::new(0, TimeBase::from_rate(16000), Buf::from_vec(vec![0x7C]));
    dec.send_packet(&p).unwrap();
    assert!(dec.receive_frame().is_ok());
    // a truncated frame and a reserved type are errors, not panics
    assert!(dec.decode(&[0x04, 1, 2]).is_err());
    assert!(dec.decode(&[0x5C]).is_err());
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut dec = AmrWbDecoder::new();
    let mut s = 0xdead_beef_u64;
    for _ in 0..4000 {
        let len = 1 + usize::from(lcg(&mut s)) % 70;
        let mut p: Vec<u8> = (0..len).map(|_| lcg(&mut s)).collect();
        // make most packets well formed so the speech paths run
        if !lcg(&mut s).is_multiple_of(4) {
            let ft = lcg(&mut s) % 10;
            p.truncate(1 + PAYLOAD[usize::from(ft)]);
            p[0] = (ft << 3) | 4;
            p.resize(1 + PAYLOAD[usize::from(ft)], 0x5a);
        }
        let _ = dec.decode(&p);
    }
}
