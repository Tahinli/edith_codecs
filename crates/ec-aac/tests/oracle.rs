//! Decoder and encoder against ffmpeg, on the generated fixtures and on the
//! real library. Every test that needs a fixture or ffmpeg skips loudly when it
//! is absent rather than passing quietly.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::{AacDecoder, AacEncoder, AacEncoderConfig};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// ffmpeg's decode of a file, as planar-by-channel `f32`.
fn ffmpeg_decode(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    deinterleave(&bytes_to_f32(&out.stdout), channels)
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn deinterleave(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let mut out = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels];
    for (i, &v) in samples.iter().enumerate() {
        out[i % channels].push(v);
    }
    out
}

/// Our decode of an ADTS file, as planar-by-channel `f32`.
fn our_decode(path: &Path) -> (Vec<Vec<f32>>, u32) {
    let (planes, rate, _) = our_decode_with_pns(path);
    (planes, rate)
}

fn our_decode_with_pns(path: &Path) -> (Vec<Vec<f32>>, u32, usize) {
    let data = std::fs::read(path).expect("fixture readable");
    let mut decoder = AacDecoder::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut rate = 0;
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let header = ec_aac::parse_adts(&data[at..]).expect("adts header");
        let end = (at + header.frame_length).min(data.len());
        let frame = decoder.decode(&data[at..end], None).expect("frame decodes");
        rate = frame.sample_rate;
        let ch = usize::from(frame.channels);
        if planes.is_empty() {
            planes = vec![Vec::new(); ch];
        }
        for (i, v) in frame.samples.iter().enumerate() {
            planes[i % ch].push(*v);
        }
        at = end;
    }
    (planes, rate, decoder.pns_aus())
}

/// Pearson correlation over the overlap of two channels.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    assert!(n > 1024, "not enough overlap to compare: {n} samples");
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..n {
        let (x, y) = (f64::from(a[i]) - ma, f64::from(b[i]) - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return if da == db { 1.0 } else { 0.0 };
    }
    num / (da * db).sqrt()
}

fn compare(path: &Path) -> Vec<f64> {
    let (ours, _rate) = our_decode(path);
    assert!(!ours.is_empty(), "no channels decoded from {path:?}");
    let theirs = ffmpeg_decode(path, ours.len());
    ours.iter()
        .zip(&theirs)
        .map(|(a, b)| correlation(a, b))
        .collect()
}

#[test]
fn adts_fixtures_match_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let mut checked = 0;
    for name in [
        "aac-adts-mono-44100.aac",
        "aac-adts-mono-48000.aac",
        "aac-adts-stereo-44100.aac",
        "aac-adts-stereo-48000.aac",
        "aac-adts-5.1-44100.aac",
        "aac-adts-5.1-48000.aac",
    ] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        let corr = compare(&path);
        println!("{name}: {corr:?}");
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "{name} channel {ch} correlation {c:.6} < 0.999"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "no AAC fixtures found: run scripts/gen-fixtures.sh"
    );
}

#[test]
fn mp4_fixtures_match_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let tmp = std::env::temp_dir().join("ec-aac-mp4-oracle");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut checked = 0;
    for name in [
        "aac-mp4-mono-48000.mp4",
        "aac-mp4-stereo-48000.mp4",
        "aac-mp4-5.1-48000.mp4",
    ] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        // The mp4 side of the family is another slice; the elementary stream
        // comes out with a stream copy so this test stays about AAC.
        let adts = tmp.join(format!("{name}.aac"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&path)
            .args(["-c:a", "copy", "-f", "adts"])
            .arg(&adts)
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success(), "remux failed for {name}");
        let corr = compare(&adts);
        println!("{name}: {corr:?}");
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "{name} channel {ch} correlation {c:.6} < 0.999"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no AAC-in-mp4 fixtures found");
}

