//! Inputs the fuzz review and the read-through found to allocate without bound, before the fixes
//! in `lib.rs`: an object or a canvas sized by a 16-bit field.

use ec_pgs::{PgsDecoder, SEG_END, SEG_ODS, SEG_PCS, SEG_PDS};

fn segment(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = b"PG".to_vec();
    out.extend_from_slice(&[0; 8]);
    out.push(kind);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// A 1920x1080 canvas with one object placed at (0,0), epoch start.
fn pcs() -> Vec<u8> {
    segment(
        SEG_PCS,
        &[0x07, 0x80, 0x04, 0x38, 0x10, 0, 0, 0x80, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
    )
}

/// Palette entry 1 opaque white.
fn pds() -> Vec<u8> {
    segment(SEG_PDS, &[0, 0, 1, 235, 128, 128, 255])
}

/// One ODS whose declared size is `width` x `height`, with `rle` as its data.
fn ods(width: u16, height: u16, rle: &[u8]) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0xC0];
    body.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    body.extend_from_slice(&width.to_be_bytes());
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(rle);
    segment(SEG_ODS, &body)
}

/// A 65535 x 65535 object used to be allocated as a RLE bitmap (4 GB) and decoded at render time.
/// It is now refused with `Error::corrupt`, the segments after it still decode, and nothing is
/// allocated for it.
#[test]
fn an_object_past_the_dimension_limit_is_refused_and_the_stream_goes_on() {
    let mut stream = pcs();
    stream.extend(pds());
    stream.extend(ods(65535, 65535, &[0x00, 0x88, 0x01]));
    stream.extend(segment(SEG_END, &[]));
    // A small, legal object after it, in its own display set.
    stream.extend(pcs());
    stream.extend(pds());
    stream.extend(ods(8, 1, &[0x00, 0x88, 0x01]));
    stream.extend(segment(SEG_END, &[]));

    let mut decoder = PgsDecoder::new();
    let error = decoder
        .push(&stream)
        .expect_err("oversized object is refused");
    assert!(error.to_string().contains("65535"), "{error}");
    let frame = decoder.take_frame().expect("the legal set still renders");
    assert_eq!((frame.width, frame.height), (1920, 1080));
}

/// A canvas stated at 65535 x 65535 used to be allocated as RGBA (17 GB). It is refused.
#[test]
fn a_canvas_past_the_dimension_limit_is_refused() {
    let mut stream = segment(SEG_PCS, &[0xFF, 0xFF, 0xFF, 0xFF, 0x10, 0, 0, 0x80, 0, 0, 0]);
    stream.extend(segment(SEG_END, &[]));
    let mut decoder = PgsDecoder::new();
    assert!(decoder.push(&stream).is_err());
    assert!(decoder.take_frame().is_none());
}
