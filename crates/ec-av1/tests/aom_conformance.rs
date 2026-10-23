//! Conformance against the libaom AV1 test vectors (profile 0, 8 and 10 bit).
//!
//! The vectors are the `av1-1-b8-*` and `av1-1-b10-*` files listed in libaom's
//! `test/test_vectors.cc`, fetched from the `aom-test-data` bucket into the
//! directory named by `EC_AV1_AOM_DIR`. Each `<name>.ivf` (or `.mkv`) comes with a
//! `<name>.md5` file: one line per SHOWN frame, `md5  <name>-WxH-NNNN.i420`.
//! The digest covers the cropped planes (Y, then U, then V; chroma `(w+1)/2` by
//! `(h+1)/2`; Y only for monochrome) and, above 8 bits, each sample is two
//! little-endian bytes (libaom `test/md5_helper.h`, as used by
//! `test/test_vector_test.cc`).
//!
//! The test decodes every vector, digests every shown picture and compares the
//! digest with the matching line, in order. A vector is one of three things:
//!
//! - exact: every shown frame matches and the frame counts agree;
//! - refused by name: the stream is outside DONE.md's scope (profiles 1 and 2,
//!   that is 4:4:4, 4:2:2 and 12-bit) or the decoder names the feature as
//!   unsupported; the reason is printed and it is not a failure;
//! - failed: a digest differs, the frame counts differ, or the decoder errors or
//!   panics without naming an unsupported feature. The first mismatching frame
//!   is listed by vector name.
//!
//! Without `EC_AV1_AOM_DIR` the test uses `fixtures/av1-aom` if present, else skips loudly. With `EC_REQUIRE_FIXTURES`
//! set, a missing directory is a failure.

use ec_av1::stream::decode_stream_with;
use ec_av1_syntax::Av1Parser;
use std::path::PathBuf;

/// Environment variable naming the directory of fetched vectors and md5 files.
const DIR_VAR: &str = "EC_AV1_AOM_DIR";

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

