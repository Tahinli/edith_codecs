//! Stateless VP9 decoding.

use std::rc::Rc;

use ec_va::Display;
use ec_va::caps::Profile;
use ec_va::va::BufferType;
use ec_vp9_syntax::{
    FrameHeader, FrameType, MAX_SEGMENTS, NUM_REF_FRAMES, SEG_LVL_REF_FRAME, SEG_LVL_SKIP,
    Vp9Parser, superframe,
};

use super::{ReadyFrames, Session, StreamInfo};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::params::vp9::{
    PicFields, PictureParameterBufferVP9, SegmentFlags, SegmentParameterVP9,
    SliceParameterBufferVP9,
};
use crate::pool::PooledSurface;

/// Reference slots plus the picture being decoded plus caller-held frames.
const EXTRA_SURFACES: usize = 6;

/// A stateless VP9 decoder.
pub struct Vp9Decoder {
    display: Rc<Display>,
    parser: Vp9Parser,
    session: Option<Session>,
    /// The eight reference slots, as surfaces.
    refs: [Option<Rc<PooledSurface>>; NUM_REF_FRAMES],
    ready: ReadyFrames,
}

impl Vp9Decoder {
    /// A decoder with no stream state.
    pub fn new(display: &Rc<Display>) -> Vp9Decoder {
        Vp9Decoder {
            display: Rc::clone(display),
            parser: Vp9Parser::new(),
            session: None,
            refs: [const { None }; NUM_REF_FRAMES],
            ready: ReadyFrames::default(),
        }
    }

    /// Decode one chunk, which may be a superframe holding several frames.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // A superframe packs a hidden ALTREF and the frame that shows it into
        // one chunk; the driver is handed each frame separately, and the offsets
        // matter because the picture parameters describe one frame at a time.
        let frames = superframe::split(data)?;
        for frame in frames {
            self.decode_frame(frame, timestamp)?;
        }
        Ok(())
    }

    /// The next frame in display order (VP9 codes in display order).
    pub fn next_frame(&mut self) -> Option<Frame> {
        self.ready.pop()
    }

    /// Nothing is buffered beyond the reference slots, so this is a no-op.
    pub fn flush(&mut self) {}

    /// Drop reference state after a seek.
    pub fn reset(&mut self) {
        self.parser = Vp9Parser::new();
        self.refs = [const { None }; NUM_REF_FRAMES];
        self.ready.clear();
    }

    /// What the stream turned out to be.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.session.as_ref().map(Session::info)
    }

    fn decode_frame(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        let header = self.parser.parse_frame(data)?;

        if header.show_existing_frame {
            // No decode: the frame in that slot is shown again. It keeps its
            // slot as well, so the surface is shared, not moved.
            let slot = usize::from(header.frame_to_show_map_idx);
            let (Some(surface), Some(session)) = (
                self.refs.get(slot).and_then(|s| s.clone()),
                self.session.as_ref(),
            ) else {
                return Err(Error::Stream(ec_core::Error::corrupt(
                    "VP9 show_existing_frame names an empty reference slot",
                )));
            };
            self.ready.push(Frame::new(
                surface,
                Rc::clone(&self.display),
                timestamp,
                (header.width, header.height),
                session.coded_size,
                session.bit_depth,
                None,
            ));
            return Ok(());
        }

        self.ensure_session(&header)?;
        // An inter frame predicts from LAST, GOLDEN and ALTREF, which name
        // three reference slots. A slot no decoded frame filled has no picture,
        // and the driver would read VA_INVALID_SURFACE: refuse the broken GOP.
        if header.frame_type == FrameType::Inter {
            for (ref_index, &slot) in header.ref_frame_idx[..3].iter().enumerate() {
                if self.refs[usize::from(slot)].is_none() {
                    return Err(Error::Stream(ec_core::Error::corrupt(format!(
                        "VP9 inter frame names reference slot {slot} (ref {ref_index}), \
                         which no decoded frame has filled"
                    ))));
                }
            }
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| Error::config("VP9 frame before any session"))?;
        let surface = session
            .pool
            .acquire()
            .ok_or_else(|| Error::config("VP9 decode: every surface is still in use"))?;

        let pic_param = picture_parameters(&header, &slot_ids(&self.refs));
        let slice_param = slice_parameters(&header, data.len());
        let buffers = vec![
            ec_va::picture::buffer(&session.context, pic_param.to_buffer())?,
            ec_va::picture::buffer(&session.context, slice_param.to_buffer())?,
            ec_va::picture::buffer(&session.context, BufferType::SliceData(data.to_vec()))?,
        ];
        session.submit(&surface, buffers)?;

        // Reference slot refresh (spec 8.10), after the submission that used
        // the old contents.
        for slot in 0..NUM_REF_FRAMES {
            if header.refresh_frame_flags & (1 << slot) != 0 {
                self.refs[slot] = Some(Rc::clone(&surface));
            }
        }

        if header.show_frame {
            self.ready.push(Frame::new(
                surface,
                Rc::clone(&self.display),
                timestamp,
                (header.width, header.height),
                session.coded_size,
                session.bit_depth,
                None,
            ));
        }
        Ok(())
    }

    fn ensure_session(&mut self, header: &FrameHeader) -> Result<()> {
        let profile = match header.profile {
            0 => Profile::VP9Profile0,
            2 => Profile::VP9Profile2,
            other => {
                return Err(Error::unsupported(
                    format!("VP9 profile {other}"),
                    "only the 4:2:0 profiles 0 and 2 are decoded in hardware",
                ));
            }
        };
        // VP9 codes in 64x64 superblocks; a surface has to hold whole ones.
        let coded = (
            header.width.next_multiple_of(64).max(64),
            header.height.next_multiple_of(64).max(64),
        );
        if let Some(session) = &self.session
            && session.coded_size.0 >= coded.0
            && session.coded_size.1 >= coded.1
            && session.bit_depth == header.bit_depth
            && session.profile == profile
        {
            return Ok(());
        }
        self.refs = [const { None }; NUM_REF_FRAMES];
        self.session = Some(Session::new(
            &self.display,
            profile,
            coded,
            (header.width, header.height),
            header.bit_depth,
            NUM_REF_FRAMES + EXTRA_SURFACES,
        )?);
        Ok(())
    }
}

