//! His library, not fixtures: open, list, decode and seek real files, and
//! compare what the probe says against ffprobe file by file.
//!
//! Driven by `fixtures/real-library-manifest.tsv` (written by
//! `scripts/scan-real-library.sh`). Files that have since moved are skipped and
//! reported, never failed — the manifest is a snapshot of a moving library.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use ec_core::registry::{CodecId, MediaType, SeekMode};
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

fn decode_window(reader: &mut Reader, stream: u32, secs: f64) -> Result<Vec<f32>, String> {
    let mut decoder = reader.make_decoder(stream).map_err(|e| e.to_string())?;
    let channels = decoder.channels().max(1);
    let want = (secs * f64::from(decoder.sample_rate().max(1)) * channels as f64) as usize;
    let mut scratch = Vec::new();
    let mut pcm = Vec::new();
    while pcm.len() < want {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => return Err(format!("demux: {e}")),
        };
        if packet.stream != stream {
            continue;
        }
        decoder
            .decode(&packet, &mut scratch)
            .map_err(|e| format!("decode: {e}"))?;
        pcm.extend_from_slice(&scratch);
    }
    pcm.truncate(want);
    Ok(pcm)
}

fn reference_seek_window(path: &Path, at: f64) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{at:.6}"), "-i"])
        .arg(path)
        .args([
            "-t",
            "2",
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-",
        ])
        .output()
        .expect("reference decoder runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

struct CountedFile {
    inner: File,
    bytes: Arc<AtomicU64>,
}

impl Read for CountedFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for CountedFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn hive_avi_files() -> Vec<PathBuf> {
    let root = Path::new("/home/tahinli/Downloads");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("Hive-CM8"))
        {
            continue;
        }
        let Ok(children) = std::fs::read_dir(path) else {
            continue;
        };
        for child in children.flatten() {
            let path = child.path();
            if path.extension().and_then(|e| e.to_str()) == Some("avi") {
                files.push(path);
            }
        }
    }
    files.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));
    files
}

fn assert_avi_index_is_bounded(path: &Path) {
    let file_size = std::fs::metadata(path).expect("AVI metadata").len();
    let index_len = ec_riff::AviReader::new(File::open(path).expect("open AVI"))
        .expect("AVI index builds")
        .index_len();
    assert!(
        u64::try_from(index_len).expect("index count fits u64") <= file_size / 8,
        "{}: index has {index_len} chunks for {file_size} bytes",
        path.display()
    );
    eprintln!(
        "{}: AVI index {index_len} chunks / {file_size} bytes",
        path.display()
    );
}

fn avi_indexed_point(path: &Path, target: u64, after: bool) -> (ec_riff::AviIndexPoint, f64) {
    let reader = ec_riff::AviReader::new(File::open(path).expect("open AVI"))
        .expect("AVI index builds");
    let stream = *reader.audio_streams().first().expect("AVI audio stream");
    let interval = f64::from(stream.length) * f64::from(stream.scale) / f64::from(stream.rate)
        / reader.index_len() as f64;
    (
        reader
            .indexed_point(stream.index, target, after)
            .expect("AVI indexed point"),
        interval,
    )
}

fn samples_to_avi_units(
    stream: ec_riff::AviAudioStream,
    samples: u64,
    round_up: bool,
) -> u64 {
    let denominator = u128::from(stream.scale) * u128::from(stream.sample_rate);
    let numerator = u128::from(samples) * u128::from(stream.rate);
    let units = if round_up {
        numerator.saturating_add(denominator.saturating_sub(1)) / denominator
    } else {
        numerator / denominator
    };
    units.min(u128::from(u64::MAX)) as u64
}

fn avi_codec_preroll(codec: CodecId, rate: u32) -> f64 {
    match codec {
        CodecId::Mp3 => 1728.0 / f64::from(rate),
        CodecId::Aac => 1024.0 / f64::from(rate),
        _ => 0.0,
    }
}


fn decode_stream_to_eof(
    reader: &mut Reader,
    stream: u32,
    collect: bool,
) -> Result<(u64, u64, Vec<f32>), String> {
    let mut decoder = reader.make_decoder(stream).map_err(|e| e.to_string())?;
    let mut scratch = Vec::new();
    let mut pcm = Vec::new();
    let mut packets = 0u64;
    let mut frames = 0u64;
    loop {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => return Err(format!("demux: {e}")),
        };
        if packet.stream != stream {
            continue;
        }
        decoder
            .decode(&packet, &mut scratch)
            .map_err(|e| format!("decode: {e}"))?;
        packets += 1;
        frames += (scratch.len() / decoder.channels().max(1)) as u64;
        if collect {
            pcm.extend_from_slice(&scratch);
        }
    }
    decoder
        .flush(&mut scratch)
        .map_err(|e| format!("flush: {e}"))?;
    frames += (scratch.len() / decoder.channels().max(1)) as u64;
    if collect {
        pcm.extend_from_slice(&scratch);
    }
    Ok((packets, frames, pcm))
}

