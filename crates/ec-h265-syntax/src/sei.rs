//! SEI messages: mastering display, content light level, decoded picture hash.
//!
//! Three messages, and each one is here because something downstream reads it.
//! The two HDR messages are what a tone map finds when the container lost its
//! metadata (see [`ec_core::color::hevc_sei_light`], which reads exactly these
//! payloads back). The decoded picture hash is the encoder's own oracle: an MD5
//! of the reconstructed picture, which any conformant decoder must reproduce, so
//! a mismatch is a *bit-exactness* failure report rather than a mystery.
//!
//! [`parse_sei_rbsp`] is the reader: it returns the typed messages of one SEI
//! RBSP, and refuses a malformed one rather than guessing.

use crate::md5::Md5;
use crate::nal::{NalHeader, NalUnitType};
use crate::ps::rbsp_trailing_bits;
use ec_core::bitio::BitWriter;
use ec_core::color::ContentLight;
use ec_core::error::{Error, Result};

/// `payloadType` values written here.
const PAYLOAD_MASTERING_DISPLAY: u32 = 137;
const PAYLOAD_CONTENT_LIGHT_LEVEL: u32 = 144;
const PAYLOAD_DECODED_PICTURE_HASH: u32 = 132;

/// Build one SEI RBSP holding `messages`, each a `(payloadType, payload)` pair.
///
/// The `ff` byte run for sizes over 254 is written the way the spec spells it,
/// even though nothing here comes close: a hash message is 49 bytes.
fn sei_rbsp(messages: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(64);
    for (payload_type, payload) in messages {
        let mut left = *payload_type;
        while left >= 255 {
            w.write_bits(0xff, 8);
            left -= 255;
        }
        w.write_bits(left, 8);
        let mut size = payload.len() as u32;
        while size >= 255 {
            w.write_bits(0xff, 8);
            size -= 255;
        }
        w.write_bits(size, 8);
        w.write_bytes(payload);
    }
    rbsp_trailing_bits(&mut w);
    w.into_bytes()
}

/// The prefix SEI RBSP describing HDR mastering metadata, or `None` when there
/// is nothing to say.
///
/// Both messages are written whole or not at all: a `mastering_display_colour_volume`
/// with a zero luminance pair is what the oracle writes when it knows nothing, and a
/// tone map that believes it maps the film to black.
pub fn hdr_metadata_rbsp(light: ContentLight) -> Option<Vec<u8>> {
    hdr_metadata_rbsp_with(None, light)
}

/// [`hdr_metadata_rbsp`] with an explicit mastering display: when `display` is
/// given it is written verbatim (chromaticities and luminances), and the mastering
/// pair inside `light` is ignored. The content light level comes from `light`.
pub fn hdr_metadata_rbsp_with(
    display: Option<MasteringDisplay>,
    light: ContentLight,
) -> Option<Vec<u8>> {
    let mut messages = Vec::new();
    if let Some(d) = display {
        let mut payload = Vec::with_capacity(24);
        for (x, y) in d.display_primaries {
            payload.extend_from_slice(&x.to_be_bytes());
            payload.extend_from_slice(&y.to_be_bytes());
        }
        payload.extend_from_slice(&d.white_point.0.to_be_bytes());
        payload.extend_from_slice(&d.white_point.1.to_be_bytes());
        payload.extend_from_slice(&d.max_luminance.to_be_bytes());
        payload.extend_from_slice(&d.min_luminance.to_be_bytes());
        messages.push((PAYLOAD_MASTERING_DISPLAY, payload));
    } else if let (Some(max), Some(min)) = (light.mastering_max, light.mastering_min) {
        let mut payload = Vec::with_capacity(24);
        // The chromaticities are not modelled by this family (the tone map wants
        // the luminance pair), so BT.2020's are written: they are what an HDR
        // grade is approved on, and a decoder that reads them gets a true answer
        // rather than zeros.
        const BT2020_PRIMARIES: [(u16, u16); 3] = [(8500, 39_850), (6550, 2300), (35_400, 14_600)];
        const D65: (u16, u16) = (15_635, 16_450);
        for (x, y) in BT2020_PRIMARIES {
            payload.extend_from_slice(&x.to_be_bytes());
            payload.extend_from_slice(&y.to_be_bytes());
        }
        payload.extend_from_slice(&D65.0.to_be_bytes());
        payload.extend_from_slice(&D65.1.to_be_bytes());
        payload.extend_from_slice(&((max * 10_000.0) as u32).to_be_bytes());
        payload.extend_from_slice(&((min * 10_000.0) as u32).to_be_bytes());
        messages.push((PAYLOAD_MASTERING_DISPLAY, payload));
    }
    if let (Some(cll), Some(fall)) = (light.max_cll, light.max_fall) {
        let mut payload = Vec::with_capacity(4);
        payload.extend_from_slice(&(cll as u16).to_be_bytes());
        payload.extend_from_slice(&(fall as u16).to_be_bytes());
        messages.push((PAYLOAD_CONTENT_LIGHT_LEVEL, payload));
    }
    if messages.is_empty() {
        None
    } else {
        Some(sei_rbsp(&messages))
    }
}

