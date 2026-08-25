//! A key-frame picture encoder: a planar 4:2:0 picture in, an AV1 stream out.
//!
//! This is the layer that turns the pieces below it — the intra predictors of
//! [`crate::intra`], the forward transform and quantizer of
//! [`crate::transform`], and the tile writer of [`crate::tile`] — into
//! something that takes a picture. Every block is 32x32 (its chroma 16x16),
//! which is the subset the tile writer codes, and each one picks its luma mode
//! from the seven non-directional ones by rate-distortion; chroma is predicted
//! DC, which is the mode the tile writer codes for it. Block sizes and
//! transform types are what a loop above this would choose between.
//!
//! The encoder carries its own reconstruction, because prediction reads it: a
//! block predicts from the reconstructed samples above and to its left, exactly
//! as the decoder will. That makes the reconstruction a claim about what a
//! decoder produces, and it is gated as one — sample for sample against ffmpeg.

use ec_av1_syntax::sequence::{ColorConfig, OperatingPoint, SequenceHeader};
use ec_av1_syntax::{
    ChromaSamplePosition, FrameHeader, FrameType, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
    TxMode,
};
use ec_core::{Error, Result};

use crate::frame::frame_obu;
use crate::intra::{DC_PRED, NON_DIRECTIONAL};
use crate::obu::temporal_delimiter;
use crate::quant::ac_q;
use crate::sequence::sequence_header_obu;
use crate::tile::{BlockCoeffs, Coeff, Superblock, sb_coeff_key_frame_tile};
use crate::transform::{dequant_and_inverse, forward_and_quantize};

/// The side of the luma blocks this encoder codes, in samples.
const BLOCK: usize = 32;

/// How heavily the mode search weighs rate against squared error, in units of
/// the quantizer's reconstruction step squared per bit.
///
/// Swept over three clips and two synthetic pictures at 0, 0.05, 0.1, 0.2, 0.4
/// and 0.8 (`probe_lambda`, and the table in the lane report): 0 leaves half the
/// saving on the table on a picture that runs one way (-35.6% against -59.5% on
/// stripes), and everything from 0.1 up gives a little back on screen capture
/// (-10.7% against -11.5%) and on a hand-held clip. 0.05 is the best point on
/// every clip measured.
const LAMBDA_SCALE: f64 = 0.05;

/// One 8-bit planar 4:2:0 picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The picture's width in luma samples.
    pub width: usize,
    /// Its height in luma samples.
    pub height: usize,
    /// The luma plane, `width * height` samples in raster order.
    pub y: Vec<u8>,
    /// The U plane, at half the width and half the height.
    pub u: Vec<u8>,
    /// The V plane, the same shape as U.
    pub v: Vec<u8>,
}

impl Picture {
    /// A mid-grey picture of the given size.
    #[must_use]
    pub fn grey(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            y: vec![128; width * height],
            u: vec![128; width * height / 4],
            v: vec![128; width * height / 4],
        }
    }

    fn check(&self) -> Result<()> {
        if self.width == 0
            || !self.width.is_multiple_of(BLOCK)
            || !self.height.is_multiple_of(BLOCK)
            || !self.height.is_multiple_of(BLOCK)
        {
            return Err(Error::unsupported(
                "AV1 encode",
                "the picture must be a whole number of 32x32 blocks in each direction",
            ));
        }
        let (luma, chroma) = (self.width * self.height, self.width * self.height / 4);
        if self.y.len() != luma || self.u.len() != chroma || self.v.len() != chroma {
            return Err(Error::unsupported(
                "AV1 encode",
                "each plane must carry one sample per position at 4:2:0",
            ));
        }
        Ok(())
    }
}

/// What one call to [`encode_key_frame`] produced.
#[derive(Clone, Debug)]
pub struct Encoded {
    /// The AV1 stream: a temporal delimiter, a sequence header OBU and a frame
    /// OBU carrying one tile.
    pub stream: Vec<u8>,
    /// What a decoder will produce from `stream` — the encoder's own
    /// reconstruction, which its prediction was built on.
    pub reconstruction: Picture,
    /// The luma intra mode each block was coded under, in the order the blocks
    /// are coded — raster order among the quadrants of each superblock.
    pub modes: Vec<u8>,
}

