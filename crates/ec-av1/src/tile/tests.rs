use super::*;
use crate::frame::frame_obu;
use crate::obu::temporal_delimiter;
use crate::probe::run_with_stdin;
use crate::sequence::sequence_header_obu;
use ec_av1_syntax::sequence::SequenceHeader;
use ec_av1_syntax::{
    FrameHeader, FrameType, LoopFilterParams, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
    TxMode,
};
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

/// Whether ffmpeg is on PATH. Absence normally SKIPs, but
/// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
/// aomenc gate decodes its stream through ffmpeg and is meaningless
/// without it -- turns it into a hard failure. Without this the require
/// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
/// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
/// printed SKIP and reported green (class gate-skips-on-its-own-failure).
fn have_ffmpeg() -> bool {
    let present = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        present
            || (std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_none()
                && std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none()),
        "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
    );
    present
}

/// Decodes an AV1 OBU stream with ffmpeg and hands back the planes.
fn ffmpeg_decode(stream: &[u8], w: usize, h: usize) -> Vec<u8> {
    let out = run_with_stdin(
        Command::new("ffmpeg").args([
            "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
        ]),
        stream,
    );
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
    let (seq, mut header) = frame_of(w, h);
    // The DC fixture writers code against the default CDFs and never
    // update them, so their frames say so.
    header.disable_cdf_update = true;
    let tile = dc_key_frame_tile_levels(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    ffmpeg_decode(&stream, w as usize, h as usize)
}

/// A sequence and key frame header for a `w` by `h` frame of whole
/// superblocks.
fn frame_of(w: u32, h: u32) -> (SequenceHeader, FrameHeader) {
    let mut seq = sequence_64();
    seq.max_frame_width = w;
    seq.max_frame_height = h;
    let mut header = flat_key_frame();
    // Every tile these headers carry is written by the adapting writer.
    header.disable_cdf_update = false;
    header.frame_width = w;
    header.frame_height = h;
    header.upscaled_width = w;
    header.render_width = w;
    header.render_height = h;
    header.mi_cols = w / 4;
    header.mi_rows = h / 4;
    header.tile_info.mi_col_starts = vec![0, header.mi_cols];
    header.tile_info.mi_row_starts = vec![0, header.mi_rows];
    header.quantization.base_q_idx = Q_IDX;
    (seq, header)
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

/// What a DC level adds to a 32x32 block's prediction at `base_q_idx` 100,
/// pinned from the decoder the way [`DECODED_AT_Q100`] is. A 32x32
/// transform spreads its DC over a quarter of the samples a 64x64 one
/// does, so the same level moves the picture further: level 14 is five
/// sample values here against three there.
const SPLIT_RESIDUAL_AT_Q100: [(i32, i32); 28] = [
    (1, 0),
    (2, 1),
    (3, 1),
    (4, 1),
    (5, 2),
    (6, 2),
    (7, 3),
    (8, 3),
    (9, 3),
    (10, 4),
    (11, 4),
    (12, 4),
    (13, 5),
    (14, 5),
    (-1, 0),
    (-2, -1),
    (-3, -1),
    (-4, -1),
    (-5, -2),
    (-6, -2),
    (-7, -2),
    (-8, -3),
    (-9, -3),
    (-10, -4),
    (-11, -4),
    (-12, -4),
    (-13, -5),
    (-14, -5),
];

fn split_residual(level: i32) -> i32 {
    SPLIT_RESIDUAL_AT_Q100
        .iter()
        .find(|&&(l, _)| l == level)
        .map(|&(_, r)| r)
        .expect("the level is in the pinned table")
}

/// Splitting a superblock puts three more syntax elements in front of every
/// block — the split partition itself, the 32x32 partition below it and a
/// chroma mode from the wider table CFL-capable blocks read — and moves the
/// coefficients onto the 32x32 CDFs. Getting any of them wrong desyncs the
/// arithmetic decoder, and the check is that all sixteen blocks still land
/// on the grey their own level and their neighbours ask for.
#[test]
fn a_split_superblock_decodes_each_quadrant_on_its_own_level() {
    if !have_ffmpeg() {
        eprintln!("SKIP a_split_superblock_decodes_each_quadrant_on_its_own_level: no ffmpeg");
        return;
    }
    // Four by four 32x32 blocks over two by two superblocks, so the walk
    // crosses superblock boundaries in both directions, and signs that
    // alternate so the DC sign context lands on all three of its values.
    let levels: [i32; 16] = [14, -3, 5, -5, -7, 7, -14, 3, 2, -2, 9, -9, -11, 11, -1, 1];
    let planes = decode_split_grid(&levels, 2, 2);
    let (luma, chroma) = planes.split_at(128 * 128);

    let mut recon = [0i32; 16];
    for (block, level) in levels.iter().enumerate() {
        let (br, bc) = (block / 4, block % 4);
        let above = (br > 0).then(|| recon[block - 4] as u8);
        let left = (bc > 0).then(|| recon[block - 1] as u8);
        let want =
            (i32::from(dc_prediction(above, left)) + split_residual(*level)).clamp(0, 255);
        recon[block] = want;
        for y in 0..32 {
            for x in 0..32 {
                let i = (br * 32 + y) * 128 + bc * 32 + x;
                assert_eq!(
                    i32::from(luma[i]),
                    want,
                    "luma at ({x}, {y}) of the block carrying level {level} at row {br}, \
                     column {bc}"
                );
            }
        }
    }
    for (i, &sample) in chroma.iter().enumerate() {
        assert_eq!(sample, 128, "chroma sample {i}");
    }
}

/// Encode a grid of 32x32 blocks, each carrying its own DC level.
fn decode_split_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
    let (w, h) = (64 * sb_cols, 64 * sb_rows);
    let (seq, mut header) = frame_of(w, h);
    // The DC fixture writers code against the default CDFs and never
    // update them, so their frames say so.
    header.disable_cdf_update = true;
    let tile = split_dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    ffmpeg_decode(&stream, w as usize, h as usize)
}

#[test]
fn a_level_grid_that_does_not_cover_the_frame_is_refused() {
    assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3]).is_err());
    assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3, 0]).is_err());
    assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3]).is_err());
    assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 3, 3, 3]).is_err());
    assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 0, 3]).is_err());
}

#[test]
fn dc_levels_the_base_syntax_cannot_carry_are_refused() {
    assert!(dc_key_frame_tile(16, 16, 100, 0).is_err());
    assert!(dc_key_frame_tile(16, 16, 100, 15).is_err());
    assert!(dc_key_frame_tile(16, 16, 100, -15).is_err());
}

#[test]
fn partial_superblocks_are_refused() {
    assert!(flat_key_frame_tile(16, 20).is_err());
    assert!(flat_key_frame_tile(0, 16).is_err());
}

/// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
/// hands back the luma plane as rows of samples.
fn decode_coeff_quadrants(blocks: &[Vec<Coeff>]) -> Vec<Vec<u8>> {
    let blocks: Vec<BlockCoeffs> = blocks.iter().cloned().map(BlockCoeffs::from).collect();
    let [luma, u, v] = decode_coeff_planes(&blocks);
    for (name, plane) in [("U", &u), ("V", &v)] {
        for (y, row) in plane.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(
                    s, 128,
                    "{name} sample ({x},{y}): no block codes a chroma coefficient, and a \
                     DC prediction with no coded neighbour is mid-grey"
                );
            }
        }
    }
    luma
}

/// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
/// hands back its three planes as rows of samples: 64 rows of luma and, at
/// 4:2:0, 32 rows of each chroma plane.
fn decode_coeff_planes(blocks: &[BlockCoeffs]) -> [Vec<Vec<u8>>; 3] {
    let (seq, header) = frame_of(64, 64);
    let tile =
        split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    let planes = ffmpeg_decode(&stream, 64, 64);
    let (luma, chroma) = planes.split_at(64 * 64);
    let (u, v) = chroma.split_at(32 * 32);
    [
        luma.as_chunks::<64>().0.iter().map(|c| c.to_vec()).collect(),
        u.as_chunks::<32>().0.iter().map(|c| c.to_vec()).collect(),
        v.as_chunks::<32>().0.iter().map(|c| c.to_vec()).collect(),
    ]
}

/// The top-left 32x32 block of a decoded frame, which is the only one whose
/// prediction has no neighbour to lean on.
fn top_left_block(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
    rows[..32].iter().map(|r| r[..32].to_vec()).collect()
}

/// A transform's basis functions all average to zero except the DC's, so a
/// block carrying no DC decodes to samples that average to its prediction —
/// mid-grey for the block with no neighbours. Rounding of the inverse
/// transform moves the average by less than a sample.
fn assert_mean_is_mid_grey(block: &[Vec<u8>]) {
    let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
    let mean = f64::from(sum) / (32.0 * 32.0);
    assert!(
        (mean - 128.0).abs() <= 1.0,
        "a block with no DC coefficient averages to its mid-grey prediction, got {mean}"
    );
}

