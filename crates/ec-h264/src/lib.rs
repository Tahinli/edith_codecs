//! An ITU-T H.264 (ISO/IEC 14496-10) software decoder.
//!
//! What this release decodes: I and IDR pictures, CAVLC entropy coding, 4:2:0
//! 8-bit, progressive frames — that is, the intra half of the Baseline, Main
//! and High profiles, including `I_PCM`. What it does not decode it *refuses*,
//! by name and with a reason ([`ec_core::Error::Unsupported`]): CABAC, inter
//! prediction, fields and MBAFF, the 8x8 transform, scaling matrices, 4:2:2 and
//! 4:4:4, and sample depths above 8 bits. A refusal is a capability statement,
//! never a picture that is quietly wrong.
//!
//! The implementation is deliberately a transcription of the specification:
//! modules are named after its clauses ([`intra`] is 8.3, [`transform`] is 8.5,
//! [`deblock`] is 8.7, [`cavlc`] is 9.2), tables keep their published row order,
//! and no step is fused with another for speed. Correctness and auditability
//! come first; the optimisation passes come later, and have this to be checked
//! against.
//!
//! ```no_run
//! use ec_core::registry::{CodecId, CodecParameters, Decoder};
//! use ec_core::{Packet, TimeBase};
//! use ec_h264::H264Decoder;
//!
//! # fn main() -> ec_core::Result<()> {
//! let annex_b: Vec<u8> = std::fs::read("stream.264")?;
//! let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264))?;
//! decoder.send_packet(&Packet::new(0, TimeBase::new(1, 25), annex_b))?;
//! let frame = decoder.receive_frame()?; // the first decoded picture, I420
//! # let _ = frame;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cavlc;
pub mod deblock;
pub mod intra;
pub mod picture;
pub mod slice;
pub mod tables;
pub mod transform;

use std::collections::{BTreeMap, VecDeque};

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Frame, PixelFormat};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder, MediaParameters};
use ec_core::timebase::Timestamp;
use ec_h264_syntax::nal::{NalUnit, NalUnitType, RbspReader, annex_b_units};
use ec_h264_syntax::pps::{self, PicParameterSet};
use ec_h264_syntax::slice::SliceHeader;
use ec_h264_syntax::sps::SequenceParameterSet;

use crate::deblock::deblock_picture;
use crate::picture::Picture;
use crate::slice::SliceDecoder;

/// The picture being decoded, plus the slice header fields that decide where it
/// ends (clause 7.4.1.2.4).
#[derive(Debug)]
struct CurrentPicture {
    picture: Picture,
    seq_parameter_set_id: u32,
    pic_parameter_set_id: u32,
    frame_num: u32,
    idr_pic_id: u32,
    idr: bool,
    slice_count: i32,
    pts: Option<Timestamp>,
}

/// An H.264 decoder: packets in, [`ec_core::VideoFrame`]s out.
#[derive(Debug)]
pub struct H264Decoder {
    params: CodecParameters,
    /// Active sequence parameter sets, by `seq_parameter_set_id`.
    sps: BTreeMap<u32, SequenceParameterSet>,
    /// Active picture parameter sets, by `pic_parameter_set_id`.
    pps: BTreeMap<u32, PicParameterSet>,
    /// `lengthSizeMinusOne + 1` when packets carry length-prefixed NAL units
    /// (an `avcC` stream), `None` for Annex B byte streams.
    nal_length_size: Option<usize>,
    current: Option<CurrentPicture>,
    frames: VecDeque<Frame>,
    /// Timestamp of the packet being decoded, handed to the picture it starts.
    pending_pts: Option<Timestamp>,
    end_of_stream: bool,
}

