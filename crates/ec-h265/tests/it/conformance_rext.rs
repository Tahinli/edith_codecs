//! Checks every output picture of each JCT-VC range extension conformance stream
//! (`scripts/fetch-vectors.sh hevc-rext`) against the hash in its decoded picture hash SEI
//! (D.3.19), or against the whole-output MD5 of the package when the stream has no hash.
//!
//! A stream the decoder refuses must be refused by name: the error text has to be one of
//! `REFUSED`. Every other stream has to decode exactly, picture for picture.

use crate::common::{cropped_bytes, describe, plane_matches, whole_output_digest};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use ec_h265::dec::Decoder;

/// The outcome of one stream, one row of the report.
struct Row {
    name: String,
    accepted: &'static str,
    checked: usize,
    exact: usize,
    detail: String,
    /// Whether any picture was checked against the reference YUV (no hash SEI).
    by_reference: bool,
    /// Wall time of decode and flush, in seconds (0 when the stream was refused early).
    decode_secs: f64,
}

/// The conformance directory: `EC_H265_REXT_DIR`, or the fixtures link.
fn jctvc_root() -> PathBuf {
    match std::env::var("EC_H265_REXT_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/hevc-rext"),
    }
}

/// The conformance streams: the first `.bin` or `.bit` bitstream of each zip's
/// directory under the root, sorted by name. Some zips extract one directory
/// deeper, so two levels are searched.
fn streams() -> Vec<PathBuf> {
    let root = jctvc_root();
    fn bitstreams_in(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() && depth > 0 {
                bitstreams_in(&p, depth - 1, out);
            } else if matches!(p.extension().and_then(|x| x.to_str()), Some("bin" | "bit")) {
                out.push(p);
            }
        }
    }
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = dirs.filter_map(|e| e.ok().map(|e| e.path())).collect();
    dirs.sort();
    dirs.into_iter()
        .filter(|d| d.is_dir())
        .filter(|d| {
            std::env::var("EC_H265_REXT_ONLY")
                .map_or(true, |only| d.to_string_lossy().contains(&only))
        })
        .filter_map(|d| {
            let mut found = Vec::new();
            bitstreams_in(&d, 2, &mut found);
            found.into_iter().next()
        })
        .collect()
}

