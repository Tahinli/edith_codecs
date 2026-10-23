//! Parity with ffprobe, file by file: what ec-mp4 reads out of an mp4 or mov has
//! to be what ffprobe reads out of the same bytes, sample for sample and field
//! for field. This is the swap criterion for edith's `mp4::` demuxer: identical
//! packet bytes, identical timestamps, identical stream metadata.
//!
//! The corpus is three sets, each optional where it is not in the checkout:
//!
//! - `fixtures/` (video, audio and the derived shapes): always, through the
//!   usual `../../fixtures` symlink.
//! - edith's `assets/*.mp4|mov|m4a` when `EC_EDITH_ASSETS` names the directory
//!   (default `/home/user/Documents/Code/Rust/edith/assets`; skipped with a
//!   note when absent).
//! - the generated cases from `scripts/gen-mp4-parity-corpus.sh` when
//!   `EC_MP4_PARITY_CORPUS` names its output directory.
//!
//! Per track, the comparison is: codec, coded size, sample rate, channels,
//! timebase, rotation, and per sample the byte offset, size, presentation time
//! (`cts` against ffprobe `pts`), the sync flag, and the decode time up to one
//! constant. ffmpeg's `dts` carries a constant shift of its own (the largest
//! composition delay), so only its differences are the same claim. Edit lists
//! are applied on both sides, the same way ffmpeg applies them.
//!
//! Where edith's engine reads the file differently on purpose (it trims the
//! picture by the edit list where ffprobe shifts it, and ignores empty edits),
//! the edith-only values are printed in the table, not compared. See
//! `lanes/mp4demux.report.md`.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{CodecId, Demuxer, MediaParameters};
use ec_mp4::{Mp4Demuxer, Sample};

use crate::common::ffmpeg_name;

/// One ffprobe `key=value` record block, keyed by field name.
type Record = HashMap<String, String>;

/// Every `stream=`/`packet=` block of `default=nw=1` output, split where the
/// `first` key starts a new block.
fn records(text: &str, first: &str) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == first {
            out.push(Record::new());
        }
        if let Some(record) = out.last_mut() {
            record.insert(key.to_string(), value.to_string());
        }
    }
    out
}

fn ffprobe(path: &Path, entries: &str, first: &str) -> Vec<Record> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "default=nw=1"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "ffprobe refused {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    records(&String::from_utf8_lossy(&out.stdout), first)
}

/// What ffprobe says about one stream, `None` where a field is `N/A` or absent.
#[derive(Debug, Default)]
struct ProbeStream {
    codec: String,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
    time_base: String,
    duration_ts: Option<i64>,
    rotation: Option<i32>,
    attached_pic: bool,
}

#[derive(Debug, Clone, Copy)]
struct ProbePacket {
    pts: Option<i64>,
    dts: Option<i64>,
    size: u64,
    pos: Option<u64>,
    key: bool,
}

fn number<T: std::str::FromStr>(value: Option<&String>) -> Option<T> {
    value.and_then(|v| v.parse().ok())
}

fn probe_streams(path: &Path, rotations: &HashMap<u32, i32>) -> Vec<ProbeStream> {
    ffprobe(
        path,
        "stream=index,codec_name,codec_type,width,height,sample_rate,channels,time_base,duration_ts:stream_disposition=attached_pic",
        "index",
    )
    .into_iter()
    .map(|r| {
        let index = number::<u32>(r.get("index")).unwrap_or(u32::MAX);
        ProbeStream {
        codec: r.get("codec_name").cloned().unwrap_or_default(),
        width: number(r.get("width")),
        height: number(r.get("height")),
        sample_rate: number(r.get("sample_rate")),
        channels: number(r.get("channels")),
        time_base: r.get("time_base").cloned().unwrap_or_default(),
        duration_ts: number(r.get("duration_ts")),
        rotation: rotations.get(&index).copied(),
        attached_pic: r
            .get("DISPOSITION:attached_pic")
            .is_some_and(|v| v == "1"),
        }
    })
    .collect()
}