impl H264Decoder {
    /// A decoder for `params`.
    ///
    /// When `params.extradata` holds an `avcC` record its parameter sets are
    /// read immediately and packets are then expected to carry length-prefixed
    /// NAL units; otherwise packets are read as Annex B byte streams.
    pub fn new(params: CodecParameters) -> Result<H264Decoder> {
        if params.codec != CodecId::H264 {
            return Err(Error::unsupported(
                format!("codec {:?}", params.codec),
                "this decoder implements H.264 only",
            ));
        }
        let mut decoder = H264Decoder {
            params,
            sps: BTreeMap::new(),
            pps: BTreeMap::new(),
            nal_length_size: None,
            current: None,
            frames: VecDeque::new(),
            pending_pts: None,
            end_of_stream: false,
        };
        if let Some(extradata) = decoder.params.extradata.clone()
            && extradata.len() >= 7
            && extradata[0] == 1
        {
            decoder.parse_avcc(&extradata)?;
        }
        Ok(decoder)
    }

    /// Read an `avcC` record (ISO/IEC 14496-15 clause 5.3.3.1): the NAL length
    /// size and the in-band parameter sets.
    fn parse_avcc(&mut self, extradata: &[u8]) -> Result<()> {
        let mut r = BitReader::new(extradata);
        // configurationVersion, AVCProfileIndication, profile_compatibility,
        // AVCLevelIndication.
        r.skip_bits(32)?;
        let length_size_minus_one = r.read_bits(8)? & 0x03;
        self.nal_length_size = Some(length_size_minus_one as usize + 1);
        let num_sps = r.read_bits(8)? & 0x1F;
        for _ in 0..num_sps {
            let len = r.read_bits(16)? as usize;
            let bytes = r.read_bytes(len)?.to_vec();
            self.decode_nal_unit(&bytes)?;
        }
        let num_pps = r.read_bits(8)?;
        for _ in 0..num_pps {
            let len = r.read_bits(16)? as usize;
            let bytes = r.read_bytes(len)?.to_vec();
            self.decode_nal_unit(&bytes)?;
        }
        Ok(())
    }

    /// Split a packet into NAL units and decode each of them.
    fn decode_packet_payload(&mut self, data: &[u8]) -> Result<()> {
        match self.nal_length_size {
            Some(size) => {
                let mut offset = 0usize;
                while offset + size <= data.len() {
                    let mut length = 0usize;
                    for i in 0..size {
                        length = (length << 8) | data[offset + i] as usize;
                    }
                    offset += size;
                    let end = offset
                        .checked_add(length)
                        .filter(|&end| end <= data.len())
                        .ok_or_else(|| {
                            Error::corrupt(format!(
                                "H.264: a NAL unit of {length} bytes overruns the packet"
                            ))
                        })?;
                    let unit = data[offset..end].to_vec();
                    self.decode_nal_unit(&unit)?;
                    offset = end;
                }
            }
            None => {
                for unit in annex_b_units(data) {
                    let unit = unit.to_vec();
                    self.decode_nal_unit(&unit)?;
                }
            }
        }
        Ok(())
    }

    /// Decode one NAL unit, its header byte included.
    fn decode_nal_unit(&mut self, bytes: &[u8]) -> Result<()> {
        let unit = NalUnit::parse(bytes)?;
        match unit.nal_unit_type {
            NalUnitType::Sps => {
                let sps = SequenceParameterSet::parse(&unit.rbsp)?;
                self.update_parameters(&sps)?;
                self.sps.insert(sps.seq_parameter_set_id, sps);
            }
            NalUnitType::Pps => {
                let (_, seq_parameter_set_id) = pps::peek_ids(&unit.rbsp)?;
                let chroma_format_idc = self
                    .sps
                    .get(&seq_parameter_set_id)
                    .map(|sps| sps.chroma_format_idc)
                    .unwrap_or(1);
                let pps = PicParameterSet::parse(&unit.rbsp, chroma_format_idc)?;
                self.pps.insert(pps.pic_parameter_set_id, pps);
            }
            ty if ty.is_vcl() => self.decode_slice(ty, unit.nal_ref_idc, &unit.rbsp)?,
            // SEI, access unit delimiters, filler and the end-of-sequence and
            // end-of-stream units carry nothing this decoder acts on.
            _ => {}
        }
        Ok(())
    }

