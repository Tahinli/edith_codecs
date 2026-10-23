//! Implicit Parametric Stereo: an ADTS stream whose headers say plain AAC-LC,
//! and whose SBR extension carries a PS extension, is HE-AACv2 by its payload.
//! `ps_implicit` reports it once that PS extension has been decoded.

use std::path::Path;

use ec_aac::AacDecoder;

#[test]
fn implicit_he_aac_v2_reports_implicit_ps() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/he/he_v2_stereo.aac");
    let data = std::fs::read(&path).expect("he_v2_stereo.aac is committed");

    let mut decoder = AacDecoder::new();
    decoder.decode(&data, None).expect("the stream decodes");

    assert!(
        decoder.sbr_implicit(),
        "the SBR payload switches implicit SBR on"
    );
    assert!(
        decoder.ps_implicit(),
        "the PS extension is seen in an implicit stream"
    );
}

#[test]
fn explicit_he_aac_v1_has_no_implicit_ps() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/he/he_v1_stereo.aac");
    let data = std::fs::read(&path).expect("he_v1_stereo.aac is committed");

    let mut decoder = AacDecoder::new();
    decoder.decode(&data, None).expect("the stream decodes");

    assert!(
        !decoder.ps_implicit(),
        "a v1 stream carries no PS extension"
    );
}