/// A coefficient at row 0, column 1 selects the basis function that is flat
/// down the block and a half-cycle of a cosine across it. Nothing about
/// that shape is pinned here: every row of the block must be the same row,
/// the samples must fall from left to right and actually move, and the
/// block must still average to its prediction. A desync in the scan, the
/// end-of-block position or the level contexts breaks one of those.
#[test]
fn a_coefficient_off_the_origin_selects_its_own_basis_function() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP a_coefficient_off_the_origin_selects_its_own_basis_function: no ffmpeg"
        );
        return;
    }
    let blocks = vec![
        vec![Coeff {
            row: 0,
            col: 1,
            level: 12,
        }],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ];
    let rows = decode_coeff_quadrants(&blocks);
    let block = top_left_block(&rows);

    for (y, row) in block.iter().enumerate() {
        assert_eq!(row, &block[0], "row {y} of a horizontal basis function");
    }
    assert!(
        block[0][0] > block[0][31],
        "a positive coefficient leans the block towards its left edge: {:?}",
        block[0]
    );
    for x in 1..32 {
        assert!(
            block[0][x] <= block[0][x - 1],
            "the half-cycle falls from left to right, broke at column {x}: {:?}",
            block[0]
        );
    }
    assert_mean_is_mid_grey(&block);

    // The same coefficient transposed selects the basis function that is
    // flat across the block and a half-cycle down it.
    let blocks = vec![
        vec![Coeff {
            row: 1,
            col: 0,
            level: 12,
        }],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ];
    let rows = decode_coeff_quadrants(&blocks);
    let block = top_left_block(&rows);
    for (y, row) in block.iter().enumerate() {
        for (x, &s) in row.iter().enumerate() {
            assert_eq!(s, row[0], "sample ({y},{x}) of a vertical basis function");
        }
    }
    assert!(
        block[0][0] > block[31][0],
        "a positive coefficient leans the block up"
    );
    for y in 1..32 {
        assert!(
            block[y][0] <= block[y - 1][0],
            "the half-cycle falls from top to bottom, broke at row {y}"
        );
    }
    assert_mean_is_mid_grey(&block);
}

/// A coefficient late in the scan pushes the end-of-block position into a
/// group that carries an offset — the far corner of a 32x32 transform is
/// the last of the eleven groups, whose offset is nine bits wide, one of
/// them from a CDF and the rest raw. Every coefficient before it is coded
/// too, so this also walks the base and base-range contexts over a block
/// with neighbours that carry levels rather than zeros.
#[test]
fn a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group() {
    if !have_ffmpeg() {
        eprintln!("SKIP a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group");
        return;
    }
    let block = vec![
        Coeff {
            row: 31,
            col: 31,
            level: 5,
        },
        Coeff {
            row: 5,
            col: 7,
            level: -9,
        },
        Coeff {
            row: 1,
            col: 1,
            level: 14,
        },
        Coeff {
            row: 0,
            col: 2,
            level: -3,
        },
        Coeff {
            row: 2,
            col: 0,
            level: 2,
        },
        Coeff {
            row: 1,
            col: 2,
            level: -1,
        },
    ];
    let rows = decode_coeff_quadrants(&[block, Vec::new(), Vec::new(), Vec::new()]);
    let block = top_left_block(&rows);
    assert_mean_is_mid_grey(&block);
    assert!(
        block.iter().flatten().any(|&s| s != block[0][0]),
        "six coefficients do not decode to a flat block"
    );
    // The blocks that carry nothing are flat: their prediction is whatever
    // their neighbours left, and they add no residual to it.
    for (name, ys, xs) in [("top right", 0..32, 32..64), ("bottom left", 32..64, 0..32)] {
        let first = rows[ys.start][xs.start];
        for y in ys {
            for x in xs.clone() {
                assert_eq!(rows[y][x], first, "the {name} block carries no coefficient");
            }
        }
    }
}

/// The price the mode search pays for a block's levels has to be the price
/// the writer charges for them, or the search is ranking modes by a
/// fiction. Both halves are asked for the same grids under the same
/// neutral contexts, and the priced bits are compared against the bytes
/// the writer really spends.
#[test]
fn the_priced_coefficient_bits_match_the_bytes_written() {
    let scan = default_scan(TX32);
    // Three grids that reach different parts of the syntax: a lone DC, a
    // sparse spread of small levels with both signs, and levels past the
    // base-range alphabet so the Golomb tail runs.
    let mut grids = vec![vec![0i32; TX32 * TX32]; 3];
    grids[0][0] = -7;
    for (i, &pos) in scan.iter().enumerate().take(400) {
        if i % 7 == 0 {
            grids[1][pos as usize] = if i % 2 == 0 { 2 } else { -1 };
        }
    }
    grids[2][0] = 40;
    for (i, &pos) in scan.iter().enumerate().take(64) {
        grids[2][pos as usize] += i as i32 % 23 - 11;
    }

    // A single block is too short for the coder's own flush to average
    // out, so all three go through one encoder and one flush.
    let mut enc = SymbolEncoder::new();
    let mut priced = 0.0;
    for grid in &grids {
        write_coeffs(
            &mut enc,
            &mut Cdfs::new(2).txb(TxbSet::Luma32, 0),
            grid,
            &scan,
            0,
            0,
            None,
            TxType::DctDct,
        );
        priced += luma_32_coeff_bits(grid);
    }
    let spent = enc.finish().len() as f64 * 8.0;
    assert!(
        (priced - spent).abs() / spent < 0.02,
        "priced {priced} bits, wrote {spent}"
    );
}

/// A rate term the search can read as constant is a rate term that ranks
/// nothing, so the price is checked to move the way the levels move: an
/// empty block is the cheapest thing there is, one coefficient costs more
/// than none, and both spreading the coefficients and growing them costs
/// more again.
#[test]
fn the_price_grows_with_the_levels() {
    let grid = |f: &dyn Fn(&mut Vec<i32>)| {
        let mut g = vec![0i32; TX32 * TX32];
        f(&mut g);
        luma_32_coeff_bits(&g)
    };
    let empty = grid(&|_| {});
    let one = grid(&|g| g[0] = 1);
    let bigger = grid(&|g| g[0] = 30);
    let spread = grid(&|g| {
        for i in 0..TX32 {
            g[i * TX32 + i] = 1;
        }
    });
    assert!(empty < one, "{empty} {one}");
    assert!(one < bigger, "{one} {bigger}");
    assert!(one < spread, "{one} {spread}");
    // An empty block is one symbol -- an expensive one, because a block
    // with no coefficients at all is the rarer of the two under the
    // neutral context -- and nothing else.
    assert!(
        empty < 6.0,
        "an empty block is one skip symbol, not {empty}"
    );
}

/// Neighbouring blocks that each carry coefficients read their DC sign
/// context, their prediction and their own contexts off each other, so a
/// frame whose four blocks all carry mixed-sign coefficients exercises what
/// a single coded block cannot. The check is that it decodes at all — a
/// desync anywhere fails the decode or the frame size — and that the block
/// with no neighbours still averages to mid-grey.
#[test]
fn every_quadrant_carries_its_own_coefficients() {
    if !have_ffmpeg() {
        eprintln!("SKIP every_quadrant_carries_its_own_coefficients: no ffmpeg");
        return;
    }
    let blocks = vec![
        vec![
            Coeff {
                row: 0,
                col: 1,
                level: 7,
            },
            Coeff {
                row: 3,
                col: 4,
                level: -14,
            },
        ],
        vec![
            Coeff {
                row: 0,
                col: 0,
                level: -6,
            },
            Coeff {
                row: 2,
                col: 2,
                level: 9,
            },
        ],
        vec![
            Coeff {
                row: 0,
                col: 0,
                level: 6,
            },
            Coeff {
                row: 1,
                col: 0,
                level: -2,
            },
            Coeff {
                row: 8,
                col: 9,
                level: 3,
            },
        ],
        vec![
            Coeff {
                row: 0,
                col: 0,
                level: 11,
            },
            Coeff {
                row: 16,
                col: 1,
                level: -4,
            },
        ],
    ];
    let rows = decode_coeff_quadrants(&blocks);
    assert_mean_is_mid_grey(&top_left_block(&rows));
    for (name, ys, xs) in [
        ("top right", 0..32, 32..64),
        ("bottom left", 32..64, 0..32),
        ("bottom right", 32..64, 32..64),
    ] {
        let block: Vec<Vec<u8>> = rows[ys].iter().map(|r| r[xs.clone()].to_vec()).collect();
        assert!(
            block.iter().flatten().any(|&s| s != block[0][0]),
            "the {name} block carries coefficients, so it is not flat"
        );
    }
}

/// The top-left 16x16 block of a decoded chroma plane, which is the only
/// one whose prediction has no neighbour to lean on.
fn top_left_chroma(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
    rows[..16].iter().map(|r| r[..16].to_vec()).collect()
}

/// The mean of a decoded block, which every basis function but the DC's
/// leaves at the block's prediction.
fn mean(block: &[Vec<u8>]) -> f64 {
    let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
    let count = block.iter().map(Vec::len).sum::<usize>() as f64;
    f64::from(sum) / count
}

