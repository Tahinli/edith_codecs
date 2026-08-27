//! Bit-exactness against an external decoder: the first 60 s of a real 7.1
//! Blu-ray TrueHD remux (3 substreams, noise type 1 matrices), and
//! externally encoded MLP/TrueHD streams (mono, stereo, 5.1 pink noise —
//! every codebook, matrices with the 2-channel noise generator, two
//! substreams). Both need `ffmpeg` on PATH and skip loudly without it; the
//! remux is a live discovery over `~/Downloads`.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{CodecId, Demuxer};
use ec_matroska::MatroskaDemuxer;
use ec_truehd::{TrueHdDecoder, TrueHdEncoder, frame_length};

fn find_file() -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let mut all = Vec::new();
    walk(&PathBuf::from(home).join("Downloads"), &mut all);
    all.into_iter().find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("Book of Dragons") && n.ends_with(".mkv"))
    })
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn s32le(bytes: &[u8]) -> Vec<i32> {
    bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Compares `ours` with `reference` sample-for-sample; returns (exact,
/// total, first mismatch index).
fn compare(ours: &[i32], reference: &[i32]) -> (usize, usize, Option<usize>) {
    let n = ours.len().min(reference.len());
    let mut exact = 0;
    let mut first = None;
    for i in 0..n {
        if ours[i] == reference[i] {
            exact += 1;
        } else if first.is_none() {
            first = Some(i);
        }
    }
    (exact, n, first)
}

#[test]
fn first_60_seconds_of_a_real_7_1_remux_are_bit_exact() {
    let Some(path) = find_file() else {
        eprintln!("skip: no 'Book of Dragons ... TrueHD ...mkv' under ~/Downloads");
        return;
    };
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }
    const SECONDS: usize = 60;
    let mut demux = MatroskaDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let track = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD track");
    let stream_index = track.index;

    let reference = s32le(&ffmpeg(&[
        "-i",
        path.to_str().unwrap(),
        "-map",
        &format!("0:{stream_index}"),
        "-t",
        &SECONDS.to_string(),
        "-f",
        "s32le",
        "-",
    ]));
    let channels = 8;
    let wanted = SECONDS * 48_000 * channels;

    let mut decoder = TrueHdDecoder::new();
    let mut ours: Vec<i32> = Vec::with_capacity(wanted);
    let mut aus = 0;
    while ours.len() < wanted {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        };
        if packet.stream != stream_index {
            continue;
        }
        let frame = decoder
            .decode_access_unit(&packet.data)
            .unwrap_or_else(|e| panic!("AU {aus}: {e}"));
        aus += 1;
        if let Some(f) = frame {
            assert_eq!(f.channels(), channels);
            ours.extend(s32le(&f.data[0]));
        }
    }
    let (exact, total, first) = compare(&ours, &reference);
    let stats = decoder.check_stats();
    for ch in 0..channels {
        let n = total / channels;
        let e = (0..n)
            .filter(|&i| ours[i * channels + ch] == reference[i * channels + ch])
            .count();
        eprintln!("channel {ch}: {e}/{n} exact");
    }
    eprintln!(
        "{aus} access units, {total} samples compared ({} s), {exact} exact ({:.4}%), first mismatch {first:?}, {stats:?}",
        total / channels / 48_000,
        100.0 * exact as f64 / total as f64
    );
    assert!(
        total >= (SECONDS - 1) * 48_000 * channels,
        "only {total} samples"
    );
    assert_eq!(stats.lossless_check_failures, 0, "{stats:?}");
    assert_eq!(stats.parity_failures, 0, "{stats:?}");
    assert_eq!(stats.restart_crc_failures, 0, "{stats:?}");
    assert_eq!(stats.length_mismatches, 0, "{stats:?}");
    assert!(
        exact as f64 >= 0.9999 * total as f64,
        "{exact}/{total} exact, first mismatch {first:?}"
    );
}