/// One fixture per §4.6 coding tool ffmpeg's native encoder can force in
/// isolation (plus one with all four together), pinning the PNS/M-S ordering
/// bug found against real OBS screen recordings (repo ledger): PNS must fill
/// its bands before M/S and intensity stereo combine them.
#[test]
fn aac_coding_tools_match_ffmpeg_in_isolation() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let tmp = std::env::temp_dir().join("ec-aac-tools-oracle");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut checked = 0;
    for tool in ["pns", "is", "ms", "tns", "all"] {
        let name = format!("aac-tool-{tool}-mp4-stereo-48000.m4a");
        let path = dir.join(&name);
        if !path.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        let adts = tmp.join(format!("{name}.aac"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&path)
            .args(["-c:a", "copy", "-f", "adts"])
            .arg(&adts)
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success(), "remux failed for {name}");
        let corr = compare(&adts);
        let pns = our_decode_with_pns(&adts).2;
        assert_eq!(
            pns > 0,
            matches!(tool, "pns" | "all"),
            "{name}: pns_aus={pns}"
        );
        println!("{name}: {corr:?}");
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "{name} channel {ch} correlation {c:.6} < 0.999"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no AAC tool-isolation fixtures found");
}

/// The contract every downmix in this family folds: FL, FR, FC, LFE, BL, BR.
/// Each channel carries its own tone, mapped explicitly onto the layout, so a
/// wrong element order shows up as the wrong tone on the wrong channel.
#[test]
fn five_one_comes_out_in_film_order() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let tmp = std::env::temp_dir().join("ec-aac-order");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let src = tmp.join("order.aac");
    // The LFE tone stays low: the reference encoder band-limits that channel,
    // and a 1.5 kHz tone would simply not survive the trip.
    let tones = [300.0, 700.0, 1100.0, 60.0, 1900.0, 2300.0];
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y"]);
    for f in tones {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={f}:duration=2:sample_rate=48000"),
        ]);
    }
    let out = cmd
        .args([
            "-filter_complex",
            "[0:a][1:a][2:a][3:a][4:a][5:a]join=inputs=6:channel_layout=5.1:\
map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-BL|5.0-BR[a]",
            "-map",
            "[a]",
            "-c:a",
            "aac",
            "-b:a",
            "320k",
            "-f",
            "adts",
        ])
        .arg(&src)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "tone build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ours, rate) = our_decode(&src);
    assert_eq!(ours.len(), 6, "5.1 decodes to six channels");
    for (ch, plane) in ours.iter().enumerate() {
        let peak = dominant_frequency(plane, rate);
        let want = tones[ch];
        assert!(
            (peak - want).abs() < 40.0,
            "channel {ch} peaks at {peak:.0} Hz, expected {want:.0} Hz: \
             the element order was not remapped to FL, FR, FC, LFE, BL, BR"
        );
    }
}

/// Crude DFT peak: enough to tell six well-separated tones apart.
fn dominant_frequency(samples: &[f32], rate: u32) -> f64 {
    let n = samples.len().min(16384);
    let start = samples.len() / 3;
    let seg = &samples[start..start + n.min(samples.len() - start)];
    let mut best = (0.0f64, 0.0f64);
    let mut f = 40.0;
    while f < 4000.0 {
        let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in seg.iter().enumerate() {
            re += f64::from(v) * (w * i as f64).cos();
            im += f64::from(v) * (w * i as f64).sin();
        }
        let mag = re * re + im * im;
        if mag > best.1 {
            best = (f, mag);
        }
        f += 5.0;
    }
    best.0
}

#[test]
fn asc_round_trips_every_sample_rate() {
    for (index, rate) in ec_aac::SAMPLE_RATES.iter().enumerate() {
        assert_eq!(ec_aac::sf_index_for_rate(*rate), Some(index as u8));
        assert_eq!(ec_aac::sample_rate_for_index(index as u8), *rate);
        for channels in [1u16, 2, 6, 8] {
            let bytes = ec_aac::audio_specific_config_bytes(*rate, channels);
            let cfg = ec_aac::parse_audio_specific_config(&bytes).expect("asc parses");
            assert_eq!(cfg.sample_rate, *rate, "rate round trip at {rate} Hz");
            assert_eq!(cfg.channels, channels, "channels round trip at {rate} Hz");
            assert_eq!(cfg.object_type, ec_aac::AOT_AAC_LC);
        }
    }
    assert_eq!(ec_aac::sf_index_for_rate(37_000), None);
}

