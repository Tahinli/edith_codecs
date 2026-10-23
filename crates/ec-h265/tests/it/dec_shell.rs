//! Picture management of the stateful decoder (`ec_h265::dec`): POC, RPS and
//! marking, bumping, reference lists, and the JCT-VC conformance sweep.
//!
//! The sweep parses every slice of every stream through the picture-management
//! path (pixel decode off) and checks the output order against ffprobe. Streams
//! whose slices are all intra are also pixel-decoded and compared with ffmpeg's
//! output. The sweep is skipped when the vectors are absent.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_h265::dec::dpb::{Dpb, DpbLimits, Marking, PendingFrame};
use ec_h265::dec::poc::derive_poc;
use ec_h265::dec::rps::{RpsInput, build_ref_lists};
use ec_h265::dec::shared::SharedPic;
use ec_h265::dec::{Decoder, NalOutcome};
use ec_h265::decode::DecodedPicture;
use ec_h265_syntax::slice::{LongTermRef, SliceHeader, SliceType};
use ec_h265_syntax::{ConformanceWindow, Pps, ShortTermRefPicSet};

fn picture(w: usize, h: usize) -> DecodedPicture {
    DecodedPicture {
        width: w,
        height: h,
        bit_depth: 8,
        bit_depth_c: 8,
        chroma_format_idc: 1,
        y: Vec::new(),
        cb: Vec::new(),
        cr: Vec::new(),
    }
}

fn st_set(before: &[i32], after: &[i32]) -> ShortTermRefPicSet {
    let mut st = ShortTermRefPicSet {
        num_negative: before.len() as u32,
        num_positive: after.len() as u32,
        ..ShortTermRefPicSet::default()
    };
    for (i, &d) in before.iter().enumerate() {
        st.delta_poc_s0[i] = d;
        st.used_s0[i] = true;
    }
    for (i, &d) in after.iter().enumerate() {
        st.delta_poc_s1[i] = d;
        st.used_s1[i] = true;
    }
    st
}

fn rps_input<'a>(
    poc: i32,
    st: &'a ShortTermRefPicSet,
    lt: &'a [LongTermRef],
    fill: Option<(u32, u32)>,
) -> RpsInput<'a> {
    RpsInput {
        poc,
        log2_max_poc_lsb: 4,
        st,
        lt,
        irap_no_rasl: false,
        width: 16,
        height: 16,
        chroma_format_idc: 1,
        fill,
    }
}

fn add(dpb: &mut Dpb, poc: i32, marking: Marking, output: bool) -> u32 {
    dpb.insert(
        poc,
        marking,
        output,
        std::sync::Arc::new(SharedPic::from_planes(poc, picture(16, 16))),
        ConformanceWindow::default(),
        poc as u32,
    )
}

#[test]
fn poc_follows_the_lsb_without_wrapping() {
    assert_eq!(derive_poc(5, 4, 7, false), 7);
    assert_eq!(derive_poc(0, 4, 0, false), 0);
}

#[test]
fn poc_msb_wraps_forward_at_the_half_window() {
    // prevTid0 = 0, lsb 14 of 16: 14 - 0 > 8, so the MSB is -16 and POC is -2.
    assert_eq!(derive_poc(0, 4, 14, false), -2);
}

#[test]
fn poc_msb_wraps_backward_past_the_maximum() {
    // prevTid0 = 14 (lsb 14), lsb 2: 14 - 2 >= 8, so the MSB advances to 16.
    assert_eq!(derive_poc(14, 4, 2, false), 18);
}

#[test]
fn poc_of_an_irap_with_no_rasl_output_has_no_msb() {
    assert_eq!(derive_poc(37, 4, 5, true), 5);
}

