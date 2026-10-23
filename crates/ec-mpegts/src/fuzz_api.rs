//! Entry points the fuzz targets reach below the public API. Hidden from the
//! docs; not a stable interface.

use crate::{codec, demux, psi, ts};

/// `buf` as a stream of PSI sections split over packets, then PAT and PMT parsers.
pub fn psi(buf: &[u8]) -> usize {
    let mut sections = psi::SectionBuf::default();
    let mut out = Vec::new();
    for (i, chunk) in buf.chunks(184).enumerate() {
        sections.push(i % 3 == 0, chunk, &mut out);
    }
    let _ = psi::parse_pat(buf);
    let _ = psi::parse_pmt(buf);
    out.len()
}

/// `buf` as one PES packet header.
pub fn pes(buf: &[u8]) -> bool {
    demux::parse_pes(buf).is_some()
}

/// `buf` as an Annex-B access unit: NAL scan, config records, length-prefixing.
pub fn access_unit(buf: &[u8]) -> usize {
    let a = codec::scan_au(buf, false);
    let b = codec::scan_au(buf, true);
    let _ = codec::build_avcc(&a.sps, &a.pps);
    let _ = codec::build_hvcc(&b.vps, &b.sps, &b.pps);
    let _ = codec::annexb_to_length_prefixed(buf, 4);
    codec::nal_units(buf).count()
}

/// `buf` as audio frame headers and the LPCM header.
pub fn audio_headers(buf: &[u8]) -> usize {
    let mut n = 0;
    for i in 0..buf.len().min(64) {
        let b = &buf[i..];
        n += usize::from(codec::adts_header(b).is_some());
        n += usize::from(codec::ac3_header(b).is_some());
        n += usize::from(codec::mpa_header(b).is_some());
        n += usize::from(codec::parse_lpcm_header(b).is_some());
    }
    n
}

/// `buf` as the head of a transport stream: layout detection and the first packet.
pub fn packet_layer(buf: &[u8]) -> bool {
    let _ = ts::parse(buf);
    ts::detect(buf).is_some()
}
