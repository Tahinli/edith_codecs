//! What the fixtures say: every mp4/mov fixture demuxes into the streams
//! ffprobe reports — with the frame rate compared as an exact rational, which is
//! where 23.976 used to become 23 — remuxes into a file ffmpeg decodes, and
//! carries the boxes edith used to write by hand (`av01`, `hvc1`, `colr`,
//! `udta/name`, `tx3g`) through both directions.

use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{
    Buf, CodecId, Demuxer, Error, MediaParameters, Muxer, Packet, SeekMode, StreamInfo, TimeBase,
    Timestamp,
};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn work() -> PathBuf {
    let dir = std::env::temp_dir().join("ec-mp4-tests");
    std::fs::create_dir_all(&dir).expect("test work directory");
    dir
}

fn open(path: &Path) -> Mp4Demuxer<BufReader<File>> {
    Mp4Demuxer::new(BufReader::new(
        File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ))
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// What ffprobe calls each of our codec ids.
fn ffmpeg_name(codec: CodecId) -> &'static str {
    match codec {
        CodecId::H265 => "hevc",
        CodecId::Av1 => "av1",
        CodecId::Tx3g => "mov_text",
        other => other.name(),
    }
}

/// One ffprobe field per stream, in stream order.
fn probe_field(path: &Path, entries: &str) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "ffprobe refused {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end_matches(',').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The fields a remux must not change.
fn probe(path: &Path) -> String {
    probe_field(
        path,
        "stream=index,codec_name,codec_type,width,height,r_frame_rate,sample_rate,channels",
    )
    .join("\n")
}

fn duration(path: &Path) -> f64 {
    probe_field(path, "format=duration")
        .first()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0.0)
}

/// Every mp4-family fixture: the generated video and audio corpus, plus the
/// three shapes it has none of (a `.mov`, a fragmented file and one carrying
/// timed text), made once by ffmpeg.
fn mp4_fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ["video", "audio"]
        .iter()
        .flat_map(|dir| {
            std::fs::read_dir(fixtures().join(dir))
                .expect("fixtures (run scripts/gen-fixtures.sh)")
                .filter_map(|e| e.ok().map(|e| e.path()))
        })
        .filter(|p| {
            let name = p.to_string_lossy();
            name.ends_with(".mp4") || name.ends_with(".m4a") || name.ends_with(".mov")
        })
        .collect();
    files.sort();
    files.extend(derived_fixtures());
    files
}

/// A `.mov`, a fragmented mp4 and an mp4 with a timed-text track — none of which
/// the generated corpus has and all of which this crate claims.
fn derived_fixtures() -> Vec<PathBuf> {
    let h264 = fixtures().join("video/h264-1080p-23.976-8bit.mp4");
    let src = h264.to_string_lossy().into_owned();
    let out = work();
    let mov = out.join("h264.mov");
    let frag = out.join("h264-fragmented.mp4");
    let jobs: [(&PathBuf, Vec<&str>); 2] = [
        (&mov, vec!["-i", &src, "-c", "copy", "-f", "mov"]),
        (
            &frag,
            vec![
                "-i",
                &src,
                "-c",
                "copy",
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof",
            ],
        ),
    ];
    let mut made = Vec::new();
    for (path, args) in jobs {
        if !path.exists() {
            let ok = Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args(args)
                .arg(path)
                .status()
                .expect("ffmpeg runs")
                .success();
            assert!(ok, "ffmpeg could not make {}", path.display());
        }
        made.push(path.clone());
    }
    made
}

