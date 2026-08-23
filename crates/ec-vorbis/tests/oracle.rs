//! Vorbis against the world: the Xiph decoder vectors, this repo's fixtures,
//! and our own encoder's output decoded by ffmpeg *and* by us.
//!
//! ffmpeg is the oracle for decode agreement (labelled as such — this is not an
//! ISO conformance claim), and the lossless property under test on the encode
//! side is the timing one: a file must decode to exactly as many samples as
//! went in, with no pre-roll and no grid overshoot.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Decoder, Demuxer, Frame};
use ec_ogg::{OggDemuxer, granule_of};
use ec_vorbis::{EncoderConfig, VorbisDecoder, VorbisEncoder};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-vorbis-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Decode with ffmpeg into planar f32, one vector per channel.
///
/// Answers [`None`] when ffmpeg itself refuses the file — the chained Xiph
/// vectors change channel count mid-file, which ffmpeg's Ogg demuxer states it
/// does not implement. Those files are still decoded by us, just without an
/// oracle to compare against.
fn ffmpeg_decode(path: &Path) -> Option<(Vec<Vec<f32>>, u32)> {
    // Key=value rather than csv: ffprobe prints the fields in its own order,
    // not the order they were asked for, and a positional parse of that reads
    // the sample rate as a channel count.
    let channels: usize = probe_field(path, "channels")?.parse().ok()?;
    let rate: u32 = probe_field(path, "sample_rate")?.parse().ok()?;

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    if !out.stderr.is_empty() {
        return None;
    }
    let interleaved: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mut planes = vec![Vec::with_capacity(interleaved.len() / channels); channels];
    for (i, value) in interleaved.into_iter().enumerate() {
        planes[i % channels].push(value);
    }
    Some((planes, rate))
}

/// One `stream=` field of the first audio stream.
fn probe_field(path: &Path, field: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            &format!("stream={field}"),
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match text.is_empty() || text == "N/A" {
        true => None,
        false => Some(text),
    }
}

/// Decode with ours, trimming exactly the way an Ogg player does: the first
/// page's granule says how much of the first block is before the start of the
/// stream, and the last page's granule says where it ends.
fn our_decode(path: &Path) -> (Vec<Vec<f32>>, u32) {
    let mut demuxer = OggDemuxer::open(File::open(path).expect("open")).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::CodecId::Vorbis)
        .expect("a Vorbis stream")
        .clone();
    let extradata = stream.params.extradata.clone().expect("headers");
    let mut decoder = VorbisDecoder::from_extradata(&extradata).expect("headers parse");
    let rate = stream
        .params
        .audio()
        .map(|a| a.sample_rate)
        .expect("audio params");

    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut offset: Option<i64> = None;
    let mut last_granule = 0i64;
    let mut produced = 0i64;
    loop {
        let packet = match demuxer.next_packet() {
            Ok(packet) => packet,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux {}: {e}", path.display()),
        };
        if packet.stream != stream.index {
            continue;
        }
        // A packet a decoder cannot use is skipped, not fatal: the Xiph
        // `unused-mode` vector states a mode number its own setup header never
        // defined, and ffmpeg skips those packets too.
        let Ok(decoded) = decoder.decode_audio(&packet.data) else {
            continue;
        };
        if planes.is_empty() {
            planes = vec![Vec::new(); decoded.len()];
        }
        for (channel, samples) in decoded.iter().enumerate() {
            planes[channel].extend_from_slice(samples);
        }
        produced += decoded.first().map_or(0, Vec::len) as i64;
        if let Some(granule) = granule_of(&packet) {
            if offset.is_none() {
                offset = Some(granule - produced);
            }
            last_granule = granule;
        }
    }
    let offset = offset.unwrap_or(0);
    let front = (-offset).max(0) as usize;
    let end = (last_granule - offset).max(0) as usize;
    for plane in &mut planes {
        let end = end.min(plane.len());
        let front = front.min(end);
        *plane = plane[front..end].to_vec();
    }
    (planes, rate)
}

/// Normalised cross-correlation of two signals over their common length.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut sum_ab = 0.0f64;
    let mut sum_aa = 0.0f64;
    let mut sum_bb = 0.0f64;
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        sum_ab += x * y;
        sum_aa += x * x;
        sum_bb += y * y;
    }
    match sum_aa > 0.0 && sum_bb > 0.0 {
        true => sum_ab / (sum_aa.sqrt() * sum_bb.sqrt()),
        // Two silent signals agree; one silent and one not does not.
        false => f64::from(u8::from(sum_aa == sum_bb)),
    }
}

fn rms(values: &[f32]) -> f64 {
    match values.is_empty() {
        true => 0.0,
        false => (values
            .iter()
            .map(|v| f64::from(*v) * f64::from(*v))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt(),
    }
}

/// Every `.ogg` in a fixture directory, sorted.
fn ogg_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ogg"))
        .collect();
    files.sort();
    files
}

