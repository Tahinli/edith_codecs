//! What the fixtures say: every Matroska/WebM fixture demuxes, remuxes into a
//! file ffprobe agrees with and ffmpeg decodes, and a hundred random seeks land
//! on a sync point at or before their target — all on the one reader each file
//! was opened with.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Demuxer, Error, Muxer, SeekMode, TimeBase, Timestamp};
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
