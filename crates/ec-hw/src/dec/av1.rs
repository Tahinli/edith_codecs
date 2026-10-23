//! Stateless AV1 decoding.

use std::rc::Rc;

use ec_av1_syntax::{
    Av1Parser, FilmGrainParams, FrameHeader, FrameType, NUM_REF_FRAMES, ObuKind, REFS_PER_FRAME,
    SequenceHeader, Tile,
};
use ec_va::caps::Profile;
use ec_va::va::BufferType;
use ec_va::{Display, picture};

use super::{ReadyFrames, Session, StreamInfo};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::params::av1::{
    FilmGrainInfoFields, FilmGrainStructAV1, LoopFilterInfoFields, LoopRestorationFields,
    ModeControlFields, PicInfoFields, PictureParameterBufferAV1, QMatrixFields, SegmentInfoFields,
    SeqInfoFields, SliceParameterBufferAV1, WarpedMotionParamsAV1, slice_buffer,
};
use crate::pool::PooledSurface;

/// Reference slots plus the picture being decoded plus caller-held frames.
const EXTRA_SURFACES: usize = 6;

/// Grain outputs held alongside the reference slots, plus the one being
/// decoded: a grained picture is a second surface, so a stream with film grain
/// needs room for both. Zero for a stream without grain.
const GRAIN_SURFACES: usize = NUM_REF_FRAMES + 2;

/// A stateless AV1 decoder.
pub struct Av1Decoder {
    display: Rc<Display>,
    parser: Av1Parser,
    session: Option<Session>,
    /// Each slot's grain-free reconstruction: what later frames predict from.
    refs: [Option<Rc<PooledSurface>>; NUM_REF_FRAMES],
    /// Each slot's grained copy, when the frame in it was decoded with film
    /// grain. A `show_existing_frame` outputs this copy (spec 7.21 reloads the
    /// grain parameters of the frame it shows), so the driver's grain pass runs
    /// once, at decode time, and the slot keeps the result.
    grained: [Option<Rc<PooledSurface>>; NUM_REF_FRAMES],
    ready: ReadyFrames,
}

