//! A key-frame picture encoder: a planar 4:2:0 picture in, an AV1 stream out.
//!
//! This is the layer that turns the pieces below it — DC intra prediction, the
//! forward transform and quantizer of [`crate::transform`], and the tile writer
//! of [`crate::tile`] — into something that takes a picture. Every block is
//! DC-predicted and 32x32 (its chroma 16x16), which is the subset the tile
//! writer codes; block sizes, transform types and mode decision are what a
//! rate-distortion loop above this would choose between.
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
use crate::obu::temporal_delimiter;
use crate::sequence::sequence_header_obu;
use crate::tile::{BlockCoeffs, Coeff, Superblock, sb_coeff_key_frame_tile};
use crate::transform::{dequant_and_inverse, forward_and_quantize};

/// The side of the luma blocks this encoder codes, in samples.
const BLOCK: usize = 32;

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

/// `dc_predict` (spec 7.11.2): the average of the reconstructed row above and
/// the reconstructed column to the left, of whichever of them exists.
fn dc_predict(above: Option<&[u8]>, left: Option<&[u8]>) -> u8 {
    match (above, left) {
        (None, None) => 128,
        (Some(a), None) => {
            let sum: u32 = a.iter().map(|&s| u32::from(s)).sum();
            ((sum + (a.len() as u32 >> 1)) / a.len() as u32) as u8
        }
        (None, Some(l)) => {
            let sum: u32 = l.iter().map(|&s| u32::from(s)).sum();
            ((sum + (l.len() as u32 >> 1)) / l.len() as u32) as u8
        }
        (Some(a), Some(l)) => {
            let sum: u32 = a.iter().chain(l).map(|&s| u32::from(s)).sum();
            let count = (a.len() + l.len()) as u32;
            ((sum + (count >> 1)) / count) as u8
        }
    }
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
    /// Codes one block: predicts it from the reconstruction, transforms and
    /// quantizes what is left, writes the reconstruction back and hands the
    /// levels to the caller.
    fn code_block(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        base_q_idx: u8,
        deadzone: f64,
    ) -> Vec<Coeff> {
        let above = (y > 0).then(|| &self.reconstruction[(y - 1) * self.width + x..][..side]);
        let left = (x > 0).then(|| {
            (0..side)
                .map(|i| self.reconstruction[(y + i) * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let prediction = i32::from(dc_predict(above, left.as_deref()));

        let mut residual = vec![0i32; side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col]) - prediction;
            }
        }
        let levels = forward_and_quantize(&residual, side, 8, i32::from(base_q_idx), deadzone);
        let coded = dequant_and_inverse(&levels, side, 8, i32::from(base_q_idx));
        for row in 0..side {
            for col in 0..side {
                self.reconstruction[(y + row) * self.width + x + col] =
                    (prediction + coded[row * side + col]).clamp(0, 255) as u8;
            }
        }
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
    picture.check()?;
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

    let (cols, rows) = (picture.width / BLOCK, picture.height / BLOCK);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let mut superblocks = Vec::with_capacity(sb_cols * sb_rows);
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
                blocks.push(BlockCoeffs {
                    luma: luma.code_block(x, y, BLOCK, base_q_idx, deadzone),
                    u: chroma[0].code_block(x / 2, y / 2, BLOCK / 2, base_q_idx, deadzone),
                    v: chroma[1].code_block(x / 2, y / 2, BLOCK / 2, base_q_idx, deadzone),
                    mode: 0,
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
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &skip, "-i", &clip, "-frames:v", "1"])
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
        let picture = Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        };

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
