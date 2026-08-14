//! The Ogg-Opus header packets, RFC 7845.
//!
//! Only the two headers and the channel-mapping table live here; the pages
//! themselves are `ec-ogg`'s job. Keeping the mapping in this crate is what
//! lets an encoder and a muxer agree without either depending on the other.
//!
//! Two numbers a caller must not invent:
//!
//! - **pre-skip** is the encoder's delay in 48 kHz samples
//!   ([`crate::Encoder::look_ahead`] scaled to 48 kHz — 120 for this encoder).
//!   A player discards that many samples, which is exactly what cancels the
//!   delay; getting it wrong shifts the whole stream.
//! - **granule position** counts 48 kHz samples *including* the pre-skip, so
//!   the first page's granule is `samples_encoded` and the file's duration is
//!   `last_granule - pre_skip`.

/// Builds an `OpusHead` packet (RFC 7845, Section 5.1).
///
/// `input_rate` is the rate the samples arrived at, informational only —
/// the granule clock is always 48 kHz. `mapping` is `(family, streams,
/// coupled, table)`; [`None`] writes family 0, which is mono or stereo.
pub fn opus_head(
    channels: u8,
    pre_skip: u16,
    input_rate: u32,
    output_gain_q8: i16,
    mapping: Option<(u8, u8, u8, &[u8])>,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(21 + channels as usize);
    v.extend_from_slice(b"OpusHead");
    v.push(1);
    v.push(channels);
    v.extend_from_slice(&pre_skip.to_le_bytes());
    v.extend_from_slice(&input_rate.to_le_bytes());
    v.extend_from_slice(&output_gain_q8.to_le_bytes());
    match mapping {
        None => v.push(0),
        Some((family, streams, coupled, table)) => {
            v.push(family);
            v.push(streams);
            v.push(coupled);
            v.extend_from_slice(table);
        }
    }
    v
}

/// Builds an `OpusTags` packet with a vendor string and no comments.
pub fn opus_tags(vendor: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + vendor.len());
    v.extend_from_slice(b"OpusTags");
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor.as_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

/// The default channel mapping of RFC 7845 Section 5.1.1.2: `(family, streams,
/// coupled, table)` for 1 to 8 channels.
///
/// Families 0 and 1 differ in more than the table: family 1 puts the channels
/// in *Vorbis* order (for 5.1: left, centre, right, back left, back right,
/// LFE), so a caller whose buffers are in the usual film order
/// (FL, FR, FC, LFE, BL, BR) permutes by `[0, 2, 1, 4, 5, 3]` on the way in and
/// by the same permutation on the way out.
pub fn default_mapping(channels: usize) -> Option<(u8, u8, u8, Vec<u8>)> {
    let m: (u8, u8, u8, &[u8]) = match channels {
        1 => (0, 1, 0, &[0]),
        2 => (0, 1, 1, &[0, 1]),
        3 => (1, 2, 1, &[0, 2, 1]),
        4 => (1, 2, 2, &[0, 1, 2, 3]),
        5 => (1, 3, 2, &[0, 4, 1, 2, 3]),
        6 => (1, 4, 2, &[0, 4, 1, 2, 3, 5]),
        7 => (1, 4, 3, &[0, 4, 1, 2, 3, 5, 6]),
        8 => (1, 5, 4, &[0, 6, 1, 2, 3, 4, 5, 7]),
        _ => return None,
    };
    Some((m.0, m.1, m.2, m.3.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_head_is_what_the_rfc_describes() {
        let h = opus_head(2, 120, 48000, 0, None);
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!(h[8], 1, "version");
        assert_eq!(h[9], 2, "channels");
        assert_eq!(u16::from_le_bytes([h[10], h[11]]), 120, "pre-skip");
        assert_eq!(u32::from_le_bytes([h[12], h[13], h[14], h[15]]), 48000);
        assert_eq!(h[18], 0, "mapping family");
        assert_eq!(h.len(), 19);

        let (family, streams, coupled, table) = default_mapping(6).unwrap();
        let h = opus_head(6, 120, 48000, 0, Some((family, streams, coupled, &table)));
        assert_eq!(h.len(), 21 + 6);
        assert_eq!(&h[18..21], &[1, 4, 2]);
        assert_eq!(&h[21..], &[0, 4, 1, 2, 3, 5]);
    }
}