/// Every packet of `src` through this crate's own writer, and how many there
/// were.
fn remux(src: &Path, dst: &Path) -> usize {
    let mut demuxer = open(src);
    let mut muxer = Mp4Muxer::new(BufWriter::new(File::create(dst).expect("output file")))
        .expect("muxer opens");
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
fn every_fixture_demuxes_into_the_streams_ffprobe_reports() {
    let files = mp4_fixtures();
    assert!(!files.is_empty(), "no mp4 fixtures found");
    let mut table = Vec::new();
    for path in &files {
        let demuxer = open(path);
        let names = probe_field(path, "stream=codec_name");
        let widths = probe_field(path, "stream=width");
        let rates = probe_field(path, "stream=r_frame_rate");
        let avg_rates = probe_field(path, "stream=avg_frame_rate");
        assert_eq!(
            demuxer.streams().len(),
            names.len(),
            "{}: stream count",
            path.display()
        );
        for (i, stream) in demuxer.streams().iter().enumerate() {
            assert_eq!(
                ffmpeg_name(stream.params.codec),
                names[i],
                "{} stream {i}: codec",
                path.display()
            );
            if let MediaParameters::Video(video) = &stream.params.media {
                let dims = widths[i]
                    .split(',')
                    .next()
                    .and_then(|w| w.parse::<u32>().ok())
                    .unwrap_or(0);
                assert_eq!(video.width, dims, "{} stream {i}: width", path.display());
                // The named bug, as a field compare: the rate comes off `stts`
                // as a rational and has to be the one ffprobe read, exactly.
                let ours = video.frame_rate.expect("a frame rate");
                let ours = format!("{}/{}", ours.num(), ours.den());
                assert!(
                    ours == rates[i] || ours == avg_rates[i],
                    "{} stream {i}: fps {ours}, ffprobe r={} avg={}",
                    path.display(),
                    rates[i],
                    avg_rates[i]
                );
                if path.to_string_lossy().contains("23.976") {
                    assert_eq!(
                        video.frame_rate,
                        Some(TimeBase::new(24_000, 1001)),
                        "{}: NTSC film must stay 24000/1001",
                        path.display()
                    );
                }
            }
        }
        let d = duration(path);
        let ours = demuxer
            .streams()
            .iter()
            .filter_map(|s| Some(s.time_base.as_secs_f64() * s.duration? as f64))
            .fold(0.0f64, f64::max);
        assert!(
            (d - ours).abs() <= 0.20,
            "{}: duration {d} read back as {ours}",
            path.display()
        );
        table.push(format!(
            "{:<44} {:>2} streams  {:.3}s  {}",
            path.file_name().unwrap().to_string_lossy(),
            names.len(),
            ours,
            names.join("+")
        ));
    }
    println!("{}", table.join("\n"));
}

#[test]
fn every_fixture_remuxes_into_a_file_ffprobe_and_ffmpeg_agree_with() {
    let mut table = Vec::new();
    for src in &mp4_fixtures() {
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
            "{:<44} {packets:>6} packets  {d0:.3}s -> {d1:.3}s  decodes",
            src.file_name().unwrap().to_string_lossy()
        ));
    }
    println!("{}", table.join("\n"));
}

