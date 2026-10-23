//! CU syntax tests over the JCT-VC conformance streams: the P/B slice syntax with the
//! zero-motion stand-in (the sweep, pixel-pipeline and motion-determinism tests were removed by
//! testcut Phase 2).

use ec_core::error::Error;
use ec_h265::dec::Decoder;
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