/// The sequence and frame headers a picture of this size is coded under: one
/// tile, one transform size per block, no in-loop filtering, no CDF adaptation
/// and no tool the tile writer does not code.
///
/// # Errors
/// Returns an error when the picture is larger than the 16-bit frame size the
/// sequence header carries.
pub fn key_frame_headers(
    width: usize,
    height: usize,
    base_q_idx: u8,
) -> Result<(SequenceHeader, FrameHeader)> {
    let bits = |n: usize| -> Result<u32> {
        let n = u32::try_from(n).map_err(|_| too_large())?;
        if n == 0 || n > 1 << 16 {
            return Err(too_large());
        }
        Ok((32 - (n - 1).leading_zeros()).max(1))
    };
    let (frame_width_bits, frame_height_bits) = (bits(width)?, bits(height)?);
    let (w, h) = (width as u32, height as u32);
    let seq = SequenceHeader {
        seq_profile: 0,
        operating_points: vec![OperatingPoint {
            seq_level_idx: 8,
            ..OperatingPoint::default()
        }],
        frame_width_bits,
        frame_height_bits,
        max_frame_width: w,
        max_frame_height: h,
        use_128x128_superblock: false,
        enable_filter_intra: false,
        enable_intra_edge_filter: false,
        enable_order_hint: true,
        order_hint_bits: 7,
        enable_superres: false,
        enable_cdef: false,
        enable_restoration: false,
        seq_force_screen_content_tools: 0,
        seq_force_integer_mv: 0,
        color_config: ColorConfig {
            bit_depth: 8,
            mono_chrome: false,
            num_planes: 3,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
            chroma_sample_position: ChromaSamplePosition::Unknown,
            separate_uv_delta_q: false,
        },
        still_picture: false,
        reduced_still_picture_header: false,
        timing_info: None,
        decoder_model_info: None,
        initial_display_delay_present_flag: false,
        operating_point: 0,
        operating_point_idc: 0,
        frame_id_numbers_present_flag: false,
        delta_frame_id_length: 0,
        additional_frame_id_length: 0,
        enable_interintra_compound: false,
        enable_masked_compound: false,
        enable_warped_motion: false,
        enable_dual_filter: false,
        enable_jnt_comp: false,
        enable_ref_frame_mvs: false,
        film_grain_params_present: false,
    };
    let (mi_cols, mi_rows) = (w / 4, h / 4);
    let header = FrameHeader {
        frame_type: FrameType::Key,
        frame_is_intra: true,
        show_frame: true,
        error_resilient_mode: true,
        disable_cdf_update: true,
        force_integer_mv: true,
        refresh_frame_flags: 0xFF,
        primary_ref_frame: PRIMARY_REF_NONE,
        frame_width: w,
        frame_height: h,
        upscaled_width: w,
        render_width: w,
        render_height: h,
        mi_cols,
        mi_rows,
        tile_info: TileInfo {
            uniform_spacing: true,
            cols: 1,
            rows: 1,
            mi_col_starts: vec![0, mi_cols],
            mi_row_starts: vec![0, mi_rows],
            tile_size_bytes: 1,
            ..TileInfo::default()
        },
        quantization: QuantizationParams {
            base_q_idx,
            ..QuantizationParams::default()
        },
        tx_mode: TxMode::Largest,
        ..FrameHeader::default()
    };
    Ok((seq, header))
}

fn too_large() -> Error {
    Error::unsupported(
        "AV1 encode",
        "the picture is larger than a frame size can carry",
    )
}

/// One plane of the picture being coded, and the reconstruction being built
/// beside it.
struct Plane<'a> {
    source: &'a [u8],
    reconstruction: Vec<u8>,
    width: usize,
    height: usize,
}

