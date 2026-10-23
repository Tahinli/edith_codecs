//! The personal media library, Ogg half: every `ogg` row of
//! `fixtures/real-library-manifest.tsv` whose audio is Vorbis or Opus is read
//! by [`OggDemuxer`] end to end and compared with ffmpeg/ffprobe.
//!
//! Per file the oracle checks:
//! - packet count and every packet's size (ffprobe, audio stream 0);
//! - the concatenated packet payloads (ffmpeg `-c copy -f data`), byte for byte;
//! - the header block: Opus's `OpusHead` against ffprobe's `extradata_size`
//!   exactly; Vorbis's three Xiph-laced packets by their lacing and magic (see
//!   below for why the size is not compared);
//! - timing: where a packet carries a pts, it equals ffprobe's pts after the
//!   Opus pre-skip shift; where a page ends on a packet, the granule position
//!   equals that packet's ffprobe end (pts + duration) after the same shift.
//!
//! Timing rule, as ffmpeg's Ogg demuxer applies it (measured on these files):
//! - Vorbis: granule = the sample position at the packet's end, at the stream
//!   rate; ffmpeg's pts = granule - duration. No shift. The first packet is the
//!   one exception: ffmpeg starts it at minus its own length, while ec-ogg, by
//!   its documented convention, starts the stream at zero and states no
//!   duration for a Vorbis packet. Its pts is therefore not compared; its
//!   granule still is.
//! - Opus: granule counts 48 kHz samples *including* the pre-skip; ffmpeg's pts
//!   = granule - pre_skip - duration, so the end is `granule - pre_skip`. Our
//!   packet pts start at zero, so ours = ffmpeg's pts + pre_skip.
//!
//! Header block: ffmpeg's stated Vorbis extradata size differs from ours on
//! some files (ours keeps the comment header verbatim, which the remux path
//! relies on for bit-identical output). ffprobe does not expose the extradata
//! bytes, so the Vorbis block is checked structurally and the size difference
//! is reported, not failed.
//!
//! Scope: demux only. The decoded PCM is not compared here.
//!
//! Each file either matches, is refused by name (no logical stream with a
//! known mapping), or fails. A refusal is reported, not counted as a failure.
//!
//! LOCAL ONLY and ignored: the manifest is generated from the user's own media
//! by `scripts/scan-real-library.sh` and is gitignored, so no other host has it.
//!   cargo test -p ec-ogg --test real_library -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ec_core::Demuxer;
use ec_ogg::{Mapping, OggDemuxer, granule_of};

/// One manifest row: the columns the sweep reads.
struct Entry {
    path: PathBuf,
    acodecs: String,
}

/// One packet as ffprobe reports it.
struct Probed {
    pts: Option<i64>,
    duration: Option<i64>,
    size: usize,
}

/// The verdict for one file, with the text the table prints.
enum Verdict {
    Match(String),
    Refused(String),
    Failed(String),
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The manifest's Ogg rows with Vorbis or Opus audio, or `None` when the
/// manifest itself is absent.
fn manifest_ogg_entries() -> Option<Vec<Entry>> {
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv");
    let text = std::fs::read_to_string(&manifest).ok()?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 8 || f[1] != "ogg" || !Path::new(f[0]).is_file() {
            continue;
        }
        // The first audio stream decides the mapping this file is checked as.
        let first = f[7].split(',').next().unwrap_or_default();
        if first != "vorbis" && first != "opus" {
            continue;
        }
        out.push(Entry {
            path: PathBuf::from(f[0]),
            acodecs: f[7].to_string(),
        });
    }
    Some(out)
}

/// Parses an ffprobe csv cell: `N/A` and blanks are absent.
fn cell(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s == "N/A" {
        None
    } else {
        s.parse().ok()
    }
}

/// Audio stream 0's packets: header packets are not among them, as with ours.
fn ffprobe_packets(path: &Path) -> Result<Vec<Probed>, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "packet=pts,duration,size",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !out.status.success() {
        return Err("ffprobe failed on the packets".into());
    }
    let mut packets = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 3 {
            return Err(format!("unexpected ffprobe packet row: {line}"));
        }
        let size = cell(f[2]).ok_or_else(|| format!("no packet size in: {line}"))?;
        packets.push(Probed {
            pts: cell(f[0]),
            duration: cell(f[1]),
            size: size as usize,
        });
    }
    Ok(packets)
}

