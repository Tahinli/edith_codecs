#![no_main]

use ec_matroska::fuzz_api;
use libfuzzer_sys::fuzz_target;

// `Tracks` in full: TrackEntry, the audio and video masters, CodecPrivate (copied
// into the stream's extradata), ContentEncodings (zlib, header stripping, and the
// refused schemes), colour and mastering metadata, and the language elements. The
// first 8 bytes are the TimestampScale, the rest is the Tracks body.
fuzz_target!(|data: &[u8]| {
    let Some((scale, body)) = data.split_at_checked(8) else {
        return;
    };
    let scale = u64::from_le_bytes(scale.try_into().unwrap_or([0; 8]));
    let _ = fuzz_api::tracks(body, scale);
});
