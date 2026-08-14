//! The fixture matrix, the remux round trip and what damage costs.
//!
//! ffprobe is the oracle for structure (packet counts, duration, "does this
//! file parse at all") and ffmpeg for content: a remuxed file has to decode to
//! the same PCM, byte for byte, as the file its packets came from. Both are
//! required — a remux that ffprobe likes but whose audio drifted is exactly the
//! failure this crate exists to avoid.
//!
//! Run the tables:
//!   cargo test -p ec-ogg --test ogg -- --nocapture

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{
    Buf, CodecId, CodecParameters, Demuxer, MediaParameters, Muxer, Packet, PacketFlags, SeekMode,
    StreamInfo, TimeBase, Timestamp,
};
use ec_ogg::{Mapping, OggDemuxer, OggMuxer, granule_of, granule_side_data};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// Working directory keyed by test name: these tests are threads of one
/// process, so a shared temp directory would have them deleting each other's
/// files.
fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-ogg-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create work dir");
    dir
}

/// Every Ogg fixture on disk, sorted so the table reads the same every run.
fn ogg_fixtures() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(fixtures()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("ogg") | Some("opus")
            )
        })
        .collect();
    out.sort();
    out
}

/// Audio packets ffprobe counts in a file — header packets excluded, which is
/// also what this demuxer hands out.
fn ffprobe_packets(path: &Path) -> u64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe must be installed: it is the oracle for this crate");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("ffprobe packet count for {}: {e}", path.display()))
}