#[test]
fn xiph_vectors_decode_like_ffmpeg() {
    let dir = fixtures().join("vectors/vorbis-xiph");
    let files = ogg_files(&dir);
    assert!(
        files.len() >= 20,
        "expected the Xiph vector set, got {}",
        files.len()
    );
    let mut failures = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let (ours, our_rate) = our_decode(file);
        let Some((theirs, their_rate)) = ffmpeg_decode(file) else {
            // No oracle for this one: hold it to what can still be checked,
            // that it decodes at all and is not silence.
            let energy: f64 = ours.iter().map(|c| rms(c)).sum();
            println!(
                "{name}: ffmpeg refuses this file; ours decodes {} ch, rms sum {energy:.5}",
                ours.len()
            );
            if ours.is_empty() || energy <= 0.0 {
                failures.push(format!("{name}: decoded to nothing"));
            }
            continue;
        };
        if our_rate != their_rate || ours.len() != theirs.len() {
            failures.push(format!(
                "{name}: {our_rate} Hz x{} vs ffmpeg {their_rate} Hz x{}",
                ours.len(),
                theirs.len()
            ));
            continue;
        }
        for (channel, (mine, theirs)) in ours.iter().zip(theirs.iter()).enumerate() {
            let corr = correlation(mine, theirs);
            let length = (mine.len() as i64 - theirs.len() as i64).abs();
            println!(
                "{name} ch{channel}: corr {corr:.6} len {} vs {} (delta {length}) rms {:.5}/{:.5}",
                mine.len(),
                theirs.len(),
                rms(mine),
                rms(theirs)
            );
            if corr < 0.999 || length > 0 {
                failures.push(format!(
                    "{name} ch{channel}: corr {corr:.6}, length delta {length}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn repo_fixtures_decode_like_ffmpeg_including_surround() {
    let dir = fixtures().join("audio");
    let files: Vec<PathBuf> = ogg_files(&dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("vorbis-")
        })
        .collect();
    assert!(!files.is_empty(), "no vorbis fixtures in {}", dir.display());
    let mut failures = Vec::new();
    for file in &files {
        let (theirs, _) = ffmpeg_decode(file).expect("ffmpeg decodes the repo fixtures");
        let (ours, _) = our_decode(file);
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        for (channel, (mine, reference)) in ours.iter().zip(theirs.iter()).enumerate() {
            let corr = correlation(mine, reference);
            println!(
                "{name} ch{channel}: corr {corr:.6} len {} vs {}",
                mine.len(),
                reference.len()
            );
            if corr < 0.999 || mine.len() != reference.len() {
                failures.push(format!("{name} ch{channel}: corr {corr:.6}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A test signal with a different tone in every channel, so a channel-order
/// mistake shows up as a correlation of nearly zero rather than a subtlety.
fn tones(channels: usize, rate: u32, samples: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|channel| {
            let hz = 220.0 * f64::from(channel as u32 + 1);
            (0..samples)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    // A slow envelope so the encoder has to track something.
                    let envelope = 0.5 + 0.4 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
                    (envelope * (2.0 * std::f64::consts::PI * hz * t).sin() * 0.6) as f32
                })
                .collect()
        })
        .collect()
}

/// Write our packets into an Ogg file with the ec-ogg muxer.
fn mux(path: &Path, encoder: &VorbisEncoder, packets: &[(Vec<u8>, i64)], rate: u32, channels: u16) {
    use ec_core::{
        AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, MediaParameters, Muxer,
        Packet, StreamInfo, TimeBase,
    };
    let base = TimeBase::new(1, i64::from(rate));
    let mut params = CodecParameters::new(CodecId::Vorbis);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: rate,
        layout: ChannelLayout::from_count(usize::from(channels)),
        format: None,
        bits_per_sample: None,
    });
    params.extradata = Some(Buf::from_vec(encoder.extradata()));
    let file = File::create(path).expect("create");
    let mut muxer = ec_ogg::OggMuxer::new(file);
    muxer
        .add_stream(StreamInfo::new(0, base, params))
        .expect("add stream");
    muxer.write_headers().expect("headers");
    for (data, granule) in packets {
        let mut packet = Packet::new(0, base, data.clone());
        packet.side_data.push(ec_ogg::granule_side_data(*granule));
        muxer.write_packet(&packet).expect("packet");
    }
    muxer.finish().expect("finish");
}

/// Encode `source` and answer the file it was written to.
fn encode_to_file(source: &[Vec<f32>], rate: u32, bitrate: i32, name: &str) -> PathBuf {
    let channels = source.len() as u16;
    let mut encoder = VorbisEncoder::new(EncoderConfig {
        sample_rate: rate,
        channels,
        bitrate_bps: bitrate,
        quality: 0.6,
    })
    .expect("encoder");
    let borrowed: Vec<&[f32]> = source.iter().map(|c| &c[..]).collect();
    encoder.push_planar(&borrowed).expect("push");
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push((packet.data, packet.granule)),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("encode: {e}"),
        }
    }
    let path = scratch().join(name);
    mux(&path, &encoder, &packets, rate, channels);
    path
}

/// Duration ffprobe reports, in samples of the stream's own rate.
fn probed_samples(path: &Path) -> i64 {
    probe_field(path, "duration_ts")
        .and_then(|text| text.parse().ok())
        .unwrap_or(-1)
}

#[test]
fn encoded_streams_decode_cleanly_at_the_stated_length() {
    let rate = 48_000u32;
    let samples = rate as usize * 2;
    let mut failures = Vec::new();
    for channels in [1usize, 2, 6] {
        let source = tones(channels, rate, samples);
        for bitrate in [96_000i32, 128_000, 192_000] {
            let name = format!("enc-{channels}ch-{}k.ogg", bitrate / 1000);
            let path = encode_to_file(&source, rate, bitrate, &name);
            let bytes = std::fs::metadata(&path).expect("file").len();
            let measured = bytes as f64 * 8.0 * f64::from(rate) / samples as f64 / 1000.0;

            // ffmpeg decodes it without a word of complaint...
            let Some((theirs, _)) = ffmpeg_decode(&path) else {
                failures.push(format!("{name}: ffmpeg refused the file"));
                continue;
            };
            // ...for exactly as long as the input was.
            let duration = probed_samples(&path);
            if duration != samples as i64 {
                failures.push(format!(
                    "{name}: ffprobe says {duration} samples, source is {samples}"
                ));
            }
            let mut worst = 1.0f64;
            for (channel, reference) in source.iter().enumerate() {
                let corr = correlation(&theirs[channel], reference);
                worst = worst.min(corr);
            }
            // ...and our own decoder agrees with ffmpeg about what it says.
            let (ours, _) = our_decode(&path);
            let mut agreement = 1.0f64;
            for (mine, theirs) in ours.iter().zip(theirs.iter()) {
                agreement = agreement.min(correlation(mine, theirs));
            }
            println!(
                "{name}: {measured:.0} kbps, corr vs source {worst:.4}, ours vs ffmpeg {agreement:.6}, samples {duration}"
            );
            let bar = match channels {
                6 => 0.9,
                _ => 0.95,
            };
            if worst < bar {
                failures.push(format!("{name}: corr vs source {worst:.4} under {bar}"));
            }
            if agreement < 0.999 {
                failures.push(format!(
                    "{name}: our decode disagrees with ffmpeg ({agreement:.6})"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The `Decoder` trait's own EOS path — `send_packet`/`receive_frame` until
/// exhausted, then `flush` — has to deliver the terminal block's last hop:
/// without it, decode is always exactly one hop short of the encoded length.
#[test]
fn flush_delivers_the_terminal_hop() {
    let rate = 48_000u32;
    let hop = 1024usize;
    // Not a multiple of the hop, so the tail block's un-overlapped half is
    // the only place these trailing samples can come from.
    let samples = rate as usize * 2 + 777;
    let source = tones(2, rate, samples);
    let path = encode_to_file(&source, rate, 128_000, "flush-tail.ogg");

    let mut demuxer = OggDemuxer::open(File::open(&path).expect("open")).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::CodecId::Vorbis)
        .expect("a Vorbis stream")
        .clone();
    let extradata = stream.params.extradata.clone().expect("headers");
    let mut decoder = VorbisDecoder::from_extradata(&extradata).expect("headers parse");

    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut offset: Option<i64> = None;
    let mut last_granule = 0i64;
    let mut produced = 0i64;
    let push_frame = |frame: ec_core::AudioFrame, planes: &mut Vec<Vec<f32>>| {
        if planes.is_empty() {
            *planes = vec![Vec::new(); frame.channels()];
        }
        for (channel, plane) in planes.iter_mut().enumerate() {
            let bytes = &frame.data[channel];
            for chunk in bytes.chunks_exact(4).take(frame.samples) {
                plane.push(f32::from_le_bytes(chunk.try_into().unwrap()));
            }
        }
        frame.samples as i64
    };
    loop {
        let packet = match demuxer.next_packet() {
            Ok(packet) => packet,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        };
        if packet.stream != stream.index {
            continue;
        }
        decoder.send_packet(&packet).expect("send_packet");
        loop {
            match decoder.receive_frame() {
                Ok(Frame::Audio(frame)) => produced += push_frame(frame, &mut planes),
                Ok(Frame::Video(_)) => unreachable!("Vorbis decodes to audio only"),
                Err(e) if e.is_need_more() => break,
                Err(e) => panic!("receive_frame: {e}"),
            }
        }
        if let Some(granule) = granule_of(&packet) {
            if offset.is_none() {
                offset = Some(granule - produced);
            }
            last_granule = granule;
        }
    }
    decoder.flush().expect("flush");
    loop {
        match decoder.receive_frame() {
            Ok(Frame::Audio(frame)) => {
                let _ = push_frame(frame, &mut planes);
            }
            Ok(Frame::Video(_)) => unreachable!("Vorbis decodes to audio only"),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("receive_frame after flush: {e}"),
        }
    }
    let offset = offset.unwrap_or(0);
    let front = (-offset).max(0) as usize;
    let end = (last_granule - offset).max(0) as usize;
    for plane in &mut planes {
        let end = end.min(plane.len());
        let front = front.min(end);
        *plane = plane[front..end].to_vec();
    }

    for (channel, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            samples,
            "ch{channel}: expected exactly {samples} samples out"
        );
        let tail_ours = &plane[samples - (hop - 1)..];
        let tail_source = &source[channel][samples - (hop - 1)..];
        let corr = correlation(tail_ours, tail_source);
        assert!(corr > 0.99, "ch{channel}: tail hop corr {corr:.4}");
    }
}

#[test]
fn a_short_input_keeps_its_own_length() {
    // Shorter than one block: the grid still has to close and the granule
    // still has to state the input's own count.
    let rate = 44_100u32;
    for samples in [1usize, 700, 1024, 1025, 4096] {
        let source = tones(2, rate, samples);
        let path = encode_to_file(&source, rate, 128_000, &format!("short-{samples}.ogg"));
        let duration = probed_samples(&path);
        assert_eq!(
            duration, samples as i64,
            "{samples} samples in, ffprobe says {duration}"
        );
    }
}

/// Noise-plus-tones: a signal with enough going on that the rate loop has
/// something to spend bits on, which a pure tone does not.
fn busy(channels: usize, rate: u32, samples: usize) -> Vec<Vec<f32>> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / 8_388_608.0 - 1.0
    };
    (0..channels)
        .map(|channel| {
            (0..samples)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    let hz = 300.0 * f64::from(channel as u32 + 1);
                    let tone = (2.0 * std::f64::consts::PI * hz * t).sin() as f32;
                    0.35 * tone + 0.35 * noise()
                })
                .collect()
        })
        .collect()
}

/// A hard silence-to-full-scale onset must not leak into the long block
/// before it: that block should now be coded with a short right window (or
/// the onset itself carried by a short block), which windows the leak out
/// instead of smearing it across ~46 ms the way an all-long-block encoder
/// does.
#[test]
fn onset_after_silence_has_no_pre_echo() {
    // Onsets swept by phase (fractions of a second, so they land anywhere
    // relative to the long hop) and by offset from the long-hop grid itself,
    // since the short run's placement is decided per hop. Stereo carries
    // distinct content per channel — a different tone on the right, starting
    // 11 samples later — so coupling cannot hide a per-channel leak. The gap
    // is measured over the WHOLE silence up to the onset, so a leak in the
    // last few hundred samples before it cannot hide behind a guard band, and
    // the peak is barred too.
    let amp = 0.7f64;
    for (rate, layouts) in [(44_100u32, &[1usize, 2][..]), (48_000, &[1, 2, 6]), (96_000, &[6])] {
        let grid = 16 * 1024usize;
        let onsets = [0.3333f64, 0.5, 0.7321, 0.9137, 1.0]
            .into_iter()
            .map(|s| (s * f64::from(rate)) as usize)
            .chain([0usize, 37, 512, 1000, 1023].into_iter().map(|o| grid + o));
        for onset in onsets {
            for &channels in layouts {
                let tone_len = rate as usize;
                let mut source = vec![vec![0.0f32; onset + tone_len + 11 * channels]; channels];
                for (c, plane) in source.iter_mut().enumerate() {
                    let hz = 1_000.0 + 370.0 * c as f64;
                    let start = onset + 11 * c;
                    for i in 0..tone_len {
                        let t = i as f64 / f64::from(rate);
                        plane[start + i] = (amp * (2.0 * std::f64::consts::PI * hz * t).sin()) as f32;
                    }
                }
                let name = format!("onset-{rate}-{channels}ch-{onset}.ogg");
                let path = encode_to_file(&source, rate, 128_000, &name);

                // The reference oracle must decode the short-block stream
                // cleanly: no warnings, let alone errors.
                let warn = Command::new("ffmpeg")
                    .args(["-v", "warning", "-i"])
                    .arg(&path)
                    .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
                    .output();
                if let Ok(out) = warn {
                    assert!(
                        out.stderr.is_empty(),
                        "{name}: oracle decode warnings: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }

                let tone_rms = rms(&source[0][onset..]);
                let check = |label: &str, planes: &[Vec<f32>]| {
                    for (c, plane) in planes.iter().enumerate() {
                        let gap = &plane[..onset.min(plane.len())];
                        let gap_rms = rms(gap);
                        let peak = gap.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                        println!("{name}/{label} ch{c}: gap RMS {gap_rms:.6} peak {peak:.4}");
                        assert!(
                            gap_rms <= 0.002 * tone_rms,
                            "{name}/{label} ch{c}: gap RMS {gap_rms:.6} vs tone RMS {tone_rms:.6}"
                        );
                        assert!(f64::from(peak) <= 0.01 * amp, "{name}/{label} ch{c}: gap peak {peak:.4}");
                    }
                };
                let (ours, _) = our_decode(&path);
                check("ours", &ours);
                if let Some((theirs, _)) = ffmpeg_decode(&path) {
                    check("oracle", &theirs);
                }
            }
        }
    }
}

#[test]
fn the_rate_loop_tracks_the_target_bitrate() {
    let rate = 48_000u32;
    let samples = rate as usize * 3;
    let source = busy(2, rate, samples);
    let mut measured = Vec::new();
    for target in [96_000i32, 128_000, 192_000] {
        let path = encode_to_file(
            &source,
            rate,
            target,
            &format!("abr-{}k.ogg", target / 1000),
        );
        let bytes = std::fs::metadata(&path).expect("file").len();
        let kbps = bytes as f64 * 8.0 * f64::from(rate) / samples as f64 / 1000.0;
        println!("target {} kbps -> {kbps:.0} kbps", target / 1000);
        measured.push(kbps);
    }
    // Monotone in the target, and within a quarter of it: the loop moves one
    // quantiser gain, so it tracks rather than hits.
    assert!(
        measured[0] < measured[1] && measured[1] < measured[2],
        "{measured:?}"
    );
    for (kbps, target) in measured.iter().zip([96.0, 128.0, 192.0]) {
        assert!(
            (kbps - target).abs() / target < 0.25,
            "{kbps:.0} kbps for a {target:.0} kbps target"
        );
    }
}

/// Quiet and sparse content must reach the target too: with the floor bounded
/// by the threshold of hearing, most partitions code as class 0 at any step,
/// so the rate loop's extra headroom has to lower that bound to find bits to
/// spend. The fixture is a sparse tone at -18 dBFS; the synthetic sources are
/// the busy and tonal signals at -40 dBFS, the tones over a -90 dBFS noise
/// floor (what any 16-bit source carries; a pure digital tone has nothing
/// outside its own bins for any step to find). Noise is incompressible, so the
/// quiet noise is held to the full-scale noise's own fidelity rather than a
/// fixed bar.
///
/// The rate loop's step search bottoms out at the +-127 step range (36 dB
/// residue range + 12-point floor): once a quiet source's codable residue is
/// exhausted, no finer step can spend more bits without a second (cascade)
/// coding pass, which does not exist yet -- that cascade is the upgrade path.
/// So the contract is: measured kbps lands within +-25% of target, OR the
/// encode undershoots because there is nothing left to spend and the result
/// is transparent (corr >= 0.999 against the source). The monotone-in-target
/// spend check only applies where the case actually spends against target.
#[test]
fn quiet_content_reaches_the_target_or_is_transparent() {
    let rate = 48_000u32;
    let samples = rate as usize * 3;
    let fixture = ffmpeg_decode(&fixtures().join("audio/wav16-stereo-48000.wav"))
        .expect("fixture decodes");
    assert_eq!(fixture.1, rate);
    let attenuate = |mut source: Vec<Vec<f32>>| {
        for channel in source.iter_mut() {
            for value in channel.iter_mut() {
                *value *= 0.01;
            }
        }
        source
    };
    let corr_at = |path: &Path, source: &[Vec<f32>]| {
        let (decoded, _) = our_decode(path);
        decoded
            .iter()
            .zip(source.iter())
            .map(|(channel, original)| {
                let n = channel.len().min(original.len());
                correlation(&channel[..n], &original[..n])
            })
            .fold(1.0f64, f64::min)
    };
    let loud = busy(2, rate, samples);
    let loud_corr = corr_at(&encode_to_file(&loud, rate, 128_000, "loud-128k.ogg"), &loud);
    let quiet_noise = attenuate(loud);
    let mut quiet_tones = attenuate(tones(2, rate, samples));
    for (channel, noise) in quiet_tones.iter_mut().zip(busy(2, rate, samples)) {
        for (value, n) in channel.iter_mut().zip(noise) {
            *value += n * 0.0001;
        }
    }
    let cases = [
        ("fixture", &fixture.0, 0.99),
        ("quiet-tones", &quiet_tones, 0.99),
        ("quiet-noise", &quiet_noise, loud_corr - 0.02),
    ];
    for (name, source, bar) in cases {
        let mut measured = Vec::new();
        let mut spent = Vec::new();
        for target in [96_000i32, 128_000, 192_000] {
            let path = encode_to_file(source, rate, target, &format!("{name}-{}k.ogg", target / 1000));
            let bytes = std::fs::metadata(&path).expect("file").len();
            let kbps = bytes as f64 * 8.0 * f64::from(rate) / source[0].len() as f64 / 1000.0;
            let corr = corr_at(&path, source);
            println!("{name}: target {} kbps -> {kbps:.0} kbps, corr {corr:.4}", target / 1000);
            let target_kbps = f64::from(target) / 1000.0;
            let within_25pct = (kbps - target_kbps).abs() / target_kbps < 0.25;
            let transparent_undershoot = kbps < target_kbps && corr >= 0.999;
            assert!(
                within_25pct || transparent_undershoot,
                "{name}: {kbps:.0} kbps for {target_kbps:.0} kbps target, corr {corr:.4}"
            );
            spent.push(within_25pct);
            measured.push(kbps);
            if target == 128_000 {
                assert!(corr >= bar, "{name}: corr {corr:.4} under {bar:.4}");
            }
        }
        if spent.iter().all(|&s| s) {
            assert!(measured[0] < measured[1] && measured[1] < measured[2], "{name}: {measured:?}");
        }
    }
}

/// Per-Bark-band quantised-residue histogram comparison: ours vs libvorbis
/// reference.  We encode the source with our encoder (capturing residue), then
/// encode the same source with libvorbis and decode that stream with our own
/// decoder, capturing its decoded residue (after inverse coupling, before
/// floor multiply — the same domain as the encoder's `quantised`).  The
/// reference is libvorbis's own residue, so per-band divergence names where
/// our psy spends differently from libvorbis.
#[test]
#[ignore = "slow: encodes 7 sources × 128k, needs ffmpeg/libvorbis"]
fn residue_histogram_vs_reference() {
    use std::io::Write;

    let sources: &[(&str, &str)] = &[
        ("nik", "~/Music/Yok - Nikbinler.mp4"),
        ("zaur", "~/Music/Zaur Xan- Dusun Meni.mp3"),
        ("her", "~/Music/Her Nerdeysen.mp3"),
        ("naz", "~/Music/naz_aglama_ben_aglarim.mp4"),
        ("sadie", "~/Music/sadie.wav"),
        ("dl8a", "~/Downloads/8a3b6d1d19.mp3"),
        ("hein", "~/Downloads/Sadie Sink Talks Her Little Known Singing Skills, Stranger Things 5 and Brendan Fraser.mp3"),
    ];
    let bitrate = 128_000i32;
    let rate = 48_000u32;
    let channels = 2u16;
    // Limit to ~12 s for speed — enough blocks for a stable histogram.
    let max_samples = rate as usize * 12;

    let out_dir = scratch().join("vorbis7");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let mut report = String::new();
    report.push_str("# Residue histogram: ours vs libvorbis reference\n\n");
    report.push_str(&format!("Bitrate: {} kbps, {} Hz, {}ch, {} s max\n\n",
        bitrate / 1000, rate, channels, max_samples / rate as usize));
    let mut total_ours_bytes = 0u64;
    let mut total_ref_bytes = 0u64;

    for &(name, src_path) in sources {
        let expanded = shellexpand(src_path);
        let src = PathBuf::from(&expanded);
        if !src.exists() {
            report.push_str(&format!("## {name}: SKIP (file not found: {expanded})\n\n"));
            continue;
        }
        // Decode source with ffmpeg, limit duration.
        let Some((source_pcm, src_rate)) = ffmpeg_decode_limited(&src, rate, max_samples) else {
            report.push_str(&format!("## {name}: SKIP (ffmpeg decode failed)\n\n"));
            continue;
        };
        if source_pcm.len() < 2 || source_pcm[0].is_empty() {
            report.push_str(&format!("## {name}: SKIP (empty decode)\n\n"));
            continue;
        }
        // Resample to 48k if needed — ffmpeg_decode_limited already targets 48k.
        let _ = src_rate;

        // --- Ours: encode source with our encoder, capture residue, keep the ogg ---
        let ours_ogg = out_dir.join(format!("ours-{name}-128k.ogg"));
        let (ours_residue, ours_bytes) =
            encode_and_capture(&source_pcm, rate, channels, bitrate, &ours_ogg);

        // --- Sanity: decoding our own ogg with capture on must reproduce the
        // encoder's capture band for band, or the two captures are not in the
        // same domain and the table below means nothing. ---
        let (roundtrip, ours_split) = decode_capture_with_bits(&ours_ogg);
        let (sanity_ok, sanity_detail) = compare_captures(&ours_residue, &roundtrip, rate);
        let ours_bits = bit_split_summary(&ours_split, &roundtrip);

        // --- Reference: encode with libvorbis, decode with OUR decoder, capture ---
        let ref_ogg = out_dir.join(format!("ref-{name}-128k.ogg"));
        let ref_status = Command::new("ffmpeg")
            // `-vn`: with the video stream still attached (naz, h264+aac mp4)
            // ffmpeg's libvorbis wrapper ignores -b:a and encodes at ~3.2x the
            // target; audio-only input encodes at the requested rate.
            .args(["-y", "-v", "error", "-i"])
            .arg(&src)
            .args(["-vn", "-t", &format!("{}", max_samples / rate as usize),
                   "-ac", "2", "-ar", "48000",
                   "-c:a", "libvorbis", "-b:a", "128k"])
            .arg(&ref_ogg)
            .status();
        let (ref_residue, ref_split) = match ref_status {
            Ok(s) if s.success() => decode_capture_with_bits(&ref_ogg),
            _ => {
                report.push_str(&format!("## {name}: libvorbis encode failed\n\n"));
                continue;
            }
        };
        let ref_bytes = std::fs::metadata(&ref_ogg).expect("ref ogg").len();
        total_ours_bytes += ours_bytes;
        total_ref_bytes += ref_bytes;

        // --- Build per-Bark-band histograms ---
        let bins = 6; // |q|: 0, 1, 2, 3-4, 5-8, 9+
        let max_bark = 25;
        let mut ours_hist = vec![vec![0u64; bins]; max_bark];
        let mut ref_hist = vec![vec![0u64; bins]; max_bark];
        for (half, quantised) in &ours_residue {
            accumulate(quantised, *half, rate, &mut ours_hist, max_bark);
        }
        for (half, quantised) in &ref_residue {
            accumulate(quantised, *half, rate, &mut ref_hist, max_bark);
        }

        report.push_str(&format!(
            "## {name} — sanity {}: {} — ours {} B vs ref {} B ({:.2}x)\n\n",
            if sanity_ok { "PASS" } else { "FAIL" },
            sanity_detail,
            ours_bytes,
            ref_bytes,
            ours_bytes as f64 / ref_bytes as f64
        ));
        let ref_bits = bit_split_summary(&ref_split, &ref_residue);
        for (who, b) in [("ours", ours_bits), ("ref", ref_bits)] {
            let (f, r, p, nz) = b;
            report.push_str(&format!(
                "bits {who}: floor {f} ({:.1}%), residue {r} ({:.1}%), other {}, packet {p}; non-zero {nz}, residue bits/non-zero {:.2}\n",
                100.0 * f as f64 / p.max(1) as f64,
                100.0 * r as f64 / p.max(1) as f64,
                p.saturating_sub(f + r),
                r as f64 / nz.max(1) as f64
            ));
        }
        report.push('\n');
        report.push_str("| Bark | q=0 ours/ref | q=1 | q=2 | q=3-4 | q=5-8 | q=9+ | total ours/ref | spend ratio |\n");
        report.push_str("|------|-------------|-----|-----|-------|-------|------|-----------------|-------------|\n");
        for band in 0..max_bark {
            let o = &ours_hist[band];
            let r = &ref_hist[band];
            let o_total: u64 = o.iter().sum();
            let r_total: u64 = r.iter().sum();
            if o_total == 0 && r_total == 0 {
                continue;
            }
            // "Spend" = non-zero entries (bins 1..) — proxy for bits spent.
            let o_spend: u64 = o[1..].iter().sum();
            let r_spend: u64 = r[1..].iter().sum();
            let ratio = if r_spend > 0 {
                o_spend as f64 / r_spend as f64
            } else { f64::INFINITY };
            report.push_str(&format!(
                "| {} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} | {:.2} |\n",
                band, o[0], r[0], o[1], r[1], o[2], r[2], o[3], r[3], o[4], r[4], o[5], r[5],
                o_total, r_total, ratio,
            ));
        }

        // Fraction table: per-band share of entries in each |q| class and the
        // shape distance = sum |f_ours - f_ref| over the four classes
        // (0 = identical spend shape; convergence target < 0.15 in Bark 3-22).
        report.push_str("| Bark | f0 ours | f0 ref | f1 ours | f1 ref | f2 ours | f2 ref | f3+ ours | f3+ ref | dist |\n");
        report.push_str("|------|---------|--------|---------|--------|---------|--------|----------|---------|------|\n");
        let frac = |h: &[u64], total: u64| {
            vec![
                h[0] as f64 / total as f64,
                h[1] as f64 / total as f64,
                h[2] as f64 / total as f64,
                (h[3] + h[4] + h[5]) as f64 / total as f64,
            ]
        };
        let mut band_dist: Vec<(usize, f64)> = Vec::new();
        for band in 0..max_bark {
            let o_total: u64 = ours_hist[band].iter().sum();
            let r_total: u64 = ref_hist[band].iter().sum();
            if o_total == 0 || r_total == 0 {
                continue;
            }
            let fo = frac(&ours_hist[band], o_total);
            let fr = frac(&ref_hist[band], r_total);
            let dist: f64 = fo.iter().zip(&fr).map(|(a, b)| (a - b).abs()).sum();
            band_dist.push((band, dist));
            report.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                band, fo[0], fr[0], fo[1], fr[1], fo[2], fr[2], fo[3], fr[3], dist,
            ));
        }
        let in_scope: Vec<(usize, f64)> = band_dist
            .iter()
            .filter(|(b, _)| (3..=22).contains(b))
            .cloned()
            .collect();
        if !in_scope.is_empty() {
            let max_d = in_scope.iter().map(|(_, d)| *d).fold(f64::MIN, f64::max);
            let over = in_scope.iter().filter(|(_, d)| *d >= 0.15).count();
            report.push_str(&format!(
                "\nshape Bark 3-22: max dist {max_d:.3}, bands >= 0.15: {over}/{} — {}\n",
                in_scope.len(),
                if over == 0 { "SHAPE PASS" } else { "shape fail" },
            ));
        }
        report.push_str("\n");
    }

    report.push_str(&format!(
        "\nTotal bytes: ours {total_ours_bytes} vs ref {total_ref_bytes} ({:.2}x)\n",
        total_ours_bytes as f64 / total_ref_bytes as f64,
    ));
    let report_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../lanes/vorbis-psy-r1.histogram.txt");
    let mut f = File::create(&report_path).expect("create report");
    f.write_all(report.as_bytes()).expect("write report");
    println!("Histogram report: {}", report_path.display());
    // Also print to stdout for immediate viewing.
    print!("{report}");
}

/// Encode PCM with our encoder, capturing per-block quantised residue, and mux
/// the result to `out` so it can be decoded back (sanity check + rate).
fn encode_and_capture(
    source: &[Vec<f32>],
    rate: u32,
    channels: u16,
    bitrate: i32,
    out: &Path,
) -> (Vec<(usize, Vec<Vec<i32>>)>, u64) {
    let mut encoder = VorbisEncoder::new(EncoderConfig {
        sample_rate: rate,
        channels,
        bitrate_bps: bitrate,
        quality: 0.6,
    })
    .expect("encoder");
    encoder.enable_residue_capture();
    let borrowed: Vec<&[f32]> = source.iter().map(|c| &c[..]).collect();
    encoder.push_planar(&borrowed).expect("push");
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push((packet.data, packet.granule)),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("encode: {e}"),
        }
    }
    mux(out, &encoder, &packets, rate, channels);
    let bytes = std::fs::metadata(out).expect("muxed file").len();
    (encoder.take_residue_capture(), bytes)
}

