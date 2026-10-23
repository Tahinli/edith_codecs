//! Syntax-only sweep of the JCT-VC conformance streams through the CU syntax
//! parser ([`ec_h265::dec::cu_syntax`]). Pixel decode is off; every slice of
//! every picture is parsed to `end_of_slice_segment_flag`. A slice is clean when
//! that flag lands on the last CTB and nothing refuses or desyncs.
//!
//! The per-stream table goes to stdout. With `EC_H265_SYNTAX_TABLE` set, the same
//! table is also written there as TSV.

use ec_core::error::Error;
use ec_h265::dec::{Decoder, NalOutcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The conformance streams: the first `.bin` or `.bit` bitstream of each directory
/// under `fixtures/vectors/hevc-jctvc` (or `EC_H265_JCTVC_DIR`), sorted.
fn streams() -> Vec<PathBuf> {
    let root = match std::env::var("EC_H265_JCTVC_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/hevc-jctvc"),
    };
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
        .filter_map(|d| {
            let mut found = Vec::new();
            bitstreams_in(&d, 2, &mut found);
            found.into_iter().next()
        })
        .collect()
}

/// Split an Annex-B byte stream into NAL units (header included, start codes and
/// trailing zero bytes removed).
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |&next| next - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// Whether a NAL unit is a coded slice segment (types 0 to 9 and 16 to 21).
fn is_slice(nal: &[u8]) -> bool {
    let t = (nal[0] >> 1) & 0x3f;
    t <= 9 || (16..=21).contains(&t)
}

#[derive(Default)]
struct Row {
    slices: usize,
    clean: usize,
    refused: BTreeMap<String, usize>,
    desync: usize,
    desync_first: String,
    other: usize,
    /// Parameter sets refused, by name (a slice then fails with "never received").
    param_refused: BTreeMap<String, usize>,
}

fn sweep(path: &Path) -> Row {
    let data = std::fs::read(path).expect("read bitstream");
    let mut dec = Decoder::new();
    dec.set_pixel_decode(false);
    dec.set_syntax_check(true);
    let mut row = Row::default();
    for nal in nal_units(&data) {
        let slice = is_slice(nal);
        match dec.push_nal(nal) {
            Ok(NalOutcome::Slice) if slice => {
                row.slices += 1;
                row.clean += 1;
            }
            Ok(_) => {}
            Err(err) => {
                if !slice {
                    if let Error::Unsupported { what, .. } = err {
                        *row.param_refused.entry(what).or_default() += 1;
                    }
                    continue;
                }
                row.slices += 1;
                match err {
                    Error::Unsupported { what, .. } => {
                        *row.refused.entry(what).or_default() += 1;
                    }
                    // A slice whose parameter set was refused by name: not a parse failure.
                    Error::Corrupt { context } if context.contains("never received") => {
                        *row.refused
                            .entry("parameter set refused (see above)".to_string())
                            .or_default() += 1;
                    }
                    Error::Corrupt { context } => {
                        row.desync += 1;
                        if row.desync_first.is_empty() {
                            row.desync_first = context;
                        }
                    }
                    other => {
                        row.other += 1;
                        if row.desync_first.is_empty() {
                            row.desync_first = other.to_string();
                        }
                    }
                }
            }
        }
    }
    row
}

#[test]
fn cu_syntax_sweep_over_conformance_streams() {
    let streams = streams();
    if streams.is_empty() {
        eprintln!("cu_syntax: no conformance streams found, skipping");
        return;
    }
    let mut table = String::from("stream\tslices\tclean\trefused\tdesync\tother\tnote\n");
    let (mut total_slices, mut total_clean, mut streams_clean, mut streams_all) = (0, 0, 0, 0);
    for path in &streams {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let row = sweep(path);
        let refused: usize = row.refused.values().sum();
        let refused_note: Vec<String> = row
            .refused
            .iter()
            .map(|(what, n)| format!("{what} x{n}"))
            .collect();
        let mut parts = refused_note;
        parts.extend(
            row.param_refused
                .iter()
                .map(|(what, n)| format!("{what} x{n} (parameter set)")),
        );
        let mut note = parts.join("; ");
        if !row.desync_first.is_empty() {
            if !note.is_empty() {
                note.push_str("; ");
            }
            note.push_str(&row.desync_first);
        }
        table.push_str(&format!(
            "{name}\t{}\t{}\t{refused}\t{}\t{}\t{note}\n",
            row.slices, row.clean, row.desync, row.other
        ));
        total_slices += row.slices;
        total_clean += row.clean;
        streams_all += 1;
        if row.slices > 0 && row.clean == row.slices {
            streams_clean += 1;
        }
    }
    print!("{table}");
    println!(
        "cu_syntax: {streams_clean}/{streams_all} streams parse every slice clean; {total_clean}/{total_slices} slices clean"
    );
    if let Ok(path) = std::env::var("EC_H265_SYNTAX_TABLE") {
        std::fs::write(path, &table).expect("write syntax table");
    }
}

