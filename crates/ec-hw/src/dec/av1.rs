//! Stateless AV1 decoding.

use std::sync::Arc;

use ec_av1_syntax::{
    Av1Parser, FrameHeader, FrameType, NUM_REF_FRAMES, ObuKind, REFS_PER_FRAME, SequenceHeader,
    Tile,
};
use ec_va::caps::Profile;
use ec_va::{Buffer, Display, sys};

use super::{ReadyFrames, Session, StreamInfo};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::params::av1::{
    PictureParameterBufferAV1, SliceParameterBufferAV1, WarpedMotionParamsAV1,
};
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// Reference slots plus the picture being decoded plus caller-held frames.
const EXTRA_SURFACES: usize = 6;

/// A stateless AV1 decoder.
pub struct Av1Decoder {
    display: Arc<Display>,
    parser: Av1Parser,
    session: Option<Session>,
    refs: [Option<Arc<PooledSurface>>; NUM_REF_FRAMES],
    ready: ReadyFrames,
}

impl Av1Decoder {
    /// A decoder with no stream state.
    pub fn new(display: &Arc<Display>) -> Av1Decoder {
        Av1Decoder {
            display: Arc::clone(display),
            parser: Av1Parser::new(),
            session: None,
            refs: [const { None }; NUM_REF_FRAMES],
            ready: ReadyFrames::default(),
        }
    }

