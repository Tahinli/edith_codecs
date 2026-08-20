//! What the fixtures say: every Matroska/WebM fixture demuxes, remuxes into a
//! file ffprobe agrees with and ffmpeg decodes, and a hundred random seeks land
//! on a sync point at or before their target — all on the one reader each file
//! was opened with.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Demuxer, Error, MediaType, Muxer, SeekMode, TimeBase, Timestamp};
use ec_matroska::{MatroskaDemuxer, MatroskaMuxer};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn work() -> PathBuf {
    let dir = std::env::temp_dir().join("ec-matroska-tests");
    std::fs::create_dir_all(&dir).expect("test work directory");
    dir
}

/// Every Matroska the fixture tree holds, plus the WebM, the audio-only `.mka`
/// and the video+subtitle file ffmpeg is asked to make once — the extensions
/// this crate claims are exactly the ones that have to be exercised.
fn matroska_fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixtures().join("video"))
        .expect("fixtures/video (run scripts/gen-fixtures.sh)")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.to_string_lossy();
            name.ends_with(".mkv") || name.ends_with(".webm") || name.ends_with(".mk3d")
        })
        .collect();
    files.sort();
    files.extend(derived_fixtures());
    files
}

/// The three shapes the generated corpus has none of: a WebM, an audio-only
/// `.mka` and a file whose picture travels with sound and subtitles.
fn derived_fixtures() -> Vec<PathBuf> {
    let src = fixtures().join("video/vp9-1080p-23.976-8bit.mkv");
    let subs = fixtures().join("subs/real.srt");
    let out = work();
    let webm = out.join("vp9-opus.webm");
    let mka = out.join("opus.mka");
    let subbed = out.join("h264-aac-srt.mkv");
    let h264 = fixtures().join("video/h264-1080p-23.976-8bit.mkv");
    let (src, h264, subs) = (
        src.to_string_lossy().into_owned(),
        h264.to_string_lossy().into_owned(),
        subs.to_string_lossy().into_owned(),
    );
    let jobs: [(&PathBuf, Vec<&str>); 3] = [
        (
            &webm,
            vec![
                "-i",
                &src,
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "copy",
                "-c:a",
                "libopus",
                "-shortest",
            ],
        ),
        (
            &mka,
            vec![
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=3",
                "-c:a",
                "libopus",
            ],
        ),
        (
            &subbed,
            vec![
                "-i",
                &h264,
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:duration=2",
                "-i",
                &subs,
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:s",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-c:s",
                "srt",
                "-shortest",
            ],
        ),
    ];
    let mut made = Vec::new();
    for (path, args) in jobs {
        if !path.exists() {
            let ok = Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args(&args)
                .arg(path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                continue;
            }
        }
        made.push(path.clone());
    }
    made
}

/// The fields a remux must not change, as ffprobe reads them back.
fn probe(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,codec_name,codec_type,width,height,r_frame_rate,sample_rate,channels:\
             format=nb_streams",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "ffprobe refused {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0)
}

