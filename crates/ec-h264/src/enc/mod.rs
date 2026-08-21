//! The H.264 encoder: I and P pictures, CAVLC, 8-bit 4:2:0 progressive.
//!
//! # Shared reconstruction
//!
//! There is no second reconstruction path here. Prediction, dequantisation,
//! the inverse transforms and the deblocking filter are the decoder's own
//! modules, and the encoder fills the very same [`Picture`] the decoder fills,
//! macroblock metadata included. Encoder and decoder therefore agree on every
//! reconstructed sample by construction, not by testing — the tests only check
//! that nothing broke the property.
//!
//! # Parallelism
//!
//! A picture is cut into one slice per worker, each slice a band of whole
//! macroblock rows. A slice is independent by definition (no prediction
//! crosses its boundary), so each worker owns a private picture buffer, writes
//! its own bitstream and never reads another worker's samples; motion
//! compensation reads the *previous* picture, which is finished and immutable.
//! The bands are stitched together afterwards and the deblocking filter runs
//! once over the whole picture, across slice boundaries exactly as a decoder
//! runs it. No `unsafe`, no shared mutable state at all.

mod cabac_enc;
mod entropy;
mod headers;
mod mb;
mod quant;
mod rc;
mod vlc;

use ec_core::BitWriter;
use ec_core::error::{Error, Result};
use ec_h264_syntax::nal::{NalUnitType, escape_rbsp};
use ec_h264_syntax::{SliceType, Sps};

use crate::decoder::deblock_picture;
use crate::dpb::{Picture, SliceParams as DeblockParams};
use crate::entropy::{FLAG_INTER, FLAG_TRANS8X8};
use crate::transform::{LevelScale4x4, LevelScale8x8};

use entropy::EncEntropy;
use headers::{SeqParams, SliceParams, write_pps, write_slice_header, write_sps};
use mb::{MbEnc, Source, encode_mb};
use rc::RateControl;

pub use mb::Preset;

/// Encoder settings.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Displayed luma width; must be even.
    pub width: u32,
    /// Displayed luma height; must be even.
    pub height: u32,
    /// Frames per second, for the VUI timing and the bit budget.
    pub framerate: f32,
    /// Target bits per second. Zero means constant quantiser ([`Self::qp`]).
    pub bitrate: u32,
    /// Pictures between IDR pictures; 1 codes every picture as an IDR.
    pub gop_size: u32,
    /// Consecutive B pictures. Only 0 is coded today (see the crate note);
    /// a non-zero value is accepted and coded as 0 rather than refused.
    pub bframes: u32,
    /// Speed/quality rung.
    pub preset: Preset,
    /// Entropy coder: CABAC (Main profile) or CAVLC (Baseline). CABAC costs
    /// roughly 12% fewer bits at the same reconstruction, which is why it is
    /// the default; CAVLC is there for a decoder that cannot do better.
    pub cabac: bool,
    /// Worker threads, which is also the number of slices per picture; 0 means
    /// as many as the machine has.
    pub threads: usize,
    /// Quantiser for constant-QP mode (`bitrate` zero).
    pub qp: i32,
    /// 8x8 transform (High profile). Off by default.
    ///
    /// NOT SAFE TO SET YET: this only flips the SPS/PPS High-profile tail
    /// (`profile_idc` 100, `transform_8x8_mode_flag`). Once the PPS carries
    /// that flag, a conformant decoder unconditionally reads an extra
    /// `transform_size_8x8_flag` per macroblock (7.3.5: every non-I16x16
    /// intra macroblock, and every inter macroblock with a nonzero luma cbp —
    /// see `decoder.rs:1151` and `:1789-1793`). Nothing in `enc::entropy` /
    /// `enc::mb` writes that bit yet, so turning this on desyncs every real
    /// stream (headers.rs's own tests only exercise the parameter sets, not a
    /// full encode). Wiring that emission — and then the mode decision that
    /// actually picks 8x8 blocks — is later work.
    pub transform_8x8: bool,
}

impl EncoderConfig {
    /// Defaults for `width x height`: 30 fps, constant QP 26, every core.
    pub fn new(width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            framerate: 30.0,
            bitrate: 0,
            gop_size: 250,
            bframes: 0,
            preset: Preset::Fast,
            cabac: true,
            threads: 0,
            qp: 26,
            transform_8x8: false,
        }
    }
}

