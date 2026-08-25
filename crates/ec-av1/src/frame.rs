//! The frame header OBU (spec 5.9), for shown key frames.
//!
//! Every writer here is the bit-exact inverse of the matching reader in
//! [`ec_av1_syntax::frame`], and the tests prove it by parsing back what they
//! wrote. The subset is the one a stateless hardware encoder needs: a key frame
//! that is shown, in a sequence with no decoder model, no frame ids and no
//! still-picture shortcut. Everything outside it is refused by name rather than
//! written wrong — superres with a denominator other than 8, film grain
//! synthesis, non-uniform tile spacing, and every non-key or hidden frame.

use ec_av1_syntax::obu::{ObuHeader, ObuType, tile_log2};
use ec_av1_syntax::sequence::{SELECT_INTEGER_MV, SELECT_SCREEN_CONTENT_TOOLS, SequenceHeader};
use ec_av1_syntax::{
    FrameHeader, FrameType, MAX_SEGMENTS, MAX_TILE_AREA, MAX_TILE_COLS, MAX_TILE_ROWS,
    MAX_TILE_WIDTH, PRIMARY_REF_NONE, RestorationType, SEG_LVL_MAX, TOTAL_REFS_PER_FRAME, TxMode,
};
use ec_core::{BitWriter, Error, Result};

use crate::bits::{write_byte_alignment, write_delta_q, write_su};
use crate::obu::wrap_obu;

/// `Segmentation_Feature_Bits` (spec 5.9.14).
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
/// `Segmentation_Feature_Signed` (spec 5.9.14).
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
/// The loop filter reference deltas a frame with no primary reference starts
/// from (spec 6.8.2, `setup_past_independence`); only the ones that differ are
/// coded.
const DEFAULT_REF_DELTAS: [i8; TOTAL_REFS_PER_FRAME] = [1, 0, 0, 0, -1, 0, -1, -1];

/// `REMAP_LR_TYPE` inverted (spec 5.9.20): the coded index is not the enum's
/// discriminant, so the mapping is spelled out.
fn lr_type_code(t: RestorationType) -> u32 {
    match t {
        RestorationType::None => 0,
        RestorationType::Switchable => 1,
        RestorationType::Wiener => 2,
        RestorationType::Sgrproj => 3,
    }
}

/// The increasing-flag idiom the tile columns and rows are coded with: a one
/// per step from `min` to `target`, and a terminating zero unless `target` is
/// already the maximum.
fn ramp(w: &mut BitWriter, target: u32, min: u32, max: u32) -> Result<()> {
    if target < min || target > max {
        return Err(Error::corrupt("AV1 tile_info: tile log2 out of range"));
    }
    for _ in min..target {
        w.write_bit(true);
    }
    if target < max {
        w.write_bit(false);
    }
    Ok(())
}

fn sb_cols(seq: &SequenceHeader, mi_cols: u32) -> u32 {
    if seq.use_128x128_superblock {
        mi_cols.div_ceil(32)
    } else {
        mi_cols.div_ceil(16)
    }
}

fn sb_rows(seq: &SequenceHeader, mi_rows: u32) -> u32 {
    if seq.use_128x128_superblock {
        mi_rows.div_ceil(32)
    } else {
        mi_rows.div_ceil(16)
    }
}

/// `tile_info()` (spec 5.9.15), uniform spacing only.
fn write_tile_info(w: &mut BitWriter, seq: &SequenceHeader, h: &FrameHeader) -> Result<()> {
    if !h.tile_info.uniform_spacing {
        return Err(Error::unsupported(
            "AV1 tile_info",
            "non-uniform tile spacing is not written",
        ));
    }
    let (cols, rows) = (sb_cols(seq, h.mi_cols), sb_rows(seq, h.mi_rows));
    let sb_size = if seq.use_128x128_superblock { 7 } else { 6 };
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
    let max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
    let min_log2_tile_cols = tile_log2(max_tile_width_sb, cols);
    let max_log2_tile_cols = tile_log2(1, cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2(1, rows.min(MAX_TILE_ROWS));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, rows * cols));

    w.write_bit(true);
    ramp(
        w,
        h.tile_info.cols_log2,
        min_log2_tile_cols,
        max_log2_tile_cols,
    )?;
    let rows_start = min_log2_tiles.saturating_sub(h.tile_info.cols_log2);
    ramp(w, h.tile_info.rows_log2, rows_start, max_log2_tile_rows)?;
    if h.tile_info.cols_log2 > 0 || h.tile_info.rows_log2 > 0 {
        w.write_bits(
            h.tile_info.context_update_tile_id,
            h.tile_info.rows_log2 + h.tile_info.cols_log2,
        );
        w.write_bits(h.tile_info.tile_size_bytes - 1, 2);
    }
    Ok(())
}