/// A chroma plane's coefficients ride their own transform, their own CDFs
/// and their own end-of-block alphabet, so a chroma basis function is the
/// gate on all three: U carries the coefficient that is flat down the block
/// and a half-cycle across it, V the one turned a quarter turn, and neither
/// may reach the other plane or luma.
#[test]
fn each_chroma_plane_codes_its_own_basis_function() {
    if !have_ffmpeg() {
        eprintln!("SKIP each_chroma_plane_codes_its_own_basis_function: no ffmpeg");
        return;
    }
    let mut blocks = vec![BlockCoeffs::default(); 4];
    blocks[0].u = vec![Coeff {
        row: 0,
        col: 1,
        level: 12,
    }];
    blocks[0].v = vec![Coeff {
        row: 1,
        col: 0,
        level: 12,
    }];
    let [luma, u, v] = decode_coeff_planes(&blocks);

    for (y, row) in luma.iter().enumerate() {
        for (x, &s) in row.iter().enumerate() {
            assert_eq!(
                s, 128,
                "luma sample ({x},{y}): no block codes a luma coefficient, so the picture \
                 stays at its mid-grey prediction"
            );
        }
    }

    let u_block = top_left_chroma(&u);
    for (y, row) in u_block.iter().enumerate() {
        assert_eq!(
            row, &u_block[0],
            "row {y} of a horizontal chroma basis function"
        );
    }
    assert!(
        u_block[0][0] > u_block[0][15],
        "a positive coefficient leans the U block towards its left edge: {:?}",
        u_block[0]
    );

    let v_block = top_left_chroma(&v);
    for (x, &top) in v_block[0].iter().enumerate() {
        assert_eq!(
            top, v_block[0][0],
            "column {x} of a vertical chroma basis function"
        );
        let column: Vec<u8> = v_block.iter().map(|r| r[x]).collect();
        assert!(
            column[0] > column[15],
            "a positive coefficient leans the V column towards its top edge: {column:?}"
        );
    }

    for (name, block) in [("U", &u_block), ("V", &v_block)] {
        let mean = mean(block);
        assert!(
            (mean - 128.0).abs() <= 1.0,
            "the {name} block carries no DC, so it averages to its mid-grey prediction, \
             got {mean}"
        );
    }
}

/// Every block coding chroma puts the chroma all-zero flag on the contexts
/// a coded neighbour above and to the left select, and the DC sign on the
/// contexts their signs select. A frame whose four blocks all carry chroma
/// DCs of mixed sign walks those contexts; the picture that comes back must
/// still lean the way each block's own DC asks.
#[test]
fn chroma_dc_signs_read_their_neighbours() {
    if !have_ffmpeg() {
        eprintln!("SKIP chroma_dc_signs_read_their_neighbours: no ffmpeg");
        return;
    }
    let dc = |level| {
        vec![Coeff {
            row: 0,
            col: 0,
            level,
        }]
    };
    let signs = [8, -8, -8, 8];
    let blocks: Vec<BlockCoeffs> = signs
        .iter()
        .map(|&level| BlockCoeffs {
            luma: Vec::new(),
            u: dc(level),
            v: dc(-level),
            ..BlockCoeffs::default()
        })
        .collect();
    let [luma, u, v] = decode_coeff_planes(&blocks);

    for (y, row) in luma.iter().enumerate() {
        for (x, &s) in row.iter().enumerate() {
            assert_eq!(s, 128, "luma sample ({x},{y}) with no luma coefficient");
        }
    }

    // A DC prediction leans on the neighbours a block has, so each block is
    // measured against the prediction it was given rather than mid-grey:
    // the only block with no neighbour is the first, and the rest are read
    // as a step away from the block above them and to their left.
    for (i, &level) in signs.iter().enumerate() {
        let (r, c) = (i / 2, i % 2);
        let block = |plane: &[Vec<u8>]| -> Vec<Vec<u8>> {
            plane[r * 16..r * 16 + 16]
                .iter()
                .map(|row| row[c * 16..c * 16 + 16].to_vec())
                .collect()
        };
        for (name, plane, want) in [("U", &u, level), ("V", &v, -level)] {
            let block = block(plane);
            let mean = mean(&block);
            for (y, row) in block.iter().enumerate() {
                assert_eq!(
                    row.iter().collect::<std::collections::HashSet<_>>().len(),
                    1,
                    "row {y} of a DC-only {name} block is flat"
                );
            }
            if i == 0 {
                assert!(
                    (want > 0) == (mean > 128.0),
                    "{name} block {i} carries a DC of {want}, so it must lean off its \
                     mid-grey prediction that way, got {mean}"
                );
            }
        }
    }
    assert_ne!(u, v, "the two chroma planes carry opposite DCs");
}

/// The coefficient writer refuses what it cannot code rather than writing a
/// stream a decoder walks off the end of.
/// The 32x32 block at the given block row and column of a decoded plane.
fn block_at(rows: &[Vec<u8>], block_row: usize, block_col: usize) -> Vec<Vec<u8>> {
    rows[block_row * 32..block_row * 32 + 32]
        .iter()
        .map(|r| r[block_col * 32..block_col * 32 + 32].to_vec())
        .collect()
}

/// Encodes a key frame of the given size, whose 32x32 blocks carry
/// `blocks` in raster order, and hands back its luma plane as rows of
/// The neighbour context a block reads is gathered across every cell it
/// spans, not taken from the first one: a 32x32 block whose left-hand
/// neighbour cells disagree still sees the coded one. Reading a single
/// cell agreed with the decoder only while every block was the same size,
/// and desynchronised the arithmetic decoder the moment a 16x16 one
/// appeared beside a 32x32 one.
#[test]
fn a_block_gathers_the_cells_its_neighbours_cover() {
    let mut neighbours = Neighbours::new(4, 4, 64, 64);
    let quiet = [
        vec![0i32; TX16 * TX16],
        vec![0; TX8 * TX8],
        vec![0; TX8 * TX8],
    ];
    let mut loud = quiet.clone();
    loud[1][0] = -3;
    // Two 16x16 blocks above the right-hand 32x32 block of the row, of
    // which only the second codes anything.
    neighbours.record((0, 2), SUB, DC_PRED, &quiet);
    neighbours.record((0, 3), SUB, DC_PRED, &loud);
    let around = neighbours.around((2, 2), BLOCK);
    assert!(
        around[1].above_coded,
        "the coded cell has to reach the block below it"
    );
    assert_eq!(
        dc_sign_ctx(around[1].dc_vote),
        1,
        "the one negative DC above the block is what it votes"
    );
    // The same read one cell at a time misses it, which is the bug this
    // gate is here for. `above[8]` is the first 4x4 unit the quiet block
    // at `(0, 2)` wrote (2 SUB units * 4 4x4-units-per-SUB).
    assert!(!neighbours.above[8][1].coded);
}

/// samples.
fn decode_luma_at(width: usize, height: usize, blocks: &[BlockCoeffs]) -> Vec<Vec<u8>> {
    let (seq, header) = frame_of(width as u32, height as u32);
    let tile =
        split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    let planes = ffmpeg_decode(&stream, width, height);
    planes[..width * height]
        .chunks_exact(width)
        .map(<[u8]>::to_vec)
        .collect()
}

/// Every row of a block is the same row, and that row falls from left to
/// right — the shape of a coefficient at row 0, column 1.
fn assert_falls_across(block: &[Vec<u8>], name: &str) {
    for row in block {
        assert_eq!(
            row, &block[0],
            "{name}: every row of the block is the same row"
        );
    }
    assert!(
        block[0][0] > block[0][31],
        "{name}: the row falls from left to right, got {} then {}",
        block[0][0],
        block[0][31]
    );
}

/// Every column of a block is constant across it and falls down it — the
/// shape of a coefficient at row 1, column 0.
fn assert_falls_down(block: &[Vec<u8>], name: &str) {
    for (y, row) in block.iter().enumerate() {
        assert!(
            row.iter().all(|&s| s == row[0]),
            "{name}: row {y} of the block is one constant"
        );
    }
    assert!(
        block[0][0] > block[31][0],
        "{name}: the column falls down the block, got {} then {}",
        block[0][0],
        block[31][0]
    );
}

/// The mean sample of a decoded block.
fn mean_of(block: &[Vec<u8>]) -> f64 {
    let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
    f64::from(sum) / (block.len() * block[0].len()) as f64
}

/// The top-left block of a frame whose only coefficient is a luma DC of
/// `level`.
fn decode_dc_level_block(level: i32) -> Vec<Vec<u8>> {
    let blocks = vec![
        vec![Coeff {
            row: 0,
            col: 0,
            level,
        }],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ];
    top_left_block(&decode_coeff_quadrants(&blocks))
}

/// `read_golomb` (spec 5.11.40), written from the specification's
/// pseudocode, and the level its caller builds from it: a level above the
/// base-range tail is fifteen plus what the tail carries.
fn read_golomb_level(dec: &mut crate::msac::tests::SymbolDecoder) -> u32 {
    let mut length = 0;
    while dec.literal(1) == 0 {
        length += 1;
        assert!(
            length < 20,
            "the tail's length prefix runs past twenty bits"
        );
    }
    let mut x = 1;
    for _ in 0..length {
        x = (x << 1) | dec.literal(1);
    }
    (MAX_BR_LEVEL + 1) as u32 + (x - 1)
}

/// The tail says how much of the level the base and base-range syntax could
/// not carry, and the spec's own reader must hand back the level that was
/// asked for — not one either side of it. A tail written a bit long, a bit
/// short, or against the wrong base fails this at the first level it
/// reaches.
#[test]
fn the_golomb_tail_reads_back_as_the_level_it_was_written_for() {
    for level in (MAX_BR_LEVEL + 1..=MAX_BR_LEVEL + 300)
        .chain([MAX_LEVEL - 1, MAX_LEVEL])
        .map(|l| l as u32)
    {
        let mut enc = SymbolEncoder::new();
        write_golomb(&mut enc, level - (MAX_BR_LEVEL + 1) as u32);
        // The tail is raw bits, so it needs no padding beyond what the
        // encoder's own flush writes.
        let data = enc.finish();
        let mut dec = crate::msac::tests::SymbolDecoder::new(&data);
        assert_eq!(
            read_golomb_level(&mut dec),
            level,
            "the tail of level {level}"
        );
    }
}