/// One coded picture: an Annex-B access unit and what it cost.
#[derive(Debug, Clone, Default)]
pub struct EncodedPicture {
    /// The access unit, with 4-byte start codes; parameter sets ride along on
    /// every IDR so that every IDR is a seek point.
    pub au: Vec<u8>,
    /// True when the picture is an IDR.
    pub key_frame: bool,
    /// The quantiser the picture was coded at.
    pub qp: i32,
}

/// A software H.264 encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    sps: Sps,
    sps_nal: Vec<u8>,
    pps_nal: Vec<u8>,
    /// The previous reconstruction, which every P picture predicts from.
    reference: Picture,
    /// One private picture per worker; worker 0's doubles as the picture the
    /// other bands are stitched into.
    workers: Vec<Picture>,
    src: Source,
    rc: RateControl,
    threads: usize,
    coded_width: usize,
    coded_height: usize,
    frames: u64,
    frame_num: u32,
    idr_pic_id: u32,
    next_id: i32,
    /// True once a reference picture exists.
    have_reference: bool,
    /// Macroblocks coded with transform_size_8x8_flag set: (intra, inter).
    t8x8_mbs: (u64, u64),
}

/// A picture handed to the encoder: three planes, no padding assumptions.
#[derive(Debug, Clone, Copy)]
pub struct PictureView<'a> {
    pub width: u32,
    pub height: u32,
    /// Luma plane, `width` samples per row unless `y_stride` says otherwise.
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    /// Row pitches; zero means "tightly packed".
    pub y_stride: usize,
    pub c_stride: usize,
}