/// The suffix SEI RBSP carrying one MD5 per colour plane (D.2.20).
///
/// `planes` are the *reconstructed* planes in coding order, each as
/// `(samples, stride, width, height)` over the whole coded picture — the
/// conformance window is not applied, because the hash is defined over
/// `pic_width_in_luma_samples`.
pub fn decoded_picture_hash_rbsp(planes: &[(&[u8], usize, usize, usize)]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 16 * planes.len());
    payload.push(0); // hash_type = MD5
    for &(data, stride, width, height) in planes {
        let mut md5 = Md5::new();
        for y in 0..height {
            md5.update(&data[y * stride..y * stride + width]);
        }
        payload.extend_from_slice(&md5.finish());
    }
    sei_rbsp(&[(PAYLOAD_DECODED_PICTURE_HASH, payload)])
}

/// The NAL header a prefix SEI travels under.
pub fn prefix_sei_header() -> NalHeader {
    NalHeader::new(NalUnitType::PrefixSei)
}

/// The NAL header a suffix SEI (the picture hash) travels under.
pub fn suffix_sei_header() -> NalHeader {
    NalHeader::new(NalUnitType::SuffixSei)
}

/// `payloadType` of the alternative transfer characteristics message (D.2.x).
const PAYLOAD_ALTERNATIVE_TRANSFER: u32 = 147;

/// `mastering_display_colour_volume` (D.2.28): what the grade was approved on.
///
/// Chromaticities are in increments of 0.00002 and luminances in increments of
/// 0.0001 cd/m2, exactly as the message carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasteringDisplay {
    /// `display_primaries_x[c]`, `display_primaries_y[c]` for c = 0, 1, 2. The spec
    /// order is green, blue, red.
    pub display_primaries: [(u16, u16); 3],
    /// `white_point_x`, `white_point_y`.
    pub white_point: (u16, u16),
    /// `max_display_mastering_luminance`, in 0.0001 cd/m2.
    pub max_luminance: u32,
    /// `min_display_mastering_luminance`, in 0.0001 cd/m2.
    pub min_luminance: u32,
}

impl MasteringDisplay {
    /// The peak white in cd/m2 (nits).
    pub fn max_nits(&self) -> f32 {
        self.max_luminance as f32 / 10_000.0
    }

    /// The black level in cd/m2 (nits).
    pub fn min_nits(&self) -> f32 {
        self.min_luminance as f32 / 10_000.0
    }
}

/// `content_light_level_info` (D.2.35), in cd/m2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLightLevel {
    /// `max_content_light_level` (MaxCLL).
    pub max_content_light_level: u16,
    /// `max_pic_average_light_level` (MaxFALL).
    pub max_pic_average_light_level: u16,
}

/// `decoded_picture_hash` (D.2.20): one digest per colour plane, in coding order.
///
/// The number of planes is one for 4:0:0 and three otherwise. The message carries no
/// chroma format, so it is read from the payload size, and any other count is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureHash {
    /// `hash_type` 0: an MD5 of each plane.
    Md5(Vec<[u8; 16]>),
    /// `hash_type` 1: a 16-bit CRC of each plane.
    Crc(Vec<u16>),
    /// `hash_type` 2: a 32-bit checksum of each plane.
    Checksum(Vec<u32>),
}

/// One SEI message read from a prefix or suffix SEI RBSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeiMessage {
    /// `mastering_display_colour_volume`, payloadType 137.
    MasteringDisplay(MasteringDisplay),
    /// `content_light_level_info`, payloadType 144.
    ContentLightLevel(ContentLightLevel),
    /// `decoded_picture_hash`, payloadType 132.
    DecodedPictureHash(PictureHash),
    /// `alternative_transfer_characteristics`, payloadType 147: the
    /// `preferred_transfer_characteristics` code point.
    AlternativeTransferCharacteristics(u8),
    /// Any other payload type, carried verbatim.
    Unknown {
        /// `payloadType`.
        payload_type: u32,
        /// The payload bytes, without the type and size fields.
        bytes: Vec<u8>,
    },
}