/// libaom `md5_helper.h` digest of one picture's cropped planes. A monochrome
/// picture is digested as libaom's output has it: the luma plane, then two 4:2:0
/// chroma planes of mid-grey (`1 << (bit_depth - 1)`).
fn picture_md5(pic: &ec_av1::encode::Picture, bit_depth: u8, mono: bool) -> String {
    let wide = bit_depth > 8;
    let chroma_len = pic.width.div_ceil(2) * pic.height.div_ceil(2);
    let grey = vec![1u16 << (bit_depth - 1); chroma_len];
    let planes: [&[u16]; 3] = if mono {
        [&pic.y, &grey, &grey]
    } else {
        [&pic.y, &pic.u, &pic.v]
    };
    let mut buf = Vec::with_capacity(planes.iter().map(|p| p.len()).sum::<usize>() * 2);
    for plane in planes {
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
        let end = pos
            .checked_add(sz)
            .filter(|&e| e <= data.len())
            .ok_or("truncated IVF frame")?;
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

/// Video frame payloads of a Matroska file. A flat walk: Segment, Cluster and
/// BlockGroup are entered, SimpleBlock and Block carry one frame each, and
/// everything else is skipped. Laced blocks are refused. The libaom `.mkv`
/// vectors have one video track and no lacing.
fn mkv_frames(data: &[u8]) -> Result<Vec<&[u8]>, String> {
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
                    .ok_or("Matroska block runs past the file")?;
                let (_track, tlen) = ebml_vint(data, body, false)?;
                let header = body + tlen + 3; // 16-bit timecode + flags
                let flags = *data
                    .get(body + tlen + 2)
                    .ok_or("Matroska block without flags")?;
                if (flags >> 1) & 3 != 0 {
                    return Err("laced Matroska block (not produced by libaom)".into());
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
                    .ok_or("Matroska element size overflow")?;
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------- driver ----

/// Vectors the decoder does not yet match, with the cause. The test passes when
/// exactly these diverge; a vector that starts matching must leave this list.
/// Empty: every vector in the set is bit-exact (242/242).
const KNOWN_FAILING: &[(&str, &str)] = &[];

/// What became of one vector.
enum Outcome {
    /// Every shown frame matched and the counts agree.
    Exact { frames: usize },
    /// Outside DONE.md's scope, or the decoder named the feature unsupported.
    Refused(String),
    /// At least one difference; each string names the vector and the frame.
    Failed(Vec<String>),
}

/// Profile and chroma format of a vector, read from its first sequence header.
struct Format {
    profile: u8,
    bit_depth: u8,
    mono: bool,
    subsampling: (u8, u8),
}

fn stream_format(stream: &[u8]) -> Option<Format> {
    let mut parser = Av1Parser::new();
    let mut pos = 0usize;
    while pos < stream.len() && parser.sequence_header().is_none() {
        let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
            break;
        };
        pos += obu.total_size.max(1);
    }
    let seq = parser.sequence_header()?;
    let cc = &seq.color_config;
    Some(Format {
        profile: seq.seq_profile,
        bit_depth: cc.bit_depth,
        mono: cc.mono_chrome,
        subsampling: (cc.subsampling_x, cc.subsampling_y),
    })
}

/// Decode one vector's frames as one OBU stream and compare every shown
/// picture with the matching md5 line.
fn check_vector(name: &str, frames: &[&[u8]], expected: &[String]) -> Outcome {
    // The IVF or Matroska payloads are temporal units of low-overhead OBUs, so
    // their concatenation is the raw OBU stream the decoder takes.
    let stream: Vec<u8> = frames.iter().flat_map(|f| f.iter().copied()).collect();
    let Some(fmt) = stream_format(&stream) else {
        return Outcome::Failed(vec![format!("{name}: no sequence header in the stream")]);
    };
    // DONE.md freezes profiles 1 and 2 (4:4:4, 4:2:2, 12-bit). Refused by name.
    let frozen = if fmt.profile != 0 {
        Some(format!("seq_profile {}", fmt.profile))
    } else if fmt.bit_depth > 10 {
        Some(format!("{}-bit", fmt.bit_depth))
    } else if !fmt.mono && fmt.subsampling != (1, 1) {
        Some(format!("subsampling {}/{}", fmt.subsampling.0, fmt.subsampling.1))
    } else {
        None
    };
    if let Some(what) = frozen {
        return Outcome::Refused(format!(
            "{name}: {what} is Frozen by DONE.md (4:4:4, 4:2:2, 12-bit)"
        ));
    }
    let mut problems = Vec::new();
    let mut shown = 0usize;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_stream_with(&stream, |pic, _decode_idx, is_shown| {
            if !is_shown {
                return Ok(());
            }
            match expected.get(shown) {
                None => problems.push(format!("{name}: extra shown frame {shown}")),
                Some(want) => {
                    let got = picture_md5(pic, fmt.bit_depth, fmt.mono);
                    if &got != want {
                        problems.push(format!(
                            "{name}: first mismatch at frame {shown} ({}x{}): md5 {got} != {want}",
                            pic.width, pic.height
                        ));
                    }
                }
            }
            shown += 1;
            Ok(())
        })
    }));
    match result {
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            problems.push(format!("{name}: decoder panicked after {shown} frames: {msg}"));
        }
        Ok(Err(ec_core::Error::Unsupported { what, why })) => {
            if problems.is_empty() {
                return Outcome::Refused(format!("{name}: unsupported {what}: {why}"));
            }
            problems.push(format!("{name}: refused after {shown} frames: {what}: {why}"));
        }
        Ok(Err(e)) => problems.push(format!("{name}: decode error at frame {shown}: {e}")),
        Ok(Ok(())) => {}
    }
    if problems.is_empty() && shown != expected.len() {
        problems.push(format!(
            "{name}: decoded {shown} shown frames, md5 file lists {}",
            expected.len()
        ));
    }
    if problems.is_empty() {
        Outcome::Exact { frames: shown }
    } else {
        Outcome::Failed(problems)
    }
}