impl<'a> PictureView<'a> {
    /// A tightly packed I420 picture.
    pub fn i420(width: u32, height: u32, y: &'a [u8], u: &'a [u8], v: &'a [u8]) -> PictureView<'a> {
        PictureView {
            width,
            height,
            y,
            u,
            v,
            y_stride: width as usize,
            c_stride: width.div_ceil(2) as usize,
        }
    }
}

impl Encoder {
    /// Build an encoder for `cfg`, refusing what it cannot code.
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if cfg.width < 16 || cfg.height < 16 {
            return Err(Error::unsupported(
                "picture smaller than one macroblock",
                "H.264 codes 16x16 macroblocks; encode at 16x16 or larger",
            ));
        }
        if !cfg.width.is_multiple_of(2) || !cfg.height.is_multiple_of(2) {
            return Err(Error::unsupported(
                "odd picture dimensions",
                "4:2:0 chroma is half resolution in both directions, and the \
                 frame cropping offsets are in two-sample units (7.4.2.1.1); \
                 encode at even width and height",
            ));
        }
        if cfg.width > 8192 || cfg.height > 8192 {
            return Err(Error::unsupported(
                "picture larger than 8192 samples",
                "no level defines a picture this large for this profile",
            ));
        }
        let mb_w = cfg.width.div_ceil(16);
        let mb_h = cfg.height.div_ceil(16);
        let fps = f64::from(cfg.framerate.max(1.0));
        // A rational tick close to the requested rate; time_scale is twice the
        // frame rate for progressive frames (E.2.1).
        let timing = rational_timing(fps);
        let seq = SeqParams {
            width: cfg.width,
            height: cfg.height,
            mb_w,
            mb_h,
            timing,
            bitrate: cfg.bitrate,
            cabac: cfg.cabac,
            transform_8x8: cfg.transform_8x8,
        };
        let sps_rbsp = write_sps(&seq);
        let sps = Sps::parse(&sps_rbsp)?;
        let pps_rbsp = write_pps(cfg.cabac, cfg.transform_8x8);
        let threads = if cfg.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            cfg.threads
        }
        .clamp(1, mb_h as usize);
        let workers = (0..threads).map(|_| Picture::default()).collect();
        let fixed_qp = (cfg.bitrate == 0).then_some(cfg.qp.clamp(0, 51));
        Ok(Encoder {
            sps_nal: annex_b(NalUnitType::Sps, 3, &sps_rbsp),
            pps_nal: annex_b(NalUnitType::Pps, 3, &pps_rbsp),
            sps,
            reference: Picture::default(),
            workers,
            src: Source {
                y: Vec::new(),
                u: Vec::new(),
                v: Vec::new(),
                stride: mb_w as usize * 16,
                c_stride: mb_w as usize * 8,
            },
            rc: RateControl::new(cfg.bitrate, fps, cfg.gop_size, fixed_qp),
            threads,
            coded_width: mb_w as usize * 16,
            coded_height: mb_h as usize * 16,
            frames: 0,
            frame_num: 0,
            idr_pic_id: 0,
            next_id: 0,
            have_reference: false,
            t8x8_mbs: (0, 0),
            cfg,
        })
    }

    /// The configuration in force.
    /// Macroblocks coded so far with `transform_size_8x8_flag` set, as
    /// `(intra, inter)`: the 8x8 transform's share of the stream.
    pub fn transform_8x8_mbs(&self) -> (u64, u64) {
        self.t8x8_mbs
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// The reconstruction of the last coded picture, cropped, as I420 planes.
    /// This is what a conformant decoder produces for that picture — the tests
    /// compare it against this crate's decoder and against the oracle.
    pub fn reconstruction(&self) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        if !self.have_reference {
            return None;
        }
        let pic = &self.reference;
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        let mut y = Vec::with_capacity(w * h);
        for row in 0..h {
            let o = pic.y.at(0, row);
            y.extend_from_slice(&pic.y.data[o..o + w]);
        }
        let (cw, ch) = (w / 2, h / 2);
        let mut planes = [Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch)];
        for (comp, out) in planes.iter_mut().enumerate() {
            let plane = if comp == 0 { &pic.cb } else { &pic.cr };
            for row in 0..ch {
                let o = plane.at(0, row);
                out.extend_from_slice(&plane.data[o..o + cw]);
            }
        }
        let [u, v] = planes;
        Some((y, u, v))
    }

    /// Code one picture, in display order, and hand back its access unit.
    pub fn encode(&mut self, frame: &PictureView<'_>) -> Result<EncodedPicture> {
        if frame.width != self.cfg.width || frame.height != self.cfg.height {
            return Err(Error::corrupt("frame size differs from the encoder's"));
        }
        self.pad_source(frame)?;
        let idr = self.cfg.gop_size <= 1
            || self
                .frames
                .is_multiple_of(u64::from(self.cfg.gop_size.max(1)));
        let idr = idr || !self.have_reference;
        let qp = self.rc.frame_qp(idr);
        if idr {
            self.frame_num = 0;
        }
        let slice_type = if idr { SliceType::I } else { SliceType::P };

        let mb_w = self.sps.mb_width as usize;
        let mb_h = self.sps.mb_height as usize;
        let bands = split_bands(mb_h, self.threads);
        let picture_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        // Per-band coding. Every worker owns its picture and its bitstream;
        // the only shared state is the immutable reference picture and source.
        let sps = &self.sps;
        let src = &self.src;
        let cfg = &self.cfg;
        let rc = &self.rc;
        let reference = (!idr).then_some(&self.reference);
        let frame_num = self.frame_num;
        let idr_pic_id = self.idr_pic_id;
        let target = rc.frame_target(idr);
        let total_mbs = (mb_w * mb_h) as f64;

        let mut outputs: Vec<Vec<u8>> = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(bands.len());
            for (slice_id, (&(row0, row1), worker)) in
                bands.iter().zip(self.workers.iter_mut()).enumerate()
            {
                handles.push(scope.spawn(move || {
                    worker.start(sps);
                    worker.id = picture_id;
                    let band_mbs = ((row1 - row0) * mb_w) as f64;
                    code_band(
                        worker,
                        BandJob {
                            src,
                            reference,
                            cfg,
                            rc,
                            slice_id: slice_id as u16,
                            slice_type,
                            first_mb: (row0 * mb_w) as u32,
                            rows: (row0, row1),
                            qp,
                            cabac: cfg.cabac,
                            frame_num,
                            idr,
                            idr_pic_id,
                            band_target: target * band_mbs / total_mbs,
                        },
                    )
                }));
            }
            for h in handles {
                outputs.push(h.join().expect("a band worker panicked"));
            }
        });

        // Stitch the bands into worker 0's picture, then filter the whole
        // picture the way a decoder does.
        let (first, rest) = self.workers.split_at_mut(1);
        let master = &mut first[0];
        master.slices.clear();
        for _ in 0..bands.len() {
            master.slices.push(DeblockParams {
                disable_deblock_idc: 0,
                alpha_offset: 0,
                beta_offset: 0,
                cb_qp_offset: 0,
                cr_qp_offset: 0,
            });
        }
        for (band, worker) in bands.iter().skip(1).zip(rest.iter()) {
            merge_band(master, worker, *band);
        }
        for &f in &master.mb_flags {
            if f & FLAG_TRANS8X8 != 0 {
                if f & FLAG_INTER != 0 {
                    self.t8x8_mbs.1 += 1;
                } else {
                    self.t8x8_mbs.0 += 1;
                }
            }
        }
        deblock_picture(master);
        master.extend_borders();
        master.complete = true;
        std::mem::swap(master, &mut self.reference);
        self.have_reference = true;

        // Assemble the access unit.
        let mut au = Vec::with_capacity(outputs.iter().map(Vec::len).sum::<usize>() + 64);
        if idr {
            au.extend_from_slice(&self.sps_nal);
            au.extend_from_slice(&self.pps_nal);
        }
        let unit = if idr {
            NalUnitType::SliceIdr
        } else {
            NalUnitType::Slice
        };
        for rbsp in &outputs {
            au.extend_from_slice(&annex_b(unit, 3, rbsp));
        }

        let bits = au.len() as u64 * 8;
        self.rc.update(idr, qp, bits);
        self.frames += 1;
        self.frame_num = (self.frame_num + 1) % (1 << headers::LOG2_MAX_FRAME_NUM);
        if idr {
            self.idr_pic_id ^= 1;
        }
        Ok(EncodedPicture {
            au,
            key_frame: idr,
            qp,
        })
    }

    /// Copy the caller's planes into whole-macroblock buffers, replicating the
    /// edge where the picture does not fill the last macroblock.
    fn pad_source(&mut self, f: &PictureView<'_>) -> Result<()> {
        let (w, h) = (f.width as usize, f.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        let y_stride = if f.y_stride == 0 { w } else { f.y_stride };
        let c_stride = if f.c_stride == 0 { cw } else { f.c_stride };
        if f.y.len() < y_stride * (h - 1) + w
            || f.u.len() < c_stride * (ch - 1) + cw
            || f.v.len() < c_stride * (ch - 1) + cw
        {
            return Err(Error::corrupt("source plane shorter than its stated size"));
        }
        pad_plane(
            &mut self.src.y,
            f.y,
            y_stride,
            w,
            h,
            self.coded_width,
            self.coded_height,
        );
        pad_plane(
            &mut self.src.u,
            f.u,
            c_stride,
            cw,
            ch,
            self.coded_width / 2,
            self.coded_height / 2,
        );
        pad_plane(
            &mut self.src.v,
            f.v,
            c_stride,
            cw,
            ch,
            self.coded_width / 2,
            self.coded_height / 2,
        );
        self.src.stride = self.coded_width;
        self.src.c_stride = self.coded_width / 2;
        Ok(())
    }
}

