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
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(SB_MI)
        || !mi_rows.is_multiple_of(SB_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a flat key frame is written only for frames that are a whole \
             number of 64x64 superblocks",
        ));
    }
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

    #[test]
    fn partial_superblocks_are_refused() {
        assert!(flat_key_frame_tile(16, 20).is_err());
        assert!(flat_key_frame_tile(0, 16).is_err());
    }
}