/// Display rotation per stream index, from the `side_data_list` of the JSON
/// stream dump. The `side_data=rotation` entry query is not used: on a 2 GB film
/// ffprobe spends minutes decoding for it, while the JSON dump is immediate.
fn rotations(path: &Path) -> HashMap<u32, i32> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    let mut found = HashMap::new();
    let mut current = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        // Stream objects open with `"index": n,` at the stream's own indent.
        if let Some(rest) = line.strip_prefix("\"index\": ") {
            current = rest.trim_end_matches(',').parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("\"rotation\": ")
            && let (Some(index), Ok(r)) = (current, rest.trim_end_matches(',').parse::<i32>())
        {
            found.insert(index, r);
        }
    }
    found
}

/// Packets per ffprobe stream index, each list in the order ffprobe demuxes
/// them, which for one stream is decode order.
fn probe_packets(path: &Path) -> HashMap<u32, Vec<ProbePacket>> {
    let mut by_stream: HashMap<u32, Vec<ProbePacket>> = HashMap::new();
    for r in ffprobe(
        path,
        "packet=stream_index,pts,dts,size,pos,flags",
        "stream_index",
    ) {
        let Some(index) = number::<u32>(r.get("stream_index")) else {
            continue;
        };
        by_stream.entry(index).or_default().push(ProbePacket {
            pts: number(r.get("pts")),
            dts: number(r.get("dts")),
            size: number(r.get("size")).unwrap_or(0),
            pos: number(r.get("pos")),
            key: r.get("flags").is_some_and(|f| f.starts_with('K')),
        });
    }
    by_stream
}

/// ffprobe's rotation (counter-clockwise degrees) as this crate's clockwise
/// quarter turn: the same matrix, the other direction.
fn expected_rotation(rotation: Option<i32>) -> Option<u8> {
    match rotation {
        None => Some(0),
        Some(r) if r % 90 == 0 => Some((-r / 90).rem_euclid(4) as u8),
        Some(_) => None,
    }
}