/// A refusal is a claim: this pins the current contract -- plain SBR (no
/// Parametric Stereo) is reconstructed and reports the extension rate; a
/// Parametric Stereo stream still falls back to its core (PS itself is a
/// separate, unimplemented tool); plain AAC-LC reports nothing missing.
#[test]
fn sbr_is_reported_not_silently_upsampled() {
    // AOT 5 (SBR), core index 6 (24 kHz), stereo, extension index 3 (48 kHz),
    // then AOT 2 for the core: 00101 0110 0010 0011 00010.
    let asc = [0x2Bu8, 0x11, 0x88];
    let decoder = AacDecoder::with_config_bytes(&asc).expect("HE-AAC config parses");
    assert_eq!(decoder.sbr_support(), ec_aac::SbrSupport::V1);
    assert_eq!(
        decoder.output_sample_rate(),
        Some(48_000),
        "a reconstructed HE-AAC stream must report the extension rate it now actually produces"
    );

    // Same layout, AOT 29 (PS) instead of 5 (SBR): 11101 0110 0010 0011 00010.
    let ps_asc = [0xEBu8, 0x11, 0x88];
    let ps_decoder = AacDecoder::with_config_bytes(&ps_asc).expect("HE-AAC v2 config parses");
    assert_eq!(ps_decoder.sbr_support(), ec_aac::SbrSupport::CoreOnly);
    assert_eq!(
        ps_decoder.output_sample_rate(),
        Some(24_000),
        "PS is still unreconstructed: must report the core rate it actually produces"
    );

    let plain = AacDecoder::with_config_bytes(&ec_aac::audio_specific_config_bytes(48_000, 2))
        .expect("LC config parses");
    assert_eq!(plain.sbr_support(), ec_aac::SbrSupport::NotSignalled);
}

/// The incumbent encoder's per-channel correlation on these exact fixtures,
/// measured by running the incumbent AAC encoder, 0.5.0, as a black box through the same
/// ffmpeg decode (2026-08-14). This is the bar the replacement has to clear.
const INCUMBENT_BAR: &[(&str, usize, u32, f64)] = &[
    ("mono", 1, 96, 0.9846),
    ("stereo", 2, 128, 0.9743),
    ("stereo", 2, 192, 0.9864),
    ("5.1", 6, 384, 0.9648),
    ("5.1", 6, 448, 0.9611),
];

fn encode_to_adts(pcm: &[f32], channels: u16, rate: u32, kbps: u32) -> Vec<u8> {
    let mut enc = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: kbps * 1000,
        adts: true,
        ..Default::default()
    });
    enc.push_pcm(pcm, channels, rate).expect("pcm accepted");
    enc.finish();
    let mut out = Vec::new();
    while let Ok(p) = enc.next_packet() {
        out.extend_from_slice(&p.data);
    }
    out
}

/// Correlation at the encoder's own delay, which is one frame of lookahead.
fn best_correlation(source: &[f32], decoded: &[f32]) -> f64 {
    [0usize, ec_aac::FRAME_LEN, 2 * ec_aac::FRAME_LEN]
        .into_iter()
        .filter(|lag| decoded.len() > *lag + 1024)
        .map(|lag| correlation(source, &decoded[lag..]))
        .fold(f64::MIN, f64::max)
}

/// Our encode, through ffmpeg, against the incumbent's own numbers on the same
/// lossless fixtures at the same bitrates.
#[test]
fn encoder_beats_the_incumbent_bar() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let tmp = std::env::temp_dir().join("ec-aac-bar");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut checked = 0;
    for (name, channels, kbps, bar) in INCUMBENT_BAR {
        let src = dir.join(format!("flac-{name}-48000.flac"));
        if !src.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        let source = ffmpeg_decode(&src, *channels);
        let mut interleaved = Vec::with_capacity(source[0].len() * channels);
        for i in 0..source[0].len() {
            for plane in &source {
                interleaved.push(plane[i]);
            }
        }
        let adts = encode_to_adts(&interleaved, *channels as u16, 48_000, *kbps);
        let path = tmp.join(format!("{name}-{kbps}k.aac"));
        std::fs::write(&path, &adts).expect("write");
        let rate = adts.len() as f64 * 8.0 * 48_000.0 / source[0].len() as f64 / 1000.0;
        let ours = ffmpeg_decode(&path, *channels);
        let corr: Vec<f64> = source
            .iter()
            .zip(&ours)
            .map(|(a, b)| best_correlation(a, b))
            .collect();
        let worst = corr.iter().copied().fold(f64::MAX, f64::min);
        println!(
            "enc {name} {kbps}k: {rate:.0} kbps actual, worst channel {worst:.4} \
             (incumbent {bar:.4}) per-channel {corr:?}"
        );
        assert!(
            rate <= f64::from(*kbps) * 1.12,
            "{name} {kbps}k overshot its target: {rate:.0} kbps"
        );
        assert!(
            worst >= *bar,
            "{name} {kbps}k worst channel {worst:.4} is under the incumbent's {bar:.4}"
        );
        checked += 1;
    }
    assert!(checked > 0, "no lossless fixtures found");
}