/// Everything one band worker needs.
struct BandJob<'a> {
    src: &'a Source,
    reference: Option<&'a Picture>,
    cfg: &'a EncoderConfig,
    rc: &'a RateControl,
    slice_id: u16,
    slice_type: SliceType,
    first_mb: u32,
    /// Macroblock rows `[start, end)`.
    rows: (usize, usize),
    qp: i32,
    cabac: bool,
    frame_num: u32,
    idr: bool,
    idr_pic_id: u32,
    band_target: f64,
}

/// Code one slice into its own RBSP.
fn code_band(pic: &mut Picture, job: BandJob<'_>) -> Vec<u8> {
    let mb_w = pic.mb_w;
    let mut w = BitWriter::with_capacity(4096);
    write_slice_header(
        &mut w,
        &SliceParams {
            first_mb: job.first_mb,
            slice_type: job.slice_type,
            frame_num: job.frame_num,
            idr: job.idr,
            idr_pic_id: job.idr_pic_id,
            qp: job.qp,
            cabac: job.cabac,
        },
    );
    // CABAC starts on a byte boundary after cabac_alignment_one_bit (7.3.4).
    let mut w = if job.cabac {
        // cabac_init_idc 0 is the initialisation column this encoder writes.
        EncEntropy::cabac(w, job.qp, usize::from(job.slice_type != SliceType::I))
    } else {
        EncEntropy::cavlc(w)
    };
    let header_bits = w.bit_len();
    let mut e = MbEnc {
        src: job.src,
        reference: job.reference,
        slice_type: job.slice_type,
        slice_id: job.slice_id,
        qp: job.qp,
        target_qp: job.qp,
        lambda: lambda_for(job.qp),
        preset: job.cfg.preset,
        transform_8x8: job.cfg.transform_8x8,
        ls: LevelScale4x4::new(&[16; 16]),
        ls8: LevelScale8x8::new(&[16; 64]),
        mb_ctx: crate::entropy::MbCtx::default(),
        skip_inc: 0,
        qp_delta_inc: 0,
    };
    let rows = job.rows.1 - job.rows.0;
    for (n, mb_y) in (job.rows.0..job.rows.1).enumerate() {
        // Row-level rate control: what this band has spent against its
        // pro-rata budget.
        if n > 0 {
            let spent = w.bit_len() - header_bits;
            let expected = job.band_target * n as f64 / rows as f64;
            let delta = job.rc.row_delta(spent, expected);
            e.target_qp = (job.qp + delta).clamp(10, 51);
            e.lambda = lambda_for(e.target_qp);
        }
        for mb_x in 0..mb_w {
            let addr = mb_y * mb_w + mb_x;
            encode_mb(pic, &mut e, &mut w, addr);
            // end_of_slice_flag, which CABAC codes after every macroblock.
            w.end_of_slice(mb_y + 1 == job.rows.1 && mb_x + 1 == mb_w);
        }
    }
    w.finish(job.slice_type == SliceType::P)
}