/// One file, compared. Returns the table row and every mismatch it found.
fn compare(path: &Path) -> (String, Vec<String>) {
    let mut problems = Vec::new();
    let file = File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let demuxer = Mp4Demuxer::new(BufReader::new(file))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    // Cover art is not a track: ffprobe lists an `covr` picture as a video stream
    // with `attached_pic` set, and this crate does not list it (a player that
    // shows an album cover as a video stream is the bug). Dropped on both sides.
    let rotations = rotations(path);
    let all_probed = probe_streams(path, &rotations);
    let probed: Vec<ProbeStream> = all_probed.into_iter().filter(|p| !p.attached_pic).collect();
    let packets = probe_packets(path);
    let tracks = demuxer.tracks().to_vec();
    if probed.len() != tracks.len() {
        // The track position is the only index the two share, so the counts agree
        // or the rest of the comparison is meaningless.
        problems.push(format!("track count: ec {} ffprobe {}", tracks.len(), probed.len()));
    }
    let mut rows = Vec::new();
    for (position, track) in tracks.iter().enumerate() {
        let Some(stream) = track.stream else {
            continue;
        };
        let Some(p) = probed.get(position) else {
            problems.push(format!("track {position}: no ffprobe stream"));
            continue;
        };
        let info = demuxer.streams()[stream as usize].clone();
        let samples: Vec<Sample> = demuxer.samples(stream).to_vec();
        let tag = format!("track {position} ({})", ffmpeg_name(info.params.codec));
        if ffmpeg_name(info.params.codec) != p.codec {
            problems.push(format!("{tag}: codec, ffprobe says {}", p.codec));
        }
        let tb = format!("{}/{}", info.time_base.num(), info.time_base.den());
        if tb != p.time_base {
            problems.push(format!("{tag}: timebase {tb} vs {}", p.time_base));
        }
        match &info.params.media {
            MediaParameters::Video(v) => {
                if Some((v.width, v.height)) != p.width.zip(p.height) {
                    problems.push(format!(
                        "{tag}: coded size {}x{} vs ffprobe {:?}x{:?}",
                        v.width, v.height, p.width, p.height
                    ));
                }
                // Both sides name the same matrix; ffprobe counts counter-clockwise.
                if info.rotation.steps() != expected_rotation(p.rotation) {
                    problems.push(format!(
                        "{tag}: rotation {:?} vs ffprobe {:?}",
                        info.rotation, p.rotation
                    ));
                }
            }
            MediaParameters::Audio(a) => {
                // Implicit HE-AAC (SBR with no extension frequency in the config):
                // ffprobe decodes the first frames for the doubled output rate,
                // and so does edith (`mp4_aac_prime`); this reader reports the core's.
                let implicit_sbr = info.params.codec == CodecId::Aac
                    && p.sample_rate == Some(a.sample_rate * 2);
                if Some(a.sample_rate) != p.sample_rate && !implicit_sbr {
                    problems.push(format!("{tag}: rate {} vs {:?}", a.sample_rate, p.sample_rate));
                }
                if Some(a.layout.channel_count() as u32) != p.channels {
                    problems.push(format!(
                        "{tag}: channels {} vs {:?}",
                        a.layout.channel_count(),
                        p.channels
                    ));
                }
            }
            MediaParameters::Subtitle => {}
        }
        // Per sample, against ffprobe's packets of the same stream.
        let theirs = packets.get(&(position as u32)).cloned().unwrap_or_default();
        // A subtitle track ends in a zero-duration sample at the movie's end, an
        // end marker ffprobe does not hand out as a packet. Dropped on this side
        // for the comparison only; the sample is still in the table.
        let mut samples = samples;
        if matches!(info.params.media, MediaParameters::Subtitle)
            && samples.len() == theirs.len() + 1
            && samples.last().is_some_and(|s| s.duration == 0)
        {
            samples.pop();
        }
        if theirs.len() != samples.len() {
            problems.push(format!(
                "{tag}: {} samples vs {} packets",
                samples.len(),
                theirs.len()
            ));
        }
        // VP8 and VP9 carry no `stss` in these files, so the box makes every
        // sample a sync sample, while ffprobe reads the key flag out of the
        // bitstream. The box is what edith follows; the flag difference is named.
        let box_says_all_sync = samples.iter().all(|s| s.sync);
        let bitstream_keys = theirs.iter().filter(|p| p.key).count();
        let vp_box_only = matches!(info.params.codec, CodecId::Vp8 | CodecId::Vp9)
            && box_says_all_sync
            && bitstream_keys < samples.len();
        let mut dts_shift: Option<i64> = None;
        let (mut bad_pos, mut bad_size, mut bad_pts, mut bad_sync, mut bad_dts) = (0, 0, 0, 0, 0);
        for (ours, theirs) in samples.iter().zip(&theirs) {
            if theirs.pos != Some(ours.offset) {
                bad_pos += 1;
            }
            if u64::from(ours.size) != theirs.size {
                bad_size += 1;
            }
            if theirs.pts != Some(ours.cts) {
                bad_pts += 1;
            }
            if theirs.key != ours.sync && !vp_box_only {
                bad_sync += 1;
            }
            match (theirs.dts, dts_shift) {
                (Some(d), None) => dts_shift = Some(ours.dts - d),
                (Some(d), Some(k)) if ours.dts - d != k => bad_dts += 1,
                (None, _) => bad_dts += 1,
                _ => {}
            }
        }
        for (what, n) in [
            ("offset", bad_pos),
            ("size", bad_size),
            ("pts", bad_pts),
            ("sync", bad_sync),
            ("dts", bad_dts),
        ] {
            if n > 0 {
                problems.push(format!("{tag}: {n} samples differ in {what}"));
            }
        }
        let syncs = demuxer.sync_in_display_order(stream).len();
        rows.push(format!(
            "  t{position} {:<7} n={:<6} trim={:<8} first={:<6} dts-shift={:<6} edits={:?} display-syncs={syncs} duration_ts={:?}/{:?}",
            ffmpeg_name(info.params.codec),
            samples.len(),
            format!("{:?}", demuxer.presentation_trim(stream)),
            demuxer.first_frame_sample(stream),
            dts_shift.map_or("-".into(), |k| k.to_string()),
            track.edits.iter().map(|e| (e.segment_duration, e.media_time)).collect::<Vec<_>>(),
            p.duration_ts,
            info.duration,
        ));
    }
    let row = format!("{name}\n{}", rows.join("\n"));
    (row, problems)
}