impl Av1Decoder {
    /// A decoder with no stream state.
    pub fn new(display: &Rc<Display>) -> Av1Decoder {
        Av1Decoder {
            display: Rc::clone(display),
            parser: Av1Parser::new(),
            session: None,
            refs: [const { None }; NUM_REF_FRAMES],
            grained: [const { None }; NUM_REF_FRAMES],
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
        self.grained = [const { None }; NUM_REF_FRAMES];
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
        // The grained copy when the shown frame carries grain, else the
        // reconstruction itself.
        let out = self.grained[slot]
            .clone()
            .unwrap_or_else(|| Rc::clone(&surface));
        self.ready.push(Frame::new(
            out,
            Rc::clone(&self.display),
            timestamp,
            (header.render_width, header.render_height),
            session.coded_size,
            session.bit_depth,
            None,
        ));
        // 7.21: showing a KEY frame again refreshes every reference slot with
        // it, which the reference *surfaces* have to follow too.
        if header.frame_type == FrameType::Key {
            let grained = self.grained[slot].clone();
            for entry in &mut self.refs {
                *entry = Some(Rc::clone(&surface));
            }
            for entry in &mut self.grained {
                entry.clone_from(&grained);
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

        // An inter frame predicts from the seven reference slots it names, and
        // the spec requires each of them to hold a decoded frame (RefValid).
        // A slot never filled, from a cut stream or a GOP that opens on an
        // inter frame, has no picture for the driver to read, so refuse here
        // rather than hand it VA_INVALID_SURFACE and output what it makes of it.
        if matches!(header.frame_type, FrameType::Inter | FrameType::Switch) {
            for (ref_index, &slot) in header.ref_frame_idx.iter().enumerate() {
                if self.refs[usize::from(slot)].is_none() {
                    return Err(Error::Stream(ec_core::Error::corrupt(format!(
                        "AV1 inter frame names reference slot {slot} (ref {ref_index}), \
                         which no decoded frame has filled"
                    ))));
                }
            }
        }

        let session = self
            .session
            .as_ref()
            .ok_or_else(|| Error::config("AV1 frame before any session"))?;
        let surface = session
            .pool
            .acquire()
            .ok_or_else(|| Error::config("AV1 decode: every surface is still in use"))?;

        // Film grain (spec 7.18.3) is applied by the driver onto a separate
        // display surface, so the reconstruction the references point at stays
        // grain-free and the grained copy is what gets shown.
        let grained = if wants_grain(&sequence, header) {
            Some(session.pool.acquire().ok_or_else(|| {
                Error::config("AV1 decode: every surface is still in use (grain output)")
            })?)
        } else {
            None
        };
        let display_id = grained.as_ref().map_or(surface.id(), |g| g.id());

        let pic_param = picture_parameters(
            &sequence,
            header,
            &slot_ids(&self.refs),
            surface.id(),
            display_id,
        );
        let mut buffers = Vec::with_capacity(3);
        buffers.push(picture::buffer(&session.context, pic_param.to_buffer())?);
        if !tiles.is_empty() {
            let slices: Vec<SliceParameterBufferAV1> = tiles.iter().map(tile_parameters).collect();
            buffers.push(picture::buffer(&session.context, slice_buffer(&slices))?);
        }
        buffers.push(picture::buffer(
            &session.context,
            BufferType::SliceData(data.to_vec()),
        )?);
        session.submit(&surface, buffers)?;

        for slot in 0..NUM_REF_FRAMES {
            if header.refresh_frame_flags & (1 << slot) != 0 {
                self.refs[slot] = Some(Rc::clone(&surface));
                self.grained[slot].clone_from(&grained);
            }
        }

        if header.show_frame {
            let out = grained.unwrap_or_else(|| Rc::clone(&surface));
            self.ready.push(Frame::new(
                out,
                Rc::clone(&self.display),
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
        self.grained = [const { None }; NUM_REF_FRAMES];
        let grain_surfaces = if seq.film_grain_params_present {
            GRAIN_SURFACES
        } else {
            0
        };
        self.session = Some(Session::new(
            &self.display,
            profile,
            coded,
            (header.render_width, header.render_height),
            bit_depth,
            NUM_REF_FRAMES + EXTRA_SURFACES + grain_surfaces,
        )?);
        Ok(())
    }
}

fn picture_parameters(
    seq: &SequenceHeader,
    h: &FrameHeader,
    ref_ids: &[Option<u32>; NUM_REF_FRAMES],
    current: u32,
    display: u32,
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
        current_display_picture: display,
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
    for (slot, id) in ref_ids.iter().enumerate() {
        if let Some(id) = id {
            p.ref_frame_map[slot] = *id;
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
    p.seq_info_fields = SeqInfoFields {
        still_picture: u32::from(seq.still_picture),
        use_128x128_superblock: u32::from(seq.use_128x128_superblock),
        enable_filter_intra: u32::from(seq.enable_filter_intra),
        enable_intra_edge_filter: u32::from(seq.enable_intra_edge_filter),
        enable_interintra_compound: u32::from(seq.enable_interintra_compound),
        enable_masked_compound: u32::from(seq.enable_masked_compound),
        enable_dual_filter: u32::from(seq.enable_dual_filter),
        enable_order_hint: u32::from(seq.enable_order_hint),
        enable_jnt_comp: u32::from(seq.enable_jnt_comp),
        enable_cdef: u32::from(seq.enable_cdef),
        mono_chrome: u32::from(seq.color_config.mono_chrome),
        color_range: u32::from(seq.color_config.color_range),
        subsampling_x: u32::from(seq.color_config.subsampling_x),
        subsampling_y: u32::from(seq.color_config.subsampling_y),
        chroma_sample_position: 0,
        film_grain_params_present: u32::from(seq.film_grain_params_present),
    };
    p.pic_info_fields = PicInfoFields {
        frame_type: h.frame_type as u32,
        show_frame: u32::from(h.show_frame),
        showable_frame: u32::from(h.showable_frame),
        error_resilient_mode: u32::from(h.error_resilient_mode),
        disable_cdf_update: u32::from(h.disable_cdf_update),
        allow_screen_content_tools: u32::from(h.allow_screen_content_tools),
        force_integer_mv: u32::from(h.force_integer_mv),
        allow_intrabc: u32::from(h.allow_intrabc),
        use_superres: u32::from(h.use_superres),
        allow_high_precision_mv: u32::from(h.allow_high_precision_mv),
        is_motion_mode_switchable: u32::from(h.is_motion_mode_switchable),
        use_ref_frame_mvs: u32::from(h.use_ref_frame_mvs),
        disable_frame_end_update_cdf: u32::from(h.disable_frame_end_update_cdf),
        uniform_tile_spacing_flag: u32::from(h.tile_info.uniform_spacing),
        allow_warped_motion: u32::from(h.allow_warped_motion),
        large_scale_tile: 0,
    };
    p.loop_filter_info_fields = LoopFilterInfoFields {
        sharpness_level: h.loop_filter.sharpness,
        mode_ref_delta_enabled: u8::from(h.loop_filter.delta_enabled),
        mode_ref_delta_update: u8::from(h.loop_filter.delta_update),
    };
    p.qmatrix_fields = QMatrixFields {
        using_qmatrix: u16::from(h.quantization.using_qmatrix),
        qm_y: u16::from(h.quantization.qm_y),
        qm_u: u16::from(h.quantization.qm_u),
        qm_v: u16::from(h.quantization.qm_v),
    };
    p.mode_control_fields = ModeControlFields {
        delta_q_present_flag: u32::from(h.delta.q_present),
        log2_delta_q_res: u32::from(h.delta.q_res),
        delta_lf_present_flag: u32::from(h.delta.lf_present),
        log2_delta_lf_res: u32::from(h.delta.lf_res),
        delta_lf_multi: u32::from(h.delta.lf_multi),
        tx_mode: h.tx_mode as u32,
        reference_select: u32::from(h.reference_select),
        reduced_tx_set_used: u32::from(h.reduced_tx_set),
        skip_mode_present: u32::from(h.skip_mode_present),
    };
    p.loop_restoration_fields = LoopRestorationFields {
        yframe_restoration_type: h.loop_restoration.frame_restoration_type[0] as u16,
        cbframe_restoration_type: h.loop_restoration.frame_restoration_type[1] as u16,
        crframe_restoration_type: h.loop_restoration.frame_restoration_type[2] as u16,
        lr_unit_shift: u16::from(h.loop_restoration.lr_unit_shift),
        lr_uv_shift: u16::from(h.loop_restoration.lr_uv_shift),
    };
    p.seg_info.segment_info_fields = SegmentInfoFields {
        enabled: u32::from(h.segmentation.enabled),
        update_map: u32::from(h.segmentation.update_map),
        temporal_update: u32::from(h.segmentation.temporal_update),
        update_data: u32::from(h.segmentation.update_data),
    };
    for segment in 0..ec_av1_syntax::MAX_SEGMENTS {
        p.seg_info.feature_data[segment] = h.segmentation.feature_data[segment];
        p.seg_info.feature_mask[segment] = h.segmentation.feature_mask(segment);
    }
    for i in 0..REFS_PER_FRAME {
        let warp = &h.global_motion[i];
        let mut wm = WarpedMotionParamsAV1 {
            wmtype: warp.model as u32,
            invalid: u8::from(warp.invalid),
            ..WarpedMotionParamsAV1::default()
        };
        wm.wmmat[..6].copy_from_slice(&warp.params);
        p.wm[i] = wm;
    }
    if wants_grain(seq, h) {
        p.film_grain_info = film_grain_struct(&h.film_grain);
    }
    p
}

/// The surface id in each reference slot, `None` for an empty one.
fn slot_ids(refs: &[Option<Rc<PooledSurface>>; NUM_REF_FRAMES]) -> [Option<u32>; NUM_REF_FRAMES] {
    refs.each_ref().map(|s| s.as_ref().map(|s| s.id()))
}

/// The slice parameters of one tile.
fn tile_parameters(tile: &Tile) -> SliceParameterBufferAV1 {
    SliceParameterBufferAV1 {
        slice_data_size: tile.size as u32,
        slice_data_offset: tile.offset as u32,
        slice_data_flag: 0, // VA_SLICE_DATA_FLAG_ALL
        tile_row: tile.row as u16,
        tile_column: tile.column as u16,
        ..SliceParameterBufferAV1::default()
    }
}

/// Feeds a run of AV1 temporal units (see `super::units`) through the
/// parameter builders, with the sequence, frame and tile OBUs the real parser
/// produces. One parser runs across the units, as the decoder's does, so a frame
/// sees the sequence header and reference state the earlier units left. No GPU.
/// Returns the number of frames built.
#[doc(hidden)]
pub fn fuzz_build_parameters(data: &[u8]) -> usize {
    let mut parser = Av1Parser::new();
    let mut built = 0usize;
    for unit in super::units(data) {
        let Ok(obus) = parser.parse_temporal_unit(unit) else {
            continue;
        };
        let Some(seq) = parser.sequence_header().cloned() else {
            continue;
        };
        let mut pending: Option<Box<FrameHeader>> = None;
        for obu in obus {
            let (header, tiles) = match obu.kind {
                ObuKind::FrameHeader(header) => {
                    pending = Some(header);
                    continue;
                }
                ObuKind::TileGroup(tiles) => match pending.take() {
                    Some(header) => (header, tiles),
                    None => continue,
                },
                ObuKind::Frame(header, tiles) => (header, tiles),
                _ => continue,
            };
            if header.show_existing_frame {
                continue;
            }
            let mut ids = [None; NUM_REF_FRAMES];
            for (slot, id) in ids.iter_mut().enumerate() {
                if !(slot + data.len()).is_multiple_of(3) {
                    *id = Some(0x100 + slot as u32);
                }
            }
            let pic = picture_parameters(&seq, &header, &ids, 0x200, 0x201);
            let slices: Vec<SliceParameterBufferAV1> = tiles.iter().map(tile_parameters).collect();
            std::hint::black_box((pic.to_buffer(), slice_buffer(&slices)));
            built += 1;
        }
    }
    built
}

/// Whether this frame's grained picture is produced: the stream carries film
/// grain, and the frame is shown now or later (`showable_frame`, shown through
/// `show_existing_frame`) with `apply_grain` set. Spec 5.9.30 reads the grain
/// parameters under the same condition.
fn wants_grain(seq: &SequenceHeader, h: &FrameHeader) -> bool {
    seq.film_grain_params_present && (h.show_frame || h.showable_frame) && h.film_grain.apply_grain
}

/// `VAFilmGrainStructAV1` from the frame header's `film_grain_params`.
fn film_grain_struct(fg: &FilmGrainParams) -> FilmGrainStructAV1 {
    FilmGrainStructAV1 {
        film_grain_info_fields: FilmGrainInfoFields {
            apply_grain: 1,
            chroma_scaling_from_luma: u32::from(fg.chroma_scaling_from_luma),
            grain_scaling_minus_8: u32::from(fg.grain_scaling_minus_8),
            ar_coeff_lag: u32::from(fg.ar_coeff_lag),
            ar_coeff_shift_minus_6: u32::from(fg.ar_coeff_shift_minus_6),
            grain_scale_shift: u32::from(fg.grain_scale_shift),
            overlap_flag: u32::from(fg.overlap_flag),
            clip_to_restricted_range: u32::from(fg.clip_to_restricted_range),
        },
        grain_seed: fg.grain_seed,
        num_y_points: fg.num_y_points,
        point_y_value: fg.point_y_value,
        point_y_scaling: fg.point_y_scaling,
        num_cb_points: fg.num_cb_points,
        point_cb_value: fg.point_cb_value,
        point_cb_scaling: fg.point_cb_scaling,
        num_cr_points: fg.num_cr_points,
        point_cr_value: fg.point_cr_value,
        point_cr_scaling: fg.point_cr_scaling,
        ar_coeffs_y: fg.ar_coeffs_y,
        ar_coeffs_cb: fg.ar_coeffs_cb,
        ar_coeffs_cr: fg.ar_coeffs_cr,
        cb_mult: fg.cb_mult,
        cb_luma_mult: fg.cb_luma_mult,
        cb_offset: fg.cb_offset,
        cr_mult: fg.cr_mult,
        cr_luma_mult: fg.cr_luma_mult,
        cr_offset: fg.cr_offset,
    }
}
