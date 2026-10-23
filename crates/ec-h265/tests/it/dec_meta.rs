//! Metadata on the decoder's output frames: HDR SEI from the IRAP on, the VUI
//! colour description, the decoded picture hash, and the rule that Dolby Vision
//! NAL units (62, 63) and malformed SEI change nothing.
//!
//! The streams come from `fixtures/vectors/hevc-jctvc`. Without them the tests
//! skip, unless `EC_REQUIRE_FIXTURES` is set, in which case they fail.

use std::path::{Path, PathBuf};

use ec_h265::dec::{Decoder, Frame};
use ec_h265_syntax::nal::{escape_rbsp, split_annex_b};
use ec_h265_syntax::sei::{ContentLightLevel, MasteringDisplay, PictureHash};

fn stream_files() -> Vec<PathBuf> {
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

/// The raw NAL units (two-byte header and escaped payload) of an Annex-B stream.
fn raw_nals(data: &[u8]) -> Vec<Vec<u8>> {
    split_annex_b(data)
        .iter()
        .map(|n| {
            let mut v = n.header.to_bytes().to_vec();
            v.extend_from_slice(n.payload);
            v
        })
        .collect()
}

/// Decode with picture management only, returning every frame, or the first error.
fn decode(nals: &[Vec<u8>]) -> Result<Vec<Frame>, String> {
    let mut dec = Decoder::new();
    dec.set_pixel_decode(false);
    for nal in nals {
        dec.push_nal(nal).map_err(|e| e.to_string())?;
    }
    dec.flush().map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    while let Some(f) = dec.next_frame() {
        frames.push(f);
    }
    Ok(frames)
}

/// The first conformance stream that decodes without error, with its NAL units.
fn clean_stream() -> Option<(String, Vec<Vec<u8>>)> {
    for path in stream_files() {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let nals = raw_nals(&data);
        if decode(&nals).is_ok() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            return Some((name, nals));
        }
    }
    None
}

fn require_stream() -> Option<(String, Vec<Vec<u8>>)> {
    let found = clean_stream();
    if found.is_none() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but no clean conformance stream is available"
        );
        eprintln!("decoder metadata tests skipped: vectors absent");
    }
    found
}

/// What a frame looks like to a caller, for comparing two decodes.
fn key(f: &Frame) -> (i32, u32, usize, usize, Vec<u16>, Vec<u16>, Vec<u16>) {
    (
        f.poc,
        f.au_index,
        f.picture.width,
        f.picture.height,
        f.picture.y.clone(),
        f.picture.cb.clone(),
        f.picture.cr.clone(),
    )
}

/// A NAL unit of the given type, with the payload escaped.
fn nal_unit(nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut v = vec![nal_type << 1, 1];
    escape_rbsp(rbsp, &mut v);
    v
}

fn is_vcl(nal: &[u8]) -> bool {
    matches!((nal[0] >> 1) & 0x3f, 0..=21)
}

/// The HDR10 mastering display of a 1000 nit, 0.0050 nit BT.2020 grade, then a
/// content light level of MaxCLL 1000 and MaxFALL 400, as one prefix SEI RBSP.
fn hdr10_prefix_rbsp() -> Vec<u8> {
    let mut mdcv = Vec::new();
    for (x, y) in [
        (8500u16, 39_850u16),
        (6550, 2300),
        (35_400, 14_600),
        (15_635, 16_450),
    ] {
        mdcv.extend_from_slice(&x.to_be_bytes());
        mdcv.extend_from_slice(&y.to_be_bytes());
    }
    mdcv.extend_from_slice(&10_000_000u32.to_be_bytes());
    mdcv.extend_from_slice(&50u32.to_be_bytes());
    let mut rbsp = vec![137, 24];
    rbsp.extend_from_slice(&mdcv);
    rbsp.extend_from_slice(&[144, 4, 0x03, 0xe8, 0x01, 0x90, 0x80]);
    rbsp
}

