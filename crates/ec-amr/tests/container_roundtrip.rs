//! The `.amr` demuxer and the mp4 `samr` path against ffmpeg-made files: one
//! synthetic sine per AMR-NB mode, stream-copied into `.amr`, `.3gp` and `.mp4`.
//! All three must yield the same packets, and the `.amr` bytes must be exactly
//! the magic followed by those packets back to back.

use std::fs::File;
use std::io::BufReader;
use std::process::Command;

use ec_amr::container::{AmrDemuxer, Kind, probe};
use ec_core::registry::{CodecId, Demuxer, MediaParameters, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};

const RATES: [u32; 8] = [4750, 5150, 5900, 6700, 7400, 7950, 10200, 12200];
const PAYLOAD: [usize; 8] = [12, 13, 15, 17, 19, 20, 26, 31];

fn ffmpeg(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn skip(why: &str) -> bool {
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES=1 but {why}"
    );
    eprintln!("SKIP: {why}");
    true
}

fn drain(d: &mut dyn Demuxer) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push(p.data.to_vec()),
            Err(e) if e.is_eof() => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn amr_3gp_mp4_agree_for_all_modes() {
    let dir = std::env::temp_dir().join(format!("ec-amr-container-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (mode, rate) in RATES.iter().enumerate() {
        let amr = dir.join(format!("m{mode}.amr"));
        let enc = ffmpeg(&[
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1.5", "-ar", "8000", "-ac", "1",
            "-c:a", "libopencore_amrnb", "-b:a", &rate.to_string(), "-f", "amr",
            amr.to_str().unwrap(),
        ]);
        if !enc {
            skip("ffmpeg with libopencore_amrnb is not available");
            return;
        }
        let bytes = std::fs::read(&amr).unwrap();
        assert_eq!(probe(&bytes), Some(Kind::Nb));

        let mut d = AmrDemuxer::new(BufReader::new(File::open(&amr).unwrap())).unwrap();
        let s = &d.streams()[0];
        assert_eq!(s.params.codec, CodecId::AmrNb);
        let MediaParameters::Audio(a) = &s.params.media else { panic!() };
        assert_eq!(a.sample_rate, 8000);
        let mut packets = Vec::new();
        let mut n = 0i64;
        loop {
            match d.next_packet() {
                Ok(p) => {
                    assert_eq!((p.pts, p.duration), (Some(n * 160), Some(160)));
                    assert_eq!(p.time_base, TimeBase::from_rate(8000));
                    packets.push(p.data.to_vec());
                    n += 1;
                }
                Err(e) if e.is_eof() => break,
                Err(e) => panic!("{e}"),
            }
        }
        // ffprobe's own packet count for the same file (75 frames of 20 ms plus
        // the encoder's flush frame).
        let probed = Command::new("ffprobe")
            .args(["-v", "error", "-count_packets", "-select_streams", "a:0"])
            .args(["-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
            .arg(&amr)
            .output()
            .expect("ffprobe");
        let probed: usize = String::from_utf8_lossy(&probed.stdout).trim().parse().unwrap();
        assert_eq!(packets.len(), probed, "mode {mode} frame count");
        assert!((75..=76).contains(&packets.len()));
        for p in &packets {
            assert_eq!(p.len(), 1 + PAYLOAD[mode], "mode {mode} packet size");
            assert_eq!((p[0] >> 3) & 15, mode as u8);
        }
        assert_eq!(bytes[6..], packets.concat()[..], "mode {mode} byte-identical");

        // Seek by index, then the next packet is the one at that frame.
        d.seek(0, Timestamp::new(40 * 160 + 7, TimeBase::from_rate(8000)), SeekMode::SyncBefore)
            .unwrap();
        let p = d.next_packet().unwrap();
        assert_eq!((p.pts, p.data.to_vec()), (Some(40 * 160), packets[40].clone()));

        // The same stream in 3gp and mp4 (samr + damr; ffmpeg refuses amr in .mp4).
        for ext in ["3gp", "3g2", "mov"] {
            let out = dir.join(format!("m{mode}.{ext}"));
            assert!(ffmpeg(&[
                "-i", amr.to_str().unwrap(), "-c", "copy", out.to_str().unwrap()
            ]));
            let mut m = ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(&out).unwrap())).unwrap();
            let s = &m.streams()[0];
            assert_eq!(s.params.codec, CodecId::AmrNb, "{ext} codec");
            assert!(ext == "mov" || s.params.extradata.is_some(), "{ext} damr extradata");
            let MediaParameters::Audio(a) = &s.params.media else { panic!() };
            assert_eq!(a.sample_rate, 8000);
            assert_eq!(a.layout, ec_core::frame::ChannelLayout::Mono);
            assert_eq!(drain(&mut m), packets, "mode {mode} {ext} packets");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
