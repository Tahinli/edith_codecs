//! The `esds` box: MPEG-4 ES descriptors, both directions.
//!
//! The one field anybody actually wants out of here is
//! [`Esds::decoder_specific`] — the raw `DecoderSpecificInfo`, which for an AAC
//! track is its `AudioSpecificConfig` and for an MP3-in-mp4 track is nothing at
//! all. It is public *bytes*, deliberately: a caller that has to rebuild an ASC
//! out of a parsed profile/frequency/channel triplet is a caller whose container
//! threw the bytes away.

use ec_core::{Buf, Error, Result};

/// `ES_Descriptor`.
const TAG_ES: u8 = 0x03;
/// `DecoderConfigDescriptor`.
const TAG_DECODER_CONFIG: u8 = 0x04;
/// `DecoderSpecificInfo`.
const TAG_DECODER_SPECIFIC: u8 = 0x05;
/// `SLConfigDescriptor`.
const TAG_SL_CONFIG: u8 = 0x06;

/// Object type indications, ISO/IEC 14496-1 table 5: what the elementary stream
/// actually is, which is the only thing telling an `mp4a` entry holding MP3 from
/// one holding AAC.
pub mod object_type {
    /// MPEG-4 Audio (AAC and the rest of 14496-3).
    pub const MPEG4_AUDIO: u8 = 0x40;
    /// MPEG-2 AAC Main/LC/SSR.
    pub const MPEG2_AAC_MAIN: u8 = 0x66;
    /// MPEG-2 AAC LC.
    pub const MPEG2_AAC_LC: u8 = 0x67;
    /// MPEG-2 AAC SSR.
    pub const MPEG2_AAC_SSR: u8 = 0x68;
    /// MPEG-2 Audio Part 3 (MP3 at half rates).
    pub const MPEG2_AUDIO: u8 = 0x69;
    /// MPEG-1 Audio (MP3).
    pub const MPEG1_AUDIO: u8 = 0x6B;
}

/// An `esds` payload as its fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Esds {
    /// Elementary stream id; 0 for the single-stream tracks every file has.
    pub es_id: u16,
    /// What the stream is: see [`object_type`].
    pub object_type: u8,
    /// `streamType` (5 = audio, 4 = video), as the descriptor states it.
    pub stream_type: u8,
    /// Decoding buffer size in bytes.
    pub buffer_size_db: u32,
    /// Peak bitrate in bits per second, 0 for "not stated".
    pub max_bitrate: u32,
    /// Average bitrate in bits per second, 0 for "not stated".
    pub avg_bitrate: u32,
    /// The `DecoderSpecificInfo` bytes verbatim — an `AudioSpecificConfig` for
    /// AAC. [`None`] where the track carries none, which is normal for MP3.
    pub decoder_specific: Option<Buf>,
}

impl Esds {
    /// An AAC descriptor around one `AudioSpecificConfig`, which is what a muxer
    /// has to write and all it has to write.
    pub fn aac(config: impl Into<Buf>) -> Esds {
        Esds {
            es_id: 0,
            object_type: object_type::MPEG4_AUDIO,
            stream_type: 5,
            buffer_size_db: 0,
            max_bitrate: 0,
            avg_bitrate: 0,
            decoder_specific: Some(config.into()),
        }
    }

    /// True for the object types whose `DecoderSpecificInfo` is an
    /// `AudioSpecificConfig`.
    pub fn is_aac(&self) -> bool {
        matches!(
            self.object_type,
            object_type::MPEG4_AUDIO
                | object_type::MPEG2_AAC_MAIN
                | object_type::MPEG2_AAC_LC
                | object_type::MPEG2_AAC_SSR
        )
    }

