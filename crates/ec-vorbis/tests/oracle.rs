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
