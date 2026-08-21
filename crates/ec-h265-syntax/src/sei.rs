//! SEI messages: mastering display, content light level, decoded picture hash.
//!
//! Three messages, and each one is here because something downstream reads it.
//! The two HDR messages are what a tone map finds when the container lost its
//! metadata (see [`ec_core::color::hevc_sei_light`], which reads exactly these
//! payloads back). The decoded picture hash is the encoder's own oracle: an MD5
//! of the reconstructed picture, which any conformant decoder must reproduce, so
//! a mismatch is a *bit-exactness* failure report rather than a mystery.

use crate::md5::Md5;
use crate::nal::{NalHeader, NalUnitType};
use crate::ps::rbsp_trailing_bits;
use ec_core::bitio::BitWriter;
use ec_core::color::ContentLight;

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
    let mut messages = Vec::new();
    if let (Some(max), Some(min)) = (light.mastering_max, light.mastering_min) {
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
}