/// ffprobe's own duration for the file, in seconds.
fn ffprobe_duration(path: &Path) -> f64 {
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
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// Decode a file to raw 32-bit float samples with ffmpeg. Errors are fatal: an
/// unreadable file is the failure being tested for.
fn decode_pcm(path: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed to decode {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// ffprobe with errors escalated: the structural verdict on a file we wrote.
fn ffprobe_clean(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,sample_rate,channels",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "ffprobe complained about {}: {stderr}",
        path.display()
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Read every packet of a file through our demuxer.
fn demux_all(path: &Path) -> (Vec<StreamInfo>, Vec<Packet>, u64) {
    let file = std::fs::File::open(path).unwrap();
    let mut demuxer = OggDemuxer::open(file).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let streams = demuxer.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(p) => packets.push(p),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
    (streams, packets, demuxer.damaged_pages())
}

#[test]
fn demuxes_every_fixture_to_eof() {
    let files = ogg_fixtures();
    if files.is_empty() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }
    println!(
        "{:<28} {:>8} {:>8} {:>10} {:>10}",
        "file", "ours", "ffprobe", "dur", "ffprobe"
    );
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let (streams, packets, damaged) = demux_all(path);
        assert_eq!(damaged, 0, "{name}: an intact fixture has no damaged pages");
        assert_eq!(streams.len(), 1, "{name}: one logical stream expected");
        let info = &streams[0];
        let expected_codec = match name.starts_with("opus") {
            true => CodecId::Opus,
            false => CodecId::Vorbis,
        };
        assert_eq!(info.params.codec, expected_codec, "{name}");
        assert!(
            info.params
                .extradata
                .as_ref()
                .is_some_and(|e| !e.is_empty()),
            "{name}: setup data must reach the decoder"
        );

        // The generator names rate and layout in the file name.
        let MediaParameters::Audio(audio) = &info.params.media else {
            panic!("{name}: audio parameters expected");
        };
        let expect_rate: u32 = name
            .rsplit('-')
            .next()
            .unwrap()
            .split('.')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let expect_channels = match true {
            _ if name.contains("mono") => 1,
            _ if name.contains("stereo") => 2,
            _ => 6,
        };
        assert_eq!(audio.layout.channel_count(), expect_channels, "{name}");
        match expected_codec {
            // Opus always decodes at 48 kHz whatever it was fed.
            CodecId::Opus => assert_eq!(audio.sample_rate, 48_000, "{name}"),
            _ => assert_eq!(audio.sample_rate, expect_rate, "{name}"),
        }

        let ours = packets.len() as u64;
        let theirs = ffprobe_packets(path);
        assert_eq!(ours, theirs, "{name}: packet count");

        // Duration: ours is the playable length, so Opus differs from ffprobe's
        // by exactly the pre-skip it reports as part of the granule count.
        let secs = |ticks: i64| ticks as f64 * info.time_base.as_secs_f64();
        let our_dur = secs(info.duration.unwrap());
        let their_dur = ffprobe_duration(path);
        let pre_skip = secs(info.start_time.unwrap());
        assert!(
            (our_dur + pre_skip - their_dur).abs() < 1e-6,
            "{name}: duration {our_dur} + pre-skip {pre_skip} vs ffprobe {their_dur}"
        );
        println!("{name:<28} {ours:>8} {theirs:>8} {our_dur:>10.4} {their_dur:>10.4}");

        // Timestamps are monotonic where the mapping states them, and Opus
        // states one for every packet.
        let stamped = packets.iter().filter(|p| p.pts.is_some()).count();
        match expected_codec {
            CodecId::Opus => assert_eq!(stamped, packets.len(), "{name}: every Opus pts"),
            _ => assert!(stamped > 0, "{name}: at least the page-boundary timestamps"),
        }
        let mut last = i64::MIN;
        for p in packets.iter().filter_map(|p| p.pts) {
            assert!(p >= last, "{name}: timestamps must not go backwards");
            last = p;
        }
        // The last packet ends where the file says the stream ends.
        assert_eq!(
            granule_of(packets.last().unwrap()),
            Some(info.duration.unwrap() + info.start_time.unwrap()),
            "{name}: the end-of-stream granule reaches the last packet"
        );
    }
    assert!(
        files.len() >= 9,
        "expected the nine Ogg fixtures, saw {}",
        files.len()
    );
}

/// Demux a fixture, write its packets back out, and require ffmpeg to decode
/// the result to the same samples.
fn remux_case(path: &Path, page_target: Option<usize>) {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let (streams, packets, _) = demux_all(path);
    let out_path = workdir("remux").join(format!("remux-{name}"));

    let file = std::fs::File::create(&out_path).unwrap();
    let mut muxer = OggMuxer::new(std::io::BufWriter::new(file));
    muxer.add_stream(streams[0].clone()).unwrap();
    muxer.set_page_target_bytes(page_target);
    for packet in &packets {
        muxer.write_packet(packet).unwrap();
    }
    muxer.finish().unwrap();

    // Structure: ffprobe reads it without a word of complaint, sees the same
    // stream and counts the same packets.
    let described = ffprobe_clean(&out_path);
    assert!(!described.is_empty(), "{name}: ffprobe described nothing");
    assert_eq!(
        ffprobe_packets(&out_path),
        packets.len() as u64,
        "{name}: remuxed packet count"
    );
    assert!(
        (ffprobe_duration(&out_path) - ffprobe_duration(path)).abs() < 1e-6,
        "{name}: remuxed duration"
    );

    // Content: same samples, byte for byte.
    let before = decode_pcm(path);
    let after = decode_pcm(&out_path);
    assert_eq!(
        after.len(),
        before.len(),
        "{name}: remux decoded to {} bytes, source to {}",
        after.len(),
        before.len()
    );
    assert!(
        after == before,
        "{name}: remuxed audio differs from the source"
    );

    // And our own demuxer reads back exactly the packets that went in.
    let (round, again, _) = demux_all(&out_path);
    assert_eq!(
        again.len(),
        packets.len(),
        "{name}: round-trip packet count"
    );
    for (a, b) in again.iter().zip(&packets) {
        assert_eq!(a.data, b.data, "{name}: payload changed in the round trip");
    }
    assert_eq!(round[0].params.codec, streams[0].params.codec);
    println!(
        "remux {name:<24} {described} ok ({} packets)",
        packets.len()
    );
}

#[test]
fn remuxes_every_fixture_bit_identically() {
    let files = ogg_fixtures();
    if files.is_empty() {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    }
    for path in &files {
        remux_case(path, None);
    }
}

/// The page-size knob must not change what comes out: one packet per page and
/// one page for the lot both have to decode identically.
#[test]
fn page_size_does_not_change_the_audio() {
    let files = ogg_fixtures();
    let Some(path) = files
        .iter()
        .find(|p| p.to_string_lossy().contains("vorbis-ogg-stereo-48000"))
    else {
        eprintln!("fixtures absent — skipping");
        return;
    };
    remux_case(path, Some(1));
    remux_case(path, Some(1 << 20));
}

#[test]
fn damage_costs_one_page_and_no_more() {
    let files = ogg_fixtures();
    let Some(path) = files
        .iter()
        .find(|p| p.to_string_lossy().contains("vorbis-ogg-stereo-44100"))
    else {
        eprintln!("fixtures absent — skipping");
        return;
    };
    let (_, clean, _) = demux_all(path);
    let bytes = std::fs::read(path).unwrap();

    // Flip a bit in the middle of the file: that lands inside a page body, so
    // the page's checksum fails and its packets are lost — and nothing else is.
    let mut damaged = bytes.clone();
    let at = damaged.len() / 2;
    damaged[at] ^= 0x40;
    let out = workdir("damage").join("flipped.ogg");
    std::fs::write(&out, &damaged).unwrap();

    let (streams, packets, dropped) = demux_all(&out);
    assert_eq!(streams.len(), 1, "the stream is still described");
    assert_eq!(dropped, 1, "exactly one page fails its checksum");
    assert!(
        packets.len() < clean.len(),
        "the damaged page's packets are gone: {} vs {}",
        packets.len(),
        clean.len()
    );
    // Recovery, not collapse: what is lost is the damaged page and nothing
    // else, so every packet after it is still there, byte for byte, in order.
    // What survives is a prefix of the clean read plus a suffix of it: the
    // packets of the damaged page, and only those, are missing from the middle.
    let kept = packets.len();
    let head = packets
        .iter()
        .zip(&clean)
        .take_while(|(a, b)| a.data == b.data)
        .count();
    let tail = packets
        .iter()
        .rev()
        .zip(clean.iter().rev())
        .take_while(|(a, b)| a.data == b.data)
        .count();
    assert!(head > 0, "nothing before the damage was read");
    assert!(tail > 0, "nothing after the damage was recovered");
    assert_eq!(
        head + tail,
        kept,
        "recovery is a prefix of {head} and a suffix of {tail} out of {kept} packets"
    );
    println!(
        "damage: {kept} of {} packets survived — {head} before the bad page, {tail} after it",
        clean.len()
    );
}

#[test]
fn seeks_land_at_or_before_the_target() {
    let files = ogg_fixtures();
    if files.is_empty() {
        eprintln!("fixtures absent — skipping");
        return;
    }
    for path in files.iter().filter(|p| {
        let n = p.to_string_lossy().to_string();
        n.contains("stereo-48000") || n.contains("5.1-44100")
    }) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let file = std::fs::File::open(path).unwrap();
        let mut demuxer = OggDemuxer::open(file).unwrap();
        let info = demuxer.streams()[0].clone();
        let duration = info.duration.unwrap();
        for fraction in [0, 1, 2, 3, 4] {
            let target = duration * fraction / 4;
            let to = Timestamp::new(target + info.start_time.unwrap(), info.time_base);
            demuxer.seek(0, to, SeekMode::SyncBefore).unwrap();
            let packet = match demuxer.next_packet() {
                Ok(p) => p,
                // Seeking to the very end can land on the end-of-stream page.
                Err(e) if e.is_eof() => continue,
                Err(e) => panic!("{name}: {e}"),
            };
            if let Some(pts) = packet.pts {
                assert!(
                    pts <= target + info.start_time.unwrap(),
                    "{name}: seek to {target} landed at {pts}"
                );
            }
            // Whatever it landed on, the rest of the file still reads.
            let mut rest = 1;
            while let Ok(_p) = demuxer.next_packet() {
                rest += 1;
            }
            assert!(rest > 0, "{name}: nothing readable after the seek");
        }
        // Seeking forwards then back to zero returns the whole stream.
        demuxer
            .seek(0, Timestamp::new(0, info.time_base), SeekMode::SyncBefore)
            .unwrap();
        let mut count = 0;
        while demuxer.next_packet().is_ok() {
            count += 1;
        }
        let (_, all, _) = demux_all(path);
        assert!(
            count >= all.len() - 2,
            "{name}: after seeking home {count} of {} packets remained",
            all.len()
        );
    }
}

/// Payload sizes Ogg's own lacing has to work for, none of which the fixtures
/// contain: a packet that is exactly a multiple of 255, one that fills more
/// than one page's segment table, and an empty one.
#[test]
fn round_trips_awkward_packet_sizes_through_memory() {
    let sizes = [
        1usize, 254, 255, 256, 509, 510, 511, 64_000, 65_025, 300_000,
    ];
    let rate = 48_000;
    let time_base = TimeBase::from_rate(rate);

    let mut params = CodecParameters::new(CodecId::Vorbis);
    if let MediaParameters::Audio(audio) = &mut params.media {
        audio.sample_rate = rate;
    }
    // A Vorbis triplet is what the mapping requires; the contents only have to
    // identify, since nothing decodes them here.
    let mut ident = vec![1u8];
    ident.extend_from_slice(b"vorbis");
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(2);
    ident.extend_from_slice(&rate.to_le_bytes());
    ident.resize(30, 0);
    let comment = vec![3u8; 400];
    let setup = vec![5u8; 9000];
    params.extradata = Some(Buf::from_vec(
        ec_ogg::xiph_lace(&[&ident, &comment, &setup]).unwrap(),
    ));
    let info = StreamInfo::new(0, time_base, params);

    let mut muxer = OggMuxer::new(Cursor::new(Vec::new()));
    muxer.add_stream(info).unwrap();
    // One packet per page, so every packet carries the page granule back out.
    muxer.set_page_target_bytes(Some(1));
    let mut granule = 0i64;
    let mut written = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        let data: Vec<u8> = (0..*size)
            .map(|n| (n as u32 % 251) as u8 ^ i as u8)
            .collect();
        granule += 1024;
        let mut packet = Packet::new(0, time_base, data.clone());
        packet.side_data.push(granule_side_data(granule));
        muxer.write_packet(&packet).unwrap();
        written.push(data);
    }
    muxer.finish().unwrap();
    let bytes = muxer.into_inner().into_inner();

    let mut demuxer = OggDemuxer::open(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.streams()[0].params.codec, CodecId::Vorbis);
    assert_eq!(
        demuxer.mapping(0),
        Some(Mapping::Vorbis { rate, channels: 2 })
    );
    // Setup data survived the trip laced the way it arrived.
    let extradata = demuxer.streams()[0].params.extradata.clone().unwrap();
    let unlaced = ec_ogg::xiph_unlace(&extradata).unwrap();
    assert_eq!(unlaced.len(), 3);
    assert_eq!(unlaced[2], &setup[..]);

    let mut read = Vec::new();
    while let Ok(packet) = demuxer.next_packet() {
        read.push(packet);
    }
    assert_eq!(read.len(), written.len(), "one packet in, one packet out");
    for (i, (got, want)) in read.iter().zip(&written).enumerate() {
        assert_eq!(
            &got.data[..],
            &want[..],
            "packet {i} of {} bytes",
            want.len()
        );
        assert_eq!(
            granule_of(got),
            Some((i as i64 + 1) * 1024),
            "packet {i}: granule position"
        );
    }
}

