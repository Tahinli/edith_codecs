//! Generator for the two AV1 coverage cells no encoder emits, so both are
//! GATED rather than left as declared debt (`lanes/av1formatsweep.report.md`
//! §6.4 and its "declared debt" note: the `4:2:0 with an odd luma dimension`
//! and `4:4:0 (ss 0,1)` rows were "unmeasured and unproducible by recipe").
//!
//! Both cells are produced here, offline, with no encoder and no ffmpeg:
//!
//! 1. **`420_odd65x65_key.obu`** -- a 4:2:0 key frame whose header declares
//!    an ODD luma size (65x65). aomenc cannot produce this one: it rounds the
//!    coded size DOWN to even (`av1enc_set_frame_size`; the measured
//!    `testsrc2 131x131` y4m comes out as `max_frame=130x130`), so the cell
//!    needs the encoder to be told what size to DECLARE. That is
//!    [`ec_av1::encode::encode_key_frame_at_size`]: a 96x96 picture padded to
//!    the block grid, coded with a 65x65 frame header. The result is a real,
//!    fully decodable AV1 stream -- the gate compares it sample for sample
//!    against the instrumented `aomdec`.
//!
//! 2. **`440_request_is_422.obu`** -- what this crate's writer produces when
//!    it is HANDED 4:4:0 (`subsampling_x = 0`, `subsampling_y = 1`). The
//!    cell turns out not to exist: `subsampling_y` is coded only when
//!    `subsampling_x` is 1 (spec 5.5.2 `color_config`), so a sequence header
//!    carries 4:2:0 `(1,1)`, 4:2:2 `(1,0)` or 4:4:4 `(0,0)` and never
//!    `(0,1)` -- libaom's own writer ASSERTS it ("4:4:0 subsampling not
//!    allowed in AV1", `av1/encoder/bitstream.c:2467-2468`) and its reader
//!    forces
//!    `subsampling_y = 0` when `subsampling_x` is 0
//!    (`av1_read_color_config`, `av1/decoder/decodeframe.c:4171-4175`).
//!    aomenc having no `yuv440p` input path and ffmpeg's y4m muxer refusing
//!    the format ("yuv4mpeg can only handle yuv444p, yuv422p, yuv420p,
//!    yuv411p and gray8") are consequences of that, not the reason. The pin
//!    is the byte-level evidence: a 4:4:0 request at profile 2 (4:2:2, and
//!    no coded subsampling bit below 12 bits) lands on `(1,0)`. The frame OBU
//!    behind it is a real 4:2:0 key frame's, byte for byte -- the subsampling
//!    lives only in the sequence header -- so the pin is a HEADER witness, NOT
//!    a decodable 4:2:2 stream. lane-av1422lift: it was never decodable as
//!    one; the decoder used to refuse it at the sequence header, and now
//!    admits the header, which is exactly why this pin may not be turned into
//!    a 4:2:2 exactness gate. The gate
//!    `the_440_cell_is_not_a_codable_chroma_shape` says exactly that, and the
//!    real 4:2:2 exactness lives in `a_real_422_stream_decodes_pixel_exact`
//!    and `the_pinned_422_corpus_cells_decode_pixel_exact`, which read
//!    genuinely 4:2:2 pins.
//!
//! ```text
//! cargo run -p ec-av1 --example gen_coverage_cells -- crates/ec-av1/fixtures
//! sha256sum crates/ec-av1/fixtures/420_odd65x65_key.obu \
//!           crates/ec-av1/fixtures/440_request_is_422.obu
//! ```
//!
//! Both files are deterministic: the source card is a fixed integer pattern
//! and the encoder is deterministic, so re-running this overwrites the pins
//! with the same bytes. `ec-av1` is a hand-code-only repository -- do NOT run
//! `cargo fmt` on this file; rustfmt runs at commit time and its churn is
//! accepted, never reverted.

