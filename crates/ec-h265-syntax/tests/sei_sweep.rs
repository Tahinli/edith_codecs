//! The SEI reader over the JCT-VC conformance streams.
//!
//! Every prefix and suffix SEI NAL unit of every stream must parse without error,
//! and the streams that carry a `decoded_picture_hash` are counted. The sweep is
//! skipped when the vectors are absent, unless `EC_REQUIRE_FIXTURES` is set, in
//! which case an absent fixture fails the test.

use std::path::{Path, PathBuf};

use ec_h265_syntax::nal::split_annex_b;
use ec_h265_syntax::sei::{SeiMessage, parse_sei_rbsp};

/// Every `.bin` or `.bit` stream under `fixtures/vectors/hevc-jctvc`, two directory
/// levels deep, sorted by path. `EC_H265_JCTVC_DIR` overrides the root.
fn streams() -> Vec<PathBuf> {
    let root = match std::env::var("EC_H265_JCTVC_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/hevc-jctvc"),
    };
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() && depth > 0 {
                walk(&p, depth - 1, out);
            } else if matches!(p.extension().and_then(|x| x.to_str()), Some("bin" | "bit")) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root, 2, &mut out);
    out
}

#[test]
fn every_sei_in_the_conformance_streams_parses() {
    let streams = streams();
    if streams.is_empty() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but fixtures/vectors/hevc-jctvc has no streams"
        );
        eprintln!("SEI sweep skipped: vectors absent");
        return;
    }
    let mut with_hash = 0usize;
    let mut hash_streams = Vec::new();
    let mut sei_nals = 0usize;
    let mut messages = 0usize;
    let mut hashes = 0usize;
    let mut failures = Vec::new();
    for path in &streams {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{name}: read: {e}"));
                continue;
            }
        };
        let mut stream_hashes = 0usize;
        for nal in split_annex_b(&data) {
            if nal.header.layer_id > 0 || !matches!(nal.header.nal_type.code(), 39 | 40) {
                continue;
            }
            sei_nals += 1;
            match parse_sei_rbsp(&nal.rbsp()) {
                Ok(msgs) => {
                    messages += msgs.len();
                    for msg in &msgs {
                        if matches!(msg, SeiMessage::DecodedPictureHash(_)) {
                            stream_hashes += 1;
                        }
                    }
                }
                Err(e) => failures.push(format!("{name}: SEI NAL #{sei_nals}: {e}")),
            }
        }
        if stream_hashes > 0 {
            with_hash += 1;
            hash_streams.push(name.clone());
        }
        hashes += stream_hashes;
    }
    eprintln!(
        "SEI sweep: {} streams, {with_hash} carry decoded_picture_hash ({hashes} hash messages), \
         {sei_nals} SEI NAL units, {messages} messages, {} parse failures",
        streams.len(),
        failures.len()
    );
    eprintln!("streams with decoded_picture_hash: {}", hash_streams.join(" "));
    assert!(failures.is_empty(), "SEI that failed to parse:\n{}", failures.join("\n"));
    assert!(with_hash > 0, "no conformance stream carries decoded_picture_hash SEI");
}
