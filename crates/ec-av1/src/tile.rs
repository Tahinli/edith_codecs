//! Tile payload writer (spec 5.11), for the one block coding the encoder can
//! produce today: a key frame whose every superblock is a single 64x64
//! DC-predicted block with no residual.
//!
//! That frame decodes to a flat mid-grey picture — every sample is the value a
//! DC prediction with no neighbours produces — which is what makes it a usable
//! gate: any desync between this writer and a real decoder shows up as a
//! decode failure or a sample that is not mid-grey, with no metric in the way.
//! It is the skeleton the block modes, transform sizes and coefficients hang
//! off as they arrive.

use ec_core::{Error, Result};

use crate::cdf;
use crate::msac::SymbolEncoder;

/// `PARTITION_NONE` (spec 6.10.4): the whole block, undivided.
const PARTITION_NONE: usize = 0;
/// `DC_PRED` (spec 6.10.2), as both the luma and the chroma mode.
const DC_PRED: usize = 0;
/// Side of a superblock in 4x4 mode-info units when 128x128 superblocks are off.
const SB_MI: u32 = 16;

/// `NUM_BASE_LEVELS` (spec 3): levels above this carry a base-range tail.
const NUM_BASE_LEVELS: i32 = 2;
/// `COEFF_BASE_RANGE` (spec 3): how far the base-range tail reaches before the
/// Golomb tail takes over.
const COEFF_BASE_RANGE: i32 = 12;
/// `BR_CDF_SIZE - 1` (spec 3): the largest increment one base-range symbol
/// carries.
const BR_STEP: i32 = 3;
/// The largest level the base and base-range syntax carry between them.
const MAX_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;
/// The q-context band whose default CDFs [`crate::cdf`] carries.
const Q_CTX_2: std::ops::RangeInclusive<u8> = 61..=120;

/// Both writers here code whole superblocks only: a partial one forces the
/// partition syntax down the block tree, which they do not code yet.
fn check_superblocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(SB_MI)
        || !mi_rows.is_multiple_of(SB_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a key frame is written only for frames that are a whole number \
             of 64x64 superblocks",
        ));
    }
    Ok(())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// skipped 64x64 DC-predicted block.
///
/// `mi_cols` and `mi_rows` are the frame's dimensions in 4x4 mode-info units,
/// as the frame header carries them.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks:
/// a partial superblock forces the partition syntax down the block tree, which
/// this writer does not code yet.
pub fn flat_key_frame_tile(mi_cols: u32, mi_rows: u32) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);

    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        for c in 0..sb_cols {
            // decode_partition (spec 5.11.4). Every neighbour is a 64x64 block,
            // whose stored partition context has a zero bit at this block
            // size, so the context is 0 wherever the block sits.
            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // intra_frame_mode_info (spec 5.11.16). Segmentation, delta q,
            // delta lf, palette, filter intra and intrabc are all off in the
            // frame header, and a skipped block codes no CDEF index, so the
            // block is three symbols: the skip flag and the two modes.
            let skip_ctx = usize::from(r > 0) + usize::from(c > 0);
            enc.symbol_fixed(1, &cdf::SKIP[skip_ctx]);
            // Both neighbours are DC-predicted, and an unavailable neighbour
            // counts as DC too, so both mode contexts are 0.
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            // Chroma from luma is only offered up to 32x32, so the CFL-free
            // table is the one a 64x64 block reads.
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            // read_block_tx_size codes nothing while the frame's tx_mode is
            // TX_MODE_LARGEST, and a skipped block has no residual, so the
            // block ends here.
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// 64x64 DC-predicted block carrying one luma DC coefficient of `dc_level` and
/// no chroma residual.
///
/// `dc_level` is a quantised level, not a sample value: the decoder multiplies
/// it by the frame's DC quantiser and inverse-transforms it over the whole
/// block, so the picture it makes is a flat grey some distance either side of
/// the mid-grey a zero level gives. `base_q_idx` is the frame header's, and
/// picks the coefficient CDFs.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], which this is the one-level case of.
pub fn dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    dc_level: i32,
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let blocks = ((mi_cols / SB_MI) * (mi_rows / SB_MI)) as usize;
    dc_key_frame_tile_levels(mi_cols, mi_rows, base_q_idx, &vec![dc_level; blocks])
}

