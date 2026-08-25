//! The encoder API and the wavefront that makes one call use every core.
//!
//! # Parallelism
//!
//! `entropy_coding_sync_enabled_flag` (WPP) is on. Each CTB row is a substream
//! with its own entry point; a worker may start row `r` once row `r - 1` is two
//! CTBs ahead, which is exactly the dependency intra prediction has (a block
//! reaches at most one CTB to the right on the line above) and exactly what the
//! CABAC context sync in 9.3.2.1 assumes. Rows are handed to `threads` workers
//! round-robin, and every worker owns:
//!
//! - its own band of the reconstruction (`chunks_mut`, no sharing),
//! - its own substream buffer and CABAC engine,
//! - a published bottom line ([`RowBoundary`]) the row below reads through
//!   atomics, plus a progress counter.
//!
//! No `unsafe`, no scoped raw pointers, no rayon: the only shared mutable state
//! is one atomic byte per boundary sample and one atomic counter per row.
//!
//! # What comes out
//!
//! One Annex-B access unit per picture, carrying VPS, SPS and PPS every time —
//! every picture is an IDR, so every picture is a seek point, and a seek point
//! whose parameter sets are elsewhere is not one.

use crate::ctu::{CtuEncoder, RowBoundary, RowState, SourcePlanes};
use ec_core::color::ContentLight;
use ec_core::error::{Error, Result};
use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265_syntax::nal::{NalHeader, NalUnitType, escape_rbsp, write_annex_b};
use ec_h265_syntax::ps::{ConformanceWindow, ProfileTierLevel};
use ec_h265_syntax::slice::SliceHeader;
use ec_h265_syntax::vui::{VideoSignalType, VuiParameters};
use ec_h265_syntax::{Pps, Sps, Vps, sei};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::cabac::{CabacEncoder, Contexts};

/// How the encoder picks QP for a picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateControl {
    /// One fixed quantiser for every picture.
    ConstantQp(i32),
    /// Aim for `bits` per picture: a model picks the QP, and a picture that
    /// lands more than 25% off is coded a second time at a corrected QP.
    TargetBits(u64),
}

/// Encoder settings.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Displayed width; the coded picture is padded up to a whole number of
    /// minimum coding blocks and cropped back by the conformance window.
    pub width: u32,
    /// Displayed height.
    pub height: u32,
    /// Quantiser or bit target.
    pub rate_control: RateControl,
    /// Worker threads; 0 means "as many as the machine has".
    pub threads: usize,
    /// Coding tree block size, 32 (default) or 64.
    ///
    /// The wavefront hands one CTB *row* to a worker, so the row count is the
    /// parallelism this encoder has: 1080 lines are 17 rows at 64 but 34 at 32.
    /// Rows are dealt round-robin, so on a twelve-core machine 17 rows means
    /// five workers take two rows while seven take one — half the machine idles
    /// through the second half of the picture. 34 rows deal three-and-three.
    /// Measured on 1080p at QP 27: the two sizes encode at the same speed on one
    /// thread (1.80 fps each), but on twelve 32 runs 9.03 fps against 64's 7.92,
    /// for 1.0% more bits and 0.02 dB less PSNR — the whole difference is
    /// wavefront occupancy, which is why 32 is the default. 64 takes its bits
    /// back when the picture is tall enough to fill the workers anyway (2160
    /// lines are 34 rows at 64) or when nothing is waiting for the encode.
    pub ctb_size: usize,
    /// How many of the 35 intra modes get a full rate-distortion trial, on top
    /// of the most probable mode, which always gets one.
    ///
    /// Swept against x265 at matched features on four real clips (BD-PSNR
    /// luma / all-plane, wall time for the whole four-QP ladder):
    ///
    /// | k | film 3840x1608 | web 1138x640 | screen 2560x1440 | phone 1080p |
    /// |---|----------------|-------------------|------------------|-------------|
    /// | 1 | -0.840 / -0.815 38s | -0.728 / -0.733 8s | | |
    /// | 2 | -0.731 / -0.730 40s | -0.603 / -0.632 9s | -2.521 / -2.420 29s | -0.435 / -0.444 11s |
    /// | 3 | -0.703 / -0.709 43s | -0.556 / -0.596 10s | -2.493 / -2.402 31s | -0.401 / -0.416 11s |
    /// | 4 | -0.693 / -0.705 46s | -0.530 / -0.574 10s | | |
    /// | 5 | -0.685 / -0.703 49s | -0.507 / -0.556 11s | -2.465 / -2.360 35s | -0.366 / -0.387 13s |
    /// | 8 | -0.684 / -0.710 59s | -0.471 / -0.525 14s | -2.453 / -2.368 41s | -0.340 / -0.361 16s |
    ///
    /// Every clip improves monotonically up to 5, which is why 5 is the
    /// default. Eight buys nothing on the film clip (+0.001 luma, and its
    /// all-plane figure goes *backwards*) for another 20% of wall time, so the
    /// knee is where the default sits. `EC_H265_RDO_CANDIDATES` in the gate
    /// test re-runs the sweep.
    pub rdo_candidates: usize,
    /// Smallest coding unit the search will produce, 8 or 16. Sixteen is a
    /// *coarser* search that is not always faster: on real pictures the 8x8
    /// blocks pay for their search in residual bits (measured: -27% bits and
    /// +1.5 dB at QP 22 on a 1080p camera frame, at the same wall time), which
    /// is why 8 is the default.
    pub min_cu_size: usize,
    /// What the samples mean, written into the VUI.
    pub video_signal_type: Option<VideoSignalType>,
    /// Sample aspect ratio.
    pub sample_aspect_ratio: Option<(u16, u16)>,
    /// `(num_units_in_tick, time_scale)`; also picks the level.
    pub timing: Option<(u32, u32)>,
    /// HDR mastering metadata, written as prefix SEI.
    pub content_light: ContentLight,
    /// Emit the decoded picture hash SEI (MD5 of the reconstruction).
    pub picture_hash: bool,
    /// Return the reconstruction alongside the bitstream.
    pub keep_recon: bool,
}