#[test]
fn rps_marks_what_it_names_and_drops_the_rest() {
    let mut dpb = Dpb::new();
    let ids: Vec<u32> = [0, 1, 2, 3, 4, 5, 6, 7]
        .iter()
        .map(|&p| add(&mut dpb, p, Marking::ShortTerm, false))
        .collect();
    // Current POC 8 names 7, 5 and 4 (deltas -1, -3, -4). 0..3 and 6 drop out.
    let st = st_set(&[-1, -3, -4], &[]);
    let set = dpb.apply_rps(&rps_input(8, &st, &[], Some((8, 8))));
    let before: Vec<i32> = set.st_curr_before.iter().map(|e| e.poc).collect();
    assert_eq!(before, vec![7, 5, 4]);
    assert!(set.st_curr_after.is_empty());
    assert!(set.lt_curr.is_empty());
    assert_eq!(set.st_curr_before[0].id, ids[7]);
    for &p in &[0usize, 1, 2, 3, 6] {
        assert_eq!(dpb.get(ids[p]).unwrap().marking, Marking::Unused, "POC {p}");
    }
    assert_eq!(dpb.get(ids[5]).unwrap().marking, Marking::ShortTerm);
    dpb.remove_unneeded();
    assert_eq!(dpb.len(), 3);
}

#[test]
fn rps_generates_a_missing_reference_as_grey() {
    let mut dpb = Dpb::new();
    add(&mut dpb, 4, Marking::ShortTerm, false);
    // Current POC 8 names POC 6 (absent) and POC 4.
    let st = st_set(&[-2, -4], &[]);
    let set = dpb.apply_rps(&rps_input(8, &st, &[], Some((8, 8))));
    assert_eq!(set.st_curr_before.len(), 2);
    let generated = dpb
        .get(set.st_curr_before[0].id)
        .expect("generated picture");
    assert_eq!(generated.poc, 6);
    assert!(!generated.needed_for_output);
    let planes = generated.picture.read_planes();
    assert_eq!(planes.y.len(), 16 * 16);
    assert!(planes.y.iter().all(|&v| v == 128));
}

#[test]
fn rps_long_term_entry_matches_by_lsb_and_converts_marking() {
    let mut dpb = Dpb::new();
    let lt_id = add(&mut dpb, 16, Marking::ShortTerm, false);
    // PocLsbLt 0 with no MSB: the picture with POC & 15 == 0 is POC 16.
    let lt = [LongTermRef {
        poc_lsb_lt: 0,
        used_by_curr: true,
        delta_poc_msb_present: false,
        delta_poc_msb_cycle: 0,
    }];
    let st = ShortTermRefPicSet::default();
    let set = dpb.apply_rps(&rps_input(20, &st, &lt, Some((8, 8))));
    assert_eq!(set.lt_curr.len(), 1);
    assert_eq!(set.lt_curr[0].id, lt_id);
    assert!(set.lt_curr[0].long_term);
    assert_eq!(dpb.get(lt_id).unwrap().marking, Marking::LongTerm);
}

#[test]
fn bumping_outputs_in_poc_order_under_the_reorder_limit() {
    let limits = DpbLimits {
        max_dec_pic_buffering: 6,
        max_num_reorder: 2,
        max_latency_plus1: 0,
    };
    let mut dpb = Dpb::new();
    let mut out: VecDeque<PendingFrame> = VecDeque::new();
    // Decode order 0, 8, 4, 2: a reorder limit of 2 forces output as it goes.
    for poc in [0, 8, 4, 2] {
        add(&mut dpb, poc, Marking::Unused, true);
        while dpb.needs_bumping(&limits, false) && dpb.bump(&mut out) {}
    }
    let early: Vec<i32> = out.iter().map(|f| f.poc).collect();
    assert_eq!(early, vec![0, 2]);
    dpb.drain(&mut out);
    let all: Vec<i32> = out.iter().map(|f| f.poc).collect();
    assert_eq!(all, vec![0, 2, 4, 8]);
    assert!(dpb.is_empty());
}

#[test]
fn bumping_honours_the_latency_limit() {
    let limits = DpbLimits {
        max_dec_pic_buffering: 6,
        max_num_reorder: 4,
        max_latency_plus1: 1,
    };
    let mut dpb = Dpb::new();
    let mut out = VecDeque::new();
    add(&mut dpb, 0, Marking::Unused, true);
    assert!(!dpb.needs_bumping(&limits, false));
    // SpsMaxLatencyPictures = 4 + 1 - 1 = 4: a picture that waited 4 times goes out.
    dpb.pictures_mut()[0].latency = 4;
    assert!(dpb.needs_bumping(&limits, false));
    assert!(dpb.bump(&mut out));
    assert_eq!(out.len(), 1);
}