/// Writes the payload of a one-tile key frame carrying one luma DC coefficient
/// per superblock, `levels` giving them in the raster order the superblocks are
/// coded in.
///
/// Each superblock decodes to a flat block of its own grey, so a frame written
/// here is a grid of greys the caller chooses — which is what makes the sign
/// context observable: a block's sign context is read off its coded neighbours,
/// and a frame whose levels differ in sign exercises the three-way split that a
/// single-sign frame cannot reach.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks,
/// when `levels` does not carry exactly one level per superblock, when a level
/// is outside the range the base and base-range syntax carry (`-14..=14`
/// without zero), or when `base_q_idx` is outside the q-context band whose
/// default CDFs this crate carries.
pub fn dc_key_frame_tile_levels(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    if levels.len() != (sb_cols * sb_rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame needs one level per 64x64 superblock",
        ));
    }
    if levels.iter().any(|&l| l == 0 || l.abs() > MAX_LEVEL) {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame is written for levels -14..=14 without zero; \
             wider levels need the Golomb tail",
        ));
    }
    if !Q_CTX_2.contains(&base_q_idx) {
        return Err(Error::unsupported(
            "AV1 tile",
            "the coefficient CDFs of only one q context are known, so \
             base_q_idx must be 61..=120",
        ));
    }

    // The sign of the DC each coded block left behind, for the two neighbours
    // the sign context is read from: one row of them above, and the block to
    // the left, which is dropped at the start of every superblock row the way a
    // decoder clears its left context there.
    let mut above: Vec<Option<bool>> = vec![None; sb_cols as usize];
    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        let mut left: Option<bool> = None;
        for c in 0..sb_cols {
            let dc_level = levels[(r * sb_cols + c) as usize];
            let level = dc_level.abs();
            let negative = dc_level < 0;

            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // Nothing is skipped now, so every neighbour's skip flag is 0 and
            // the skip context stays 0 across the frame.
            enc.symbol_fixed(0, &cdf::SKIP[0]);
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            // coeffs() for the luma plane (spec 5.11.39). The block size is the
            // transform size, so the all-zero flag's context is 0; a 64x64
            // transform codes only its top-left 32x32, which is the 1024-
            // position end-of-block alphabet; an end-of-block of one is token 0
            // with no extra bits; and a 64x64 transform's type set is DCT-only,
            // so no transform type is coded either.
            enc.symbol_fixed(0, &cdf::TXB_SKIP_LUMA_64);
            enc.symbol_fixed(0, &cdf::EOB_PT_1024_LUMA);
            enc.symbol_fixed(
                (level.min(NUM_BASE_LEVELS + 1) - 1) as usize,
                &cdf::COEFF_BASE_EOB_LUMA_64,
            );
            // The base-range tail. Every neighbour of the DC is zero, so its
            // magnitude context is 0 throughout.
            if level > NUM_BASE_LEVELS {
                let mut remaining = level - (NUM_BASE_LEVELS + 1);
                let mut sent = 0;
                while sent < COEFF_BASE_RANGE {
                    let k = remaining.min(BR_STEP);
                    enc.symbol_fixed(k as usize, &cdf::COEFF_BR_LUMA);
                    if k < BR_STEP {
                        break;
                    }
                    remaining -= k;
                    sent += BR_STEP;
                }
            }
            // The signs come after the levels, DC first (spec 5.11.39).
            enc.symbol_fixed(
                usize::from(negative),
                &cdf::DC_SIGN_LUMA[dc_sign_ctx(above[c as usize], left)],
            );

            // Both chroma transform blocks are all-zero. Their planes carry no
            // coded coefficient anywhere in the frame, so the neighbour halves
            // of their context stay 0 and only the offset for a transform block
            // that covers its whole plane block is left: context 7.
            enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_32);
            enc.symbol_fixed(1, &cdf::TXB_SKIP_CHROMA_32);

            above[c as usize] = Some(negative);
            left = Some(negative);
        }
    }
    Ok(enc.finish())
}

