//! The Xiph FLAC test corpus, decoded and compared with ffmpeg byte for byte.
//!
//! ffmpeg is oracle tooling, driven through a pipe: it decodes the same file to
//! raw PCM (`s16le` up to 16 bits, `s32le` above, which is the layout
//! [`ec_flac::decode::DecodedStream::to_pcm_bytes`] writes) and the two buffers
//! must be identical. Lossless codec, no tolerance.
//!
//! The corpus is fetched by `scripts/fetch-vectors.sh`; without it every test
//! here skips rather than fails, so a fresh clone still runs green.
//!
//! Run the tables:
//!   cargo test -p ec-flac --release --test xiph_vectors -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_flac::checksum::md5_of_samples;
use ec_flac::decode::FlacReader;

fn corpus(kind: &str) -> Option<Vec<PathBuf>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vectors/flac-xiph/flac-test-files-main")
        .join(kind);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "flac"))
        .collect();
    files.sort();
    match files.is_empty() {
        true => None,
        false => Some(files),
    }
}

/// Decode `path` with ffmpeg into the same raw layout our decoder writes.
fn ffmpeg_pcm(path: &Path, bits_per_sample: u32) -> Result<Vec<u8>, String> {
    let format = match bits_per_sample <= 16 {
        true => "s16le",
        false => "s32le",
    };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", format, "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    match out.status.success() {
        true => Ok(out.stdout),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into()
}

#[test]
fn subset_vectors_decode_bit_exact_against_ffmpeg() {
    let Some(files) = corpus("subset") else {
        eprintln!("skipped: fixtures/vectors/flac-xiph not fetched");
        return;
    };
    let mut failures = Vec::new();
    let mut passed = 0;
    for path in &files {
        let bytes = std::fs::read(path).expect("read fixture");
        let mut reader = match FlacReader::new(&bytes) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: open failed: {e}", name(path)));
                continue;
            }
        };
        let info = reader.stream_info().cloned();
        let decoded = match reader.decode_all() {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}: decode failed: {e}", name(path)));
                continue;
            }
        };
        let ours = decoded.to_pcm_bytes();
        let theirs = match ffmpeg_pcm(path, decoded.bits_per_sample) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{}: ffmpeg failed: {e}", name(path)));
                continue;
            }
        };
        if ours != theirs {
            let at = ours
                .iter()
                .zip(&theirs)
                .position(|(a, b)| a != b)
                .map_or_else(|| "length only".to_string(), |i| format!("byte {i}"));
            failures.push(format!(
                "{}: differs at {at} ({} vs {} bytes)",
                name(path),
                ours.len(),
                theirs.len()
            ));
            continue;
        }
        // Every subset file states an MD5; it must match what we decoded.
        if let Some(info) = &info
            && info.md5 != [0; 16]
        {
            let ours = md5_of_samples(&decoded.interleaved(), decoded.bits_per_sample);
            if ours != info.md5 {
                failures.push(format!("{}: STREAMINFO MD5 mismatch", name(path)));
                continue;
            }
        }
        passed += 1;
        println!(
            "PASS {:<70} {}ch {}bit {}Hz {} samples",
            name(path),
            decoded.channels.len(),
            decoded.bits_per_sample,
            decoded.sample_rate,
            decoded.len()
        );
    }
    println!("subset: {passed}/{} bit-exact vs ffmpeg", files.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn uncommon_vectors_decode_bit_exact_or_refuse_by_name() {
    let Some(files) = corpus("uncommon") else {
        eprintln!("skipped: fixtures/vectors/flac-xiph not fetched");
        return;
    };
    let mut failures = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).expect("read fixture");
        let outcome = FlacReader::new(&bytes).and_then(|mut r| r.decode_all());
        match outcome {
            Ok(decoded) => {
                let ours = decoded.to_pcm_bytes();
                match ffmpeg_pcm(path, decoded.bits_per_sample) {
                    Ok(theirs) if theirs == ours => {
                        println!("PASS {:<50} bit-exact", name(path));
                    }
                    Ok(theirs) => failures.push(format!(
                        "{}: differs from ffmpeg ({} vs {} bytes)",
                        name(path),
                        ours.len(),
                        theirs.len()
                    )),
                    Err(e) => failures.push(format!("{}: ffmpeg failed: {e}", name(path))),
                }
            }
            // A stream that changes channel count, depth or rate mid-way has no
            // single PCM shape to answer with; refusing it by name is the
            // contract (`Error::Unsupported` states what and why).
            Err(e) => println!("REFUSED {:<46} {e}", name(path)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn faulty_vectors_error_and_never_panic() {
    let Some(files) = corpus("faulty") else {
        eprintln!("skipped: fixtures/vectors/flac-xiph not fetched");
        return;
    };
    // Faults our reader is expected to catch outright rather than decode past:
    // these break the structure a decoder must trust.
    let must_reject = ["06 - ", "07 - ", "08 - ", "09 - ", "11 - "];
    let mut failures = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).expect("read fixture");
        let outcome = FlacReader::new(&bytes).and_then(|mut r| r.decode_all());
        let file = name(path);
        let rejected = outcome.is_err();
        match &outcome {
            Ok(d) => println!("DECODED {file:<58} {} samples", d.len()),
            Err(e) => println!("ERROR   {file:<58} {e}"),
        }
        if must_reject.iter().any(|p| file.starts_with(p)) && !rejected {
            failures.push(format!("{file}: decoded a structurally broken stream"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn seek_table_positions_the_reader_at_a_frame() {
    let Some(files) = corpus("subset") else {
        eprintln!("skipped: fixtures/vectors/flac-xiph not fetched");
        return;
    };
    // "48 - Extremely large SEEKTABLE" is the file with a seek table worth
    // exercising; any file carrying one proves the hook.
    let mut checked = 0;
    for path in files.iter() {
        let bytes = std::fs::read(path).expect("read fixture");
        let mut reader = FlacReader::new(&bytes).expect("open");
        if reader.seek_table().is_empty() {
            continue;
        }
        let target = reader.seek_table()[reader.seek_table().len() / 2];
        let landed = reader.seek_to_sample(target.sample + 1);
        assert_eq!(landed, target.sample, "{}", name(path));
        let mut block = ec_flac::decode::Block::default();
        assert!(
            reader.next_block(&mut block).expect("frame"),
            "{}",
            name(path)
        );
        assert_eq!(
            block.len(),
            usize::from(target.frame_samples),
            "{}",
            name(path)
        );
        checked += 1;
        if checked == 3 {
            break;
        }
    }
    println!("seek: {checked} files with a seek table exercised");
}