    /// Parse the payload of an `esds` box (after its version/flags word).
    pub fn parse(payload: &[u8]) -> Result<Esds> {
        let (tag, body, _) = descriptor(payload)?;
        if tag != TAG_ES {
            return Err(Error::corrupt(format!(
                "mp4: esds starts with descriptor 0x{tag:02x}, not an ES_Descriptor"
            )));
        }
        let mut out = Esds {
            es_id: crate::boxes::be16(body, 0)?,
            ..Esds::default()
        };
        let flags = *body
            .get(2)
            .ok_or_else(|| Error::corrupt("mp4: esds ends in its flags"))?;
        let mut at = 3;
        if flags & 0x80 != 0 {
            at += 2; // dependsOn_ES_ID
        }
        if flags & 0x40 != 0 {
            let len = *body
                .get(at)
                .ok_or_else(|| Error::corrupt("mp4: esds ends in its URL length"))?;
            at += 1 + len as usize;
        }
        if flags & 0x20 != 0 {
            at += 2; // OCR_ES_Id
        }
        let mut rest = body.get(at..).unwrap_or(&[]);
        // A descriptor list ends where the descriptors do: a trailing byte too
        // short to be one is padding (a real export in the library ends its
        // `esds` with exactly one), not a corrupt file.
        while rest.len() >= 2 {
            let Ok((tag, inner, len)) = descriptor(rest) else {
                break;
            };
            if tag == TAG_DECODER_CONFIG {
                out.object_type = *inner
                    .first()
                    .ok_or_else(|| Error::corrupt("mp4: empty DecoderConfigDescriptor"))?;
                out.stream_type = inner.get(1).map_or(0, |b| b >> 2);
                out.buffer_size_db = u32::from_be_bytes([
                    0,
                    inner.get(2).copied().unwrap_or(0),
                    inner.get(3).copied().unwrap_or(0),
                    inner.get(4).copied().unwrap_or(0),
                ]);
                out.max_bitrate = crate::boxes::be32(inner, 5).unwrap_or(0);
                out.avg_bitrate = crate::boxes::be32(inner, 9).unwrap_or(0);
                let mut inner_rest = inner.get(13..).unwrap_or(&[]);
                while inner_rest.len() >= 2 {
                    let Ok((tag, dsi, len)) = descriptor(inner_rest) else {
                        break;
                    };
                    if tag == TAG_DECODER_SPECIFIC {
                        out.decoder_specific = Some(Buf::copy_from_slice(dsi));
                    }
                    inner_rest = &inner_rest[len..];
                }
            }
            rest = &rest[len..];
        }
        Ok(out)
    }

    /// The `esds` payload for these fields, version/flags word included.
    pub fn write(&self) -> Vec<u8> {
        let mut config = Vec::new();
        if let Some(dsi) = &self.decoder_specific {
            descriptor_out(&mut config, TAG_DECODER_SPECIFIC, dsi);
        }
        let mut decoder = Vec::with_capacity(13 + config.len());
        decoder.push(self.object_type);
        decoder.push((self.stream_type << 2) | 0x01); // upStream 0, reserved 1
        decoder.extend_from_slice(&self.buffer_size_db.to_be_bytes()[1..]);
        decoder.extend_from_slice(&self.max_bitrate.to_be_bytes());
        decoder.extend_from_slice(&self.avg_bitrate.to_be_bytes());
        decoder.extend_from_slice(&config);

        let mut es = Vec::new();
        es.extend_from_slice(&self.es_id.to_be_bytes());
        es.push(0); // no dependency, no URL, no OCR, priority 0
        descriptor_out(&mut es, TAG_DECODER_CONFIG, &decoder);
        // SLConfigDescriptor: predefined 2, "the container tells the time".
        descriptor_out(&mut es, TAG_SL_CONFIG, &[0x02]);

        let mut out = vec![0, 0, 0, 0]; // version 0, flags 0
        descriptor_out(&mut out, TAG_ES, &es);
        out
    }
}

/// One descriptor at the front of `data`: its tag, its body, and how many bytes
/// of `data` it took including the header.
fn descriptor(data: &[u8]) -> Result<(u8, &[u8], usize)> {
    let tag = *data
        .first()
        .ok_or_else(|| Error::corrupt("mp4: descriptor with no tag"))?;
    let mut len = 0u32;
    let mut at = 1;
    // Up to four length bytes, seven bits each — the fifth would be a file
    // spinning a parser rather than describing a stream.
    for _ in 0..4 {
        let b = *data
            .get(at)
            .ok_or_else(|| Error::corrupt("mp4: descriptor length runs off the box"))?;
        at += 1;
        len = (len << 7) | u32::from(b & 0x7F);
        if b & 0x80 == 0 {
            break;
        }
        if at == 5 {
            return Err(Error::corrupt("mp4: descriptor length over four bytes"));
        }
    }
    let end = at + len as usize;
    if end > data.len() {
        return Err(Error::corrupt(format!(
            "mp4: descriptor 0x{tag:02x} states {len} bytes inside {}",
            data.len() - at
        )));
    }
    Ok((tag, &data[at..end], end))
}