/// An externally encoded MLP stream (raw access units) decoded by us must
/// match the external decoder's own output exactly; noise drives every
/// codebook and a wide range of LSB widths.
#[test]
fn externally_encoded_mlp_round_trips_bit_exact() {
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ec-truehd-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for (codec, layout, channels) in [
        ("mlp", "mono", 1usize),
        ("mlp", "stereo", 2),
        ("mlp", "5.1", 6),
        ("truehd", "mono", 1),
        ("truehd", "stereo", 2),
        ("truehd", "5.1", 6),
    ] {
        let name = format!("{codec}-{layout}");
        let mlp = dir.join(format!(
            "{name}.{}",
            if codec == "mlp" { "mlp" } else { "thd" }
        ));
        let src = format!(
            "anoisesrc=color=pink:amplitude=0.3:seed=7:sample_rate=48000:duration=3,aformat=sample_fmts=s32:channel_layouts={layout}"
        );
        ffmpeg(&[
            "-y",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-c:a",
            codec,
            "-strict",
            "-2",
            mlp.to_str().unwrap(),
        ]);
        let reference = s32le(&ffmpeg(&["-i", mlp.to_str().unwrap(), "-f", "s32le", "-"]));
        let data = std::fs::read(&mlp).unwrap();

        let mut decoder = TrueHdDecoder::new();
        let mut ours = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let len = frame_length(&data[pos..]).unwrap();
            if let Some(f) = decoder.decode_access_unit(&data[pos..pos + len]).unwrap() {
                assert_eq!(f.channels(), channels);
                ours.extend(s32le(&f.data[0]));
            }
            pos += len;
        }
        let (exact, total, first) = compare(&ours, &reference);
        let stats = decoder.check_stats();
        eprintln!(
            "{name}: {exact}/{total} exact (ours {}, reference {}), first mismatch {first:?}, {stats:?}",
            ours.len(),
            reference.len()
        );
        if stats.lossless_check_failures + stats.parity_failures + stats.restart_crc_failures != 0
            || exact != total
            || total < 2 * 48_000 * channels
        {
            failures.push(format!(
                "{name}: {exact}/{total}, first mismatch {first:?}, {stats:?}"
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// GATE 2: a foreign decoder (ffmpeg's own `truehd`/MLP decoder, not this
/// crate's) decodes bytes from [`TrueHdEncoder`] sample-exact against the
/// PCM that went in — the decisive oracle per the shared-oracle-blindness
/// class (see the project ledger): our own decoder alone, as an encoder
/// gate, would only prove the two halves of this crate agree with each
/// other.
#[test]
fn ffmpeg_decodes_our_encoder_bit_exact() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found, skipping");
        return;
    }
    use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
    use ec_core::packet::Buf;
    use ec_core::registry::Encoder;

    let n = 48_000usize;
    let mut samples = Vec::with_capacity(n * 2);
    let mut lcg: u32 = 0xC0FF_EE00;
    for i in 0..n {
        let l = ((i as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 3_000_000.0) as i32;
        lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let r = l.wrapping_add((lcg >> 8) as i32 % 20_000 - 10_000);
        samples.push(l.clamp(-8_388_608, 8_388_607));
        samples.push(r.clamp(-8_388_608, 8_388_607));
    }
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in &samples {
        bytes.extend_from_slice(&(s << 8).to_le_bytes());
    }
    let reference = s32le(&bytes);
    let frame = AudioFrame::try_new(
        SampleFormat::S32,
        false,
        ChannelLayout::Stereo,
        48_000,
        n,
        vec![Buf::from_vec(bytes)],
    )
    .unwrap();

    let mut enc = TrueHdEncoder::new(48_000).unwrap();
    enc.send_frame(&Frame::Audio(frame)).unwrap();
    enc.flush().unwrap();
    let mut thd = Vec::new();
    while let Ok(pkt) = enc.receive_packet() {
        thd.extend_from_slice(&pkt.data);
    }

    let dir = std::env::temp_dir().join(format!("ec-truehd-enc-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ours.thd");
    std::fs::write(&path, &thd).unwrap();
    let out = ffmpeg(&[
        "-f",
        "truehd",
        "-i",
        path.to_str().unwrap(),
        "-f",
        "s32le",
        "-",
    ]);
    let _ = std::fs::remove_dir_all(&dir);

    let decoded = s32le(&out);
    let (exact, total, first) = compare(&decoded, &reference);
    assert_eq!(
        exact, total,
        "ffmpeg vs our own PCM: first mismatch {first:?} of {total}"
    );
}
