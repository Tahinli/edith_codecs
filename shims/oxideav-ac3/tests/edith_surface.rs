//! The AC-3 path exactly as the replica drives it: read the rate and the
//! layout out of the first syncframe's headers, ask the registry for a decoder
//! folded to that layout, decode, and read the frame back as S16 little-endian
//! (`engine/src/audio.rs:1954-2170`).
//!
//! Missing fixtures skip rather than fail, as the `ec-ac3` matrix does;
//! `scripts/gen-fixtures.sh` writes them.

use std::path::{Path, PathBuf};

use oxideav_ac3::{bsi, eac3, register_codecs, syncinfo};
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Decoder, Error, Frame, Packet, TimeBase};

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name);
    std::fs::read(path).ok()
}

/// `engine/src/audio.rs:2130` — a fresh decoder out of a fresh registry.
fn ac3_decoder(codec: &str, channels: Option<u16>) -> Box<dyn Decoder> {
    let mut registry = CodecRegistry::new();
    register_codecs(&mut registry);
    let mut params = CodecParameters::audio(CodecId::new(codec));
    params.channels = channels;
    registry.first_decoder(&params).expect("no AC-3 decoder")
}

/// `engine/src/audio.rs:2150` — one syncframe in, interleaved `f32` and the
/// frames per channel out; `None` when the decoder wants more input.
fn decode_ac3(decoder: &mut Box<dyn Decoder>, bytes: &[u8]) -> Option<(Vec<f32>, u64)> {
    let packet = Packet::new(0, TimeBase::new(1, 48_000), bytes.to_vec());
    decoder.send_packet(&packet).expect("AC-3 decode failed");
    match decoder.receive_frame() {
        Ok(Frame::Audio(audio)) => Some((
            audio.data[0]
                .chunks_exact(2)
                .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
                .collect(),
            u64::from(audio.samples),
        )),
        Ok(other) => panic!("AC-3 decoder handed back {other:?}"),
        Err(Error::NeedMore) => None,
        Err(e) => panic!("AC-3 decode failed: {e:?}"),
    }
}

/// `engine/src/audio.rs:2061` — rate and `nfchans` out of the bit stream,
/// whichever syntax it is.
fn header(codec: &str, frame: &[u8]) -> (u32, usize) {
    if codec == "eac3" {
        let bsi = eac3::bsi::parse(frame.get(2..).unwrap_or_default()).expect("E-AC-3 bsi");
        (bsi.sample_rate, bsi.nfchans)
    } else {
        let sync = syncinfo::parse(frame).expect("AC-3 syncinfo");
        let bsi = bsi::parse(frame.get(5..).unwrap_or_default()).expect("AC-3 bsi");
        (sync.sample_rate, bsi.nfchans)
    }
}

fn rms(pcm: &[f32]) -> f64 {
    (pcm.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / pcm.len().max(1) as f64).sqrt()
}

/// Every generated layout, opened the way a timeline source is opened: wider
/// than mono folds to stereo, mono stays mono, and neither decodes to silence
/// (the replica's mono `corner-cut` exists because the incumbent did).
#[test]
fn the_replicas_open_path_lands_on_audible_pcm() {
    let cases = [
        ("ac3-5.1-48000.ac3", "ac3", 48_000, 5, 2),
        ("ac3-stereo-48000.ac3", "ac3", 48_000, 2, 2),
        ("ac3-mono-44100.ac3", "ac3", 44_100, 1, 1),
        ("eac3-5.1-48000.eac3", "eac3", 48_000, 5, 2),
        ("eac3-mono-48000.eac3", "eac3", 48_000, 1, 1),
    ];
    let mut ran = 0;
    for (name, codec, rate, nfchans_want, channels_want) in cases {
        let Some(data) = fixture(name) else { continue };
        ran += 1;

        let (sample_rate, nfchans) = header(codec, &data);
        assert_eq!(sample_rate, rate, "{name}: sample rate");
        assert_eq!(nfchans, nfchans_want, "{name}: nfchans");

        let requested = (nfchans > 1).then_some(2);
        let mut decoder = ac3_decoder(codec, requested);
        let (pcm, samples) = decode_ac3(&mut decoder, &data).expect("the first syncframe decoded");
        let channels = (pcm.len() as u64 / samples.max(1)) as u16;
        assert_eq!(channels, channels_want, "{name}: decoded channels");
        assert!(samples > 0, "{name}: no samples");
        assert!(rms(&pcm) > 1e-3, "{name}: decoded to silence, rms {}", rms(&pcm));

        // The replica then feeds the rest of the stream frame by frame; a
        // second frame proves the packet walk does not desync on the shim's
        // conversion.
        let size = ec_ac3::frame_size(&data).expect("frame size");
        if let Some(next) = data.get(size..).filter(|rest| rest.len() > size) {
            let (pcm, samples) = decode_ac3(&mut decoder, next).expect("the second syncframe");
            assert_eq!((pcm.len() as u64 / samples.max(1)) as u16, channels_want);
        }
    }
    assert!(ran > 0, "no AC-3 fixtures: run scripts/gen-fixtures.sh");
}
