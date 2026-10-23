//! Interlaced H.264 (frame_mbs_only_flag 0): field pictures (PAFF) and
//! macroblock-adaptive frame/field coding (MBAFF), woven into frames.
//!
//! The authority is the JVT conformance suite: each vector's reference YUV is
//! compared frame by frame. Fixtures live in the gitignored root `fixtures/`
//! (`scripts/fetch-vectors.sh`); a missing one skips with a message, or fails
//! when `EC_REQUIRE_FIXTURES` is set, like the other conformance tests.

use std::path::{Path, PathBuf};

use ec_core::frame::{Interlace, VideoFrame};
use ec_h264::{Decoder, NalOutcome};

/// Where a vector lives: the interlaced set (`scripts/fetch-vectors.sh`), or
/// the general JVT set for the few interlaced streams that were fetched first.
fn vector_dir(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors");
    let own = root.join("h264-jvt-interlaced").join(name);
    if own.exists() {
        own
    } else {
        root.join("h264-jvt").join(name)
    }
}

/// True when `path` exists; otherwise skip (or fail under `EC_REQUIRE_FIXTURES`).
fn have_fixture(path: &Path, what: &str) -> bool {
    if path.exists() {
        return true;
    }
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "{what} missing at {} - this gate would prove nothing; run \
         scripts/link-fixtures.sh or scripts/fetch-vectors.sh",
        path.display()
    );
    eprintln!("SKIP {what} missing at {}", path.display());
    false
}

fn find_with_ext(dir: &Path, exts: &[&str]) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.contains(&x.to_ascii_lowercase().as_str()))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

fn frame_bytes(frame: &VideoFrame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let dims = [
        (w, h),
        (w.div_ceil(2), h.div_ceil(2)),
        (w.div_ceil(2), h.div_ceil(2)),
    ];
    let mut out = Vec::new();
    for (plane, (pw, ph)) in frame.planes.iter().zip(dims) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw).expect("plane row"));
        }
    }
    out
}

/// Every output frame (display order) with its interlace flag.
fn decode_all(bytes: &[u8]) -> Result<Vec<(Vec<u8>, Interlace)>, ec_h264::Error> {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            frames.push((frame_bytes(&f), f.interlace));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        frames.push((frame_bytes(&f), f.interlace));
    }
    Ok(frames)
}

/// Decode one JVT vector and compare it with its reference YUV.
fn check_vector(name: &str) -> Result<usize, String> {
    let dir = vector_dir(name);
    if !have_fixture(&dir, &format!("JVT vector {name}")) {
        return Ok(0);
    }
    let stream = find_with_ext(&dir, &["jsv", "264", "avc", "26l", "h264", "jvt"])
        .ok_or(format!("{name}: no bitstream"))?;
    let yuv = find_with_ext(&dir, &["yuv"]).ok_or(format!("{name}: no reference YUV"))?;
    let frames =
        decode_all(&std::fs::read(&stream).unwrap()).map_err(|e| format!("{name}: {e}"))?;
    let reference = std::fs::read(&yuv).unwrap();
    let len = frames.first().ok_or(format!("{name}: no frames"))?.0.len();
    if frames.len() * len != reference.len() {
        return Err(format!(
            "{name}: {} frames decoded, reference has {}",
            frames.len(),
            reference.len() / len
        ));
    }
    for (i, (f, il)) in frames.iter().enumerate() {
        let want = &reference[i * len..(i + 1) * len];
        if let Some(pos) = f.iter().zip(want).position(|(a, b)| a != b) {
            return Err(format!("{name}: frame {i} differs at byte {pos}"));
        }
        if !il.is_interlaced() {
            return Err(format!("{name}: frame {i} is not flagged interlaced"));
        }
    }
    Ok(frames.len())
}

fn sweep(names: &[&str]) {
    let mut failed = Vec::new();
    for name in names {
        match check_vector(name) {
            Ok(n) => eprintln!("exact    {name} ({n} frames)"),
            Err(e) => {
                eprintln!("MISMATCH {e}");
                failed.push(e);
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// The three interlaced groups, each formerly its own `#[test]`:
///
/// * Field pictures: I/P/B fields, field pairs and lone fields, frame and field
///   pictures mixed (PAFF), CAVLC and CABAC, field reference marking with MMCO.
/// * High profile field pictures: 8x8 transform with the field scans, CABAC,
///   8x8 intra prediction in field pictures.
/// * MBAFF frames: macroblock pairs with a per-pair field flag, CAVLC and CABAC,
///   I/P/B slices, spatial and temporal direct, implicit and explicit weights,
///   constrained intra prediction, 8x8 transform with scaling lists, and PAFF
///   pictures mixed with MBAFF frames. Every JVT MBAFF vector is bit-exact.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "jvt_paff_vectors_bit_exact",
        &[
            "CVFI1_Sony_D",
            "CVFI1_SVA_C",
            "CVFI2_SVA_C",
            "CVNLFI1_Sony_C",
            "CVNLFI2_Sony_H",
            "FI1_Sony_E",
            "CAFI1_SVA_C",
            "Sharp_MP_Field_1_B",
            "Sharp_MP_Field_3_B",
            "Sharp_MP_PAFF_1r2",
            "CVPA1_TOSHIBA_B",
            "CABREF3_Sand_D",
            "MR6_BT_B",
            "MR7_BT_B",
            "MR8_BT_B",
        ],
    ),
    ("jvt_frext_field_vectors_bit_exact", &["HCAFF1_HHI_B"]),
    (
        "jvt_mbaff_vectors_bit_exact",
        &[
            "CVMA1_Sony_D",
            "CVMA1_TOSHIBA_B",
            "CVMANL1_TOSHIBA_B",
            "CVMANL2_TOSHIBA_B",
            "CVMAQP2_Sony_G",
            "CVMAQP3_Sony_D",
            "CVMAPAQP3_Sony_E",
            "CAMA1_Sony_C",
            "CAMA1_TOSHIBA_B",
            "CAMANL1_TOSHIBA_B",
            "CAMANL2_TOSHIBA_B",
            "CAMANL3_Sand_E",
            "CAMA3_Sand_E",
            "CAMACI3_Sony_C",
            "CAMASL3_Sony_B",
            "cama1_vtc_c",
            "cama2_vtc_b",
            "cama3_vtc_b",
            "CAMP_MOT_MBAFF_L30",
            "CAMP_MOT_MBAFF_L31",
            "CAPAMA3_Sand_F",
            "FRExt2_Panasonic_C",
            "FRExt4_Panasonic_B",
            "MR9_BT_B",
        ],
    ),
];

/// Every group in [`GROUPS`] is bit-exact. Each group runs under
/// `catch_unwind`, so one failing group does not hide the others.
#[test]
fn jvt_interlaced_vectors_bit_exact() {
    let mut failures = Vec::new();
    for (name, vectors) in GROUPS {
        let run = std::panic::AssertUnwindSafe(|| sweep(vectors));
        if std::panic::catch_unwind(run).is_err() {
            failures.push(*name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}