/// ffprobe's stated extradata size for audio stream 0.
fn ffprobe_extradata_size(path: &Path) -> Result<usize, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=extradata_size",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or_default().trim_matches(',');
    cell(first)
        .map(|n| n as usize)
        .ok_or_else(|| format!("no extradata_size: {text:?}"))
}

/// ffmpeg's concatenated audio packet payloads, copied and not decoded.
fn ffmpeg_payload(path: &Path) -> Result<Vec<u8>, String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-c", "copy", "-f", "data", "-"])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed to copy the packets: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Reads one file with ec-ogg and compares it with the oracle.
fn check_file(path: &Path) -> Verdict {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Verdict::Failed(format!("open: {e}")),
    };
    let mut demuxer = match OggDemuxer::open(file) {
        Ok(d) => d,
        Err(e) => {
            let text = e.to_string();
            if text.contains("no logical stream with a known mapping") {
                return Verdict::Refused(text);
            }
            return Verdict::Failed(format!("open: {text}"));
        }
    };
    let Some(mapping) = demuxer.mapping(0) else {
        return Verdict::Refused("no audio stream with a known mapping".into());
    };
    let pre_skip = match mapping {
        Mapping::Opus { pre_skip, .. } => i64::from(pre_skip),
        _ => 0,
    };
    let streams = demuxer.streams().to_vec();
    let block: &[u8] = streams[0].params.extradata.as_ref().map_or(&[], |b| &b[..]);
    let vorbis = matches!(mapping, Mapping::Vorbis { .. });

    let mut ours = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(p) if p.stream == 0 => ours.push(p),
            Ok(_) => {}
            Err(e) if e.is_eof() => break,
            Err(e) => return Verdict::Failed(format!("demux: {e}")),
        }
    }
    if demuxer.damaged_pages() != 0 {
        return Verdict::Failed(format!("{} damaged pages", demuxer.damaged_pages()));
    }

    let probed = match ffprobe_packets(path) {
        Ok(p) => p,
        Err(e) => return Verdict::Failed(e),
    };
    let payload = match ffmpeg_payload(path) {
        Ok(p) => p,
        Err(e) => return Verdict::Failed(e),
    };
    let header_len = match ffprobe_extradata_size(path) {
        Ok(n) => n,
        Err(e) => return Verdict::Failed(e),
    };

    let mut problems: Vec<String> = Vec::new();
    if ours.len() != probed.len() {
        problems.push(format!(
            "packet count {} (ffprobe {})",
            ours.len(),
            probed.len()
        ));
    }
    let size_bad = ours
        .iter()
        .zip(&probed)
        .filter(|(p, q)| p.data.len() != q.size)
        .count();
    if size_bad > 0 {
        problems.push(format!("{size_bad} packet sizes differ"));
    }
    let ours_payload: Vec<u8> = ours.iter().flat_map(|p| p.data.iter().copied()).collect();
    if ours_payload != payload {
        let at = ours_payload
            .iter()
            .zip(&payload)
            .position(|(a, b)| a != b)
            .unwrap_or(ours_payload.len().min(payload.len()));
        problems.push(format!(
            "payload differs: {} bytes vs ffmpeg {}, first difference at byte {at}",
            ours_payload.len(),
            payload.len()
        ));
    }
    if vorbis {
        if let Some(p) = vorbis_header_problem(block) {
            problems.push(p);
        }
    } else if block.len() != header_len {
        problems.push(format!(
            "header block {} bytes (ffprobe extradata {header_len})",
            block.len()
        ));
    }

    // Timing, checked on the pairs both sides state.
    let mut pts_checked = 0usize;
    let mut pts_bad = 0usize;
    let mut granule_checked = 0usize;
    let mut granule_bad = 0usize;
    let mut first_timing: Option<String> = None;
    for (i, (p, q)) in ours.iter().zip(&probed).enumerate() {
        // The Vorbis first packet's start is the convention exception (see the
        // module doc): not compared.
        if let Some(pts) = p.pts.filter(|_| i > 0 || !vorbis) {
            pts_checked += 1;
            if q.pts.map(|f| f + pre_skip) != Some(pts) {
                pts_bad += 1;
                first_timing.get_or_insert(format!(
                    "packet {i}: pts {pts}, ffmpeg pts {:?} (+{pre_skip})",
                    q.pts
                ));
            }
        }
        if let Some(g) = granule_of(p) {
            granule_checked += 1;
            let end = q.pts.zip(q.duration).map(|(a, b)| a + b);
            if end != Some(g - pre_skip) {
                granule_bad += 1;
                first_timing.get_or_insert(format!(
                    "packet {i}: granule {g}, ffmpeg end {end:?} (-{pre_skip})"
                ));
            }
        }
    }
    if pts_bad > 0 {
        problems.push(format!("{pts_bad} of {pts_checked} pts differ"));
    }
    if granule_bad > 0 {
        problems.push(format!(
            "{granule_bad} of {granule_checked} granule ends differ"
        ));
    }
    if let Some(t) = first_timing {
        problems.push(format!("first timing difference: {t}"));
    }

    let summary = format!(
        "{} packets, {} bytes, {} pts and {} granules checked",
        ours.len(),
        payload.len(),
        pts_checked,
        granule_checked
    );
    if problems.is_empty() {
        Verdict::Match(summary)
    } else {
        Verdict::Failed(format!("{summary}; {}", problems.join("; ")))
    }
}