/// Decode with ours, capturing per-block residue (after inverse coupling,
/// before floor multiply) — the decoder-side view of the same domain the
/// encoder captures.
fn decode_capture(path: &Path) -> Vec<(usize, Vec<Vec<i32>>)> {
    decode_capture_with_bits(path).0
}

/// Bit split summed over the stream: (floor bits, residue bits, packet bits,
/// non-zero residue count) — bits per coded value is the coding-efficiency oracle.
fn bit_split_summary(split: &[(u64, u64, u64)], residue: &[(usize, Vec<Vec<i32>>)]) -> (u64, u64, u64, u64) {
    let (mut f, mut r, mut p) = (0u64, 0u64, 0u64);
    for &(a, b, c) in split {
        f += a;
        r += b;
        p += c;
    }
    let nz: u64 = residue
        .iter()
        .map(|(_, chs)| chs.iter().map(|c| c.iter().filter(|&&v| v != 0).count() as u64).sum::<u64>())
        .sum();
    (f, r, p, nz)
}

fn decode_capture_with_bits(path: &Path) -> (Vec<(usize, Vec<Vec<i32>>)>, Vec<(u64, u64, u64)>) {
    let mut demuxer = OggDemuxer::open(File::open(path).expect("open")).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::CodecId::Vorbis)
        .expect("a Vorbis stream")
        .clone();
    let extradata = stream.params.extradata.clone().expect("headers");
    let mut decoder = VorbisDecoder::from_extradata(&extradata).expect("headers parse");
    decoder.enable_residue_capture();
    loop {
        let packet = match demuxer.next_packet() {
            Ok(packet) => packet,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux {}: {e}", path.display()),
        };
        if packet.stream != stream.index {
            continue;
        }
        // A packet the decoder cannot use is skipped, like in `our_decode`.
        if decoder.decode_audio(&packet.data).is_err() {
            continue;
        }
    }
    let residue = decoder.take_residue_capture();
    let bits = decoder.take_bit_split();
    (residue, bits)
}