fn tone_pcm(rate: u32, channels: usize, secs: f64) -> Vec<f32> {
    let frames = (f64::from(rate) * secs) as usize;
    let mut pcm = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        for c in 0..channels {
            let f = 220.0 * (c + 1) as f64;
            let v = (t * f * std::f64::consts::TAU).sin() * 0.35
                + (t * f * 2.5 * std::f64::consts::TAU).sin() * 0.15;
            pcm.push(v as f32);
        }
    }
    pcm
}

#[test]
fn encoder_output_decodes_in_ffmpeg_without_warnings() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let tmp = std::env::temp_dir().join("ec-aac-enc");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    for (channels, kbps) in [(1u16, 96u32), (2, 128), (2, 192), (6, 384)] {
        let rate = 48_000;
        let pcm = tone_pcm(rate, usize::from(channels), 1.5);
        let adts = encode_to_adts(&pcm, channels, rate, kbps);
        assert!(!adts.is_empty(), "{channels}ch encode produced nothing");
        let path = tmp.join(format!("enc-{channels}ch-{kbps}k.aac"));
        std::fs::write(&path, &adts).expect("write");
        let out = Command::new("ffmpeg")
            .args(["-v", "warning", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        // ffmpeg says this about every ADTS file, its own included: the
        // container has no duration field. It is not a complaint about us.
        let log = String::from_utf8_lossy(&out.stderr);
        let log: String = log
            .lines()
            .filter(|l| !l.contains("Estimating duration from bitrate"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            out.status.success() && log.trim().is_empty(),
            "ffmpeg complained about our {channels}ch/{kbps}k ADTS:\n{log}"
        );
        // And the sound survives the trip.
        let theirs = ffmpeg_decode(&path, usize::from(channels));
        let ours = deinterleave(&pcm, usize::from(channels));
        for (ch, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            // The encoder's own frame of lookahead delays the output by one
            // frame; line the two up before comparing.
            let corr = correlation(a, &b[ec_aac::FRAME_LEN..]);
            println!("enc {channels}ch {kbps}k channel {ch}: corr {corr:.4}");
            assert!(
                corr >= 0.90,
                "{channels}ch/{kbps}k channel {ch} correlation {corr:.4} < 0.90"
            );
        }
    }
}

/// The derived tables have to be complete prefix codes, and that is checkable
/// without any oracle: a Kraft sum of exactly 1 with the entry count the
/// standard prescribes leaves no room for a transcription slip.
#[test]
fn every_codebook_is_a_complete_prefix_code() {
    let expected = [81usize, 81, 81, 81, 81, 81, 64, 64, 169, 169, 289];
    for (i, cb) in ec_aac::tables::CODEBOOKS.iter().enumerate() {
        assert_eq!(
            cb.codes.len(),
            expected[i],
            "codebook {} entry count",
            i + 1
        );
        let kraft: f64 = cb
            .codes
            .iter()
            .map(|(l, _)| 2f64.powi(-i32::from(*l)))
            .sum();
        assert!(
            (kraft - 1.0).abs() < 1e-12,
            "codebook {} Kraft sum {kraft}",
            i + 1
        );
        assert_prefix_free(cb.codes, &format!("codebook {}", i + 1));
    }
    let sf = ec_aac::tables::SCALEFACTOR_CODES;
    assert_eq!(sf.len(), 121, "scalefactor codebook entry count");
    let kraft: f64 = sf.iter().map(|(l, _)| 2f64.powi(-i32::from(*l))).sum();
    assert!((kraft - 1.0).abs() < 1e-12, "scalefactor Kraft sum {kraft}");
    assert_prefix_free(sf, "scalefactor codebook");
}

fn assert_prefix_free(codes: &[(u8, u32)], what: &str) {
    for (i, &(li, ci)) in codes.iter().enumerate() {
        for &(lj, cj) in codes.iter().skip(i + 1) {
            let short = li.min(lj);
            if (ci >> (li - short)) == (cj >> (lj - short)) {
                panic!("{what}: {ci:b}/{li} and {cj:b}/{lj} share a prefix");
            }
        }
    }
}

/// Band tables must be monotone and land exactly on the window length.
#[test]
fn band_tables_span_the_window() {
    for (i, swb) in ec_aac::tables::SWB_LONG.iter().enumerate() {
        assert_eq!(swb[0], 0, "long index {i}");
        assert_eq!(*swb.last().unwrap(), 1024, "long index {i}");
        assert!(swb.windows(2).all(|w| w[0] < w[1]), "long index {i}");
    }
    for (i, swb) in ec_aac::tables::SWB_SHORT.iter().enumerate() {
        assert_eq!(swb[0], 0, "short index {i}");
        assert_eq!(*swb.last().unwrap(), 128, "short index {i}");
        assert!(swb.windows(2).all(|w| w[0] < w[1]), "short index {i}");
    }
}

/// The AudioSpecificConfig this crate writes is byte-identical to the one the
/// reference muxer puts in an `esds`, which is what "ffmpeg accepts our ASC"
/// means in practice.
#[test]
fn asc_matches_the_one_ffmpeg_writes() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let tmp = std::env::temp_dir().join("ec-aac-esds");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    for (channels, rate) in [(1u16, 48_000u32), (2, 48_000), (2, 44_100), (6, 48_000)] {
        let pcm = tone_pcm(rate, usize::from(channels), 0.4);
        let adts = encode_to_adts(&pcm, channels, rate, 128);
        let src = tmp.join(format!("asc-{channels}-{rate}.aac"));
        std::fs::write(&src, &adts).expect("write");
        let mp4 = tmp.join(format!("asc-{channels}-{rate}.mp4"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&src)
            .args(["-c:a", "copy"])
            .arg(&mp4)
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "remux of our ADTS failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let theirs = esds_asc(&std::fs::read(&mp4).expect("read mp4"))
            .expect("the muxer wrote a DecoderSpecificInfo");
        let ours = ec_aac::audio_specific_config_bytes(rate, channels);
        assert_eq!(
            ours, theirs,
            "{channels}ch/{rate}Hz ASC differs from the reference muxer's"
        );
        // And it parses back to the parameters it went in with.
        let cfg = ec_aac::parse_audio_specific_config(&ours).expect("asc parses");
        assert_eq!((cfg.sample_rate, cfg.channels), (rate, channels));
    }
}

/// The DecoderSpecificInfo payload of the first `esds` box: tag 0x05 inside the
/// DecoderConfigDescriptor, with the usual 7-bit length encoding.
fn esds_asc(mp4: &[u8]) -> Option<Vec<u8>> {
    // Walks the MPEG-4 descriptor tree (tag, BER length, payload) rather than
    // scanning raw bytes for 0x05: a scalar field elsewhere in the tree
    // (e.g. avgBitrate) can legitimately contain that byte value, which a
    // blind byte scan would mistake for the DecSpecificInfoTag.
    let at = mp4.windows(4).position(|w| w == b"esds")?;
    let mut i = at + 4 + 4; // "esds" tag, then the box's version+flags
    while i < mp4.len() {
        let tag = mp4[i];
        i += 1;
        let mut len = 0usize;
        loop {
            let b = *mp4.get(i)?;
            i += 1;
            len = (len << 7) | usize::from(b & 0x7F);
            if b & 0x80 == 0 {
                break;
            }
        }
        match tag {
            0x05 => return mp4.get(i..i + len).map(<[u8]>::to_vec),
            // ES_DescrTag: ES_ID(2) + flags(1), then optional fields the
            // flags select, before its nested descriptors.
            0x03 => {
                let flags = *mp4.get(i + 2)?;
                i += 3;
                if flags & 0x80 != 0 {
                    i += 2; // dependsOn ES_ID
                }
                if flags & 0x40 != 0 {
                    let url_len = usize::from(*mp4.get(i)?);
                    i += 1 + url_len;
                }
                if flags & 0x20 != 0 {
                    i += 2; // OCR ES_ID
                }
            }
            // DecoderConfigDescrTag: objectType(1) + streamType/flags(1) +
            // bufferSizeDB(3) + maxBitrate(4) + avgBitrate(4), then its
            // nested DecSpecificInfo.
            0x04 => i += 13,
            // An opaque leaf descriptor: its whole payload is `len` bytes.
            _ => i += len,
        }
    }
    None
}

/// AAC tracks from the machine's own library, not just generated fixtures: the
/// elementary stream is copied out to ADTS and decoded both ways.
#[test]
fn real_library_tracks_match_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let manifest = fixtures().join("real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("SKIP: no real-library manifest; run scripts/scan-real-library.sh");
        return;
    };
    let tmp = std::env::temp_dir().join("ec-aac-real");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut stereo = 0;
    let mut wide = 0;
    let mut checked = 0;
    for line in text.lines().skip(1) {
        if stereo >= 3 && wide >= 2 {
            break;
        }
        let path = line.split('\t').next().unwrap_or_default();
        if path.is_empty() || !line.to_lowercase().contains("aac") {
            continue;
        }
        let file = Path::new(path);
        if !file.exists() {
            continue;
        }
        let Some((index, channels, profile)) = aac_stream(file) else {
            continue;
        };
        // HE-AAC is reported, not decoded: see `sbr_is_reported_not_silently_upsampled`.
        if profile.contains("HE-AAC") {
            continue;
        }
        if channels <= 2 {
            if stereo >= 3 {
                continue;
            }
            stereo += 1;
        } else {
            if wide >= 2 {
                continue;
            }
            wide += 1;
        }
        let adts = tmp.join(format!("real-{checked}.aac"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-t", "20", "-i"])
            .arg(file)
            .args(["-map", &format!("0:{index}"), "-c:a", "copy", "-f", "adts"])
            .arg(&adts)
            .output()
            .expect("ffmpeg runs");
        if !out.status.success() {
            eprintln!("SKIP {path}: stream copy failed");
            continue;
        }
        let corr = compare(&adts);
        let worst = corr.iter().copied().fold(f64::MAX, f64::min);
        println!(
            "real {channels}ch {profile} worst {worst:.6} -- {}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "channel {ch} correlation {c:.6} < 0.999 for {path}"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 3,
        "wanted at least three library tracks, swept {checked}"
    );
    assert!(wide >= 1, "no multichannel AAC track was swept");
}

/// The first AAC audio stream of a file: `(stream index, channels, profile)`.
fn aac_stream(path: &Path) -> Option<(usize, usize, String)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index,codec_name,channels,profile",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut index, mut channels, mut codec, mut profile) =
        (None, None, String::new(), String::new());
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        match key {
            "index" => {
                if codec == "aac" {
                    return Some((index?, channels?, profile));
                }
                index = value.parse().ok();
                codec.clear();
                profile.clear();
            }
            "codec_name" => codec = value.to_string(),
            "channels" => channels = value.parse().ok(),
            "profile" => profile = value.to_string(),
            _ => {}
        }
    }
    if codec == "aac" {
        return Some((index?, channels?, profile));
    }
    None
}

/// Decode speed on 5.1, the shape a film soundtrack has.
#[test]
fn five_one_decodes_faster_than_realtime() {
    let path = fixtures().join("audio/aac-adts-5.1-48000.aac");
    if !path.exists() {
        eprintln!("SKIP: 5.1 fixture missing");
        return;
    }
    let data = std::fs::read(&path).expect("fixture readable");
    let rounds = 8;
    let start = std::time::Instant::now();
    let mut frames = 0usize;
    let mut rate = 48_000u32;
    for _ in 0..rounds {
        let mut decoder = AacDecoder::new();
        let mut at = 0usize;
        frames = 0;
        while at + 7 <= data.len() {
            let header = ec_aac::parse_adts(&data[at..]).expect("adts header");
            let end = (at + header.frame_length).min(data.len());
            let block = decoder.decode(&data[at..end], None).expect("decodes");
            rate = block.sample_rate;
            frames += block.frames();
            at = end;
        }
    }
    let audio = (frames * rounds) as f64 / f64::from(rate);
    let factor = audio / start.elapsed().as_secs_f64();
    println!(
        "5.1 decode: {factor:.0}x realtime ({} build)",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    if !cfg!(debug_assertions) {
        assert!(
            factor >= 40.0,
            "5.1 decode {factor:.0}x realtime is under 40x"
        );
    }
}

/// The one HE-AAC track in this machine's library, decoded to its AAC-LC core.
/// The claim under test is the honest one: the core comes out, at the core
/// rate, matching what a reference decoder makes of the same core band.
#[test]
fn he_aac_library_track_decodes_to_its_core() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let manifest = fixtures().join("real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("SKIP: no real-library manifest");
        return;
    };
    let tmp = std::env::temp_dir().join("ec-aac-he");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    for line in text.lines().skip(1) {
        let path = line.split('\t').next().unwrap_or_default();
        let file = Path::new(path);
        if path.is_empty() || !file.exists() {
            continue;
        }
        let Some((index, _, profile)) = aac_stream_matching(file, "HE-AAC") else {
            continue;
        };
        let _ = profile;
        let adts = tmp.join("he.aac");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-t", "10", "-i"])
            .arg(file)
            .args(["-map", &format!("0:{index}"), "-c:a", "copy", "-f", "adts"])
            .arg(&adts)
            .output()
            .expect("ffmpeg runs");
        if !out.status.success() {
            continue;
        }
        let (ours, rate) = our_decode(&adts);
        assert!(!ours.is_empty(), "the HE-AAC core decoded to nothing");
        // The reference decode carries SBR, so it runs at twice this rate;
        // resampling it down to the core rate low-passes exactly the band the
        // core carries, which is the part we are claiming to reproduce.
        let core = tmp.join("he-core.f32");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&adts)
            .args(["-ar", &rate.to_string(), "-f", "f32le"])
            .arg(&core)
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "reference decode of the HE-AAC track failed"
        );
        let theirs = deinterleave(
            &bytes_to_f32(&std::fs::read(&core).expect("read")),
            ours.len(),
        );
        // Sample-by-sample comparison would need the reference resampler's own
        // delay; the claim here is about *content*, so it is checked on the
        // long-term band energies, which no delay moves.
        let corr: Vec<f64> = ours
            .iter()
            .zip(&theirs)
            .map(|(a, b)| pearson(&band_energies(a, rate), &band_energies(b, rate)))
            .collect();
        let worst = corr.iter().copied().fold(f64::MAX, f64::min);
        println!("HE-AAC core at {rate} Hz: band-energy match, worst channel {worst:.4} {corr:?}");
        assert!(
            worst >= 0.95,
            "the AAC-LC core of an HE-AAC track should carry the reference's \
             core band; worst channel {worst:.4}"
        );
        return;
    }
    eprintln!("SKIP: no HE-AAC track in the library manifest");
}

