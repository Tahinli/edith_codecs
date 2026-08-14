//! `oxideav-sub-image` as edith consumes it, over [`ec_pgs`].
//!
//! Four items: [`PGS_CODEC_ID`], [`pgs::make_decoder`], [`pgs::SEG_PCS`] and
//! [`pgs::SEG_END`] — the replica reads a Matroska block's segments, puts the
//! `.sup` framing back on and stops at the `END` that closes the first set
//! which composes something, then decodes that through the trait.

#![forbid(unsafe_code)]

/// The codec id a PGS track is named by.
pub const PGS_CODEC_ID: &str = "pgs";

/// Presentation Graphic Stream.
pub mod pgs {
    use oxideav_core::{CodecId, CodecParameters, Decoder, EcDecoder, Result};

    pub use ec_pgs::{SEG_END, SEG_ODS, SEG_PCS, SEG_PDS, SEG_WDS};

    /// A PGS decoder: `.sup`-framed segments in, one RGBA canvas per display
    /// set out. The parameters say nothing a PGS stream does not state itself.
    pub fn make_decoder(_params: &CodecParameters) -> Result<Box<dyn Decoder>> {
        Ok(Box::new(EcDecoder::new(
            CodecId::new(crate::PGS_CODEC_ID),
            Box::new(ec_pgs::PgsDecoder::new()),
        )))
    }
}

#[cfg(test)]
mod tests {
    use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

    use super::*;

    /// The `.sup` framing the replica rebuilds around a Matroska block.
    fn segment(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"PG");
        out.extend_from_slice(&[0; 8]);
        out.push(kind);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_display_set_decodes_through_the_replica_s_own_call_shape() {
        let params = CodecParameters::subtitle(CodecId::new(PGS_CODEC_ID));
        let mut decoder = pgs::make_decoder(&params).unwrap();

        // A 4x2 canvas with one 2x1 opaque-white object at (1, 0).
        let mut pcs = vec![0, 4, 0, 2, 0x10, 0, 0, 0x80, 0, 0, 1];
        pcs.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0]);
        let pds = vec![0, 0, 1, 235, 128, 128, 255];
        let rle = vec![0x00, 0x82, 0x01, 0x00, 0x00];
        let mut ods = vec![0, 0, 0, 0xC0];
        ods.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
        ods.extend_from_slice(&[0, 2, 0, 1]);
        ods.extend_from_slice(&rle);
        let mut sup = segment(pgs::SEG_PCS, &pcs);
        // The composition object count is where the replica reads whether the
        // set shows anything.
        assert_eq!(pcs[10], 1);
        sup.extend(segment(pgs::SEG_PDS, &pds));
        sup.extend(segment(pgs::SEG_ODS, &ods));
        sup.extend(segment(pgs::SEG_END, &[]));

        decoder
            .send_packet(&Packet::new(0, TimeBase::new(1, 90_000), sup))
            .unwrap();
        let Ok(Frame::Video(frame)) = decoder.receive_frame() else {
            panic!("a display set is a picture");
        };
        assert_eq!((frame.width, frame.height), (4, 2));
        let data = frame.planes.into_iter().next().unwrap().data;
        assert_eq!(data.len(), 4 * 2 * 4);
        assert_eq!(&data[4..12], &[255, 255, 255, 255, 255, 255, 255, 255]);
        assert_eq!(&data[0..4], &[0, 0, 0, 0]);
    }
}