    /// Decode one VCL NAL unit: its slice header, then its slice data.
    fn decode_slice(
        &mut self,
        nal_unit_type: NalUnitType,
        nal_ref_idc: u8,
        rbsp: &[u8],
    ) -> Result<()> {
        if matches!(
            nal_unit_type,
            NalUnitType::SlicePartitionA
                | NalUnitType::SlicePartitionB
                | NalUnitType::SlicePartitionC
        ) {
            return Err(Error::unsupported(
                "H.264 slice data partitioning",
                "nal_unit_type 2 to 4 need the partition reassembly of clause 7.4.1",
            ));
        }
        // The picture parameter set is the third syntax element of the header,
        // and the header cannot be parsed without the sets that it selects.
        let mut peek = BitReader::new(rbsp);
        let _first_mb_in_slice = peek.read_ue()?;
        let _slice_type = peek.read_ue()?;
        let pic_parameter_set_id = peek.read_ue()?;
        let pps = self
            .pps
            .get(&pic_parameter_set_id)
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "H.264 slice: picture parameter set {pic_parameter_set_id} has not been seen"
                ))
            })?
            .clone();
        let sps = self
            .sps
            .get(&pps.seq_parameter_set_id)
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "H.264 slice: sequence parameter set {} has not been seen",
                    pps.seq_parameter_set_id
                ))
            })?
            .clone();

        let mut rr = RbspReader::new(rbsp);
        let header = SliceHeader::parse(&mut rr, nal_unit_type, nal_ref_idc, &sps, &pps)?;
        let idr = nal_unit_type == NalUnitType::IdrSlice;

        // Clause 7.4.1.2.4: does this slice open a new primary coded picture?
        let starts_new_picture = match &self.current {
            None => true,
            Some(current) => {
                header.first_mb_in_slice == 0
                    || current.frame_num != header.frame_num
                    || current.pic_parameter_set_id != header.pic_parameter_set_id
                    || current.idr != idr
                    || (idr && current.idr_pic_id != header.idr_pic_id)
            }
        };
        if starts_new_picture {
            // The previous picture is complete and is queued before anything
            // about this slice can fail: a refusal never costs a decoded frame.
            self.finish_picture()?;
            self.current = Some(CurrentPicture {
                picture: Picture::new(
                    sps.pic_width_in_mbs() as usize,
                    sps.frame_height_in_mbs() as usize,
                ),
                seq_parameter_set_id: sps.seq_parameter_set_id,
                pic_parameter_set_id: header.pic_parameter_set_id,
                frame_num: header.frame_num,
                idr_pic_id: header.idr_pic_id,
                idr,
                slice_count: 0,
                pts: self.pending_pts,
            });
        }

        let current = self
            .current
            .as_mut()
            .expect("a picture is in progress by now");
        let slice_id = current.slice_count;
        current.slice_count += 1;
        let mut decoder = SliceDecoder::new(&sps, &pps, &header, &mut current.picture, slice_id)?;
        decoder.decode_slice_data(&mut rr)
    }

    /// Finish the picture in progress: filter it, crop it and queue it.
    fn finish_picture(&mut self) -> Result<()> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        if current.slice_count == 0 {
            return Ok(());
        }
        let sps = self
            .sps
            .get(&current.seq_parameter_set_id)
            .ok_or_else(|| Error::corrupt("H.264: the picture's sequence parameter set is gone"))?;
        let pps = self
            .pps
            .get(&current.pic_parameter_set_id)
            .ok_or_else(|| Error::corrupt("H.264: the picture's picture parameter set is gone"))?;
        let mut picture = current.picture;
        deblock_picture(&mut picture, pps);
        let (crop_unit_x, crop_unit_y) = sps.crop_units();
        let (width, height) = sps.cropped_size()?;
        let mut frame = picture.to_frame(
            (crop_unit_x * sps.frame_crop_left_offset) as usize,
            (crop_unit_y * sps.frame_crop_top_offset) as usize,
            width,
            height,
        )?;
        frame.color = color_info(sps);
        frame.pts = current.pts;
        self.frames.push_back(Frame::Video(frame));
        Ok(())
    }

    /// Publish the picture size and colour description of a new SPS.
    fn update_parameters(&mut self, sps: &SequenceParameterSet) -> Result<()> {
        let (width, height) = sps.cropped_size()?;
        if let MediaParameters::Video(video) = &mut self.params.media {
            video.width = width;
            video.height = height;
            video.format = Some(PixelFormat::I420);
            video.color = color_info(sps);
        }
        Ok(())
    }
}