/// The base and base-range syntax reach level fourteen between them;
/// anything above that carries the rest of itself as a Golomb tail, written
/// after its own sign. A DC level moves the whole block off its mid-grey
/// prediction by an amount that grows with the level, so a run of levels
/// either side of the tail's threshold — spaced wide enough that the
/// quantiser separates them — must decode to a run of steadily
/// brighter blocks — and the negatives of the same levels to steadily
/// darker ones. A tail written with the wrong length, in the wrong place in
/// the syntax, or off by one desyncs the decoder outright.
#[test]
fn levels_above_the_base_range_tail_carry_a_golomb_tail() {
    if !have_ffmpeg() {
        eprintln!("SKIP levels_above_the_base_range_tail_carry_a_golomb_tail: no ffmpeg");
        return;
    }
    let mut brighter = Vec::new();
    let mut darker = Vec::new();
    for level in [10, 14, 18, 31, 60, 400] {
        brighter.push((level, mean_of(&decode_dc_level_block(level))));
        darker.push((level, mean_of(&decode_dc_level_block(-level))));
    }
    for pair in brighter.windows(2) {
        let [(low, dim), (high, bright)] = [pair[0], pair[1]];
        assert!(
            bright > dim,
            "level {high} decodes brighter than level {low}, got {bright} against {dim}"
        );
    }
    for pair in darker.windows(2) {
        let [(low, bright), (high, dim)] = [pair[0], pair[1]];
        assert!(
            dim < bright,
            "level -{high} decodes darker than level -{low}, got {dim} against {bright}"
        );
    }
    let (_, mid) = brighter[0];
    assert!(
        mid > 128.0 && darker[0].1 < 128.0,
        "a positive level brightens its block and a negative one darkens it, got {mid} and {}",
        darker[0].1
    );
}

/// A frame need not be a whole number of superblocks. A 96x64 frame is a
/// superblock and a half across: the second superblock has no right-hand
/// half, so its partition is a gathered flag rather than the full symbol,
/// and the two blocks that would sit in that half are never coded at all.
/// Each of the six blocks that do exist carries one of two basis functions,
/// alternating, so a block written into the wrong place or a flag the
/// decoder reads as a different number of bits shows up as a block with the
/// other block's shape — or as a decode failure.
fn decode_luma_sb(width: usize, height: usize, sbs: &[Superblock]) -> Vec<Vec<u8>> {
    let (seq, header) = frame_of(width as u32, height as u32);
    let tile = sb_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, sbs).unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    let planes = ffmpeg_decode(&stream, width, height);
    planes[..width * height]
        .chunks_exact(width)
        .map(<[u8]>::to_vec)
        .collect()
}

/// A superblock left whole carries one 64x64 transform, whose lowest
/// horizontal basis function falls across the whole sixty-four samples in
/// one stroke. Were the same superblock split, each half would restart the
/// gradient at its own left edge and the picture would rise again in the
/// middle, so the walk across the row is what tells the two apart — and
/// the split superblock beside it proves the two partitions still agree on
/// where the next one begins.
#[test]
fn a_whole_superblock_covers_all_four_of_its_quadrants() {
    if !have_ffmpeg() {
        eprintln!("SKIP a_whole_superblock_covers_all_four_of_its_quadrants: no ffmpeg");
        return;
    }
    let across = BlockCoeffs::from(vec![Coeff {
        row: 0,
        col: 1,
        level: 20,
    }]);
    let down = BlockCoeffs::from(vec![Coeff {
        row: 1,
        col: 0,
        level: 20,
    }]);
    let sbs = [
        Superblock::Whole(across),
        Superblock::Split(vec![
            Quadrant::Whole(down.clone()),
            Quadrant::Whole(down.clone()),
            Quadrant::Whole(down.clone()),
            Quadrant::Whole(down),
        ]),
    ];
    let rows = decode_luma_sb(128, 64, &sbs);

    for (y, row) in rows.iter().enumerate() {
        assert_eq!(
            &row[..64],
            &rows[0][..64],
            "row {y} of the whole superblock repeats its first row"
        );
    }
    let first = &rows[0][..64];
    assert!(
        first.windows(2).all(|w| w[0] >= w[1]),
        "the whole superblock falls across its sixty-four samples without \
         restarting: {first:?}"
    );
    assert!(
        first[0] > first[63],
        "the whole superblock falls from left to right, got {} then {}",
        first[0],
        first[63]
    );
    for block_row in 0..2 {
        for block_col in 2..4 {
            let block = block_at(&rows, block_row, block_col);
            assert_falls_down(&block, &format!("quadrant ({block_row},{block_col})"));
        }
    }
}

/// A quadrant may be split again into four 16x16 blocks, each carrying its
/// own 16x16 luma transform and 8x8 chroma transforms. A DC level per block
/// makes each of the four a flat grey of its own, so the quadrant reads as
/// four squares rather than one — which is what tells the extra split from
/// a 32x32 block that merely carries the same coefficients.
#[test]
fn a_quadrant_splits_into_four_sixteens() {
    if !have_ffmpeg() {
        eprintln!("SKIP a_quadrant_splits_into_four_sixteens: no ffmpeg");
        return;
    }
    let dc = |level: i32| {
        BlockCoeffs::from(vec![Coeff {
            row: 0,
            col: 0,
            level,
        }])
    };
    let levels = [14, 8, -8, -14];
    let flat = BlockCoeffs::from(Vec::new());
    let sbs = [Superblock::Split(vec![
        Quadrant::Split(levels.iter().map(|&l| dc(l)).collect()),
        Quadrant::Whole(flat.clone()),
        Quadrant::Whole(flat.clone()),
        Quadrant::Whole(flat),
    ])];
    let rows = decode_luma_sb(64, 64, &sbs);

    let mut greys = Vec::new();
    for i in 0..levels.len() {
        let (y0, x0) = ((i / 2) * 16, (i % 2) * 16);
        let grey = rows[y0][x0];
        for (y, row) in rows.iter().enumerate().skip(y0).take(16) {
            for (x, &sample) in row.iter().enumerate().skip(x0).take(16) {
                assert_eq!(
                    sample, grey,
                    "the 16x16 block at ({x0},{y0}) is flat, but ({x},{y}) is not"
                );
            }
        }
        greys.push(grey);
    }

    // Each block is predicted DC from the blocks the decoder has already
    // rebuilt, so what its own level does is move it off that prediction,
    // in the level's own direction.
    let g = |i: usize| i32::from(greys[i]);
    let predictions = [128, g(0), g(0), (g(1) + g(2) + 1) / 2];
    for (i, (&level, prediction)) in levels.iter().zip(predictions).enumerate() {
        assert_eq!(
            (g(i) - prediction).signum(),
            level.signum(),
            "the 16x16 block {i} carries level {level}, so it sits on the \
             {} side of the {prediction} its neighbours predict, not at {}",
            if level > 0 { "bright" } else { "dark" },
            g(i)
        );
    }
    assert_eq!(
        greys.iter().collect::<std::collections::HashSet<_>>().len(),
        4,
        "the four blocks are four different greys, not one: {greys:?}"
    );
}

/// A superblock half outside the frame has no partition that keeps a block
/// outside it, so it cannot be left whole.
#[test]
fn a_whole_superblock_at_the_frame_edge_is_refused() {
    let block = BlockCoeffs::from(vec![Coeff {
        row: 0,
        col: 0,
        level: 4,
    }]);
    let sbs = [
        Superblock::Split(vec![
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block.clone()),
        ]),
        Superblock::Whole(block.clone()),
    ];
    // Ninety-six samples across is three 32x32 blocks, so the second
    // superblock hangs half outside the frame.
    let err = sb_coeff_key_frame_tile(24, 16, Q_IDX, &sbs).unwrap_err();
    assert!(
        format!("{err}").contains("half outside the frame"),
        "got {err}"
    );
}

#[test]
fn a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block: no \
             ffmpeg"
        );
        return;
    }
    let blocks: Vec<BlockCoeffs> = (0..6)
        .map(|i| {
            let (row, col) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
            BlockCoeffs::from(vec![Coeff {
                row,
                col,
                level: 12,
            }])
        })
        .collect();
    let rows = decode_luma_at(96, 64, &blocks);
    for block_row in 0..2 {
        for block_col in 0..3 {
            let name = format!("block ({block_row},{block_col})");
            let block = block_at(&rows, block_row, block_col);
            if (block_row * 3 + block_col) % 2 == 0 {
                assert_falls_across(&block, &name);
            } else {
                assert_falls_down(&block, &name);
            }
        }
    }
}

/// The transpose of [`a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block`]:
/// an odd number of 32x32 block *rows* (superblock hangs off the
/// *bottom*, `has_cols=true, has_rows=false`) rather than an odd number of
/// columns (hangs off the *right*, `has_cols=false, has_rows=true`) --
/// the two halves of the `(has_cols, has_rows)` match in
/// `sb_coeff_key_frame_tile` gather from different tables
/// (`VERT_ALIKE`/`HORZ_ALIKE`), and only the right-hand-edge direction had
/// a real-decoder test before this one.
#[test]
fn a_frame_that_hangs_off_the_bottom_codes_every_block() {
    if !have_ffmpeg() {
        eprintln!("SKIP a_frame_that_hangs_off_the_bottom_codes_every_block: no ffmpeg");
        return;
    }
    let blocks: Vec<BlockCoeffs> = (0..6)
        .map(|i| {
            let (row, col) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
            BlockCoeffs::from(vec![Coeff {
                row,
                col,
                level: 12,
            }])
        })
        .collect();
    let rows = decode_luma_at(64, 96, &blocks);
    for block_row in 0..3 {
        for block_col in 0..2 {
            let name = format!("block ({block_row},{block_col})");
            let block = block_at(&rows, block_row, block_col);
            if (block_row * 2 + block_col) % 2 == 0 {
                assert_falls_across(&block, &name);
            } else {
                assert_falls_down(&block, &name);
            }
        }
    }
}