fn picture_parameters(
    h: &FrameHeader,
    ref_ids: &[Option<u32>; NUM_REF_FRAMES],
) -> PictureParameterBufferVP9 {
    let mut p = PictureParameterBufferVP9 {
        frame_width: h.width as u16,
        frame_height: h.height as u16,
        filter_level: h.loop_filter.level,
        sharpness_level: h.loop_filter.sharpness,
        log2_tile_rows: h.tile_info.rows_log2,
        log2_tile_columns: h.tile_info.cols_log2,
        frame_header_length_in_bytes: h.uncompressed_header_size,
        first_partition_size: h.header_size_in_bytes,
        mb_segment_tree_probs: h.segmentation.tree_probs,
        segment_pred_probs: h.segmentation.pred_probs,
        profile: h.profile,
        bit_depth: h.bit_depth,
        ..PictureParameterBufferVP9::default()
    };
    for (slot, id) in ref_ids.iter().enumerate() {
        if let Some(id) = id {
            p.reference_frames[slot] = *id;
        }
    }
    p.pic_fields = PicFields {
        subsampling_x: u32::from(h.subsampling_x),
        subsampling_y: u32::from(h.subsampling_y),
        frame_type: u32::from(h.frame_type == FrameType::Inter),
        show_frame: u32::from(h.show_frame),
        error_resilient_mode: u32::from(h.error_resilient_mode),
        intra_only: u32::from(h.intra_only),
        allow_high_precision_mv: u32::from(h.allow_high_precision_mv),
        mcomp_filter_type: h.interpolation_filter as u32,
        frame_parallel_decoding_mode: u32::from(h.frame_parallel_decoding_mode),
        reset_frame_context: u32::from(h.reset_frame_context),
        refresh_frame_context: u32::from(h.refresh_frame_context),
        frame_context_idx: u32::from(h.frame_context_idx),
        segmentation_enabled: u32::from(h.segmentation.enabled),
        segmentation_temporal_update: u32::from(h.segmentation.temporal_update),
        segmentation_update_map: u32::from(h.segmentation.update_map),
        last_ref_frame: u32::from(h.ref_frame_idx[0]),
        last_ref_frame_sign_bias: u32::from(h.ref_frame_sign_bias[0]),
        golden_ref_frame: u32::from(h.ref_frame_idx[1]),
        golden_ref_frame_sign_bias: u32::from(h.ref_frame_sign_bias[1]),
        alt_ref_frame: u32::from(h.ref_frame_idx[2]),
        alt_ref_frame_sign_bias: u32::from(h.ref_frame_sign_bias[2]),
        lossless_flag: u32::from(h.quantization.lossless()),
    };
    p
}