/// Bin two captures by Bark and compare every histogram cell.
fn compare_captures(
    a: &[(usize, Vec<Vec<i32>>)],
    b: &[(usize, Vec<Vec<i32>>)],
    rate: u32,
) -> (bool, String) {
    let bins = 6;
    let max_bark = 25;
    let mut ha = vec![vec![0u64; bins]; max_bark];
    let mut hb = vec![vec![0u64; bins]; max_bark];
    for (half, q) in a {
        accumulate(q, *half, rate, &mut ha, max_bark);
    }
    for (half, q) in b {
        accumulate(q, *half, rate, &mut hb, max_bark);
    }
    let mut diffs = Vec::new();
    for band in 0..max_bark {
        for bucket in 0..bins {
            if ha[band][bucket] != hb[band][bucket] {
                diffs.push(format!(
                    "bark {band} q{bucket}: {} vs {}",
                    ha[band][bucket], hb[band][bucket]
                ));
            }
        }
    }
    if diffs.is_empty() {
        (true, format!("identical histograms ({} vs {} blocks)", a.len(), b.len()))
    } else {
        (false, format!(
            "{} vs {} blocks; first diffs: {}",
            a.len(),
            b.len(),
            diffs.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
        ))
    }
}

/// Decode with ffmpeg into planar f32, resampled to `rate`, limited to `max_samples`.
fn ffmpeg_decode_limited(path: &Path, rate: u32, max_samples: usize) -> Option<(Vec<Vec<f32>>, u32)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-t", &format!("{}", max_samples / rate as usize),
               "-ac", "2", "-ar", &rate.to_string(),
               "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    if !out.stderr.is_empty() {
        eprintln!("ffmpeg stderr: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let channels = 2usize;
    let interleaved: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let n = interleaved.len() / channels;
    let n = n.min(max_samples);
    let mut planes = vec![Vec::with_capacity(n); channels];
    for (i, value) in interleaved.into_iter().enumerate().take(n * channels) {
        planes[i % channels].push(value);
    }
    Some((planes, rate))
}