/// A frame whose blocks do not tile it is refused rather than written as a
/// stream a decoder walks off the end of.
#[test]
fn a_frame_that_is_not_a_whole_number_of_blocks_is_refused() {
    let (_, header) = frame_of(80, 64);
    let blocks = vec![BlockCoeffs::default(); 4];
    let err = split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, &blocks)
        .expect_err("80 is not a whole number of 32x32 blocks");
    assert!(
        format!("{err}").contains("32x32"),
        "the refusal names the block size, got {err}"
    );
}

/// A directional intra mode says where its prediction comes from, and with
/// no residual of its own a block must be exactly that prediction. The
/// top-left block is given a basis function that is flat across it and
/// falls down it; the block to its right is coded `H_PRED`, so every one of
/// its rows must be the constant its left neighbour's rightmost column
/// hands it, and the block below is coded `V_PRED`, so every one of its
/// columns must be the constant the row above it ends on — a single flat
/// value for the whole block. Nothing here pins the transform's shape or
/// the mode's CDF: a mode written with the wrong context, an angle delta
/// left out or a mode index off by one desyncs the symbol decoder and the
/// two empty blocks stop mirroring their neighbours.
#[test]
fn directional_modes_predict_from_the_neighbours_they_name() {
    if !have_ffmpeg() {
        eprintln!("SKIP directional_modes_predict_from_the_neighbours_they_name: no ffmpeg");
        return;
    }
    let gradient = BlockCoeffs {
        luma: vec![Coeff {
            row: 1,
            col: 0,
            level: 12,
        }],
        ..BlockCoeffs::default()
    };
    let blocks = [
        gradient,
        BlockCoeffs {
            mode: H_PRED as u8,
            ..BlockCoeffs::default()
        },
        BlockCoeffs {
            mode: V_PRED as u8,
            ..BlockCoeffs::default()
        },
        BlockCoeffs::default(),
    ];
    let [luma, ..] = decode_coeff_planes(&blocks);

    let source = block_at(&luma, 0, 0);
    for (r, row) in source.iter().enumerate() {
        assert!(
            row.iter().all(|&s| s == row[0]),
            "the basis function at row 1, column 0 is flat across the block, row {r} is not:                  {row:?}"
        );
    }
    let profile: Vec<u8> = source.iter().map(|r| r[0]).collect();
    assert!(
        profile.windows(2).all(|w| w[0] >= w[1]) && profile[0] > profile[31],
        "the block must fall from top to bottom, got {profile:?}"
    );

    let horizontal = block_at(&luma, 0, 1);
    for (r, row) in horizontal.iter().enumerate() {
        assert!(
            row.iter().all(|&s| s == profile[r]),
            "an H_PRED block repeats the column to its left, row {r} should be all {} but is                  {row:?}",
            profile[r]
        );
    }

    let vertical = block_at(&luma, 1, 0);
    let bottom = profile[31];
    assert!(
        vertical.iter().flatten().all(|&s| s == bottom),
        "a V_PRED block repeats the row above it, which is all {bottom} here, got rows \
         {:?} and {:?}",
        vertical[0],
        vertical[31]
    );
}

/// The mode index is checked before anything is written.
#[test]
fn an_intra_mode_outside_the_key_frame_set_is_refused() {
    let mut blocks = vec![BlockCoeffs::default(); 4];
    blocks[2].mode = INTRA_MODES as u8;
    let message = split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks)
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("intra mode 13"),
        "the refusal must name the mode, got {message}"
    );
}

#[test]
fn coefficients_the_writer_cannot_code_are_refused() {
    let empty = || vec![BlockCoeffs::default(); 4];
    let cases: [(&str, Vec<BlockCoeffs>); 6] = [
        ("one set short", vec![BlockCoeffs::default(); 3]),
        ("off the transform", {
            let mut b = empty();
            b[0].luma = vec![Coeff {
                row: 32,
                col: 0,
                level: 1,
            }];
            b
        }),
        ("a zero level", {
            let mut b = empty();
            b[1].luma = vec![Coeff {
                row: 0,
                col: 0,
                level: 0,
            }];
            b
        }),
        ("a level past the Golomb tail's reach", {
            let mut b = empty();
            b[2].luma = vec![Coeff {
                row: 0,
                col: 0,
                level: MAX_LEVEL + 1,
            }];
            b
        }),
        ("two coefficients at one position", {
            let mut b = empty();
            b[3].luma = vec![
                Coeff {
                    row: 4,
                    col: 4,
                    level: 1,
                },
                Coeff {
                    row: 4,
                    col: 4,
                    level: 2,
                },
            ];
            b
        }),
        ("off the chroma transform", {
            let mut b = empty();
            b[0].u = vec![Coeff {
                row: 0,
                col: 16,
                level: 1,
            }];
            b
        }),
    ];
    for (name, blocks) in cases {
        assert!(
            split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks).is_err(),
            "{name} must be refused"
        );
    }
}

/// The default scan is written as the rule that generates it rather than a
/// thousand pinned numbers, so the rule is checked against what a scan must
/// be: every position exactly once, the origin first, and never a position
/// before one it sits diagonally behind — which is what lets a decoder read
/// a coefficient's context off the coefficients it has already decoded.
/// lane-rdoq's two contracts, on a block shaped like the ones the pass
/// exists for (a strong DC, two mid levels and a tail of barely-rounded
/// +-1s): it may only move a block to a LOWER `D + lambda * R`, and the
/// bits it hands back must be the price of the grid it leaves behind --
/// the search takes that number as the block's rate, so a stale one
/// would price a tile nobody writes.
#[test]
fn rdoq_only_lowers_the_cost_and_reports_its_own_grid() {
    let (side, set, q, lambda) = (16usize, TxbSet::Luma16, 150u8, 0.0275);
    let mut scaled = vec![0f32; side * side];
    for (i, v) in [
        (0usize, 7.4f32),
        (1, 2.6),
        (side, -1.55),
        // The tail the pass exists for: coefficients the deadzone only
        // just rounded up to +-1, which give a whole level back for a
        // fraction of one step of squared error.
        (2, 0.55),
        (2 * side + 1, -0.52),
        (3 * side + 3, 0.51),
    ] {
        scaled[i] = v;
    }
    // The deadzone quantiser this pass runs after, at the shipped 0.5.
    let mut levels: Vec<i32> = scaled
        .iter()
        .map(|&v| {
            let m = f64::from(v).abs() + 0.5;
            let l = if m < 1.0 { 0 } else { m.floor() as i32 };
            if v < 0.0 { -l } else { l }
        })
        .collect();
    let before = levels.clone();
    let dc = f64::from(crate::quant::dc_q(8, i32::from(q)));
    let ac = f64::from(crate::quant::ac_q(8, i32::from(q)));
    let dc_weight = (dc / ac) * (dc / ac);
    let cost = |grid: &[i32]| -> f64 {
        let d: f64 = grid
            .iter()
            .zip(&scaled)
            .enumerate()
            .map(|(i, (&l, &s))| {
                (if i == 0 { dc_weight } else { 1.0 }) * (f64::from(s) - f64::from(l)).powi(2)
            })
            .sum();
        d + lambda * coeff_bits(grid, set, crate::decode::q_ctx_of(q), 0, 0)
    };
    let bits = rdoq(
        &mut levels,
        &scaled,
        side,
        q,
        set,
        0,
        0,
        lambda,
        TxType::DctDct,
    );
    assert_eq!(
        bits,
        coeff_bits(&levels, set, crate::decode::q_ctx_of(q), 0, 0)
    );
    assert!(
        cost(&levels) <= cost(&before),
        "{:?} vs {:?}",
        cost(&levels),
        cost(&before)
    );
    // The tail is what it is for: the last coded position moves earlier.
    let last = |g: &[i32]| scan_of(side).iter().rposition(|&p| g[usize::from(p)] != 0);
    assert!(
        last(&levels) < last(&before),
        "eob {:?} -> {:?}",
        last(&before),
        last(&levels)
    );
    // A 64-point transform is priced on its top-left 32x32 corner, and
    // the pass has to hand back THAT price (the dense path).
    let (side, set) = (64usize, TxbSet::Luma64);
    let mut scaled = vec![0f32; side * side];
    for (i, v) in [(0usize, 6.1f32), (1, 0.55), (side + 1, -0.53)] {
        scaled[i] = v;
    }
    let mut levels: Vec<i32> = scaled
        .iter()
        .map(|&v| {
            let m = f64::from(v).abs() + 0.5;
            let l = if m < 1.0 { 0 } else { m.floor() as i32 };
            if v < 0.0 { -l } else { l }
        })
        .collect();
    let bits = rdoq(
        &mut levels,
        &scaled,
        side,
        q,
        set,
        0,
        0,
        lambda,
        TxType::DctDct,
    );
    let corner: Vec<i32> = (0..32)
        .flat_map(|row| levels[row * side..][..32].to_vec())
        .collect();
    assert_eq!(
        bits,
        coeff_bits(&corner, set, crate::decode::q_ctx_of(q), 0, 0)
    );
}