/// The H.273 colour description a sequence parameter set carries, or the
/// "unspecified" triplet when it carries none.
fn color_info(sps: &SequenceParameterSet) -> ColorInfo {
    match &sps.vui_parameters {
        Some(vui) if vui.colour_description_present_flag => ColorInfo {
            primaries: vui.colour_primaries,
            transfer: vui.transfer_characteristics,
            matrix: vui.matrix_coefficients,
            full_range: vui.video_full_range_flag,
        },
        Some(vui) => ColorInfo {
            full_range: vui.video_full_range_flag,
            ..ColorInfo::default()
        },
        None => ColorInfo::default(),
    }
}

impl Decoder for H264Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.end_of_stream = false;
        self.pending_pts = packet
            .pts
            .map(|ticks| Timestamp::new(ticks, packet.time_base));
        self.decode_packet_payload(&packet.data)?;
        // One packet is one access unit, so whatever picture it started is
        // complete once the packet is consumed.
        self.finish_picture()
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.frames.pop_front() {
            Some(frame) => Ok(frame),
            None if self.end_of_stream => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.finish_picture()?;
        self.end_of_stream = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.current = None;
        self.frames.clear();
        self.pending_pts = None;
        self.end_of_stream = false;
    }
}

#[cfg(test)]
mod tests_support {
    use ec_core::bitio::BitWriter;
    use ec_h264_syntax::pps::PicParameterSet;

    /// A picture parameter set with no scaling matrices and zero chroma
    /// offsets: the shape the deblocking tests filter against.
    pub fn flat_pps() -> PicParameterSet {
        let mut w = BitWriter::new();
        w.write_ue(0); // pic_parameter_set_id
        w.write_ue(0); // seq_parameter_set_id
        w.write_bit(false); // entropy_coding_mode_flag
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(0); // num_ref_idx_l0_default_active_minus1
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(false); // weighted_pred_flag
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(0); // pic_init_qp_minus26
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(0); // chroma_qp_index_offset
        w.write_bit(false); // deblocking_filter_control_present_flag
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        w.write_bit(true); // rbsp_stop_one_bit
        w.align_to_byte();
        PicParameterSet::parse(&w.into_bytes(), 1).expect("hand-written PPS parses")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::TimeBase;

    #[test]
    fn only_h264_parameters_are_accepted() {
        assert!(H264Decoder::new(CodecParameters::new(CodecId::H265)).is_err());
        let decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        assert_eq!(decoder.codec_parameters().codec, CodecId::H264);
        assert!(decoder.nal_length_size.is_none(), "Annex B by default");
    }

    #[test]
    fn a_packet_without_parameter_sets_is_corrupt_not_a_panic() {
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        // A slice NAL with no SPS or PPS in front of it.
        let stream = [0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00];
        let packet = Packet::new(0, TimeBase::new(1, 25), stream.to_vec());
        let err = decoder.send_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
        assert!(decoder.receive_frame().unwrap_err().is_need_more());
    }

    #[test]
    fn flush_then_drain_reports_end_of_stream() {
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
        decoder.flush().unwrap();
        assert!(decoder.receive_frame().unwrap_err().is_eof());
        decoder.reset();
        assert!(decoder.receive_frame().unwrap_err().is_need_more());
    }
}