impl Plane<'_> {
    /// The reconstructed samples a block at `(x, y)` predicts from: the row
    /// above it, the column to its left and the sample between them, each
    /// missing where the block sits against an edge of the frame.
    fn edges(
        &self,
        x: usize,
        y: usize,
        side: usize,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<u8>) {
        let above =
            (y > 0).then(|| self.reconstruction[(y - 1) * self.width + x..][..side].to_vec());
        let left = (x > 0).then(|| {
            (0..side)
                .map(|i| self.reconstruction[(y + i) * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner = (x > 0 && y > 0).then(|| self.reconstruction[(y - 1) * self.width + x - 1]);
        (above, left, corner)
    }

    /// Codes one block under one mode without committing it: hands back the
    /// levels, the block the decoder would reconstruct, the squared error
    /// against the source and an estimate of what the levels cost in bits.
    fn trial(
        &self,
        x: usize,
        y: usize,
        side: usize,
        mode: u8,
        base_q_idx: u8,
        deadzone: f64,
    ) -> Trial {
        let (above, left, corner) = self.edges(x, y, side);
        let mut prediction = vec![0u8; side * side];
        crate::intra::predict(
            mode,
            above.as_deref(),
            left.as_deref(),
            corner,
            side,
            &mut prediction,
        );

        let mut residual = vec![0i32; side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        let levels = forward_and_quantize(&residual, side, 8, i32::from(base_q_idx), deadzone);
        let coded = dequant_and_inverse(&levels, side, 8, i32::from(base_q_idx));

        let mut reconstruction = vec![0u8; side * side];
        let mut sse = 0.0;
        for row in 0..side {
            for col in 0..side {
                let i = row * side + col;
                let sample = (i32::from(prediction[i]) + coded[i]).clamp(0, 255) as u8;
                reconstruction[i] = sample;
                let error = f64::from(
                    i32::from(self.source[(y + row) * self.width + x + col]) - i32::from(sample),
                );
                sse += error * error;
            }
        }
        // What a level costs is close enough to its magnitude's width plus a
        // sign and a run, which is all the search needs to rank modes.
        let bits: f64 = levels
            .iter()
            .filter(|&&level| level != 0)
            .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
            .sum();
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// Writes a trial's reconstruction back into the plane.
    fn commit(&mut self, x: usize, y: usize, side: usize, trial: &Trial) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&trial.reconstruction[row * side..][..side]);
        }
    }

    /// Codes one block under a fixed mode, committing it.
    fn code_block(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        mode: u8,
        base_q_idx: u8,
        deadzone: f64,
    ) -> Vec<Coeff> {
        let trial = self.trial(x, y, side, mode, base_q_idx, deadzone);
        self.commit(x, y, side, &trial);
        coeffs(&trial.levels, side)
    }

    /// Codes one block under every mode the search offers and commits the one
    /// whose squared error and estimated rate come out cheapest.
    fn search_block(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        search: &Search,
    ) -> (Vec<Coeff>, u8) {
        let mut best: Option<(f64, u8, Trial)> = None;
        for &mode in search.modes {
            let trial = self.trial(x, y, side, mode, search.base_q_idx, search.deadzone);
            let cost = trial.sse + search.lambda * trial.bits;
            if best
                .as_ref()
                .is_none_or(|(best_cost, _, _)| cost < *best_cost)
            {
                best = Some((cost, mode, trial));
            }
        }
        let (_, mode, trial) = best.expect("the search offers at least one mode");
        self.commit(x, y, side, &trial);
        (coeffs(&trial.levels, side), mode)
    }
}

/// What the luma mode search picks between, and under what terms.
struct Search<'a> {
    base_q_idx: u8,
    deadzone: f64,
    lambda: f64,
    modes: &'a [u8],
}

/// One mode's coding of one block, before it is committed.
struct Trial {
    levels: Vec<i32>,
    reconstruction: Vec<u8>,
    sse: f64,
    bits: f64,
}

/// The non-zero levels of a block, as the tile writer takes them.
fn coeffs(levels: &[i32], side: usize) -> Vec<Coeff> {
    levels
        .iter()
        .enumerate()
        .filter(|&(_, &level)| level != 0)
        .map(|(i, &level)| Coeff {
            row: (i / side) as u8,
            col: (i % side) as u8,
            level,
        })
        .collect()
}

/// Encodes one picture as a key frame.
///
/// `base_q_idx` is the frame's quantizer index, and has to sit in the band
/// whose coefficient CDFs the tile writer carries (61..=120). `deadzone` is the
/// quantizer's rounding offset: 0.5 rounds to nearest, and smaller values trade
/// fidelity for rate.
///
/// # Errors
/// Returns an error when the picture is not a whole number of 32x32 blocks,
/// when its planes are not 4:2:0 of that size, or when the tile writer refuses
/// the quantizer index.
pub fn encode_key_frame(picture: &Picture, base_q_idx: u8, deadzone: f64) -> Result<Encoded> {
    encode_key_frame_with_modes(picture, base_q_idx, deadzone, &NON_DIRECTIONAL)
}

/// Encodes one picture as a key frame, choosing each block's luma mode from
/// `modes` alone.
///
/// This is what an ablation measures against: `&[DC_PRED]` is the encoder
/// before the mode search, and [`NON_DIRECTIONAL`] is what
/// [`encode_key_frame`] uses.
///
/// # Errors
/// The same as [`encode_key_frame`], and additionally when `modes` is empty or
/// names a mode [`crate::intra::predict`] does not predict.
pub fn encode_key_frame_with_modes(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
) -> Result<Encoded> {
    picture.check()?;
    if modes.is_empty() {
        return Err(Error::unsupported(
            "AV1 encode",
            "a mode search needs at least one mode to choose from",
        ));
    }
    if let Some(bad) = modes.iter().find(|m| !NON_DIRECTIONAL.contains(m)) {
        return Err(Error::unsupported(
            "AV1 encode",
            format!("intra mode {bad} is not one the encoder predicts"),
        ));
    }
    let (seq, header) = key_frame_headers(picture.width, picture.height, base_q_idx)?;

    let mut luma = Plane {
        source: &picture.y,
        reconstruction: vec![128; picture.y.len()],
        width: picture.width,
        height: picture.height,
    };
    let mut chroma = [
        Plane {
            source: &picture.u,
            reconstruction: vec![128; picture.u.len()],
            width: picture.width / 2,
            height: picture.height / 2,
        },
        Plane {
            source: &picture.v,
            reconstruction: vec![128; picture.v.len()],
            width: picture.width / 2,
            height: picture.height / 2,
        },
    ];

    // The search trades a bit for the squared error it saves, in the units the
    // reconstruction is measured in: one step of the quantizer, squared.
    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: LAMBDA_SCALE * step * step,
        modes,
    };

    let (cols, rows) = (picture.width / BLOCK, picture.height / BLOCK);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let mut superblocks = Vec::with_capacity(sb_cols * sb_rows);
    let mut modes = Vec::with_capacity(cols * rows);
    for sb_row in 0..sb_rows {
        for sb_col in 0..sb_cols {
            // The quadrants of a superblock are coded in the order the decoder
            // walks them, which for a 64x64 split into 32x32 blocks is raster
            // order among the quadrants that are inside the frame.
            let mut blocks = Vec::with_capacity(4);
            for quadrant in 0..4 {
                let (col, row) = (sb_col * 2 + quadrant % 2, sb_row * 2 + quadrant / 2);
                if col >= cols || row >= rows {
                    continue;
                }
                let (x, y) = (col * BLOCK, row * BLOCK);
                let (luma_coeffs, mode) = luma.search_block(x, y, BLOCK, &search);
                modes.push(mode);
                blocks.push(BlockCoeffs {
                    // Chroma is predicted DC because that is the mode the tile
                    // writer codes for it.
                    u: chroma[0].code_block(x / 2, y / 2, BLOCK / 2, DC_PRED, base_q_idx, deadzone),
                    v: chroma[1].code_block(x / 2, y / 2, BLOCK / 2, DC_PRED, base_q_idx, deadzone),
                    luma: luma_coeffs,
                    mode,
                });
            }
            superblocks.push(Superblock::Split(blocks));
        }
    }

    let tile = sb_coeff_key_frame_tile(header.mi_cols, header.mi_rows, base_q_idx, &superblocks)?;
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq)?);
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile)?);

    let [u, v] = chroma;
    Ok(Encoded {
        stream,
        modes,
        reconstruction: Picture {
            width: luma.width,
            height: luma.height,
            y: luma.reconstruction,
            u: u.reconstruction,
            v: v.reconstruction,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intra::{H_PRED, V_PRED};
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the three planes.
    fn ffmpeg_decode(stream: &[u8], width: usize, height: usize) -> Picture {
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg failed to start");
        child
            .stdin
            .take()
            .expect("ffmpeg stdin")
            .write_all(stream)
            .expect("writing the stream to ffmpeg");
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        }
    }

    /// A picture with something of everything in it: a gradient, an edge, a
    /// ripple and a block of flat colour, none of them aligned to the block
    /// grid.
    fn test_card(width: usize, height: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let gradient = x as f64 * 200.0 / width as f64;
                let ripple = 30.0
                    * (x as f64 * std::f64::consts::PI / 23.0).sin()
                    * (y as f64 * std::f64::consts::PI / 37.0).cos();
                let edge = if x > width * 3 / 7 && y > height / 3 {
                    40.0
                } else {
                    0.0
                };
                picture.y[y * width + x] =
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u8;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (x * 60 / (width / 2))) as u8;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u8;
            }
        }
        picture
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let squared: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum();
        if squared == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0 * 255.0 * a.len() as f64 / squared).log10()
    }

    /// The claim the whole encoder rests on: what a decoder produces is what
    /// the encoder said it would, sample for sample, on every plane.
    ///
    /// Prediction reads the reconstruction, so a single sample of drift
    /// anywhere would spread into every block below and to the right of it —
    /// which is why this is an equality and not a tolerance.
    #[test]
    fn ffmpeg_decodes_exactly_what_the_encoder_reconstructed() {
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_decodes_exactly_what_the_encoder_reconstructed: no ffmpeg");
            return;
        }
        for &(width, height) in &[(64usize, 64usize), (96, 64), (160, 96)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(
                decoded.y, encoded.reconstruction.y,
                "{width}x{height}: luma"
            );
            assert_eq!(decoded.u, encoded.reconstruction.u, "{width}x{height}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "{width}x{height}: V");
        }
    }

    /// One frame of a real clip, scaled to a whole number of 32x32 blocks.
    fn clip_frame(clip: &str, skip: &str, width: usize, height: usize) -> Picture {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip, "-frames:v", "1"])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        }
    }

    /// Prints what the mode search saves over DC prediction alone, for the
    /// synthetic pictures and for whatever clips `EC_AV1_CLIPS` names, so the
    /// weight the search puts on rate can be swept.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_lambda() {
        let mut pictures = vec![
            ("test card".to_string(), test_card(160, 96)),
            ("stripes".to_string(), stripes(160, 96, true)),
        ];
        if let Ok(clips) = std::env::var("EC_AV1_CLIPS") {
            for entry in clips.split(':').filter(|e| !e.is_empty()) {
                let (path, skip) = entry.split_once('@').unwrap_or((entry, "0"));
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                pictures.push((name, clip_frame(path, skip, 640, 352)));
            }
        }
        for (name, picture) in pictures {
            let dc = ladder(&picture, &[DC_PRED]);
            let searched = ladder(&picture, &NON_DIRECTIONAL);
            println!(
                "{name}: {:+.1}% rate against DC alone, {:.0}B/{:.2}dB against {:.0}B/{:.2}dB at the middle point",
                bd_rate(&dc, &searched) * 100.0,
                10f64.powf(searched[1].1),
                searched[1].0,
                10f64.powf(dc[1].1),
                dc[1].0,
            );
        }
    }

    /// A striped picture, running one way or the other. The stripes are what
    /// separates a vertical predictor from a horizontal one: a mode search
    /// reading a transposed edge would pick the wrong one of the pair, which no
    /// symmetric picture would show.
    fn stripes(width: usize, height: usize, vertical: bool) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let along = if vertical { x } else { y };
                picture.y[y * width + x] = if (along / 4) % 2 == 0 { 40 } else { 210 };
            }
        }
        picture
    }

    /// Every mode the encoder offers has to predict what the decoder predicts,
    /// not just the ones a particular picture happens to choose: each is forced
    /// over a whole picture and the reconstruction gated against ffmpeg.
    #[test]
    fn every_mode_decodes_to_what_the_encoder_predicted() {
        if !have_ffmpeg() {
            eprintln!("SKIP every_mode_decodes_to_what_the_encoder_predicted: no ffmpeg");
            return;
        }
        let picture = test_card(128, 96);
        for mode in NON_DIRECTIONAL {
            let encoded = encode_key_frame_with_modes(&picture, 100, 0.5, &[mode]).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, 128, 96);
            assert_eq!(decoded.y, encoded.reconstruction.y, "mode {mode}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "mode {mode}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "mode {mode}: V");
            assert!(
                encoded.modes.iter().all(|&m| m == mode),
                "mode {mode}: the encoder coded something else"
            );
        }
    }

    /// The search has to follow the picture: vertical stripes are cheapest
    /// predicted from the row above, horizontal ones from the column to the
    /// left. Reading the edges the other way round would swap the two answers
    /// while leaving every fidelity gate intact.
    #[test]
    fn the_search_picks_the_direction_the_picture_runs() {
        for (vertical, want) in [(true, V_PRED), (false, H_PRED)] {
            let picture = stripes(128, 96, vertical);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            // The first block of the picture has neither neighbour, so it
            // cannot tell the modes apart; every other one can.
            let picked = encoded.modes[1..].iter().filter(|&&m| m == want).count();
            assert!(
                picked * 2 > encoded.modes.len() - 1,
                "vertical={vertical}: only {picked} of {} blocks picked mode {want}, modes {:?}",
                encoded.modes.len() - 1,
                encoded.modes
            );
        }
    }

    /// What the search is for: the same picture, at the same quantizer, coded
    /// smaller and more faithfully than DC alone can manage.
    /// Rate saved at matched fidelity, over a three-point ladder: the trapezoid
    /// between the two rate-distortion curves in log-rate against PSNR, as a
    /// fraction of the reference's rate. Negative means the second curve costs
    /// less for the same picture.
    fn bd_rate(reference: &[(f64, f64)], other: &[(f64, f64)]) -> f64 {
        let low = reference[0].0.max(other[0].0);
        let high = reference[reference.len() - 1]
            .0
            .min(other[other.len() - 1].0);
        assert!(high > low, "the two ladders have to overlap in PSNR");
        let log_rate_at = |curve: &[(f64, f64)], psnr: f64| {
            let i = curve
                .windows(2)
                .position(|w| psnr >= w[0].0 && psnr <= w[1].0)
                .unwrap_or(0);
            let (x0, y0) = curve[i];
            let (x1, y1) = curve[i + 1];
            y0 + (y1 - y0) * (psnr - x0) / (x1 - x0)
        };
        let steps = 64;
        let mut area = 0.0;
        for step in 0..steps {
            let psnr = low + (high - low) * (f64::from(step) + 0.5) / f64::from(steps);
            area += log_rate_at(other, psnr) - log_rate_at(reference, psnr);
        }
        10f64.powf(area / f64::from(steps)) - 1.0
    }

    /// A ladder of (luma PSNR, log10 bytes) for one mode set, ordered by
    /// fidelity.
    fn ladder(picture: &Picture, modes: &[u8]) -> Vec<(f64, f64)> {
        let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
            .iter()
            .map(|&q| {
                let encoded = encode_key_frame_with_modes(picture, q, 0.5, modes).unwrap();
                (
                    psnr(&encoded.reconstruction.y, picture.y.as_slice()),
                    (encoded.stream.len() as f64).log10(),
                )
            })
            .collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        points
    }

    /// What the search is for: the same pictures cost less to code at the same
    /// fidelity than DC prediction alone can manage. A picture that runs one
    /// way saves far more than a busy one, which is the shape a working
    /// directional pair has.
    #[test]
    fn the_search_beats_dc_alone() {
        for (name, picture, want) in [
            ("test card", test_card(160, 96), -0.05),
            ("stripes", stripes(160, 96, true), -0.40),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &[DC_PRED]),
                &ladder(&picture, &NON_DIRECTIONAL),
            );
            assert!(
                saved < want,
                "{name}: the search saved {:.1}% of the rate, wanted at least {:.1}%",
                saved * 100.0,
                -want * 100.0
            );
        }
    }

    /// A mode the encoder cannot predict must be refused rather than coded as
    /// something else, and a search with nothing to choose from likewise.
    #[test]
    fn a_mode_the_encoder_cannot_predict_is_refused() {
        let picture = test_card(64, 64);
        let message = encode_key_frame_with_modes(&picture, 100, 0.5, &[3])
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 3"),
            "the refusal must name the mode, got {message}"
        );
        assert!(encode_key_frame_with_modes(&picture, 100, 0.5, &[]).is_err());
    }

    /// The picture that comes back is the picture that went in, to within the
    /// quantizer. A prediction that read the wrong neighbours, or a block
    /// written into the wrong place, would still decode to what the encoder
    /// reconstructed — it is this gate that says the reconstruction is of the
    /// right picture.
    #[test]
    fn the_encoded_picture_is_the_one_that_went_in() {
        let picture = test_card(160, 96);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        let luma = psnr(&encoded.reconstruction.y, &picture.y);
        assert!(luma > 36.0, "luma PSNR {luma} at q 100");
        for (plane, (got, want)) in [
            (encoded.reconstruction.u.as_slice(), picture.u.as_slice()),
            (encoded.reconstruction.v.as_slice(), picture.v.as_slice()),
        ]
        .iter()
        .enumerate()
        {
            let chroma = psnr(got, want);
            assert!(chroma > 40.0, "chroma plane {plane} PSNR {chroma} at q 100");
        }
    }

    /// A finer quantizer costs bits and buys fidelity, and a wider deadzone
    /// does the opposite. Both are monotone, which is what a rate-distortion
    /// loop above this will assume.
    #[test]
    fn fidelity_and_rate_move_with_the_quantizer() {
        let picture = test_card(128, 128);
        let mut previous: Option<(usize, f64)> = None;
        for &q in &[70u8, 90, 110] {
            let encoded = encode_key_frame(&picture, q, 0.5).unwrap();
            let quality = psnr(&encoded.reconstruction.y, &picture.y);
            if let Some((bytes, better)) = previous {
                assert!(
                    encoded.stream.len() < bytes,
                    "q {q}: {} bytes",
                    encoded.stream.len()
                );
                assert!(quality < better, "q {q}: PSNR {quality}");
            }
            previous = Some((encoded.stream.len(), quality));
        }

        let mut previous = None;
        for &deadzone in &[0.5f64, 0.3, 0.15] {
            let encoded = encode_key_frame(&picture, 100, deadzone).unwrap();
            let quality = psnr(&encoded.reconstruction.y, &picture.y);
            if let Some((bytes, better)) = previous {
                assert!(
                    encoded.stream.len() < bytes,
                    "deadzone {deadzone}: {} bytes",
                    encoded.stream.len()
                );
                assert!(quality < better, "deadzone {deadzone}: PSNR {quality}");
            }
            previous = Some((encoded.stream.len(), quality));
        }
    }

    /// A flat picture is a flat stream: every block predicts its neighbours'
    /// average, which is the picture's own value, and codes nothing.
    #[test]
    fn a_flat_picture_costs_almost_nothing() {
        let mut picture = Picture::grey(128, 128);
        picture.y.fill(97);
        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        // The first block has no neighbour and predicts 128, so it carries a
        // DC; every block after it predicts 97 and carries nothing.
        assert!(
            encoded.stream.len() < 100,
            "{} bytes for a flat picture",
            encoded.stream.len()
        );
        for (i, &s) in encoded.reconstruction.y.iter().enumerate().skip(32 * 128) {
            assert_eq!(s, 97, "sample {i} of a flat picture");
        }
    }

    /// The sizes the encoder refuses, refused for a reason rather than by
    /// panicking somewhere below.
    #[test]
    fn a_picture_off_the_block_grid_is_refused() {
        assert!(encode_key_frame(&Picture::grey(40, 32), 100, 0.5).is_err());
        assert!(encode_key_frame(&Picture::grey(32, 48), 100, 0.5).is_err());
        let mut short = Picture::grey(64, 64);
        short.u.truncate(10);
        assert!(encode_key_frame(&short, 100, 0.5).is_err());
        // The tile writer carries the CDFs of one q context only.
        assert!(encode_key_frame(&Picture::grey(64, 64), 40, 0.5).is_err());
    }

    /// A frame of real video, rather than a picture built to be easy.
    ///
    /// `EC_AV1_CLIP` names the clip and `EC_AV1_CLIP_SKIP` how far into it to
    /// seek; ffmpeg decodes one frame, crops it to the block grid, and the
    /// encoder's reconstruction has to survive the same equality gate as the
    /// synthetic pictures — real video reaches contexts a test card does not.
    #[test]
    fn a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed() {
        let Ok(clip) = std::env::var("EC_AV1_CLIP") else {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: \
                 set EC_AV1_CLIP to a clip"
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        let skip = std::env::var("EC_AV1_CLIP_SKIP").unwrap_or_else(|_| "0".into());
        // A 4:2:0 frame cropped to whole 32x32 blocks, in a size that keeps the
        // test quick while still spanning many superblocks.
        let (width, height) = (640usize, 352usize);
        let picture = clip_frame(&clip, &skip, width, height);

        let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "V");
        eprintln!(
            "{} bytes, luma PSNR {:.2} dB",
            encoded.stream.len(),
            psnr(&decoded.y, &picture.y)
        );
    }
}