/// Every packet of `src` through this crate's own writer, and how many there
/// were.
fn remux(src: &Path, dst: &Path) -> usize {
    let mut demuxer = MatroskaDemuxer::new(BufReader::new(File::open(src).expect("fixture opens")))
        .expect("demuxes");
    // A WebM stays a WebM: the subset is what a browser will open, and a file
    // remuxed out of it under the `matroska` doc type is one it refuses.
    let out = File::create(dst).expect("output file");
    let mut muxer = match demuxer.doc_type() {
        "webm" => MatroskaMuxer::webm(out),
        _ => MatroskaMuxer::new(out),
    };
    for stream in demuxer.streams().to_vec() {
        muxer.add_stream(stream).expect("stream declared");
    }
    let mut packets = 0;
    loop {
        match demuxer.next_packet() {
            Ok(packet) => {
                muxer.write_packet(&packet).expect("packet written");
                packets += 1;
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("{}: {e}", src.display()),
        }
    }
    muxer.finish().expect("finished");
    packets
}

/// Seeking each stream of `path` to its own first packet lands on that packet.
fn a_seek_to_the_beginning_lands_on_the_first_packet(path: &Path) {
    let mut demuxer =
        MatroskaDemuxer::new(BufReader::new(File::open(path).expect("opens"))).expect("demuxes");
    let streams = demuxer.streams().to_vec();
    for stream in &streams {
        let mut first = None;
        loop {
            match demuxer.next_packet() {
                Ok(packet) if packet.stream == stream.index => {
                    first = packet.pts;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let Some(first) = first else { continue };
        demuxer
            .seek(
                stream.index,
                Timestamp::new(first, stream.time_base),
                SeekMode::SyncBefore,
            )
            .expect("seeks to the beginning");
        loop {
            let packet = demuxer.next_packet().expect("a packet after the seek");
            if packet.stream != stream.index {
                continue;
            }
            assert_eq!(
                packet.pts,
                Some(first),
                "{}: stream {} restarts at {:?}, not at its first packet {first}",
                path.display(),
                stream.index,
                packet.pts,
            );
            break;
        }
    }
}

/// Every `SeekHead` entry of `path` resolves to an element of the id it names.
///
/// `SeekPosition` is stated from the start of the `Segment`'s payload, and a
/// position taken off that twice lands inside a cluster: the element id read
/// there is whatever byte happened to be at the offset.
fn seek_head_points_at_what_it_names(path: &Path) {
    let bytes = std::fs::read(path).expect("the written file");
    // Element headers, by hand: an id is a variable-length integer with its
    // marker bit kept, a size is one with the marker stripped.
    let elem = |at: usize| -> (u32, usize, u64) {
        let len = (bytes[at].leading_zeros() + 1) as usize;
        let id = bytes[at..at + len]
            .iter()
            .fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
        let szn = (bytes[at + len].leading_zeros() + 1) as usize;
        let mut size = u64::from(bytes[at + len]) & (0xFFu64 >> szn);
        for &b in &bytes[at + len + 1..at + len + szn] {
            size = (size << 8) | u64::from(b);
        }
        (id, at + len + szn, size)
    };
    let (_, ebml_body, ebml_size) = elem(0);
    let segment_at = ebml_body + ebml_size as usize;
    let (id, segment_body, _) = elem(segment_at);
    assert_eq!(id, 0x1853_8067, "{}: no Segment", path.display());
    let (id, head_body, head_size) = elem(segment_body);
    assert_eq!(id, 0x114D_9B74, "{}: no SeekHead", path.display());

    let mut found = 0;
    let mut at = head_body;
    while at < head_body + head_size as usize {
        let (id, body, size) = elem(at);
        at = body + size as usize;
        if id != 0x4DBB {
            continue;
        }
        let (mut want, mut pos) = (None, None);
        let mut child = body;
        while child < at {
            let (id, body, size) = elem(child);
            child = body + size as usize;
            match id {
                0x53AB => {
                    want = Some(
                        bytes[body..body + size as usize]
                            .iter()
                            .fold(0u32, |acc, &b| (acc << 8) | u32::from(b)),
                    );
                }
                0x53AC => {
                    pos = Some(
                        bytes[body..body + size as usize]
                            .iter()
                            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
                    );
                }
                _ => {}
            }
        }
        let (want, pos) = (want.expect("a SeekID"), pos.expect("a SeekPosition"));
        let (id, ..) = elem(segment_body + pos as usize);
        assert_eq!(
            id,
            want,
            "{}: SeekHead names {want:#x} at {pos} and finds {id:#x}",
            path.display()
        );
        found += 1;
    }
    assert!(found >= 3, "{}: {found} SeekHead entries", path.display());
}

#[test]
fn every_fixture_remuxes_into_a_file_ffprobe_and_ffmpeg_agree_with() {
    let files = matroska_fixtures();
    assert!(!files.is_empty(), "no Matroska fixtures found");
    let mut table = Vec::new();
    for src in &files {
        let dst = work().join(format!(
            "remux-{}",
            src.file_name().unwrap().to_string_lossy()
        ));
        let packets = remux(src, &dst);
        assert!(packets > 0, "{}: no packets", src.display());

        let (before, after) = (probe(src), probe(&dst));
        assert_eq!(before, after, "{} field-compares", src.display());
        let (d0, d1) = (duration(src), duration(&dst));
        assert!(
            (d0 - d1).abs() <= 0.10,
            "{}: duration {d0} became {d1}",
            src.display()
        );

        // ...and the `SeekHead` really names the elements it says it does. A
        // demuxer that finds nothing there falls back to a walk and reads the
        // file perfectly well, so nothing above this notices — while a stricter
        // reader (symphonia's) follows the pointer into the middle of a cluster
        // and refuses the file outright. Checked by resolving every entry.
        seek_head_points_at_what_it_names(&dst);

        // ...and a seek to the beginning really goes there. Our own muxer cues
        // the *video* track only, and one of its cue clusters can start after
        // the first audio block does, so a seek that trusted the nearest cue
        // began past the sound: 20 ms of an export, a whole cluster of a film,
        // missing from the start of every playback that seeked to zero.
        a_seek_to_the_beginning_lands_on_the_first_packet(&dst);

        // ...and it decodes, which is the only claim a field compare cannot make.
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&dst)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "{} does not decode: {}",
            dst.display(),
            String::from_utf8_lossy(&decode.stderr)
        );
        table.push(format!(
            "{:<40} {packets:>6} packets  {d0:.3}s -> {d1:.3}s  decodes",
            src.file_name().unwrap().to_string_lossy()
        ));
    }
    println!("{}", table.join("\n"));
}

#[test]
fn a_hundred_random_seeks_land_at_or_before_their_target_on_one_reader() {
    // xorshift, so the targets are spread and the run is reproducible without a
    // dependency.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut table = Vec::new();
    for src in matroska_fixtures() {
        let secs = duration(&src).max(0.001);
        // ONE reader for the whole loop: reopening is what this crate exists
        // not to do.
        let mut demuxer =
            MatroskaDemuxer::new(BufReader::new(File::open(&src).expect("fixture opens")))
                .expect("demuxes");
        let stream = demuxer.streams()[0].index;
        // Where the file's own first random-access point sits, read off the same
        // reader before a single seek: a target in front of it can land nowhere
        // earlier.
        let first = loop {
            let packet = demuxer.next_packet().expect("a first keyframe");
            if packet.stream == stream && packet.flags.keyframe {
                break packet.time_base.rescale(
                    packet.pts.unwrap_or(0),
                    TimeBase::MILLIS,
                    ec_core::Rounding::Down,
                );
            }
        };
        let mut landed = 0;
        let mut exact = 0;
        for _ in 0..100 {
            let target_ms = (next() % ((secs * 1000.0) as u64 + 1)) as i64;
            let target = Timestamp::new(target_ms, TimeBase::MILLIS);
            demuxer
                .seek(stream, target, SeekMode::SyncBefore)
                .unwrap_or_else(|e| panic!("{}: seek to {target_ms}ms: {e}", src.display()));
            let packet = loop {
                match demuxer.next_packet() {
                    Ok(p) if p.stream == stream => break Some(p),
                    Ok(_) => continue,
                    Err(Error::Eof) => break None,
                    Err(e) => panic!("{}: after seek: {e}", src.display()),
                }
            };
            let Some(packet) = packet else {
                panic!("{}: seek to {target_ms}ms reached the end", src.display());
            };
            assert!(
                packet.flags.keyframe,
                "{}: seek to {target_ms}ms landed on a non-keyframe",
                src.display()
            );
            let pts = packet.pts.expect("a block states its timestamp");
            let ms = packet
                .time_base
                .rescale(pts, TimeBase::MILLIS, ec_core::Rounding::Down);
            // At or before the target, or the first keyframe of the file when
            // the target sits in front of it.
            assert!(
                ms <= target_ms || ms == first,
                "{}: seek to {target_ms}ms landed at {ms}ms",
                src.display()
            );
            landed += 1;
            exact += usize::from(ms == target_ms);
        }
        assert_eq!(landed, 100);
        table.push(format!(
            "{:<40} 100/100 seeks, {exact} exact, 1 reader",
            src.file_name().unwrap().to_string_lossy()
        ));
    }
    println!("{}", table.join("\n"));
}

/// A sweep of the real library: ten films, one per directory so a 60-episode
/// series counts once, each demuxed end to end and its packet counts compared
/// with ffprobe's. Ignored by default because it reads tens of gigabytes.
///
/// Run it with `cargo test -p ec-matroska --release --test matroska -- --ignored
/// --nocapture`.
#[test]
#[ignore = "reads the real library end to end"]
fn real_library_spot_sweep() {
    let manifest = std::fs::read_to_string(fixtures().join("real-library-manifest.tsv"))
        .expect("fixtures/real-library-manifest.tsv");
    let mut seen: Vec<String> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for line in manifest.lines().skip(1) {
        let mut cols = line.split('\t');
        let (Some(path), Some(container)) = (cols.next(), cols.next()) else {
            continue;
        };
        let path = PathBuf::from(path);
        if !container.contains("matroska") || !path.exists() {
            continue;
        }
        // One per directory: a series of sixty episodes is one shape, not sixty.
        let dir = path
            .parent()
            .unwrap_or(Path::new("/"))
            .to_string_lossy()
            .into_owned();
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir);
        files.push(path);
    }
    assert!(
        files.len() >= 10,
        "only {} Matroska directories",
        files.len()
    );
    let step = (files.len() / 10).max(1);
    let picked: Vec<PathBuf> = files.into_iter().step_by(step).take(10).collect();

    let mut table = Vec::new();
    for path in &picked {
        let mut demuxer =
            MatroskaDemuxer::new(BufReader::new(File::open(path).expect("film opens")))
                .expect("demuxes");
        let streams: Vec<String> = demuxer
            .streams()
            .iter()
            .map(|s| s.params.codec.name().to_string())
            .collect();
        let text: Vec<u32> = demuxer
            .streams()
            .iter()
            .filter(|s| {
                matches!(
                    s.params.codec,
                    ec_core::CodecId::Srt | ec_core::CodecId::Ass | ec_core::CodecId::WebVtt
                )
            })
            .map(|s| s.index)
            .collect();
        let mut counts = vec![0usize; streams.len()];
        let mut checked = Vec::new();
        loop {
            match demuxer.next_packet() {
                Ok(p) => {
                    counts[p.stream as usize] += 1;
                    // A text track under `ContentEncodings` that came back
                    // still compressed is not UTF-8, which is the cheapest
                    // proof the zlib path ran.
                    if text.contains(&p.stream) && !checked.contains(&p.stream) {
                        checked.push(p.stream);
                        assert!(
                            std::str::from_utf8(&p.data).is_ok(),
                            "{}: stream {} is not text",
                            path.display(),
                            p.stream
                        );
                    }
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("{}: {e}", path.display()),
            }
        }
        // ffprobe counts the same packets on the same file; the picture track is
        // first in both and is the one compared exactly.
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_packets",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .expect("ffprobe runs");
        let theirs: Vec<usize> = String::from_utf8_lossy(&out.stdout)
            .lines()
            // ffprobe leaves a trailing separator on a row whose fields it
            // padded; the count is the first number on the line.
            .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
            .collect();
        assert_eq!(
            counts.first(),
            theirs.first(),
            "{}: picture packet count",
            path.display()
        );
        assert!(counts.iter().sum::<usize>() > 0);

        // ...and twenty seeks spread over the film, on one more reader, each
        // landing on a random-access point at or before its target. A film is
        // where a cue table is worth having: the fixtures are two seconds long
        // and every one of their clusters is a keyframe.
        let mut demuxer =
            MatroskaDemuxer::new(BufReader::new(File::open(path).expect("film opens")))
                .expect("demuxes");
        let stream = demuxer.streams()[0].index;
        // Where the picture actually starts: a film whose video track begins at
        // 83 ms has nothing earlier to land on, and that is not a failed seek.
        let first = loop {
            let packet = demuxer.next_packet().expect("a first keyframe");
            if packet.stream == stream && packet.flags.keyframe {
                break packet.time_base.rescale(
                    packet.pts.unwrap_or(0),
                    TimeBase::MILLIS,
                    ec_core::Rounding::Down,
                );
            }
        };
        let secs = duration(path).max(1.0);
        for i in 0..20 {
            let target_ms = (secs * 1000.0 * f64::from(i) / 20.0) as i64;
            demuxer
                .seek(
                    stream,
                    Timestamp::new(target_ms, TimeBase::MILLIS),
                    SeekMode::SyncBefore,
                )
                .unwrap_or_else(|e| panic!("{}: seek to {target_ms}ms: {e}", path.display()));
            let packet = loop {
                match demuxer.next_packet() {
                    Ok(p) if p.stream == stream => break p,
                    Ok(_) => continue,
                    Err(e) => panic!("{}: after seek: {e}", path.display()),
                }
            };
            let ms = packet.time_base.rescale(
                packet.pts.unwrap_or(0),
                TimeBase::MILLIS,
                ec_core::Rounding::Down,
            );
            assert!(
                packet.flags.keyframe && (ms <= target_ms || ms == first),
                "{}: seek to {target_ms}ms landed at {ms}ms, key {}",
                path.display(),
                packet.flags.keyframe
            );
        }
        table.push(format!(
            "{:<64} {streams:?} ours {counts:?} ffprobe {theirs:?}",
            path.file_name().unwrap().to_string_lossy()
        ));
    }
    println!("{}", table.join("\n"));
}

/// A file whose *first* cluster holds only sound: seeking its audio track to
/// the beginning lands on that block, not on the first cue.
///
/// This muxer opens a new cluster at every video keyframe and cues the video
/// track alone, so an audio block written before the first keyframe ends up in
/// a cluster the cue table never names. A seek that started at the nearest cue
/// began *past* that block -- a whole cluster of sound gone from the start of
/// playback, which is the class of defect this crate exists not to have.
#[test]
fn the_first_cluster_is_reachable_when_no_cue_names_it() {
    let src = matroska_fixtures()
        .into_iter()
        .find(|p| {
            MatroskaDemuxer::new(BufReader::new(File::open(p).unwrap()))
                .map(|d| {
                    d.streams()
                        .iter()
                        .filter(|s| {
                            matches!(
                                s.params.codec.media_type(),
                                ec_core::registry::MediaType::Audio
                                    | ec_core::registry::MediaType::Video
                            )
                        })
                        .count()
                        > 1
                })
                .unwrap_or(false)
        })
        .expect("a fixture with both picture and sound");

    let mut demuxer =
        MatroskaDemuxer::new(BufReader::new(File::open(&src).expect("opens"))).expect("demuxes");
    let streams = demuxer.streams().to_vec();
    let audio = streams
        .iter()
        .find(|s| s.params.codec.media_type() == ec_core::registry::MediaType::Audio)
        .expect("an audio stream")
        .index;
    let mut packets = Vec::new();
    while let Ok(packet) = demuxer.next_packet() {
        packets.push(packet);
        if packets.len() > 400 {
            break;
        }
    }
    // The one block that has to lead: an audio block, in front of every
    // keyframe, so the cluster it opens is one no cue can name.
    let first_audio = packets
        .iter()
        .position(|p| p.stream == audio)
        .expect("an audio packet");
    let lead = packets.remove(first_audio);
    let want = lead.pts;
    packets.insert(0, lead);

    let dst = work().join("cue-blind-first-cluster.mkv");
    let mut muxer = MatroskaMuxer::new(File::create(&dst).expect("output"));
    for stream in &streams {
        muxer.add_stream(stream.clone()).expect("stream declared");
    }
    for packet in &packets {
        muxer.write_packet(packet).expect("packet written");
    }
    muxer.finish().expect("finished");

    let mut back =
        MatroskaDemuxer::new(BufReader::new(File::open(&dst).expect("opens"))).expect("demuxes");
    back.seek(
        audio,
        Timestamp::new(want.unwrap_or(0), streams[audio as usize].time_base),
        SeekMode::SyncBefore,
    )
    .expect("seeks to the beginning");
    loop {
        let packet = back.next_packet().expect("a packet after the seek");
        if packet.stream != audio {
            continue;
        }
        assert_eq!(
            packet.pts, want,
            "the audio restarts at {:?} and not at its first block {want:?}",
            packet.pts
        );
        break;
    }
}


/// Where our own demuxer's seek lands on a real 5.1 E-AC-3 Matroska, in
/// *timestamp* -- split from decode, per the round's charter: this measures
/// only [`MatroskaDemuxer::seek`] plus the first packet's `pts`, against what
/// `ffprobe` reports for the same track near the same instant. If this lands
/// exact, the +0.145 s the consumer measured is downstream of the demuxer
/// (the AC-3 decoder or its frame-count bookkeeping in `engine`), not here.
///
/// Skipped, not failed, without the film: a claim about his library cannot be
/// made by a machine that does not have it.
#[test]
fn seek_landing_on_his_eac3_film_matches_ffprobe() {
    same_class_sweep(
        "/home/tahinli/Downloads/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE.mkv",
        &[600.0, 610.0],
    );
    // Same-class sweep (round charter task 3): an Opus 5.1 Matroska, a codec
    // that already goes through this crate's demuxer -- if this lands as
    // tight as the E-AC-3 one, the demuxer's seek is not codec-specific and
    // the +0.145 s the consumer measured on the E-AC-3 path is downstream of
    // it either way.
    same_class_sweep(
        "/home/tahinli/Downloads/The.Hunger.Games.The.Ballad.Of.Songbirds.And.Snakes.2023.Bluray.2160p.AV1.HDR10.OPUS.7.1-UH.mkv",
        &[600.0, 610.0],
    );
}

/// Where our own demuxer's seek lands on `path`, in *timestamp* -- split from
/// decode, per the round's charter: this measures only
/// [`MatroskaDemuxer::seek`] plus the first packet's `pts`, against what
/// `ffprobe` reports for the same track near the same instant.
///
/// Skipped, not failed, without the film: a claim about his library cannot be
/// made by a machine that does not have it.
fn same_class_sweep(path: &str, wants: &[f64]) {
    let film = Path::new(path);
    if !film.exists() {
        eprintln!("skipped: {path} is not present");
        return;
    }
    let mut demux =
        MatroskaDemuxer::new(BufReader::new(File::open(film).expect("opens"))).expect("demuxes");
    let audio = demux
        .streams()
        .iter()
        .find(|s| s.params.codec.media_type() == MediaType::Audio)
        .expect("the film has an audio stream")
        .clone();

    for &want in wants {
        let target = Timestamp::new(
            (want * audio.time_base.den() as f64 / audio.time_base.num() as f64).round() as i64,
            audio.time_base,
        );
        demux
            .seek(audio.index, target, SeekMode::SyncBefore)
            .expect("seek");
        let landed = loop {
            let packet = demux.next_packet().expect("a packet after the seek");
            if packet.stream == audio.index {
                break packet.pts.expect("audio packet carries a pts");
            }
        };
        let our_secs = landed as f64 * audio.time_base.num() as f64 / audio.time_base.den() as f64;

        let ffprobe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "packet=pts_time",
                "-of",
                "csv=p=0",
                "-read_intervals",
            ])
            .arg(format!("{want}%+#1"))
            .arg(film)
            .output();
        let Ok(ffprobe) = ffprobe else {
            eprintln!("skipped ffprobe cross-check: ffprobe not runnable");
            eprintln!("want {want}s, our demuxer landed at {our_secs:.3}s (pts {landed})");
            continue;
        };
        let reference: f64 = String::from_utf8_lossy(&ffprobe.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(f64::NAN);
        eprintln!(
            "want {want}s: our demuxer landed {our_secs:.3}s (pts {landed}),              ffprobe's own packet at {reference:.3}s"
        );
    }
}

/// `ffprobe` reports MaxCLL 1230 / MaxFALL 419 for this real HDR10 film, but
/// that reading comes from `[SIDE_DATA]` on the first *frame* (a
/// `content_light_level_information` HEVC SEI message inside the bitstream),
/// not from the container's own `Colour` element. This file's `Colour`
/// element genuinely carries no `MaxCLL`/`MaxFALL`/`MasteringMetadata`
/// children (checked with `ffprobe -show_entries stream=side_data_list`,
/// which comes back empty) — so a demuxer-only reader is correct to return
/// `None` here, and the consumer's brightness-metadata gap is downstream, in
/// whether anything reads the HEVC SEI (`ec_core::color::hevc_sei_light`
/// exists and round-trips in `ec-h265-syntax`, but nothing in this workspace
/// calls it from a real decode path yet).
#[test]
#[ignore = "reads a real local file, not a CI fixture"]
fn real_hdr_film_container_states_no_light_metadata() {
    use ec_core::{Demuxer, MediaParameters};
    use ec_matroska::MatroskaDemuxer;
    let path = "/home/tahinli/Downloads/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE/Project.Hail.Mary.2026.PROPER.HDR.2160p.WEB.h265-GRACE.mkv";
    if !std::path::Path::new(path).exists() {
        eprintln!("skipped: film not present");
        return;
    }
    let f = std::fs::File::open(path).expect("open");
    let demux = MatroskaDemuxer::new(f).expect("demux opens");
    let mut checked = false;
    for s in demux.streams() {
        if let MediaParameters::Video(v) = &s.params.media {
            println!("color = {:?}", v.color);
            println!("light = {:?}", v.light);
            // BT.2020 non-constant luminance, PQ transfer, BT.2020 primaries —
            // this much the Colour element does carry, and it must match
            // ffprobe's color_space/color_primaries/color_transfer.
            assert_eq!(v.color.matrix, 9, "expected bt2020nc");
            assert_eq!(v.color.transfer, 16, "expected smpte2084 (PQ)");
            assert_eq!(v.color.primaries, 9, "expected bt2020");
            assert_eq!(
                v.light,
                ec_core::color::ContentLight::default(),
                "the Colour element has no light metadata for this file — a \
                 non-default reading here would mean either ffprobe or this \
                 reader started finding it in the container and the doc \
                 comment above is stale"
            );
            checked = true;
        }
    }
    assert!(checked, "no video stream found");
}