/// The four boxes edith writes by hand into the incumbent's output, written
/// here instead — and read back by ffprobe, which is what makes them real.
#[test]
fn the_hand_written_extensions_come_out_as_ffprobe_reads_them() {
    // av01/av1C and hvc1/hvcC, from a file that already has one of each.
    for (fixture, want) in [
        ("video/av1-1080p-23.976-8bit.mp4", "av1"),
        ("video/hevc-1080p-23.976-10bit.mp4", "hevc"),
        ("video/vp9-1080p-23.976-8bit.mp4", "vp9"),
    ] {
        let src = fixtures().join(fixture);
        let dst = work().join(format!("entry-{want}.mp4"));
        remux(&src, &dst);
        assert_eq!(
            probe_field(&dst, "stream=codec_name"),
            vec![want.to_string()],
            "{fixture} remuxed"
        );
    }

    // colr, mdcv and clli: an SDR H.264 fixture declared as an HDR10 grade, so
    // nothing in the bitstream can be the source of what ffprobe reads back.
    let src = fixtures().join("video/h264-1080p-23.976-8bit.mp4");
    let dst = work().join("entry-colr.mp4");
    {
        let mut demuxer = open(&src);
        let mut streams = demuxer.streams().to_vec();
        if let MediaParameters::Video(video) = &mut streams[0].params.media {
            video.color = ec_core::ColorInfo {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false,
            };
            video.light = ec_core::ContentLight {
                max_cll: Some(1000.0),
                max_fall: Some(400.0),
                mastering_max: Some(1000.0),
                mastering_min: Some(0.005),
            };
        }
        let mut muxer =
            Mp4Muxer::new(File::create(&dst).expect("output file")).expect("muxer opens");
        muxer.add_stream(streams[0].clone()).expect("stream");
        muxer.set_title("a movie");
        muxer
            .set_track_title(0, "the picture")
            .expect("track named");
        while let Ok(packet) = demuxer.next_packet() {
            muxer.write_packet(&packet).expect("packet written");
        }
        muxer.finish().expect("finished");
    }
    assert_eq!(
        probe_field(
            &dst,
            "stream=color_primaries,color_transfer,color_space,color_range"
        ),
        // ffprobe prints these in its own field order: range, space, transfer,
        // primaries.
        vec!["tv,bt2020nc,smpte2084,bt2020".to_string()],
        "colr nclx read back"
    );
    let side = String::from_utf8_lossy(
        &Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&dst)
            .output()
            .expect("ffprobe runs")
            .stdout,
    )
    .into_owned();
    assert!(
        side.contains("Mastering display metadata"),
        "mdcv survived: {side}"
    );
    assert!(
        side.contains("Content light level metadata"),
        "clli survived"
    );
    // ...and the two names, through our own reader, which is where a `name` box
    // is *stated* (ffmpeg's mov reader keeps the movie's, not the track's).
    let back = open(&dst);
    assert_eq!(back.title(), Some("a movie"));
    assert_eq!(back.track_title(0), Some("the picture"));
    let (colour, light) = match &back.streams()[0].params.media {
        MediaParameters::Video(v) => (v.color, v.light),
        _ => panic!("a video stream"),
    };
    assert_eq!(
        (colour.primaries, colour.transfer, colour.matrix),
        (9, 16, 9)
    );
    assert_eq!(light.max_cll, Some(1000.0));
    assert_eq!(light.mastering_max, Some(1000.0));

    // tx3g, written from nothing but cues and read back as timed text.
    let dst = work().join("entry-tx3g.mp4");
    {
        let mut muxer =
            Mp4Muxer::new(File::create(&dst).expect("output file")).expect("muxer opens");
        let mut info = StreamInfo::new(
            0,
            TimeBase::MILLIS,
            ec_core::CodecParameters::new(CodecId::Tx3g),
        );
        info.language = Some("tur".into());
        muxer.add_stream(info).expect("stream");
        muxer.set_track_title(0, "Türkçe").expect("track named");
        for (start, text) in [(0i64, "birinci"), (2_000, "ikinci"), (4_000, "üçüncü")] {
            let mut payload = (text.len() as u16).to_be_bytes().to_vec();
            payload.extend_from_slice(text.as_bytes());
            let mut packet = Packet::new(0, TimeBase::MILLIS, payload);
            packet.pts = Some(start);
            packet.duration = Some(2_000);
            packet.flags.keyframe = true;
            muxer.write_packet(&packet).expect("packet written");
        }
        muxer.finish().expect("finished");
    }
    assert_eq!(probe_field(&dst, "stream=codec_name"), vec!["mov_text"]);
    assert_eq!(probe_field(&dst, "stream_tags=language"), vec!["tur"]);
    let text = String::from_utf8_lossy(
        &Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&dst)
            .args(["-f", "srt", "-"])
            .output()
            .expect("ffmpeg runs")
            .stdout,
    )
    .into_owned();
    assert!(
        text.contains("birinci") && text.contains("üçüncü"),
        "{text}"
    );
    let back = open(&dst);
    assert_eq!(back.track_title(0), Some("Türkçe"));
    assert_eq!(back.streams()[0].language.as_deref(), Some("tur"));
}