#[test]
fn dolby_vision_nal_units_change_nothing() {
    let Some((name, nals)) = require_stream() else {
        return;
    };
    let base = decode(&nals).unwrap();
    let mut injected = Vec::new();
    for nal in &nals {
        injected.push(vec![62 << 1, 1, 0xde, 0xad, 0xbe, 0xef]);
        injected.push(vec![63 << 1, 1, 0x80]);
        injected.push(nal.clone());
    }
    let with_rpu = decode(&injected).unwrap();
    assert!(!base.is_empty(), "{name}: no frames");
    assert_eq!(base.len(), with_rpu.len(), "{name}: frame count changed");
    for (a, b) in base.iter().zip(&with_rpu) {
        assert_eq!(
            key(a),
            key(b),
            "{name}: a frame changed with type 62/63 NAL units"
        );
    }
}

#[test]
fn malformed_sei_is_skipped_and_the_stream_decodes_the_same() {
    let Some((name, nals)) = require_stream() else {
        return;
    };
    let base = decode(&nals).unwrap();
    let mut injected = Vec::new();
    for nal in &nals {
        // A message that runs past the RBSP, a suffix SEI with a reserved hash type,
        // and one with a short mastering display payload.
        injected.push(nal_unit(39, &[5, 9, 1, 2, 0x80]));
        injected.push(nal_unit(40, &[132, 2, 3, 0, 0x80]));
        let mut short_mdcv = vec![137, 23];
        short_mdcv.extend_from_slice(&[0; 23]);
        short_mdcv.push(0x80);
        injected.push(nal_unit(39, &short_mdcv));
        injected.push(nal.clone());
    }
    let with_bad = decode(&injected).unwrap();
    assert_eq!(base.len(), with_bad.len(), "{name}: frame count changed");
    for (a, b) in base.iter().zip(&with_bad) {
        assert_eq!(key(a), key(b), "{name}: a frame changed with malformed SEI");
        assert!(
            b.mastering_display().is_none(),
            "{name}: a malformed SEI produced metadata"
        );
    }
}

#[test]
fn hdr_sei_applies_to_every_frame_from_the_irap_on() {
    let Some((name, nals)) = require_stream() else {
        return;
    };
    let mut injected = Vec::new();
    let mut placed = false;
    for nal in &nals {
        if !placed && is_vcl(nal) {
            // A prefix SEI in the first access unit, before its first slice.
            injected.push(nal_unit(39, &hdr10_prefix_rbsp()));
            placed = true;
        }
        injected.push(nal.clone());
    }
    let frames = decode(&injected).unwrap();
    assert!(!frames.is_empty(), "{name}: no frames");
    for f in &frames {
        let md: &MasteringDisplay = f.mastering_display().expect("MDCV on every frame");
        assert_eq!(md.display_primaries[0], (8500, 39_850));
        assert_eq!(md.white_point, (15_635, 16_450));
        assert_eq!(md.max_nits(), 1000.0);
        assert!((md.min_nits() - 0.005).abs() < 1e-6);
        let cll: &ContentLightLevel = f.content_light_level().expect("CLL on every frame");
        assert_eq!(
            (cll.max_content_light_level, cll.max_pic_average_light_level),
            (1000, 400)
        );
        assert_eq!(f.alternative_transfer_characteristics(), None);
    }
}

#[test]
fn picture_hash_attaches_to_the_picture_it_follows() {
    let Some((name, nals)) = require_stream() else {
        return;
    };
    // One MD5 plane: payloadType 132, payloadSize 17, hash_type 0, the digest.
    let digest = [0x5au8; 16];
    let mut hash_rbsp = vec![132, 17, 0];
    hash_rbsp.extend_from_slice(&digest);
    hash_rbsp.push(0x80);

    let mut injected = Vec::new();
    let mut placed = false;
    for nal in &nals {
        injected.push(nal.clone());
        // A suffix SEI right after the first slice: it belongs to access unit 0.
        if !placed && is_vcl(nal) {
            injected.push(nal_unit(40, &hash_rbsp));
            placed = true;
        }
    }
    let frames = decode(&injected).unwrap();
    assert!(placed, "{name}: no slice NAL unit");
    let first = frames
        .iter()
        .find(|f| f.au_index == 0)
        .expect("access unit 0 is output");
    match first.picture_hash() {
        Some(PictureHash::Md5(d)) => assert_eq!(d, &[digest]),
        other => panic!("{name}: access unit 0 hash is {other:?}"),
    }
    assert!(
        frames
            .iter()
            .filter(|f| f.au_index != 0)
            .all(|f| f.picture_hash().is_none()),
        "{name}: a hash leaked to another picture"
    );
}