/// Lagrangian multiplier for mode decision in the SATD domain.
fn lambda_for(qp: i32) -> i32 {
    // 2^((qp - 12) / 6), the usual ladder, floored at 1.
    (((f64::from(qp) - 12.0) / 6.0).exp2().round() as i32).max(1)
}

/// Split `mb_h` macroblock rows into at most `n` contiguous bands.
fn split_bands(mb_h: usize, n: usize) -> Vec<(usize, usize)> {
    let n = n.clamp(1, mb_h);
    let mut out = Vec::with_capacity(n);
    let mut row = 0;
    for i in 0..n {
        let end = (i + 1) * mb_h / n;
        out.push((row, end));
        row = end;
    }
    out
}

/// Copy one band's samples and macroblock metadata from a worker picture into
/// the picture the deblocking filter will run over.
fn merge_band(dst: &mut Picture, src: &Picture, (row0, row1): (usize, usize)) {
    let mb_w = dst.mb_w;
    // Planes.
    for (d, s, mb_size) in [
        (&mut dst.y, &src.y, 16usize),
        (&mut dst.cb, &src.cb, 8),
        (&mut dst.cr, &src.cr, 8),
    ] {
        let stride = d.stride;
        let start = d.at(0, row0 * mb_size);
        let rows = (row1 - row0) * mb_size;
        let width = d.width;
        for r in 0..rows {
            let o = start + r * stride;
            d.data[o..o + width].copy_from_slice(&s.data[o..o + width]);
        }
    }
    // Per-macroblock arrays.
    let mb_range = row0 * mb_w..row1 * mb_w;
    dst.mb_qp[mb_range.clone()].copy_from_slice(&src.mb_qp[mb_range.clone()]);
    dst.mb_flags[mb_range.clone()].copy_from_slice(&src.mb_flags[mb_range.clone()]);
    dst.mb_cbp[mb_range.clone()].copy_from_slice(&src.mb_cbp[mb_range.clone()]);
    dst.mb_dc_cbf[mb_range.clone()].copy_from_slice(&src.mb_dc_cbf[mb_range.clone()]);
    dst.mb_slice[mb_range.clone()].copy_from_slice(&src.mb_slice[mb_range]);
    // Per-4x4-block arrays.
    let b = row0 * 4 * mb_w * 4..row1 * 4 * mb_w * 4;
    dst.nz_y[b.clone()].copy_from_slice(&src.nz_y[b.clone()]);
    dst.i4_modes[b.clone()].copy_from_slice(&src.i4_modes[b.clone()]);
    dst.mv[b.clone()].copy_from_slice(&src.mv[b.clone()]);
    dst.ref_idx[b.clone()].copy_from_slice(&src.ref_idx[b.clone()]);
    dst.ref_id[b.clone()].copy_from_slice(&src.ref_id[b.clone()]);
    dst.mvd_abs[b.clone()].copy_from_slice(&src.mvd_abs[b.clone()]);
    dst.blk[b.clone()].copy_from_slice(&src.blk[b]);
    // Per-chroma-block arrays.
    let c = row0 * 2 * mb_w * 2..row1 * 2 * mb_w * 2;
    for comp in 0..2 {
        let (d, s) = (&mut dst.nz_c[comp], &src.nz_c[comp]);
        d[c.clone()].copy_from_slice(&s[c.clone()]);
    }
    dst.decoded_mbs += (row1 - row0) * mb_w;
}