fn corr(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let (mut num, mut a2, mut b2) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        num += x * y;
        a2 += x * x;
        b2 += y * y;
    }
    num / (a2.sqrt() * b2.sqrt()).max(1e-12)
}

fn best_frame_corr(
    ours: &[f32],
    theirs: &[f32],
    channels: usize,
    frame_samples: usize,
) -> (i32, f64) {
    let frame = channels * frame_samples;
    (-1..=1)
        .map(|lag| {
            let (ours, theirs) = if lag < 0 {
                (ours, theirs.get(frame..).unwrap_or(&[]))
            } else {
                (ours.get(frame * lag as usize..).unwrap_or(&[]), theirs)
            };
            (lag, corr(ours, theirs))
        })
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap()
}

fn assert_avi_seeks_match_reference(path: &Path) {
    assert_avi_index_is_bounded(path);
    let bytes = Arc::new(AtomicU64::new(0));
    let src = CountedFile {
        inner: File::open(path).expect("open AVI"),
        bytes: bytes.clone(),
    };
    let mut reader = Reader::new(src, Some("avi")).expect("AVI opens");
    let audio = reader
        .default_stream(MediaType::Audio)
        .expect("AVI audio stream")
        .clone();
    let rate = audio.params.audio().unwrap().sample_rate.max(1);
    let channels = audio.params.audio().unwrap().layout.channel_count().max(1);
    let frame_samples = match audio.params.codec {
        CodecId::Ac3 | CodecId::EAc3 => 1536,
        CodecId::Mp3 => 1152,
        _ => 1,
    };
    let avi_stream = *ec_riff::AviReader::new(File::open(path).expect("open AVI"))
        .expect("AVI index builds")
        .audio_streams()
        .first()
        .expect("AVI audio stream");
    let duration = reader.duration().expect("AVI duration").as_secs_f64();
    let codec_preroll = avi_codec_preroll(audio.params.codec, rate);
    for fraction in [0.10, 0.50, 0.90] {
        let target = duration * fraction;
        let ticks = (target * f64::from(rate)) as i64;
        for (mode, label) in [
            (SeekMode::SyncBefore, "before"),
            (SeekMode::SyncAfter, "after"),
        ] {
            let target_units = samples_to_avi_units(
                avi_stream,
                ticks.max(0) as u64,
                matches!(mode, SeekMode::SyncAfter),
            );
            let (point, chunk_interval) =
                avi_indexed_point(path, target_units, matches!(mode, SeekMode::SyncAfter));
            let before = bytes.load(Ordering::Relaxed);
            let started = Instant::now();
            let landed = reader
                .seek(
                    audio.index,
                    Timestamp::new(ticks, TimeBase::from_rate(rate)),
                    mode,
                )
                .expect("AVI seek");
            let elapsed = started.elapsed();
            let read = bytes.load(Ordering::Relaxed) - before;
            let at = landed.as_secs_f64();
            eprintln!(
                "{} @{:.0}% {label}: requested {target_units} units, idx1 entry {} offset {} unit {}, pre-roll 0 units 0 frames, first packet pts {}, target {target:.3}s landed {at:.3}s read {read} bytes seek {:?}",
                path.display(),
                fraction * 100.0,
                point.index,
                point.offset,
                point.time,
                landed.ticks,
                elapsed,
            );
            assert!(
                read < 8 * 1024 * 1024,
                "{} @{fraction:.0}% {label}: seek read {read} bytes",
                path.display()
            );
            if !cfg!(debug_assertions) {
                assert!(
                    elapsed.as_millis() < 200,
                    "{} @{:.0}% {label}: seek took {:?}",
                    path.display(),
                    fraction * 100.0,
                    elapsed
                );
            }
            match mode {
                SeekMode::SyncBefore => {
                    assert!(
                        at <= target,
                        "{} @{fraction:.0}%: SyncBefore landed {at:.3}s after target {target:.3}s",
                        path.display()
                    );
                    let bound = (2.0 * chunk_interval).max(codec_preroll);
                    assert!(
                        target - at <= bound,
                        "{} @{fraction:.0}%: SyncBefore landed {:.3}s before target {target:.3}s (bound {bound:.3}s)",
                        path.display(),
                        target - at,
                    );
                    let ours =
                        decode_window(&mut reader, audio.index, 2.0).expect("decode after seek");
                    let theirs = reference_seek_window(path, at);
                    let (lag, score) =
                        best_frame_corr(&ours, &theirs, channels, frame_samples);
                    assert!(
                        score >= 0.999,
                        "{} @{:.0}% {label}: target {target:.3}s landed {at:.3}s lag {lag} frames corr vs reference decoder = {score}",
                        path.display(),
                        fraction * 100.0,
                    );
                }
                SeekMode::SyncAfter => {
                    assert!(
                        at >= target,
                        "{} @{fraction:.0}%: SyncAfter landed {at:.3}s before target {target:.3}s",
                        path.display()
                    );
                    assert!(
                        at - target <= chunk_interval,
                        "{} @{fraction:.0}%: SyncAfter landed {:.3}s after target {target:.3}s (chunk interval {chunk_interval:.3}s)",
                        path.display(),
                        at - target,
                    );
                }
                SeekMode::Exact => unreachable!("oracle checks indexed seeks only"),
            }
        }
    }
}