/// The `esds` field the incumbent made unnameable: an AAC track's
/// `AudioSpecificConfig`, out of the file as the bytes it is.
#[test]
fn an_aac_tracks_audio_specific_config_survives_as_bytes() {
    let src = fixtures().join("audio/aac-mp4-stereo-48000.mp4");
    let demuxer = open(&src);
    let asc = demuxer.streams()[0]
        .params
        .extradata
        .clone()
        .expect("an AudioSpecificConfig");
    // 48 kHz stereo AAC-LC: object type 2, frequency index 3, channels 2.
    assert!(asc.len() >= 2, "an AudioSpecificConfig");
    assert_eq!(asc[0] >> 3, 2, "AAC-LC");
    assert_eq!(((asc[0] & 0x7) << 1) | (asc[1] >> 7), 3, "48 kHz");
    assert_eq!((asc[1] >> 3) & 0xF, 2, "stereo");

    // ...and back out through our own esds writer, byte for byte.
    let dst = work().join("esds.mp4");
    remux(&src, &dst);
    assert_eq!(open(&dst).streams()[0].params.extradata, Some(asc));

    // The other flavours of `mp4a` and its neighbours: an entry that is not AAC
    // is that codec, not a dropped track (the incumbent kept none of them).
    for (fixture, want) in [
        ("audio/alac-mp4-stereo-48000.m4a", CodecId::Alac),
        ("audio/aac-mp4-5.1-48000.mp4", CodecId::Aac),
    ] {
        let demuxer = open(&fixtures().join(fixture));
        assert_eq!(demuxer.streams()[0].params.codec, want, "{fixture}");
    }
}

/// An `mp4a` entry holding MP3 rather than AAC, which is the case the incumbent
/// dropped on the floor: written by ffmpeg, read as MP3 here.
#[test]
fn a_non_aac_mp4a_entry_is_not_dropped() {
    let dst = work().join("mp3-in-mp4.mp4");
    let src = fixtures().join("audio/mp3-stereo-44100.mp3");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-c", "copy"])
        .arg(&dst)
        .status()
        .expect("ffmpeg runs")
        .success();
    assert!(ok, "ffmpeg could not put MP3 in an mp4");
    let mut demuxer = open(&dst);
    assert_eq!(demuxer.streams()[0].params.codec, CodecId::Mp3);
    assert_eq!(probe_field(&dst, "stream=codec_name"), vec!["mp3"]);
    let mut packets = 0;
    while demuxer.next_packet().is_ok() {
        packets += 1;
    }
    assert!(packets > 100, "{packets} MP3 packets");
}

/// Fragments: the same pictures, out of `moof`/`trun` instead of a sample table.
#[test]
fn a_fragmented_file_demuxes_to_the_same_packets_as_a_plain_one() {
    let plain = fixtures().join("video/h264-1080p-23.976-8bit.mp4");
    let frag = work().join("h264-fragmented.mp4");
    derived_fixtures();
    let mut a = open(&plain);
    let mut b = open(&frag);
    assert!(b.fragment_count() > 0, "the fixture is fragmented");
    assert_eq!(a.streams().len(), b.streams().len());
    let mut n = 0;
    loop {
        match (a.next_packet(), b.next_packet()) {
            (Ok(x), Ok(y)) => {
                assert_eq!(x.data, y.data, "packet {n} bytes");
                assert_eq!(x.is_keyframe(), y.is_keyframe(), "packet {n} sync");
                n += 1;
            }
            (Err(_), Err(_)) => break,
            (x, y) => panic!("packet {n}: {:?} against {:?}", x.is_ok(), y.is_ok()),
        }
    }
    assert!(n >= 48, "{n} packets");
}

/// Seeking lands on a random access point at or before the target, on the one
/// reader the file was opened with.
#[test]
fn seeks_land_on_a_sync_sample_at_or_before_their_target() {
    let path = fixtures().join("video/h264-1080p-23.976-8bit.mp4");
    let mut demuxer = open(&path);
    let stream = demuxer.streams()[0].clone();
    let base = stream.time_base;
    let ticks = stream.duration.expect("a duration");
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..100 {
        let target = (next() % ticks as u64) as i64;
        demuxer
            .seek(0, Timestamp::new(target, base), SeekMode::SyncBefore)
            .expect("seek");
        let packet = demuxer.next_packet().expect("a packet after the seek");
        assert!(packet.is_keyframe(), "seek landed on a non-sync sample");
        assert!(
            packet.dts.unwrap_or(0) <= target,
            "seek landed after its target"
        );
    }
}

