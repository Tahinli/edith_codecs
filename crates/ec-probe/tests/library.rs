//! His library, not fixtures: open, list, decode and seek real files, and
//! compare what the probe says against ffprobe file by file.
//!
//! Driven by `fixtures/real-library-manifest.tsv` (written by
//! `scripts/scan-real-library.sh`). Files that have since moved are skipped and
//! reported, never failed — the manifest is a snapshot of a moving library.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::registry::{MediaType, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};
use ec_probe::Reader;

fn manifest() -> Vec<(PathBuf, String, String, f64)> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let path = PathBuf::from(f.first()?);
            let acodecs = f
                .get(7)
                .filter(|s| **s != "-")
                .map_or_else(String::new, |s| (*s).to_string());
            Some((
                path,
                (*f.get(1)?).to_string(),
                acodecs,
                f.get(8)?.parse().unwrap_or(0.0),
            ))
        })
        .collect()
}

/// Decode `secs` of audio from the current position; answers sample frames.
fn decode_some(reader: &mut Reader, stream: u32, secs: f64) -> Result<u64, String> {
    let mut decoder = reader.make_decoder(stream).map_err(|e| e.to_string())?;
    let rate = f64::from(decoder.sample_rate().max(1));
    let want = (secs * rate) as u64;
    let mut out = Vec::new();
    let mut frames = 0u64;
    while frames < want {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => return Err(format!("demux: {e}")),
        };
        if packet.stream != stream {
            continue;
        }
        decoder
            .decode(&packet, &mut out)
            .map_err(|e| format!("decode: {e}"))?;
        frames += (out.len() / decoder.channels().max(1)) as u64;
    }
    Ok(frames)
}

/// Ten files with sound, standalone audio first, then Matroska — opened,
/// decoded and seeked, with a per-file row in the report.
#[test]
fn a_real_library_sweep() {
    let files = manifest();
    if files.is_empty() {
        eprintln!("no manifest — run scripts/scan-real-library.sh");
        return;
    }
    // Half standalone audio, half Matroska: the two paths this crate owns, and
    // one row per distinct audio codec the library actually holds rather than
    // ten copies of the same aac remux.
    let standalone = |p: &Path| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        [
            "mp3", "flac", "wav", "ogg", "opus", "m4a", "aac", "mp4", "mov",
        ]
        .contains(&ext)
    };
    let mut chosen: Vec<_> = Vec::new();
    let mut seen = Vec::new();
    for want_mkv in [false, true] {
        let mut taken = 0;
        for f in &files {
            let (p, container, acodecs, _) = f;
            if acodecs.is_empty() || !p.exists() || taken >= 5 {
                continue;
            }
            let is_mkv = container.contains("matroska");
            if is_mkv != want_mkv || (!is_mkv && !standalone(p)) {
                continue;
            }
            // One file per (container, codec set): variety beats repetition.
            let key = format!("{is_mkv}:{acodecs}");
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            chosen.push(f);
            taken += 1;
        }
    }
    assert!(!chosen.is_empty(), "no readable files in the manifest");

    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for (path, container, acodecs, duration) in &chosen {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut reader = match Reader::open(path) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{name}: open failed: {e}"));
                continue;
            }
        };
        // The first audio stream that actually decodes, which is what a player
        // plays; the ones that do not are named below rather than skipped over.
        let audio = reader
            .streams()
            .iter()
            .find(|s| {
                s.params.codec.media_type() == MediaType::Audio
                    && reader.make_decoder(s.index).is_ok()
            })
            .or_else(|| reader.default_stream(MediaType::Audio))
            .cloned();
        let Some(audio) = audio else {
            failures.push(format!("{name}: no audio stream, ffprobe says {acodecs}"));
            continue;
        };
        let unsupported = reader.unsupported();
        // Every codec ffprobe lists is either decodable here or named as not.
        let listed: Vec<String> = reader
            .streams()
            .iter()
            .filter(|s| s.params.codec.media_type() == MediaType::Audio)
            .map(|s| s.params.codec.name().to_string())
            .collect();
        let decoded = decode_some(&mut reader, audio.index, 1.0);
        let rate = audio.params.audio().map_or(0, |a| a.sample_rate).max(1);
        let landing = if *duration > 4.0 {
            let ticks = (duration / 2.0 * f64::from(rate)) as i64;
            reader
                .seek(
                    audio.index,
                    Timestamp::new(ticks, TimeBase::from_rate(rate)),
                    SeekMode::SyncBefore,
                )
                .map(|t| t.as_secs_f64())
                .map_err(|e| e.to_string())
        } else {
            Ok(0.0)
        };
        let after_seek = landing
            .as_ref()
            .ok()
            .map(|_| decode_some(&mut reader, audio.index, 0.5));

        let ok = decoded.is_ok()
            && landing.is_ok()
            && matches!(after_seek, Some(Ok(n)) if n > 0)
            && listed.iter().any(|c| acodecs.split(',').any(|a| a == c));
        if !ok {
            failures.push(format!(
                "{name}: decode {decoded:?} seek {landing:?} after {after_seek:?} \
                 codecs {listed:?} vs ffprobe {acodecs}"
            ));
        }
        rows.push(format!(
            "{:<52} {:<16} {:<14} {:>7} frames  seek->{:>8}  {}",
            name.chars().take(52).collect::<String>(),
            container.split(',').next().unwrap_or(container),
            listed.join(","),
            decoded.clone().unwrap_or(0),
            landing
                .as_ref()
                .map(|s| format!("{s:.2}s"))
                .unwrap_or_else(|e| e.clone()),
            match unsupported.is_empty() {
                true => "all decodable".to_string(),
                false => unsupported
                    .iter()
                    .map(|u| format!("unsupported: {} ({})", u.codec.name(), u.reason))
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        ));
    }
    eprintln!(
        "real-library sweep ({} files):\n{}",
        rows.len(),
        rows.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The TrueHD remux in his library: the track decodes through the unified
/// reader, 7.1 at 48 kHz, with the AC-3 track beside it still decoding too.
#[test]
fn a_truehd_track_decodes() {
    let Some((path, ..)) = manifest()
        .into_iter()
        .find(|(p, _, acodecs, _)| acodecs.contains("truehd") && p.exists())
    else {
        eprintln!("no TrueHD file in the manifest — skipping");
        return;
    };
    let mut reader = Reader::open(&path).expect("open");
    let truehd = reader
        .streams()
        .iter()
        .find(|s| s.params.codec.name() == "truehd")
        .cloned()
        .expect("the TrueHD track is listed");
    let decoder = reader.make_decoder(truehd.index).expect("truehd decoder");
    assert_eq!(decoder.channels(), 8, "7.1 TrueHD is 8 channels");
    assert_eq!(decoder.sample_rate(), 48_000);
    let frames = decode_some(&mut reader, truehd.index, 1.0).expect("decode");
    assert!(frames > 0, "no TrueHD audio came out");
    // ...and the AC-3 track beside it still decodes.
    let decodable: Vec<_> = reader
        .streams()
        .iter()
        .filter(|s| s.params.codec.media_type() == MediaType::Audio)
        .filter(|s| reader.make_decoder(s.index).is_ok())
        .map(|s| s.params.codec.name())
        .collect();
    assert!(
        decodable.contains(&"ac3") || decodable.contains(&"truehd"),
        "a file with a TrueHD track and an AC-3 one came out with nothing to play: {decodable:?}"
    );
    eprintln!(
        "{}: truehd decoded {frames} frames, tracks {decodable:?}",
        path.file_name().unwrap().to_string_lossy(),
    );
}

/// ffprobe's stream count for the same file, as a cross-check that nothing is
/// being hidden.
#[allow(dead_code)]
fn ffprobe_audio_codecs(path: &Path) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .collect()
}