/// The surface id in each reference slot, `None` for an empty one.
fn slot_ids(refs: &[Option<Rc<PooledSurface>>; NUM_REF_FRAMES]) -> [Option<u32>; NUM_REF_FRAMES] {
    refs.each_ref().map(|s| s.as_ref().map(|s| s.id()))
}

/// Feeds a run of VP9 chunks (each a frame or a superframe, see
/// `super::units`) through the parameter builders, with the frame headers the
/// real parser produces. One parser runs across the chunks, as the decoder's does,
/// so inter frames see the references the earlier frames left. No GPU. Returns the
/// number of frames built.
#[doc(hidden)]
pub fn fuzz_build_parameters(data: &[u8]) -> usize {
    let mut parser = Vp9Parser::new();
    let mut built = 0usize;
    for (i, chunk) in super::units(data).into_iter().enumerate() {
        let Ok(frames) = superframe::split(chunk) else {
            continue;
        };
        for frame in frames {
            let Ok(header) = parser.parse_frame(frame) else {
                continue;
            };
            if header.show_existing_frame {
                continue;
            }
            let mut ids = [None; NUM_REF_FRAMES];
            for (slot, id) in ids.iter_mut().enumerate() {
                if !(slot + i + data.len()).is_multiple_of(3) {
                    *id = Some(0x100 + slot as u32);
                }
            }
            let pic = picture_parameters(&header, &ids);
            let slice = slice_parameters(&header, frame.len());
            std::hint::black_box((pic.to_buffer(), slice.to_buffer()));
            built += 1;
        }
    }
    built
}

/// The slice (whole-frame) parameters, including all eight segments.
///
/// Every segment is filled whether or not segmentation is enabled: with it off,
/// segment 0 is the one the driver uses, and the other seven are harmless — but
/// leaving them zeroed would hand the driver a quantiser scale of zero.
fn slice_parameters(h: &FrameHeader, size: usize) -> SliceParameterBufferVP9 {
    let mut s = SliceParameterBufferVP9 {
        slice_data_size: size as u32,
        slice_data_offset: 0,
        slice_data_flag: 0, // VA_SLICE_DATA_FLAG_ALL
        ..SliceParameterBufferVP9::default()
    };
    for segment in 0..MAX_SEGMENTS {
        let dequant = h.segment_dequant(segment);
        let levels = h.loop_filter_levels(segment);
        s.seg_param[segment] = SegmentParameterVP9 {
            segment_flags: SegmentFlags {
                segment_reference_enabled: u16::from(
                    h.segmentation.feature_enabled[segment][SEG_LVL_REF_FRAME],
                ),
                segment_reference: h.segmentation.feature_data[segment][SEG_LVL_REF_FRAME] as u16,
                segment_reference_skipped: u16::from(
                    h.segmentation.feature_enabled[segment][SEG_LVL_SKIP],
                ),
            },
            filter_level: levels,
            luma_ac_quant_scale: dequant.luma_ac,
            luma_dc_quant_scale: dequant.luma_dc,
            chroma_ac_quant_scale: dequant.chroma_ac,
            chroma_dc_quant_scale: dequant.chroma_dc,
        };
    }
    s
}