use ec_av1::encode::Picture;
use ec_av1_syntax::{ColorConfig, ObuKind};

/// The padded coding surface the odd cell is cut from: a whole number of
/// 32x32 blocks (`Picture::check`'s own rule), and a multiple of 8 above the
/// 65x65 it declares, so no declared column is already block-aligned.
const PAD: usize = 96;

/// The odd-luma cell's DECLARED frame size. Both dimensions odd on purpose:
/// the 4:2:0 chroma extent is then `ROUND_POWER_OF_TWO(65, 1) = 33` in each
/// axis (libaom `av1_common_plane_width`, `av1/common/av1_common_int.h`),
/// which is where a floor (32) and a ceil (33) disagree -- the whole reason
/// the cell was worth closing.
const ODD: usize = 65;

/// The 4:4:0 witness's frame size, and the quantizer both streams are coded
/// at. Even, so nothing else about the witness is unusual: the shape is 4:4:0
/// and nothing else.
const WITNESS: usize = 64;

/// `base_q_idx` for both cells. Mid-scale on purpose: high enough that a
/// block carries real coefficients (so a wrong chroma extent shows in the
/// output), low enough that the stream stays small.
const Q_IDX: u8 = 100;

/// A deterministic, high-frequency card: a 2-D pattern whose wrapped
/// diagonal jumps, so every 8x8 block has real high-frequency content and
/// therefore real coefficients. That matters for the odd cell -- a flat or
/// smooth card predicts to within a quantizer step of a constant in its last
/// column too, so a wrong chroma extent could leave the OUTPUT unchanged and
/// the gate would then be green on broken code.
fn card(width: usize, height: usize) -> Picture {
    let mut picture = Picture::grey(width, height);
    for row in 0..height {
        for col in 0..width {
            picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u16;
        }
    }
    for row in 0..height / 2 {
        for col in 0..width / 2 {
            let i = row * (width / 2) + col;
            picture.u[i] = ((row * 13 + col * 29) % 241) as u16;
            picture.v[i] = ((row * 17 + col * 5) % 239) as u16;
        }
    }
    picture
}

/// The chroma shape of the 4:4:0 witness: profile 0 with `subsampling_x = 0`
/// and `subsampling_y = 1`. Profile 0 forces neither subsampling bit (only
/// profile 1 forces 4:4:4 and profile 2 below 12 bits forces 4:2:2, spec
/// 5.5.2), so both are coded in the header and `(0, 1)` -- 4:4:0 -- is legal
/// there.
fn yuv440() -> ColorConfig {
    ColorConfig {
        bit_depth: 8,
        mono_chrome: false,
        num_planes: 3,
        color_primaries: 2,
        transfer_characteristics: 2,
        matrix_coefficients: 2,
        color_range: false,
        subsampling_x: 0,
        subsampling_y: 1,
        chroma_sample_position: ec_av1_syntax::ChromaSamplePosition::Unknown,
        separate_uv_delta_q: false,
    }
}