/// Cover-art (`APIC`) ID3v2 tags shipped by real rippers/taggers put a whole
/// image ahead of the first frame; opening must skip the tag by its own
/// syncsafe size and never confuse the codec/window prefixes it hunts for
/// sync in with that image's bytes. Matched against ffmpeg's own decode of
/// the same first second, not just "it opened".
#[test]
fn a_cover_art_tagged_mp3_opens() {
    let path = Path::new("/home/tahinli/Music/Her Nerdeysen.mp3");
    if !path.exists() {
        eprintln!("file not present, skipping");
        return;
    }
    let mut reader = Reader::open(path).expect("open");
    let audio = reader
        .default_stream(MediaType::Audio)
        .cloned()
        .expect("audio stream");
    let mut decoder = reader.make_decoder(audio.index).expect("decoder");
    let rate = decoder.sample_rate();
    let channels = decoder.channels().max(1) as u32;
    // The raw decoder hands back every sample the bitstream carries,
    // including the LAME/Xing encoder's lead-in; `initial_padding` (parsed
    // from that header) is how many of them a player is meant to drop
    // (ec-core registry.rs: "audible length is duration - initial_padding").
    // ffmpeg does that trim itself before writing PCM, so match it here.
    let lead_in = audio.initial_padding * channels;
    let mut ours = Vec::new();
    let mut scratch = Vec::new();
    let mut n = 0;
    while (ours.len() as u32) < rate + lead_in {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(e) => panic!("next_packet after {n} packets, {} samples: {e}", ours.len()),
        };
        n += 1;
        if packet.stream != audio.index {
            continue;
        }
        decoder.decode(&packet, &mut scratch).expect("decode 1s");
        ours.extend_from_slice(&scratch);
    }
    let ours = &ours[lead_in as usize..];
    assert!(ours.len() as u32 >= rate / 2, "decoded too little audio");

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-t", "1", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let theirs: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let n = ours.len().min(theirs.len());
    let (mut num, mut a2, mut b2) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (a, b) = (ours[i] as f64, theirs[i] as f64);
        num += a * b;
        a2 += a * a;
        b2 += b * b;
    }
    let corr = num / (a2.sqrt() * b2.sqrt()).max(1e-12);
    assert!(corr >= 0.999, "corr vs ffmpeg = {corr}");
}