fn synthetic_mp3_avi() -> Option<PathBuf> {
    if !Command::new("ffmpeg")
        .args(["-version"])
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("reference encoder/decoder absent, skipping synthetic AVI");
        return None;
    }
    let root = PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cache/aviseek");
    std::fs::create_dir_all(&root).expect("create scratch directory");
    let path = root.join(format!("seek-oracle-{}.avi", std::process::id()));
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=997:sample_rate=48000:duration=30",
            "-c:a",
            "libmp3lame",
            "-f",
            "avi",
            "-y",
        ])
        .arg(&path)
        .output()
        .expect("reference encoder runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(path)
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

#[test]
fn avi_audio_chunks_end_cleanly_on_real_files() {
    let files = [
        "/home/tahinli/Downloads/Little.Woman.2019.DVDScr.XVID.AC3.HQ.Hive-CM8/Little.Woman.2019.DVDScr.XVID.AC3.HQ.Hive-CM8.avi",
        "/home/tahinli/Downloads/Little.Woman.2019.DVDScr.XVID.AC3.HQ.Hive-CM8/sample.avi",
    ];
    let mut present = 0;
    for path in files {
        let path = Path::new(path);
        if !path.exists() {
            continue;
        }
        present += 1;
        let mut reader = Reader::open(path).expect("AVI opens");
        let audio = reader
            .default_stream(MediaType::Audio)
            .expect("AVI audio stream")
            .index;
        let (packets, frames, _) =
            decode_stream_to_eof(&mut reader, audio, false).unwrap_or_else(|e| {
                panic!("{}: {e}", path.display());
            });
        eprintln!(
            "{}: open ok, packets {packets}, decode-to-EOF {frames} frames",
            path.display()
        );
        assert!(packets > 0, "{}: no audio packets", path.display());
        assert!(frames > 0, "{}: no decoded audio", path.display());
    }
    assert!(present > 0, "AVI files absent");
}

#[test]
fn avi_ac3_matches_reference_decoder_on_real_sample() {
    let path = Path::new(
        "/home/tahinli/Downloads/Little.Woman.2019.DVDScr.XVID.AC3.HQ.Hive-CM8/sample.avi",
    );
    if !path.exists() {
        eprintln!("file not present, skipping");
        return;
    }

    let mut reader = Reader::open(path).expect("AVI opens");
    let audio = reader
        .default_stream(MediaType::Audio)
        .expect("AVI audio stream")
        .index;
    let (packets, frames, ours) =
        decode_stream_to_eof(&mut reader, audio, true).expect("AVI decodes to EOF");
    assert!(packets > 0, "no packets");
    assert!(frames > 0, "no decoded frames");

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("reference decoder runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let theirs: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let score = corr(&ours, &theirs);
    eprintln!(
        "{}: open ok, packets {packets}, decode-to-EOF {frames} frames, corr {score:.6}",
        path.display()
    );
    assert!(score > 0.999, "corr vs reference decoder = {score}");
}

#[test]
fn avi_seek_matches_reference_decoder_on_real_files() {
    let files = hive_avi_files();
    if files.is_empty() {
        eprintln!("AVI files absent, skipping");
        return;
    }
    for path in files {
        assert_avi_seeks_match_reference(&path);
    }
}

#[test]
fn avi_seek_matches_reference_decoder_on_synthetic_mp3() {
    let Some(path) = synthetic_mp3_avi() else {
        return;
    };
    assert_avi_seeks_match_reference(&path);
    std::fs::remove_file(path).expect("remove synthetic AVI");
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
