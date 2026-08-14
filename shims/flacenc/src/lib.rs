//! Drop-in replacement for the `flacenc` 0.5.1 surface the replica consumes,
//! implemented over [`ec_flac`].
//!
//! It carries the incumbent's package name and version so the swap is a
//! `[patch.crates-io]` entry and nothing else. The scope is exactly the API
//! `engine/src/export.rs` calls (`export.rs:67-68,2930-2954`):
//!
//! ```no_run
//! use flacenc::component::BitRepr;
//! use flacenc::error::Verify;
//!
//! let config = flacenc::config::Encoder::default().into_verified().unwrap();
//! let source = flacenc::source::MemSource::from_samples(&[0i32; 32], 2, 16, 44100);
//! let mut stream =
//!     flacenc::encode_with_fixed_block_size(&config, source, config.block_size).unwrap();
//! stream
//!     .stream_info_mut()
//!     .set_block_sizes(config.block_size, config.block_size)
//!     .unwrap();
//! let mut sink = flacenc::bitsink::ByteSink::new();
//! stream.write(&mut sink).unwrap();
//! let _bytes: &[u8] = sink.as_slice();
//! ```
//!
//! Signatures match the incumbent's (types, argument order, error shapes) so
//! call sites compile unchanged; the implementation behind them is ours.
//! Deliberately *not* covered: the `par` feature knobs, `serde` config
//! round-tripping and the component tree (`Frame`/`SubFrame`/`Residual` as
//! addressable values) — the replica constructs none of them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bitsink;
pub mod component;
pub mod config;
pub mod error;
pub mod source;

pub use component::Stream;
pub use error::EncodeError;

use error::Verified;
use source::Source;

/// Encode `src` into a stream of fixed-size blocks.
///
/// Mirrors `flacenc::encode_with_fixed_block_size`: the verified config, a
/// source, and the block size (which the caller reads off the config).
pub fn encode_with_fixed_block_size<T: Source>(
    config: &Verified<config::Encoder>,
    src: T,
    block_size: usize,
) -> Result<Stream, EncodeError> {
    let encoder = ec_flac::EncoderConfig {
        block_size,
        stereo_decorrelation: config.stereo_coding.use_midside
            || config.stereo_coding.use_leftside
            || config.stereo_coding.use_rightside,
        ..ec_flac::EncoderConfig::default()
    };
    let bytes = ec_flac::encode(
        &encoder,
        src.as_raw_slice(),
        src.channels(),
        src.bits_per_sample() as u32,
        src.sample_rate() as u32,
    )
    .map_err(|e| EncodeError(e.to_string()))?;
    Ok(Stream::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::BitRepr;
    use crate::error::Verify;

    /// The replica's own call sequence, verbatim from `export.rs:2994-3025`,
    /// with the result decoded back to prove it is a real FLAC stream.
    #[test]
    fn the_replica_call_sequence_produces_a_decodable_stream() {
        let samples: Vec<i32> = (0..8192)
            .map(|i| ((i as f64 * 0.07).sin() * 12000.0) as i32)
            .collect();

        let config = config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| format!("flac encoder config: {e}"))
            .unwrap();
        let source = source::MemSource::from_samples(&samples, 2, 16, 44100);
        let mut stream = encode_with_fixed_block_size(&config, source, config.block_size)
            .map_err(|e| format!("flac encode: {e}"))
            .unwrap();
        stream
            .stream_info_mut()
            .set_block_sizes(config.block_size, config.block_size)
            .map_err(|e| format!("flac block size: {e}"))
            .unwrap();
        let mut sink = bitsink::ByteSink::new();
        stream
            .write(&mut sink)
            .map_err(|e| format!("flac write: {e}"))
            .unwrap();

        let bytes = sink.as_slice();
        assert_eq!(&bytes[..4], b"fLaC");
        assert_eq!(stream.count_bits(), bytes.len() * 8);
        let mut reader = ec_flac::FlacReader::new(bytes).expect("our own output opens");
        let info = reader.stream_info().expect("streaminfo").clone();
        assert_eq!(info.min_block_size, config.block_size as u16);
        assert_eq!(info.max_block_size, config.block_size as u16);
        let decoded = reader.decode_all().expect("decode");
        assert_eq!(decoded.interleaved(), samples, "the shim must be lossless");
    }

    #[test]
    fn a_block_size_the_stream_cannot_hold_is_an_error_not_a_panic() {
        let config = config::Encoder::default().into_verified().unwrap();
        let source = source::MemSource::from_samples(&[0i32; 64], 2, 16, 44100);
        let mut stream = encode_with_fixed_block_size(&config, source, 32).unwrap();
        assert!(stream.stream_info_mut().set_block_sizes(0, 70000).is_err());
    }

    #[test]
    fn an_invalid_config_is_refused_with_its_original_value() {
        let config = config::Encoder {
            block_size: 4,
            ..config::Encoder::default()
        };
        let (returned, error) = config.into_verified().unwrap_err();
        assert_eq!(returned.block_size, 4);
        assert!(format!("{error}").contains("block_size"));
    }
}