/// Pearson correlation with no minimum length: for short feature vectors.
fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let ma = a[..n].iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b[..n].iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (f64::from(a[i]) - ma, f64::from(b[i]) - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return 1.0;
    }
    num / (da * db).sqrt()
}

/// Long-term energy in 32 logarithmic bands: a spectral fingerprint that a
/// resampler's delay cannot shift.
fn band_energies(samples: &[f32], rate: u32) -> Vec<f32> {
    let bands = 32usize;
    let mut out = vec![0.0f64; bands];
    let n = 1024usize;
    let mut blocks = 0usize;
    for chunk in samples.chunks_exact(n).take(400) {
        blocks += 1;
        for (b, slot) in out.iter_mut().enumerate() {
            // Geometric spacing from 100 Hz to the Nyquist limit.
            let lo = 100.0 * (f64::from(rate) / 2.0 / 100.0).powf(b as f64 / bands as f64);
            let hi = 100.0 * (f64::from(rate) / 2.0 / 100.0).powf((b + 1) as f64 / bands as f64);
            let (mut re, mut im) = (0.0f64, 0.0f64);
            let f = (lo * hi).sqrt();
            let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
            for (i, &v) in chunk.iter().enumerate() {
                re += f64::from(v) * (w * i as f64).cos();
                im += f64::from(v) * (w * i as f64).sin();
            }
            *slot += (re * re + im * im).sqrt();
        }
    }
    out.iter()
        .map(|v| ((v / blocks.max(1) as f64) + 1e-9).ln() as f32)
        .collect()
}

fn aac_stream_matching(path: &Path, want: &str) -> Option<(usize, usize, String)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index,codec_name,channels,profile",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut index = None;
    let mut channels = None;
    let mut codec = String::new();
    let mut profile = String::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        match key {
            "index" => {
                if codec == "aac" && profile.contains(want) {
                    return Some((index?, channels?, profile));
                }
                index = value.parse().ok();
                codec.clear();
                profile.clear();
            }
            "codec_name" => codec = value.to_string(),
            "channels" => channels = value.parse().ok(),
            "profile" => profile = value.to_string(),
            _ => {}
        }
    }
    if codec == "aac" && profile.contains(want) {
        return Some((index?, channels?, profile));
    }
    None
}