/// `EC_AV1_AOM_DIR`, else the workspace's `fixtures/av1-aom` when it exists.
fn vector_dir() -> Option<PathBuf> {
    std::env::var_os(DIR_VAR).map(PathBuf::from).or_else(|| {
        let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/av1-aom");
        local.is_dir().then_some(local)
    })
}

#[test]
fn aom_vectors_match_per_frame_md5() {
    let require = std::env::var_os("EC_REQUIRE_FIXTURES").is_some();
    let Some(dir) = vector_dir() else {
        assert!(!require, "EC_REQUIRE_FIXTURES=1 but {DIR_VAR} is not set and fixtures/av1-aom is missing");
        eprintln!("SKIP aom_conformance: {DIR_VAR} is not set (libaom av1-1-b8/b10 vectors)");
        return;
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            assert!(!require, "EC_REQUIRE_FIXTURES=1 but {}: {e}", dir.display());
            eprintln!("SKIP aom_conformance: {}: {e}", dir.display());
            return;
        }
    };
    let mut md5_files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md5"))
        .filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            n.starts_with("av1-1-b8-") || n.starts_with("av1-1-b10-")
        })
        .collect();
    md5_files.sort();
    if md5_files.is_empty() {
        assert!(!require, "EC_REQUIRE_FIXTURES=1 but no av1-1-b* .md5 files in {}", dir.display());
        eprintln!("SKIP aom_conformance: no av1-1-b* .md5 files in {}", dir.display());
        return;
    }

    let mut exact = Vec::new();
    let mut refused = Vec::new();
    let mut known_failing = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut frames_total = 0usize;
    for md5_path in &md5_files {
        let name = md5_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .trim_end_matches(".md5")
            .to_string();
        let vec_path = dir.join(&name);
        let data = match std::fs::read(&vec_path) {
            Ok(d) => d,
            Err(e) => {
                failed.push(format!("{name}: cannot read vector: {e}"));
                continue;
            }
        };
        let frames = if name.ends_with(".mkv") {
            mkv_frames(&data)
        } else {
            ivf_frames(&data)
        };
        let frames = match frames {
            Ok(f) => f,
            Err(e) => {
                failed.push(format!("{name}: container: {e}"));
                continue;
            }
        };
        let text = std::fs::read_to_string(md5_path).expect("md5 file is UTF-8");
        let expected: Vec<String> = text
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect();
        // A decoder panic is caught inside check_vector; anything else here is a
        // harness bug and fails the run loudly.
        let known = KNOWN_FAILING.iter().find(|(n, _)| *n == name);
        match (check_vector(&name, &frames, &expected), known) {
            (Outcome::Exact { frames }, None) => {
                frames_total += frames;
                exact.push(name.clone());
            }
            (Outcome::Exact { .. }, Some(_)) => {
                failed.push(format!("{name}: now exact; remove it from KNOWN_FAILING"));
            }
            (Outcome::Refused(why), _) => {
                eprintln!("REFUSED {why}");
                refused.push(name.clone());
            }
            (Outcome::Failed(problems), Some(_)) => {
                eprintln!("KNOWN FAILING {name}: {}", problems[0]);
                known_failing.push(name.clone());
            }
            (Outcome::Failed(problems), None) => {
                eprintln!("FAILED {name}: {}", problems[0]);
                failed.extend(problems);
            }
        }
    }
    let total = md5_files.len();
    eprintln!(
        "aom vectors: {}/{total} exact ({frames_total} shown frames), {} refused by name, \
         {} known failing, {} failed",
        exact.len(),
        refused.len(),
        known_failing.len(),
        failed.len()
    );
    assert!(
        failed.is_empty(),
        "{} problems:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