/// Accumulate per-Bark-band |q| histograms from quantised residue.
fn accumulate(
    quantised: &[Vec<i32>],
    half: usize,
    rate: u32,
    hist: &mut [Vec<u64>],
    max_bark: usize,
) {
    for channel in quantised {
        for (bin, &q) in channel.iter().enumerate() {
            if bin >= half {
                break;
            }
            let hz = bin as f64 * f64::from(rate) / (2.0 * half as f64);
            let bark = VorbisEncoder::bark_hz(hz) as usize;
            let band = bark.min(max_bark - 1);
            let bucket = match q.abs() {
                0 => 0,
                1 => 1,
                2 => 2,
                3..=4 => 3,
                5..=8 => 4,
                _ => 5,
            };
            hist[band][bucket] += 1;
        }
    }
}

/// Expand `~` in a path string.
fn shellexpand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
        format!("{home}/{rest}")
    } else {
        path.to_string()
    }
}


/// Full-file sweep over the user's library at the two managed rates: ours
/// encoded at the size libvorbis actually produced (ffmpeg's -b:a is
/// unmanaged VBR, 10-30% under nominal on real music), bytes within ±3% of
/// it and correlation-to-source within .005 of it. Both encodes decode
/// through our decoder so the gap measures the encoders, not the decoders.
/// Writes `lanes/vorbis-psy-r1.sweep.txt`.
#[test]
#[ignore = "slow: encodes 7 full sources × 2 rates, needs ffmpeg/libvorbis"]
fn real_library_sweep_vs_reference() {
    let sources: &[(&str, &str)] = &[
        ("nik", "~/Music/Yok - Nikbinler.mp4"),
        ("zaur", "~/Music/Zaur Xan- Dusun Meni.mp3"),
        ("her", "~/Music/Her Nerdeysen.mp3"),
        ("naz", "~/Music/naz_aglama_ben_aglarim.mp4"),
        ("sadie", "~/Music/sadie.wav"),
        ("dl8a", "~/Downloads/8a3b6d1d19.mp3"),
        ("hein", "~/Downloads/Sadie Sink Talks Her Little Known Singing Skills, Stranger Things 5 and Brendan Fraser.mp3"),
    ];
    let rate = 48_000u32;
    let max_samples = rate as usize * 600;
    let out_dir = scratch().join("vorbis7-sweep");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let mut table = String::from("source  kbps   ours_kbps ref_kbps rate%   corr_ours corr_ref gap     minsec_o minsec_r drops verdict\n");
    let mut failures = Vec::new();
    let only = std::env::var("SWEEP_ONLY").ok();
    for &(name, src_path) in sources {
        if only.as_deref().is_some_and(|o| !o.split(',').any(|n| n == name)) {
            continue;
        }
        let src = PathBuf::from(shellexpand(src_path));
        if !src.exists() {
            table.push_str(&format!("{name:<7} -      SKIP (missing)\n"));
            continue;
        }
        let Some((source_pcm, _)) = ffmpeg_decode_limited(&src, rate, max_samples) else {
            table.push_str(&format!("{name:<7} -      SKIP (decode)\n"));
            continue;
        };
        let seconds = source_pcm[0].len() as f64 / f64::from(rate);
        for &kbps in &[96i32, 128] {
            // libvorbis under ffmpeg's -b:a runs unmanaged VBR and lands
            // 10-30% under the nominal rate on real music, so ours is asked
            // for the bytes it actually produced: quality at equal size.
            let reference = out_dir.join(format!("ref-{name}-{kbps}k.ogg"));
            let status = Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(&src)
                .args(["-vn", "-t", "600", "-ac", "2", "-ar", "48000", "-c:a", "libvorbis", "-b:a", &format!("{kbps}k")])
                .arg(&reference)
                .status()
                .expect("ffmpeg runs");
            assert!(status.success(), "libvorbis encode of {name} at {kbps}k");
            let bytes = |p: &Path| std::fs::metadata(p).expect("size").len() as f64;
            let ref_kbps = bytes(&reference) * 8.0 / seconds / 1000.0;
            let ours = encode_to_file(&source_pcm, rate, (ref_kbps * 1000.0).round() as i32, &format!("vorbis7-sweep/ours-{name}-{kbps}k.ogg"));
            let ours_kbps = bytes(&ours) * 8.0 / seconds / 1000.0;
            let rate_pct = (ours_kbps / ref_kbps - 1.0) * 100.0;
            // Whole-file corr plus a per-second trace: a dropout (the
            // rate-loop windup class, 200 ms silences after transients) is
            // invisible in the mean and bimodal per second, so it is gated
            // as "seconds where ours is under 0.9 and the reference is not".
            let corr_of = |p: &Path| -> (f64, Vec<f64>) {
                let (pcm, _) = our_decode(p);
                let n = pcm[0].len().min(source_pcm[0].len());
                let mean = (0..2).map(|c| correlation(&pcm[c][..n], &source_pcm[c][..n])).sum::<f64>() / 2.0;
                let step = rate as usize;
                let per_second = (0..n / step)
                    .map(|s| {
                        let r = s * step..(s + 1) * step;
                        (0..2).map(|c| correlation(&pcm[c][r.clone()], &source_pcm[c][r.clone()])).sum::<f64>() / 2.0
                    })
                    .collect();
                (mean, per_second)
            };
            let (corr_ours, sec_ours) = corr_of(&ours);
            let (corr_ref, sec_ref) = corr_of(&reference);
            let gap = corr_ref - corr_ours;
            let min_of = |v: &[f64]| v.iter().copied().fold(1.0f64, f64::min);
            let (min_ours, min_ref) = (min_of(&sec_ours), min_of(&sec_ref));
            let dropouts = sec_ours
                .iter()
                .zip(&sec_ref)
                .filter(|(o, r)| **o < 0.9 && **r >= 0.9)
                .count();
            let pass = rate_pct.abs() <= 3.0 && gap <= 0.005 && dropouts == 0;
            if !pass {
                failures.push(format!("{name}@{kbps}k rate {rate_pct:+.2}% gap {gap:.4} dropouts {dropouts}"));
            }
            table.push_str(&format!(
                "{name:<7} {kbps:<6} {ours_kbps:<9.1} {ref_kbps:<8.1} {rate_pct:+6.2} {corr_ours:<9.4} {corr_ref:<8.4} {gap:<7.4} {min_ours:<8.3} {min_ref:<7.3} {dropouts:<5} {}\n",
                if pass { "PASS" } else { "FAIL" }
            ));
            eprintln!("{}", table.lines().last().unwrap());
        }
    }
    eprintln!("\n{table}");
    let _ = std::fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lanes/vorbis-psy-r1.sweep.txt"),
        &table,
    );
    assert!(failures.is_empty(), "sweep failures: {failures:?}");
}