/// Copy `src` into a `out_w` x `out_h` buffer, replicating the right and
/// bottom edges into the padding.
fn pad_plane(
    out: &mut Vec<u8>,
    src: &[u8],
    stride: usize,
    w: usize,
    h: usize,
    out_w: usize,
    out_h: usize,
) {
    out.clear();
    out.resize(out_w * out_h, 0);
    for row in 0..out_h {
        let sr = row.min(h - 1);
        let line = &src[sr * stride..sr * stride + w];
        let dst = &mut out[row * out_w..row * out_w + out_w];
        dst[..w].copy_from_slice(line);
        if out_w > w {
            let edge = line[w - 1];
            dst[w..].fill(edge);
        }
    }
}

/// Wrap an RBSP as one Annex-B NAL unit with a 4-byte start code.
fn annex_b(unit: NalUnitType, ref_idc: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + 8);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.push((ref_idc << 5) | unit_code(unit));
    escape_rbsp(rbsp, &mut out);
    out
}

/// `nal_unit_type` of the units this encoder emits.
fn unit_code(unit: NalUnitType) -> u8 {
    match unit {
        NalUnitType::Slice => 1,
        NalUnitType::SliceIdr => 5,
        NalUnitType::Sps => 7,
        NalUnitType::Pps => 8,
        _ => 0,
    }
}

/// A `(num_units_in_tick, time_scale)` pair for a frame rate, exact for the
/// broadcast rates (23.976, 29.97, 59.94) and within a part in 10^5 otherwise.
fn rational_timing(fps: f64) -> (u32, u32) {
    for (num, den) in [
        (24000u32, 1001u32),
        (30000, 1001),
        (60000, 1001),
        (120000, 1001),
    ] {
        if (fps - f64::from(num) / f64::from(den)).abs() < 1e-3 {
            return (den, num * 2);
        }
    }
    let scaled = (fps * 1000.0).round().max(1.0);
    (1000, (scaled * 2.0).min(f64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_every_row_once() {
        for mb_h in [1usize, 5, 68, 135] {
            for threads in [1usize, 3, 12, 200] {
                let bands = split_bands(mb_h, threads);
                assert!(!bands.is_empty());
                assert_eq!(bands[0].0, 0);
                assert_eq!(bands.last().unwrap().1, mb_h);
                for pair in bands.windows(2) {
                    assert_eq!(pair[0].1, pair[1].0);
                }
                assert!(bands.iter().all(|&(a, b)| a < b), "empty band: {bands:?}");
            }
        }
    }

    #[test]
    fn timing_is_exact_for_broadcast_rates() {
        assert_eq!(rational_timing(24000.0 / 1001.0), (1001, 48000));
        assert_eq!(rational_timing(30000.0 / 1001.0), (1001, 60000));
        assert_eq!(rational_timing(25.0), (1000, 50000));
    }

    /// Odd dimensions are refused by name rather than coded wrong.
    #[test]
    fn odd_dimensions_are_refused() {
        let Err(err) = Encoder::new(EncoderConfig::new(641, 480)) else {
            panic!("an odd width must be refused");
        };
        assert!(matches!(err, Error::Unsupported { .. }), "{err}");
        assert!(Encoder::new(EncoderConfig::new(640, 480)).is_ok());
    }
}