#[test]
fn the_default_scan_walks_every_position_outwards() {
    for side in [TX16, TX32] {
        let area = side * side;
        let scan = default_scan(side);
        assert_eq!(scan.len(), area);
        let mut seen = vec![false; area];
        for &pos in &scan {
            assert!(
                !seen[pos as usize],
                "position {pos} is scanned twice in {side}x{side}"
            );
            seen[pos as usize] = true;
        }
        assert_eq!(scan[0], 0, "the scan starts at the DC");

        let mut order = vec![0usize; area];
        for (i, &pos) in scan.iter().enumerate() {
            order[pos as usize] = i;
        }
        for row in 0..side {
            for col in 0..side {
                for (dr, dc) in [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)] {
                    let (nr, nc) = (row + dr, col + dc);
                    if nr < side && nc < side {
                        assert!(
                            order[nr * side + nc] > order[row * side + col],
                            "({nr},{nc}) is a context neighbour of ({row},{col}), so it must \
                             be scanned after it"
                        );
                    }
                }
            }
        }
    }
}

/// The whole point of the inverse transform living in the encoder is that
/// it predicts the decoder without asking one. The residual a DC-only
/// 64x64 transform produces has already been pinned from a real decoder in
/// [`DECODED_AT_Q100`], so that table is the gate: every level in it, both
/// signs, has to come back out of `dequant_and_inverse` exactly.
#[test]
fn the_inverse_transform_reproduces_the_pinned_whole_superblock_residuals() {
    for (level, _) in DECODED_AT_Q100 {
        let mut levels = vec![0i32; 64 * 64];
        levels[0] = level;
        let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
        let want = dc_residual(level);
        assert!(
            residual.iter().all(|&r| r == want),
            "level {level}: want a flat {want}, got {}..{} (first {})",
            residual.iter().min().unwrap(),
            residual.iter().max().unwrap(),
            residual[0]
        );
    }
}

/// The same gate at the other transform size the encoder codes: a split
/// superblock's 32x32 blocks, whose residuals are pinned in
/// [`SPLIT_RESIDUAL_AT_Q100`]. The two sizes divide the dequantized
/// coefficient by different denominators, so a size-blind dequantizer
/// passes one table and fails the other.
#[test]
fn the_inverse_transform_reproduces_the_pinned_split_residuals() {
    for (level, want) in SPLIT_RESIDUAL_AT_Q100 {
        let mut levels = vec![0i32; 32 * 32];
        levels[0] = level;
        let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
        assert!(
            residual.iter().all(|&r| r == want),
            "level {level}: want a flat {want}, got {}..{} (first {})",
            residual.iter().min().unwrap(),
            residual.iter().max().unwrap(),
            residual[0]
        );
    }
}

/// A DC-only transform is flat, which is exactly the case that cannot tell
/// a transposed or mis-permuted butterfly network from a correct one. An AC
/// coefficient in the first row must vary along the row and stay constant
/// down each column, and the one in the first column the other way round;
/// a transposed network swaps the two.
#[test]
fn a_single_ac_coefficient_varies_along_its_own_axis() {
    for side in [4, 8, 16, 32, 64] {
        let mut horizontal = vec![0i32; side * side];
        horizontal[1] = 100;
        let h = crate::transform::dequant_and_inverse(&horizontal, side, 8, i32::from(Q_IDX));
        let mut vertical = vec![0i32; side * side];
        vertical[side] = 100;
        let v = crate::transform::dequant_and_inverse(&vertical, side, 8, i32::from(Q_IDX));
        for row in 0..side {
            for col in 0..side {
                assert_eq!(
                    h[row * side + col],
                    h[col],
                    "{side}: a first-row coefficient is constant down column {col}"
                );
                assert_eq!(
                    v[row * side + col],
                    v[row * side],
                    "{side}: a first-column coefficient is constant along row {row}"
                );
            }
        }
        // The two are each other's transpose. Only to within a rounding
        // step: the spec rounds halves up rather than away from zero, so a
        // basis function and its transpose can land a count apart where
        // the row and column passes round opposite ways.
        assert!(h[0] != h[side - 1], "{side}: the horizontal basis varies");
        for row in 0..side {
            for col in 0..side {
                let (a, b) = (h[row * side + col], v[col * side + row]);
                assert!(
                    (a - b).abs() <= 1,
                    "{side}: transpose at ({row},{col}): {a} vs {b}"
                );
            }
        }
    }
}

/// Negating every level negates every residual, to within the one count
/// the spec's round-halves-up leaves behind. A dequantizer that rounded a
/// negative coefficient away from zero instead of toward it would shift a
/// whole basis function, not a count of it, so the bound is what makes
/// this a test rather than a restatement.
#[test]
fn negating_the_levels_negates_the_residual() {
    for side in [32, 64] {
        for level in [1, 2, 3, 5, 9, 14, 40, 100] {
            let mut levels = vec![0i32; side * side];
            levels[0] = level;
            levels[1] = -level;
            levels[side + 1] = level * 2;
            let pos = crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
            let negated: Vec<i32> = levels.iter().map(|&l| -l).collect();
            let neg =
                crate::transform::dequant_and_inverse(&negated, side, 8, i32::from(Q_IDX));
            let worst = pos
                .iter()
                .zip(&neg)
                .map(|(&p, &n)| (p + n).abs())
                .max()
                .expect("the block is not empty");
            assert!(worst <= 1, "side {side} level {level}: off by {worst}");
        }
    }
}

/// The pinned tables above are DC-only, and a flat residual cannot tell a
/// correct butterfly network from one that is merely correct at DC. This
/// is the same claim against a real decoder and with real AC content: the
/// top-left block of a key frame predicts a flat mid-grey with no
/// neighbours to read, so what ffmpeg shows there is exactly 128 plus the
/// residual the encoder's own inverse transform computes. Every sample has
/// to agree, not most of them.
#[test]
fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block: no ffmpeg"
        );
        return;
    }
    // A DC, two low-frequency terms of each orientation, a diagonal and a
    // far corner, with both signs among them.
    let coeffs = [
        (0u8, 0u8, 9i32),
        (0, 1, -14),
        (1, 0, 7),
        (0, 3, 5),
        (3, 0, -3),
        (2, 2, 11),
        (7, 5, -6),
        (31, 31, 4),
    ];
    let block: BlockCoeffs = coeffs
        .iter()
        .map(|&(row, col, level)| Coeff { row, col, level })
        .collect::<Vec<_>>()
        .into();
    let rows = decode_luma_at(
        64,
        64,
        &[
            block,
            BlockCoeffs::default(),
            BlockCoeffs::default(),
            BlockCoeffs::default(),
        ],
    );

    let mut levels = vec![0i32; 32 * 32];
    for &(row, col, level) in &coeffs {
        levels[usize::from(row) * 32 + usize::from(col)] = level;
    }
    let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
    for row in 0..32 {
        for col in 0..32 {
            let want = (128 + residual[row * 32 + col]).clamp(0, 255);
            assert_eq!(
                i32::from(rows[row][col]),
                want,
                "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                rows[row][col]
            );
        }
    }
    // The block is not flat, so the agreement is about the transform and
    // not about a prediction both sides got trivially right.
    let (lo, hi) = (
        rows[..32].iter().flat_map(|r| &r[..32]).min().unwrap(),
        rows[..32].iter().flat_map(|r| &r[..32]).max().unwrap(),
    );
    assert!(hi - lo > 4, "the block carries real detail, got {lo}..{hi}");
}

/// The same against the smallest transform the writer codes, the 16x16 one
/// a split quadrant carries. Its rounding is the one the split path rests
/// on, and nothing else in the crate exercises it against a decoder.
#[test]
fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_16x16_block() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_16x16_block: no ffmpeg"
        );
        return;
    }
    let coeffs = [
        (0u8, 0u8, 9i32),
        (0, 1, -14),
        (1, 0, 7),
        (0, 3, 5),
        (3, 0, -3),
        (2, 2, 11),
        (7, 5, -6),
        (15, 15, 4),
    ];
    let block: BlockCoeffs = coeffs
        .iter()
        .map(|&(row, col, level)| Coeff { row, col, level })
        .collect::<Vec<_>>()
        .into();
    let (seq, header) = frame_of(64, 64);
    let quadrants = vec![
        Quadrant::Split(vec![
            block,
            BlockCoeffs::default(),
            BlockCoeffs::default(),
            BlockCoeffs::default(),
        ]),
        Quadrant::Whole(BlockCoeffs::default()),
        Quadrant::Whole(BlockCoeffs::default()),
        Quadrant::Whole(BlockCoeffs::default()),
    ];
    let tile = sb_coeff_key_frame_tile(
        header.mi_cols,
        header.mi_rows,
        Q_IDX,
        &[Superblock::Split(quadrants)],
    )
    .unwrap();
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
    let planes = ffmpeg_decode(&stream, 64, 64);

    let mut levels = vec![0i32; TX16 * TX16];
    for &(row, col, level) in &coeffs {
        levels[usize::from(row) * TX16 + usize::from(col)] = level;
    }
    let residual = crate::transform::dequant_and_inverse(&levels, TX16, 8, i32::from(Q_IDX));
    for row in 0..TX16 {
        for col in 0..TX16 {
            let want = (128 + residual[row * TX16 + col]).clamp(0, 255);
            assert_eq!(
                i32::from(planes[row * 64 + col]),
                want,
                "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                planes[row * 64 + col]
            );
        }
    }
}