/// `quantization_params()` (spec 5.9.12).
fn write_quantization_params(w: &mut BitWriter, seq: &SequenceHeader, h: &FrameHeader) {
    let color = &seq.color_config;
    let q = &h.quantization;
    w.write_bits(u32::from(q.base_q_idx), 8);
    write_delta_q(w, i32::from(q.delta_q_y_dc));
    if color.num_planes > 1 {
        let diff_uv_delta = if color.separate_uv_delta_q {
            let d = q.delta_q_v_dc != q.delta_q_u_dc || q.delta_q_v_ac != q.delta_q_u_ac;
            w.write_bit(d);
            d
        } else {
            false
        };
        write_delta_q(w, i32::from(q.delta_q_u_dc));
        write_delta_q(w, i32::from(q.delta_q_u_ac));
        if diff_uv_delta {
            write_delta_q(w, i32::from(q.delta_q_v_dc));
            write_delta_q(w, i32::from(q.delta_q_v_ac));
        }
    }
    w.write_bit(q.using_qmatrix);
    if q.using_qmatrix {
        w.write_bits(u32::from(q.qm_y), 4);
        w.write_bits(u32::from(q.qm_u), 4);
        if color.separate_uv_delta_q {
            w.write_bits(u32::from(q.qm_v), 4);
        }
    }
}

/// `segmentation_params()` (spec 5.9.14). The frame this writer codes has no
/// primary reference, so the update flags are forced and not coded.
fn write_segmentation_params(w: &mut BitWriter, h: &FrameHeader) {
    let seg = &h.segmentation;
    w.write_bit(seg.enabled);
    if !seg.enabled {
        return;
    }
    if h.primary_ref_frame != PRIMARY_REF_NONE {
        w.write_bit(seg.update_map);
        if seg.update_map {
            w.write_bit(seg.temporal_update);
        }
        w.write_bit(seg.update_data);
    }
    if seg.update_data || h.primary_ref_frame == PRIMARY_REF_NONE {
        for segment in 0..MAX_SEGMENTS {
            for feature in 0..SEG_LVL_MAX {
                let enabled = seg.feature_enabled[segment][feature];
                w.write_bit(enabled);
                if !enabled {
                    continue;
                }
                let bits = SEG_FEATURE_BITS[feature];
                let value = i32::from(seg.feature_data[segment][feature]);
                if SEG_FEATURE_SIGNED[feature] {
                    write_su(w, 1 + bits, value);
                } else if bits > 0 {
                    w.write_bits(value as u32, bits);
                }
            }
        }
    }
}

/// `delta_q_params()` (spec 5.9.17).
fn write_delta_q_params(w: &mut BitWriter, h: &FrameHeader) {
    if h.quantization.base_q_idx > 0 {
        w.write_bit(h.delta.q_present);
    }
    if h.delta.q_present {
        w.write_bits(u32::from(h.delta.q_res), 2);
    }
}

/// `delta_lf_params()` (spec 5.9.18).
fn write_delta_lf_params(w: &mut BitWriter, h: &FrameHeader) {
    if !h.delta.q_present {
        return;
    }
    if !h.allow_intrabc {
        w.write_bit(h.delta.lf_present);
    }
    if h.delta.lf_present {
        w.write_bits(u32::from(h.delta.lf_res), 2);
        w.write_bit(h.delta.lf_multi);
    }
}