/// Read every SEI message in one SEI RBSP (7.3.2.4 and 7.3.5).
///
/// The caller has already removed emulation prevention bytes. Each message is
/// `payloadType` and `payloadSize`, each a run of `0xff` bytes plus a final byte,
/// followed by the payload, and the RBSP ends with the trailing bits. A message
/// whose known type has the wrong size, a payload that runs past the end, a
/// missing trailing bit, or a picture hash with a plane count other than one or
/// three is [`Error::corrupt`]; nothing here panics on any input.
pub fn parse_sei_rbsp(rbsp: &[u8]) -> Result<Vec<SeiMessage>> {
    // The stop bit is the last set bit of the RBSP; everything before it is messages.
    let stop = rbsp
        .iter()
        .rposition(|&b| b != 0)
        .ok_or_else(|| Error::corrupt("SEI: RBSP is all zero bytes"))?;
    if rbsp[stop] != 0x80 {
        return Err(Error::corrupt("SEI: RBSP has no trailing bits"));
    }
    let end = stop;
    let mut at = 0usize;
    let mut out = Vec::new();
    while at < end {
        let payload_type = read_ff_value(rbsp, &mut at, end)?;
        let payload_type = u32::try_from(payload_type)
            .map_err(|_| Error::corrupt("SEI: payloadType does not fit 32 bits"))?;
        let size = read_ff_value(rbsp, &mut at, end)?;
        if size > end - at {
            return Err(Error::corrupt("SEI: payload runs past the end of the RBSP"));
        }
        let payload = &rbsp[at..at + size];
        at += size;
        out.push(message(payload_type, payload)?);
    }
    Ok(out)
}

/// One message from its type and payload.
fn message(payload_type: u32, payload: &[u8]) -> Result<SeiMessage> {
    Ok(match payload_type {
        PAYLOAD_MASTERING_DISPLAY => {
            if payload.len() != 24 {
                return Err(Error::corrupt(format!(
                    "SEI: mastering display payload is {} bytes, not 24",
                    payload.len()
                )));
            }
            let u16_at = |i: usize| u16::from_be_bytes([payload[i], payload[i + 1]]);
            let u32_at = |i: usize| {
                u32::from_be_bytes([payload[i], payload[i + 1], payload[i + 2], payload[i + 3]])
            };
            SeiMessage::MasteringDisplay(MasteringDisplay {
                display_primaries: [
                    (u16_at(0), u16_at(2)),
                    (u16_at(4), u16_at(6)),
                    (u16_at(8), u16_at(10)),
                ],
                white_point: (u16_at(12), u16_at(14)),
                max_luminance: u32_at(16),
                min_luminance: u32_at(20),
            })
        }
        PAYLOAD_CONTENT_LIGHT_LEVEL => {
            if payload.len() != 4 {
                return Err(Error::corrupt(format!(
                    "SEI: content light level payload is {} bytes, not 4",
                    payload.len()
                )));
            }
            SeiMessage::ContentLightLevel(ContentLightLevel {
                max_content_light_level: u16::from_be_bytes([payload[0], payload[1]]),
                max_pic_average_light_level: u16::from_be_bytes([payload[2], payload[3]]),
            })
        }
        PAYLOAD_DECODED_PICTURE_HASH => SeiMessage::DecodedPictureHash(picture_hash(payload)?),
        PAYLOAD_ALTERNATIVE_TRANSFER => {
            if payload.len() != 1 {
                return Err(Error::corrupt(format!(
                    "SEI: alternative transfer payload is {} bytes, not 1",
                    payload.len()
                )));
            }
            SeiMessage::AlternativeTransferCharacteristics(payload[0])
        }
        _ => SeiMessage::Unknown {
            payload_type,
            bytes: payload.to_vec(),
        },
    })
}