/// In-place radix-2 FFT.  Only magnitudes and differences of identically
/// transformed spectra are used, so the twiddle sign convention is irrelevant.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two());
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = ((i as u32).reverse_bits() >> (32 - bits)) as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let angle = step * k as f64;
                let (wr, wi) = (angle.cos(), angle.sin());
                let i = start + k;
                let j = i + half;
                let tr = re[j] * wr - im[j] * wi;
                let ti = re[j] * wi + im[j] * wr;
                re[j] = re[i] - tr;
                im[j] = im[i] - ti;
                re[i] += tr;
                im[i] += ti;
            }
        }
        len *= 2;
    }
}

/// Per-Bark-band spectral error, ours vs libvorbis, both decoded through our
/// decoder (so the gap measures encoders, not decoders): every 2048-sample
/// Hann frame (hop 1024) of the 48 kHz source, band edges in Hz (25 edges →
/// 24 bands), energies summed over frames and both channels.
/// NSR = 10log10(Σ|X_sig−X_src|²/Σ|X_src|²), E = 10log10(Σ|X_sig|²/Σ|X_src|²).
/// The same pass records the bit split of both streams (the decode-side
/// capture `decode_capture_with_bits` provides), so the bits table covers the
/// same rows without a second encode pass.  Writes
/// `lanes/vorbis-psy-r1.bands.txt` and `lanes/vorbis-psy-r1.bits.txt`.
#[test]
#[ignore = "slow: encodes 7 full sources × 2 rates, needs ffmpeg/libvorbis"]
fn band_error_vs_reference() {
    use std::io::Write;

    let sources: &[(&str, &str)] = &[
        ("nik", "~/Music/Yok - Nikbinler.mp4"),
        ("zaur", "~/Music/Zaur Xan- Dusun Meni.mp3"),
        ("her", "~/Music/Her Nerdeysen.mp3"),
        ("naz", "~/Music/naz_aglama_ben_aglarim.mp4"),
        ("sadie", "~/Music/sadie.wav"),
        ("dl8a", "~/Downloads/8a3b6d1d19.mp3"),
        ("hein", "~/Downloads/Sadie Sink Talks Her Little Known Singing Skills, Stranger Things 5 and Brendan Fraser.mp3"),
    ];
    let rate = 48_000u32;
    let max_samples = rate as usize * 600;
    let out_dir = scratch().join("vorbis7-bands");
    std::fs::create_dir_all(&out_dir).expect("out dir");

    const FRAME: usize = 2048;
    const HOP: usize = 1024;
    const EDGES: [f64; 25] = [
        100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0, 1720.0,
        2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0, 12000.0,
        15500.0, 24000.0,
    ];
    let band_of: Vec<Option<usize>> = (0..=FRAME / 2)
        .map(|bin| {
            let hz = bin as f64 * f64::from(rate) / FRAME as f64;
            EDGES.windows(2).position(|w| hz >= w[0] && hz < w[1])
        })
        .collect();
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();

    let mut bands = String::from("# Band error: ours vs libvorbis, both via our decoder\n\n");
    let mut bits = String::from("# Bit split over full sources, same rows as the band error\n\n");
    let only = std::env::var("SWEEP_ONLY").ok();
    for &(name, src_path) in sources {
        if only.as_deref().is_some_and(|o| !o.split(',').any(|n| n == name)) {
            continue;
        }
        let src = PathBuf::from(shellexpand(src_path));
        if !src.exists() {
            bands.push_str(&format!("## {name}: SKIP (missing)\n\n"));
            continue;
        }
        let Some((source_pcm, _)) = ffmpeg_decode_limited(&src, rate, max_samples) else {
            bands.push_str(&format!("## {name}: SKIP (decode)\n\n"));
            continue;
        };
        let seconds = source_pcm[0].len() as f64 / f64::from(rate);
        let bytes = |p: &Path| std::fs::metadata(p).expect("size").len() as f64;
        for &kbps in &[96i32, 128] {
            let t0 = std::time::Instant::now();
            let reference = out_dir.join(format!("ref-{name}-{kbps}k.ogg"));
            let status = Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(&src)
                .args(["-vn", "-t", "600", "-ac", "2", "-ar", "48000", "-c:a", "libvorbis", "-b:a", &format!("{kbps}k")])
                .arg(&reference)
                .status()
                .expect("ffmpeg runs");
            assert!(status.success(), "libvorbis encode of {name} at {kbps}k");
            let ref_kbps = bytes(&reference) * 8.0 / seconds / 1000.0;
            // Ours at the size libvorbis actually produced, exactly as the sweep.
            let ours = encode_to_file(
                &source_pcm,
                rate,
                (ref_kbps * 1000.0).round() as i32,
                &format!("vorbis7-bands/ours-{name}-{kbps}k.ogg"),
            );
            let ours_kbps = bytes(&ours) * 8.0 / seconds / 1000.0;

            // PCM through our decoder for the band error; bit split from the
            // same file's residue-domain capture (decode twice — the capture
            // path returns no PCM).
            let (ours_pcm, ours_split) = {
                let (residue, split) = decode_capture_with_bits(&ours);
                (our_decode(&ours).0, bit_split_summary(&split, &residue))
            };
            let (ref_pcm, ref_split) = {
                let (residue, split) = decode_capture_with_bits(&reference);
                (our_decode(&reference).0, bit_split_summary(&split, &residue))
            };

            // Bit-split block: (floor, residue, packet, non-zero) per stream.
            let (of, ores, op, onz) = ours_split;
            let (rf, rres, rp, rnz) = ref_split;
            bits.push_str(&format!(
                "## {name} @ {kbps}k (ours {ours_kbps:.1} vs ref {ref_kbps:.1} kbps)\n"
            ));
            for (who, f, r, p, nz) in [("ours", of, ores, op, onz), ("ref ", rf, rres, rp, rnz)] {
                bits.push_str(&format!(
                    "{who} floor {fb:>10.1}/s ({fpc:5.1}%)  residue {rb:>10.1}/s ({rpc:5.1}%)  other {ob:>8.1}/s  nz {nz:>10}  bits/nz {bnz:.2}\n",
                    fb = f as f64 / seconds,
                    fpc = 100.0 * f as f64 / p.max(1) as f64,
                    rb = r as f64 / seconds,
                    rpc = 100.0 * r as f64 / p.max(1) as f64,
                    ob = p.saturating_sub(f + r) as f64 / seconds,
                    bnz = r as f64 / nz.max(1) as f64,
                ));
            }
            bits.push_str(&format!(
                "ratio floor {:.3}  residue {:.3}  bits/nz {:.3}\n\n",
                of as f64 / rf.max(1) as f64,
                ores as f64 / rres.max(1) as f64,
                (ores as f64 / onz.max(1) as f64) / (rres as f64 / rnz.max(1) as f64),
            ));

            // Band error: align to the source exactly as the sweep does —
            // common length from sample 0 (pre-roll trim inside `our_decode`).
            let n = source_pcm[0]
                .len()
                .min(ours_pcm[0].len())
                .min(ref_pcm[0].len());
            let nb = EDGES.len() - 1;
            let (mut e_src, mut e_ours, mut e_ref) =
                (vec![0f64; nb], vec![0f64; nb], vec![0f64; nb]);
            let (mut err_ours, mut err_ref) = (vec![0f64; nb], vec![0f64; nb]);
            let mut spec: Vec<(Vec<f64>, Vec<f64>)> =
                (0..3).map(|_| (vec![0.0; FRAME], vec![0.0; FRAME])).collect();
            let mut start = 0;
            while start + FRAME <= n {
                for ch in 0..2 {
                    for (sig, pcm) in [&source_pcm, &ours_pcm, &ref_pcm].iter().enumerate() {
                        let s = &mut spec[sig];
                        for i in 0..FRAME {
                            s.0[i] = f64::from(pcm[ch][start + i]) * window[i];
                            s.1[i] = 0.0;
                        }
                        fft(&mut s.0, &mut s.1);
                    }
                    for k in 0..=FRAME / 2 {
                        let Some(b) = band_of[k] else { continue };
                        let (sr, si) = (spec[0].0[k], spec[0].1[k]);
                        e_src[b] += sr * sr + si * si;
                        let (ur, ui) = (spec[1].0[k], spec[1].1[k]);
                        e_ours[b] += ur * ur + ui * ui;
                        let (dr, di) = (ur - sr, ui - si);
                        err_ours[b] += dr * dr + di * di;
                        let (vr, vi) = (spec[2].0[k], spec[2].1[k]);
                        e_ref[b] += vr * vr + vi * vi;
                        let (er, ei) = (vr - sr, vi - si);
                        err_ref[b] += er * er + ei * ei;
                    }
                }
                start += HOP;
            }
            let db = |e: f64, s: f64| if s > 0.0 { 10.0 * (e / s).log10() } else { f64::NAN };
            bands.push_str(&format!(
                "## {name} @ {kbps}k (ours {ours_kbps:.1} vs ref {ref_kbps:.1} kbps)\n"
            ));
            bands.push_str("band      NSR_ours NSR_ref  dNSR   E_ours  E_ref\n");
            for b in 0..nb {
                if e_src[b] == 0.0 {
                    continue; // no source energy: NSR/E undefined
                }
                let (nsr_o, nsr_r) = (db(err_ours[b], e_src[b]), db(err_ref[b], e_src[b]));
                bands.push_str(&format!(
                    "{:<8}  {:7.1} {:7.1} {:6.1} {:7.1} {:7.1}\n",
                    format!("{}-{}", EDGES[b] as usize, EDGES[b + 1] as usize),
                    nsr_o,
                    nsr_r,
                    nsr_o - nsr_r,
                    db(e_ours[b], e_src[b]),
                    db(e_ref[b], e_src[b]),
                ));
            }
            let corr = |pcm: &[Vec<f32>]| {
                (0..2)
                    .map(|c| correlation(&pcm[c][..n], &source_pcm[c][..n]))
                    .sum::<f64>()
                    / 2.0
            };
            bands.push_str(&format!(
                "broadband corr: ours {:.4} ref {:.4}\n\n",
                corr(&ours_pcm),
                corr(&ref_pcm),
            ));
            eprintln!(
                "{name}@{kbps}k: ours {ours_kbps:.1} ref {ref_kbps:.1} kbps, {:.1} s",
                t0.elapsed().as_secs_f32()
            );
        }
    }
    for (text, file) in [
        (&bands, "vorbis-psy-r1.bands.txt"),
        (&bits, "vorbis-psy-r1.bits.txt"),
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../lanes")
 .join(file);
        let mut f = File::create(&path).expect("create report");
        f.write_all(text.as_bytes()).expect("write report");
        println!("{file}: {}", path.display());
    }
}