/// `loop_filter_params()` (spec 5.9.11).
fn write_loop_filter_params(
    w: &mut BitWriter,
    seq: &SequenceHeader,
    h: &FrameHeader,
    coded_lossless: bool,
) {
    if coded_lossless || h.allow_intrabc {
        return;
    }
    let lf = &h.loop_filter;
    w.write_bits(u32::from(lf.level[0]), 6);
    w.write_bits(u32::from(lf.level[1]), 6);
    if seq.color_config.num_planes > 1 && (lf.level[0] > 0 || lf.level[1] > 0) {
        w.write_bits(u32::from(lf.level[2]), 6);
        w.write_bits(u32::from(lf.level[3]), 6);
    }
    w.write_bits(u32::from(lf.sharpness), 3);
    w.write_bit(lf.delta_enabled);
    if !lf.delta_enabled {
        return;
    }
    w.write_bit(lf.delta_update);
    if !lf.delta_update {
        return;
    }
    for (i, &delta) in lf.ref_deltas.iter().enumerate() {
        let update = delta != DEFAULT_REF_DELTAS[i];
        w.write_bit(update);
        if update {
            write_su(w, 7, i32::from(delta));
        }
    }
    for &delta in &lf.mode_deltas {
        let update = delta != 0;
        w.write_bit(update);
        if update {
            write_su(w, 7, i32::from(delta));
        }
    }
}

/// `cdef_params()` (spec 5.9.19).
fn write_cdef_params(
    w: &mut BitWriter,
    seq: &SequenceHeader,
    h: &FrameHeader,
    coded_lossless: bool,
) {
    if coded_lossless || h.allow_intrabc || !seq.enable_cdef {
        return;
    }
    let cdef = &h.cdef;
    w.write_bits(u32::from(cdef.damping - 3), 2);
    w.write_bits(u32::from(cdef.bits), 2);
    // A secondary strength of 4 is coded as 3; the reader maps it back.
    let coded_sec = |s: u8| u32::from(if s == 4 { 3 } else { s });
    for i in 0..(1usize << cdef.bits) {
        w.write_bits(u32::from(cdef.y_pri_strength[i]), 4);
        w.write_bits(coded_sec(cdef.y_sec_strength[i]), 2);
        if seq.color_config.num_planes > 1 {
            w.write_bits(u32::from(cdef.uv_pri_strength[i]), 4);
            w.write_bits(coded_sec(cdef.uv_sec_strength[i]), 2);
        }
    }
}

/// `lr_params()` (spec 5.9.20).
fn write_lr_params(w: &mut BitWriter, seq: &SequenceHeader, h: &FrameHeader, all_lossless: bool) {
    if all_lossless || h.allow_intrabc || !seq.enable_restoration {
        return;
    }
    let lr = &h.loop_restoration;
    let (mut uses_lr, mut uses_chroma_lr) = (false, false);
    for plane in 0..seq.color_config.num_planes as usize {
        w.write_bits(lr_type_code(lr.frame_restoration_type[plane]), 2);
        if lr.frame_restoration_type[plane] != RestorationType::None {
            uses_lr = true;
            uses_chroma_lr |= plane > 0;
        }
    }
    if !uses_lr {
        return;
    }
    if seq.use_128x128_superblock {
        w.write_bits(u32::from(lr.lr_unit_shift) - 1, 1);
    } else {
        w.write_bit(lr.lr_unit_shift > 0);
        if lr.lr_unit_shift > 0 {
            w.write_bits(u32::from(lr.lr_unit_shift) - 1, 1);
        }
    }
    if seq.color_config.subsampling_x == 1 && seq.color_config.subsampling_y == 1 && uses_chroma_lr
    {
        w.write_bits(u32::from(lr.lr_uv_shift), 1);
    }
}