/// The same against the other transform size, the 64x64 one a whole
/// superblock carries. Only its top-left 32x32 can hold coefficients, and
/// the spec zeroes the rest before the row transform — a 64-point network
/// that read the missing half as anything else would disagree here.
#[test]
fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block: no ffmpeg"
        );
        return;
    }
    let coeffs = [
        (0u8, 0u8, 12i32),
        (0, 2, -9),
        (2, 0, 6),
        (1, 1, -5),
        (5, 9, 8),
        (31, 0, -4),
        (0, 31, 3),
    ];
    let block: BlockCoeffs = coeffs
        .iter()
        .map(|&(row, col, level)| Coeff { row, col, level })
        .collect::<Vec<_>>()
        .into();
    let rows = decode_luma_sb(64, 64, &[Superblock::Whole(block)]);

    let mut levels = vec![0i32; 64 * 64];
    for &(row, col, level) in &coeffs {
        levels[usize::from(row) * 64 + usize::from(col)] = level;
    }
    let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
    for row in 0..64 {
        for col in 0..64 {
            let want = (128 + residual[row * 64 + col]).clamp(0, 255);
            assert_eq!(
                i32::from(rows[row][col]),
                want,
                "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                rows[row][col]
            );
        }
    }
}

/// The AV1 transforms are normalized so that a quantized level means the
/// same thing at every size: doubling the transform's side halves what a
/// DC level is worth, which is the job the per-size row shift and the
/// dequantizer's denominator split between them. The frame syntax only
/// codes 32x32 and 64x64 transforms, so this is what holds the four
/// smaller sizes' shifts to the spec's table — a shift wrong by one at any
/// size doubles or halves that size's step alone.
#[test]
fn a_dc_level_is_worth_half_as_much_each_time_the_transform_doubles() {
    let mut previous: Option<i32> = None;
    for side in [4usize, 8, 16, 32, 64] {
        let mut levels = vec![0i32; side * side];
        levels[0] = 100;
        let residual =
            crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
        let dc = residual[0];
        assert!(
            residual.iter().all(|&r| r == dc),
            "side {side}: a DC coefficient reconstructs flat"
        );
        if let Some(previous) = previous {
            assert!(
                (dc * 2 - previous).abs() <= 1,
                "side {side}: {dc} is not half of the previous size's {previous}"
            );
        }
        previous = Some(dc);
    }
}

/// The encoder's own round trip, through a real bitstream.
///
/// A picture is chosen, its residual against the DC prediction of 128 is
/// forward-transformed and quantized, the levels are written into a tile,
/// and ffmpeg decodes it. Two things have to hold: what comes back is
/// exactly what the encoder's own inverse said it would be — sample for
/// sample, which is what lets a rate-distortion loop trust its
/// reconstruction — and it is within a quantizer step of the picture that
/// was asked for, which is what makes the forward transform an encoder
/// rather than a scrambler.
#[test]
fn a_quantized_picture_decodes_to_what_the_encoder_reconstructed() {
    if !have_ffmpeg() {
        eprintln!(
            "SKIP a_quantized_picture_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
        );
        return;
    }
    // A gradient, a ripple and a corner patch: low frequencies, a mid one
    // and an edge, none of them a basis function of the transform.
    let mut residual = vec![0i32; 32 * 32];
    for row in 0..32usize {
        for col in 0..32usize {
            let gradient = row as f64 * 2.0 - 32.0;
            let ripple = 25.0
                * (col as f64 * std::f64::consts::PI / 6.0).sin()
                * (row as f64 * std::f64::consts::PI / 11.0).cos();
            let patch = if row >= 24 && col >= 20 { -40.0 } else { 0.0 };
            residual[row * 32 + col] = (gradient + ripple + patch).round() as i32;
        }
    }

    let levels =
        crate::transform::forward_and_quantize(&residual, 32, 8, i32::from(Q_IDX), 0.5);
    assert!(
        levels.iter().any(|&l| l != 0),
        "the picture quantized away entirely"
    );
    let coded: Vec<Coeff> = levels
        .iter()
        .enumerate()
        .filter(|&(_, &level)| level != 0)
        .map(|(i, &level)| Coeff {
            row: (i / 32) as u8,
            col: (i % 32) as u8,
            level,
        })
        .collect();

    let rows = decode_luma_at(
        64,
        64,
        &[
            coded.into(),
            BlockCoeffs::default(),
            BlockCoeffs::default(),
            BlockCoeffs::default(),
        ],
    );
    let reconstruction =
        crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));

    let mut squared = 0.0f64;
    for row in 0..32 {
        for col in 0..32 {
            let want = (128 + reconstruction[row * 32 + col]).clamp(0, 255);
            assert_eq!(
                i32::from(rows[row][col]),
                want,
                "sample ({row},{col}): the decoder and the encoder disagree"
            );
            let error = f64::from(rows[row][col]) - f64::from(128 + residual[row * 32 + col]);
            squared += error * error;
        }
    }
    let rmse = (squared / 1024.0).sqrt();
    // A quantizer step is q/8 in residual units. Rounding to nearest
    // spreads the error over a step, and a quarter of one is a bound this
    // picture clears with room to spare (2.2 of 14 as written) while a
    // forward transform that is merely energy-preserving does not: a
    // transposed column pass measures 13.3 here.
    let step = f64::from(crate::quant::ac_q(8, i32::from(Q_IDX))) / 8.0;
    assert!(
        rmse < step / 4.0,
        "the decoded picture is {rmse} off, a step being {step}"
    );
}

/// The inverse of [`write_mv_component`], mirroring [`SymbolDecoder`]
/// against the spec's own `read_mv_component` pseudocode.
fn decode_mv_component(
    dec: &mut crate::msac::tests::SymbolDecoder,
    c: &mut MvComponentCdfs,
) -> i32 {
    let sign = dec.symbol(&mut c.sign);
    let class = dec.symbol(&mut c.class);
    let local = if class == 0 {
        let bit = dec.symbol(&mut c.class0_bit);
        let fr = dec.symbol(&mut c.class0_fr[bit]);
        (bit << 3) | (fr << 1) | 1
    } else {
        let mut d = 0;
        for i in 0..class {
            d |= dec.symbol(&mut c.bit[i]) << i;
        }
        let fr = dec.symbol(&mut c.fr);
        (d << 3) | (fr << 1) | 1
    };
    let mag = mv_class_base(class) + local as i32 + 1;
    if sign == 1 { -mag } else { mag }
}

/// Decodes one superblock a [`sb_coeff_inter_frame_tile`] payload wrote,
/// mirroring the writer's own context tracking (partition, skip,
/// is_inter, and the MV stack) symbol for symbol against a fresh
/// [`Cdfs`], so a desync between the two shows up as a wrong decoded
/// value rather than a silent pass. Every block here is skipped, so no
/// coefficient symbols are read. `wrong_y_mode` swaps in `KF_Y_MODE`'s
/// (0, 0) row for an intra block's mode read, to show that reading the
/// wrong table decodes the wrong mode rather than the one that was
/// written.
///
/// One decoded block's `(skip, is_inter, mode, mv)` — `mode` is `None`
/// for an inter block, `mv` is `(0, 0)` for an intra one.
type DecodedBlock = (bool, bool, Option<usize>, (i32, i32));

/// Returns the number of symbols and literals read, and each block's
/// decoded state.
fn decode_inter_sb(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    wrong_y_mode: bool,
) -> (usize, Vec<DecodedBlock>) {
    let mut dec = crate::msac::tests::SymbolDecoder::new(data);
    let mut cdfs = Cdfs::new(2);
    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
    let mut above_skip = [false; 2];
    let mut left_skip = [false; 2];
    let mut above_inter = [false; 2];
    let mut left_inter = [false; 2];
    let mut count = 0usize;

    count += 1;
    dec.symbol(&mut cdfs.partition_w64[0]);

    let mut results = Vec::new();
    for quadrant in 0..4usize {
        let (r32, c32) = (quadrant / 2, quadrant % 2);
        count += 1;
        dec.symbol(&mut cdfs.partition_w32[0]);

        let (has_above, has_left) = (r32 > 0, c32 > 0);
        let skip_ctx = usize::from(above_skip[c32]) + usize::from(left_skip[r32]);
        count += 1;
        let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;

        let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter[c32], left_inter[r32]);
        count += 1;
        let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;

        let (mode, mv) = if is_inter {
            let sr_ctx = crate::mvstack::single_ref_ctx(above_inter[c32] || left_inter[r32]);
            count += 3;
            assert_eq!(
                dec.symbol(&mut cdfs.single_ref[sr_ctx][0]),
                0,
                "single_ref p1"
            );
            assert_eq!(
                dec.symbol(&mut cdfs.single_ref[sr_ctx][2]),
                0,
                "single_ref p3"
            );
            assert_eq!(
                dec.symbol(&mut cdfs.single_ref[sr_ctx][3]),
                0,
                "single_ref p4"
            );

            let (mi_row, mi_col) = (r32 * 8, c32 * 8);
            let stack = find_mv_stack(
                &grid,
                mi_row,
                mi_col,
                8,
                8,
                1,
                mi_cols as usize,
                mi_rows as usize,
            );

            count += 1;
            let new_mv = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 0;
            let (mv, is_new_mv) = if new_mv {
                if stack.entries.len() > 1 {
                    count += 1;
                    dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[0]]);
                }
                count += 1;
                let joint = dec.symbol(&mut cdfs.mv_joint);
                let mut diff = (0, 0);
                if joint == 2 || joint == 3 {
                    // sign, class, class0_bit, class0_fr for this test's
                    // small (class 0) components.
                    count += 4;
                    diff.0 = decode_mv_component(&mut dec, &mut cdfs.mv_comp[0]);
                }
                if joint == 1 || joint == 3 {
                    count += 4;
                    diff.1 = decode_mv_component(&mut dec, &mut cdfs.mv_comp[1]);
                }
                ((stack.pred_mv.0 + diff.0, stack.pred_mv.1 + diff.1), true)
            } else {
                count += 2;
                assert_eq!(
                    dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]),
                    1,
                    "zero_mv"
                );
                assert_eq!(dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]), 0, "ref_mv");
                (stack.nearest_mv, false)
            };
            for dr in 0..8 {
                for dc in 0..8 {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: 1,
                            ref_frame1: NO_REF1,
                            mv1: (0, 0),
                            mv: mv16(mv),
                            is_new_mv,
                            size: 8,
                            size_h: 8,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            (None, mv)
        } else {
            count += 1;
            let m = if wrong_y_mode {
                dec.symbol(&mut cdfs.kf_y_mode[0][0])
            } else {
                dec.symbol(&mut cdfs.y_mode[3])
            };
            if (V_PRED..=D67_PRED).contains(&m) {
                count += 1;
                dec.symbol(&mut cdfs.angle_delta[m - V_PRED]);
            }
            count += 1;
            dec.symbol(&mut cdfs.uv_mode_cfl[m]);
            (Some(m), (0, 0))
        };

        above_skip[c32] = skip;
        left_skip[r32] = skip;
        above_inter[c32] = is_inter;
        left_inter[r32] = is_inter;
        results.push((skip, is_inter, mode, mv));
    }
    (count, results)
}