/// Our Vorbis header block: three Xiph-laced packets, identification,
/// comment and setup, each carrying its own magic. `None` when it is sound.
fn vorbis_header_problem(block: &[u8]) -> Option<String> {
    let Ok(parts) = ec_ogg::xiph_unlace(block) else {
        return Some("header block is not Xiph-laced".into());
    };
    if parts.len() != 3 {
        return Some(format!("{} header packets, want 3", parts.len()));
    }
    for (part, kind) in parts.iter().zip([1u8, 3, 5]) {
        if part.len() < 7 || part[0] != kind || &part[1..7] != b"vorbis" {
            return Some(format!("header packet of kind {kind} lacks its magic"));
        }
    }
    if parts[0].len() != 30 {
        return Some(format!(
            "identification header is {} bytes, want 30",
            parts[0].len()
        ));
    }
    None
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().chars().take(38).collect())
        .unwrap_or_default()
}

#[test]
#[ignore = "sweeps the PERSONAL media library (fixtures/real-library-manifest.tsv, ogg rows) through ffmpeg; local only: run with --ignored."]
fn real_library_ogg_streams_match_ffmpeg() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg and ffprobe are required to sweep the library");
        return;
    }
    let Some(entries) = manifest_ogg_entries() else {
        eprintln!(
            "skipped: no fixtures/real-library-manifest.tsv; generate it with \
             scripts/scan-real-library.sh on the machine that holds the media"
        );
        return;
    };
    if entries.is_empty() {
        eprintln!("skipped: the manifest lists no ogg rows with vorbis or opus audio");
        return;
    }

    let mut table = String::new();
    let mut checked = 0usize;
    let mut refused = 0usize;
    let mut failures = Vec::new();
    for entry in &entries {
        let name = short_name(&entry.path);
        let codec = entry.acodecs.split(',').next().unwrap_or_default();
        let verdict = check_file(&entry.path);
        let line = match verdict {
            Verdict::Match(s) => {
                checked += 1;
                format!("{name:<38} {codec:<7} match: {s}\n")
            }
            Verdict::Refused(e) => {
                refused += 1;
                format!("{name:<38} {codec:<7} refused: {e}\n")
            }
            Verdict::Failed(e) => {
                failures.push(format!("{name}: {e}"));
                format!("{name:<38} {codec:<7} FAILED {e}\n")
            }
        };
        table.push_str(&line);
    }
    eprintln!("{table}");
    eprintln!(
        "real library ogg: {checked} match, {refused} refused by name, {} failed",
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(
        checked + refused >= 1 && checked >= 1,
        "no real Ogg file was checked end to end"
    );
}
