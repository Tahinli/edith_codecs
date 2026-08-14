//! Compatibility shim carrying the `miniz_oxide` name and version, over
//! [`ec_inflate`].
//!
//! Only the surface a caller in this family actually consumes is here — edith's
//! Matroska `ContentEncodings` path uses exactly
//! [`inflate::decompress_to_vec_zlib_with_limit`] and the `Display` of the error
//! it returns. Field names, statuses and message strings match miniz_oxide
//! 0.8.9 so a `[patch.crates-io]` swap needs no source change; anything else the
//! upstream crate exposes is deliberately absent, so a new use shows up as a
//! compile error rather than as silently different behaviour.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Decompression, as `miniz_oxide::inflate`.
pub mod inflate {
    use ec_inflate::Error;

    /// Return status codes, as miniz_oxide spells them.
    #[repr(i8)]
    #[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
    pub enum TINFLStatus {
        /// More input was expected but the caller said there is no more.
        FailedCannotMakeProgress = -4,
        /// The output buffer is an invalid size.
        BadParam = -3,
        /// Decompression went fine but the Adler-32 did not match.
        Adler32Mismatch = -2,
        /// Failed to decompress due to invalid data.
        Failed = -1,
        /// Finished decompression without issues.
        Done = 0,
        /// The decompressor needs more input data to continue.
        NeedsMoreInput = 1,
        /// There is still pending data that did not fit in the output buffer.
        HasMoreOutput = 2,
    }

    /// Failure of a `decompress_to_vec*` call, with the data decoded so far.
    #[derive(Debug)]
    pub struct DecompressError {
        /// Decompressor status on failure.
        pub status: TINFLStatus,
        /// The currently decompressed data, if any.
        pub output: Vec<u8>,
    }

    impl core::fmt::Display for DecompressError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(match self.status {
                TINFLStatus::FailedCannotMakeProgress => "Truncated input stream",
                TINFLStatus::BadParam => "Invalid output buffer size",
                TINFLStatus::Adler32Mismatch => "Adler32 checksum mismatch",
                TINFLStatus::Failed => "Invalid input data",
                TINFLStatus::Done => "",
                TINFLStatus::NeedsMoreInput => "Truncated input stream",
                TINFLStatus::HasMoreOutput => "Output size exceeded the specified limit",
            })
        }
    }

    impl std::error::Error for DecompressError {}

    /// Decompress zlib-wrapped data, growing the output to at most `max_size`.
    ///
    /// Over the limit the error carries [`TINFLStatus::HasMoreOutput`] and the
    /// bytes decompressed so far, as upstream does.
    pub fn decompress_to_vec_zlib_with_limit(
        input: &[u8],
        max_size: usize,
    ) -> Result<Vec<u8>, DecompressError> {
        let mut output = Vec::new();
        match ec_inflate::inflate_into(input, &mut output, max_size, ec_inflate::Format::Zlib) {
            Ok(()) => Ok(output),
            Err(error) => Err(DecompressError {
                status: match error {
                    Error::Truncated => TINFLStatus::FailedCannotMakeProgress,
                    Error::LimitExceeded { .. } => TINFLStatus::HasMoreOutput,
                    Error::Adler32Mismatch { .. } => TINFLStatus::Adler32Mismatch,
                    Error::Corrupt { .. } | Error::Unsupported { .. } => TINFLStatus::Failed,
                },
                output,
            }),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `zlib.compress(b"edith reads this out of a Matroska ContentEncodings block" * 4, 9)`.
        const STREAM: [u8; 66] = [
            0x78, 0xda, 0xd5, 0xcb, 0xc1, 0x0d, 0x80, 0x30, 0x0c, 0x03, 0xc0, 0x55, 0xbc, 0x0b,
            0xe2, 0xc9, 0x10, 0xa1, 0x09, 0xb4, 0x2a, 0x8a, 0xa5, 0xc6, 0xec, 0xcf, 0x1c, 0xdc,
            0xff, 0xc2, 0x87, 0x3a, 0x56, 0x98, 0x17, 0xd4, 0x47, 0x81, 0xaf, 0xc0, 0x0b, 0x86,
            0xc3, 0xb4, 0x58, 0xd3, 0xb0, 0x31, 0x15, 0xa9, 0x3d, 0x1b, 0x7d, 0xe4, 0x5d, 0x38,
            0x1f, 0xb6, 0x19, 0xff, 0x89, 0x1f, 0xb5, 0xdd, 0x54, 0x95,
        ];
        const PLAIN: &[u8] = b"edith reads this out of a Matroska ContentEncodings block";

        /// The call site: engine/src/demux.rs, `Unpack::Zlib`.
        #[test]
        fn decompresses_under_the_limit() {
            let out = decompress_to_vec_zlib_with_limit(&STREAM, 64 << 20).unwrap();
            assert_eq!(out.len(), PLAIN.len() * 4);
            assert_eq!(&out[..PLAIN.len()], PLAIN);
        }

        #[test]
        fn over_the_limit_reports_has_more_output_with_partial_data() {
            let error = decompress_to_vec_zlib_with_limit(&STREAM, 64).unwrap_err();
            assert_eq!(error.status, TINFLStatus::HasMoreOutput);
            assert!(error.output.len() <= 64);
            assert_eq!(
                error.to_string(),
                "Output size exceeded the specified limit"
            );
        }

        #[test]
        fn a_broken_stream_reports_its_status() {
            let mut broken = STREAM;
            let last = broken.len() - 1;
            broken[last] ^= 0xff;
            let error = decompress_to_vec_zlib_with_limit(&broken, 64 << 20).unwrap_err();
            assert_eq!(error.status, TINFLStatus::Adler32Mismatch);
            assert_eq!(error.to_string(), "Adler32 checksum mismatch");

            let error = decompress_to_vec_zlib_with_limit(&STREAM[..20], 64 << 20).unwrap_err();
            assert_eq!(error.status, TINFLStatus::FailedCannotMakeProgress);
            assert_eq!(error.to_string(), "Truncated input stream");
        }
    }
}