/// Every mp4-family file under `dir`, recursively, into `files`.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
            continue;
        }
        let name = path.to_string_lossy().to_ascii_lowercase();
        if [".mp4", ".m4a", ".m4v", ".mov"].iter().any(|ext| name.ends_with(ext)) {
            files.push(path);
        }
    }
}

fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"), &mut files);
    let assets = std::env::var("EC_EDITH_ASSETS")
        .unwrap_or_else(|_| "/home/user/Documents/Code/Rust/edith/assets".into());
    if Path::new(&assets).is_dir() {
        collect(Path::new(&assets), &mut files);
    } else {
        eprintln!("edith_parity: no edith assets at {assets}; skipped");
    }
    // The real library: the local films the scan recorded as mp4-family, when
    // they are still on this machine. A missing file is skipped, as the scan does.
    if let Ok(manifest) = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv"),
    ) {
        for line in manifest.lines().skip(1) {
            let mut fields = line.split('\t');
            let (Some(path), Some(container)) = (fields.next(), fields.next()) else {
                continue;
            };
            if container.contains("mp4") && Path::new(path).is_file() {
                files.push(PathBuf::from(path));
            }
        }
    }
    match std::env::var("EC_MP4_PARITY_CORPUS") {
        Ok(dir) => collect(Path::new(&dir), &mut files),
        Err(_) => eprintln!("edith_parity: EC_MP4_PARITY_CORPUS unset; generated cases skipped"),
    }
    files.sort();
    files.dedup();
    files
}

#[test]
fn every_corpus_file_matches_ffprobe_sample_for_sample() {
    let files = corpus();
    assert!(!files.is_empty(), "no mp4/mov corpus; run scripts/link-fixtures.sh");
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for path in &files {
        let (row, problems) = compare(path);
        report.push(row);
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", path.display(), problems.join("\n    ")));
        }
    }
    println!("{}", report.join("\n"));
    assert!(
        failures.is_empty(),
        "{} of {} files differ from ffprobe:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

/// The edith-side reading of the edit list and the sample table, on fixtures
/// whose answers are known: an AAC track whose edit list trims one 1024-sample
/// priming packet, and an open-GOP stream whose keyframes sit out of display
/// order.
#[test]
fn edith_trim_and_display_order_read_the_known_fixtures() {
    let aac = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio/aac-mp4-stereo-48000.mp4");
    let demuxer = Mp4Demuxer::new(BufReader::new(File::open(&aac).expect("fixture")))
        .expect("the fixture demuxes");
    assert_eq!(demuxer.presentation_trim(0), Some(1024));
    assert_eq!(demuxer.first_frame_sample(0), 2, "the trim lands on packet 2");
    assert!(demuxer.sync_in_display_order(0).is_empty(), "no ctts, no reorder");

    let gop = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/video/h264-open-gop.mp4");
    let demuxer = Mp4Demuxer::new(BufReader::new(File::open(&gop).expect("fixture")))
        .expect("the fixture demuxes");
    let syncs = demuxer.sync_in_display_order(0);
    assert!(!syncs.is_empty(), "B-frames: the display table answers");
    assert!(
        syncs.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 != w[1].1),
        "display indices ascend: {syncs:?}"
    );
    let samples = demuxer.samples(0);
    for &(_, decode) in &syncs {
        assert!(samples[decode as usize].sync, "decode {decode} is a sync sample");
    }
}