    /// Decode one temporal unit — every OBU of it, in order.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // Tile offsets in the parse result are relative to `data`, and `data`
        // is what the driver is handed, so no rebasing is needed anywhere.
        let obus = self.parser.parse_temporal_unit(data)?;
        let mut pending: Option<Box<FrameHeader>> = None;
        for obu in obus {
            match obu.kind {
                ObuKind::FrameHeader(header) => {
                    if header.show_existing_frame {
                        self.show_existing(&header, timestamp)?;
                    } else {
                        pending = Some(header);
                    }
                }
                ObuKind::TileGroup(tiles) => {
                    if let Some(header) = pending.take() {
                        self.decode_frame(&header, &tiles, data, timestamp)?;
                    }
                }
                ObuKind::Frame(header, tiles) => {
                    if header.show_existing_frame {
                        self.show_existing(&header, timestamp)?;
                    } else {
                        self.decode_frame(&header, &tiles, data, timestamp)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The next frame to show.
    pub fn next_frame(&mut self) -> Option<Frame> {
        self.ready.pop()
    }

    /// Nothing is buffered beyond the reference slots.
    pub fn flush(&mut self) {}

    /// Drop reference state after a seek.
    pub fn reset(&mut self) {
        self.parser = Av1Parser::new();
        self.refs = [const { None }; NUM_REF_FRAMES];
        self.ready.clear();
    }

    /// What the stream turned out to be.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.session.as_ref().map(Session::info)
    }

    fn show_existing(&mut self, header: &FrameHeader, timestamp: i64) -> Result<()> {
        let slot = usize::from(header.frame_to_show_map_idx);
        let (Some(surface), Some(session)) = (
            self.refs.get(slot).and_then(|s| s.clone()),
            self.session.as_ref(),
        ) else {
            return Err(Error::Stream(ec_core::Error::corrupt(
                "AV1 show_existing_frame names an empty reference slot",
            )));
        };
        self.ready.push(Frame::new(
            Arc::clone(&surface),
            Arc::clone(&session.images),
            timestamp,
            (header.render_width, header.render_height),
            session.coded_size,
            session.bit_depth,
            None,
        ));
        // 7.21: showing a KEY frame again refreshes every reference slot with
        // it, which the reference *surfaces* have to follow too.
        if header.frame_type == FrameType::Key {
            for entry in &mut self.refs {
                *entry = Some(Arc::clone(&surface));
            }
        }
        Ok(())
    }

    fn decode_frame(
        &mut self,
        header: &FrameHeader,
        tiles: &[Tile],
        data: &[u8],
        timestamp: i64,
    ) -> Result<()> {
        let sequence = self
            .parser
            .sequence_header()
            .cloned()
            .ok_or_else(|| Error::config("AV1 frame before any sequence header"))?;
        self.ensure_session(&sequence, header)?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| Error::config("AV1 frame before any session"))?;
        let surface = session
            .pool
            .acquire()
            .ok_or_else(|| Error::config("AV1 decode: every surface is still in use"))?;

        let pic_param = self.picture_parameters(&sequence, header, surface.id());
        let mut buffers = Vec::with_capacity(tiles.len() + 2);
        buffers.push(param_buffer(&session.context, &pic_param)?);
        for tile in tiles {
            let slice = SliceParameterBufferAV1 {
                slice_data_size: tile.size as u32,
                slice_data_offset: tile.offset as u32,
                slice_data_flag: 0, // VA_SLICE_DATA_FLAG_ALL
                tile_row: tile.row as u16,
                tile_column: tile.column as u16,
                ..SliceParameterBufferAV1::default()
            };
            buffers.push(param_buffer(&session.context, &slice)?);
        }
        buffers.push(Buffer::from_bytes(
            &session.context,
            sys::VASliceDataBufferType,
            data,
        )?);
        session.submit(&surface, buffers)?;

        for slot in 0..NUM_REF_FRAMES {
            if header.refresh_frame_flags & (1 << slot) != 0 {
                self.refs[slot] = Some(Arc::clone(&surface));
            }
        }

        if header.show_frame {
            self.ready.push(Frame::new(
                surface,
                Arc::clone(&session.images),
                timestamp,
                (header.render_width, header.render_height),
                session.coded_size,
                session.bit_depth,
                None,
            ));
        }
        Ok(())
    }

    fn ensure_session(&mut self, seq: &SequenceHeader, header: &FrameHeader) -> Result<()> {
        let bit_depth = seq.color_config.bit_depth;
        let profile = match (seq.seq_profile, bit_depth) {
            (0, _) => Profile::AV1Profile0,
            (2, _) => Profile::AV1Profile2,
            (other, _) => {
                return Err(Error::unsupported(
                    format!("AV1 profile {other}"),
                    "only the 4:2:0 profiles 0 and 2 are decoded in hardware",
                ));
            }
        };
        // Superblocks are 64x64 or 128x128; a surface holds whole ones.
        let sb = if seq.use_128x128_superblock { 128 } else { 64 };
        let coded = (
            header.upscaled_width.next_multiple_of(sb).max(sb),
            header.frame_height.next_multiple_of(sb).max(sb),
        );
        if let Some(session) = &self.session
            && session.coded_size.0 >= coded.0
            && session.coded_size.1 >= coded.1
            && session.bit_depth == bit_depth
            && session.profile == profile
        {
            return Ok(());
        }
        self.refs = [const { None }; NUM_REF_FRAMES];
        self.session = Some(Session::new(
            &self.display,
            profile,
            coded,
            (header.render_width, header.render_height),
            bit_depth,
            NUM_REF_FRAMES + EXTRA_SURFACES,
        )?);
        Ok(())
    }

    fn picture_parameters(
        &self,
        seq: &SequenceHeader,
        h: &FrameHeader,
        current: sys::VASurfaceID,
    ) -> PictureParameterBufferAV1 {
        let sb_shift = if seq.use_128x128_superblock { 5 } else { 4 };
        let mut p = PictureParameterBufferAV1 {
            profile: seq.seq_profile,
            order_hint_bits_minus_1: seq.order_hint_bits.saturating_sub(1) as u8,
            bit_depth_idx: match seq.color_config.bit_depth {
                8 => 0,
                10 => 1,
                _ => 2,
            },
            matrix_coefficients: seq.color_config.matrix_coefficients,
            current_frame: current,
            current_display_picture: current,
            frame_width_minus1: (h.upscaled_width.max(1) - 1) as u16,
            frame_height_minus1: (h.frame_height.max(1) - 1) as u16,
            primary_ref_frame: h.primary_ref_frame,
            order_hint: h.order_hint as u8,
            tile_cols: h.tile_info.cols.min(255) as u8,
            tile_rows: h.tile_info.rows.min(255) as u8,
            tile_count_minus_1: (h.tile_info.cols * h.tile_info.rows).max(1) as u16 - 1,
            context_update_tile_id: h.tile_info.context_update_tile_id as u16,
            superres_scale_denominator: h.superres_denom,
            interp_filter: h.interpolation_filter as u8,
            filter_level: [h.loop_filter.level[0], h.loop_filter.level[1]],
            filter_level_u: h.loop_filter.level[2],
            filter_level_v: h.loop_filter.level[3],
            ref_deltas: h.loop_filter.ref_deltas,
            mode_deltas: h.loop_filter.mode_deltas,
            base_qindex: h.quantization.base_q_idx,
            y_dc_delta_q: h.quantization.delta_q_y_dc,
            u_dc_delta_q: h.quantization.delta_q_u_dc,
            u_ac_delta_q: h.quantization.delta_q_u_ac,
            v_dc_delta_q: h.quantization.delta_q_v_dc,
            v_ac_delta_q: h.quantization.delta_q_v_ac,
            cdef_damping_minus_3: h.cdef.damping.saturating_sub(3),
            cdef_bits: h.cdef.bits,
            cdef_y_strengths: h.cdef.y_strengths(),
            cdef_uv_strengths: h.cdef.uv_strengths(),
            ..PictureParameterBufferAV1::default()
        };

        for (slot, surface) in self.refs.iter().enumerate() {
            if let Some(surface) = surface {
                p.ref_frame_map[slot] = surface.id();
            }
        }
        for i in 0..REFS_PER_FRAME {
            p.ref_frame_idx[i] = h.ref_frame_idx[i];
        }

        for (i, sbs) in h
            .tile_info
            .width_in_sbs_minus_1(sb_shift)
            .into_iter()
            .take(p.width_in_sbs_minus_1.len())
            .enumerate()
        {
            p.width_in_sbs_minus_1[i] = sbs;
        }
        for (i, sbs) in h
            .tile_info
            .height_in_sbs_minus_1(sb_shift)
            .into_iter()
            .take(p.height_in_sbs_minus_1.len())
            .enumerate()
        {
            p.height_in_sbs_minus_1[i] = sbs;
        }

        p.seq_info_fields = p
            .seq_info_fields
            .still_picture(u32::from(seq.still_picture))
            .use_128x128_superblock(u32::from(seq.use_128x128_superblock))
            .enable_filter_intra(u32::from(seq.enable_filter_intra))
            .enable_intra_edge_filter(u32::from(seq.enable_intra_edge_filter))
            .enable_interintra_compound(u32::from(seq.enable_interintra_compound))
            .enable_masked_compound(u32::from(seq.enable_masked_compound))
            .enable_dual_filter(u32::from(seq.enable_dual_filter))
            .enable_order_hint(u32::from(seq.enable_order_hint))
            .enable_jnt_comp(u32::from(seq.enable_jnt_comp))
            .enable_cdef(u32::from(seq.enable_cdef))
            .mono_chrome(u32::from(seq.color_config.mono_chrome))
            .color_range(u32::from(seq.color_config.color_range))
            .subsampling_x(u32::from(seq.color_config.subsampling_x))
            .subsampling_y(u32::from(seq.color_config.subsampling_y))
            .film_grain_params_present(u32::from(seq.film_grain_params_present));

        p.pic_info_fields = p
            .pic_info_fields
            .frame_type(h.frame_type as u32)
            .show_frame(u32::from(h.show_frame))
            .showable_frame(u32::from(h.showable_frame))
            .error_resilient_mode(u32::from(h.error_resilient_mode))
            .disable_cdf_update(u32::from(h.disable_cdf_update))
            .allow_screen_content_tools(u32::from(h.allow_screen_content_tools))
            .force_integer_mv(u32::from(h.force_integer_mv))
            .allow_intrabc(u32::from(h.allow_intrabc))
            .use_superres(u32::from(h.use_superres))
            .allow_high_precision_mv(u32::from(h.allow_high_precision_mv))
            .is_motion_mode_switchable(u32::from(h.is_motion_mode_switchable))
            .use_ref_frame_mvs(u32::from(h.use_ref_frame_mvs))
            .disable_frame_end_update_cdf(u32::from(h.disable_frame_end_update_cdf))
            .uniform_tile_spacing_flag(u32::from(h.tile_info.uniform_spacing))
            .allow_warped_motion(u32::from(h.allow_warped_motion));

        p.loop_filter_info_fields = p
            .loop_filter_info_fields
            .sharpness_level(h.loop_filter.sharpness)
            .mode_ref_delta_enabled(u8::from(h.loop_filter.delta_enabled))
            .mode_ref_delta_update(u8::from(h.loop_filter.delta_update));

        p.qmatrix_fields = p
            .qmatrix_fields
            .using_qmatrix(u16::from(h.quantization.using_qmatrix))
            .qm_y(u16::from(h.quantization.qm_y))
            .qm_u(u16::from(h.quantization.qm_u))
            .qm_v(u16::from(h.quantization.qm_v));

        p.mode_control_fields = p
            .mode_control_fields
            .delta_q_present_flag(u32::from(h.delta.q_present))
            .log2_delta_q_res(u32::from(h.delta.q_res))
            .delta_lf_present_flag(u32::from(h.delta.lf_present))
            .log2_delta_lf_res(u32::from(h.delta.lf_res))
            .delta_lf_multi(u32::from(h.delta.lf_multi))
            .tx_mode(h.tx_mode as u32)
            .reference_select(u32::from(h.reference_select))
            .reduced_tx_set_used(u32::from(h.reduced_tx_set))
            .skip_mode_present(u32::from(h.skip_mode_present));

        p.loop_restoration_fields = p
            .loop_restoration_fields
            .yframe_restoration_type(h.loop_restoration.frame_restoration_type[0] as u16)
            .cbframe_restoration_type(h.loop_restoration.frame_restoration_type[1] as u16)
            .crframe_restoration_type(h.loop_restoration.frame_restoration_type[2] as u16)
            .lr_unit_shift(u16::from(h.loop_restoration.lr_unit_shift))
            .lr_uv_shift(u16::from(h.loop_restoration.lr_uv_shift));

        p.seg_info.segment_info_fields = p
            .seg_info
            .segment_info_fields
            .enabled(u32::from(h.segmentation.enabled))
            .update_map(u32::from(h.segmentation.update_map))
            .temporal_update(u32::from(h.segmentation.temporal_update))
            .update_data(u32::from(h.segmentation.update_data));
        for segment in 0..ec_av1_syntax::MAX_SEGMENTS {
            p.seg_info.feature_data[segment] = h.segmentation.feature_data[segment];
            p.seg_info.feature_mask[segment] = h.segmentation.feature_mask(segment);
        }

        for i in 0..REFS_PER_FRAME {
            let warp = &h.global_motion[i];
            let mut wm = WarpedMotionParamsAV1 {
                wmtype: warp.model as i32,
                invalid: u8::from(warp.invalid),
                ..WarpedMotionParamsAV1::default()
            };
            wm.wmmat[..6].copy_from_slice(&warp.params);
            p.wm[i] = wm;
        }

        // Film grain is a display-side effect, and edith wants the ungrained
        // reconstruction to grade and scale; the parameters are still carried
        // so a caller that wants grain has them.
        p.film_grain_info.film_grain_info_fields =
            p.film_grain_info.film_grain_info_fields.apply_grain(0);
        p
    }
}