/// `Dc_Sign_Contexts` (spec 8.3.2): every 4x4 unit above and left of the block
/// votes the sign of the DC its own block carries — plus one for a positive
/// one, minus one for a negative one, nothing for a unit with no coded
/// coefficient — and the sum picks one of three contexts. Every block here is a
/// whole superblock carrying one DC, so all sixteen units of a neighbour vote
/// together and the sum can only lean up, lean down, or cancel.
fn dc_sign_ctx(above: Option<bool>, left: Option<bool>) -> usize {
    let vote = |n: Option<bool>| match n {
        None => 0i32,
        Some(true) => -1,
        Some(false) => 1,
    };
    match (vote(above) + vote(left)).signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_obu;
    use crate::obu::temporal_delimiter;
    use crate::sequence::sequence_header_obu;
    use ec_av1_syntax::sequence::SequenceHeader;
    use ec_av1_syntax::{
        FrameHeader, FrameType, LoopFilterParams, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
        TxMode,
    };
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// A 64x64 sequence with every tool this writer does not code turned off:
    /// 64x64 superblocks, no CDEF, no loop restoration, no superres, no filter
    /// intra, and no screen content tools (which is what keeps intra block copy
    /// and palette out of the block syntax).
    fn sequence_64() -> SequenceHeader {
        let mut seq = crate::sequence::tests::sample_1080p();
        seq.frame_width_bits = 7;
        seq.frame_height_bits = 7;
        seq.max_frame_width = 64;
        seq.max_frame_height = 64;
        seq.use_128x128_superblock = false;
        seq.enable_filter_intra = false;
        seq.enable_cdef = false;
        seq.enable_restoration = false;
        seq.enable_superres = false;
        seq.seq_force_screen_content_tools = 0;
        seq.seq_force_integer_mv = 0;
        seq
    }

    /// The key frame the tile above belongs to: one tile, quantised so nothing
    /// is lossless, one transform size per block, no in-loop filtering, and no
    /// CDF adaptation (the writer codes against the defaults).
    fn flat_key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            frame_is_intra: true,
            show_frame: true,
            error_resilient_mode: true,
            disable_cdf_update: true,
            allow_screen_content_tools: false,
            force_integer_mv: true,
            refresh_frame_flags: 0xFF,
            primary_ref_frame: PRIMARY_REF_NONE,
            frame_width: 64,
            frame_height: 64,
            upscaled_width: 64,
            render_width: 64,
            render_height: 64,
            mi_cols: 16,
            mi_rows: 16,
            tile_info: TileInfo {
                uniform_spacing: true,
                cols: 1,
                rows: 1,
                cols_log2: 0,
                rows_log2: 0,
                mi_col_starts: vec![0, 16],
                mi_row_starts: vec![0, 16],
                context_update_tile_id: 0,
                tile_size_bytes: 1,
            },
            quantization: QuantizationParams {
                base_q_idx: 100,
                ..QuantizationParams::default()
            },
            loop_filter: LoopFilterParams::default(),
            tx_mode: TxMode::Largest,
            reduced_tx_set: false,
            ..FrameHeader::default()
        }
    }

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the planes.
    fn ffmpeg_decode(stream: &[u8], w: usize, h: usize) -> Vec<u8> {
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
        assert_eq!(
            out.stdout.len(),
            w * h * 3 / 2,
            "expected one 4:2:0 frame, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn flat_key_frame_decodes_to_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP flat_key_frame_decodes_to_mid_grey: no ffmpeg on PATH");
            return;
        }
        let seq = sequence_64();
        let header = flat_key_frame();
        let tile = flat_key_frame_tile(header.mi_cols, header.mi_rows).unwrap();

        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());

        let planes = ffmpeg_decode(&stream, 64, 64);
        // A DC prediction with no neighbour to average is the middle of the
        // range, and a skipped block adds no residual, so every sample of every
        // plane is 128 — one wrong symbol anywhere in the tile shows up here.
        for (i, &s) in planes.iter().enumerate() {
            assert_eq!(s, 128, "sample {i} of the decoded frame");
        }
    }

    /// The one q-context whose CDFs this crate carries; the decoded table
    /// below is pinned at this quantiser and nowhere else.
    const Q_IDX: u8 = 100;

    /// Encode a 64x64 key frame carrying `dc_level` and hand back its planes.
    fn decode_dc_frame(dc_level: i32) -> Vec<u8> {
        let seq = sequence_64();
        let mut header = flat_key_frame();
        header.quantization.base_q_idx = Q_IDX;
        let tile = dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, dc_level).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, 64, 64)
    }

    /// What a reference decoder makes of each DC level at `base_q_idx` 100:
    /// the level times the DC quantiser, inverse-transformed over a whole
    /// 64x64 block — which spreads one coefficient over 4096 samples, so a
    /// level moves the picture by a sample or two, not by tens. These numbers
    /// are pinned from the decoder rather than derived: this crate has no
    /// inverse transform of its own yet to derive them with. What the test
    /// asserts around them — flat planes, untouched chroma, monotone in the
    /// level, and the sign going the right way — is derived, and it is what a
    /// desync in the coefficient syntax breaks first.
    const DECODED_AT_Q100: [(i32, u8); 28] = [
        (1, 128),
        (2, 128),
        (3, 129),
        (4, 129),
        (5, 129),
        (6, 129),
        (7, 129),
        (8, 129),
        (9, 130),
        (10, 130),
        (11, 130),
        (12, 130),
        (13, 130),
        (14, 131),
        (-1, 128),
        (-2, 128),
        (-3, 128),
        (-4, 127),
        (-5, 127),
        (-6, 127),
        (-7, 127),
        (-8, 127),
        (-9, 126),
        (-10, 126),
        (-11, 126),
        (-12, 126),
        (-13, 126),
        (-14, 126),
    ];

    #[test]
    fn a_dc_coefficient_moves_the_whole_block_off_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_dc_coefficient_moves_the_whole_block_off_mid_grey: no ffmpeg");
            return;
        }
        let mut seen = Vec::new();
        for (level, want) in DECODED_AT_Q100 {
            let planes = decode_dc_frame(level);
            let (luma, chroma) = planes.split_at(64 * 64);
            for (i, &sample) in luma.iter().enumerate() {
                assert_eq!(sample, want, "luma sample {i} at dc level {level}");
            }
            // The chroma transform blocks are all-zero, so both planes stay at
            // the prediction. A desync in the luma coefficient syntax lands
            // here first: the chroma flags are the next symbols after it.
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} at dc level {level}");
            }
            seen.push((level, want));
        }
        // The picture moves off mid-grey the way the level says: up for a
        // positive level, down for a negative one, and never back towards it
        // as the level grows.
        for w in seen.windows(2) {
            let ((prev_level, prev), (level, value)) = (w[0], w[1]);
            if prev_level.signum() != level.signum() {
                continue;
            }
            if level > 0 {
                assert!(
                    value >= prev,
                    "level {level} decoded below level {prev_level}"
                );
                assert!(value >= 128, "a positive level darkened the picture");
            } else {
                assert!(
                    value <= prev,
                    "level {level} decoded above level {prev_level}"
                );
                assert!(value <= 128, "a negative level brightened the picture");
            }
        }
        assert!(
            DECODED_AT_Q100.iter().any(|&(_, v)| v > 128)
                && DECODED_AT_Q100.iter().any(|&(_, v)| v < 128),
            "the pinned table has to move the picture both ways"
        );
    }

    /// Encode a grid of 64x64 superblocks, each carrying its own DC level, and
    /// hand back the decoded planes.
    fn decode_level_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
        let (w, h) = (64 * sb_cols, 64 * sb_rows);
        let mut seq = sequence_64();
        seq.max_frame_width = w;
        seq.max_frame_height = h;
        let mut header = flat_key_frame();
        header.frame_width = w;
        header.frame_height = h;
        header.upscaled_width = w;
        header.render_width = w;
        header.render_height = h;
        header.mi_cols = sb_cols * SB_MI;
        header.mi_rows = sb_rows * SB_MI;
        header.tile_info.mi_col_starts = vec![0, header.mi_cols];
        header.tile_info.mi_row_starts = vec![0, header.mi_rows];
        header.quantization.base_q_idx = Q_IDX;
        let tile = dc_key_frame_tile_levels(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, w as usize, h as usize)
    }

    fn decoded_value(level: i32) -> u8 {
        DECODED_AT_Q100
            .iter()
            .find(|&&(l, _)| l == level)
            .map(|&(_, v)| v)
            .expect("the level is in the pinned table")
    }

    /// `dc_predict` (spec 7.11.2) for the flat case: a block whose neighbours
    /// are themselves flat predicts their average, and predicts mid-grey with
    /// no neighbour at all. Every block here is a whole 64x64 superblock, so
    /// the above row and the left column weigh the same.
    fn dc_prediction(above: Option<u8>, left: Option<u8>) -> u8 {
        match (above, left) {
            (None, None) => 128,
            (Some(a), None) => a,
            (None, Some(l)) => l,
            (Some(a), Some(l)) => ((u32::from(a) * 64 + u32::from(l) * 64 + 64) >> 7) as u8,
        }
    }

    /// What a DC level adds to the prediction: the pinned table is that sum
    /// against a mid-grey prediction, and the residual does not depend on what
    /// it is added to.
    fn dc_residual(level: i32) -> i32 {
        i32::from(decoded_value(level)) - 128
    }

    /// Every block reads its DC sign context off the coded blocks above and to
    /// its left, and the three ways that can land — no coded neighbour, the
    /// neighbours leaning one way, the neighbours cancelling — are only
    /// reachable in a frame whose levels differ in sign. Getting the context
    /// wrong desyncs the arithmetic decoder, so the check is that every block
    /// still decodes to the grey its own level asks for.
    #[test]
    fn each_superblock_decodes_to_the_grey_its_own_level_asks_for() {
        if !have_ffmpeg() {
            eprintln!("SKIP each_superblock_decodes_to_the_grey_its_own_level_asks_for: no ffmpeg");
            return;
        }
        // Read in raster order the two grids put a positive, a negative and a
        // cancelling sign context in front of the bottom-right block, and a
        // leaning-down one in front of the second grid's right and bottom
        // blocks.
        for levels in [[14, 3, -3, -14], [-14, -3, 3, 14]] {
            let planes = decode_level_grid(&levels, 2, 2);
            let (luma, chroma) = planes.split_at(128 * 128);
            // The blocks are not independent: a DC prediction reads the
            // reconstructed neighbours, so each block's grey is its
            // neighbours' average plus its own residual.
            let mut recon = [0u8; 4];
            for (block, level) in levels.iter().enumerate() {
                let (br, bc) = (block / 2, block % 2);
                let above = (br > 0).then(|| recon[block - 2]);
                let left = (bc > 0).then(|| recon[block - 1]);
                let want = (i32::from(dc_prediction(above, left)) + dc_residual(*level))
                    .clamp(0, 255) as u8;
                recon[block] = want;
                for y in 0..64 {
                    for x in 0..64 {
                        let i = (br * 64 + y) * 128 + bc * 64 + x;
                        assert_eq!(
                            luma[i], want,
                            "luma at ({x}, {y}) of the block carrying level {level} in {levels:?}"
                        );
                    }
                }
            }
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} of {levels:?}");
            }
        }
    }

    #[test]
    fn a_level_grid_that_does_not_cover_the_frame_is_refused() {
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 3, 3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 0, 3]).is_err());
    }

    #[test]
    fn dc_levels_the_base_syntax_cannot_carry_are_refused() {
        assert!(dc_key_frame_tile(16, 16, 100, 0).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, 15).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, -15).is_err());
        assert!(dc_key_frame_tile(16, 16, 40, 3).is_err());
    }

    #[test]
    fn partial_superblocks_are_refused() {
        assert!(flat_key_frame_tile(16, 20).is_err());
        assert!(flat_key_frame_tile(0, 16).is_err());
    }
}