/// `uncompressed_header()` (spec 5.9.2) for a shown key frame.
pub fn write_frame_header(w: &mut BitWriter, seq: &SequenceHeader, h: &FrameHeader) -> Result<()> {
    if h.frame_type != FrameType::Key || !h.show_frame || h.show_existing_frame {
        return Err(Error::unsupported(
            "AV1 frame header",
            "only a shown key frame is written",
        ));
    }
    if seq.reduced_still_picture_header {
        return Err(Error::unsupported(
            "AV1 frame header",
            "reduced_still_picture_header changes the whole header layout",
        ));
    }
    if seq.decoder_model_info.is_some() {
        return Err(Error::unsupported(
            "AV1 frame header",
            "decoder model timing is not written",
        ));
    }
    if seq.frame_id_numbers_present_flag {
        return Err(Error::unsupported(
            "AV1 frame header",
            "frame id numbers are not written",
        ));
    }

    w.write_bit(false); // show_existing_frame
    w.write_bits(0, 2); // frame_type: KEY_FRAME
    w.write_bit(true); // show_frame
    // showable_frame and error_resilient_mode are forced for a shown key frame.

    w.write_bit(h.disable_cdf_update);
    if seq.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
        w.write_bit(h.allow_screen_content_tools);
    } else if h.allow_screen_content_tools != (seq.seq_force_screen_content_tools != 0) {
        return Err(Error::corrupt(
            "AV1 frame header: allow_screen_content_tools contradicts the sequence",
        ));
    }
    if h.allow_screen_content_tools && seq.seq_force_integer_mv == SELECT_INTEGER_MV {
        // The reader forces this back to true for an intra frame; the bit is
        // still coded, and the value it carries is the one the reader discards.
        w.write_bit(h.force_integer_mv);
    }
    w.write_bit(h.frame_size_override_flag);
    w.write_bits(h.order_hint, seq.order_hint_bits);
    // primary_ref_frame, buffer_removal_time, refresh_frame_flags and the
    // ref_order_hint loop are all forced for a shown key frame.

    if h.frame_size_override_flag {
        w.write_bits(h.frame_width - 1, seq.frame_width_bits);
        w.write_bits(h.frame_height - 1, seq.frame_height_bits);
    }
    if seq.enable_superres {
        w.write_bit(h.use_superres);
    }
    if h.use_superres || h.superres_denom != 8 {
        return Err(Error::unsupported(
            "AV1 superres",
            "only a superres denominator of 8 (no scaling) is written",
        ));
    }
    let render_differs = h.render_width != h.upscaled_width || h.render_height != h.frame_height;
    w.write_bit(render_differs);
    if render_differs {
        w.write_bits(h.render_width - 1, 16);
        w.write_bits(h.render_height - 1, 16);
    }
    if h.allow_screen_content_tools && h.upscaled_width == h.frame_width {
        w.write_bit(h.allow_intrabc);
    }
    if !h.disable_cdf_update {
        w.write_bit(h.disable_frame_end_update_cdf);
    }

    write_tile_info(w, seq, h)?;
    write_quantization_params(w, seq, h);
    write_segmentation_params(w, h);
    write_delta_q_params(w, h);
    write_delta_lf_params(w, h);

    // `coded_lossless` gates the loop filter, CDEF, restoration and transform
    // mode syntax; it is derived, never coded (spec 5.9.2).
    let q = &h.quantization;
    let coded_lossless = (0..MAX_SEGMENTS).all(|segment| {
        h.segment_qindex(segment) == 0
            && q.delta_q_y_dc == 0
            && q.delta_q_u_ac == 0
            && q.delta_q_u_dc == 0
            && q.delta_q_v_ac == 0
            && q.delta_q_v_dc == 0
    });
    let all_lossless = coded_lossless && h.frame_width == h.upscaled_width;

    write_loop_filter_params(w, seq, h, coded_lossless);
    write_cdef_params(w, seq, h, coded_lossless);
    write_lr_params(w, seq, h, all_lossless);
    if coded_lossless {
        if h.tx_mode != TxMode::Only4x4 {
            return Err(Error::corrupt(
                "AV1 frame header: a lossless frame can only use TX_MODE_ONLY_4X4",
            ));
        }
    } else {
        w.write_bit(h.tx_mode == TxMode::Select);
    }
    // reference_select, skip mode and warped motion are all forced for intra.
    w.write_bit(h.reduced_tx_set);
    // Global motion is the identity for intra and is not coded.
    if seq.film_grain_params_present {
        w.write_bit(h.film_grain.apply_grain);
        if h.film_grain.apply_grain {
            return Err(Error::unsupported(
                "AV1 film grain",
                "film grain synthesis parameters are not written",
            ));
        }
    }
    Ok(())
}