/// The pixel pipeline over the same streams: pixel decode on, so intra slices go
/// through `decode_i_slice` and P/B slices through the placeholder inter path.
/// Prints one line per stream with the first error, if any. Samples are not
/// compared; this checks that the pipeline runs.
#[test]
fn pixel_pipeline_runs_over_conformance_streams() {
    let streams = streams();
    if streams.is_empty() {
        eprintln!("pixel pipeline: no conformance streams found, skipping");
        return;
    }
    let mut ok_streams = 0;
    for path in &streams {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let data = std::fs::read(path).expect("read bitstream");
        let mut dec = Decoder::new();
        let mut first_err = String::new();
        for nal in nal_units(&data) {
            if let Err(err) = dec.push_nal(nal) {
                first_err = match err {
                    Error::Unsupported { what, .. } => format!("refused: {what}"),
                    other => other.to_string(),
                };
                break;
            }
        }
        if first_err.is_empty() && dec.flush().is_ok() {
            ok_streams += 1;
            println!("pixel\t{name}\tok");
        } else {
            println!("pixel\t{name}\t{first_err}");
        }
    }
    println!(
        "pixel pipeline: {ok_streams}/{} streams decode without error",
        streams.len()
    );
}

/// The P/B slice syntax ([`ec_h265::dec::cu_syntax::parse_slice`] with the zero-motion
/// stand-in, no DPB to derive motion from) over every P and B slice segment of every
/// stream. Pixel decode is off so the decoder only keeps parameter sets. Refusals are
/// fine; any other error is a failure.
#[test]
fn inter_placeholder_runs_over_pb_slices() {
    use ec_core::bitio::BitReader;
    use ec_h265_syntax::nal::{NalUnitType, unescape_rbsp};
    use ec_h265_syntax::slice::{SliceHeader, SliceType};
    let streams = streams();
    if streams.is_empty() {
        eprintln!("inter placeholder: no conformance streams found, skipping");
        return;
    }
    let (mut pb_slices, mut decoded, mut refused) = (0usize, 0usize, 0usize);
    for path in &streams {
        let data = std::fs::read(path).expect("read bitstream");
        let mut dec = Decoder::new();
        dec.set_pixel_decode(false);
        for nal in nal_units(&data) {
            let code = (nal[0] >> 1) & 0x3f;
            if is_slice(nal) {
                let rbsp = unescape_rbsp(&nal[2..]);
                let mut r = BitReader::new(&rbsp);
                let _first = r.read_bit();
                if (16..=21).contains(&code) {
                    let _no_output_of_prior_pics = r.read_bit();
                }
                let pps_id = r.read_ue().unwrap_or(u32::MAX);
                if let Some(pps) = dec.pps(pps_id)
                    && let Some(sps) = dec.sps(pps.sps_id)
                    && let Ok((header, pos)) =
                        SliceHeader::parse(&rbsp, sps, pps, NalUnitType::from_code(code))
                    && header.slice_type != SliceType::I
                    && header.first_slice_segment_in_pic
                {
                    pb_slices += 1;
                    let off = (pos.header_bits / 8) as usize;
                    let slice_data = rbsp.get(off..).unwrap_or(&[]);
                    let mut zero = ec_h265::dec::cu_syntax::ZeroMotion;
                    match ec_h265::dec::cu_syntax::parse_slice(
                        sps,
                        pps,
                        &header,
                        slice_data,
                        &mut zero,
                        &mut |_| {},
                    ) {
                        Ok(_) => decoded += 1,
                        Err(Error::Unsupported { .. }) => refused += 1,
                        Err(err) => {
                            panic!("{}: P/B placeholder failed: {err}", path.display())
                        }
                    }
                }
            }
            let _ = dec.push_nal(nal);
        }
    }
    println!(
        "inter syntax: {decoded} P/B slices parsed, {refused} refused by name, of {pb_slices} first-slice P/B segments"
    );
}

/// The motion derivation (merge, AMVP, temporal) runs through the syntax-only decode
/// of every stream. Two decodes of the same stream must give the same motion field
/// hash, and the derivation must not fail on any slice that parses cleanly.
#[test]
fn motion_derivation_is_deterministic_over_conformance_streams() {
    let streams = streams();
    if streams.is_empty() {
        eprintln!("motion determinism: no conformance streams found, skipping");
        return;
    }
    fn run(path: &Path) -> (u64, usize) {
        let data = std::fs::read(path).expect("read bitstream");
        let mut dec = Decoder::new();
        dec.set_pixel_decode(false);
        dec.set_syntax_check(true);
        let mut failures = 0usize;
        for nal in nal_units(&data) {
            match dec.push_nal(nal) {
                Ok(_) | Err(Error::Unsupported { .. }) => {}
                // A slice whose parameter set was refused by name (see the sweep).
                Err(Error::Corrupt { context }) if context.contains("never received") => {}
                Err(_) => failures += 1,
            }
        }
        let _ = dec.flush();
        (dec.motion_checksum(), failures)
    }
    let (mut streams_run, mut total_failures) = (0usize, 0usize);
    for path in &streams {
        let (first, failures) = run(path);
        let (second, _) = run(path);
        assert_eq!(
            first,
            second,
            "{}: motion field differs between runs",
            path.display()
        );
        total_failures += failures;
        streams_run += 1;
    }
    println!("motion determinism: {streams_run} streams, {total_failures} slice failures");
    assert_eq!(total_failures, 0, "slices failed while deriving motion");
}