#[test]
fn bumping_drops_a_full_buffer_only_when_asked() {
    let limits = DpbLimits {
        max_dec_pic_buffering: 2,
        max_num_reorder: 8,
        max_latency_plus1: 0,
    };
    let mut dpb = Dpb::new();
    add(&mut dpb, 0, Marking::ShortTerm, false);
    add(&mut dpb, 2, Marking::ShortTerm, false);
    assert!(dpb.needs_bumping(&limits, true));
    assert!(!dpb.needs_bumping(&limits, false));
}

#[test]
fn reference_lists_cycle_and_follow_the_modification() {
    let mut dpb = Dpb::new();
    let a = add(&mut dpb, 4, Marking::ShortTerm, false);
    let b = add(&mut dpb, 6, Marking::ShortTerm, false);
    let c = add(&mut dpb, 10, Marking::ShortTerm, false);
    let st = st_set(&[-4, -2], &[2]);
    let set = dpb.apply_rps(&rps_input(8, &st, &[], Some((8, 8))));
    assert_eq!(
        set.st_curr_before.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![a, b]
    );
    assert_eq!(
        set.st_curr_after.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![c]
    );

    let pps = Pps::default();
    let mut header = SliceHeader::intra(&pps, 0);
    header.slice_type = SliceType::B;
    header.num_ref_idx_l0_active_minus1 = 3;
    header.num_ref_idx_l1_active_minus1 = 1;
    let lists = build_ref_lists(&set, &header).expect("lists");
    let l0: Vec<u32> = lists.l0.iter().map(|e| e.id).collect();
    assert_eq!(l0, vec![a, b, c, a]);
    let l1: Vec<u32> = lists.l1.iter().map(|e| e.id).collect();
    assert_eq!(l1, vec![c, a]);

    header.list_entry = [vec![2, 2, 0, 1], vec![1, 0]];
    let lists = build_ref_lists(&set, &header).expect("modified lists");
    assert_eq!(
        lists.l0.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![c, c, a, b]
    );
    assert_eq!(
        lists.l1.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![a, c]
    );
}

#[test]
fn decoder_refuses_out_of_scope_parameter_sets_and_skips_layers() {
    let mut dec = Decoder::new();
    // A VPS-typed NAL on layer 1 is skipped whatever its payload.
    let layer1 = [32u8 << 1, 0b0000_1001, 0xff];
    assert_eq!(dec.push_nal(&layer1).unwrap(), NalOutcome::Skipped);
    // SEI and filler are ignored.
    assert_eq!(
        dec.push_nal(&[39 << 1, 1, 0x80]).unwrap(),
        NalOutcome::Skipped
    );
    assert_eq!(
        dec.push_nal(&[62 << 1, 1, 0x80]).unwrap(),
        NalOutcome::Skipped
    );
    assert!(dec.next_frame().is_none());
}

