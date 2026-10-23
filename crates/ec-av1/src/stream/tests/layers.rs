//! The spatial-layer output plan ([`LayerOutput`]): which shown frames the
//! sink receives. Only OBU headers and the frame show bits are read, so these
//! streams are hand-built from header bytes.

use super::*;

/// An OBU with no extension, `obu_has_size_field = 1`.
fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(obu_type << 3) | 0x02, payload.len() as u8];
    out.extend_from_slice(payload);
    out
}

/// An OBU with an extension header naming `spatial_id`.
fn obu_spatial(obu_type: u8, spatial_id: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![
        (obu_type << 3) | 0x04 | 0x02,
        spatial_id << 3,
        payload.len() as u8,
    ];
    out.extend_from_slice(payload);
    out
}

/// A frame header payload: `show_existing_frame = 0`, `frame_type = KEY`, and
/// the given `show_frame`.
fn frame_header(show: bool) -> [u8; 1] {
    [if show { 0x10 } else { 0x00 }]
}

const TD: u8 = 2;
const FRAME_HEADER: u8 = 3;

#[test]
fn only_the_last_shown_frame_of_each_temporal_unit_is_kept() {
    // TU 0: base layer shown, top layer shown (kept), a hidden frame.
    // TU 1: one shown frame (kept). TU 2: only a hidden frame, so no output.
    let mut data = Vec::new();
    data.extend(obu(TD, &[]));
    data.extend(obu_spatial(FRAME_HEADER, 0, &frame_header(true)));
    data.extend(obu_spatial(FRAME_HEADER, 1, &frame_header(true)));
    data.extend(obu_spatial(FRAME_HEADER, 1, &frame_header(false)));
    data.extend(obu(TD, &[]));
    data.extend(obu_spatial(FRAME_HEADER, 0, &frame_header(true)));
    data.extend(obu(TD, &[]));
    data.extend(obu_spatial(FRAME_HEADER, 0, &frame_header(false)));
    let plan = last_shown_per_temporal_unit(&data);
    assert_eq!(plan, Some(vec![false, true, true]));
}

#[test]
fn a_stream_without_spatial_layers_has_no_plan() {
    let mut data = Vec::new();
    data.extend(obu(TD, &[]));
    data.extend(obu(FRAME_HEADER, &frame_header(true)));
    data.extend(obu(TD, &[]));
    data.extend(obu(FRAME_HEADER, &frame_header(true)));
    assert_eq!(last_shown_per_temporal_unit(&data), None);
}

#[test]
fn a_stream_without_temporal_delimiters_has_no_plan() {
    let mut data = Vec::new();
    data.extend(obu_spatial(FRAME_HEADER, 0, &frame_header(true)));
    data.extend(obu_spatial(FRAME_HEADER, 1, &frame_header(true)));
    assert_eq!(last_shown_per_temporal_unit(&data), None);
}

#[test]
fn show_existing_frame_counts_as_shown() {
    // show_existing_frame = 1 (bit 7), frame_to_show_map_idx 0 in the next 3 bits.
    let show_existing = [0x80u8];
    let mut data = Vec::new();
    data.extend(obu(TD, &[]));
    data.extend(obu_spatial(FRAME_HEADER, 0, &frame_header(true)));
    data.extend(obu_spatial(FRAME_HEADER, 1, &show_existing));
    assert_eq!(last_shown_per_temporal_unit(&data), Some(vec![false, true]));
}