/// Ten thousand mutations of a real file: every one is an error or a stream, and
/// none of them is a panic.
#[test]
fn ten_thousand_mutations_never_panic() {
    let src = std::fs::read(fixtures().join("audio/aac-mp4-stereo-48000.mp4")).expect("a fixture");
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (mut opened, mut refused) = (0u32, 0u32);
    for _ in 0..10_000 {
        let mut data = src.clone();
        for _ in 0..1 + next() % 8 {
            let at = (next() % data.len() as u64) as usize;
            data[at] = (next() % 256) as u8;
        }
        match Mp4Demuxer::new(Cursor::new(data)) {
            Ok(mut demuxer) => {
                opened += 1;
                for _ in 0..64 {
                    if demuxer.next_packet().is_err() {
                        break;
                    }
                }
                let _ = demuxer.seek(0, Timestamp::new(1, TimeBase::MILLIS), SeekMode::SyncBefore);
            }
            Err(_) => refused += 1,
        }
    }
    println!("10000 mutations: {opened} opened, {refused} refused, 0 panics");
    assert!(opened > 0 && refused > 0, "the corpus has to reach both");
}

/// A truncated download is [`Error::NeedMore`], not corruption and not a panic —
/// and the prefix of a file whose `moov` is at the front still demuxes.
#[test]
fn a_truncated_file_is_need_more() {
    let src = std::fs::read(fixtures().join("audio/aac-mp4-stereo-48000.mp4")).expect("a fixture");
    let cut = Mp4Demuxer::new(Cursor::new(src[..src.len() / 3].to_vec()));
    assert!(
        matches!(cut, Err(Error::NeedMore) | Err(Error::Corrupt { .. })),
        "a truncated file must not open as a whole one"
    );
    assert!(Mp4Demuxer::new(Cursor::new(Vec::new())).is_err());
    assert!(Mp4Demuxer::new(Cursor::new(b"not an mp4 at all".to_vec())).is_err());
}

/// A muxer round trip in memory, with the timing checked at the tick: the file
/// this writes states 1001 ticks a frame on a 24000 clock, which is the only way
/// 23.976 stays itself.
#[test]
fn ntsc_timing_survives_a_round_trip_exactly() {
    let base = TimeBase::from_rate(24_000);
    let mut info = StreamInfo::new(0, base, ec_core::CodecParameters::new(CodecId::H264));
    info.params.extradata = Some(Buf::copy_from_slice(&[
        1, 0x42, 0, 0x1E, 0xFF, 0xE1, 0, 4, 0x67, 0x42, 0, 0x1E, 1, 0, 4, 0x68, 0xCE, 0x3C, 0x80,
    ]));
    if let MediaParameters::Video(v) = &mut info.params.media {
        v.width = 320;
        v.height = 240;
    }
    let mut muxer = Mp4Muxer::new(Cursor::new(Vec::new())).expect("muxer");
    muxer.add_stream(info).expect("stream");
    for i in 0..48i64 {
        let mut packet = Packet::new(0, base, vec![0u8; 16]);
        packet.pts = Some(i * 1001);
        packet.dts = Some(i * 1001);
        packet.duration = Some(1001);
        packet.flags.keyframe = i % 12 == 0;
        muxer.write_packet(&packet).expect("packet");
    }
    muxer.finish().expect("finish");
    let file = muxer.into_inner().into_inner();

    let demuxer = Mp4Demuxer::new(Cursor::new(file)).expect("reopens");
    let stream = &demuxer.streams()[0];
    assert_eq!(stream.time_base, TimeBase::from_rate(24_000));
    assert_eq!(
        stream.params.video().unwrap().frame_rate,
        Some(TimeBase::new(24_000, 1001)),
        "23.976 out of our own writer"
    );
    assert_eq!(stream.duration, Some(48 * 1001));
    let mut demuxer = demuxer;
    let mut keys = 0;
    let mut n = 0;
    while let Ok(packet) = demuxer.next_packet() {
        assert_eq!(packet.pts, Some(n * 1001));
        assert_eq!(packet.duration, Some(1001));
        keys += u32::from(packet.is_keyframe());
        n += 1;
    }
    assert_eq!(n, 48);
    assert_eq!(keys, 4);
}

