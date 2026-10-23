//! A refused access unit must not poison the access units after it.
//!
//! The oracle is the same stream with the corrupt access unit removed. A refused unit is
//! defined to leave no trace: every later unit must decode exactly as it would have if the
//! refused unit had never been sent. This is checked for every single-byte corruption of one
//! access unit in the middle of each committed fixture that the decoder refuses.
//!
//! Each fixture is read from `crates/ec-truehd/fixtures/` (made with ffmpeg's encoder, see
//! `fixture_exact.rs`). Nothing here needs ffmpeg at run time.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use ec_core::{CodecId, Demuxer};
use ec_matroska::MatroskaDemuxer;
use ec_truehd::TrueHdDecoder;

/// One access unit's outcome: `Ok(frame bytes)` (or `None` when no frame yet) or a refusal.
type Outcome = Result<Option<Vec<u8>>, String>;

const FIXTURES: [&str; 4] = [
    "truehd-s16-2ch.mkv",
    "truehd-s16-51.mkv",
    "truehd-s24-2ch.mkv",
    "truehd-s24-51.mkv",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name)
}

/// Every access unit of the file's TrueHD track, in stream order.
fn access_units(path: &Path) -> Vec<Vec<u8>> {
    let mut demux = MatroskaDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let stream = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD track")
        .index;
    let mut aus = Vec::new();
    loop {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{}: demux: {e}", path.display()),
        };
        if packet.stream == stream {
            aus.push(packet.data.to_vec());
        }
    }
    aus
}

/// Decodes `aus` in order through one decoder, recording each unit's outcome.
fn run(aus: &[Vec<u8>]) -> Vec<Outcome> {
    let mut dec = TrueHdDecoder::new();
    aus.iter()
        .map(|au| match dec.decode_access_unit(au) {
            Ok(frame) => Ok(frame.map(|f| f.data[0].to_vec())),
            Err(e) => Err(e.to_string()),
        })
        .collect()
}

#[test]
fn refused_access_unit_does_not_poison_later_units() {
    let mut total_refusals = 0;
    let mut total_checked = 0;
    let mut failures = Vec::new();

    for name in FIXTURES {
        let clean = access_units(&fixture(name));
        assert!(clean.len() > 40, "{name}: too few access units ({})", clean.len());

        let clean_run = run(&clean);
        assert!(
            clean_run.iter().all(Result::is_ok),
            "{name}: the clean stream must decode without refusals"
        );
        let frames = clean_run.iter().filter(|o| matches!(o, Ok(Some(_)))).count();
        assert!(frames > 20, "{name}: only {frames} frames decoded");

        // One unit in the middle of the stream, after the first major sync and several restarts.
        let k = clean.len() / 3;
        let without_k: Vec<Vec<u8>> = clean
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != k)
            .map(|(_, au)| au.clone())
            .collect();
        let oracle = run(&without_k);

        for pos in 0..clean[k].len() {
            let mut corrupt = clean[k].clone();
            corrupt[pos] ^= 0xFF;

            // Only units the decoder refuses matter here. Check that cheaply on the prefix.
            let mut prefix: Vec<Vec<u8>> = clean[..k].to_vec();
            prefix.push(corrupt.clone());
            let Err(err) = run(&prefix).pop().expect("prefix is non-empty") else {
                continue;
            };
            total_refusals += 1;

            let mut stream = clean.clone();
            stream[k] = corrupt;
            let got = run(&stream);
            assert!(got[k].is_err(), "{name} byte {pos}: the corrupt unit must be refused");

            for j in k + 1..clean.len() {
                total_checked += 1;
                if got[j] != oracle[j - 1] {
                    failures.push(format!(
                        "{name}: byte {pos} of unit {k} refused ({err}); unit {j} differs from the \
                         stream without unit {k}: got {:?}, want {:?}",
                        got[j].as_ref().map(|o| o.as_ref().map(Vec::len)),
                        oracle[j - 1].as_ref().map(|o| o.as_ref().map(Vec::len)),
                    ));
                    break;
                }
            }
        }
    }

    assert!(total_refusals >= 10, "only {total_refusals} refused corruptions found");
    assert!(
        failures.is_empty(),
        "{} of {} refused corruptions poisoned later units; first: {}",
        failures.len(),
        total_refusals,
        failures[0]
    );
    eprintln!("refused corruptions: {total_refusals}, later units compared: {total_checked}");
}