impl EncoderConfig {
    /// Defaults for `width x height`: constant QP 27, every core, 32x32 CTBs
    /// (see [`EncoderConfig::ctb_size`] for why 32 and not 64).
    pub fn new(width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            rate_control: RateControl::ConstantQp(27),
            threads: 0,
            ctb_size: 32,
            rdo_candidates: 5,
            min_cu_size: 8,
            video_signal_type: None,
            sample_aspect_ratio: None,
            timing: None,
            content_light: ContentLight::default(),
            picture_hash: false,
            keep_recon: false,
        }
    }
}

/// One coded picture.
#[derive(Debug, Clone)]
pub struct EncodedPicture {
    /// The access unit, Annex-B framed: VPS, SPS, PPS, optional SEI, IDR slice.
    pub au: Vec<u8>,
    /// The quantiser the picture was coded at.
    pub qp: i32,
    /// The reconstruction, cropped to the displayed size, when
    /// [`EncoderConfig::keep_recon`] asked for it. Bit-identical to what a
    /// conformant decoder produces, because the in-loop filters are off.
    pub recon: Option<VideoFrame>,
}

/// An intra-only HEVC encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    vps: Vps,
    sps: Sps,
    pps: Pps,
    coded_width: usize,
    coded_height: usize,
    threads: usize,
}

/// Bits per pixel this encoder spends at QP 27, the anchor the bit-target model
/// walks from (measured on 1080p fixtures; the incumbent's own figure was 0.6).
const BPP_AT_QP27: f64 = 0.6;