/// The inverse, with the length in as few bytes as it fits.
fn descriptor_out(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    let mut len = body.len() as u32;
    let mut bytes = [0u8; 4];
    let mut n = 0;
    loop {
        bytes[n] = (len & 0x7F) as u8;
        len >>= 7;
        n += 1;
        if len == 0 || n == 4 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(bytes[i] | if i == 0 { 0 } else { 0x80 });
    }
    out.extend_from_slice(body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_aac_config_survives_the_round_trip_as_bytes() {
        // 48 kHz stereo AAC-LC: the two bytes edith had to rebuild by hand
        // because the incumbent parsed them apart and dropped the original.
        let asc = [0x11u8, 0x90];
        let bytes = Esds::aac(&asc[..]).write();
        let (_, _, payload) = crate::boxes::full(&bytes).unwrap();
        let back = Esds::parse(payload).unwrap();
        assert!(back.is_aac());
        assert_eq!(back.object_type, object_type::MPEG4_AUDIO);
        assert_eq!(back.stream_type, 5);
        assert_eq!(back.decoder_specific.as_deref(), Some(&asc[..]));
    }

    #[test]
    fn long_descriptors_use_the_multi_byte_length() {
        let big = vec![0xAB; 300];
        let bytes = Esds::aac(&big[..]).write();
        let (_, _, payload) = crate::boxes::full(&bytes).unwrap();
        assert_eq!(
            Esds::parse(payload).unwrap().decoder_specific.as_deref(),
            Some(&big[..])
        );
    }

    #[test]
    fn an_mp3_descriptor_carries_no_config_and_is_not_aac() {
        let mut esds = Esds::aac(&[][..]);
        esds.object_type = object_type::MPEG1_AUDIO;
        esds.decoder_specific = None;
        esds.avg_bitrate = 128_000;
        let bytes = esds.write();
        let (_, _, payload) = crate::boxes::full(&bytes).unwrap();
        let back = Esds::parse(payload).unwrap();
        assert!(!back.is_aac());
        assert_eq!(back.avg_bitrate, 128_000);
        assert_eq!(back.decoder_specific, None);
    }

    /// The `esds` an export in the real library actually carries: an
    /// `SLConfigDescriptor` and then one padding byte, which is not a truncated
    /// descriptor and must not be read as one.
    #[test]
    fn a_trailing_padding_byte_is_not_a_broken_descriptor() {
        let bytes: [u8; 31] = [
            0x03, 0x19, 0x00, 0x01, 0x00, 0x04, 0x11, 0x40, 0x15, 0x00, 0x02, 0xA9, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x02, 0x11, 0x90, 0x06, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00,
        ];
        let esds = Esds::parse(&bytes).expect("the file plays, so it parses");
        assert!(esds.is_aac());
        assert_eq!(esds.decoder_specific.as_deref(), Some(&[0x11, 0x90][..]));
    }

    #[test]
    fn truncated_descriptors_are_errors() {
        assert!(Esds::parse(&[]).is_err());
        assert!(Esds::parse(&[TAG_ES, 0x7F, 0, 0]).is_err()); // states 127 bytes, has 2
        assert!(Esds::parse(&[TAG_ES, 2, 0, 0]).is_err()); // ends inside the flags
        assert!(Esds::parse(&[TAG_SL_CONFIG, 1, 2]).is_err()); // not an ES_Descriptor
        // Four continuation bytes in a row must stop rather than spin.
        assert!(Esds::parse(&[TAG_ES, 0x80, 0x80, 0x80, 0x80, 0x80]).is_err());
    }
}