/// Every 32-digit hex word of the MD5 files beside the bitstream (`*md5*`): one of them is the
/// MD5 of the whole decoded output, the others are the bitstream's own.
fn candidate_digests(path: &Path) -> Vec<[u8; 16]> {
    let mut out = Vec::new();
    let Some(dir) = path.parent() else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if !name.contains("md5") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(e.path()) {
            for word in text.split_whitespace() {
                if let Some(d) = whole_output_digest(word) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// The stream cut at its start codes: each chunk is one NAL unit with its start code.
fn nal_chunks(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = (0..data.len().saturating_sub(2))
        .filter(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
        .collect();
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

/// Decodes one stream and checks every output picture against its MD5 SEI.
fn check_stream(path: &Path, name: &str, ref_dir: Option<&Path>) -> Row {
    let mut row = Row {
        name: name.to_string(),
        accepted: "no",
        checked: 0,
        exact: 0,
        detail: String::new(),
        by_reference: false,
        decode_secs: 0.0,
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            row.detail = format!("read error: {e}");
            return row;
        }
    };
    let reference = ref_dir.and_then(|d| std::fs::read(d.join(format!("{name}.yuv"))).ok());
    // A `<name>_md5.txt` beside the bitstream is the authority for frames without a hash SEI. It
    // takes precedence over the ffmpeg reference YUV, which ffmpeg cannot reproduce for such a
    // stream (VPSSPSPPS_A: ffmpeg outputs 2 of its 6 frames).
    let whole_output = Some(candidate_digests(path)).filter(|d| !d.is_empty());
    let mut whole_output_md5 = ec_h265_syntax::md5::Md5::new();
    let mut whole_output_checked = 0usize;

    let started = std::time::Instant::now();
    let mut dec = Decoder::new();
    let mut out_index = 0usize;
    let mut first: Option<String> = None;
    let mut handle = |frame: ec_h265::dec::Frame| {
        let index = out_index;
        out_index += 1;
        if whole_output.is_some() {
            whole_output_md5.update(&cropped_bytes(&frame.picture, &frame.window));
        }
        let Some(expected) = frame.meta.picture_hash.as_ref() else {
            if whole_output.is_some() {
                whole_output_checked += 1;
                return;
            }
            // No hash SEI: the frame is checked against the reference YUV, when there is one.
            if let Some(reference) = reference.as_deref() {
                let want = cropped_bytes(&frame.picture, &frame.window);
                if let Ok(dir) = std::env::var("EC_H265_DUMP_DIR") {
                    // Diagnostic: append this frame to the stream's dump file.
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(Path::new(&dir).join(format!("{name}.ours.yuv")))
                    {
                        let _ = f.write_all(&want);
                    }
                }
                let start = index * want.len();
                row.checked += 1;
                if reference.get(start..start + want.len()) == Some(&want[..]) {
                    row.exact += 1;
                } else if first.is_none() {
                    let at = (0..3)
                        .find_map(|plane| {
                            let d =
                                describe(Some(reference), index, &frame.picture, frame.poc, plane);
                            (!d.contains("no sample differs")).then_some(d)
                        })
                        .unwrap_or_else(|| format!("reference YUV frame {index} differs"));
                    first = Some(format!("{at} (reference YUV, cropped)"));
                }
                row.by_reference = true;
            }
            return;
        };
        row.checked += 1;
        let pic = &*frame.picture;
        let mut exact = true;
        for (plane, ok) in plane_matches(pic, expected).into_iter().enumerate() {
            if ok {
                continue;
            }
            exact = false;
            if first.is_none() {
                let kind = if frame.meta.intra_only { "I" } else { "P/B" };
                let at = describe(reference.as_deref(), index, pic, frame.poc, plane);
                first = Some(format!("{at} slices={kind}"));
            }
        }
        if exact {
            row.exact += 1;
        }
    };
    // Frames are drained after every NAL unit, so the output of a long stream is not held.
    for unit in nal_chunks(&data) {
        if let Err(e) = dec.decode_annex_b(unit) {
            row.detail = format!("error: {e}");
            return row;
        }
        while let Some(frame) = dec.next_frame() {
            handle(frame);
        }
    }
    if let Err(e) = dec.flush() {
        row.detail = format!("error: {e}");
        return row;
    }
    row.accepted = "yes";
    while let Some(frame) = dec.next_frame() {
        handle(frame);
    }
    row.decode_secs = started.elapsed().as_secs_f64();
    if let Some(want) = whole_output.as_ref().filter(|_| whole_output_checked > 0) {
        let got = whole_output_md5.finish();
        row.checked += whole_output_checked;
        if want.contains(&got) {
            row.exact += whole_output_checked;
        } else {
            let hex: String = got.iter().map(|b| format!("{b:02x}")).collect();
            first.get_or_insert(format!(
                "whole-output MD5 {hex} differs from {name}_md5.txt"
            ));
        }
        row.by_reference = true;
    }
    row.detail = match (first, row.checked) {
        (Some(f), _) => format!("first mismatch {f}"),
        (None, 0) => "no pictures with a hash SEI or reference YUV".to_string(),
        (None, _) if whole_output.is_some() && whole_output_checked > 0 => {
            "ok (whole-output MD5 from the conformance package)".to_string()
        }
        (None, _) if row.by_reference => "ok (against the reference YUV)".to_string(),
        (None, _) => "ok".to_string(),
    };
    row
}

#[test]
fn rext_conformance_pictures_match_the_sei_hash() {
    let root = jctvc_root();
    let streams = streams();
    if streams.is_empty() {
        eprintln!(
            "conformance check skipped: no streams under {}",
            root.display()
        );
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none() && !root.is_dir(),
            "fixture directory {} holds no bitstreams",
            root.display()
        );
        return;
    }
    let ref_dir = std::env::var("EC_H265_REF_YUV_DIR").ok().map(PathBuf::from);

    let mut rows = Vec::new();
    for path in &streams {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        eprintln!("stream {name}");
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            check_stream(path, &name, ref_dir.as_deref())
        }));
        let mut row = match outcome {
            Ok(row) => row,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Row {
                    name: name.clone(),
                    accepted: "panic",
                    checked: 0,
                    exact: 0,
                    detail: format!("panic: {msg}"),
                    by_reference: false,
                    decode_secs: 0.0,
                }
            }
        };
        row.detail = row.detail.replace(['\t', '\n'], " ");
        rows.push(row);
    }

    let mut table =
        String::from("stream\taccepted\tpictures_checked\tpictures_exact\tdetail\tdecode_s\n");
    for r in &rows {
        println!(
            "{} | {} | {} | {} | {} | {:.2}s",
            r.name, r.accepted, r.checked, r.exact, r.detail, r.decode_secs
        );
        table.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{:.2}\n",
            r.name, r.accepted, r.checked, r.exact, r.detail, r.decode_secs
        ));
    }
    let checked: usize = rows.iter().map(|r| r.checked).sum();
    let exact: usize = rows.iter().map(|r| r.exact).sum();
    let accepted = rows.iter().filter(|r| r.accepted == "yes").count();
    println!(
        "total: {checked} pictures checked, {exact} exact, {accepted}/{} streams accepted",
        rows.len()
    );
    let report = std::env::var("EC_H265_REXT_REPORT")
        .ok()
        .filter(|p| !p.is_empty());
    let written = report.map(|path| std::fs::write(&path, &table).map_err(|e| (path, e)));
    if let Some(Err((path, e))) = written {
        eprintln!("could not write report {path}: {e}");
    }

    // Every accepted stream must be fully exact; a refusal must name a tool in REFUSED.
    let failing: Vec<String> = rows
        .iter()
        .filter(|r| r.accepted == "yes" && (r.checked == 0 || r.exact != r.checked))
        .map(|r| format!("{} ({}/{} exact: {})", r.name, r.exact, r.checked, r.detail))
        .collect();
    assert!(
        failing.is_empty(),
        "accepted streams that are not exact:\n{}",
        failing.join("\n")
    );
    let unexpected: Vec<String> = rows
        .iter()
        .filter(|r| r.accepted != "yes")
        .filter(|r| !REFUSED.iter().any(|name| r.detail.contains(name)))
        .map(|r| format!("{} ({}: {})", r.name, r.accepted, r.detail))
        .collect();
    assert!(
        unexpected.is_empty(),
        "streams refused for a reason not in REFUSED:\n{}",
        unexpected.join("\n")
    );
}

/// The refusals a conformance stream may end in: tools this decoder names and does not decode.
const REFUSED: &[&str] = &[
    "extended_precision_processing_flag",
    "cabac_bypass_alignment_enabled_flag",
    "high_precision_offsets_enabled_flag",
    "bit depth 16",
    "bit depth 14",
    "bit depth 13",
];