/// A `OBU_FRAME`: the header above, byte-aligned, followed by the tile group
/// payload the caller supplies.
pub fn frame_obu(seq: &SequenceHeader, h: &FrameHeader, tile_data: &[u8]) -> Result<Vec<u8>> {
    let mut w = BitWriter::new();
    write_frame_header(&mut w, seq, h)?;
    write_byte_alignment(&mut w);
    let mut payload = w.into_bytes();
    payload.extend_from_slice(tile_data);
    Ok(wrap_obu(
        &ObuHeader {
            obu_type: ObuType::Frame,
            extension_flag: false,
            has_size_field: true,
            temporal_id: 0,
            spatial_id: 0,
        },
        &payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::sequence_header_obu;
    use crate::sequence::tests::sample_1080p;
    use ec_av1_syntax::{Av1Parser, ObuKind, QuantizationParams};

    /// The key frame the sequence in [`sample_1080p`] would carry: 1920x1080,
    /// one tile, quantised (so nothing is lossless), CDEF on.
    fn sample_key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            frame_is_intra: true,
            show_frame: true,
            showable_frame: false,
            error_resilient_mode: true,
            force_integer_mv: true,
            refresh_frame_flags: 0xFF,
            primary_ref_frame: PRIMARY_REF_NONE,
            order_hint: 0,
            frame_width: 1920,
            frame_height: 1080,
            upscaled_width: 1920,
            render_width: 1920,
            render_height: 1080,
            mi_cols: 480,
            mi_rows: 270,
            tile_info: ec_av1_syntax::TileInfo {
                uniform_spacing: true,
                cols: 1,
                rows: 1,
                cols_log2: 0,
                rows_log2: 0,
                mi_col_starts: vec![0, 480],
                mi_row_starts: vec![0, 270],
                context_update_tile_id: 0,
                tile_size_bytes: 1,
            },
            quantization: QuantizationParams {
                base_q_idx: 100,
                ..QuantizationParams::default()
            },
            loop_filter: ec_av1_syntax::LoopFilterParams {
                level: [12, 12, 0, 0],
                sharpness: 0,
                delta_enabled: true,
                delta_update: false,
                ref_deltas: DEFAULT_REF_DELTAS,
                mode_deltas: [0; 2],
            },
            cdef: ec_av1_syntax::CdefParams {
                damping: 4,
                bits: 0,
                y_pri_strength: [3, 0, 0, 0, 0, 0, 0, 0],
                y_sec_strength: [4, 0, 0, 0, 0, 0, 0, 0],
                uv_pri_strength: [2, 0, 0, 0, 0, 0, 0, 0],
                uv_sec_strength: [0; 8],
            },
            tx_mode: TxMode::Select,
            reduced_tx_set: false,
            ..FrameHeader::default()
        }
    }

    /// Parse a sequence header OBU followed by a frame OBU, and hand back what
    /// the frame OBU held.
    fn roundtrip(
        seq: &SequenceHeader,
        h: &FrameHeader,
        tile_data: &[u8],
    ) -> (FrameHeader, Vec<ec_av1_syntax::Tile>) {
        let mut stream = sequence_header_obu(seq).unwrap();
        let frame_start = stream.len();
        stream.extend_from_slice(&frame_obu(seq, h, tile_data).unwrap());

        let mut parser = Av1Parser::new();
        let obus = parser.parse_temporal_unit(&stream).unwrap();
        assert_eq!(obus.len(), 2);
        assert_eq!(obus[1].offset, frame_start);
        assert_eq!(obus[1].offset + obus[1].total_size, stream.len());
        match &obus[1].kind {
            ObuKind::Frame(parsed, tiles) => ((**parsed).clone(), tiles.clone()),
            other => panic!("expected an OBU_FRAME, got {other:?}"),
        }
    }

    #[test]
    fn key_frame_header_roundtrips() {
        let seq = sample_1080p();
        let mut expected = sample_key_frame();
        let (parsed, tiles) = roundtrip(&seq, &expected, &[0x11, 0x22, 0x33]);

        // The header's own length is derived, not written; check it against the
        // writer rather than copying it out of the parse.
        let mut w = BitWriter::new();
        write_frame_header(&mut w, &seq, &expected).unwrap();
        assert_eq!(parsed.header_bits, w.bit_len());
        expected.header_bits = parsed.header_bits;

        assert_eq!(parsed, expected);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].size, 3);
    }

    #[test]
    fn four_tile_columns_roundtrip() {
        let seq = sample_1080p();
        let mut expected = sample_key_frame();
        expected.tile_info = ec_av1_syntax::TileInfo {
            uniform_spacing: true,
            cols: 4,
            rows: 1,
            cols_log2: 2,
            rows_log2: 0,
            // 15 superblocks of 128 wide, 4 to a tile: 4, 4, 4, 3.
            mi_col_starts: vec![0, 128, 256, 384, 480],
            mi_row_starts: vec![0, 270],
            context_update_tile_id: 3,
            tile_size_bytes: 1,
        };
        // tile_start_and_end_present_flag = 0, then three sized tiles of one
        // byte each (the field codes size - 1) and one unsized last tile.
        let tile_data = [0x00, 0x00, 0x11, 0x00, 0x22, 0x00, 0x33, 0x44];
        let (parsed, tiles) = roundtrip(&seq, &expected, &tile_data);

        let mut w = BitWriter::new();
        write_frame_header(&mut w, &seq, &expected).unwrap();
        expected.header_bits = w.bit_len();
        assert_eq!(parsed, expected);
        assert_eq!(tiles.len(), 4);
        assert!(tiles.iter().all(|t| t.size == 1));
        assert_eq!(tiles[3].column, 3);
    }

    #[test]
    fn segmentation_and_delta_q_roundtrip() {
        let seq = sample_1080p();
        let mut expected = sample_key_frame();
        expected.segmentation.enabled = true;
        expected.segmentation.update_map = true;
        expected.segmentation.update_data = true;
        expected.segmentation.feature_enabled[0][0] = true;
        expected.segmentation.feature_data[0][0] = -40;
        expected.segmentation.feature_enabled[2][5] = true;
        expected.segmentation.feature_data[2][5] = 3;
        // Both are derived from the feature table, not coded.
        expected.segmentation.seg_id_pre_skip = true;
        expected.segmentation.last_active_seg_id = 2;
        expected.delta.q_present = true;
        expected.delta.q_res = 2;
        expected.delta.lf_present = true;
        expected.delta.lf_res = 1;
        expected.delta.lf_multi = true;

        let (parsed, _) = roundtrip(&seq, &expected, &[0x00]);
        let mut w = BitWriter::new();
        write_frame_header(&mut w, &seq, &expected).unwrap();
        expected.header_bits = w.bit_len();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn loop_restoration_roundtrips_with_its_own_type_codes() {
        let mut seq = sample_1080p();
        seq.enable_restoration = true;
        let mut expected = sample_key_frame();
        expected.loop_restoration.frame_restoration_type = [
            RestorationType::Sgrproj,
            RestorationType::Wiener,
            RestorationType::None,
        ];
        expected.loop_restoration.lr_unit_shift = 1;
        expected.loop_restoration.lr_uv_shift = 1;

        let (parsed, _) = roundtrip(&seq, &expected, &[0x00]);
        assert_eq!(
            parsed.loop_restoration.frame_restoration_type,
            expected.loop_restoration.frame_restoration_type
        );
        assert_eq!(parsed.loop_restoration.lr_unit_shift, 1);
        assert_eq!(parsed.loop_restoration.lr_uv_shift, 1);
    }

    #[test]
    fn unsupported_frames_are_refused_by_name() {
        let seq = sample_1080p();

        let mut inter = sample_key_frame();
        inter.frame_type = FrameType::Inter;
        assert!(frame_obu(&seq, &inter, &[0x00]).is_err());

        let mut hidden = sample_key_frame();
        hidden.show_frame = false;
        assert!(frame_obu(&seq, &hidden, &[0x00]).is_err());

        let mut scaled = sample_key_frame();
        scaled.superres_denom = 9;
        assert!(frame_obu(&seq, &scaled, &[0x00]).is_err());

        let mut ragged = sample_key_frame();
        ragged.tile_info.uniform_spacing = false;
        assert!(frame_obu(&seq, &ragged, &[0x00]).is_err());

        let mut grainy = seq.clone();
        grainy.film_grain_params_present = true;
        let mut grain_frame = sample_key_frame();
        grain_frame.film_grain.apply_grain = true;
        assert!(frame_obu(&grainy, &grain_frame, &[0x00]).is_err());
    }
}