/// Writes `bytes` to `<dir>/<name>`, then reports size, FNV-1a64 and sha256.
fn emit(dir: &str, name: &str, bytes: &[u8]) {
    let path = std::path::Path::new(dir).join(name);
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    let mut fnv: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        fnv = (fnv ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    println!("{name}: {} bytes, fnv1a64 {fnv:016x}", bytes.len());
    // sha256sum is coreutils, present on every host this runs on; the pins'
    // provenance comments in `stream.rs` quote its output.
    let out = std::process::Command::new("sha256sum")
        .arg(&path)
        .output()
        .expect("sha256sum");
    assert!(
        out.status.success(),
        "sha256sum failed on {}",
        path.display()
    );
    println!(
        "{name}: sha256 {}",
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .expect("a sha256sum line")
    );
}

/// The subsampling and the max frame size a reader gets back out of `bytes`
/// -- the generator never takes the shape it wrote on faith, it re-parses
/// its own output.
fn read_back(name: &str, bytes: &[u8]) -> (u8, u8, u32, u32) {
    let mut parser = ec_av1_syntax::Av1Parser::new();
    let mut pos = 0usize;
    while pos < bytes.len() && parser.sequence_header().is_none() {
        let obu = parser
            .parse_obu(&bytes[pos..])
            .unwrap_or_else(|e| panic!("{name}: OBU at byte {pos} does not parse: {e:?}"));
        pos += obu.total_size;
    }
    let seq = parser
        .sequence_header()
        .unwrap_or_else(|| panic!("{name}: no sequence header parsed back out of its own bytes"));
    (
        seq.color_config.subsampling_x,
        seq.color_config.subsampling_y,
        seq.max_frame_width,
        seq.max_frame_height,
    )
}

/// The bytes of `stream`'s single `OBU_FRAME`, OBU header and size field
/// included. An example cannot reach `Encoded::tile` (crate-private, and
/// documented as a `crate::decode` test-only reader) and does not need to:
/// the OBU is right there in the stream the encoder just wrote.
fn frame_obu_of(stream: &[u8], name: &str) -> Vec<u8> {
    let mut parser = ec_av1_syntax::Av1Parser::new();
    let mut pos = 0usize;
    while pos < stream.len() {
        let obu = parser
            .parse_obu(&stream[pos..])
            .unwrap_or_else(|e| panic!("{name}: OBU at byte {pos} does not parse: {e:?}"));
        if matches!(obu.kind, ObuKind::Frame(..)) {
            return stream[pos..pos + obu.total_size].to_vec();
        }
        pos += obu.total_size;
    }
    panic!(
        "{name}: no frame OBU in a {}-byte key frame stream",
        stream.len()
    );
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/ec-av1/fixtures".to_string());
    std::fs::create_dir_all(&dir).expect("output dir");

    // ---- cell 1: 4:2:0 with an odd luma dimension -------------------------
    let encoded = ec_av1::encode::encode_key_frame_at_size(&card(PAD, PAD), ODD, ODD, Q_IDX, 0.5)
        .expect("encoding the odd-luma key frame");
    let (sx, sy, mw, mh) = read_back("420_odd65x65_key", &encoded.stream);
    assert_eq!(
        (sx, sy),
        (1, 1),
        "the odd cell must stay 4:2:0 -- it is the luma SIZE that is odd, not the shape"
    );
    assert_eq!(
        (mw, mh),
        (ODD as u32, ODD as u32),
        "the odd cell's sequence header must declare {ODD}x{ODD}"
    );
    assert_eq!(
        (encoded.reconstruction.width, encoded.reconstruction.height),
        (ODD, ODD),
        "the returned reconstruction must be cropped to the DECLARED size"
    );
    assert_eq!(
        encoded.reconstruction.u.len(),
        ODD.div_ceil(2) * ODD.div_ceil(2),
        "a 4:2:0 frame of odd luma size keeps the ceil chroma extent"
    );
    println!(
        "420_odd65x65_key: max_frame {mw}x{mh} at ({sx},{sy}), chroma {}x{} \
         (ROUND_POWER_OF_TWO({ODD},1) = {}), mi {}x{}, reconstruction {}x{} luma + {} chroma \
         samples",
        ODD.div_ceil(2),
        ODD.div_ceil(2),
        ODD.div_ceil(2),
        2 * ((ODD + 7) >> 3),
        2 * ((ODD + 7) >> 3),
        encoded.reconstruction.width,
        encoded.reconstruction.height,
        encoded.reconstruction.u.len(),
    );
    emit(&dir, "420_odd65x65_key.obu", &encoded.stream);

    // ---- cell 2: the 4:4:0 (ss 0,1) row ------------------------------------
    // 4:4:0 is NOT A CELL: `subsampling_y` is coded only when
    // `subsampling_x` is 1 (spec 5.5.2 `color_config`), so a header can
    // carry 4:2:0 (1,1), 4:2:2 (1,0) or 4:4:4 (0,0) and never (0,1).
    // libaom asserts it in its own writer --
    // `assert(seq_params->subsampling_y == 0 && "4:4:0 subsampling not
    // allowed in AV1")`, av1/encoder/bitstream.c:2467-2468 -- and its reader
    // leaves `subsampling_y = 0` when `subsampling_x` is 0
    // (`av1_read_color_config`, av1/decoder/decodeframe.c:4171-4175).
    //
    // So this file pins WHAT A 4:4:0 REQUEST PRODUCES: the writer handed
    // subsampling (0,1) at profile 2, which below 12 bits is 4:2:2 and codes
    // no subsampling bit at all, and the request lands on (1,0). The frame
    // OBU behind it is a real 4:2:0 key frame's, byte for byte: the
    // subsampling lives only in the sequence header, so the frame header is
    // the same one either way. The stream is NOT a decodable 4:2:2 stream
    // (its tile is 4:2:0). lane-av1422lift: the decoder no longer refuses it
    // by name, and that is NOT an endorsement -- the header now claims 4:2:2
    // over a 4:2:0 tile, so a decode of these bytes is a header/tile
    // mismatch. This pin stays a byte-level shape witness; every 4:2:2
    // exactness claim lives on genuinely 4:2:2 pins in `stream.rs`.
    let tile_stream = ec_av1::encode::encode_key_frame(&card(WITNESS, WITNESS), Q_IDX, 0.5)
        .expect("encoding the witness tile")
        .stream;
    let frame_bytes = frame_obu_of(&tile_stream, "the 4:4:0 request's tile");
    let (mut seq, _header) =
        ec_av1::encode::key_frame_headers_colour(WITNESS, WITNESS, Q_IDX, yuv440())
            .expect("building the 4:4:0-request sequence header");
    seq.seq_profile = 2;
    let mut witness =
        ec_av1::sequence::sequence_header_obu(&seq).expect("writing that sequence header");
    witness.extend_from_slice(&frame_bytes);
    let (sx, sy, mw, mh) = read_back("440_request_is_422", &witness);
    assert_eq!(
        (sx, sy),
        (1, 0),
        "a 4:4:0 request at profile 2 must land on 4:2:2 (1,0). If it ever reads back (0,1), \
         4:4:0 became codable and the whole cell-2 story is wrong."
    );
    assert_eq!(
        (mw, mh),
        (WITNESS as u32, WITNESS as u32),
        "the request stream's frame size"
    );
    // The frame OBU behind the header must still parse as the frame header
    // the 4:2:0 stream carried.
    let mut parser = ec_av1_syntax::Av1Parser::new();
    let first = parser
        .parse_obu(&witness)
        .expect("the request's sequence header OBU parses");
    let frame = match parser
        .parse_obu(&witness[first.total_size..])
        .expect("the request's frame OBU parses")
        .kind
    {
        ObuKind::Frame(h, tiles) => {
            assert_eq!(
                tiles.len(),
                1,
                "the request stream carries exactly one tile"
            );
            h
        }
        other => panic!("440_request_is_422: expected a frame OBU, got {other:?}"),
    };
    assert_eq!(
        (frame.frame_width, frame.frame_height),
        (WITNESS as u32, WITNESS as u32),
        "the request stream's frame header must name its own size"
    );
    println!(
        "440_request_is_422: asked for subsampling (0,1) at profile {} -- got ({sx},{sy}) at \
         max_frame {mw}x{mh}, i.e. 4:2:2 (DECODED since lane-av1422lift, but these bytes carry \
         a 4:2:0 tile, so the pin is a shape witness and not an exactness witness); frame header + \
         one tile ({} bytes) from a 4:2:0 {WITNESS}x{WITNESS} key frame",
        seq.seq_profile,
        frame_bytes.len()
    );
    emit(&dir, "440_request_is_422.obu", &witness);
}