impl Encoder {
    /// Build an encoder, refusing dimensions it cannot code.
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if cfg.width < 16 || cfg.height < 16 {
            return Err(Error::unsupported(
                format!("HEVC encode of {}x{}", cfg.width, cfg.height),
                "pictures below 16x16 have no coding tree",
            ));
        }
        if cfg.ctb_size != 64 && cfg.ctb_size != 32 {
            return Err(Error::unsupported(
                format!("HEVC coding tree block of {}", cfg.ctb_size),
                "this encoder codes 64x64 or 32x32 trees",
            ));
        }
        // The coded picture is a whole number of minimum coding blocks (8x8);
        // the conformance window crops the padding back off.
        let coded_width = (cfg.width as usize).next_multiple_of(8);
        let coded_height = (cfg.height as usize).next_multiple_of(8);
        let fps = cfg
            .timing
            .map(|(units, scale)| f64::from(scale) / f64::from(units.max(1)))
            .unwrap_or(30.0);
        let ptl = ProfileTierLevel::main(ProfileTierLevel::level_for(
            coded_width as u32,
            coded_height as u32,
            fps,
        ));
        let vps = Vps {
            id: 0,
            ptl,
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
        };
        let log2_ctb = cfg.ctb_size.trailing_zeros();
        let sps = Sps {
            vps_id: 0,
            id: 0,
            chroma_format_idc: 1,
            separate_colour_plane: false,
            pic_width_in_luma_samples: coded_width as u32,
            pic_height_in_luma_samples: coded_height as u32,
            conf_win: ConformanceWindow {
                left: 0,
                right: (coded_width - cfg.width as usize) as u32 / 2,
                top: 0,
                bottom: (coded_height - cfg.height as usize) as u32 / 2,
            },
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_poc_lsb_minus4: 4,
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: log2_ctb - 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 3,
            max_transform_hierarchy_depth_inter: 0,
            max_transform_hierarchy_depth_intra: 0,
            scaling_list_enabled: false,
            amp_enabled: false,
            // No in-loop filters: see `Encoder` docs and the PPS below.
            sao_enabled: false,
            pcm_enabled: false,
            pcm: None,
            num_short_term_ref_pic_sets: 0,
            short_term_ref_pic_sets: Vec::new(),
            long_term_ref_pics_present: false,
            num_long_term_ref_pics_sps: 0,
            long_term_ref_pics_sps: Vec::new(),
            temporal_mvp_enabled: false,
            strong_intra_smoothing: true,
            ptl,
            vui: Some(VuiParameters {
                sample_aspect_ratio: cfg.sample_aspect_ratio,
                video_signal_type: cfg.video_signal_type,
                timing: cfg.timing,
            }),
        };
        let pps = Pps {
            id: 0,
            sps_id: 0,
            entropy_coding_sync_enabled: true,
            // Deblocking and SAO are both off, which is what makes the
            // encoder's reconstruction bit-identical to a decoder's output: an
            // intra-only stream predicts from its own reconstruction only, so
            // an in-loop filter would change what a decoder shows without
            // changing what either side predicts from. Turning them off buys
            // that identity (and the picture-hash check that rides on it)
            // rather than a fraction of a dB.
            deblocking_filter_control_present: true,
            deblocking_filter_disabled: true,
            loop_filter_across_slices_enabled: false,
            ..Pps::default()
        };
        let threads = if cfg.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            cfg.threads
        };
        Ok(Encoder {
            cfg,
            vps,
            sps,
            pps,
            coded_width,
            coded_height,
            threads,
        })
    }

    /// The sequence parameter set this encoder writes.
    pub fn sps(&self) -> &Sps {
        &self.sps
    }

    /// Encode one picture as an IDR access unit.
    ///
    /// The frame must be [`PixelFormat::I420`] at the configured size; padding
    /// to the coded size is this encoder's job, not the caller's.
    pub fn encode_idr(&self, frame: &VideoFrame) -> Result<EncodedPicture> {
        if frame.format != PixelFormat::I420 {
            return Err(Error::unsupported(
                format!("HEVC encode of {:?}", frame.format),
                "this encoder codes 8-bit 4:2:0 planar",
            ));
        }
        if frame.width != self.cfg.width || frame.height != self.cfg.height {
            return Err(Error::corrupt(format!(
                "HEVC encode: frame is {}x{}, encoder configured for {}x{}",
                frame.width, frame.height, self.cfg.width, self.cfg.height
            )));
        }
        let planes: Vec<(&[u8], usize)> = frame
            .planes
            .iter()
            .map(|p: &Plane| (&p.data[..], p.stride))
            .collect();
        self.encode_idr_planes(
            planes[0].0,
            planes[0].1,
            planes[1].0,
            planes[1].1,
            planes[2].0,
            planes[2].1,
        )
    }

    /// Encode one picture from raw I420 planes with their strides.
    ///
    /// This is the surface a compatibility shim maps onto: the planes are the
    /// *displayed* picture, and the conformance window handles the rest.
    pub fn encode_idr_planes(
        &self,
        y: &[u8],
        y_stride: usize,
        cb: &[u8],
        cb_stride: usize,
        cr: &[u8],
        cr_stride: usize,
    ) -> Result<EncodedPicture> {
        let padded = self.pad_source(y, y_stride, cb, cb_stride, cr, cr_stride)?;
        let (mut qp, target) = match self.cfg.rate_control {
            RateControl::ConstantQp(qp) => (qp.clamp(0, 51), None),
            RateControl::TargetBits(bits) => {
                let pixels = (self.cfg.width as f64) * (self.cfg.height as f64);
                let target_bpp = (bits as f64 / pixels).max(1e-4);
                let qp = 27.0 + 6.0 * (BPP_AT_QP27 / target_bpp).log2();
                ((qp.round() as i32).clamp(0, 51), Some(bits))
            }
        };
        let mut coded = self.code_picture(&padded, qp)?;
        if let Some(target) = target {
            let actual = coded.au.len() as f64 * 8.0;
            let target = target as f64;
            if (actual - target).abs() / target > 0.25 {
                // One correction pass, from the measured slope rather than the
                // model: 6 QP steps is a factor of two in rate.
                let correction = 6.0 * (actual / target).log2();
                let corrected = (qp as f64 + correction.clamp(-12.0, 12.0)).round() as i32;
                let corrected = corrected.clamp(0, 51);
                if corrected != qp {
                    qp = corrected;
                    coded = self.code_picture(&padded, qp)?;
                }
            }
        }
        Ok(coded)
    }

    /// Encode a batch of pictures, each as its own IDR access unit.
    ///
    /// The wavefront inside a single picture already uses every core, so this is
    /// a convenience for callers with a list, not a second layer of threads.
    pub fn encode_batch<'a, I>(&self, frames: I) -> Result<Vec<EncodedPicture>>
    where
        I: IntoIterator<Item = &'a VideoFrame>,
    {
        frames.into_iter().map(|f| self.encode_idr(f)).collect()
    }

    /// Copy the source into coded-size planes, replicating the edge into the
    /// padding rather than filling it with black: the padding is coded and then
    /// cropped away, and a black border would bleed into the last real column
    /// through the prediction and the transform that straddles it.
    fn pad_source(
        &self,
        y: &[u8],
        y_stride: usize,
        cb: &[u8],
        cb_stride: usize,
        cr: &[u8],
        cr_stride: usize,
    ) -> Result<PaddedSource> {
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (coded_w, coded_h) = (self.coded_width, self.coded_height);
        let plane = |src: &[u8],
                     stride: usize,
                     width: usize,
                     height: usize,
                     out_w: usize,
                     out_h: usize|
         -> Result<Vec<u8>> {
            if stride < width || src.len() < (height - 1) * stride + width {
                return Err(Error::corrupt(
                    "HEVC encode: source plane shorter than its stated size".to_string(),
                ));
            }
            let mut out = vec![0u8; out_w * out_h];
            for row in 0..out_h {
                let src_row = row.min(height - 1);
                let line = &src[src_row * stride..src_row * stride + width];
                let dst = &mut out[row * out_w..row * out_w + out_w];
                dst[..width].copy_from_slice(line);
                let edge = line[width - 1];
                dst[width..].fill(edge);
            }
            Ok(out)
        };
        Ok(PaddedSource {
            y: plane(y, y_stride, w, h, coded_w, coded_h)?,
            cb: plane(cb, cb_stride, cw, ch, coded_w / 2, coded_h / 2)?,
            cr: plane(cr, cr_stride, cw, ch, coded_w / 2, coded_h / 2)?,
        })
    }

    /// Code one picture at `qp`, wavefront across CTB rows.
    fn code_picture(&self, src: &PaddedSource, qp: i32) -> Result<EncodedPicture> {
        let (width, height) = (self.coded_width, self.coded_height);
        let ctb = self.cfg.ctb_size;
        let cols = width.div_ceil(ctb);
        let rows = height.div_ceil(ctb);

        let mut rec_y = vec![0u8; width * height];
        let mut rec_cb = vec![0u8; width / 2 * height / 2];
        let mut rec_cr = vec![0u8; width / 2 * height / 2];

        let boundaries: Vec<RowBoundary> = (0..rows).map(|_| RowBoundary::new(width)).collect();
        let progress: Vec<Progress> = (0..rows).map(|_| Progress(AtomicUsize::new(0))).collect();
        let wpp_contexts: Vec<Mutex<Option<Contexts>>> =
            (0..rows).map(|_| Mutex::new(None)).collect();
        let substreams: Vec<Mutex<Vec<u8>>> = (0..rows).map(|_| Mutex::new(Vec::new())).collect();

        // One band of the reconstruction per CTB row, handed out by index.
        let mut bands_y: Vec<&mut [u8]> = rec_y.chunks_mut(ctb * width).collect();
        let mut bands_cb: Vec<&mut [u8]> = rec_cb.chunks_mut(ctb / 2 * (width / 2)).collect();
        let mut bands_cr: Vec<&mut [u8]> = rec_cr.chunks_mut(ctb / 2 * (width / 2)).collect();
        let threads = self.threads.min(rows).max(1);
        // Round-robin: worker t owns rows t, t + threads, ... Each row's
        // predecessor therefore belongs to another worker that is never waiting
        // on it, so the wavefront cannot deadlock.
        let mut per_worker: Vec<Vec<RowAssignment<'_>>> =
            (0..threads).map(|_| Vec::new()).collect();
        for row in (0..rows).rev() {
            let y = bands_y.pop().expect("one band per row");
            let cb = bands_cb.pop().expect("one band per row");
            let cr = bands_cr.pop().expect("one band per row");
            per_worker[row % threads].push((row, y, cb, cr));
        }
        for worker in per_worker.iter_mut() {
            worker.reverse();
        }

        let slice_qp = qp;
        std::thread::scope(|scope| {
            for worker_rows in per_worker {
                let boundaries = &boundaries;
                let progress = &progress;
                let wpp_contexts = &wpp_contexts;
                let substreams = &substreams;
                let src = &src;
                scope.spawn(move || {
                    for (row, band_y, band_cb, band_cr) in worker_rows {
                        let band_y0 = row * ctb;
                        // Wait for the row above to be two CTBs ahead, which is
                        // both the prediction reach and the CABAC sync point.
                        if row > 0 {
                            wait_for(&progress[row - 1], 2.min(cols));
                        }
                        let contexts = if row == 0 || cols < 2 {
                            Contexts::new(slice_qp)
                        } else {
                            wpp_contexts[row - 1]
                                .lock()
                                .ok()
                                .and_then(|c| c.clone())
                                .unwrap_or_else(|| Contexts::new(slice_qp))
                        };
                        let mut enc = CabacEncoder::new(contexts);
                        let mut coder = CtuEncoder::new(
                            SourcePlanes {
                                y: &src.y,
                                cb: &src.cb,
                                cr: &src.cr,
                            },
                            RowState {
                                rec_y: band_y,
                                rec_cb: band_cb,
                                rec_cr: band_cr,
                                publish: &boundaries[row],
                                above: if row == 0 {
                                    None
                                } else {
                                    Some(&boundaries[row - 1])
                                },
                            },
                            width,
                            height,
                            ctb,
                            band_y0,
                            slice_qp,
                            true,
                            self.cfg.rdo_candidates,
                            self.cfg.min_cu_size.max(8).trailing_zeros(),
                        );
                        for col in 0..cols {
                            if row > 0 {
                                wait_for(&progress[row - 1], (col + 2).min(cols));
                            }
                            coder.encode_ctu(col, &mut enc);
                            if col == 1 {
                                // 9.3.2.1: the row below syncs from the state
                                // after the second CTB of this row.
                                if let Ok(mut slot) = wpp_contexts[row].lock() {
                                    *slot = Some(enc.contexts.clone());
                                }
                            }
                            progress[row].0.store(col + 1, Ordering::Release);
                            let last_in_picture = row + 1 == rows && col + 1 == cols;
                            enc.encode_terminate(u32::from(last_in_picture));
                            if !last_in_picture && col + 1 == cols {
                                // end_of_subset_one_bit, then byte alignment.
                                enc.encode_terminate(1);
                            }
                        }
                        if let Ok(mut slot) = substreams[row].lock() {
                            *slot = enc.finish();
                        }
                    }
                });
            }
        });

        let substreams: Vec<Vec<u8>> = substreams
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_default())
            .collect();
        let au = self.assemble_au(qp, &substreams, &rec_y, &rec_cb, &rec_cr);
        let recon = self
            .cfg
            .keep_recon
            .then(|| self.crop_recon(&rec_y, &rec_cb, &rec_cr));
        Ok(EncodedPicture { au, qp, recon })
    }

    /// Parameter sets, optional SEI, and the slice, in Annex-B framing.
    fn assemble_au(
        &self,
        qp: i32,
        substreams: &[Vec<u8>],
        rec_y: &[u8],
        rec_cb: &[u8],
        rec_cr: &[u8],
    ) -> Vec<u8> {
        let mut au = Vec::with_capacity(substreams.iter().map(|s| s.len()).sum::<usize>() + 256);
        write_annex_b(
            &mut au,
            NalHeader::new(NalUnitType::Vps),
            &self.vps.to_rbsp(),
            true,
        );
        write_annex_b(
            &mut au,
            NalHeader::new(NalUnitType::Sps),
            &self.sps.to_rbsp(),
            true,
        );
        write_annex_b(
            &mut au,
            NalHeader::new(NalUnitType::Pps),
            &self.pps.to_rbsp(),
            true,
        );
        if let Some(rbsp) = sei::hdr_metadata_rbsp(self.cfg.content_light) {
            write_annex_b(&mut au, sei::prefix_sei_header(), &rbsp, true);
        }

        // Entry points count the bytes as they appear in the NAL, emulation
        // prevention included. Each substream ends on a byte whose last written
        // bit is the flush's one bit, so no escape sequence straddles a
        // boundary and each substream can be measured on its own.
        let mut escaped: Vec<Vec<u8>> = Vec::with_capacity(substreams.len());
        for stream in substreams {
            let mut out = Vec::with_capacity(stream.len() + 8);
            escape_rbsp(stream, &mut out);
            escaped.push(out);
        }
        let mut header = SliceHeader::intra(&self.pps, qp - 26);
        header.entry_point_offsets = escaped
            .iter()
            .take(escaped.len().saturating_sub(1))
            .map(|s| s.len() as u32)
            .collect();
        let mut writer = ec_core::bitio::BitWriter::with_capacity(64);
        header.write(&mut writer, &self.sps, &self.pps, NalUnitType::IdrWRadl);
        let mut rbsp = writer.into_bytes();
        for stream in substreams {
            rbsp.extend_from_slice(stream);
        }
        write_annex_b(&mut au, NalHeader::new(NalUnitType::IdrWRadl), &rbsp, true);

        if self.cfg.picture_hash {
            let (w, h) = (self.coded_width, self.coded_height);
            let rbsp = sei::decoded_picture_hash_rbsp(&[
                (rec_y, w, w, h),
                (rec_cb, w / 2, w / 2, h / 2),
                (rec_cr, w / 2, w / 2, h / 2),
            ]);
            write_annex_b(&mut au, sei::suffix_sei_header(), &rbsp, false);
        }
        au
    }

    fn crop_recon(&self, rec_y: &[u8], rec_cb: &[u8], rec_cr: &[u8]) -> VideoFrame {
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let crop = |src: &[u8], stride: usize, width: usize, height: usize| -> Vec<u8> {
            let mut out = Vec::with_capacity(width * height);
            for row in 0..height {
                out.extend_from_slice(&src[row * stride..row * stride + width]);
            }
            out
        };
        let planes = vec![
            Plane::new(crop(rec_y, self.coded_width, w, h), w),
            Plane::new(crop(rec_cb, self.coded_width / 2, cw, ch), cw),
            Plane::new(crop(rec_cr, self.coded_width / 2, cw, ch), cw),
        ];
        VideoFrame::try_new(PixelFormat::I420, w as u32, h as u32, planes)
            .expect("cropped planes are exactly the size I420 asks for")
    }
}

/// A progress counter on its own cache line.
///
/// Twelve workers polling twelve counters packed eight to a line spend their
/// time invalidating each other's caches instead of coding; the padding is the
/// difference between a wavefront and a contention benchmark.
#[repr(align(64))]
struct Progress(AtomicUsize);

/// Spin, then yield, then sleep until a row's progress counter reaches `target`.
///
/// A worker that spins hard is a worker holding a core the wavefront needs
/// somewhere else, so the wait backs off to a sleep at roughly the granularity
/// of one coding tree block.
fn wait_for(counter: &Progress, target: usize) {
    let mut spins = 0u32;
    while counter.0.load(Ordering::Acquire) < target {
        if spins < 128 {
            std::hint::spin_loop();
        } else if spins < 256 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
        spins += 1;
    }
}

/// One CTB row handed to a worker: its index and its band of each plane.
type RowAssignment<'a> = (usize, &'a mut [u8], &'a mut [u8], &'a mut [u8]);

struct PaddedSource {
    y: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
}