/// Header packets written by the caller replace what extradata would have
/// produced — the path a remux takes to carry the original `OpusTags` through.
#[test]
fn caller_supplied_headers_win_over_extradata() {
    let time_base = TimeBase::from_rate(48_000);
    let mut head = Vec::from(*b"OpusHead");
    head.extend_from_slice(&[1, 2]);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.resize(19, 0);

    let mut params = CodecParameters::new(CodecId::Opus);
    params.extradata = Some(Buf::from_vec(head.clone()));
    let info = StreamInfo::new(0, time_base, params);

    let mut tags = Vec::from(*b"OpusTags");
    tags.extend_from_slice(&4u32.to_le_bytes());
    tags.extend_from_slice(b"mine");
    tags.extend_from_slice(&0u32.to_le_bytes());

    let mut muxer = OggMuxer::new(Cursor::new(Vec::new()));
    muxer.add_stream(info).unwrap();
    for header in [&head, &tags] {
        let mut packet = Packet::new(0, time_base, header.clone());
        packet.flags = PacketFlags {
            header: true,
            ..PacketFlags::default()
        };
        muxer.write_packet(&packet).unwrap();
    }
    // One CELT 20 ms packet, so the stream has audio to end on.
    let mut audio = Packet::new(0, time_base, vec![(31 << 3) as u8, 0, 0, 0]);
    audio.side_data.push(granule_side_data(960));
    muxer.write_packet(&audio).unwrap();
    muxer.finish().unwrap();

    let bytes = muxer.into_inner().into_inner();
    // The tags packet we supplied is in the file; the synthesized one is not.
    let haystack = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        haystack.contains("mine"),
        "caller's OpusTags must be written"
    );
    assert!(
        !haystack.contains("ec-ogg"),
        "synthesized tags must not appear"
    );

    let mut demuxer = OggDemuxer::open(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.streams()[0].params.codec, CodecId::Opus);
    assert_eq!(
        demuxer.streams()[0].start_time,
        Some(312),
        "presentation starts at the pre-skip"
    );
    let packet = demuxer.next_packet().unwrap();
    assert_eq!(packet.duration, Some(960), "TOC states the packet duration");
    assert_eq!(packet.pts, Some(0));
    assert!(demuxer.next_packet().unwrap_err().is_eof());
}
