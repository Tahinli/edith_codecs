//! The call sites edith actually has, compiled and run against the shim.
//!
//! Each one mirrors a line in the replica: `audio.rs:1731` builds the decoder
//! for a multichannel track from its AudioSpecificConfig, `export.rs:1678`
//! turns a sample rate into a `samplingFrequencyIndex`, and `export.rs:1756`
//! plus `mux.rs:2116` drive the encoder over a mix.

#[test]
fn sf_index_for_rate_covers_the_rates_the_exporter_offers() {
    assert_eq!(rusty_aac::sf_index_for_rate(48_000), Some(3));
    assert_eq!(rusty_aac::sf_index_for_rate(44_100), Some(4));
    assert_eq!(rusty_aac::sf_index_for_rate(96_000), Some(0));
    assert_eq!(rusty_aac::sf_index_for_rate(8_000), Some(11));
    // The exporter's own error path: a rate AAC cannot carry.
    assert_eq!(rusty_aac::sf_index_for_rate(37_000), None);
}

#[test]
fn a_multichannel_track_builds_from_its_audio_specific_config() {
    // 5.1 at 48 kHz, which is what a film's mkv hands over as CodecPrivate.
    let asc = rusty_aac::audio_specific_config_bytes(48_000, 6);
    let decoder = rusty_aac::AacDecoder::with_config_bytes(&asc).expect("decoder builds");
    assert_eq!(decoder.output_sample_rate(), Some(48_000));
    assert_eq!(decoder.sbr_support(), rusty_aac::SbrSupport::NotSignalled);
}

/// `export.rs:1756`: the caller's bitrate over the config's defaults, PCM in,
/// packets out until `Eof`.
#[test]
fn the_export_encoder_loop_runs_to_eof() {
    let rate = 48_000u32;
    let channels = 2u16;
    let frames = rate as usize;
    let mut pcm = Vec::with_capacity(frames * usize::from(channels));
    for i in 0..frames {
        let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
        for _ in 0..channels {
            pcm.push(s);
        }
    }
    let mut encoder = rusty_aac::AacEncoder::new(rusty_aac::AacEncoderConfig {
        bitrate_bps: 256 * 1_000,
        ..Default::default()
    });
    encoder
        .push_pcm(&pcm, channels, rate)
        .expect("pcm accepted");
    encoder.finish();
    let mut packets = Vec::new();
    while let Ok(packet) = encoder.next_packet() {
        assert_eq!(packet.duration, 1024, "one AAC frame per packet");
        assert!(!packet.data.is_empty(), "a packet with no payload");
        packets.push(packet);
    }
    assert!(
        packets.len() > 10,
        "a second of audio is many packets, got {}",
        packets.len()
    );
    assert_eq!(encoder.sample_rate(), rate);
    assert_eq!(encoder.channels(), channels);

    // And the packets are a stream this family's own decoder reads back.
    let asc = rusty_aac::audio_specific_config_bytes(rate, channels);
    let mut decoder = rusty_aac::AacDecoder::with_config_bytes(&asc).expect("decoder builds");
    let mut decoded = 0usize;
    for packet in &packets {
        let audio = decoder.decode(&packet.data, None).expect("packet decodes");
        assert_eq!(audio.channels, channels);
        decoded += audio.frames();
    }
    assert_eq!(decoded, packets.len() * 1024);
}