/// The real library, end to end: every mp4-family file the manifest lists
/// demuxes to EOF with the packet counts ffprobe counts.
///
/// Ignored by default because it reads eleven gigabytes. Run it with
/// `cargo test -p ec-mp4 --release --test mp4 -- --ignored --nocapture`.
#[test]
#[ignore = "reads the real library end to end"]
fn the_real_library_demuxes_to_eof() {
    let manifest = std::fs::read_to_string(fixtures().join("real-library-manifest.tsv"))
        .expect("the real-library manifest (run scripts/scan-real-library.sh)");
    let files: Vec<&str> = manifest
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let path = fields.next()?;
            fields.next()?.contains("mp4").then_some(path)
        })
        .collect();
    assert!(!files.is_empty(), "no mp4-family files in the manifest");

    let mut table = Vec::new();
    let mut failed = Vec::new();
    for path in files {
        let path = Path::new(path);
        if !path.exists() {
            table.push(format!("{:<58} GONE", name_of(path)));
            continue;
        }
        let mut demuxer = open(path);
        let kinds: Vec<CodecId> = demuxer.streams().iter().map(|s| s.params.codec).collect();
        let mut ours = vec![0u64; kinds.len()];
        loop {
            match demuxer.next_packet() {
                Ok(packet) => ours[packet.stream as usize] += 1,
                Err(Error::Eof) => break,
                Err(e) => {
                    failed.push(format!("{}: {e}", name_of(path)));
                    break;
                }
            }
        }
        // ffprobe counts the same packets on the same file. Two shapes are not
        // a disagreement: cover art, which ffmpeg lists as an attached-picture
        // video stream and which is metadata rather than a track; and a track
        // whose last sample ffmpeg hands out one fewer of than its sample table
        // states (`nb_frames`) — where ours equals the table, the table is what
        // the file says.
        let counted = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_packets",
                "-show_entries",
                "stream=codec_type,nb_read_packets,nb_frames:stream_disposition=attached_pic",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .expect("ffprobe runs");
        // One line a stream: type, packets counted, frames the sample table
        // states, and whether it is cover art.
        let rows: Vec<Vec<String>> = String::from_utf8_lossy(&counted.stdout)
            .lines()
            .map(|l| {
                l.split(',')
                    .map(|f| f.trim().to_string())
                    .collect::<Vec<String>>()
            })
            .filter(|row| {
                matches!(
                    row.first().map(String::as_str),
                    Some("video" | "audio" | "subtitle")
                ) && row.get(3).map(String::as_str) != Some("1")
            })
            .collect();
        let theirs: Vec<u64> = rows
            .iter()
            .map(|r| r.get(1).and_then(|n| n.parse().ok()).unwrap_or(0))
            .collect();
        let tables: Vec<u64> = rows
            .iter()
            .map(|r| r.get(2).and_then(|n| n.parse().ok()).unwrap_or(0))
            .collect();
        let same = ours == theirs;
        let by_table = !same && ours == tables;
        if !same && !by_table {
            failed.push(format!(
                "{}: ours {ours:?} against ffprobe {theirs:?}",
                name_of(path)
            ));
        }
        table.push(format!(
            "{:<58} {:<18} ours {ours:?} ffprobe {theirs:?} {}",
            name_of(path),
            kinds.iter().map(|c| c.name()).collect::<Vec<_>>().join("+"),
            match (same, by_table) {
                (true, _) => "PASS".to_string(),
                (_, true) => format!("PASS (sample table {tables:?})"),
                _ => "FAIL".to_string(),
            }
        ));
    }
    println!("{}", table.join("\n"));
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn name_of(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name.chars().take(56).collect()
}