/// A `decoded_picture_hash` payload: `hash_type`, then one digest per plane.
fn picture_hash(payload: &[u8]) -> Result<PictureHash> {
    let Some((&hash_type, digests)) = payload.split_first() else {
        return Err(Error::corrupt("SEI: empty decoded picture hash payload"));
    };
    let size = match hash_type {
        0 => 16,
        1 => 2,
        2 => 4,
        other => {
            return Err(Error::corrupt(format!(
                "SEI: decoded picture hash_type {other} is reserved"
            )));
        }
    };
    if digests.len() % size != 0 || !matches!(digests.len() / size, 1 | 3) {
        return Err(Error::corrupt(format!(
            "SEI: decoded picture hash carries {} bytes, not one or three planes of {size}",
            digests.len()
        )));
    }
    Ok(match hash_type {
        0 => PictureHash::Md5(digests.as_chunks::<16>().0.to_vec()),
        1 => PictureHash::Crc(
            digests
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect(),
        ),
        _ => PictureHash::Checksum(
            digests
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_be_bytes(*c))
                .collect(),
        ),
    })
}

/// A `payloadType` or `payloadSize`: a run of `0xff` bytes and one more. Stops at
/// `end`, so a value that runs off the RBSP is an error.
fn read_ff_value(rbsp: &[u8], at: &mut usize, end: usize) -> Result<usize> {
    let mut value = 0usize;
    loop {
        if *at >= end {
            return Err(Error::corrupt("SEI: message header runs past the RBSP"));
        }
        let byte = rbsp[*at];
        *at += 1;
        value += usize::from(byte);
        if byte != 0xff {
            return Ok(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_sei_reads_back_through_ec_core() {
        let light = ContentLight {
            max_cll: Some(1000.0),
            max_fall: Some(400.0),
            mastering_max: Some(1000.0),
            mastering_min: Some(0.005),
            ..ContentLight::default()
        };
        let rbsp = hdr_metadata_rbsp(light).unwrap();
        let mut annex_b = Vec::new();
        crate::nal::write_annex_b(&mut annex_b, prefix_sei_header(), &rbsp, true);
        // ec-core's reader is the consumer this exists for.
        let read = ec_core::color::hevc_sei_light(&annex_b);
        assert_eq!(read.max_cll, Some(1000.0));
        assert_eq!(read.max_fall, Some(400.0));
        assert_eq!(read.mastering_max, Some(1000.0));
        assert_eq!(read.mastering_min, Some(0.005));
        assert!(hdr_metadata_rbsp(ContentLight::default()).is_none());
    }

    #[test]
    fn picture_hash_is_md5_of_each_plane() {
        let luma = vec![7u8; 16 * 16];
        let rbsp = decoded_picture_hash_rbsp(&[(&luma, 16, 16, 16)]);
        // payloadType 132, payloadSize 17, hash_type 0, then the digest.
        assert_eq!(rbsp[0], 132);
        assert_eq!(rbsp[1], 17);
        assert_eq!(rbsp[2], 0);
        let mut md5 = Md5::new();
        md5.update(&luma);
        assert_eq!(&rbsp[3..19], &md5.finish()[..]);
    }

    /// The HDR10 mastering display of a 1000 nit, 0.0050 nit BT.2020 grade on D65.
    fn hdr10_mdcv_payload() -> Vec<u8> {
        let mut p = Vec::new();
        for (x, y) in [(8500u16, 39_850u16), (6550, 2300), (35_400, 14_600)] {
            p.extend_from_slice(&x.to_be_bytes());
            p.extend_from_slice(&y.to_be_bytes());
        }
        p.extend_from_slice(&15_635u16.to_be_bytes());
        p.extend_from_slice(&16_450u16.to_be_bytes());
        p.extend_from_slice(&10_000_000u32.to_be_bytes());
        p.extend_from_slice(&50u32.to_be_bytes());
        p
    }

    #[test]
    fn hdr10_mastering_display_example_reads_back() {
        let rbsp = sei_rbsp(&[(137, hdr10_mdcv_payload())]);
        let msgs = parse_sei_rbsp(&rbsp).unwrap();
        assert_eq!(msgs.len(), 1);
        let SeiMessage::MasteringDisplay(md) = msgs[0] else {
            panic!("not a mastering display: {:?}", msgs[0]);
        };
        assert_eq!(md.display_primaries[0], (8500, 39_850)); // green
        assert_eq!(md.display_primaries[1], (6550, 2300)); // blue
        assert_eq!(md.display_primaries[2], (35_400, 14_600)); // red
        assert_eq!(md.white_point, (15_635, 16_450)); // D65
        assert_eq!(md.max_nits(), 1000.0);
        assert!((md.min_nits() - 0.005).abs() < 1e-6);
    }

    #[test]
    fn content_light_and_transfer_read_back_among_others() {
        let mut cll = Vec::new();
        cll.extend_from_slice(&1000u16.to_be_bytes());
        cll.extend_from_slice(&400u16.to_be_bytes());
        // payloadType 300 and a 260-byte payload: both need the ff extension.
        let unknown = vec![0xab; 260];
        let mut rbsp = Vec::new();
        rbsp.extend_from_slice(&[0xff, 45, 0xff, 5]);
        rbsp.extend_from_slice(&unknown);
        rbsp.extend_from_slice(&[144, 4]);
        rbsp.extend_from_slice(&cll);
        rbsp.extend_from_slice(&[147, 1, 16]);
        rbsp.push(0x80);
        let msgs = parse_sei_rbsp(&rbsp).unwrap();
        assert_eq!(
            msgs,
            vec![
                SeiMessage::Unknown {
                    payload_type: 300,
                    bytes: unknown,
                },
                SeiMessage::ContentLightLevel(ContentLightLevel {
                    max_content_light_level: 1000,
                    max_pic_average_light_level: 400,
                }),
                SeiMessage::AlternativeTransferCharacteristics(16),
            ]
        );
    }

    #[test]
    fn picture_hashes_read_back_for_each_hash_type() {
        // MD5, three planes: one digest each, 1 + 48 bytes.
        let mut md5 = vec![0u8];
        md5.extend(1..=48u8);
        // CRC, one plane.
        let crc = [1u8, 0xbe, 0xef];
        // Checksum, three planes.
        let mut sum = vec![2u8];
        sum.extend_from_slice(&1u32.to_be_bytes());
        sum.extend_from_slice(&2u32.to_be_bytes());
        sum.extend_from_slice(&3u32.to_be_bytes());
        let rbsp = sei_rbsp(&[(132, md5.clone()), (132, crc.to_vec()), (132, sum)]);
        let msgs = parse_sei_rbsp(&rbsp).unwrap();
        let mut digests = [[0u8; 16]; 3];
        for (i, d) in digests.iter_mut().enumerate() {
            d.copy_from_slice(&md5[1 + 16 * i..1 + 16 * (i + 1)]);
        }
        assert_eq!(
            msgs[0],
            SeiMessage::DecodedPictureHash(PictureHash::Md5(digests.to_vec()))
        );
        assert_eq!(
            msgs[1],
            SeiMessage::DecodedPictureHash(PictureHash::Crc(vec![0xbeef]))
        );
        assert_eq!(
            msgs[2],
            SeiMessage::DecodedPictureHash(PictureHash::Checksum(vec![1, 2, 3]))
        );
    }

    #[test]
    fn malformed_sei_is_an_error_not_a_panic() {
        let short_mdcv = sei_rbsp(&[(137, vec![0; 23])]);
        let long_cll = sei_rbsp(&[(144, vec![0; 5])]);
        let two_plane_md5 = sei_rbsp(&[(132, [vec![0u8], vec![0; 32]].concat())]);
        let reserved_hash = sei_rbsp(&[(132, vec![3, 0, 0])]);
        let bad_atc = sei_rbsp(&[(147, vec![1, 2])]);
        let no_stop_bit = vec![5u8, 1, 0x00];
        let all_zero = vec![0u8, 0];
        let empty = Vec::new();
        let unterminated_ff = vec![0xffu8, 0xff];
        let past_end = vec![5u8, 9, 1, 2, 0x80];
        let size_ff_past_end = vec![5u8, 0xff, 0xff, 0x80];
        for (name, bad) in [
            ("short mdcv", short_mdcv),
            ("long cll", long_cll),
            ("two-plane md5", two_plane_md5),
            ("reserved hash type", reserved_hash),
            ("bad atc", bad_atc),
            ("no stop bit", no_stop_bit),
            ("all zero", all_zero),
            ("empty", empty),
            ("unterminated ff", unterminated_ff),
            ("payload past end", past_end),
            ("size ff past end", size_ff_past_end),
        ] {
            assert!(parse_sei_rbsp(&bad).is_err(), "{name} should be refused");
        }
        // Every truncation of a valid message is refused or read, never a panic.
        let good = sei_rbsp(&[(137, hdr10_mdcv_payload()), (144, vec![0, 1, 0, 2])]);
        for n in 0..good.len() {
            let _ = parse_sei_rbsp(&good[..n]);
        }
    }
}