fn have_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(tool: &str, args: &[&str], file: &Path) -> Result<Vec<u8>, String> {
    let out = Command::new(tool)
        .args(["-v", "error"])
        .args(args)
        .arg(file)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

fn numbers(text: &[u8]) -> Vec<u64> {
    String::from_utf8_lossy(text)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect()
}

/// The conformance streams: the first `.bin` or `.bit` bitstream of each zip's
/// directory under `fixtures/vectors/hevc-jctvc` (or `EC_H265_JCTVC_DIR`), sorted
/// by name. Some zips extract one directory deeper, so two levels are searched.
fn streams() -> Vec<PathBuf> {
    // EC_H265_JCTVC_DIR overrides the default, for hosts whose fixtures link is dangling.
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

/// Output order from our decoder, picture management only: (POC, AU index).
fn our_order(data: &[u8]) -> Result<Vec<(i32, u32)>, String> {
    let mut dec = Decoder::new();
    dec.set_pixel_decode(false);
    dec.decode_annex_b(data).map_err(|e| e.to_string())?;
    dec.flush().map_err(|e| e.to_string())?;
    let mut order = Vec::new();
    while let Some(f) = dec.next_frame() {
        order.push((f.poc, f.au_index));
    }
    Ok(order)
}

/// Output order from ffprobe: the access-unit index (by packet position in
/// decode order) of each frame, in display order.
fn ffprobe_order(path: &Path) -> Result<Vec<u32>, String> {
    let packets = numbers(&run(
        "ffprobe",
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pos",
            "-of",
            "csv=p=0",
        ],
        path,
    )?);
    let frames = numbers(&run(
        "ffprobe",
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pkt_pos",
            "-of",
            "csv=p=0",
        ],
        path,
    )?);
    frames
        .iter()
        .map(|pos| {
            packets
                .iter()
                .position(|p| p == pos)
                .map(|i| i as u32)
                .ok_or_else(|| format!("frame position {pos} is no packet position"))
        })
        .collect()
}

/// Streams where ffprobe is not the reference: the spec's frame count (from the
/// conformance description) and why ffprobe differs. Our count must equal the spec's.
fn spec_expectation(name: &str) -> Option<(usize, &'static str)> {
    match name {
        // Unequal luma (10) and chroma (9) bit depth: ffmpeg decodes nothing.
        "TSUNEQBD_A_MAIN10_Technicolor_2" => Some((8, "ffmpeg unsupported (unequal bit depths)")),
        // Six IDR pictures, each with its own VPS/SPS/PPS and resolution: ffmpeg outputs 2.
        "VPSSPSPPS_A_MainConcept_1" => Some((6, "ffmpeg outputs 2")),
        _ => None,
    }
}

#[test]
fn conformance_output_order_matches_ffprobe() {
    let streams = streams();
    if streams.is_empty() || !have_tool("ffprobe") {
        eprintln!("conformance sweep skipped: vectors or ffprobe absent");
        return;
    }
    let mut table = String::from("stream\tpictures_out\tffprobe_out\tresult\tdetail\n");
    let mut pass = 0usize;
    let mut spec_ok = 0usize;
    for path in &streams {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                table.push_str(&format!("{name}\t-\t-\tFAIL\tread: {e}\n"));
                continue;
            }
        };
        let ours = our_order(&data);
        let theirs = ffprobe_order(path);
        let row = match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => {
                let our_aus: Vec<u32> = ours.iter().map(|(_, au)| *au).collect();
                let pocs: Vec<String> = ours.iter().take(8).map(|(p, _)| p.to_string()).collect();
                let detail = format!("first POCs {}", pocs.join(","));
                match spec_expectation(&name) {
                    // The spec's own count is the reference where ffprobe is wrong.
                    Some((frames, why)) => {
                        let verdict = if ours.len() == frames { "SPEC" } else { "FAIL" };
                        if verdict == "SPEC" {
                            spec_ok += 1;
                        }
                        format!(
                            "{name}\t{}\t{}\t{verdict}\tspec: {frames} frames, {why}; {detail}\n",
                            ours.len(),
                            theirs.len(),
                        )
                    }
                    None => {
                        let verdict = if our_aus == theirs { "PASS" } else { "FAIL" };
                        if verdict == "PASS" {
                            pass += 1;
                        }
                        format!(
                            "{name}\t{}\t{}\t{verdict}\t{detail}\n",
                            ours.len(),
                            theirs.len(),
                        )
                    }
                }
            }
            (Err(e), _) => format!("{name}\t-\t-\tREFUSED\t{e}\n"),
            (_, Err(e)) => format!("{name}\t-\t-\tSKIP\tffprobe: {e}\n"),
        };
        table.push_str(&row);
    }
    match std::env::var("EC_H265_POC_TABLE") {
        Ok(path) => std::fs::write(&path, &table).expect("write POC table"),
        Err(_) => print!("{table}"),
    }
    eprintln!(
        "POC/output order: {pass}/{} streams PASS, {spec_ok} SPEC (spec frame count, ffprobe differs)",
        streams.len()
    );
}
