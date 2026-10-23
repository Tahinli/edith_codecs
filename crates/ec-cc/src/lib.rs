//! Closed captions: carriage extraction and CEA-608 / CEA-708 decoding.
//!
//! * [`ccdata`]: the `cc_data` triplet type and the wrappers it travels in (ATSC A/53 `GA94`,
//!   the DVD `CC` variant, CDP packets, QuickTime `c608`/`c708` samples).
//! * [`extract`]: side-channel extractors for H.264, HEVC and MPEG-2 elementary streams. They never
//!   touch the decoders: a light parse of the SEI / user data plus just enough of the slice header
//!   (H.264/HEVC) or picture header (MPEG-2) to restore presentation order.
//! * [`cea608`]: the field 1 / field 2 line-21 decoder (CC1 to CC4) with pop-on, roll-up and
//!   paint-on modes.
//! * [`cea708`]: DTVCC service 1 (windows plus text commands); other services are refused by name.
//! * [`synth`]: cc_data generator and SEI / user-data writers used by tests and tools.

#![forbid(unsafe_code)]

pub mod ccdata;
pub mod cea608;
pub mod cea708;
pub mod extract;
pub mod synth;

pub use ccdata::CcTriplet;
pub use cea608::{CaptionCue, CaptionLine, Cea608Decoder};
pub use cea708::Cea708Decoder;
pub use extract::{CcStream, H264Extractor, HevcExtractor, Mpeg2Extractor, TimedCc};

/// Decode a presentation-ordered triplet list into 608 cues and 708 service 1 cues.
///
/// Convenience for callers that just want captions: field 1/2 pairs (`cc_type` 0 and 1) go to the
/// 608 decoder, DTVCC bytes (types 2 and 3) to the 708 decoder.
pub fn decode_all(timed: &[TimedCc]) -> DecodedCaptions {
    let mut d608 = Cea608Decoder::new();
    let mut d708 = Cea708Decoder::new();
    let mut end = 0;
    for t in timed {
        end = t.time_us;
        for tr in &t.triplets {
            if !tr.valid {
                continue;
            }
            match tr.cc_type {
                0 | 1 => d608.push_pair(t.time_us, tr.cc_type, tr.data[0], tr.data[1]),
                2 | 3 => d708.push_triplet(t.time_us, tr.cc_type, tr.data[0], tr.data[1]),
                _ => {}
            }
        }
    }
    DecodedCaptions {
        cea608: d608.finish(end),
        cea708: d708.finish(end),
        unsupported_708_services: d708.unsupported_services(),
    }
}

/// Result of [`decode_all`].
#[derive(Debug, Clone, Default)]
pub struct DecodedCaptions {
    /// CEA-608 cues, all four channels, ordered by start time.
    pub cea608: Vec<CaptionCue>,
    /// CEA-708 service 1 cues (channel field is 1).
    pub cea708: Vec<CaptionCue>,
    /// DTVCC service numbers seen but not decoded (everything except service 1).
    pub unsupported_708_services: Vec<u8>,
}
