//! Conformance against the libvpx VP9 test vectors (profile 0 and profile 2).
//!
//! Every vector under `fixtures/vectors/vp9-libvpx/` comes with a `<name>.md5`
//! file from libvpx's test data: one line per SHOWN frame, `md5  <name>-WxH-NNNN.i420`.
//! The md5 covers the cropped planes (Y, then U, then V; chroma `(w+1)/2`
//! by `(h+1)/2`) and, for depths above 8, each sample is two little-endian
//! bytes (libvpx `test/md5_helper.h`).
//!
//! The test decodes every vector, digests every returned picture and compares
//! the digest with the matching line, in order. Any frame-count or digest
//! difference fails the test and is listed by vector name and frame index.
//! Vectors are read from the fixture tree (`scripts/fetch-vectors.sh
//! vp9-libvpx`). Without the fixtures the test skips, unless
//! `EC_REQUIRE_FIXTURES` is set, in which case it fails.

use ec_vp9::decode::{Decoder, Picture};
use std::path::{Path, PathBuf};

/// Directory of the fetched libvpx vectors (crates/ec-vp9/../../fixtures).
fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/vp9-libvpx")
}

// ------------------------------------------------------------------ MD5 ----

/// RFC 1321 MD5 of `data`, as lowercase hex.
fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
        5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for block in msg.as_chunks::<64>().0 {
        let m: Vec<u32> = block
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// libvpx `md5_helper.h` digest of one picture's cropped planes.
fn picture_md5(pic: &Picture) -> String {
    let wide = pic.bit_depth > 8;
    let mut buf = Vec::with_capacity((pic.y.len() + pic.u.len() + pic.v.len()) * 2);
    for plane in [&pic.y, &pic.u, &pic.v] {
        for &s in plane.iter() {
            if wide {
                buf.extend_from_slice(&s.to_le_bytes());
            } else {
                buf.push(s as u8);
            }
        }
    }
    md5_hex(&buf)
}

// --------------------------------------------------------- containers ----

/// Frame payloads of an IVF file (DKIF, 32-byte file header).
fn ivf_frames(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    if data.len() < 32 || &data[0..4] != b"DKIF" {
        return Err("not an IVF file".into());
    }
    let hdr_len = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = hdr_len;
    let mut out = Vec::new();
    while pos + 12 <= data.len() {
        let sz = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        let end = pos.checked_add(sz).filter(|&e| e <= data.len()).ok_or("truncated IVF frame")?;
        out.push(&data[pos..end]);
        pos = end;
    }
    Ok(out)
}

/// Read an EBML variable-length integer at `pos`. Returns `(value, length)`;
/// `keep_marker` keeps the length marker bit (element IDs) and clears it for
/// sizes. An all-ones size (unknown) comes back as `u64::MAX`.
fn ebml_vint(data: &[u8], pos: usize, keep_marker: bool) -> Result<(u64, usize), String> {
    let first = *data.get(pos).ok_or("EBML truncated")?;
    if first == 0 {
        return Err("EBML vint with no marker".into());
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = data.get(pos..pos + len).ok_or("EBML truncated")?;
    let mut v = if keep_marker {
        bytes[0] as u64
    } else {
        (bytes[0] as u64) & ((1u64 << (8 - len)) - 1)
    };
    for &b in &bytes[1..] {
        v = (v << 8) | b as u64;
    }
    if !keep_marker && v == (1u64 << (7 * len)) - 1 {
        return Ok((u64::MAX, len));
    }
    Ok((v, len))
}

/// VP9 frame payloads of the first video track of a WebM (Matroska) file.
///
/// A flat walk: Segment, Cluster and BlockGroup are entered (their children
/// follow in the byte stream, which also covers unknown-size clusters),
/// SimpleBlock and Block carry one frame each, and everything else is
/// skipped. Laced blocks are refused; libvpx's vectors are not laced.
fn webm_frames(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    const SEGMENT: u64 = 0x1853_8067;
    const CLUSTER: u64 = 0x1F43_B675;
    const BLOCK_GROUP: u64 = 0xA0;
    const BLOCK: u64 = 0xA1;
    const SIMPLE_BLOCK: u64 = 0xA3;

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let (id, id_len) = ebml_vint(data, pos, true)?;
        let (size, size_len) = ebml_vint(data, pos + id_len, false)?;
        let body = pos + id_len + size_len;
        match id {
            SEGMENT | CLUSTER | BLOCK_GROUP => {
                pos = body;
                continue;
            }
            BLOCK | SIMPLE_BLOCK => {
                let end = body
                    .checked_add(size as usize)
                    .filter(|&e| e <= data.len())
                    .ok_or("WebM block runs past the file")?;
                let (_track, tlen) = ebml_vint(data, body, false)?;
                let header = body + tlen + 3; // 16-bit timecode + flags
                let flags = *data.get(body + tlen + 2).ok_or("WebM block without flags")?;
                if (flags >> 1) & 3 != 0 {
                    return Err("laced WebM block (not produced by libvpx)".into());
                }
                out.push(&data[header.min(end)..end]);
                pos = end;
            }
            _ => {
                if size == u64::MAX {
                    return Err("unknown-size non-master element".into());
                }
                pos = body
                    .checked_add(size as usize)
                    .ok_or("WebM element size overflow")?;
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------- driver ----

/// Profile of a libvpx vector from its name: `vp9<P>-2-...`.
fn vector_profile(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("vp9")?;
    let d = rest.as_bytes().first()?;
    d.is_ascii_digit().then(|| d - b'0')
}

/// Profile 0 and 2 vectors the decoder does not yet match, with the first
/// divergence observed. The test passes when exactly these (and no others)
/// diverge; a vector that starts passing must be removed from the list.
const KNOWN_FAILING: &[(&str, &str)] = &[];

/// Decode one vector and return the mismatches, one string per problem.
fn check_vector(name: &str, frames: &[&[u8]], expected: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut dec = Decoder::new();
    let mut shown = 0usize;
    for (i, frame) in frames.iter().enumerate() {
        let pic = match dec.decode(frame) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                problems.push(format!("{name}: chunk {i} refused: {e}"));
                break;
            }
        };
        match expected.get(shown) {
            None => problems.push(format!("{name}: extra output frame {shown}")),
            Some(want) => {
                if let Some(d) = std::env::var_os("EC_VP9_DUMP") {
                    let mut raw = Vec::new();
                    for plane in [&pic.y, &pic.u, &pic.v] {
                        for &s in plane.iter() {
                            if pic.bit_depth > 8 {
                                raw.extend_from_slice(&s.to_le_bytes());
                            } else {
                                raw.push(s as u8);
                            }
                        }
                    }
                    let _ = std::fs::write(Path::new(&d).join(format!("{name}.{shown}.yuv")), raw);
                }
                let got = picture_md5(&pic);
                if &got != want {
                    problems.push(format!("{name}: frame {shown} md5 {got} != {want}"));
                }
            }
        }
        shown += 1;
    }
    if shown != expected.len() && problems.is_empty() {
        problems.push(format!(
            "{name}: decoded {shown} frames, md5 file lists {}",
            expected.len()
        ));
    }
    problems
}

#[test]
fn libvpx_vectors_match_per_frame_md5() {
    let dir = vector_dir();
    let require = std::env::var_os("EC_REQUIRE_FIXTURES").is_some();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => {
            assert!(
                !require,
                "EC_REQUIRE_FIXTURES=1 but {} is absent (run scripts/fetch-vectors.sh vp9-libvpx)",
                dir.display()
            );
            eprintln!("skip: {} absent", dir.display());
            return;
        }
    };
    let mut md5_files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md5"))
        .collect();
    md5_files.sort();
    assert!(!md5_files.is_empty(), "no .md5 files under {}", dir.display());

    let mut all_problems = Vec::new();
    let mut skipped = Vec::new();
    let mut known_hit = Vec::new();
    let mut vectors = 0usize;
    let mut frames_total = 0usize;
    for md5_path in &md5_files {
        let name = md5_path.file_name().unwrap().to_string_lossy().trim_end_matches(".md5").to_string();
        // DONE.md scopes bit-exactness to profile 0 (8-bit 4:2:0) and profile 2
        // (10-bit 4:2:0). Profiles 1 and 3 (4:4:4, 4:2:2, 4:4:0) are Frozen.
        match vector_profile(&name) {
            Some(0) | Some(2) => {}
            other => {
                skipped.push(format!("{name}: profile {other:?} is Frozen (profile 0/2 only)"));
                continue;
            }
        }
        if let Ok(only) = std::env::var("EC_VP9_ONLY")
            && !name.contains(&only)
        {
            continue;
        }
        let vec_path = dir.join(&name);
        let data = match std::fs::read(&vec_path) {
            Ok(d) => d,
            Err(e) => {
                all_problems.push(format!("{name}: cannot read vector: {e}"));
                continue;
            }
        };
        let frames = if name.ends_with(".webm") {
            webm_frames(&data)
        } else {
            ivf_frames(&data)
        };
        let frames = match frames {
            Ok(f) => f,
            Err(e) => {
                all_problems.push(format!("{name}: container: {e}"));
                continue;
            }
        };
        let text = std::fs::read_to_string(md5_path).expect("md5 file is UTF-8");
        let expected: Vec<String> = text
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect();
        frames_total += expected.len();
        vectors += 1;
        // A decoder panic is a failure of this vector, not of the whole run.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_vector(&name, &frames, &expected)
        }));
        let problems = match outcome {
            Ok(p) => p,
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                vec![format!("{name}: decoder panicked: {msg}")]
            }
        };
        eprintln!(
            "vector {name}: {} shown, {} problems{}",
            expected.len(),
            problems.len(),
            problems.first().map(|p| format!(" first: {p}")).unwrap_or_default()
        );
        if KNOWN_FAILING.iter().any(|(n, _)| *n == name) {
            if problems.is_empty() {
                all_problems.push(format!("{name}: now passes; remove it from KNOWN_FAILING"));
            } else {
                known_hit.push(name.clone());
            }
        } else {
            all_problems.extend(problems);
        }
    }
    for s in &skipped {
        eprintln!("skipped {s}");
    }
    eprintln!(
        "libvpx vectors: {} skipped (Frozen profiles), {} known failing still failing",
        skipped.len(),
        known_hit.len()
    );
    eprintln!(
        "libvpx vectors: {vectors} checked, {frames_total} shown frames, {} problems",
        all_problems.len()
    );
    assert!(
        all_problems.is_empty(),
        "{} problems:\n{}",
        all_problems.len(),
        all_problems.join("\n")
    );
}

#[test]
fn md5_known_answers() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
        "57edf4a22be3c955ac49da2e2107b67a"
    );
}