/// Every superblock here skips: no residual, so the only symbols are the
/// mode chain, and the byte count and the symbol count both come out
/// small enough to hand-check.
#[test]
fn an_all_skip_inter_superblock_is_small_and_reads_back() {
    let block = BlockCoeffs {
        inter: Some(InterInfo {
            ref1: None,
            mv1: (0, 0),
            ref_frame: crate::mvstack::LAST_FRAME,
            mode: InterMode::NearestMv,
            mv: (0, 0),
            ref_mv_idx: 0,
        }),
        skip: true,
        ..BlockCoeffs::default()
    };
    let blocks = vec![
        Quadrant::Whole(block.clone()),
        Quadrant::Whole(block.clone()),
        Quadrant::Whole(block.clone()),
        Quadrant::Whole(block),
    ];
    let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();
    // Hand count: one partition_w64, then per block partition_w32 + skip +
    // is_inter + 3 single_ref + new_mv + zero_mv + ref_mv = 9, times four.
    assert!(
        data.len() <= 16,
        "an all-skip NEARESTMV superblock took {} bytes",
        data.len()
    );

    let (count, results) = decode_inter_sb(&data, 16, 16, false);
    assert_eq!(count, 1 + 4 * 9, "symbol count against the hand count");
    for (skip, is_inter, mode, mv) in results {
        assert!(skip);
        assert!(is_inter);
        assert_eq!(mode, None);
        // Every block's only inter neighbours carry (0, 0), so the stack's
        // nearest candidate is (0, 0) throughout.
        assert_eq!(mv, (0, 0));
    }
}

/// The fourth block codes NEWMV against a (0, 0) predictor with a coded
/// column component: the symbol sequence a decoder reads back has to be
/// exactly what [`write_mv`] wrote, not a residual that merely decodes
/// to *some* valid vector.
#[test]
fn a_newmv_block_reads_back_the_exact_symbol_sequence_it_was_written_with() {
    let nearest = BlockCoeffs {
        inter: Some(InterInfo {
            ref1: None,
            mv1: (0, 0),
            ref_frame: crate::mvstack::LAST_FRAME,
            mode: InterMode::NearestMv,
            mv: (0, 0),
            ref_mv_idx: 0,
        }),
        skip: true,
        ..BlockCoeffs::default()
    };
    let newmv = BlockCoeffs {
        inter: Some(InterInfo {
            ref1: None,
            mv1: (0, 0),
            ref_frame: crate::mvstack::LAST_FRAME,
            mode: InterMode::NewMv,
            mv: (0, 2),
            ref_mv_idx: 0,
        }),
        skip: true,
        ..BlockCoeffs::default()
    };
    let blocks = vec![
        Quadrant::Whole(nearest.clone()),
        Quadrant::Whole(nearest.clone()),
        Quadrant::Whole(nearest),
        Quadrant::Whole(newmv),
    ];
    let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();

    let (count, results) = decode_inter_sb(&data, 16, 16, false);
    // Three NEARESTMV blocks at 9 symbols each, one NEWMV block at
    // partition + skip + is_inter + 3 single_ref + new_mv + mv_joint +
    // (sign, class, class0_bit, class0_fr for the one nonzero component)
    // = 12, plus the superblock's partition_w64.
    assert_eq!(count, 1 + 3 * 9 + 12, "symbol count against the hand count");
    let (skip, is_inter, mode, mv) = results[3];
    assert!(skip);
    assert!(is_inter);
    assert_eq!(mode, None);
    assert_eq!(
        mv,
        (0, 2),
        "the decoded motion vector must be exactly what was written"
    );
}

/// Decodes only the first 32x32 block's mode symbol of a superblock: the
/// tile's own first block always reads partition, skip and is_inter at
/// context zero, so this needs none of [`decode_inter_sb`]'s neighbour
/// bookkeeping — which matters here because the mutation this feeds
/// desyncs everything after the one symbol it is testing, and a decoder
/// that kept reading past it would be asserting on garbage.
fn decode_first_block_mode(data: &[u8], wrong_y_mode: bool) -> usize {
    let mut dec = crate::msac::tests::SymbolDecoder::new(data);
    let mut cdfs = Cdfs::new(2);
    dec.symbol(&mut cdfs.partition_w64[0]);
    dec.symbol(&mut cdfs.partition_w32[0]);
    dec.symbol(&mut cdfs.skip[0]);
    dec.symbol(&mut cdfs.intra_inter[0]);
    if wrong_y_mode {
        dec.symbol(&mut cdfs.kf_y_mode[0][0])
    } else {
        dec.symbol(&mut cdfs.y_mode[3])
    }
}

/// An inter frame's intra block reads its mode against `Y_MODE`'s size
/// group, not `KF_Y_MODE`'s neighbour-context table a key frame's block
/// reads. Reading the wrong table back — `KF_Y_MODE`'s `(0, 0)` row —
/// decodes a different mode than the one [`sb_coeff_inter_frame_tile`]
/// wrote, because the two tables carry different default probabilities
/// even though a row of each is the same fourteen-entry width. (The swap
/// this test's name warns of does not typecheck as a one-line
/// substitution at the write site either — `cdfs.kf_y_mode[3]` is a
/// `[[u16; 14]; 5]` row, not the single `[u16; 14]` `Y_MODE` indexing
/// needs — so a mutation that got the *outer* shape wrong would be
/// caught by `cargo check` before it ever reached a test; this test is
/// what catches getting the *right* leaf shape from the *wrong* table.)
#[test]
fn an_intra_block_in_an_inter_frame_is_caught_reading_kf_y_mode() {
    let block = BlockCoeffs {
        mode: H_PRED as u8,
        skip: true,
        ..BlockCoeffs::default()
    };
    let blocks = vec![Quadrant::Whole(block); 4];
    let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();

    assert_eq!(
        decode_first_block_mode(&data, false),
        H_PRED,
        "the right table reads the mode back exactly"
    );
    assert_ne!(
        decode_first_block_mode(&data, true),
        H_PRED,
        "KF_Y_MODE's different probabilities must decode a different mode"
    );
}

/// lane-rectdq witness: [`crate::encode::delta_q_bits`] prices exactly
/// what [`write_delta_q`] writes, over the whole `delta_q_abs` domain
/// (0..=20 steps, both signs -- the escape literal pair starts at 3 and
/// its `rem_bits` widens at 4, 5, 9 and 17). The writer is run into a
/// PRICING coder whose account (`SymbolEncoder::bits`) is the same
/// arithmetic-coder cost the pricer's `symbol_bits` sums, so the two
/// agree to floating-point noise, not to a tolerance band.
///
/// Both sides read the DEFAULT `delta_q` row: the writer's `cdfs` is
/// rebuilt per case, exactly as every tile's is armed before its first
/// superblock.
#[test]
fn the_delta_q_pricer_matches_the_writer_over_the_whole_domain() {
    const RES: i32 = 4;
    const CUR: i32 = 128;
    for steps in -20i32..=20 {
        let target = CUR + steps * RES;
        arm_delta_q(RES, 1, vec![u8::try_from(target).expect("in range")], 128);
        let mut cdfs = Cdfs::new(2);
        let mut enc = SymbolEncoder::pricer();
        enc.reset_bits();
        // `is_whole_sb = false`: the rect half / 64-superblock shape,
        // the one whose pricer this witnesses.
        write_delta_q(&mut enc, &mut cdfs, (0, 0), false, true);
        let written = enc.bits();
        let priced = crate::encode::delta_q_bits(target, CUR, RES);
        assert!(
            (written - priced).abs() < 1e-9,
            "steps {steps}: writer {written} bits, pricer {priced}"
        );
    }
    // Disarmed plan: the writer codes nothing and the pricer charges
    // nothing.
    arm_delta_q(0, 1, Vec::new(), 128);
    let mut cdfs = Cdfs::new(2);
    let mut enc = SymbolEncoder::pricer();
    enc.reset_bits();
    write_delta_q(&mut enc, &mut cdfs, (0, 0), false, true);
    assert!(enc.bits().abs() < 1e-9, "a disarmed plan writes no symbol");
    assert!((crate::encode::delta_q_bits(200, 128, 0)).abs() < 1e-9);
}
