use super::*;
use super::support::*;
use super::gates_a::*;
use crate::encode::{Picture as Pic, encode_key_frame_with_ctx, encode_sequence_with_ctx};
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-av1422lift: THE 4:2:2 LIFT, in two pins. This test is the renamed,
    /// inverted replacement for `a_non_420_subsampled_sequence_header_is_
    /// refused_by_name`, and the two fixtures replace that gate's fixture --
    /// a hand-built profile-2 header bolted onto a 4:2:0 TILE from this
    /// crate's own encoder, which could never be inverted into an exactness
    /// claim no matter how the assert was flipped (class `refusal-lifted-
    /// without-a-gate`, and the risk study's R6).
    ///
    /// Both pins are REAL 4:2:2 streams from libaom's own writer over a
    /// y4m whose header line is `C422`, so every chroma symbol in them is a
    /// 4:2:2 chroma symbol. Provenance (both, verbatim, no other encoder):
    ///
    /// ```text
    /// aomenc --codec=av1 --profile=2 \
    ///        --input-chroma-subsampling-x=1 --input-chroma-subsampling-y=0 \
    ///        --passes=1 --end-usage=q --cq-level=24 --cpu-used=0 \
    ///        --threads=1 --row-mt=0 --obu -o <pin> <source.y4m>
    /// ```
    ///
    /// * `422_key_64x64.obu` -- 755 bytes, sha256
    ///   `3c9396c9e42701d720419ec2cb5577387e305482df0cb150f7f0b965edeaaffc`,
    ///   fnv1a64 `0x87fc569cf53bcbc6`. A single 64x64 8-bit KEY frame
    ///   (`--limit=1 --kf-min-dist=0 --kf-max-dist=0`) over a
    ///   hand-generated two-frame integer `C422` card source, the second
    ///   source frame unused by the `--limit=1`. The smallest thing that can
    ///   say "4:2:2 intra reconstructs".
    /// * `422_inter_160x128_3f.obu` -- 6522 bytes, sha256
    ///   `7b54834c18f5fdc0e8606a6dc5bf4ff368402417d87fcfc2ab9758e4e9164902`,
    ///   fnv1a64 `0xf87eba3bf4fc2bb8`. Three 160x128 8-bit frames with a
    ///   panning square
    ///   (`--limit=3 --kf-min-dist=3 --kf-max-dist=3`, so frames 1 and 2 are
    ///   INTER), over a three-frame integer `C422` card source whose square
    ///   moves (6,4) -> (9,6) -> (12,8). This is what exercises the
    ///   inter-predicted 4:2:2 chroma reference: a decoder that walked the
    ///   key frame correctly and then predicted chroma from a 4:2:0-shaped
    ///   reference passes the first pin and reds here.
    ///
    /// Both are asserted BYTE-EXACT against ffmpeg 8.1.3's own rawvideo of
    /// the same bytes at `yuv422p`, per plane and per frame, with the
    /// per-plane coefficient-unit census as the non-vacuity arm (see
    /// [`assert_422_stream_pixel_exact`]). A gate whose only assertion was
    /// `err.contains(REFUSAL)` would have passed unchanged on a tree where
    /// 4:2:2 decode was completely broken; that is what this replaces.
    #[test]
    fn a_real_422_key_frame_and_inter_sequence_decode_pixel_exact() {
        const NAME: &str = "a_real_422_key_frame_and_inter_sequence_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let key = crate_pin("422_key_64x64.obu");
        let key_stream = read_pin(&key, 755, 0x87fc569cf53bcbc6, NAME);
        let (units, _) = assert_422_stream_pixel_exact(NAME, &key_stream, 64, 64, 1, 8, true);
        let (l, u, v) = (units[0], units[1], units[2]);
        assert!(
            l > 0 && u > 0 && v > 0,
            "{NAME}: the key-frame pin walked no 4:2:2 chroma units [{l}, {u}, {v}]"
        );

        let inter = crate_pin("422_inter_160x128_3f.obu");
        let inter_stream = read_pin(&inter, 6522, 0xf87eba3bf4fc2bb8, NAME);
        let (units, _) = assert_422_stream_pixel_exact(NAME, &inter_stream, 160, 128, 3, 8, true);
        let (l, u, v) = (units[0], units[1], units[2]);
        assert!(
            u > 0 && v > 0,
            "{NAME}: the inter pin walked no 4:2:2 chroma units [{l}, {u}, {v}] -- the inter \
             chroma reference path was never reached"
        );
    }

    /// lane-av1422lift: 4:2:2 at 12 bits, and 4:2:2 with superres, are the two
    /// cells the corpus had no coverage for at all (the risk study's R3 and
    /// R9: the sweep recipe passed no 12-bit depth and no `--superres-mode`,
    /// so every 4:2:2 measurement in the tree was 8- or 10-bit, none
    /// superres). Both are pinned and both are byte-exact here.
    ///
    /// `s422_12bit_160x128.obu` -- 9214 bytes, sha256
    /// `eacc70f75c82ad7d795efbd0242aa4ee33db917b8c1a3570ebce2570ee8bc245`,
    /// fnv1a64 `0xa62b8e3fe6f60ae5`. 160x128, 12-bit, 3 frames,
    /// `yuv422p12le` per ffprobe, profile 2 ("Professional").
    ///
    /// **Provenance is ffmpeg's libaom-av1 wrapper, not aomenc, and the
    /// reason is measured, not preferred.** aomenc's own 12-bit y4m path
    /// issues `AV1E_SET_CHROMA_SUBSAMPLING_X` (apps/aomenc.c:2272-2278,
    /// reached whenever the input is not 4:2:0 and the bit depth is 12)
    /// BEFORE `aom_codec_enc_config_set`, and on this libaom that control
    /// returns `AOM_CODEC_INTERNAL_ERROR` for any non-4:2:0 shape -- measured
    /// on four independently built trees
    /// (`~/.cache/aom-oracle`, `aom-affine`, `aom-oracle2`, `aom-oracle3`):
    /// `C422p12` and `C444p12` both die with "Failed to set chroma
    /// subsampling x", while `C420p12` and the 8-bit `C422`/`C444` both
    /// encode. It is not a 4:2:2-specific gap (12-bit 4:4:4 fails too) and
    /// not a 4:2:0 gap (12-bit 4:2:0 encodes). The same control succeeds at
    /// 8 bits, so the failure is the 12-bit configuration order, and the
    /// workaround is a wrapper that sets the codec config first. ffmpeg's
    /// libaom-av1 does exactly that:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=160x128:rate=1:duration=3" \
    ///        -pix_fmt yuv422p12le -c:v libaom-av1 -profile:v 2 -b:v 0 -crf 30 \
    ///        -cpu-used 8 -auto-alt-ref 0 -lag-in-frames 0 \
    ///        -strict experimental -f obu s422_12bit_160x128.obu
    /// ```
    ///
    /// That is libaom's encoder either way -- ffmpeg links it and calls
    /// `aom_codec_encode` -- and the oracle side is ffmpeg's own
    /// `yuv422p12le` rawvideo decode of the same bytes, so the pin and the
    /// oracle come from the same library, which is the property that makes
    /// byte-exactness a claim about OUR decoder rather than about a
    /// disagreement between two encoders.
    ///
    /// `s422_superres_160x128.obu` -- 5304 bytes, sha256
    /// `5852ff2c273cb773c4886762c6a6f2cce6e12065a690eb3a5ca2119c0b6f2957`,
    /// fnv1a64 `0x0789d1b2851b94ae`. 160x128, 8-bit, 3 frames, `--superres-
    /// mode=1` (fixed) over a `C422` testsrc2 source, encoded by the
    /// aomenc recipe above plus `--superres-mode=1`. Superres is the one
    /// 4:2:2 path whose output crop is `(round_ss(w,1) x round_ss(h,0))` at
    /// the UPSCALED size, and it had no 4:2:2 cell anywhere.
    #[test]
    fn a_real_422_12bit_and_superres_stream_decode_pixel_exact() {
        const NAME: &str = "a_real_422_12bit_and_superres_stream_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let hbd = crate_pin("s422_12bit_160x128.obu");
        let hbd_stream = read_pin(&hbd, 9214, 0xa62b8e3fe6f60ae5, NAME);
        assert_422_stream_pixel_exact(NAME, &hbd_stream, 160, 128, 3, 12, true);

        let sr = crate_pin("s422_superres_160x128.obu");
        let sr_stream = read_pin(&sr, 5304, 0x0789d1b2851b94ae, NAME);
        assert_422_stream_pixel_exact(NAME, &sr_stream, 160, 128, 3, 8, true);
    }

    /// lane-av1lr422: a libaom-recipe 10-bit 4:2:2 stream with loop
    /// restoration ON, `s422_lr_10bit_200x120.obu` (129409 bytes, sha256
    /// `5d6bc195934709390191c9ea8af167d385552c5b1330e8abd38f76a4416ba751`,
    /// fnv1a64 `0xc5d5489355fdc2df`), 200x120, 8 frames. It was REFUSED by
    /// the sub-8x8 plane-block check before the fix: the refusal was a
    /// parse desync, not a wrong check. The 32x8 / 8x32 rect4 general arm
    /// gathered the chroma dc-sign votes per mi instead of per 4-px chroma
    /// column, which desynced the coefficient contexts. Decodes byte-exact
    /// against ffmpeg libdav1d rawvideo.
    #[test]
    fn a_422_10bit_lr_stream_decodes_pixel_exact() {
        const NAME: &str = "a_422_10bit_lr_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let p = crate_pin("s422_lr_10bit_200x120.obu");
        let stream = read_pin(&p, 129409, 0xc5d5489355fdc2df, NAME);
        assert_422_stream_pixel_exact(NAME, &stream, 200, 120, 8, 10, true);
    }

    /// lane-av1odd440, CELL 1 of 2: `4:2:0 with an ODD luma dimension`, the
    /// cell `lanes/av1formatsweep.report.md` left as declared debt because
    /// "aomenc rounds a 4:2:0 frame's coded size DOWN to even, so no stream
    /// this recipe can produce codes an odd luma dimension".
    ///
    /// The pin, `420_odd65x65_key.obu` (2305 bytes,
    /// sha256 `66986019a561f81130f407f426c4379b25e6e80c2cd9ce47d517b3216e9a03cf`),
    /// is that missing stream, built offline by
    /// `examples/gen_coverage_cells.rs`: a 96x96 card padded to the block
    /// grid, coded by `encode::encode_key_frame_at_size` with a 65x65 frame
    /// header. Provenance: that example, `cargo run -p ec-av1 --example
    /// gen_coverage_cells -- crates/ec-av1/fixtures`; no encoder, no ffmpeg,
    /// no lavfi.
    ///
    /// WHY THE CELL IS NOT A RUBBER STAMP -- the chroma extent is where a
    /// floor shows. A 65-wide 4:2:0 frame has
    /// `ROUND_POWER_OF_TWO(65, 1) = 33` chroma columns (libaom
    /// `av1_common_plane_width`, `av1/common/av1_common_int.h`), not 32, so
    /// the decoded frame's own plane sizes, the rawvideo sample count and
    /// the oracle compare all move if `decode::round_ss` is floored. The
    /// mutation was run: flooring `round_ss` (`decode.rs:697`) turns the
    /// `round_ss` assert below red at 32 chroma columns and the oracle arm
    /// red at a 6273-byte raw against the oracle's 6403. The gate is
    /// therefore not green on a floored decoder, which is the whole content
    /// of this cell.
    ///
    /// The stream decodes PIXEL-EXACT against the instrumented `aomdec`
    /// (6403 raw bytes = 65*65 + 2*33*33, byte-identical), so the cell is a
    /// `y` and not a `–`.
    #[test]
    fn an_odd_luma_420_key_frame_decodes_pixel_exact() {
        const NAME: &str = "an_odd_luma_420_key_frame_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("420_odd65x65_key.obu");
        let stream = read_pin(&path, 2305, 0xf55645c4b139e8f7, NAME);

        // The stream's OWN header, read from its own bytes: an odd luma size
        // at 4:2:0, which is the cell. (Taken from the pin, not from the
        // gate's name -- class `helper-packs-u16-for-an-8-bit-stream`.)
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{NAME}: OBU at byte {pos} does not parse: {e:?}"));
            pos += obu.total_size;
        }
        let seq = probe
            .sequence_header()
            .unwrap_or_else(|| panic!("{NAME}: the pin carries no sequence header"));
        assert_eq!(
            (seq.max_frame_width, seq.max_frame_height),
            (65, 65),
            "{NAME}: the pin must declare an ODD luma size, it declares {}x{}",
            seq.max_frame_width,
            seq.max_frame_height
        );
        assert_eq!(
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            ),
            (1, 1),
            "{NAME}: this cell is an odd luma SIZE at 4:2:0, not another chroma shape"
        );

        // The decoded frame's own extents. This arm needs no oracle, and it
        // is the one a floored `round_ss` turns red.
        let frames = decode_stream(&stream).expect("the odd-luma 4:2:0 pin must decode");
        assert_eq!(frames.len(), 1, "{NAME}: frame count");
        let frame = &frames[0];
        assert_eq!(
            (frame.width, frame.height),
            (65, 65),
            "{NAME}: decoded luma extent"
        );
        assert_eq!(frame.y.len(), 65 * 65, "{NAME}: decoded luma sample count");
        assert_eq!(
            (frame.u.len(), frame.v.len()),
            (33 * 33, 33 * 33),
            "{NAME}: a 65x65 4:2:0 frame has ROUND_POWER_OF_TWO(65,1) = 33 chroma \
             columns and rows -- 32 means the extent was floored"
        );

        if !aomdec_available(NAME) {
            return;
        }
        assert_rawvideo_matches(&path, &stream, NAME, 1, 8);
    }

    /// lane-av1odd440, CELL 2 of 2: the `4:4:0 (ss 0,1)` row, which the same
    /// report called "unmeasured and unproducible by recipe". It is neither
    /// unmeasured nor unproducible: it is NOT A CELL AT ALL.
    ///
    /// `subsampling_y` is coded only when `subsampling_x` is 1 (spec 5.5.2
    /// `color_config`), so a sequence header can carry exactly three chroma
    /// shapes -- 4:2:0 `(1,1)`, 4:2:2 `(1,0)`, 4:4:4 `(0,0)` -- and never
    /// `(0,1)`. libaom says so in both directions: the reader leaves
    /// `subsampling_y = 0` when `subsampling_x` is 0
    /// (`av1_read_color_config`, `av1/decoder/decodeframe.c:4171-4175`) and
    /// the writer ASSERTS it -- `assert(seq_params->subsampling_y == 0 &&
    /// "4:4:0 subsampling not allowed in AV1")`, `av1/encoder/bitstream.c:
    /// 2463-2465`. So there is no stream to decode, no fixture to pin that
    /// a decoder could be handed, and no oracle to compare against: a
    /// 4:4:0 row in a coverage matrix is a matrix bug, not a decoder gap.
    ///
    /// What is pinned, then, is the claim itself, in three arms that a
    /// mutation turns red:
    ///
    /// 1. WRITER: every `(profile, bit_depth, mono, colour description,
    ///    requested subsampling)` combination a `color_config` can carry is
    ///    written by this crate's own writer and read back by this crate's
    ///    own reader. The reachable set is asserted to be EXACTLY
    ///    `{(1,1), (1,0), (0,0)}` -- each legal shape present, `(0,1)`
    ///    absent -- and, on the BYTES rather than on the round trip, the two
    ///    requests that differ only in `subsampling_y` must write identical
    ///    headers whenever `subsampling_x` is 0. A sweep that silently wrote
    ///    nothing would pass, so the count of combinations actually written
    ///    and read is asserted too.
    /// 2. READER: the one byte that carries `subsampling_x` is found by
    ///    diffing an `(0,0)` header against an `(1,0)` one, and then all 256
    ///    values of that byte are fed back through the reader. Not one of the
    ///    headers that parse reads back `(0,1)`, because the reader never
    ///    reads a `subsampling_y` bit in that position. A reader that always
    ///    read a second bit would produce `(0,1)` at the one value that sets
    ///    both bits.
    /// 3. THE PIN: `440_request_is_422.obu` (2014 bytes, sha256
    ///    `4afaa5470c4e010d9d970086ef514680adfe801a579e4deeec582f52d13c5e7c`,
    ///    fnv1a64 `0x98a1378df976253d`) is what this crate's writer produces
    ///    when it is handed `subsampling (0,1)` at profile 2 -- profile 2
    ///    below 12 bits is 4:2:2 and codes no subsampling bit at all, so the
    ///    request lands on `(1,0)`. The pin is asserted to read back `(1,0)`,
    ///    NOT `(0,1)`. Its frame OBU is a 4:2:0 key frame's, so the pin is a
    ///    HEADER witness, not a decodable 4:2:2 stream -- nothing here claims
    ///    pixels for it. Provenance: `examples/gen_coverage_cells.rs`, `cargo
    ///    run -p ec-av1 --example gen_coverage_cells -- crates/ec-av1/fixtures`.
    ///
    ///    lane-av1422lift: this arm used to end "and the decoder is asserted
    ///    to refuse it by name, which is the honest outcome for the shape a
    ///    4:4:0 request actually yields". That was honest only because the
    ///    decoder refused EVERY (1,0) header -- the assertion was about the
    ///    guard, not about these bytes. With 4:2:2 admitted, refusing these
    ///    bytes would be wrong and accepting them without comment would be
    ///    worse, so the arm now asserts the one thing that IS true of them:
    ///    the SEQUENCE HEADER, not the tile, fixes the decode's chroma
    ///    geometry. The pixel claim moved to genuinely 4:2:2 pins.
    #[test]
    fn the_440_cell_is_not_a_codable_chroma_shape() {
        const NAME: &str = "the_440_cell_is_not_a_codable_chroma_shape";
        let _gate_lock = lock_gate_counters();
        let header_obu = |profile: u8, color: ec_av1_syntax::ColorConfig| -> Vec<u8> {
            let (mut seq, _header) = crate::encode::key_frame_headers_colour(64, 64, 100, color)
                .expect("the header builders accept a colour config");
            seq.seq_profile = profile;
            crate::sequence::sequence_header_obu(&seq).expect("writing the sequence header")
        };
        let shape_of = |obu: &[u8]| -> Option<(u8, u8)> {
            let mut parser = Av1Parser::new();
            let parsed = parser.parse_obu(obu).ok()?;
            if !matches!(parsed.kind, ec_av1_syntax::ObuKind::SequenceHeader(_)) {
                return None;
            }
            let seq = parser.sequence_header()?;
            Some((
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y,
            ))
        };

        // ---- arm 1: the writer's whole reachable space --------------------
        let mut reachable: std::collections::BTreeSet<(u8, u8)> = Default::default();
        let mut asked = 0usize;
        for profile in [0u8, 1, 2] {
            for bit_depth in [8u8, 10, 12] {
                for mono_chrome in [false, true] {
                    // Profile 1 is 4:4:4 and codes no mono bit; asking for
                    // monochrome there is refused by the writer, which is
                    // not a chroma shape and not this gate's subject.
                    if mono_chrome && profile == 1 {
                        continue;
                    }
                    for (label, (cp, tc, mc)) in [
                        ("unspecified", (2u8, 2u8, 2u8)),
                        ("bt709", (1u8, 1u8, 1u8)),
                        ("srgb", (1u8, 13u8, 0u8)),
                    ] {
                        for sx in [0u8, 1] {
                            for sy in [0u8, 1] {
                                let color = ec_av1_syntax::ColorConfig {
                                    bit_depth,
                                    mono_chrome,
                                    num_planes: if mono_chrome { 1 } else { 3 },
                                    color_primaries: cp,
                                    transfer_characteristics: tc,
                                    matrix_coefficients: mc,
                                    color_range: false,
                                    subsampling_x: sx,
                                    subsampling_y: sy,
                                    chroma_sample_position:
                                        ec_av1_syntax::ChromaSamplePosition::Unknown,
                                    separate_uv_delta_q: false,
                                };
                                asked += 1;
                                let obu = header_obu(profile, color);
                                // The WRITER half of the claim, measured on the
                                // bytes rather than on a round trip: with
                                // `subsampling_x == 0` the spec codes no
                                // `subsampling_y` bit at all, so the two
                                // requests that differ ONLY in `subsampling_y`
                                // must produce byte-identical headers. A
                                // writer that emitted the bit anyway (the
                                // mutation libaom's own `assert` exists to
                                // catch, av1/encoder/bitstream.c:2467-2468) fails
                                // here -- and note it does NOT fail the
                                // round trip, because the reader then absorbs
                                // the extra bit as `separate_uv_delta_q`.
                                if sx == 0 {
                                    let mut other = color;
                                    other.subsampling_y = 1;
                                    assert_eq!(
                                        obu,
                                        header_obu(profile, other),
                                        "{NAME}: a (0,0) and a (0,1) request wrote DIFFERENT \
                                         bytes at profile {profile}, {bit_depth}-bit, mono \
                                         {mono_chrome}, {label} -- `subsampling_y` must not be \
                                         coded at all when `subsampling_x` is 0"
                                    );
                                }
                                if let Some(got) = shape_of(&obu) {
                                    assert_ne!(
                                        got,
                                        (0, 1),
                                        "{NAME}: a 4:4:0 shape came back out of the writer at \
                                         profile {profile}, {bit_depth}-bit, mono {mono_chrome}, \
                                         {label}, asked for ({sx},{sy}) -- libaom forbids this \
                                         shape outright (av1/encoder/bitstream.c:2467-2468)"
                                    );
                                    reachable.insert(got);
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            asked >= 100 && reachable.len() == 3,
            "{NAME}: the sweep asked {asked} combinations and reached {} shapes -- it must reach \
             all three legal ones, or it is not measuring the space",
            reachable.len()
        );
        assert_eq!(
            reachable,
            std::collections::BTreeSet::from([(0u8, 0u8), (1u8, 0u8), (1u8, 1u8)]),
            "{NAME}: the chroma shapes a `color_config` can code are exactly 4:4:4 (0,0), 4:2:2 \
             (1,0) and 4:2:0 (1,1)"
        );

        // ---- arm 2: the reader's bit at that position ----------------------
        // Profile 2 at 12 bits is the only place BOTH subsampling bits are
        // coded, so it is where a hand-built header could try to smuggle a
        // `subsampling_y`. Find the byte that carries `subsampling_x` by
        // diffing the two shapes, then read back all eight of its values.
        let at_444 = |sx: u8| {
            let color = ec_av1_syntax::ColorConfig {
                bit_depth: 12,
                subsampling_x: sx,
                subsampling_y: 0,
                chroma_sample_position: ec_av1_syntax::ChromaSamplePosition::Unknown,
                ..ec_av1_syntax::ColorConfig::default()
            };
            let (mut seq, _h) =
                crate::encode::key_frame_headers_colour(64, 64, 100, color).unwrap();
            seq.seq_profile = 2;
            let obu = crate::sequence::sequence_header_obu(&seq).unwrap();
            let mut parser = Av1Parser::new();
            parser.parse_obu(&obu).expect("the profile-2 header parses");
            let seq = parser.sequence_header().expect("sequence header");
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y,
            )
        };
        assert_eq!(at_444(0), (0, 0), "{NAME}: the sx=0 header is 4:4:4");
        assert_eq!(at_444(1), (1, 0), "{NAME}: the sx=1 header is 4:2:2");
        let base = {
            let color = ec_av1_syntax::ColorConfig {
                bit_depth: 12,
                subsampling_x: 0,
                subsampling_y: 0,
                chroma_sample_position: ec_av1_syntax::ChromaSamplePosition::Unknown,
                ..ec_av1_syntax::ColorConfig::default()
            };
            let (mut seq, _h) =
                crate::encode::key_frame_headers_colour(64, 64, 100, color).unwrap();
            seq.seq_profile = 2;
            crate::sequence::sequence_header_obu(&seq).unwrap()
        };
        let other = {
            let color = ec_av1_syntax::ColorConfig {
                bit_depth: 12,
                subsampling_x: 1,
                subsampling_y: 0,
                chroma_sample_position: ec_av1_syntax::ChromaSamplePosition::Unknown,
                ..ec_av1_syntax::ColorConfig::default()
            };
            let (mut seq, _h) =
                crate::encode::key_frame_headers_colour(64, 64, 100, color).unwrap();
            seq.seq_profile = 2;
            crate::sequence::sequence_header_obu(&seq).unwrap()
        };
        assert_eq!(
            base.len(),
            other.len(),
            "{NAME}: the two headers must differ in place"
        );
        let byte_at = base
            .iter()
            .zip(&other)
            .position(|(a, b)| a != b)
            .expect("the two shapes must differ somewhere");
        // Every value of that byte is fed back through the reader -- it is the
        // LAST payload byte, so the bits a smuggled `subsampling_y` would come
        // from are in it too (spec 5.5.2 always codes `separate_uv_delta_q`
        // after the subsampling pair, and the payload is padded out to the
        // byte with `trailing_bits`). A header that stops parsing is not a
        // 4:4:0 shape and cannot be counted as one; the sweep asserts how
        // many DID parse, so a sweep that quietly parsed nothing is not
        // evidence of anything.
        let mut parsed_headers = 0usize;
        for value in 0..256u16 {
            let mut poked = base.clone();
            poked[byte_at] = value as u8;
            let mut parser = Av1Parser::new();
            let Ok(parsed) = parser.parse_obu(&poked) else {
                continue;
            };
            if !matches!(parsed.kind, ec_av1_syntax::ObuKind::SequenceHeader(_)) {
                continue;
            }
            let seq = parser.sequence_header().expect("sequence header");
            parsed_headers += 1;
            assert_ne!(
                (
                    seq.color_config.subsampling_x,
                    seq.color_config.subsampling_y
                ),
                (0, 1),
                "{NAME}: payload byte {byte_at} = {value:#04x} read back as 4:4:0 -- the reader \
                 took a `subsampling_y` bit that spec 5.5.2 does not code when `subsampling_x` \
                 is 0"
            );
        }
        assert!(
            parsed_headers > 100,
            "{NAME}: only {parsed_headers} of the 256 poked headers parsed -- the sweep is not \
             measuring the reader"
        );

        // ---- arm 3: the pinned bytes a 4:4:0 request produces --------------
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("440_request_is_422.obu");
        let pinned = read_pin(&path, 2014, 0x98a1378df976253d, NAME);
        let mut parser = Av1Parser::new();
        let obu = parser
            .parse_obu(&pinned)
            .unwrap_or_else(|e| panic!("{NAME}: the pin's first OBU does not parse: {e:?}"));
        assert!(
            matches!(obu.kind, ec_av1_syntax::ObuKind::SequenceHeader(_)),
            "{NAME}: the pin must start with its sequence header"
        );
        let seq = parser.sequence_header().expect("the pin's sequence header");
        assert_eq!(
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            ),
            (1, 0),
            "{NAME}: the pin is what a 4:4:0 request at profile 2 produces -- 4:2:2 (1,0). If this \
             ever reads (0,1), a 4:4:0 cell became codable and this whole gate is wrong about the \
             format."
        );
        // lane-av1unwritten: lane-av1422lift replaced the old 4:2:2 header
        // refusal with "it decodes, and the decoded chroma planes are 32x64".
        // That was the wrong contract for THIS pin: its frame OBU is a 4:2:0
        // key frame's, so at 4:2:2 its partition symbols name subsizes with no
        // chroma plane block, which libaom rejects as a corrupt frame
        // ("Block size 8x16 invalid with this subsampling mode",
        // decodeframe.c:1456) and `aomdec` does. The replacement is the
        // REFUSAL, by that rule.
        //
        // The pin's remaining job is unchanged and still worth saying: it
        // proves a 4:4:0 REQUEST at profile 2 lands on 4:2:2 (1,0), i.e. that
        // `the_440_cell_is_not_a_codable_chroma_shape`'s claim is still true.
        let err = decode_stream(&pinned).err().unwrap_or_else(|| {
            panic!(
                "{NAME}: the pin is a 4:2:2 header over a 4:2:0 tile -- libaom refuses it \
                     (aomdec: \"Block size 8x16 invalid with this subsampling mode\"), and so \
                     must we"
            )
        });
        assert!(
            err.to_string()
                .contains("invalid with this subsampling mode"),
            "{NAME}: the refusal must name libaom's rule, got: {err}"
        );
        // The pixels this pin never had: the sibling gate
        // `a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt`
        // pins the refusal plus a genuine 4:2:2 positive control, and every
        // real 4:2:2 exactness claim lives on genuinely 4:2:2 pins in this
        // file.
    }

    /// lane-av1unwritten: this pin is a MISSING REFUSAL, not a pixel defect.
    ///
    /// `440_request_is_422.obu` is a 4:2:2 sequence header wrapped around a
    /// 4:2:0 key frame's tile, so the tile's partition symbols make subsizes
    /// (8x16 among them) that have NO chroma plane block at ss (1,0). libaom
    /// refuses exactly that, before descending into the subsize:
    ///
    /// ```text
    /// $ aomdec --rawvideo -o /dev/null fixtures/440_request_is_422.obu
    /// Warning: Failed to decode frame 1: Corrupt frame detected
    /// Warning: Additional information: Block size 8x16 invalid with this subsampling mode
    /// ```
    ///
    /// (`av1/decoder/decodeframe.c:1449-1458`,
    /// `get_plane_block_size(subsize, ss_x, ss_y) == BLOCK_INVALID` ->
    /// `AOM_CODEC_CORRUPT_FRAME`.) We walked those subsizes instead, and the
    /// first version of this lane misread the downstream symptom -- a chroma
    /// plane block covered by a square unit, whole 4x4 blocks left unwritten,
    /// and a `EC_AV1_FINAL_DUMP` hash that CHANGED RUN TO RUN because the
    /// unwritten samples were uninitialised plane memory.
    ///
    /// The contract pinned here is the one libaom has: the pin REFUSES, by a
    /// named error that says which shape and which subsampling mode. The
    /// positive control is a genuinely 4:2:2 pin, which must still decode --
    /// so this gate cannot pass by refusing everything.
    #[test]
    fn a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt() {
        const NAME: &str = "a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt";
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("440_request_is_422.obu");
        let pinned = read_pin(&path, 2014, 0x98a1378df976253d, NAME);
        // The refusal this gate PROVES, quoted whole -- the anchor the PROVEN
        // row in `refusal_inventory.rs` is checked against inside this body's
        // own bounds. The assertion below used to match only the libaom
        // citation *inside* the string, so a reworded guard kept passing for
        // as long as it kept citing `decodeframe.c:1456`: the half that names
        // OUR shapes (4x8 / 8x16 / 16x4, and 8x4 at 4:4:0) was never pinned.
        const REFUSAL: &str = "a block size 4x8, 8x16 or 16x4 (or 8x4 at 4:4:0) has no chroma \
                               plane block at this frame's subsampling mode (libaom: \"Block \
                               size %dx%d invalid with this subsampling mode\", \
                               av1/decoder/decodeframe.c:1456, refusing by the same rule)";
        let err = decode_stream(&pinned)
            .err()
            .unwrap_or_else(|| panic!("{NAME}: the pin must be REFUSED, not decoded"));
        let msg = err.to_string();
        assert!(
            msg.contains("invalid with this subsampling mode"),
            "{NAME}: the refusal must name the rule it refuses by (libaom's own wording), got: \
             {msg}"
        );
        assert!(
            msg.contains(REFUSAL),
            "{NAME}: the refusal must be this decoder's OWN string, whole -- the guard's wording \
             drifted, so the PROVEN row no longer names what fires, got: {msg}"
        );
        // Positive control: a real 4:2:2 stream must still decode, so the gate
        // cannot pass by refusing everything.
        let ok_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("422_key_64x64.obu");
        let ok = decode_stream(&std::fs::read(&ok_path).expect("the 4:2:2 control fixture"))
            .unwrap_or_else(|e| panic!("{NAME}: a genuine 4:2:2 pin must still decode: {e}"));
        assert!(
            !ok.is_empty(),
            "{NAME}: the 4:2:2 control decoded no frames"
        );
    }

    /// lane-av1subsizesweep: the FRAME-EDGE walk of a BLOCK_INVALID subsize,
    /// pinned. This is the one install site the partition-symbol guards missed,
    /// and the sibling pin above cannot see it -- that pin's 8x16 comes from a
    /// 16x16-level `PARTITION_VERT_B` (an interior AB strip, refused at
    /// decode.rs:36472), while this pin's comes from the key-frame 16-level
    /// FRAME-EDGE arm, which is not a `dec.symbol(` site at all: it reads one
    /// gathered bit with `dec.symbol_fixed(&gather(..))` and dispatches
    /// `decode_leaf_rect` straight off it.
    ///
    /// `422_header_edge16_walk.obu` (128 bytes, sha256
    /// `e8ab2bb229896f796cdec4d2cb383b41f057ef8d364da30f160a3f55a452657f`,
    /// fnv1a64 `0xcfa2394ebe1f7764`) is a 4:2:2 sequence header over a 4:2:0
    /// key frame's tile, at 88x64. Width 88 is 8 mod 16, so the last 16x16
    /// column block is COLUMN-short and the frame-edge arm fires; height 64 is
    /// 16-aligned, so the ROW is not short and the both-cut arm is not taken.
    /// The gathered bit names `PARTITION_VERT`, so the strip is `BLOCK_8X16`.
    ///
    /// libaom's own verdict, which is the rule this pin pins:
    ///
    /// ```text
    /// $ aomdec --rawvideo -o /dev/null fixtures/422_header_edge16_walk.obu
    /// Warning: Failed to decode frame 1: Corrupt frame detected
    /// Warning: Additional information: Block size 8x16 invalid with this subsampling mode
    /// ```
    ///
    /// What makes this pin different from every other refusal gate in this file
    /// is that the 8x16 is the ONLY offending subsize in the stream. Measured
    /// over a 260-candidate sweep (4 card textures x 5 quantizers x 13 declared
    /// widths), this is the one candidate whose decode with the guard removed
    /// SUCCEEDS: with the `refuse_invalid_subsize` at decode.rs:37098 deleted,
    /// this decoder returned a full 88x64 picture where libaom refuses the
    /// frame. So the guard is not a redundant second refusal here -- it is the
    /// only thing between a corrupt frame and a plausible-looking decode.
    #[test]
    fn a_422_header_whose_only_invalid_subsize_is_the_16_level_frame_edge_walk_is_refused() {
        const NAME: &str =
            "a_422_header_whose_only_invalid_subsize_is_the_16_level_frame_edge_walk_is_refused";
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("422_header_edge16_walk.obu");
        let pinned = read_pin(&path, 128, 0xcfa2394ebe1f7764, NAME);
        // The shape must be the one this lane's arm carries: if the pin ever
        // stops being an 8x16 strip the refusal is coming from somewhere else
        // and this gate is measuring the wrong thing.
        assert!(
            !crate::decode::chroma_plane_block_codable(8, 16, 1, 0),
            "{NAME}: BLOCK_8X16 must be BLOCK_INVALID at 4:2:2 -- this gate's whole \
             subject is `av1_ss_size_lookup[BLOCK_8X16][1][0] == BLOCK_INVALID`"
        );
        assert!(
            crate::decode::chroma_plane_block_codable(16, 8, 1, 0),
            "{NAME}: the 16x8 mirror MUST stay legal at 4:2:2 (BLOCK_8X8) -- it is what \
             keeps the new guard from refusing the sibling arm's strip"
        );
        let err = decode_stream(&pinned)
            .err()
            .unwrap_or_else(|| panic!("{NAME}: the pin must be REFUSED, not decoded"));
        let msg = err.to_string();
        assert!(
            msg.contains("invalid with this subsampling mode"),
            "{NAME}: the refusal must name libaom's rule (its own wording), got: {msg}"
        );
        // Positive control: a genuine 4:2:2 stream must still decode, so this
        // gate cannot pass by refusing everything.
        let ok_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("422_key_64x64.obu");
        let ok = decode_stream(&std::fs::read(&ok_path).expect("the 4:2:2 control fixture"))
            .unwrap_or_else(|e| panic!("{NAME}: a genuine 4:2:2 pin must still decode: {e}"));
        assert!(
            !ok.is_empty(),
            "{NAME}: the 4:2:2 control decoded no frames"
        );
    }

    /// lane-av1-422bigblock: the two witnessed 4:2:2 big-block defects, pinned
    /// as fixtures (`git add -f`; `fixtures/` is gitignored).
    ///
    /// `422_allskip_2f.obu` (62 bytes) is a 128x128 profile-2 key frame plus
    /// its all-skip first inter frame: the pre-port decoder read phantom
    /// coefficient units in the inter frame and then panicked in
    /// `reconstruct_mc_rect` on a full-height 4:2:2 chroma rect. At this
    /// lane's HEAD the block-half port's per-axis plane-block model already
    /// has it entropy-locked (all 20 key-frame coefficient units pair
    /// rng-for-rng with the instrumented oracle, the inter frame reads
    /// zero, matching the oracle) and pixel-exact vs aomdec.
    ///
    /// `422_sb128_3f.obu` (12543 bytes) carries the f2 128x128
    /// `PARTITION_NONE` non-skip inter block: the mu-chunk chroma walk cut
    /// each chunk's 32x64 chroma into a `chroma_side`-square 64x64 (twice
    /// the units the oracle codes), indexed the prediction buffer at the
    /// square `chroma_side` instead of `chroma_stride` (the empty-slice
    /// panic at `push_mc_rect_tx`'s densify), and the reference plane
    /// resolution guessed every non-4:4:4 picture quarter-height, so the
    /// bottom mu-chunk row predicted from a clamped row-63 edge. All fixed;
    /// both witnesses decode pixel-exact vs aomdec on the lane's local
    /// `EC_AV1_ALLOW_422_PROBE` patch-run-restore build (recipe and measured
    /// numbers in `lanes/av1422bigblock.report.md`).
    ///
    /// lane-av1422lift: `decode_stream` no longer refuses 4:2:2 at the
    /// sequence header, so this gate stops being a refusal contract and
    /// becomes the one it should have been: BOTH pins, byte-pinned AND
    /// byte-exact against ffmpeg, with the per-plane coefficient-unit census
    /// as the non-vacuity arm. The pins are unchanged and still load-bearing
    /// (size + fnv1a64); only the assertion moved.
    #[test]
    fn the_pinned_422_bigblock_witnesses_decode_pixel_exact() {
        const NAME: &str = "the_pinned_422_bigblock_witnesses_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        for (file, bytes, fp, frames, require_coded) in [
            (
                "422_allskip_2f.obu",
                62usize,
                0x7e4d69d3c728c55f_u64,
                2usize,
                false,
            ),
            ("422_sb128_3f.obu", 12543, 0x5171e0aab8000da7, 3, true),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(file);
            let data = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                    path.display()
                )
            });
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fp, "{NAME}: {file} bytes drifted");
            let (_units, coded_delta) =
                assert_422_stream_pixel_exact(NAME, &data, 128, 128, frames, 8, require_coded);
            if !require_coded {
                // `422_allskip_2f.obu` is named for what it does NOT code, and
                // the pre-port defect was exactly that: the decoder read
                // PHANTOM coefficient units in the all-skip inter frame and
                // then panicked in `reconstruct_mc_rect` on a full-height
                // 4:2:2 chroma rect. So the cell's non-vacuity is a NEGATIVE
                // measurement on the CHROMA planes, asserted here rather than
                // inherited: the walk ran (units > 0, which the shared body
                // checks) and coded nothing on U or V.
                assert_eq!(
                    (coded_delta[1], coded_delta[2]),
                    (0, 0),
                    "{NAME}: {file} CODED a chroma coefficient -- this is the all-skip inter-frame \
                     cell and a phantom residual read is exactly the defect it witnesses"
                );
            }
        }
    }

    /// lane-av1muchroma: the `chroma_side * chroma_side` mu-chroma site
    /// census, over the whole committed 4:2:2 corpus, as a per-site
    /// REACHABILITY measurement with a non-vacuity arm.
    ///
    /// `lanes/av1subsizesweep.report.md` named "mu-chroma sites using
    /// `chroma_side * chroma_side`" as unprobed residue. This lane's sweep
    /// (env-gated probe at each of the 12 sites, run over every pinned 4:2:2
    /// gate) measured them:
    ///
    /// - sites 4, 7, 10 are guarded `ss_x == 0 && ss_y == 0`, i.e. 4:4:4
    ///   ONLY -- structurally unreachable at 4:2:2, not merely unwitnessed.
    /// - sites 2, 8 (`side > 64` mu-chunk walks) were not reached by any
    ///   committed 4:2:2 fixture; their single-reference twin, site 5, is
    ///   (side 128, plane 64x128), and its square allocation is the ENCLOSING
    ///   square, so it is 2x oversized, never short.
    /// - sites 1, 3, 5, 6, 9, 11, 12 ARE reached. For 3/5/6/9/11/12
    ///   `chroma_side` is `max(side >> ss_x, side >> ss_y)` (decode.rs), the
    ///   ENCLOSING square of the plane rect, so at 4:2:2 the square is 2x
    ///   the per-axis extent: oversized, never short.
    /// - site 1 alone still used `side >> ss_x` and was therefore sized at
    ///   HALF the plane block at 4:2:2. Fixed per-axis; this gate pins it.
    ///
    /// Non-vacuity: [`crate::decode::skip_arm_chroma_plane_block_hits`]
    /// fires only where the per-axis and square forms differ, so a green run
    /// that never reached the site would be vacuous and this asserts > 0.
    #[test]
    fn the_422_skip_arm_chroma_grid_is_sized_per_axis_not_square() {
        const NAME: &str = "the_422_skip_arm_chroma_grid_is_sized_per_axis_not_square";
        const FILE: &str = "s422_322x240.obu";
        const BYTES: usize = 21096;
        const FP: u64 = 0x9a96dda2a19d47a6;
        const W: usize = 322;
        const H: usize = 240;
        const FRAMES: usize = 16;
        let _gate_lock = lock_gate_counters();
        let data = std::fs::read(crate_pin(FILE)).unwrap_or_else(|e| {
            panic!("{NAME}: the committed pin {FILE} is missing ({e}) -- a failure, not a skip")
        });
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FP, "{NAME}: {FILE} bytes drifted");
        let rect_before = crate::decode::skip_arm_chroma_rect_shapes();
        let short_before = crate::decode::skip_arm_chroma_short_samples();
        let ours = decode_stream(&data)
            .unwrap_or_else(|e| panic!("{NAME}: decode_stream refused a real 4:2:2 stream: {e}"));
        assert_eq!(ours.len(), FRAMES, "{NAME}: decoded frame count");
        for (i, f) in ours.iter().enumerate() {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: frame {i} dimensions");
            assert_eq!(
                (f.u.len(), f.v.len()),
                (W.div_ceil(2) * H, W.div_ceil(2) * H),
                "{NAME}: frame {i} chroma planes are {}/{} samples -- 4:2:2 is HALF WIDTH and \
                 FULL HEIGHT; a quarter-height plane means the decode kept a 4:2:0 allocation",
                f.u.len(),
                f.v.len()
            );
        }
        let rect_after = crate::decode::skip_arm_chroma_rect_shapes();
        assert!(
            rect_after > rect_before,
            "{NAME}: no skipped block decoded at a rect chroma shape ({rect_before} -> \
             {rect_after}) -- this gate is then VACUOUS: per-axis and square sizing are the \
             same number at 4:2:0 and 4:4:4, so it proves nothing about the site it claims"
        );
        let short_after = crate::decode::skip_arm_chroma_short_samples();
        assert_eq!(
            short_after, short_before,
            "{NAME}: decode_block's SKIP arm chroma grids fell {short_after} samples SHORT of \
             their own plane block -- the square `side >> ss_x` form this lane removed allocates \
             exactly half the plane block at 4:2:2 (ss (1,0))"
        );
    }

    /// lane-av1floorshift: the two FRAME chroma plane allocators
    /// (`decode_key_frame_tile_with_cdfs`, `decode_inter_tile`) size U and V as
    /// `(width >> ss_x) x (height >> ss_y)`. `lanes/av1subsizesweep.report.md`
    /// named them "floor-`>>` chroma plane allocators -- shape assumption at
    /// frame edges; not probed" and left them open. This is the probe.
    ///
    /// **The verdict is ALREADY CORRECT, and it is structural, not
    /// corpus-luck.** `width`/`height` are `block_grid(mi_cols, mi_rows) *
    /// BLOCK` (`BLOCK == 32`) and the allocator is handed those, not the frame's
    /// own `true_width`/`true_height`: both axes are multiples of 32, and
    /// `ss_x`/`ss_y` are the sequence header's single-bit subsampling fields,
    /// so `ss <= 1` on every stream that exists. `32k >> ss == round_ss(32k, ss)`
    /// for every `k` and every `ss <= 1` -- the `>>` can never floor one sample
    /// short of libaom's own chroma crop, at a frame edge or anywhere else. The
    /// static half of the gate below proves that exhaustively over every
    /// `mi_cols`/`mi_rows` this decoder admits.
    ///
    /// The corpus half keeps the instrument honest: it decodes one frame per
    /// selectable subsampling cell and asserts the allocators were REACHED
    /// (`frame_chroma_alloc_calls` moved), so a green run cannot be the census
    /// having been wired to nothing.
    ///
    /// Witnesses, all byte-pinned (`size`, `fnv1a64`), chosen to cover all
    /// three selectable cells and both allocators:
    ///
    /// | file | cell | frame | covers |
    /// |---|---|---|---|
    /// | `420_odd65x65_key.obu` | 4:2:0, **odd** 65x65 | key | key allocator, the odd-dimension cell where a floor WOULD show |
    /// | `422_key_64x64.obu` | 4:2:2 (1,0) | key | key allocator, half-width/full-height |
    /// | `444_intrabc_rect4_witness.obu` | 4:4:4 (0,0) | key | key allocator, the `ss == 0` cell where `>> ss` is the identity |
    ///
    /// 4:4:0 (`0,1`) is excluded and cannot be added: libaom's
    /// `av1_read_color_config` asserts `subsampling_y == 0` when
    /// `subsampling_x == 0`, and no of 256 header bytes reads back `(0,1)` --
    /// the reader census at `the_reader_never_produces_440` names it.
    #[test]
    fn the_frame_chroma_planes_are_allocated_on_the_per_axis_floor_that_never_rounds() {
        const NAME: &str =
            "the_frame_chroma_planes_are_allocated_on_the_per_axis_floor_that_never_rounds";
        let _gate_lock = lock_gate_counters();
        for (file, bytes, fp, cell, ss) in [
            (
                "420_odd65x65_key.obu",
                2305usize,
                0xf55645c4b139e8f7u64,
                "4:2:0",
                (1usize, 1usize),
            ),
            (
                "422_key_64x64.obu",
                755,
                0x87fc569cf53bcbc6,
                "4:2:2",
                (1, 0),
            ),
            (
                "444_intrabc_rect4_witness.obu",
                1016,
                0x27c2fad540472994,
                "4:4:4",
                (0, 0),
            ),
        ] {
            let data = std::fs::read(crate_pin(file))
                .unwrap_or_else(|e| panic!("{NAME}: the committed pin {file} is missing ({e})"));
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fp, "{NAME}: {file} bytes drifted");
            let calls_before = crate::decode::frame_chroma_alloc_calls();
            let short_before = crate::decode::frame_chroma_alloc_short_samples();
            let crop_before = crate::decode::frame_chroma_alloc_crop_shapes();
            let frames = decode_stream(&data)
                .unwrap_or_else(|e| panic!("{NAME}: {file} ({cell}) refused: {e}"));
            let calls = crate::decode::frame_chroma_alloc_calls() - calls_before;
            assert!(
                calls >= frames.len(),
                "{NAME}: {file} ({cell}) decoded {} frames but the chroma plane allocator ran \
                 {calls} times -- the instrument is not on the path this gate claims to cover",
                frames.len()
            );
            // The shape the allocator owes the tile, and the shape the output
            // crop owes the caller: libaom's chroma CROP of the frame's own
            // dimensions (`av1_round_shift`), which is what
            // `decode_key_frame_tile_with_cdfs`'s `Picture` crop uses. At the
            // odd 65x65 4:2:0 cell this is 33x33 -- 1089 samples, NOT the
            // floor 32x32 an `>>` short of the true extent would have left.
            for (i, f) in frames.iter().enumerate() {
                let (cw, ch) = (
                    crate::decode::round_ss(f.width, ss.0),
                    crate::decode::round_ss(f.height, ss.1),
                );
                assert_eq!(
                    (f.u.len(), f.v.len()),
                    (cw * ch, cw * ch),
                    "{NAME}: {file} ({cell}) frame {i} is {}x{} and its chroma planes are {} / {} \
                     samples; {cell} is a {cw}x{ch} = {} crop",
                    f.width,
                    f.height,
                    f.u.len(),
                    f.v.len(),
                    cw * ch
                );
            }
            let short = crate::decode::frame_chroma_alloc_short_samples() - short_before;
            assert_eq!(
                short, 0,
                "{NAME}: {file} ({cell}) allocated {short} chroma samples SHORT of the `round_ss` \
                 extent the tile's chroma walks may address -- the frame chroma planes are sized \
                 by a FLOOR `>> ss` that does not round"
            );
            let crop = crate::decode::frame_chroma_alloc_crop_shapes() - crop_before;
            assert_eq!(
                crop, 0,
                "{NAME}: {file} ({cell}) reached a frame shape on which `>> ss` and `round_ss` \
                 disagree ({crop} frames) -- the allocators are handed a MULTIPLE OF 32 \
                 (block_grid * BLOCK) and `ss <= 1`, so this can only mean the allocator's own \
                 argument changed, and the shortfall assertion above is then vacuous on this file"
            );
        }
    }

    /// lane-av1floorshift: the STATIC half of the same claim, and the part that
    /// does not need a corpus. The frame chroma allocators' shift argument is
    /// `block_grid(mi_cols, mi_rows) * BLOCK` with `BLOCK == 32`, so for every
    /// `mi_cols`/`mi_rows` this decoder can admit and every subsampling the
    /// header can carry (`ss in {0, 1}` per axis), `>> ss` and `round_ss` are
    /// the same number. Exhaustively checked here over a range wider than any
    /// frame a header can declare; the two counters above are zero for a
    /// reason, and this is the reason.
    #[test]
    fn the_frame_chroma_alloc_shift_argument_cannot_floor_below_round_ss() {
        const NAME: &str = "the_frame_chroma_alloc_shift_argument_cannot_floor_below_round_ss";
        const BLOCK: usize = 32;
        // `block_grid(mi) = mi.div_ceil(BLOCK_MI)` with `BLOCK_MI == 8`
        // (`tile.rs`), so the allocator's width is `(mi.div_ceil(8)) * 32`.
        for mi in 1..=512usize {
            let coded = mi.div_ceil(8) * BLOCK;
            for ss in 0..=1usize {
                assert_eq!(
                    coded >> ss,
                    crate::decode::round_ss(coded, ss),
                    "{NAME}: coded {coded} (mi {mi}) at ss {ss}: the floor and the crop differ -- \
                     the frame chroma plane would be allocated short"
                );
                // The TRUE extent is `mi * 4`; it is what the allocator
                // derives `true_width` from and what every chroma walk is
                // clamped against, so it earns the same check.
                let truth = mi * 4;
                assert_eq!(
                    truth >> ss,
                    crate::decode::round_ss(truth, ss),
                    "{NAME}: true {truth} (mi {mi}) at ss {ss}: the floor and the crop differ"
                );
            }
        }
        // And the 4:4:0 cell libaom refuses, spelled out rather than left as
        // an assumption: at `ss (0,1)` the shift is exact too, so the site
        // would be correct even if the cell were ever admitted.
        for mi in 1..=64usize {
            assert_eq!((mi * 4) >> 1, crate::decode::round_ss(mi * 4, 1));
            assert_eq!(mi * 4, crate::decode::round_ss(mi * 4, 0));
        }
    }

    /// lane-av1422tailskip: the WITNESS cell for the 4:2:2 intra-in-inter
    /// PALETTE chroma per-unit window, and its byte pin.
    ///
    /// `422_palette_intra_in_inter_384x240_17f.obu` (19344 B, sha256
    /// `bf1c658e9bfdc13ec1ed51e59049d2b81201941b6ef4571f8382f356ea4b815b`) is
    /// the one 4:2:2 cell of the 384x240 census sweep: 384x240, ss (1,0),
    /// 8-bit, **17 decode frames, 16 shown** (decode 1 is the hidden altref).
    /// Its decode-frame-1 block at mi(32,0) is a 64x64 INTRA block with a
    /// PALETTE -- `aomdec`'s `EC_PIB` prints `use_palette=1` there, and the
    /// oracle's own prediction rungs print nothing for the block because
    /// `av1_predict_intra_block` returns at its palette branch before them
    /// (`reconintra.c:1735`).
    ///
    /// At 4:2:2 that block's chroma plane block is 32x64, but
    /// `av1_get_max_uv_txsize` adjusts `max_tx_size_rect_lookup[BLOCK_32X64]`
    /// down to TX_32X32, so it codes TWO STACKED SQUARE units per plane.
    /// `decode_inter_block`'s `side == 64` walk armed the ONE block-sized
    /// 2048-entry chroma palette prediction before the loop: the first unit
    /// consumed all of it and the second found none pending, so it silently
    /// fell back to the edge DC prediction -- a flat 90/240 box where the
    /// oracle carries its palette (U/V rows 170..189, cols 23..31, 180 wrong
    /// samples per plane at the pre-filter stage). Each unit now takes its own
    /// `palette_window` of that buffer (`decode.rs`) -- the same helper the
    /// split-transform chroma paths already used.
    ///
    /// **The committed assertion follows the family's standing rule** (same
    /// shape as
    /// [`the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name`]):
    /// the byte pin plus this decoder's disposition of those bytes.
    /// lane-av1422lift: `decode_stream` no longer refuses 4:2:2 at the sequence
    /// header, so the OK arm is the live one -- the gate decodes the pin and
    /// asserts 17 decode-order frames byte-exact against aomdec with
    /// `palette_422_unit_window_hits()` fired. lane-av1oracledepth: the stale
    /// `Err` arm (it matched the pre-lift refusal string
    /// `"a chroma format of 4:2:2"` the decoder no longer returns, and could
    /// have swallowed a future decode error as green) is deleted; a refusal
    /// now fails this gate loudly.
    #[test]
    fn the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact() {
        const NAME: &str = "the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact";
        const FILE: &str = "422_palette_intra_in_inter_384x240_17f.obu";
        const BYTES: usize = 19344;
        const FP: u64 = 0x923ccabfcb933e90;
        let _gate_lock = lock_gate_counters();
        let data = std::fs::read(crate_pin(FILE)).unwrap_or_else(|e| {
            panic!("{NAME}: the committed pin {FILE} is missing ({e}) -- a failure, not a skip")
        });
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FP, "{NAME}: {FILE} bytes drifted");
        let before = crate::decode::palette_422_unit_window_hits();
        // lane-av1oracledepth: the OK arm is MEASURED to be the live one (the
        // pinned bytes decode, 17 decode-order frames byte-exact vs aomdec,
        // 16 windowed units) and the old `Err` arm matching the pre-lift
        // refusal string "a chroma format of 4:2:2" is deleted: committed
        // code no longer returns that string, so the arm could only ever
        // swallow a FUTURE decode error that happened to contain it and read
        // green off a stream that decoded nothing. A refusal today now fails
        // this gate loudly, as it should.
        match decode_stream(&data) {
            Err(e) => {
                panic!(
                    "{NAME}: {FILE} was refused by the current decoder: {e}. 4:2:2 is a \
                     DECODED format; a refusal here is a regression, not the old sequence-\
                     header gate"
                )
            }
            Ok(_) => {
                let windows = crate::decode::palette_422_unit_window_hits() - before;
                assert!(
                    windows > 0,
                    "{NAME}: {FILE} decoded, but no 4:2:2 intra-in-inter chroma unit took \
                     the per-unit palette window ({windows} did) -- this cell does not \
                     exercise the arm at all (class gate-blind-to-feature)"
                );
                let (decoded, hidden) = decode_all_frames_vs_oracle(&data, FILE);
                assert_eq!(
                    decoded, 17,
                    "{NAME}: the oracle decoded {decoded} frames in DECODE order, not 17"
                );
                assert_eq!(hidden, 1, "{NAME}: expected exactly the one hidden altref");
                eprintln!(
                    "{NAME}: {decoded} decode-order frame(s) byte-exact against aomdec \
                     ({hidden} hidden), {windows} chroma palette unit window(s)"
                );
            }
        }
    }

    /// lane-av1-ibc128chunk: the WITNESS for the 128-root intrabc strip's
    /// de-square-cut mu-chunk chroma walk, and the byte pin for it.
    ///
    /// `decode_intrabc_128rect` cut each mu chunk's chroma plane block with
    /// `chunk_chroma = cside * 64 / side` -- a `chroma_side`-SQUARE chunk,
    /// i.e. the 4:2:0 count on BOTH axes (the same defect lane-av1-422bigblock
    /// fixed, and witnessed, in `decode_inter_block` and
    /// `decode_block_128rect`; this copy was deferred there as unwitnessed).
    /// At 4:2:2 a 64x64 mu chunk's chroma plane block is 32x64, so it holds
    /// TWO STACKED TX_32X32 units per plane: the square-cut walk read one per
    /// plane per chunk, left the chunk's lower 32 chroma rows with no
    /// residual at all, and stamped each unit's entropy context over a
    /// 16x16-mi luma span instead of 16x8.
    ///
    /// Measured red-before on the pinned witness (parent `a7d22aec`, via the
    /// local `EC_AV1_ALLOW_422_PROBE` patch-run-restore build): 8 chroma
    /// units stamped over the stream's two reached blocks and frames 0 and 3
    /// diverge from `aomdec` (45135 / 44540 of 245760 samples each), the
    /// first difference at Y sample 98368 -- frame 0, row 256, col 64, the
    /// top-left corner of the first 128x64 intrabc block at mi(64,0). The
    /// red build then lost 4 of the stream's 6 intrabc blocks to the desync.
    /// Green-after, same fixture: 48 chroma units stamped, all 6 blocks
    /// decoded, and ALL FIVE frames byte-identical to `aomdec --rawvideo`.
    ///
    /// lane-av1422lift: committed code no longer refuses 4:2:2 at the
    /// sequence header, so the red/green evidence above is no longer
    /// probe-only: this gate is the one that asserts it. Both pins are
    /// byte-pinned AND byte-exact against ffmpeg's `yuv422p` rawvideo of the
    /// same bytes, per plane and per frame, with the per-plane
    /// coefficient-unit census as the non-vacuity arm -- the census is what
    /// makes "48 chroma units stamped" a committed, reproducible number here
    /// instead of a sentence in a lane report. The source-scan arm below
    /// keeps its own claim (the per-axis SPELLING) and does not overlap.
    #[test]
    fn the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact() {
        const NAME: &str = "the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        for (file, bytes, fp) in [
            (
                "422_intrabc_sb128_strip.obu",
                1672usize,
                0x50f5cfc576e4cd00_u64,
            ),
            (
                "422_intrabc_sb128_strip_notxsearch.obu",
                1675,
                0xd4936f252ff8cff0,
            ),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(file);
            let data = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                    path.display()
                )
            });
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fp, "{NAME}: {file} bytes drifted");
            assert_422_stream_pixel_exact(NAME, &data, 384, 320, 5, 8, true);
        }
    }

    /// lane-av1422a2a4: the firing gate for `decode_block`'s var-tx EITHER/OR
    /// tail at 4:2:2 -- an INTRABC block whose var-tx tree resolved MIXED
    /// leaves, where the old blanket `set_txfm_ctxs` + `fill_lf_grid` publishes
    /// clobbered the per-leaf TXFM_CONTEXT bands libaom keeps.
    ///
    /// Provenance: aomenc profile 2, `testsrc2=size=128x112:rate=25` tiled 2x2
    /// to 256x224, cq 30, 3 frames (`lanes/av1422ibcrect.report.md` §2, stream
    /// A4). The oracle decodes it to completion; the pre-fix tree refused it
    /// mid-tile with the documented `decodeframe.c:1456` subsize string -- the
    /// E6/E7 desync-symptom class, NOT an invalid shape.
    ///
    /// Measured on the pinned bytes: the 32x32 intrabc block at mi(32,8)
    /// resolves leaves (0,0)+((0,4) 8x8, (4,0)+(4,4) 16x16, so
    /// `left_txfm[32..35]` must read 16 when the NEXT tree's sub-unit at
    /// (32,16) row0col0 reads its `txfm_partition` context (oracle operands
    /// `above=16 left=16`, ctx 9). The blanket publishes reset those cells to
    /// 8 -> ctx 10 -> a wrong `is_split` -> the first divergent read, and the
    /// refusal surfaced ~30 partition symbols later.
    #[test]
    fn the_pinned_422_intrabc_vartx_mixedtree_witnesses_decode_pixel_exact() {
        const NAME: &str = "the_pinned_422_intrabc_vartx_mixedtree_witnesses_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("422_intrabc_vartx_mixedtree_a4.obu");
        let data = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                path.display()
            )
        });
        assert_eq!(data.len(), 30937, "{NAME}: pinned witness size drifted");
        assert_eq!(
            fnv1a64(&data),
            0x6f0fc77c2217910a_u64,
            "{NAME}: pinned witness bytes drifted"
        );
        assert_422_stream_pixel_exact(NAME, &data, 256, 224, 3, 8, true);
    }

    /// lane-av1422ibctxsel: the firing gate for `decode_intrabc_owned_rect`'s
    /// NON-SPLIT chroma coefficient context at 4:2:2.
    ///
    /// `lanes/av1422gathercensus.report.md` closed its ss (1, 0) class sweep
    /// with this one gather at STOP -- unwitnessed: `around_mi_rect` over the
    /// block's LUMA span feeds the CHROMA `dc_sign_ctx`, and at ss (1, 0) a
    /// chroma 4-px cell spans TWO luma mi columns, so the per-mi sum counts
    /// every chroma column twice (`around_mi_422_chroma`'s every-second
    /// sampling is libaom's one-vote-per-cell sum). `lane-av1422ibcrect`
    /// found a stream that ENTERS the gather (24 entries) but whose two
    /// gathers agree on every entry, so the class stayed harmless-ON-THAT-
    /// STREAM, not proven.
    ///
    /// This fixture is the discriminating witness: aomenc profile 2, 384x288
    /// 4:2:2, 4 key frames, `--sb-size=64` with the last superblock row half
    /// outside the frame (so the 64-root partition is a single gathered bit
    /// and the strips are 64x32 HORZ intra-BC strips), `--enable-tx-size-
    /// search=1` plus a high-frequency patch that keeps the frame's `tx_mode`
    /// at `TX_MODE_SELECT` (`txb_split_count > 0`; without it encodeframe.c
    /// :2690 downgrades every frame to `TX_MODE_LARGEST`), and a chroma band
    /// above the strips so a full-weight above context meets a full-weight
    /// left one of the opposite sign.
    ///
    /// Measured on the pinned bytes: 24 non-split chroma gathers entered (6
    /// strips x 4 frames; 48 plane reads), and FIVE of them -- plane 2, strips
    /// mi (64,16)..(64,80) of frame 0 -- read `dc_sign_ctx` 1 through the
    /// luma-span gather against libaom's 0 through the pair rule (`full = 2A +
    /// L = -8`, `pair = A + L = 0`: the double-counted above vote crosses
    /// zero, and the context is a sign).
    ///
    /// Red-before (the route reverted, same fixture bytes): the wrong sign
    /// context changes the decoded dc-sign CDF and desyncs the tile -- the
    /// decoder refuses the stream mid-frame with "a block size 4x8, 8x16 or
    /// 16x4 ... has no chroma plane block at this frame's subsampling mode",
    /// so NO frame decodes at all. That refusal is also the proof it is a
    /// desync symptom and not a real sub-8x8 shape: both oracles decode the
    /// pinned bytes fully. Green-after: 4/4 frames byte-exact against
    /// ffmpeg's `yuv422p` rawvideo, with the per-plane coefficient-unit
    /// census (the helper's) and the pair-rule route counter as the
    /// non-vacuity arms. The 4:4:4 arm below pins the guard's other side:
    /// the same function at ss (0, 0) keeps the verbatim luma-span gather and
    /// must NOT fire the route.
    #[test]
    fn a_422_intrabc_owned_rect_nonsplit_chroma_context_uses_the_pair_rule() {
        const NAME: &str = "a_422_intrabc_owned_rect_nonsplit_chroma_context_uses_the_pair_rule";
        const FILE: &str = "ibc422_nonsplit_ctx.obu";
        const LEN: usize = 2170;
        /// sha256 `4f82d5da8b4c3d6553953fc373001a1c7a729c9b189214b65503af1e04eb6b68`,
        /// fnv1a64 `0x92e2c4e45ca27e34`.
        const FNV: u64 = 0x92e2c4e45ca27e34;
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(FILE);
        let data = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                path.display()
            )
        });
        assert_eq!(data.len(), LEN, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV, "{NAME}: {FILE} bytes drifted");
        // No `lock_gate_counters()` here: the bumps are thread-local, and
        // `assert_422_stream_pixel_exact` takes that same (non-reentrant)
        // mutex itself -- holding it across this call self-deadlocks.
        crate::decode::reset_ibc_owned_rect_422_pair_hits();
        assert_422_stream_pixel_exact(NAME, &data, 384, 288, 4, 8, true);
        let hits = crate::decode::ibc_owned_rect_422_pair_hits();
        assert_eq!(
            hits, 24,
            "{NAME}: the pair-rule route fired {hits} times, not 24 (6 non-split chroma gathers \
             x 4 frames) -- the witness stopped exercising the arm, or the guard's extent moved"
        );
        // The guard's other arm: the 4:4:4 owner-rect stream (`r512.obu`,
        // pinned by the gate below) reaches this same function at ss (0, 0),
        // where the verbatim luma-span gather is correct -- the route counter
        // must stay put.
        let r512 = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/r512.obu"),
        )
        .expect("r512.obu must exist");
        crate::decode::reset_ibc_owned_rect_422_pair_hits();
        let _ = decode_stream(&r512).expect("the 4:4:4 owner-rect witness decodes");
        assert_eq!(
            crate::decode::ibc_owned_rect_422_pair_hits(),
            0,
            "{NAME}: the 4:2:2 pair-rule route fired on a 4:4:4 stream -- the ss (1, 0) guard \
             leaks (class gate-blind-to-feature)"
        );
    }

    /// lane-av1-ibc128chunk: the SOURCE-SCAN arm of the witness gate above.
    ///
    /// The decode-level half of the red/green evidence now runs in committed
    /// code too -- the witness gate above,
    /// `the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact`,
    /// decodes both pins byte-exact against ffmpeg (lane-av1422lift lifted the
    /// sequence-header refusal). This arm complements it by pinning the thing
    /// that would silently bring the defect back: the square-cut
    /// `chunk_chroma = cside * 64 / side` expression, and the per-axis
    /// replacements that replaced it at BOTH sites in
    /// [`decode_intrabc_128rect`] -- the mu-chunk read walk and the
    /// `mu_chroma` per-unit context replay. It also pins the per-axis
    /// counters' accessors so the arms stay reachable, and the walk's own
    /// per-axis extent, derived here from the same `64 >> ss` form the
    /// witnesses measured (4:2:0 = one TX_32X32 per plane per chunk,
    /// 4:2:2 = two stacked, 4:4:4 = four).
    #[test]
    fn the_intrabc_128rect_chroma_chunk_walk_stays_per_axis() {
        const NAME: &str = "the_intrabc_128rect_chroma_chunk_walk_stays_per_axis";
        let src = crate::test_source::module_source("decode");
        let body = src
            .split_once("fn decode_intrabc_128rect(")
            .expect("decode_intrabc_128rect must exist")
            .1
            .split_once("\nfn ")
            .expect("decode_intrabc_128rect must be a top-level fn")
            .0;
        // Scan CODE only: the fix's own comments quote the old expression to
        // say what it was, and a prose mention must not read as a
        // reintroduction (nor mask one).
        let stripped: Vec<&str> = body
            .lines()
            .map(|l| l.split_once("//").map_or(l, |(c, _)| c))
            .collect();
        let code = stripped.join("\n");
        let body = code.as_str();
        assert_eq!(
            body.matches("cside * 64 / side").count(),
            0,
            "{NAME}: the square-cut `cside * 64 / side` chunk extent is BACK in \
             decode_intrabc_128rect -- that expression is the 4:2:2 defect this \
             gate exists for (a 64x64 mu chunk's chroma plane block is \
             (64 >> ss_x) x (64 >> ss_y), not `chroma_side` square)"
        );
        // The per-axis chunk extent, declared once at the mu-chunk read walk
        // and once at the `mu_chroma` replay -- the two de-square-cut sites.
        //
        // The replay must ALSO enumerate every unit inside a chunk, not just
        // its first: counting mu CHUNKS in `rows`/`cols` and then stepping the
        // mi origin and reading the grid per UNIT only agrees when a chunk
        // holds exactly one unit (4:2:0). At 4:2:2 and 4:4:4 a chunk holds two
        // or four, so the per-unit body alone left the rest of the chunk with
        // the composed-grid smearing the replay exists to undo.
        for name in [
            "let chunk_chroma_w = (64usize) >> ss_x(fctx);",
            "let chunk_chroma_h = (64usize) >> ss_y(fctx);",
        ] {
            assert_eq!(
                body.matches(name).count(),
                2,
                "{NAME}: `{name}` must appear at BOTH de-square-cut sites in \
                 decode_intrabc_128rect (the mu-chunk read walk and the mu_chroma \
                 context replay), found {}",
                body.matches(name).count()
            );
        }
        for name in [
            "let (ur, uc) = (chunk_chroma_h / cu, chunk_chroma_w / cu);",
            "for kr in 0..ur {",
            "for kc in 0..uc {",
        ] {
            assert_eq!(
                body.matches(name).count(),
                1,
                "{NAME}: `{name}` must appear exactly once in \
                 decode_intrabc_128rect -- the mu_chroma replay must walk chunk \
                 THEN unit (decode_inter_block's `mu_chroma_units` shape), found {}",
                body.matches(name).count()
            );
        }
        for name in [
            "let unit_luma_w = cu_tx << ss_x(fctx);",
            "let unit_luma_h = cu_tx << ss_y(fctx);",
            "let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));",
            "let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));",
            "neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)",
        ] {
            assert_eq!(
                body.matches(name).count(),
                1,
                "{NAME}: `{name}` must appear exactly once in \
                 decode_intrabc_128rect, found {}",
                body.matches(name).count()
            );
        }
        // lane-av1muchunk422sym: the CHROMA gather of that same walk must take
        // the 4:2:2 pair rule, exactly as `decode_inter_block`'s mu-chunk walk
        // does (one chroma column per pair of luma mi cells; the plain rect
        // gather doubles the above dc vote and flips `dc_sign_ctx` wherever
        // above and left cancel). Reachability is measured: the walk fires at
        // ss (1, 0) on `422_intrabc_sb128_strip.obu` (`EC_IBC128_BANDS`
        // prints); that fixture's own votes do not cancel, so THIS gate -- not
        // a pixel compare -- is what stops the routing being reverted.
        for name in [
            "let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {",
            "neighbours.around_mi_422_chroma(",
        ] {
            assert_eq!(
                body.matches(name).count(),
                1,
                "{NAME}: `{name}` must appear exactly once in \
                 decode_intrabc_128rect's chroma chunk walk, found {} -- the \
                 gather has fallen back to the square per-mi rule",
                body.matches(name).count()
            );
        }
        // The replay's own unit luma span is the lossless/lossy `cu << ss` form
        // (site 2 spells it separately from the read walk's `cu_tx << ss`).
        for name in [
            "let unit_luma_w = cu << ss_x(fctx);",
            "let unit_luma_h = cu << ss_y(fctx);",
        ] {
            assert_eq!(
                body.matches(name).count(),
                1,
                "{NAME}: `{name}` must appear exactly once in \
                 decode_intrabc_128rect, found {}",
                body.matches(name).count()
            );
        }
        // The per-axis chunk extent the geometry demands, spelled out here so
        // the scan and the witnesses cannot disagree about it.
        let per_axis_units = |ss_x: u32, ss_y: u32| (64 >> ss_x) / 32 * (64 >> ss_y) / 32;
        assert_eq!(per_axis_units(1, 1), 1, "{NAME}: 4:2:0 must stay one unit");
        assert_eq!(per_axis_units(1, 0), 2, "{NAME}: 4:2:2 must be two units");
        assert_eq!(per_axis_units(0, 0), 4, "{NAME}: 4:4:4 must be four units");
        // The counters the probe arms read must stay exported.
        for name in [
            "pub fn intrabc_128rect_chroma_chunk_hits()",
            "pub fn intrabc_128rect_chunk_shape()",
        ] {
            assert!(
                src.contains(name),
                "{NAME}: the accessor `{name}` must stay in decode.rs"
            );
        }
    }

    /// lane-av1422gather3: `decode_block_128rect`'s per-chunk CHROMA unit
    /// walk gathered its coefficient context with the plain square-cut
    /// `around_mi_rect`, so at ss (1,0) its above dc vote summed 16 LUMA mi
    /// cells where libaom's `get_txb_ctx_general` reads `txb_w_unit` =
    /// `chroma_tx / 4` = 8 CHROMA cells (one chroma column per PAIR of luma
    /// mi columns; both luma cells of a column carry the covering unit's
    /// whole-unit dc sign). That is the same pair rule the mu-chunk walks
    /// take (`around_mi_422_chroma`), and the gather now routes through it
    /// under the ss guard -- verbatim `around_mi_rect` at every other shape.
    ///
    /// Reachability is MEASURED, not assumed: the walk fires at ss (1,0) on
    /// both pinned `422_intrabc_sb128_strip*.obu` cells, 72 chroma unit
    /// gathers each (`sb128rect_chroma_replay_hits().0`, the same walk's own
    /// replay counter). No committed cell DISCRIMINATES the change: with the
    /// pair rule in place all 21 committed 4:2:2 cells are byte-exact
    /// against ffmpeg, exactly as the pre-change tree was (the gathered
    /// votes cancel at every reached unit), so THIS gate -- not a pixel
    /// compare -- is what stops the routing being reverted.
    ///
    /// lane-av1422mismatch9: the same decode loop now also asserts the
    /// `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` count (NINE per pin) and states
    /// why it is not a pixel miss -- the counter's predicate is a span
    /// DIFFERENCE, true by construction at ss (1,0); the replay re-stamps the
    /// identical context cells with the same per-axis pair the read walk used;
    /// and `record_mi_chroma` writes no pixel sample. See the assert's message.
    #[test]
    fn the_422_block_128rect_chroma_chunk_gather_stays_per_axis() {
        const NAME: &str = "the_422_block_128rect_chroma_chunk_gather_stays_per_axis";
        let src = crate::test_source::module_source("decode");
        let body = src
            .split_once("fn decode_block_128rect(")
            .expect("decode_block_128rect must exist")
            .1
            .split_once("\nfn ")
            .expect("decode_block_128rect must be a top-level fn")
            .0;
        // Scan CODE only: the fix's own comment quotes the guard form, and a
        // prose mention must not read as a reintroduction (nor mask one).
        let stripped: Vec<&str> = body
            .lines()
            .map(|l| l.split_once("//").map_or(l, |(c, _)| c))
            .collect();
        let code = stripped.join("\n");
        let body = code.as_str();
        assert_eq!(
            body.matches("if ss_x(fctx) == 1 && ss_y(fctx) == 0 {")
                .count(),
            1,
            "{NAME}: the ss (1, 0) guard over the per-chunk chroma gather must \
             appear exactly once in decode_block_128rect, found {}",
            body.matches("if ss_x(fctx) == 1 && ss_y(fctx) == 0 {")
                .count()
        );
        for name in [
            "neighbours.around_mi_422_chroma(cu_mi, luma_span, luma_span_h)",
            "neighbours.around_mi_rect(cu_mi, luma_span, luma_span_h)",
        ] {
            assert_eq!(
                body.matches(name).count(),
                1,
                "{NAME}: `{name}` must appear exactly once in \
                 decode_block_128rect's per-chunk chroma gather, found {} -- one \
                 arm has been dropped, so the gather has fallen back to the \
                 square per-mi rule at ss (1, 0)",
                body.matches(name).count()
            );
        }
        // Reachability arm: the walk must actually RUN at ss (1, 0) on the
        // pinned strips, or the routing above is a no-op the gate cannot see.
        // A stream that stopped coding 128-axis intra blocks reads 0 and this
        // assert fails instead of passing over a decode that never reached
        // the fixed gather (class gate-blind-to-feature).
        for (file, bytes, fp) in [
            (
                "422_intrabc_sb128_strip.obu",
                1672usize,
                0x50f5cfc576e4cd00_u64,
            ),
            (
                "422_intrabc_sb128_strip_notxsearch.obu",
                1675,
                0xd4936f252ff8cff0,
            ),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(file);
            let data = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                    path.display()
                )
            });
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fp, "{NAME}: {file} bytes drifted");
            let _guard = lock_gate_counters();
            let before = crate::decode::sb128rect_chroma_replay_hits();
            let frames = decode_stream(&data)
                .unwrap_or_else(|e| panic!("{NAME}: {file} no longer decodes cleanly: {e}"));
            let after = crate::decode::sb128rect_chroma_replay_hits();
            assert_eq!(
                after.0 - before.0,
                72,
                "{NAME}: {file} gathered {} chroma unit context(s) in \
                 decode_block_128rect's per-chunk walk; 72 were measured at ss (1, 0) -- \
                 a lower number means the gather is no longer REACHED here \
                 (class gate-blind-to-feature)",
                after.0 - before.0
            );
            // lane-av1422mismatch9: `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` reads
            // NINE on each of these two pins, and this is the only place that
            // number is asserted. It is NOT a pixel miss, and the three facts
            // below are why -- the assert encodes the measurement, not a wish:
            //
            //  1. The predicate (`decode.rs`, end of `decode_block_128rect`) is
            //     `luma_span != luma_span_h` -- `chroma_tx << ss_x` vs
            //     `chroma_tx << ss_y`. At ss (1, 0) the two DIFFER BY
            //     CONSTRUCTION for every 128-root rect block, so the counter is
            //     a shape test ("this is the 4:2:2 span shape"), not a
            //     divergence test. Nine is the number of such blocks the five
            //     frames code.
            //  2. The replay re-stamps with the SAME per-axis pair the in-loop
            //     walk stamped with (`record_mi_chroma(cu_mi, luma_span,
            //     luma_span_h, ...)` at both the read walk and the replay), so
            //     it rewrites the identical context cells -- this is exactly
            //     the `record_mi_chroma(cu_mi, luma_span, luma_span_h)` pairing
            //     the source-scan arm above pins. The pre-lane-av1-128rectspan
            //     defect (width passed on both axes) is caught by that
            //     spelling arm, not by this counter.
            //  3. `record_mi_chroma` writes only `Neighbours::left`/`above`
            //     (entropy-context arrays); it touches ZERO pixel samples. So
            //     the 9 events cannot write a sample the oracle lacks. The
            //     pixel proof is the same file's
            //     `the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_
            //     exact`, which pins all five frames byte-exact against
            //     ffmpeg's `yuv422p` rawvideo.
            //
            // Change this expected count ONLY with a new measurement; a 4:2:2
            // stream coding more 128-root rect blocks legitimately changes it.
            assert_eq!(
                after.1 - before.1,
                9,
                "{NAME}: {file} fired the 4:2:2 span-shape arm {} time(s); NINE were \
                 measured and are NOT a pixel miss (the predicate is `luma_span != \
                 luma_span_h`, true by construction at ss (1,0); the replay re-stamps the \
                 identical context cells with the same per-axis pair; `record_mi_chroma` \
                 writes no pixel) -- see the report above",
                after.1 - before.1
            );
            assert_eq!(frames.len(), 5, "{NAME}: {file} frame count");
        }
    }

    /// lane-av1422lpf: the three 4:2:2 LOSSLESS INTER witness fixtures,
    /// PINNED. As of lane-av1422lift they are asserted to DECODE pixel-exact
    /// against ffmpeg (the lane-av1422lift paragraph at the end of this doc);
    /// before the lift this gate asserted the SEQUENCE-HEADER refusal instead.
    ///
    /// W/X/Y (`W_intrabc.obu`, `X_intrabc_tiled.obu`, `Y_intrabc_10b.obu`) are
    /// the streams on which lane-av1422lpf found the seven-site chroma-extent
    /// PANIC family -- each site indexed a block's chroma PLANE BLOCK with
    /// the ENCLOSING SQUARE `max(side >> ss_x, side >> ss_y)` instead of the
    /// PER-AXIS `(bw >> ss_x, bh >> ss_y)` -- so the pins keep the bytes that
    /// witnessed it from silently changing under the gate.
    ///
    /// WHAT THIS GATE DID NOT COVER BEFORE THE LIFT, measured rather than
    /// argued: the refusal it asserted was the HEADER refusal, which fired
    /// BEFORE any of the seven sites is reachable, and it fired IDENTICALLY on
    /// the pre-fix tree (commit `aef4fa67` prints the same string, built and
    /// run for this correction). As a refusal contract the gate was therefore
    /// VACUOUS with respect to the seven-site fix -- it passed before and
    /// after the fix, and its only real content was the three pins.
    ///
    /// That refusal is GONE (lane-av1422lift lifted the sequence-header
    /// refusal), so the decode-level half of the evidence now runs in
    /// committed code: this gate's own byte-exact arm below decodes all three
    /// fixtures against ffmpeg. `EC_AV1_ALLOW_422_PROBE` was only ever the
    /// patch-run-restore bypass for the manual measurement in
    /// `lanes/av1422lpf.report.md`; it is still deliberately not committed.
    /// The source-scan arm
    /// `the_422_lossless_inter_chroma_walk_sites_stay_per_axis` remains the
    /// per-axis SPELLING proof; the pixel proof is now the committed decode.
    ///
    /// CORRECTED by lane-av1422llintra: the name said `..._panics_refuse_
    /// by_name`, and lane-av1422lpf's report claimed W and X now REFUSE with
    /// `a Golomb tail longer than this decoder reads` at frame 2. That
    /// refusal DOES NOT REPRODUCE on this tip: with the bypass applied, all
    /// three cells DECODE 16/16 -- no panic and no second refusal.
    ///
    /// lane-av1422lift: the three pins are asserted BYTE-EXACT against
    /// ffmpeg's rawvideo of the same bytes, per plane and per frame, with the
    /// per-plane coefficient-unit census as the non-vacuity arm -- so the
    /// three DECODE 16/16 fact the paragraph above records from a bypassed
    /// probe build is now a committed assertion. `Y` is the 10-bit member
    /// and carries the depth-correct HBD compare (`yuv422p10le`, 16-bit
    /// containers on both sides) rather than an 8-bit narrowing of a
    /// high-bit-depth cell.
    #[test]
    fn the_pinned_422_lossless_inter_witnesses_decode_pixel_exact() {
        const NAME: &str = "the_pinned_422_lossless_inter_witnesses_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        for (file, bytes, fp, depth) in [
            ("W_intrabc.obu", 130_320usize, 0xb6d5c4653a32567a_u64, 8u8),
            ("X_intrabc_tiled.obu", 131_696, 0xb6e04621d2116ef7, 8),
            ("Y_intrabc_10b.obu", 202_281, 0x2ecf03435a14dbf, 10),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(file);
            let data = std::fs::read(&path).unwrap_or_else(|e| {
                panic!("{NAME}: pinned witness {file} is missing ({e}) -- the gate cannot run")
            });
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fp, "{NAME}: {file} bytes drifted");
            assert_422_stream_pixel_exact(NAME, &data, 320, 240, 16, depth, true);
        }
    }

    /// lane-av1422lpf: the SOURCE-SCAN arm of the witness gate above -- one
    /// assertion per site, so a mutation at any single site reds exactly that
    /// site and leaves the other six alone.
    ///
    /// Every assertion has a NEGATIVE twin (`forbidden`) and a POSITIVE twin
    /// (`required`), so neither an unreverted site nor a matcher that has
    /// stopped matching can pass: the forbidden string is absent AND the
    /// required string is present in the same function body. The `required`
    /// half is what makes this gate non-vacuous in the
    /// `dead-matcher-positive-control` sense -- a scan that matched nothing
    /// would fail it rather than pass silently.
    ///
    /// WHAT IT DOES NOT COVER: this arm is a SPELLING scan. It cannot observe
    /// a decode, so it cannot prove any cell decodes and cannot prove any
    /// pixel -- the same limit as the witness gate above it, and stated here
    /// because "one assertion per site, each with a mutation proof" reads
    /// like more coverage than it is.
    ///
    /// `strip_code` is the same comment-stripping the arms above use: the
    /// fixes quote their own old expression to say what it was, and a prose
    /// mention must neither read as a reintroduction nor mask one.
    #[test]
    fn the_422_lossless_inter_chroma_walk_sites_stay_per_axis() {
        const NAME: &str = "the_422_lossless_inter_chroma_walk_sites_stay_per_axis";
        let src = crate::test_source::module_source("decode");
        let strip_code = |body: &str| -> String {
            body.lines()
                .map(|l| l.split_once("//").map_or(l, |(c, _)| c))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // A SECOND form with the same comment stripping but every run of
        // whitespace collapsed to one space, so a matcher can span an argument
        // list that a line-anchored fix's own comment sits inside. Without it
        // the site-5 and site-6 matchers are DEAD: the mutation that reverts
        // them still passes, because the literal being searched for never
        // matched even when the fix was in place. Measured, not assumed --
        // `verifier-dead-matcher-positive-control`.
        let flatten = |body: &str| -> String {
            strip_code(body)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let body_of = |fn_name: &str| -> String {
            let after = src
                .split_once(&format!("\nfn {fn_name}("))
                .unwrap_or_else(|| panic!("{NAME}: `{fn_name}` must exist in decode.rs"));
            let end = after.1.find("\nfn ").unwrap_or(after.1.len());
            strip_code(&after.1[..end])
        };
        let flat_of = |fn_name: &str| -> String {
            let after = src
                .split_once(&format!("\nfn {fn_name}("))
                .unwrap_or_else(|| panic!("{NAME}: `{fn_name}` must exist in decode.rs"));
            let end = after.1.find("\nfn ").unwrap_or(after.1.len());
            flatten(&after.1[..end])
        };
        // (label, enclosing fn, forbidden square-cut spelling, required
        //  per-axis spelling, required count in that fn)
        let sites: [(&str, &str, &str, &str, usize); 4] = [
            (
                "site 1 -- read_inter_chroma_lossless's composed-grid extent",
                "read_inter_chroma_lossless",
                "stride * stride,\n                );",
                "stride * blk_h,",
                1,
            ),
            (
                "site 7 -- read_intra_chroma_lossless's composed-grid extent",
                "read_intra_chroma_lossless",
                "stride * stride,\n                );",
                "stride * blk_h,",
                1,
            ),
            (
                "site 4 -- the strip chroma rect's VERT arm at ss (1,0)",
                "decode_inter_block",
                "} else if s.horz {\n                (8, 4)\n            } else {\n                (4, 8)\n            }",
                "} else if ss_y(fctx) == 0 {\n                (4, 16)\n            } else {\n                (4, 8)\n            }",
                1,
            ),
            (
                "site 5 -- the mu-chunk lossless walk's prediction stride",
                "decode_inter_block",
                "(write_chroma_w, write_chroma_h), chroma_side, mode_for_tx,",
                "(write_chroma_w, write_chroma_h), chroma_stride, mode_for_tx,",
                2,
            ),
        ];
        for (label, fn_name, forbidden, required, count) in sites {
            // The last table row is written in the flattened form (it spans a
            // comment the fix itself wrote); the others are line-anchored.
            let flattened = required.contains(" mode_for_tx,");
            let body = if flattened {
                flat_of(fn_name)
            } else {
                body_of(fn_name)
            };
            assert_eq!(
                body.matches(forbidden).count(),
                0,
                "{NAME}: {label} is BACK -- the square-cut expression \
                 `{forbidden}` is what this gate exists for"
            );
            assert!(
                body.matches(required).count() >= count,
                "{NAME}: {label}: the per-axis expression `{required}` must appear \
                 at least {count} time(s) in `{fn_name}` -- found {}",
                body.matches(required).count()
            );
        }
        // Sites 2, 5 and 6 are the `chroma_side`-vs-`chroma_stride` and
        // `chroma_side`-vs-`chroma_buf_h` argument choices, which are
        // single-token differences on otherwise identical lines. Counting the
        // remaining `chroma_side` uses per arm is the only way to see them, so
        // they get their own exact-count assertions instead of a substring.
        let dib = body_of("decode_inter_block");
        // Site 2: the lossless TX_4x4 unit replay indexes the composed grid.
        assert_eq!(
            dib.matches("let start = (cr * cu + rr) * chroma_stride + cc * cu;")
                .count(),
            1,
            "{NAME}: site 2 -- the lossless TX_4x4 replay must index the \
             composed plane grid at `chroma_stride` (the plane block's own \
             width), not `chroma_side`"
        );
        assert_eq!(
            dib.matches("let start = (cr * cu + rr) * chroma_side + cc * cu;")
                .count(),
            0,
            "{NAME}: site 2 is BACK -- the lossless TX_4x4 replay indexes the \
             composed grid at the enclosing square again"
        );
        // Sites 5 and 6. The two mu-chunk walks (the single-reference and the
        // compound twin) take the block's `(write_chroma_w, write_chroma_h)`
        // and the stride SEPARATELY; the two intra-in-inter walks take both
        // the block shape and the region as `(chroma_stride, chroma_buf_h)`.
        // Every one of the four must be per-axis; the pre-fix spelling was
        // the enclosing square `chroma_side` for the stride, and
        // `(chroma_side, chroma_side)` for the intra block shape.
        let dibf = format!(
            "{}{}",
            flat_of("decode_inter_block"),
            flat_of("decode_inter_block_intra_arm"),
        );  // the intra arm was split out of decode_inter_block (av1split D8a)
        assert_eq!(
            dibf.matches("(chroma_stride, chroma_buf_h),").count(),
            3,
            "{NAME}: site 6 -- the two intra-in-inter walks pass the plane block \
             as `(chroma_stride, chroma_buf_h)` at all THREE argument positions \
             it occupies -- the mu-chunk walk's blk shape, and the whole-block              walk's blk shape and region -- found {}",
            dibf.matches("(chroma_stride, chroma_buf_h),").count()
        );
        assert_eq!(
            dibf.matches("(chroma_side, chroma_side),").count(),
            0,
            "{NAME}: site 6 is BACK -- an intra-in-inter walk still hands the \
             plane block the enclosing square `chroma_side`"
        );
        // The geometry the scan cannot see, derived from the same `>> ss`
        // forms the fixes use, so the scan and the fix cannot disagree about
        // what per-axis means at each chroma format. A 16x16 luma block:
        // (side >> ss_x, side >> ss_y) is the plane block and
        // max(side >> ss_x, side >> ss_y) is the enclosing square.
        let plane_block = |side: usize, ss_x: u32, ss_y: u32| (side >> ss_x, side >> ss_y);
        let enclosing = |side: usize, ss_x: u32, ss_y: u32| {
            (
                (side >> ss_x).max(side >> ss_y),
                (side >> ss_x).max(side >> ss_y),
            )
        };
        for (label, side, ss_x, ss_y, block, square) in [
            ("4:2:0", 16usize, 1u32, 1u32, (8, 8), (8, 8)),
            ("4:2:2", 16, 1, 0, (8, 16), (16, 16)),
            ("4:4:4", 16, 0, 0, (16, 16), (16, 16)),
        ] {
            assert_eq!(
                plane_block(side, ss_x, ss_y),
                block,
                "{NAME}: the {label} chroma plane block of a {side}x{side} luma block"
            );
            assert_eq!(
                enclosing(side, ss_x, ss_y),
                square,
                "{NAME}: the {label} enclosing chroma square of a {side}x{side} \
                 luma block"
            );
        }
        // The load-bearing claim of "unchanged at 4:2:0 and 4:4:4": the two
        // are the same number exactly when `ss_x == ss_y`, and DIFFERENT at
        // 4:2:2 -- which is what makes every site above inert there and
        // active at 4:2:2. If this ever held at 4:2:2 the fixes would be
        // untested rather than inert.
        assert_eq!(plane_block(16, 1, 1), enclosing(16, 1, 1));
        assert_eq!(plane_block(16, 0, 0), enclosing(16, 0, 0));
        assert_ne!(
            plane_block(16, 1, 0),
            enclosing(16, 1, 0),
            "{NAME}: 4:2:2 must NOT be square -- if the plane block equalled \
             the enclosing square at ss (1,0), none of these sites could ever \
             be wrong and the gates above would be measuring nothing"
        );
        // Site 4's own geometry: a PARTITION_VERT_4 strip is 4x16 luma and
        // pairs on COLUMNS while `ss_x == 1`, so the pair is 8x16 and its
        // plane block is (8 >> ss_x, 16 >> ss_y).
        for (label, ss_x, _ss_y, want) in [
            ("4:2:0", 1u32, 1u32, (4usize, 8usize)),
            ("4:2:2", 1, 0, (4, 16)),
            ("4:4:4", 0, 0, (8, 16)),
        ] {
            assert_eq!(
                plane_block(8, ss_x, 0).0,
                want.0,
                "{NAME}: site 4 -- the {label} VERT strip pair's chroma width"
            );
        }
        // ...and the HORZ arm of the same match needs NO third shape at
        // 4:2:2: a HORZ_4 strip pairs on ROWS only while `ss_y == 1`, and
        // 4:2:2 has `ss_y == 0`, so each 16x4 strip is its own chroma
        // reference and its own (8 >> ss_x, 4 >> 0) plane block -- which is
        // the (8, 4) the existing arm already returns.
        assert_eq!(
            (plane_block(16, 1, 0).0, plane_block(4, 1, 0).1),
            (8usize, 4usize),
            "{NAME}: site 4 -- the 4:2:2 HORZ strip's own plane block is already \
             the (8, 4) the unsplit arm returns, which is why only the VERT arm \
             gained a shape"
        );
    }

    /// lane-av1422warp: the SECOND 4:2:2 coverage witness -- the one
    /// `422_sb128_3f.obu` could not be, because that stream's inter content
    /// is skip-heavy (see `lanes/av1422stripwrite.report.md`, which named a
    /// second fixture with real residuals, compound and warped motion above
    /// the vertical midpoint as the thing that would justify lifting the
    /// refusal). This is that fixture.
    ///
    /// `422_residual_compound_warp_16f.obu` (38845 bytes), sha256
    /// `d78e2afb43ce311d3d82335a945c6f80f62db4537966d881c1349a68b3aecb95`:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "mandelbrot=size=256x288:rate=24:maxiter=220:start_scale=3:end_scale=0.35:end_pts=300,rotate=a=0.10*t:c=none:ow=256:oh=288" \
    ///        -frames:v 24 -pix_fmt yuv422p -f yuv4mpegpipe src422.y4m
    /// aomenc --codec=av1 --profile=2 --input-bit-depth=8 --limit=16
    ///        --width=256 --height=288 --lag-in-frames=25 --auto-alt-ref=1
    ///        --enable-global-motion=1 --pass=1
    ///        --cq-level=24 --cpu-used=0
    ///        --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
    ///        src422.y4m -o a1.webm
    /// ffmpeg -i a1.webm -c copy -f obu -y a1.obu
    /// ```
    ///
    /// CORRECTED by lane-av1422lrless (reviewer P2). This doc previously
    /// carried a different ffmpeg filter (`rotate=a=0.10`, no `*t`, no
    /// `:c=none:ow/oh`) and a `-n ffp --pass=2 --fpf=...` two-pass line.
    /// **Neither produced these bytes.** Re-encoding from the recorded
    /// source `src422.y4m` (sha256
    /// `4d35eaf65d1a5541b3177e1183644c163b3868d8f141bed0ce9fdf833280ba9f`)
    /// was measured three ways: the two-pass form gives 38774 bytes
    /// (`e93c4ed7...`), and only adding `--pass=1` reproduces these 38845
    /// bytes byte for byte (`d78e2afb...`). `aomenc` DEFAULTS to two-pass,
    /// so the flag has to be stated to get back the pinned stream. The
    /// recipe above is the one that was actually run.
    ///
    /// Measured on a patch-run-restore build (the `EC_AV1_ALLOW_422_PROBE`
    /// bypass applied for the run, reverted after -- it is deliberately NOT
    /// committed). This block is the bypassed measurement; the committed
    /// assertion is now the byte-exact compare in the lane-av1422lift
    /// paragraph below:
    ///
    /// - all 16 frames pixel-exact against `aomdec --rawvideo`, 2359296
    ///   bytes each, sha256 `4bfc2395e5ca6ea178f606e6b1ced773e26fc2d85b
    ///   c3a6cdb63a402939aeb203` on both sides;
    /// - the entropy stream pairs bit-for-bit against the instrumented
    ///   oracle -- 246735 symbol reads each, no divergence anywhere in the
    ///   stream, not merely at the window this lane spent nine rounds
    ///   narrowing;
    /// - the arms this fixture exists to cover really fire ABOVE the
    ///   vertical midpoint: 3 upper-half blocks on a non-`TRANSLATION`
    ///   global-motion model (`top_half_warp_hits`, gated on libaom's
    ///   `is_global_mv_block` per slot) and 3 upper-half
    ///   `GLOBAL_GLOBALMV` compound blocks (`top_half_compound_hits`),
    ///   alongside 25 compound-warp, 18 compound-warp-8x8-leaf, 84
    ///   rotzoom global-warp, 22 Wiener and 10 SGRPROJ loop-restoration
    ///   units frame-globally.
    ///
    /// The loop-restoration numbers are load-bearing twice over: decoding
    /// this stream is what exposed the per-axis `read_lr` corner bug
    /// (`restoration.rs`, `av1_loop_restoration_corners_in_sb`), whose fix
    /// is what made the whole sequence pair. 4:2:0 and 4:4:4 are unchanged
    /// by it -- byte-identical to their pre-lane decodes, with the LR gate
    /// family green.
    ///
    /// lane-av1422lift: the committed assertion is now the byte pin AND
    /// byte-exactness against ffmpeg's `yuv422p` rawvideo of the same bytes,
    /// per plane and per frame, with the per-plane coefficient-unit census
    /// as the non-vacuity arm. The paragraph above's coverage counts (25
    /// compound-warp, 84 rotzoom global-warp, 22 Wiener, 10 SGRPROJ, CDEF
    /// 39) are what make the row non-vacuous: without them a byte-exact
    /// compare could be green over a stream that never entered a warp arm.
    #[test]
    fn the_pinned_422_residual_compound_warp_witness_decodes_pixel_exact() {
        const NAME: &str = "the_pinned_422_residual_compound_warp_witness_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        const FILE: &str = "422_residual_compound_warp_16f.obu";
        const BYTES: usize = 38845;
        const FP: u64 = 0x0e73a51e2cc0c424;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(FILE);
        let data = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                path.display()
            )
        });
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FP, "{NAME}: {FILE} bytes drifted");
        assert_422_stream_pixel_exact(NAME, &data, 256, 288, 16, 8, true);
    }

    /// lane-av1422lrless: the LR-OFF half of the 4:2:2 coverage pair.
    ///
    /// This is `422_residual_compound_warp_16f.obu`'s recipe with EXACTLY
    /// ONE flag changed, `--enable-restoration=0`, and that is a measured
    /// claim rather than an assertion: both arms were re-encoded from the
    /// same source `src422.y4m` (sha256
    /// `4d35eaf65d1a5541b3177e1183644c163b3868d8f141bed0ce9fdf833280ba9f`)
    /// with the same aomenc invocation, and the LR-on arm reproduces the
    /// committed 38845 bytes byte for byte. So the pair is controlled, and
    /// the corners code is the only thing that can differ between their
    /// decodes: with restoration off, every plane's `frame_restoration_type`
    /// is `RESTORE_NONE`, so `read_lr` skips each plane and never computes a
    /// range. The LR-on stream exercises the range; this one proves the skip
    /// is the skip.
    ///
    /// ```text
    /// aomenc --codec=av1 --profile=2 --input-bit-depth=8 --limit=16
    ///        --width=256 --height=288 --lag-in-frames=25 --auto-alt-ref=1
    ///        --enable-global-motion=1 --pass=1 --enable-restoration=0
    ///        --cq-level=24 --cpu-used=0
    ///        --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
    ///        src422.y4m -o a3.webm
    /// ffmpeg -i a3.webm -c copy -f obu -y a3.obu
    /// ```
    ///
    /// 38538 bytes, sha256
    /// `8bed368f7ec4bca772137c6a9c2af89e1b2767ed5f937b91bd9e72ec60aeb42f`.
    ///
    /// Measured on a patch-run-restore build (the `EC_AV1_ALLOW_422_PROBE`
    /// bypass applied for the run, reverted after -- deliberately not
    /// committed). This block is the bypassed measurement; the committed
    /// assertion is now the byte-exact compare in the lane-av1422lift
    /// paragraph below:
    ///
    /// - all 16 frames pixel-exact against `aomdec --rawvideo`, 2359296
    ///   bytes each, sha256 `1cf47bc9e06a6e286452dfe4ffe457a85b6badd25
    ///   657dfd36af48262e8eb2464` on both sides;
    /// - the entropy stream pairs **244756-for-244756** against the
    ///   instrumented oracle with no divergence anywhere -- a different
    ///   read count from the LR-on sibling's 246735, which is the corners
    ///   skip showing up in the symbol stream, so the two arms really are
    ///   on different paths;
    /// - **loop restoration is proved off, not inferred from the flag**:
    ///   all five LR counters read zero where the LR-on sibling reads
    ///   `lr_wiener` 22, `lr_sgrproj` 10, `lr_stripe0` 26, `lr_last_stripe`
    ///   28;
    /// - and it carries MORE of the coverage than its sibling, which is
    ///   what rescues the thin leg of the lift argument: **top-half
    ///   compound 11** against the sibling's 3, and top-half warp 5 against
    ///   3. Frame-globally compound_warp 25, compound_warp_8 15, rotzoom
    ///   global-warp 77, cdef_idx 39, part128_split 95. The stronger arm is
    ///   the one where `read_lr` does nothing at all, so the engagement is
    ///   not an artefact of the code under test.
    ///
    /// lane-av1422lift: same scope change as its siblings, and this is the arm
    /// where it matters most: this stream is the one where `read_lr` does
    /// nothing at all, so a byte-exact green over it is a claim about the
    /// per-axis path WITHOUT the loop-restoration corner fix carrying it.
    /// The pin stays, the compare against ffmpeg's `yuv422p` rawvideo is
    /// added per plane and per frame, and the per-plane coefficient-unit
    /// census is the non-vacuity arm.
    #[test]
    fn the_pinned_422_lr_off_witness_decodes_pixel_exact() {
        const NAME: &str = "the_pinned_422_lr_off_witness_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        const FILE: &str = "422_residual_compound_warp_nolr_16f.obu";
        const BYTES: usize = 38538;
        const FP: u64 = 0x4b8ff761701e2bef;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(FILE);
        let data = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned witness {} is missing ({e}) -- the gate cannot run",
                path.display()
            )
        });
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FP, "{NAME}: {FILE} bytes drifted");
        assert_422_stream_pixel_exact(NAME, &data, 256, 288, 16, 8, true);
    }

    /// lane-av1422lift: 4:2:2 FILM GRAIN, and why this one gate cannot use
    /// the dump path or the shared body above.
    ///
    /// `EC_AV1_FINAL_DUMP` writes the picture "exactly as it is about to be
    /// stored into the reference slots" (`stream.rs`, the dump site beside
    /// the emit), which is BEFORE `apply_grain` runs on the output. ffmpeg's
    /// rawvideo is POST-grain. So every grain cell -- 4:2:0, 4:2:2, 12-bit,
    /// all of them -- is incomparable through the dump, and the corpus
    /// comparator hits it as a hard error rather than as a clean red
    /// (measured on `s422_grain_160x128.obu`: "luma identity never covers the
    /// display side (3 decode frames, 3 display frames)"). This gate
    /// therefore compares the IN-MEMORY `decode_stream` output, which is
    /// post-grain, exactly like
    /// `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact` does for
    /// 12-bit 4:2:0. Using the pre-grain dump here would make the gate
    /// permanently red for a reason that has nothing to do with grain.
    ///
    /// The pin, `s422_grain_160x128.obu` (5433 bytes, sha256
    /// `a4a6957b5e8298d9138f64498cab2d3008b3390fa73cd0d74aa0632b13d93d15`,
    /// fnv1a64 `0x2aed47614b9461b8`), is aomenc profile 2 over a `C422`
    /// testsrc2 160x128 3-frame source with `--film-grain-test=5`,
    /// `--passes=1 --end-usage=q --cq-level=24 --cpu-used=0 --threads=1
    /// --row-mt=0 --limit=3 --obu`.
    ///
    /// Two arms, because a green compare over a stream whose grain never ran
    /// is the "params-without-apply" false green:
    ///
    /// 1. the stream's OWN frame headers carry `apply_grain` and scaling
    ///    points on all THREE planes -- at 4:2:2 that is the shape libaom's
    ///    `generate_grain_uv_422` exists for, and it is read out of the
    ///    bytes, not out of the flags handed to the encoder;
    /// 2. `film_grain::grain_hits()` moves across the decode.
    #[test]
    fn a_real_422_film_grain_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_422_film_grain_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let _gate_lock = lock_gate_counters();
        let path = crate_pin("s422_grain_160x128.obu");
        let stream = read_pin(&path, 5433, 0x2aed47614b9461b8, NAME);
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        let mut grain_frames = 0usize;
        while pos < stream.len() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
            if let ObuKind::Frame(header, _) = obu.kind {
                let fg = &header.film_grain;
                assert!(
                    fg.apply_grain,
                    "{NAME}: frame without apply_grain -- aomenc emitted params it does not apply"
                );
                assert!(
                    fg.num_y_points > 0 && fg.num_cb_points > 0 && fg.num_cr_points > 0,
                    "{NAME}: grain without all three planes' scaling points -- the per-axis \
                     `generate_grain_uv_422` geometry would not be exercised"
                );
                grain_frames += 1;
            }
        }
        assert_eq!(
            grain_frames, 3,
            "{NAME}: expected grain on all three frames"
        );
        let before = crate::film_grain::grain_hits();
        let units_before = crate::decode::census_nonsub_units();
        let ours = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 4:2:2 grain stream: {e}")
        });
        assert_eq!(ours.len(), 3, "{NAME}: decoded frame count");
        assert_eq!(
            ours[0].u.len(),
            80 * 128,
            "{NAME}: 160x128 4:2:2 chroma is 80x128; {} means a 4:2:0 plane",
            ours[0].u.len()
        );
        assert!(
            crate::film_grain::grain_hits() > before,
            "{NAME}: matched but zero grain_hits -- grain synthesis never ran"
        );
        let units = crate::decode::census_nonsub_units();
        for (p, plane) in ["luma", "U", "V"].into_iter().enumerate() {
            assert!(
                units[p] > units_before[p],
                "{NAME}: the {plane} plane walked ZERO coefficient units -- this byte-exact \
                 compare is vacuous with respect to 4:2:2"
            );
        }
        let theirs = ffmpeg_decode_sequence_422(&stream, 160, 128, 3);
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            for (plane, ours_plane, theirs_plane) in
                [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)]
            {
                if let Some(bad) = ours_plane
                    .iter()
                    .zip(theirs_plane)
                    .position(|(x, y)| x != y)
                {
                    panic!(
                        "{NAME}: {plane} differs from ffmpeg at frame {i} sample {bad} ({} vs {})",
                        ours_plane[bad], theirs_plane[bad]
                    );
                }
            }
        }
        eprintln!(
            "{NAME}: post-grain byte-exact vs ffmpeg; grain_hits={}",
            crate::film_grain::grain_hits()
        );
    }

    /// lane-av1422lift: THE CORPUS BATTERY -- the gate the 4:2:2 campaign
    /// earned, and the one that replaces seven gates whose only behavioural
    /// assertion was `err.contains(REFUSAL)`.
    ///
    /// **Why these seven.** The measurement base
    /// (`lanes/av1422ffmpegbase.report.md` section 2.1) recorded seven 4:2:2
    /// cells that diverged from ffmpeg: `s422_320x246`, `s422_322x240`,
    /// `s422_322x246`, `s422_352x242_10b`, `s422_384x240`,
    /// `s422_416x242_10b`, `s422_416x250_10b`. ALL SEVEN are closed and are
    /// pinned here, each byte-exact against ffmpeg with the per-plane
    /// coefficient-unit census as its non-vacuity arm. Six of them are
    /// CHROMA-only divergences at non-8-aligned geometries -- odd or
    /// 2-mod-4 luma dimensions, where `round_ss` and the chroma block walk
    /// disagree with a hardcoded `>> 1` -- which is why they are the rows a
    /// lift cannot afford to leave unasserted: they are the cells that would
    /// go quietly wrong again.
    ///
    /// **The seventh, `s422_384x240`, IS here now.** lane-av1422lift pinned six
    /// of the seven and deliberately left this one out: it was still red, and
    /// pinning a red cell inside a green gate is how a gate stops being a gate
    /// ("THIS BRANCH MUST NOT LAND UNTIL THAT CELL CLOSES AND THE CORPUS
    /// RE-MEASURES 51/51", `lanes/av1422lift.report.md` §7). The cell closed in
    /// lane-av1422tailskip, and NOT through the reserved group-tail chroma SKIP
    /// arm the risk study's R4 blamed: that lane measured it byte-exact against
    /// ffmpeg on all 16 displayed frames and all 17 decode-order frames, 0/0/0
    /// wrong samples in Y/U/V, with the fix's own counter
    /// (`palette_422_unit_window_hits`) non-zero on this stream.
    /// `lanes/av1422tailskip.report.md` §1 records the measurement and the
    /// refutation of the arm attribution.
    ///
    /// That lane had already committed these exact bytes under the name
    /// `422_palette_intra_in_inter_384x240_17f.obu` (sha256 `bf1c658e...a815b`,
    /// the sweep artifact byte for byte), so the row below pins THAT file
    /// rather than committing a second copy of one stream under a second name.
    /// Unlike the other six, this cell sits at an 8- AND 16-aligned geometry
    /// (384x240): it was the late-starting frame-1 alt-ref chroma residual
    /// (`lanes/av1422late.report.md`), not the `round_ss` class -- which is why
    /// it is pinned as its own row here rather than folded into that sentence.
    ///
    /// **The corpus re-measures 51/51**, which was the second half of the
    /// condition the paragraph above quotes. Re-run for this row with the
    /// ffmpeg-oracle census driver (`~/.cache/census422b/ffcensus.py`: display
    /// order, shown frames, geometry from `ffprobe`, a count over zero frames
    /// is a hard error) and a `dump_yuv` built from THIS lane's tree:
    /// **51 of 51 4:2:2 cells BYTE-EXACT** -- `s422_384x240` among them at
    /// 0/0/0 -- and, in the same run, all 44 4:2:0 / 4:4:4 controls BYTE-EXACT
    /// (95/95 over the whole corpus). That run is a measurement of this tree,
    /// not a re-reading of the closure report; the per-cell rows here are what
    /// keep the seven named ones from drifting back.
    ///
    /// **The census arm, per row.** `assert_422_stream_pixel_exact` asserts
    /// that all three planes read a non-zero number of coefficient units AND
    /// a non-zero number of units that coded at least one coefficient, on
    /// this row's own bytes. That is the requirement R8 named as unwritable
    /// before lane-av1422census422 armed the census for `(1,0)`; measured on
    /// this tree the OLD arming read `[0, 0, 0]` on every 4:2:2 cell while
    /// the same decode walked 4994 units, so a green here with no census
    /// assert would have proven nothing about 4:2:2.
    ///
    /// **Provenance of the seven pins.** All seven are `aomenc --codec=av1
    /// --profile=2 --input-bit-depth=<8|10> --cpu-used=0 --cq-level=24
    /// --limit=16 --auto-alt-ref=1 --enable-global-motion=1 --passes=1
    /// --obu` over a hand-built `C422` y4m (header line, then `FRAME\n` plus
    /// one frame of planes per frame -- libaom's y4minput requires those six
    /// bytes before EVERY frame), produced by the census sweep. The first six
    /// were copied in from `~/.cache/census422b/sweep/`; the seventh is the
    /// same sweep's `s422_384x240.obu`, which lane-av1422tailskip had already
    /// committed under the name in its own row (the two files are byte for
    /// byte identical -- sha256 `bf1c658e9bfdc13ec1ed51e59049d2b81201941b6ef45\
    /// 71f8382f356ea4b815b`), so it is pinned under that name here. They are
    /// ordinary libaom 4:2:2 streams with nothing bolted on.
    #[test]
    fn the_pinned_422_corpus_cells_decode_pixel_exact() {
        const NAME: &str = "the_pinned_422_corpus_cells_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // (file, bytes, fnv1a64, sha256, w, h, depth, frames)
        for (file, bytes, fp, sha, w, h, depth, frames) in [
            (
                "s422_320x246.obu",
                20692usize,
                0x01e556015878e04eu64,
                "aebd3a10d46e617b803579819f2502798076384be52e032b11b46f09eeb09a79",
                320usize,
                246usize,
                8u8,
                16usize,
            ),
            (
                "s422_322x240.obu",
                21096,
                0x9a96dda2a19d47a6,
                "e74bf7e91f23353986edf9380bcd6cb760194e619dbbdf6a239b7a05eff81799",
                322,
                240,
                8,
                16,
            ),
            (
                "s422_322x246.obu",
                20528,
                0x2c6f8be6d30384ab,
                "ee96617fe45ba963d3628f37086077a94806ef0465a74cb538a8e57d2ed900b3",
                322,
                246,
                8,
                16,
            ),
            (
                "s422_352x242_10b.obu",
                20575,
                0xf1d370f80d8ddc6d,
                "8893bbed108d7555e66c35f2f45d6e44044dcd6810b0510fb251be05dabaa2d0",
                352,
                242,
                10,
                16,
            ),
            (
                "s422_416x242_10b.obu",
                20282,
                0x9826402d91abbc56,
                "38d0b592e5e358901742122c590c8964b5d08d0ceccfa051e677c28f4b75d174",
                416,
                242,
                10,
                16,
            ),
            (
                "s422_416x250_10b.obu",
                19967,
                0x6b07e837eae8a6be,
                "d6aa7f450f54d6fb381400a0a0226da87599c00509eeb5031abe63383e272627",
                416,
                250,
                10,
                16,
            ),
            (
                // Census cell `s422_384x240` -- the seventh previously
                // diverging cell, closed by lane-av1422tailskip. Same bytes as
                // the sweep's `s422_384x240.obu`, already in this fixtures dir
                // under the name lane-av1422tailskip's own gate pins.
                "422_palette_intra_in_inter_384x240_17f.obu",
                19344usize,
                0x923ccabfcb933e90u64,
                "bf1c658e9bfdc13ec1ed51e59049d2b81201941b6ef4571f8382f356ea4b815b",
                384usize,
                240usize,
                8u8,
                16usize,
            ),
        ] {
            let row = format!("{NAME}[{file}]");
            let path = crate_pin(file);
            let data = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{row}: pinned corpus cell {} is missing ({e}) -- the gate cannot run",
                    path.display()
                )
            });
            assert_eq!(data.len(), bytes, "{row}: size drifted");
            assert_eq!(fnv1a64(&data), fp, "{row}: bytes drifted");
            // The sha256 is the corpus's own identity column (its
            // `cells_422_3d56.json` records one per cell and the census
            // verified zero drift); fnv1a64 is what `read_pin` can check
            // in-process without spawning sha256sum, so both are named and
            // the sha256 is what a re-capture must reproduce.
            eprintln!("{row}: sha256 {sha}");
            assert_422_stream_pixel_exact(&row, &data, w, h, frames, depth, true);
        }
    }

    /// lane-av1-qmatrix: WITNESS for the lifted `using_qmatrix` refusal (the
    /// old gate `a_frame_using_quantisation_matrices_is_refused_by_name` is
    /// its mirror image -- its own panic said "flip this gate to a witness"
    /// the moment the dequant landed).
    ///
    /// A REAL `aomenc --enable-qm=1` stream (same source and recipe as the
    /// refusal's gate, plus its `--enable-qm=0` control) now DECODES, and
    /// every frame of both arms is pixel-exact against ffmpeg's own decode
    /// of the same OBU. The census proves the feature non-vacuously both
    /// ways (class `gate-blind-to-feature`): the parsed frame headers really
    /// carry `using_qmatrix = 1`, the dequantiser really SCALED transform
    /// units through a matrix on the qm-on arm (`qm_scaled_units() > 0`),
    /// and the qm-off control really scaled none (`== 0`).
    #[test]
    fn a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!(
                "SKIP {NAME}: needs ffmpeg + an aomenc oracle at {}",
                aomenc_path().display()
            );
            return;
        }
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 3;
        // Every frame header's (using_qmatrix, qm_y, qm_u, qm_v), parsed
        // straight off the encoded bytes.
        let qm_headers = |stream: &[u8]| -> Vec<(bool, u8, u8, u8)> {
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut out = Vec::new();
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind {
                    let q = &header.quantization;
                    out.push((q.using_qmatrix, q.qm_y, q.qm_u, q.qm_v));
                }
            }
            out
        };
        // `census_attempt` hands back only the stream LENGTH; this witness
        // needs the BYTES to hand ffmpeg, so the same recipe (its base args
        // verbatim, `testsrc2` 128x96 @ 25fps, 3 frames) runs inline.
        let encode = |extra: &[&str]| -> Vec<u8> {
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size={W}x{H}:rate=25"),
                    "-t",
                    "0.12",
                    "-pix_fmt",
                    "yuv420p",
                    "-strict",
                    "-1",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(y4m.status.success(), "ffmpeg refused the {W}x{H} fixture");
            let mut args: Vec<String> = [
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--threads=1",
                "--row-mt=0",
                "--kf-max-dist=9999",
                "--lag-in-frames=0",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
            args.push(format!("--limit={FRAMES}"));
            args.extend(extra.iter().map(|s| (*s).to_owned()));
            args.extend([
                "--obu".to_owned(),
                "-o".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
            ]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        // Pixel-exact every decoded frame of `stream` against ffmpeg, plane
        // by plane, first differing sample named.
        let compare = |stream: &[u8], frames: usize, label: &str| {
            let want = ffmpeg_decode_sequence(stream, W, H, frames);
            let got =
                decode_stream(stream).unwrap_or_else(|e| panic!("{NAME}: {label} decode: {e}"));
            assert_eq!(got.len(), want.len(), "{NAME}: {label} frame count");
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                for (plane, (gp, wp)) in [(&g.y, &w.y), (&g.u, &w.u), (&g.v, &w.v)]
                    .into_iter()
                    .enumerate()
                {
                    assert_eq!(
                        gp,
                        wp,
                        "{NAME}: {label} frame {i} plane {plane} differs from ffmpeg (first mismatching sample: got {}, want {})",
                        gp.iter()
                            .zip(wp.iter())
                            .find(|(a, b)| a != b)
                            .map_or(0, |(a, _)| *a),
                        gp.iter()
                            .zip(wp.iter())
                            .find(|(a, b)| a != b)
                            .map_or(0, |(_, b)| *b)
                    );
                }
            }
        };

        // CONTROL: the same source and recipe with quantisation matrices OFF
        // -- unchanged by this lane, and it must not scale a single unit
        // through a matrix.
        crate::quant::reset_qm_scaled_units();
        let control_stream = encode(&["--enable-qm=0"]);
        let control_frames = decode_stream(&control_stream)
            .unwrap_or_else(|e| panic!("{NAME}: the --enable-qm=0 control must decode, got: {e}"))
            .len();
        assert!(
            control_frames >= 1,
            "{NAME}: the qm-off control decoded no frame"
        );
        for (using, y, u, v) in qm_headers(&control_stream) {
            assert!(
                !using,
                "{NAME}: the control's frame header set using_qmatrix"
            );
            assert_eq!(
                (y, u, v),
                (0, 0, 0),
                "{NAME}: the control's frame header carried qm levels {y}/{u}/{v}"
            );
        }
        compare(&control_stream, control_frames, "qm-off control");
        assert_eq!(
            crate::quant::qm_scaled_units(),
            0,
            "{NAME}: the qm-off control scaled units through a matrix"
        );

        // The qm-on stream: decodes now, pixel-exact, levels 5 per plane as
        // aomenc writes them.
        crate::quant::reset_qm_scaled_units();
        let qm_stream = encode(&["--enable-qm=1"]);
        let qm_frames = decode_stream(&qm_stream)
            .unwrap_or_else(|e| panic!("{NAME}: the --enable-qm=1 stream must decode, got: {e}"))
            .len();
        assert!(qm_frames >= 1, "{NAME}: the qm-on stream decoded no frame");
        let headers = qm_headers(&qm_stream);
        assert!(
            !headers.is_empty() && headers.iter().all(|h| h.0),
            "{NAME}: not every qm-on frame header set using_qmatrix: {headers:?}"
        );
        assert!(
            headers.iter().all(|h| (h.1, h.2, h.3) == (5, 5, 5)),
            "{NAME}: expected aomenc's qm levels 5/5/5, got {headers:?}"
        );
        compare(&qm_stream, qm_frames, "qm-on");
        let scaled = crate::quant::qm_scaled_units();
        assert!(
            scaled > 0,
            "{NAME}: the --enable-qm=1 stream dequantized every unit flat -- \
             the matrices were never applied (class gate-blind-to-feature)"
        );
        eprintln!(
            "{NAME}: qm-off control {control_frames} frames exact (0 matrix-scaled units), \
             qm-on {qm_frames} frames exact ({scaled} matrix-scaled units, levels 5/5/5)"
        );
    }

    /// lane-av1mono: the INVERSE of the pilot's
    /// `a_real_libaom_monochrome_key_frame_is_refused_by_name`, which the
    /// pilot's merge `d71084ae` landed on `main` (after this lane's base
    /// `7f4e8301`): the very same recipe now decodes, and its luma is
    /// byte-exact against ffmpeg's own decode of the same OBU. A merge of this
    /// lane into `main` must therefore resolve that pinning gate by adopting
    /// this witness in its place.
    ///
    /// `ffmpeg -pix_fmt gray -c:v libaom-av1` sets `color_config.mono_chrome`,
    /// so libaom codes NO chroma symbol anywhere in the sequence -- not the
    /// mode syntax, not a coefficient. That reaches all three classes this lane
    /// had to close: chroma SYMBOL reads (`uv_mode`/cfl/angle/palette), chroma
    /// COEFFICIENT reads on the rect-strip paths that call `read_coeffs_rect`
    /// DIRECTLY instead of through `read_plane`'s own plane gate, and the
    /// palette-mode neighbour band a mono sub-8x8 leaf must still clear before
    /// it returns (an uncleared band made the next block read `palette_y_mode`
    /// off the wrong CDF row and silently desynced the rest of the tile --
    /// measured at decode-order frame 33 of this very recipe).
    ///
    /// The 4:2:0 twin of the same source and settings is the control: it proves
    /// the recipe is decodable, so a failure here is about the plane count and
    /// not about the source or the encoder settings.
    #[test]
    fn a_real_libaom_monochrome_key_frame_decodes_pixel_exact() {
        const NAME: &str = "a_real_libaom_monochrome_key_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let emit = |pix: &str| -> Vec<u8> {
            let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=30:duration=2",
                    "-pix_fmt",
                    pix,
                    "-c:v",
                    "libaom-av1",
                    "-cpu-used",
                    "8",
                    "-b:v",
                    "300k",
                    "-f",
                    "obu",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                out.status.success(),
                "{NAME}: ffmpeg ({pix}) failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        // CONTROL: the identical recipe at 4:2:0 still decodes.
        let twin = decode_stream(&emit("yuv420p")).expect("the 4:2:0 twin must decode");
        assert!(!twin.is_empty(), "{NAME}: the 4:2:0 twin decoded no frames");
        // THE WITNESS: the monochrome stream decodes and every luma sample
        // equals ffmpeg's own decode of the same OBU.
        let gray = emit("gray");
        let ours = decode_stream(&gray)
            .unwrap_or_else(|e| panic!("{NAME}: the monochrome stream is REFUSED again: {e}"));
        let (width, height) = (320usize, 240usize);
        const FRAMES: usize = 60;
        assert_eq!(ours.len(), FRAMES, "{NAME}: expected {FRAMES} mono frames");
        let reference = ffmpeg_decode_gray_sequence(&gray, width, height, FRAMES);
        for (i, plane) in reference.chunks_exact(width * height).enumerate() {
            assert_eq!(ours[i].width, width, "{NAME}: frame {i} width");
            assert_eq!(ours[i].height, height, "{NAME}: frame {i} height");
            assert_eq!(
                ours[i].y.len(),
                width * height,
                "{NAME}: frame {i} luma length"
            );
            for (j, (got, want)) in plane.iter().zip(ours[i].y.iter()).enumerate() {
                assert_eq!(
                    *want,
                    u16::from(*got),
                    "{NAME}: frame {i} luma sample {j} (row {}, col {}) is {want}, ffmpeg says {got}",
                    j / width,
                    j % width
                );
            }
        }
        eprintln!(
            "{NAME}: 4:2:0 twin {} frames, monochrome {} frames byte-exact vs ffmpeg",
            twin.len(),
            ours.len()
        );
    }

    /// lane-t900 r25: the refusal "a frame OBU with no tile group" names a
    /// shape the parser cannot hand [`decode_stream`].
    ///
    /// `tiles` reaches that check from exactly two places -- `ObuKind::Frame`'s
    /// own vector and the `ObuKind::TileGroup` accumulation -- and both are
    /// filled by `ec_av1_syntax`'s `parse_tile_group`, whose only success path
    /// pushes one `Tile` per `tile_num` of the INCLUSIVE range
    /// `tg_start..=tg_end`. That range is never empty: the single-tile-group
    /// case takes `(0, num_tiles - 1)` saturating at 0, and the multi-group
    /// case rejects `end < start` as corrupt. So an `OBU_FRAME` either fails to
    /// parse or carries at least one tile, however short its payload is --
    /// including the empty payload the charter for this round asked to build,
    /// which parses into ONE zero-byte tile rather than none.
    ///
    /// Sweeping a real frame OBU's tile payload from empty upwards asserts both
    /// halves: every `Frame` OBU that parses has a tile, and `decode_stream`
    /// never reports this refusal for any of them.
    #[test]
    fn a_frame_obu_that_parses_always_carries_at_least_one_tile() {
        let fctx = &crate::decode::FrameCtx::new();
        const REFUSAL: &str = "a frame OBU with no tile group";
        let picture = test_card(64, 64);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (seq, header) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        let mut frame_obus_parsed = 0;
        for len in 0..=encoded.tile.len().min(24) {
            let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
            stream.extend_from_slice(
                &crate::frame::frame_obu(&seq, &header, &encoded.tile[..len]).unwrap(),
            );
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
                    break;
                };
                pos += obu.total_size;
                if let ObuKind::Frame(_, tiles) = &obu.kind {
                    frame_obus_parsed += 1;
                    assert!(
                        !tiles.is_empty(),
                        "a {len}-byte tile payload parsed into a frame OBU with no tile group -- \
                         the refusal {REFUSAL:?} is reachable after all"
                    );
                }
            }
            if let Err(e) = decode_stream(&stream) {
                let e = e.to_string();
                assert!(
                    !e.contains(REFUSAL),
                    "a {len}-byte tile payload reached {REFUSAL:?}: {e}"
                );
            }
        }
        assert!(
            frame_obus_parsed >= 2,
            "the sweep parsed {frame_obus_parsed} frame OBUs -- it proves nothing about the \
             refusal's reachability"
        );
    }

    /// lane-t900 r27, negative gate: a frame whose segmentation enables one of
    /// the three MODE-OVERRIDING features -- `SEG_LVL_REF_FRAME` (forces a
    /// block's reference), `SEG_LVL_SKIP` and `SEG_LVL_GLOBALMV` (both
    /// suppress symbols `decode_inter_block` reads unconditionally) -- is
    /// refused BY NAME and outputs no picture, rather than desyncing on the
    /// first block that would have obeyed the override.
    ///
    /// Written, not hand-built: `write_segmentation_params` (spec 5.9.14)
    /// codes `feature_enabled`/`feature_data` for every segment, so a real
    /// key frame carrying the flag is a two-line change to the header the
    /// encoder already produced. Swept over all three features and over the
    /// first, a middle and the last segment id.
    #[test]
    fn a_frame_whose_segmentation_overrides_a_block_mode_is_refused_by_name() {
        let fctx = &crate::decode::FrameCtx::new();
        const REFUSAL: &str = "a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV";
        let picture = test_card(64, 64);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (seq, key) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        for feature in [SEG_LVL_REF_FRAME, SEG_LVL_SKIP, SEG_LVL_GLOBALMV] {
            for segment in [0usize, 3, 7] {
                let mut header = key.clone();
                header.segmentation.enabled = true;
                header.segmentation.update_map = true;
                header.segmentation.update_data = true;
                header.segmentation.feature_enabled[segment][feature] = true;
                header.segmentation.feature_data[segment][feature] = 1;
                let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
                stream.extend_from_slice(
                    &crate::frame::frame_obu(&seq, &header, &encoded.tile).unwrap(),
                );
                let mut shown = 0;
                let err = decode_stream_with(&stream, |_, _, _| {
                    shown += 1;
                    Ok(())
                })
                .unwrap_err()
                .to_string();
                assert!(
                    err.contains(REFUSAL),
                    "feature {feature} segment {segment}: expected {REFUSAL:?}, got: {err}"
                );
                assert_eq!(
                    shown, 0,
                    "feature {feature} segment {segment}: a refused frame produced a picture"
                );
            }
        }
    }

    /// lane-hdrlossless WITNESS → lane-av1mixloss LIFT. `--end-usage=q
    /// --cq-level=0 --aq-mode=1` (variance AQ over a lossless base) gives a
    /// high-variance segment a positive `SEG_LVL_ALT_Q` while a flat segment
    /// stays at `qindex == 0` (`aq_variance.c` sets `SEG_LVL_ALT_Q` per
    /// segment and only clamps when `base_qindex != 0`, so a lossless base is
    /// exactly where the mixed case comes from): a frame MIXING lossless and
    /// lossy segments, which this decoder refused by name until the
    /// per-segment lossless table (libaom `decodeframe.c:5205`,
    /// `xd->lossless[segment_id]`; `read_tx_size` returns TX_4X4 before it
    /// reads a symbol, `decodeframe.c:1170`) landed. Non-vacuity, three ways:
    /// (1) the header census proves the arms REALLY carry a mixed frame
    /// (class `gate-blind-to-feature` -- variance AQ over a lossless base is
    /// the whole point, not the flag spelling), (2) the WHT-unit counter
    /// proves the per-segment lossless reconstruction really ran, and (3)
    /// every decoded plane is compared SAMPLE-EXACT against ffmpeg's libaom,
    /// which accepts the same stream -- so the old refusal named a capability
    /// gap, and this gate now proves the gap closed on a real encoder output.
    #[test]
    fn a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact() {
        const NAME: &str = "a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const FRAMES: usize = 4;
        const WIDTH: usize = 256;
        const HEIGHT: usize = 192;
        // (source, per-arm overrides). `--end-usage=q --cq-level=0` is the
        // lossless base; `--aq-mode=1` is variance AQ, the only aomenc path
        // that sets `SEG_LVL_ALT_Q` with no clamp at base 0 (`aq_variance.c`).
        // Several arms because the segment split depends on the encoder's own
        // variance decisions (class knob-never-reached-the-tool: require at
        // least one arm to actually mix, and fail if none does).
        let arms: [(&str, &[&str]); 3] = [
            (
                "testsrc2=size=256x192:rate=25",
                &[
                    "--end-usage=q",
                    "--cq-level=0",
                    "--aq-mode=1",
                    "--good",
                    "--cpu-used=3",
                    "--sb-size=64",
                ],
            ),
            (
                "testsrc2=size=256x192:rate=25",
                &[
                    "--end-usage=q",
                    "--cq-level=0",
                    "--aq-mode=1",
                    "--good",
                    "--cpu-used=5",
                    "--sb-size=64",
                ],
            ),
            (
                "gradients=size=256x192:rate=25",
                &[
                    "--end-usage=q",
                    "--cq-level=0",
                    "--aq-mode=1",
                    "--good",
                    "--cpu-used=6",
                    "--sb-size=64",
                ],
            ),
        ];
        let mut mixed_arms = 0usize;
        // The WHT-unit delta below is a process-wide counter: serialize the
        // arms against the other counter-reading gates.
        let _gate_lock = lock_gate_counters();
        for (src, extra) in &arms {
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    *src,
                    "-t",
                    "0.16",
                    "-pix_fmt",
                    "yuv420p",
                    "-strict",
                    "-1",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(y4m.status.success(), "{NAME}: ffmpeg failed for {src}");
            let limit = format!("--limit={FRAMES}");
            let mut args: Vec<String> = [
                "--codec=av1",
                "--passes=1",
                "--threads=1",
                "--obu",
                "-o",
                "-",
                "-",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
            args.insert(1, limit);
            args.extend(extra.iter().map(|s| (*s).to_owned()));
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            // (2) the stream really carries a frame the old refusal named.
            let mut parser = Av1Parser::new();
            let (mut pos, mut mixed) = (0usize, 0usize);
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).expect("aomenc OBU parses");
                pos += obu.total_size.max(1);
                let header = match &obu.kind {
                    ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => h,
                    _ => continue,
                };
                if header.lossless.iter().any(|&l| l) && !header.lossless.iter().all(|&l| l) {
                    mixed += 1;
                }
            }
            if mixed == 0 {
                eprintln!("{NAME}: arm {extra:?} coded no mixed frame -- trying the next");
                continue;
            }
            // (3) ours DECODES it, sample-exact against ffmpeg's libaom (the
            // reference decode doubles as the libaom-accepts proof). The
            // WHT-unit delta proves the per-segment lossless reconstruction
            // really ran inside a frame that is NOT `CodedLossless`.
            let before = crate::decode::lossless_wht_units();
            let ours = decode_stream(&stream).unwrap_or_else(|e| {
                panic!("{NAME}: a mixed-lossless frame decodes after the per-segment lift: {e}")
            });
            let wht_units = crate::decode::lossless_wht_units() - before;
            let refs = ffmpeg_decode_sequence(&stream, WIDTH, HEIGHT, FRAMES);
            assert_eq!(ours.len(), FRAMES, "{NAME}: frame count");
            for (i, (got, want)) in ours.iter().zip(refs.iter()).enumerate() {
                for (plane, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: arm {extra:?} frame {i} plane {plane}: {bad} samples differ"
                    );
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME}: arm {extra:?} frame {i} plane {plane}: ours has {} samples, \
                         ffmpeg's has {} -- a short plane would zip-truncate the compare above \
                         and hide a chroma-span defect",
                        g.len(),
                        r.len()
                    );
                }
            }
            assert!(
                wht_units > 0,
                "{NAME}: arm {extra:?} reconstructed no Walsh-Hadamard unit -- the mixed census is lying or the lossless path never ran"
            );
            mixed_arms += 1;
            eprintln!(
                "{NAME}: arm {extra:?}: {mixed} mixed frame(s), {FRAMES} frames sample-exact vs ffmpeg, {wht_units} WHT units"
            );
        }
        assert!(
            mixed_arms > 0,
            "{NAME}: no arm produced a mixed lossless/lossy frame -- the witness is vacuous \
             (re-measure the aomenc recipe)"
        );
    }

    /// lane-t900 r27 CENSUS behind the negative gate above: does a real
    /// aomenc ever ENABLE one of the three mode-overriding segmentation
    /// features? Measured over the recipes that turn segmentation on --
    /// `--aq-mode=1/2/3` (variance, complexity and cyclic-refresh AQ) and
    /// `--deltaq-mode=1` -- with the refusal as the instrument: if one of
    /// those features appeared, the decode would stop by name.
    ///
    /// lane-av1seglvl re-scope: r27's arms were all `--passes=1
    /// --lag-in-frames=0`, so they never coded an alt-ref frame or ran an
    /// encoder second pass -- the only shapes where libaom's OTHER seg
    /// feature setup (`configure_static_seg_features`) could ever run. The
    /// re-scoped half below adds alt-ref-group arms and a real two-pass arm,
    /// and upgrades the instrument from the refusal (blind to a pure-ALT_LF
    /// stream) to a direct parse of every frame header's `feature_enabled`
    /// table: over all of it, aomenc enables `SEG_LVL_ALT_Q` and nothing
    /// else.
    #[test]
    fn a_segmentation_census_over_real_aq_streams_finds_no_mode_overriding_feature() {
        if !have_aomenc() {
            eprintln!("SKIP a_segmentation_census_over_real_aq_streams: no aomenc");
            return;
        }
        const REFUSAL: &str = "a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV";
        let recipes: [(&str, Vec<&str>); 6] = [
            ("baseline", vec!["--cpu-used=4", "--cq-level=40"]),
            (
                "aq-mode=1",
                vec!["--cpu-used=4", "--aq-mode=1", "--cq-level=40"],
            ),
            (
                "aq-mode=2",
                vec!["--cpu-used=4", "--aq-mode=2", "--cq-level=40"],
            ),
            (
                "aq-mode=3",
                vec!["--cpu-used=4", "--aq-mode=3", "--cq-level=40"],
            ),
            (
                "aq-mode=3 cq10",
                vec!["--cpu-used=4", "--aq-mode=3", "--cq-level=10"],
            ),
            (
                "deltaq-mode=1",
                vec!["--cpu-used=4", "--deltaq-mode=1", "--cq-level=40"],
            ),
        ];
        // Arrival, not intent (class knob-never-reached-the-tool): a recipe
        // whose stream is byte-for-byte the baseline's changed nothing, and a
        // census over such arms would be vacuous. Measured r27: only
        // `--aq-mode=3` (cyclic refresh) does -- aq-mode 1/2 and deltaq-mode=1
        // all reproduce the baseline's 7147 B at this recipe.
        let mut baseline_bytes = 0usize;
        let mut segment_symbols = 0usize;
        let mut refused = 0usize;
        let mut decoded = 0usize;
        for (name, extra) in &recipes {
            let before = crate::decode::segment_id_hits();
            let (bytes, result) = census_attempt("mandelbrot", 192, 128, 8, extra);
            match result {
                Ok(pictures) => {
                    decoded += 1;
                    let symbols = crate::decode::segment_id_hits() - before;
                    segment_symbols += symbols;
                    if *name == "baseline" {
                        baseline_bytes = bytes;
                    }
                    eprintln!(
                        "SEGCENSUS {name}: {bytes} B{}, {pictures} pictures, segment_id symbols {symbols}",
                        if bytes == baseline_bytes && *name != "baseline" {
                            " (== baseline: the flag changed nothing)"
                        } else {
                            ""
                        }
                    );
                }
                Err(e) => {
                    eprintln!("SEGCENSUS {name}: {bytes} B, refused: {e}");
                    assert!(
                        !e.contains(REFUSAL),
                        "{name}: a real aomenc stream DOES enable a mode-overriding \
                         segmentation feature -- the refusal is a capability gap, not a \
                         claim about the encoder: {e}"
                    );
                    refused += 1;
                }
            }
        }
        eprintln!(
            "SEGCENSUS: {} recipes, {decoded} decoded, {refused} refused for other reasons",
            recipes.len()
        );
        assert_eq!(
            decoded,
            recipes.len(),
            "every recipe must decode; {refused} refused"
        );
        assert!(
            segment_symbols > 0,
            "no recipe coded a single segment_id symbol -- segmentation never arrived, so \
             this census measures nothing about the three mode-overriding features"
        );

        // lane-av1seglvl re-scope: the r27 arms above are all `--passes=1
        // --lag-in-frames=0`, which never codes an alt-ref frame -- the one
        // frame type whose segmentation libaom sets up differently
        // (`configure_static_seg_features`, encoder_utils.c:738, is further
        // gated on the second pass AND on `hl_sf.static_segmentation`, which
        // speed_features.h itself documents as "Always set to 0"). The arms
        // below cover that whole remaining surface: alt-ref groups under
        // `--aq-mode`, a high-cq arm (the branch's own `high_q` gate is
        // `avg_q > 48`), and a genuine two-pass encode. The instrument is no
        // longer the refusal -- the refusal only fires on
        // REF_FRAME/SKIP/GLOBALMV, so an ALT_LF-only stream would decode
        // silently past it -- but a direct parse of every frame header's
        // `feature_enabled` table.
        let lag_recipes: [(&str, &str, usize, usize, Vec<&str>); 3] = [
            (
                "aq-mode=1 lag16 mandelbrot",
                "mandelbrot",
                192,
                128,
                vec![
                    "--cpu-used=4",
                    "--aq-mode=1",
                    "--cq-level=45",
                    "--lag-in-frames=16",
                    "--auto-alt-ref=1",
                ],
            ),
            (
                "aq-mode=1 lag16 screen",
                "testsrc2",
                256,
                192,
                vec![
                    "--cpu-used=4",
                    "--aq-mode=1",
                    "--cq-level=45",
                    "--lag-in-frames=16",
                    "--auto-alt-ref=1",
                ],
            ),
            (
                "aq-mode=1 lag16 cq63 high_q",
                "mandelbrot",
                192,
                128,
                vec![
                    "--cpu-used=4",
                    "--aq-mode=1",
                    "--cq-level=63",
                    "--lag-in-frames=16",
                    "--auto-alt-ref=1",
                ],
            ),
            // `--aq-mode=2 lag16` (complexity AQ over an alt-ref group) was
            // measured 2026-09-24: it encodes and decodes cleanly but codes
            // ZERO segmentation-enabled frames -- complexity AQ never reaches
            // the seg table at this recipe -- so as a census arm it measures
            // nothing (class gate-blind-to-feature) and was dropped; the
            // 1-pass `aq-mode=2` r27 arm above stays.
        ];
        let mut total_enabled = 0usize;
        let mut total_inherited = 0usize;
        let mut total_alt_q = 0usize;
        let mut feature_frames = [0usize; SEG_LVL_MAX];
        for (name, source, w, h, extra) in &lag_recipes {
            let stream = encode_aomenc_stream(source, *w, *h, 40, extra);
            let (enabled, umap, inherited, alt_q, frames_per_feature) =
                segmentation_feature_census(&stream);
            eprintln!(
                "SEGCENSUS {name}: seg-enabled {enabled}, update_map {umap}, inherited {inherited}, ALT_Q {alt_q}, per-feature {frames_per_feature:?}"
            );
            total_enabled += enabled;
            total_inherited += inherited;
            total_alt_q += alt_q;
            for (f, n) in frames_per_feature.iter().enumerate() {
                feature_frames[f] += n;
            }
        }
        // The two-pass arm: `configure_static_seg_features` runs ONLY at
        // `is_stat_consumption_stage_twopass`, so this is the arm that would
        // catch its ALT_LF/REF_FRAME/SKIP tables if the dead speed feature
        // were ever revived.
        {
            let stream = encode_aomenc_stream_twopass(
                "mandelbrot",
                192,
                128,
                40,
                &[
                    "--cpu-used=4",
                    "--aq-mode=1",
                    "--cq-level=45",
                    "--lag-in-frames=16",
                    "--auto-alt-ref=1",
                ],
            );
            let (enabled, umap, inherited, alt_q, frames_per_feature) =
                segmentation_feature_census(&stream);
            eprintln!(
                "SEGCENSUS twopass aq-mode=1 lag16: seg-enabled {enabled}, update_map {umap}, inherited {inherited}, ALT_Q {alt_q}, per-feature {frames_per_feature:?}"
            );
            total_enabled += enabled;
            total_inherited += inherited;
            total_alt_q += alt_q;
            for (f, n) in frames_per_feature.iter().enumerate() {
                feature_frames[f] += n;
            }
        }
        // Premise (class gate-blind-to-feature): the arms must actually reach
        // the shapes the census speaks about -- segmentation-carrying frames,
        // frames whose map is INHERITED (update_map == 0), and frames with a
        // real ALT_Q table -- or the "only ALT_Q" verdict below measures
        // nothing.
        assert!(
            total_enabled > 0 && total_alt_q > 0,
            "no lag/two-pass arm carried a segmentation-enabled frame -- the census premise \
             never arrived"
        );
        assert!(
            total_inherited > 0,
            "no arm carried an update_map == 0 frame -- the map-inheritance path the decoder \
             reads segment ids through is unmeasured"
        );
        // The census verdict, over every frame of every arm: segmentation on
        // aomenc's CLI carries SEG_LVL_ALT_Q and NOTHING else. The three
        // mode-overriding features and the four ALT_LF deltas have no encoder
        // path: `configure_static_seg_features` (the only setter of ALT_LF/
        // REF_FRAME/SKIP in the tree) is gated on `hl_sf.static_segmentation`,
        // documented "Always set to 0" (speed_features.h), and ROI/active-map
        // are control-API only (aomenc.c has no flag for either).
        for (feature, frames) in feature_frames.iter().enumerate() {
            if feature == SEG_LVL_ALT_Q {
                continue;
            }
            assert_eq!(
                *frames, 0,
                "feature index {feature} appeared on {frames} frames of real aomenc output -- \
                 the refusal's encoder-never-writes claim is FALSE and the mode path needs \
                 porting before this census can stay green"
            );
        }
        eprintln!(
            "SEGCENSUS lag/twopass: {total_enabled} seg-enabled frames over 4 arms, {total_inherited} inherited-map frames, {total_alt_q} with ALT_Q, all other SEG_LVL features: 0 frames"
        );
    }

    /// lane-av1seglvl WITNESS: the reaching stream. `--aq-mode=1` turns
    /// segmentation on for every frame; `--lag-in-frames=16 --auto-alt-ref=1`
    /// adds real alt-ref groups, so the stream mixes map-CODING frames
    /// (`update_map == 1`: the key frame and each group's arf) with dozens of
    /// map-INHERITING frames (`update_map == 0`), and an inheriting block's
    /// id is the MIN of the previous map over its own footprint while the
    /// saved map itself inherits that footprint VERBATIM
    /// (`copy_segment_id`, decodemv.c:342) -- heterogeneous footprints are
    /// exactly what a 1:4-split arf leaves behind. Stamping the minified id
    /// (the pre-lane defect) flattened those footprints and mis-dequantized
    /// scattered blocks frames later: 2 luma bytes of mandelbrot frame 35
    /// differed from the oracle, everything else exact.
    ///
    /// Both arms decode EVERY frame in decode order -- hidden arfs included
    /// -- byte-exact against the instrumented oracle, and every SHOWN frame
    /// pixel-exact against ffmpeg; the census counters prove the premise
    /// (segmentation symbols read, a wider-than-zero quantizer span from
    /// `SEG_LVL_ALT_Q`, and an inheritance-heavy shape).
    #[test]
    fn a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact";
        // lane-av1cmpaudit2: this gate reaches the oracle without the gate
        // mutex, so a sibling flipping `EC_AV1_AOMDEC` (the one-byte-wrong
        // oracle control) could hand it a tampered oracle mid-compare.
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc");
            return;
        }
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let extra = [
            "--cpu-used=4",
            "--aq-mode=1",
            "--cq-level=45",
            "--lag-in-frames=16",
            "--auto-alt-ref=1",
        ];
        let arms: [(&str, &str, usize, usize); 2] = [
            ("screen testsrc2", "testsrc2", 256, 192),
            ("film mandelbrot", "mandelbrot", 192, 128),
        ];
        for (arm, source, width, height) in &arms {
            // r7: the ENCODE's frame count, bound so the compare below asserts a
            // spec rather than our own decode's length.
            const ENCODED_FRAMES: usize = 40;
            let stream = encode_aomenc_stream(source, *width, *height, ENCODED_FRAMES, &extra);
            // Premise: the shape under test really arrived -- segmentation on
            // every frame, the map both coded and inherited, and real ALT_Q
            // tables. An aomenc drift that dropped any of these would make
            // the decode below prove nothing (class gate-blind-to-feature).
            let (enabled, umap, inherited, alt_q, _) = segmentation_feature_census(&stream);
            eprintln!(
                "{NAME} {arm}: seg-enabled {enabled}, update_map {umap}, inherited {inherited}, ALT_Q-carrying {alt_q}"
            );
            assert!(
                enabled > 0 && umap > 0,
                "{arm}: no segmentation-carrying/map-coding frame -- the premise is gone"
            );
            assert!(
                inherited > 0,
                "{arm}: no update_map == 0 frame -- the map-inheritance path under test never runs"
            );
            assert!(
                alt_q > 0,
                "{arm}: no frame carries a SEG_LVL_ALT_Q table -- the feature under test never fires"
            );
            // Every frame in decode order -- hidden arfs included (each one
            // re-surfaces via show-existing in this recipe, so `hidden == 0`
            // is possible while `frames` still covers the whole coded set) --
            // byte-exact against the oracle.
            let (frames, hidden) = decode_all_frames_vs_oracle(&stream, arm);
            assert!(frames > 0, "{arm}: decoded nothing");
            assert_eq!(
                frames, enabled,
                "{arm}: decode-order frame count != segmentation-enabled frame count -- the \
                 parse and the decode disagree about the stream"
            );
            let _ = hidden;
            // And every shown frame pixel-exact against ffmpeg.
            crate::decode::reset_segment_hits();
            let pictures = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} {arm}: decode refused: {e}"));
            assert!(
                crate::decode::segment_id_hits() > 0,
                "{arm}: zero segment_id symbols read -- the decode proved nothing about \
                 segmentation"
            );
            let (lo, hi) = crate::decode::block_q_span();
            assert!(
                hi > lo,
                "{arm}: every block dequantized at the same qindex ({lo}) -- SEG_LVL_ALT_Q \
                 never changed any block's quantizer"
            );
            // `ENCODED_FRAMES`, not `pictures.len()`: our decode's length is the
            // thing UNDER TEST, so using it as the oracle's expected count is
            // circular -- a decode that dropped a frame would ask ffmpeg for the
            // same short count and the compare would silently shrink.
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, *width, *height, ENCODED_FRAMES);
            for (i, (ours, theirs)) in pictures.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(ours.y, theirs.y, "{NAME} {arm}: frame {i} luma vs ffmpeg");
                assert_eq!(ours.u, theirs.u, "{NAME} {arm}: frame {i} U vs ffmpeg");
                assert_eq!(ours.v, theirs.v, "{NAME} {arm}: frame {i} V vs ffmpeg");
            }
            eprintln!(
                "{NAME} {arm}: {frames} frames decode-order exact vs oracle ({hidden} hidden), {} shown frames exact vs ffmpeg, q span {lo}..{hi}, segment_id symbols {}",
                pictures.len(),
                crate::decode::segment_id_hits()
            );
        }
    }

    /// lane-t900 r27, WITNESS half of the proof for "an OBMC neighbour whose
    /// switchable interp filter was never recorded" (the structural half is
    /// `decode::tests::every_inter_record_publishes_an_obmc_readable_filter`,
    /// which shows no INTER cell is ever stamped with the `[3, 3]`
    /// sentinel): real OBMC streams whose frames use a SWITCHABLE (and dual)
    /// interp filter decode with the refusal firing 0 times while OBMC blends
    /// really read the neighbour band -- a decode that never read one would
    /// pass just as well and prove nothing (class gate-blind-to-feature).
    #[test]
    fn a_real_obmc_stream_reads_a_recorded_switchable_filter_for_every_neighbour() {
        if !have_aomenc() {
            eprintln!("SKIP a_real_obmc_stream_reads_a_recorded_switchable_filter: no aomenc");
            return;
        }
        // `--cpu-used=0` is load-bearing: at speed 4 aomenc settles on a
        // FRAME-level fixed interp filter, every neighbour then legitimately
        // carries the sentinel, and the band is never read at all (measured
        // r27: 0 reads over two speed-4 streams).
        let base = vec![
            "--cpu-used=0",
            "--enable-obmc=1",
            "--enable-dual-filter=1",
            "--min-partition-size=8",
            "--max-partition-size=16",
            "--enable-palette=0",
            "--enable-intrabc=0",
        ];
        let recipes: [(&str, usize, usize, Vec<&str>); 2] = [
            (
                "obmc dual cq40",
                192,
                128,
                [vec!["--cq-level=40"], base.clone()].concat(),
            ),
            (
                "obmc dual cq20",
                192,
                128,
                [vec!["--cq-level=20"], base.clone()].concat(),
            ),
        ];
        let mut reads = 0usize;
        let mut decoded = 0usize;
        for (name, w, h, extra) in &recipes {
            let before = crate::decode::obmc_filter_band_reads();
            let obmc_before = crate::decode::obmc_hits();
            let (bytes, result) = census_attempt("mandelbrot", *w, *h, 8, extra);
            match result {
                Ok(pictures) => {
                    decoded += 1;
                    let got = crate::decode::obmc_filter_band_reads() - before;
                    reads += got;
                    eprintln!(
                        "OBMCFILT {name}: {bytes} B, {pictures} pictures, {got} band reads, \
                         {} OBMC blocks",
                        crate::decode::obmc_hits() - obmc_before
                    );
                }
                Err(e) => {
                    assert!(
                        !e.contains(
                            "an OBMC neighbour whose switchable interp filter was never recorded"
                        ),
                        "{name}: a real OBMC stream reached the unrecorded-filter refusal -- \
                         a publication site is missing: {e}"
                    );
                    eprintln!("OBMCFILT {name}: {bytes} B, refused for another reason: {e}");
                }
            }
        }
        assert!(
            decoded > 0,
            "no OBMC recipe decoded -- the witness is vacuous"
        );
        assert!(
            reads > 0,
            "every OBMC blend took the fixed-filter path: the band was never read, so this \
             proves nothing about the refusal"
        );
    }

    /// lane-t900 r27 CENSUS for the three SUPERRES refusals -- "warp
    /// prediction with a scaled reference", "a sub-8x8 inter block under a
    /// scaled reference" and "an 8x8 partition leaf under a scaled reference".
    /// Six real `--superres-mode` encodes (fixed denominators and the auto
    /// mode, warped motion and OBMC on), decoded by this decoder: the table it
    /// prints is which of the three a real encoder actually reaches.
    ///
    /// Every one of them is a REAL capability gap (unlike the enumerated
    /// refusals elsewhere in the inventory), so this gate asserts the measured
    /// shape rather than "no refusal": at least one recipe must decode (else
    /// the census says nothing), and any refusal must be one of the three
    /// named superres ones -- a fourth string would be a new gap this round
    /// did not measure.
    #[test]
    fn a_superres_census_over_six_real_streams_records_which_scaled_refusals_fire() {
        if !have_aomenc() {
            eprintln!("SKIP a_superres_census_over_six_real_streams: no aomenc");
            return;
        }
        const WARP: &str = "warp prediction with a scaled reference";
        const SUB8: &str = "a sub-8x8 inter block under a scaled reference";
        const LEAF8: &str = "an 8x8 partition leaf under a scaled reference";
        let common = [
            "--enable-warped-motion=1",
            "--enable-obmc=1",
            "--enable-palette=0",
            "--enable-intrabc=0",
        ];
        let recipes: [(&str, usize, usize, Vec<&str>); 6] = [
            (
                "denom12 320x180 cq32",
                320,
                180,
                vec![
                    "--cpu-used=4",
                    "--superres-mode=1",
                    "--superres-denominator=12",
                    "--cq-level=32",
                ],
            ),
            (
                "denom12 320x180 cq50",
                320,
                180,
                vec![
                    "--cpu-used=4",
                    "--superres-mode=1",
                    "--superres-denominator=12",
                    "--cq-level=50",
                ],
            ),
            (
                "denom16 320x180 cq32",
                320,
                180,
                vec![
                    "--cpu-used=4",
                    "--superres-mode=1",
                    "--superres-denominator=16",
                    "--cq-level=32",
                ],
            ),
            (
                "denom10 192x128 cq32",
                192,
                128,
                vec![
                    "--cpu-used=4",
                    "--superres-mode=1",
                    "--superres-denominator=10",
                    "--cq-level=32",
                ],
            ),
            (
                "auto 320x180 cq50",
                320,
                180,
                vec!["--cpu-used=4", "--superres-mode=3", "--cq-level=50"],
            ),
            (
                "auto 192x128 cq50",
                192,
                128,
                vec!["--cpu-used=4", "--superres-mode=3", "--cq-level=50"],
            ),
        ];
        let (mut decoded, mut warp, mut sub8, mut leaf8) = (0, 0, 0, 0);
        // lane-t900 r36 (class [[gate-blind-to-feature]]): unknown refusal
        // strings used to `other += 1` and panic on the spot, so the census
        // summary below -- the whole point of the gate -- never printed when
        // one fired. They are collected instead, reported in the summary, and
        // asserted after it.
        let mut other: Vec<String> = Vec::new();
        for (name, w, h, extra) in &recipes {
            let mut args = extra.clone();
            args.extend_from_slice(&common);
            let before = crate::mc::predict_scaled_hits();
            let (bytes, result) = census_attempt("mandelbrot", *w, *h, 10, &args);
            match result {
                Ok(pictures) => {
                    decoded += 1;
                    eprintln!(
                        "SUPERRES {name}: {bytes} B, {pictures} pictures, scaled MC blocks {}",
                        crate::mc::predict_scaled_hits() - before
                    );
                }
                Err(e) => {
                    eprintln!("SUPERRES {name}: {bytes} B, refused: {e}");
                    if e.contains(WARP) {
                        warp += 1;
                    } else if e.contains(SUB8) {
                        sub8 += 1;
                    } else if e.contains(LEAF8) {
                        leaf8 += 1;
                    } else {
                        other.push(format!("{name}: {e}"));
                    }
                }
            }
        }
        eprintln!(
            "SUPERRES CENSUS: 6 streams, {decoded} decoded, warp {warp}x, sub8 {sub8}x, leaf8 {leaf8}x, other {}x",
            other.len()
        );
        assert!(
            other.is_empty(),
            "{} superres stream(s) stopped on a string this census does not know: {other:?}",
            other.len()
        );
        // Measured r27: the four FIXED-denominator recipes all stop at the
        // 8x8-leaf refusal, and the two `--superres-mode=3` (auto) streams
        // decode with ZERO scaled MC blocks -- auto picked denominator 8, so
        // those two arms carry no scaling at all and prove nothing. The warp
        // and sub-8x8 refusals never fire because the 8x8-leaf one sits in
        // front of them: whichever partition path a scaled frame takes, the
        // leaf guard is reached first. Lifting the leaf gap is what makes the
        // other two measurable, so this gate pins the ORDER, not just a count.
        assert!(
            decoded > 0,
            "no superres recipe decoded -- the census is vacuous"
        );
        // lane-t900 r28: all three scaled refusals are LIFTED, so the census
        // now pins the opposite shape -- every fixed-denominator recipe must
        // DECODE, and none of the three strings may come back.
        assert_eq!(
            (warp, sub8, leaf8),
            (0, 0, 0),
            "a scaled-reference refusal came back (warp {warp}, sub8 {sub8}, leaf8 {leaf8})"
        );
        assert!(
            decoded > 0,
            "no superres recipe decoded -- the census is vacuous"
        );
    }

    /// lane-t900 r25, negative gate: a `show_existing_frame` header naming a
    /// slot no frame has refreshed is refused BY NAME, and outputs no picture.
    ///
    /// The OBU is hand-built because [`crate::frame::write_frame_header`]
    /// refuses to write one (it writes shown key and inter frames only): spec
    /// 5.9.2's whole header for this case is `show_existing_frame` (1) plus
    /// `frame_to_show_map_idx` (3), the sequence carrying neither a decoder
    /// model nor frame ids.
    ///
    /// Deviation from the round's charter, which asked for a key frame first
    /// with the named slot's `refresh_frame_flags` bit clear: a SHOWN key frame
    /// refreshes every slot (spec 5.9.2 forces `refresh_frame_flags` to
    /// `all_frames` and does not code it, which is also what
    /// `write_frame_header` does), so the only reachable empty-slot stream is
    /// one where no frame has refreshed that slot yet. The stream below is a
    /// valid sequence header followed by the header OBU, and every one of the
    /// eight slots is tried.
    #[test]
    fn a_show_existing_frame_header_naming_an_empty_slot_is_refused_by_name() {
        const REFUSAL: &str = "a show_existing_frame header naming an empty reference slot";
        let (seq, _) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        for slot in 0..NUM_REF_FRAMES as u32 {
            let mut w = ec_core::BitWriter::new();
            w.write_bit(true); // show_existing_frame
            w.write_bits(slot, 3); // frame_to_show_map_idx
            crate::bits::write_byte_alignment(&mut w);
            let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
            stream.extend_from_slice(&crate::obu::wrap_obu(
                &ec_av1_syntax::obu::ObuHeader {
                    obu_type: ec_av1_syntax::obu::ObuType::FrameHeader,
                    extension_flag: false,
                    has_size_field: true,
                    temporal_id: 0,
                    spatial_id: 0,
                },
                &w.into_bytes(),
            ));

            let mut shown = 0;
            let err = decode_stream_with(&stream, |_, _, _| {
                shown += 1;
                Ok(())
            })
            .unwrap_err()
            .to_string();
            assert!(
                err.contains(REFUSAL),
                "slot {slot}: expected {REFUSAL:?}, got: {err}"
            );
            assert_eq!(
                shown, 0,
                "slot {slot}: an empty slot was output as a picture"
            );
        }
    }

    /// lane-t900 r25, negative gate: an inter frame whose `ref_frame_idx[0]`
    /// slot holds no picture -- a stream that opens on an inter frame -- is
    /// refused BY NAME rather than predicting from nothing.
    ///
    /// Deviation from the round's charter, which asked for a real aomenc
    /// two-frame stream with the key frame's OBUs stripped: this crate's own
    /// `write_frame_header` writes a shown INTER frame (short reference
    /// signalling and non-identity global motion are the parts it refuses), so
    /// the fixture is built from the writers, needs neither ffmpeg nor aomenc,
    /// and is byte-identical on every run. The tile payload is a real key
    /// frame's; the refusal fires on the header's reference lookup, before any
    /// tile is decoded.
    #[test]
    fn an_inter_frame_opening_a_stream_is_refused_by_name() {
        let fctx = &crate::decode::FrameCtx::new();
        const REFUSAL: &str = "an inter frame with no key frame before it";
        let picture = test_card(64, 64);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (seq, key) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        let inter = FrameHeader {
            frame_type: FrameType::Inter,
            frame_is_intra: false,
            // Keeps `primary_ref_frame`, `frame_size_with_refs` and
            // `use_ref_frame_mvs` out of the written header (spec 5.9.2).
            error_resilient_mode: true,
            primary_ref_frame: PRIMARY_REF_NONE,
            refresh_frame_flags: 0x02,
            order_hint: 1,
            ..key
        };
        let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
        stream.extend_from_slice(&crate::frame::frame_obu(&seq, &inter, &encoded.tile).unwrap());

        let mut shown = 0;
        let err = decode_stream_with(&stream, |_, _, _| {
            shown += 1;
            Ok(())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains(REFUSAL), "expected {REFUSAL:?}, got: {err}");
        assert_eq!(
            shown, 0,
            "an inter frame with no reference produced a picture"
        );
    }

    /// lane-t900 r26, negative gate: an inter frame whose own coded size is
    /// SHORTER than the reference it predicts from is refused BY NAME, and
    /// outputs no picture beyond the key frame that preceded it.
    ///
    /// The height mismatch is the one size difference this decoder cannot
    /// absorb: `mc::predict_scaled` scales horizontally (a `use_superres`
    /// reference), and nothing on this path scales vertically. The fixture is
    /// written, not hand-built: a shown key frame at 64x64 (which refreshes
    /// every slot), then a shown inter frame carrying
    /// `frame_size_override_flag` with `error_resilient_mode` set -- the one
    /// combination [`crate::frame::write_frame_header`] writes, since
    /// `frame_size_with_refs` (spec 5.9.7) is only read when error resilience
    /// is OFF. The inter frame's tile payload is the key frame's; the refusal
    /// fires on the reference's dimensions before a tile symbol is read. A 31
    /// row frame against a 64 row reference is past the 2x down limit.
    #[test]
    fn an_inter_frame_shorter_than_its_reference_is_refused_by_name() {
        let fctx = &crate::decode::FrameCtx::new();
        const REFUSAL: &str =
            "a reference picture outside the 2x down / 16x up scaling limits";
        let picture = test_card(64, 64);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (seq, key) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        let inter = FrameHeader {
            frame_type: FrameType::Inter,
            frame_is_intra: false,
            error_resilient_mode: true,
            primary_ref_frame: PRIMARY_REF_NONE,
            refresh_frame_flags: 0x02,
            order_hint: 1,
            frame_size_override_flag: true,
            frame_height: 31,
            render_height: 31,
            ..key.clone()
        };
        let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
        stream.extend_from_slice(&crate::frame::frame_obu(&seq, &key, &encoded.tile).unwrap());
        stream.extend_from_slice(&crate::frame::frame_obu(&seq, &inter, &encoded.tile).unwrap());

        let mut shown = 0;
        let err = decode_stream_with(&stream, |_, _, _| {
            shown += 1;
            Ok(())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains(REFUSAL), "expected {REFUSAL:?}, got: {err}");
        assert_eq!(
            shown, 1,
            "expected the key frame alone, got {shown} pictures"
        );
    }

    /// lane-t900 r26, negative gate: an inter frame naming a
    /// `primary_ref_frame` whose reference slot has never been refreshed is
    /// refused BY NAME rather than decoded from the spec 8.4 default tables.
    ///
    /// Enumeration first, because the refusal's premise is an invariant: the
    /// picture slots and the CDF slots are written by exactly two loops in
    /// [`decode_stream`] (the `show_existing_frame` arm's copy and the
    /// end-of-frame store), and both walk the SAME `refresh_frame_flags` mask
    /// writing `ref_slots[i]` and `cdf_slots[i]` together -- so no slot can
    /// hold a picture without CDFs, and the reachable case is a slot nothing
    /// has refreshed at all. That is what this builds: a stream that OPENS on
    /// a non-error-resilient inter frame (the only header shape that codes
    /// `primary_ref_frame` at all, spec 5.9.2), tried at all eight slots.
    ///
    /// This refusal, not "an inter frame with no key frame before it", is the
    /// one such a stream reaches: the CDF load happens in the header path,
    /// before the reference pictures are looked up.
    #[test]
    fn an_inter_frame_naming_an_unrefreshed_primary_ref_slot_is_refused_by_name() {
        let fctx = &crate::decode::FrameCtx::new();
        const REFUSAL: &str =
            "a frame naming primary_ref_frame at a reference slot with no saved CDF state";
        // The invariant above, read off the source: every line that stores a
        // picture into a slot sits in a block that stores that slot's CDFs too.
        let src = crate::test_source::module_source("stream");
        let stores: Vec<usize> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim().starts_with("ref_slots[i] = "))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            stores.len(),
            2,
            "the picture slots gained a writer -- check it stores cdf_slots too"
        );
        let lines: Vec<&str> = src.lines().collect();
        for &i in &stores {
            let window = lines[i.saturating_sub(6)..(i + 6).min(lines.len())].join("\n");
            assert!(
                window.contains("cdf_slots[i] = "),
                "line {} stores a reference picture with no cdf_slots store beside it: a slot \
                 can then hold a picture with no saved CDF state",
                i + 1
            );
        }

        let picture = test_card(64, 64);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (seq, key) = crate::encode::key_frame_headers(64, 64, 100).unwrap();
        for slot in 0..NUM_REF_FRAMES as u8 {
            let inter = FrameHeader {
                frame_type: FrameType::Inter,
                frame_is_intra: false,
                // `primary_ref_frame` is coded only when error resilience is
                // off (spec 5.9.2); `frame_size_override_flag` stays clear so
                // the header writes a plain `frame_size()`.
                error_resilient_mode: false,
                primary_ref_frame: 0,
                ref_frame_idx: [slot; ec_av1_syntax::REFS_PER_FRAME],
                refresh_frame_flags: 0x02,
                order_hint: 1,
                ..key.clone()
            };
            let mut stream = crate::sequence::sequence_header_obu(&seq).unwrap();
            stream
                .extend_from_slice(&crate::frame::frame_obu(&seq, &inter, &encoded.tile).unwrap());

            let mut shown = 0;
            let err = decode_stream_with(&stream, |_, _, _| {
                shown += 1;
                Ok(())
            })
            .unwrap_err()
            .to_string();
            assert!(
                err.contains(REFUSAL),
                "slot {slot}: expected {REFUSAL:?}, got: {err}"
            );
            assert_eq!(
                shown, 0,
                "slot {slot}: a frame with no CDF state produced a picture"
            );
        }
    }

    /// lane-t900 r26, enumeration: no frame header a stream can carry derives
    /// an empty mode-info grid, so "a frame with no mode-info grid" names a
    /// case outside the coded domain.
    ///
    /// `compute_image_size` (spec 5.9.9) is `mi_cols = 2 * ((frame_width + 7)
    /// >> 3)`, which is 0 only for `frame_width == 0`, and a frame width is
    /// > > never 0 on any of the three paths that set one:
    ///
    /// * `frame_size()` (5.9.5) reads `frame_width_minus_1` and adds one, or
    ///   takes the sequence's `max_frame_width` (itself `+ 1`-coded);
    /// * `superres_params()` (5.9.8) divides by a denominator of 9..=16 with
    ///   rounding, whose minimum over the whole 16-bit upscaled-width domain
    ///   is enumerated below and is 1, not 0;
    /// * `show_existing_frame` and `frame_size_with_refs()` copy a slot's own
    ///   dimensions, which came from one of the above.
    ///
    /// The sweep drives the REAL header reader (write a header, parse it back)
    /// over every width and every height a 16-bit `frame_size()` can code,
    /// per axis -- the derivation is independent per axis -- rather than
    /// re-deriving the formula here.
    #[test]
    fn every_frame_size_a_header_can_code_has_a_mode_info_grid() {
        // lane-av1refusalspan: this gate is the PROOF for the refusal below, so
        // it names it and pins its guard site (the decode-path guard in
        // decode.rs carries the same string).
        const REFUSAL: &str = "a frame with no mode-info grid";
        assert!(
            crate::refusal_inventory::pins_refusal(crate::test_source::module_source("stream"), REFUSAL),
            "this gate proves {REFUSAL:?}, but that string is no longer in stream.rs"
        );
        let mut sizes: Vec<u32> = (1..=1024).collect();
        sizes.extend([2048, 4095, 4096, 8192, 16384, 32768, 65535, 65536]);
        // `mi_cols` is nondecreasing in `frame_width`, so an empty grid can
        // only appear at the SMALL end -- which is why a size this crate's
        // tile-info writer cannot express (its tile log2 range runs out well
        // before 65536) is skipped rather than worked around, while every
        // size up to 1024 must be covered.
        let mut skipped: Vec<u32> = Vec::new();
        for n in sizes {
            for (w, h) in [(n, 16), (16, n)] {
                let Ok((seq, header)) =
                    crate::encode::key_frame_headers(w as usize, h as usize, 100)
                else {
                    skipped.push(n);
                    continue;
                };
                let Ok(stream) = crate::frame::frame_obu(&seq, &header, &[]) else {
                    skipped.push(n);
                    continue;
                };
                let mut parser = Av1Parser::new();
                parser
                    .parse_obu(&crate::sequence::sequence_header_obu(&seq).unwrap())
                    .unwrap();
                let obu = parser.parse_obu(&stream).unwrap();
                let ObuKind::Frame(parsed, _) = &obu.kind else {
                    panic!("{w}x{h}: the written frame OBU did not parse as one");
                };
                assert!(
                    parsed.mi_cols >= 2 && parsed.mi_rows >= 2,
                    "{w}x{h} parsed to a {}x{} mode-info grid",
                    parsed.mi_cols,
                    parsed.mi_rows
                );
            }
        }
        assert!(
            skipped.iter().all(|&n| n > 1024),
            "the writer could not express these small frame sizes, so the small end of the \
             sweep is incomplete: {skipped:?}"
        );
        // The superres arm, which no writer here can express: spec 5.9.8's
        // `(upscaled_width * 8 + denom / 2) / denom` over its whole domain.
        for denom in 9u32..=16 {
            for upscaled in [1u32, 2, 3, 7, 8, 9, 255, 4096, 65535, 65536] {
                let width = (upscaled * 8 + denom / 2) / denom;
                assert!(
                    width >= 1,
                    "superres denom {denom} maps an upscaled width of {upscaled} to {width}, \
                     which would derive an empty mode-info grid"
                );
            }
        }
    }

    /// lane-interedge r1 GATE (was lane-oddh r2's `#[ignore]`d twin): the INTER
    /// frame-edge half strips -- a `PARTITION_HORZ`/`PARTITION_VERT` forced by
    /// the gathered edge symbol at the 64, 32 or 16 level whose second half lies
    /// outside the true frame. Both of the user's films are height mod 16 == 8
    /// (1920x792, 3840x1608), so every inter frame crosses these nodes.
    ///
    /// r1 root cause: the inter tile's 32-level `PARTITION_HORZ`/`PARTITION_VERT`
    /// decoded the SECOND strip unconditionally, while libaom `decode_partition`
    /// (decodeframe.c, PARTITION_HORZ) guards it with `has_rows`/`has_cols` --
    /// so at the edge we read symbols the encoder never wrote. With that guard
    /// removed this gate's 72x192 cq 32 arm refuses outright and its 104x192 arm
    /// decodes luma rows 127..191 of frame 5 wrong (measured r1).
    ///
    /// The compare is LUMA-exact per frame plus a CONTROL size with no edge node
    /// at all: chroma is excluded because a chroma-only inter residue that has
    /// nothing to do with the frame edge is open on main (lanes/tiny-r3,
    /// lanes/gmaffine-r4, lanes/palette2-r8) and the control below reproduces it
    /// with `inter_edge_strip_hits` all zero.
    #[test]
    fn a_real_aomenc_inter_frame_edge_half_strip_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_inter_frame_edge_half_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let frames = 6usize;
        // h mod 16 == 8 (bottom edge) and w mod 16 == 8 (right edge).
        let sizes: &[(usize, usize)] = &[(192, 72), (192, 88), (72, 192), (88, 192), (104, 192)];
        let mut totals = [0usize; 6];
        let mut compared = 0u32;
        let mut chroma_frames = 0usize;
        for &(width, height) in sizes {
            for bit_depth in [8u32, 10] {
                for cq in [20u32, 32, 45] {
                    let (hits, luma_bad, chroma_bad) =
                        inter_edge_strip_attempt(NAME, width, height, bit_depth, cq, frames);
                    assert_eq!(
                        luma_bad, 0,
                        "{NAME}: {width}x{height} {bit_depth}-bit cq {cq}: {luma_bad} of {frames} \
                         frames differ from ffmpeg in LUMA (edge hits {hits:?})"
                    );
                    // merge J, after lane-rectchroma2: the chroma residue this
                    // gate used to tolerate WAS this decoder's -- the per-axis
                    // narrow-block MC kernel decision read side x side instead
                    // of the block's true dims. Full-plane now.
                    assert_eq!(
                        chroma_bad, 0,
                        "{NAME}: {width}x{height} {bit_depth}-bit cq {cq}: {chroma_bad} of \
                         {frames} frames differ from ffmpeg in CHROMA (edge hits {hits:?})"
                    );
                    for (t, h) in totals.iter_mut().zip(hits) {
                        *t += h;
                    }
                    chroma_frames += chroma_bad;
                    compared += 1;
                }
            }
        }
        // Every level x axis this recipe reaches must have fired on a COMPARED
        // attempt. h32 (a bottom-edge HORZ at the 32 level) is not asserted: no
        // recipe found in r1 makes libaom answer that gathered symbol HORZ
        // (sweep ~/.cache/interedge-tmp/h32.sh, 3 sizes x 6 cq, all SPLIT); it
        // is the transposed twin of v32, which this gate does fire.
        assert!(
            totals[0] > 0 && totals[1] > 0 && totals[3] > 0 && totals[4] > 0 && totals[5] > 0,
            "{NAME}: {compared} attempts luma-exact but the edge strips did not all fire \
             (h64,v64,h32,v32,h16,v16 = {totals:?}) -- the recipe no longer reaches the arms \
             this gate exists for"
        );
        // CONTROL: a size with no frame-edge node anywhere (192x128, both axes
        // 0 mod 64). It must be luma-exact with ZERO edge hits -- that is what
        // makes the chroma exclusion above a statement about a foreign defect
        // rather than about this lane's code.
        let (chits, cluma, cchroma) = inter_edge_strip_attempt(NAME, 192, 128, 8, 32, frames);
        assert_eq!(
            (chits, cluma, cchroma),
            ([0usize; 6], 0usize, 0usize),
            "{NAME}: control 192x128 (no frame-edge node) must be pixel-exact with no edge hits"
        );
        eprintln!(
            "{NAME}: {compared} attempts luma-exact over {} sizes, edge hits \
             (h64,v64,h32,v32,h16,v16)={totals:?}, 0 refusals; chroma-only frames {chroma_frames} \
             (control 192x128: hits {chits:?}, chroma-only frames {cchroma} -- foreign defect)",
            sizes.len()
        );
    }

    /// lane-sb128 r4 GATE (was lane-oddh r2's refusal gate): the SAME real
    /// aomenc 64x72 `--cpu-used=0 --sb-size=128` stream that once desynced at
    /// the very first symbol now decodes PIXEL-EXACT against aomdec. mi_rows
    /// = 18 > 16, so the BLOCK_128X128 root reads a real gathered partition
    /// symbol (2 symbols: has_cols false, has_rows true) -- exactly the case
    /// r1 mis-read as a 10-symbol BLOCK_64X64 partition.
    ///
    /// The `use_128x128_superblock` assert is the "knob reached the tool"
    /// half: without it an aomenc that ignored `--sb-size=128` would leave
    /// the gate proving nothing about 128 superblocks.
    #[test]
    fn a_real_aomenc_128x128_superblock_stream_decodes_pixel_exact() {
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (64usize, 72usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "mandelbrot=size=64x72:rate=25:start_x=-0.6:start_y=-0.4",
                "-t",
                "0.04",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=32",
                "--cpu-used=0",
                "--sb-size=128",
                "--min-partition-size=16",
                "--kf-max-dist=0",
                "--limit=1",
                "--threads=1",
                "--row-mt=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "aomenc refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;

        // The knob actually arrived: this stream's sequence header codes
        // `use_128x128_superblock = 1`.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut sb128 = false;
        while pos < stream.len() {
            let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
                break;
            };
            pos += obu.total_size.max(1);
            if let Some(seq) = parser.sequence_header() {
                sb128 = seq.use_128x128_superblock;
                break;
            }
        }
        assert!(
            sb128,
            "aomenc ignored --sb-size=128: this stream is 64x64-superblock"
        );

        let _guard = lock_gate_counters();
        let before = crate::decode::part128_symbols() + crate::decode::part128_gathered_symbols();
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, "sb128-oddh-64x72");
        assert!(frames > 0, "no frames decoded for {width}x{height}");
        // The 128 root actually READ a partition symbol here: at mi_rows=18
        // `has_rows` is true, so BLOCK_128X128 is not a forced split.
        let part128 =
            crate::decode::part128_symbols() + crate::decode::part128_gathered_symbols() - before;
        assert!(
            part128 > 0,
            "{width}x{height} decoded without reading a single BLOCK_128X128 \
             partition symbol -- the gate proves nothing about the 128 root"
        );
        eprintln!(
            "a_real_aomenc_128x128_superblock_stream_decodes_pixel_exact: \
             {frames} frames pixel-exact ({hidden} hidden) at {width}x{height}, \
             part128_symbols={part128}"
        );
    }

    /// `decode_stream` on a lone key frame's stream matches both the tile
    /// path (`decode_key_frame_tile` called directly on `Encoded::tile`) and
    /// the encoder's own reconstruction, at an even and an odd size.
    #[test]
    fn decode_stream_matches_the_tile_path_on_a_key_frame() {
        // lane-b128m: the ENCODER writes at this process's superblock size, so
        // its own trial decode -- and the raw tile decode below -- must be given
        // that size too (class: stale header in the test harness).
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for &(width, height) in &[(64usize, 64usize), (216, 96)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
            let via_tile = decode_key_frame_tile_lr(
                &encoded.tile,
                encoded.mi_cols,
                encoded.mi_rows,
                encoded.base_q_idx,
                width as u32,
                height as u32,
                // lane-fintra: the sequence bit the encoder wrote this tile under
                // (stale-header class -- a hard-coded `false` here reads one symbol
                // short at the first DC_PRED block once the search offers filter intra).
                crate::encode::filter_intra_on(),
                &encoded.cdef,
                &encoded.loop_filter,
                encoded.tx_select,
                // lane-intertx: the frame's own `reduced_tx_set` -- a screen
                // key frame at preset 0 codes the wider intra alphabets, so
                // the old hardcoded `true` (ours "always" wrote true) is a
                // stale-header bug waiting for the first screen fixture.
                encoded.reduced_tx_set,
                // `allow_screen_content_tools` off the frame the encoder
                // actually wrote: a screen-content picture codes two palette
                // symbols per intra block, and a raw tile decode that guessed
                // `false` desyncs at the first one (class: test asserts
                // against a stale header).
                encoded.screen,
                // ... and its own `allow_intrabc` (stale-header class).
                encoded.allow_intrabc,
                &encoded.loop_restoration,
                fctx,
            )
            .unwrap();
            let via_stream = decode_stream(&encoded.stream).unwrap();
            assert_eq!(via_stream.len(), 1, "{width}x{height}: one picture");
            assert_eq!(via_stream[0].y, via_tile.y, "{width}x{height}: luma");
            assert_eq!(via_stream[0].u, via_tile.u, "{width}x{height}: U");
            assert_eq!(via_stream[0].v, via_tile.v, "{width}x{height}: V");
            assert_eq!(
                via_stream[0].y, encoded.reconstruction.y,
                "{width}x{height}: luma vs encoder reconstruction"
            );
        }
    }

    #[test]
    fn decode_stream_round_trips_a_gop() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        gop_round_trips(128, 64, fctx);
    }

    /// Same claim at a size that is not a whole number of 32x32 blocks, so
    /// the inter frame's 16x16-leaf split path is exercised too.
    #[test]
    fn decode_stream_round_trips_an_odd_size_gop() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        gop_round_trips(216, 96, fctx);
    }

    /// lane-streamsink r1: `decode_stream_with` emits exactly the pictures
    /// `decode_stream` collects, in the same order -- the streaming path is
    /// the decoder now (`decode_stream` is a collecting wrapper), so an
    /// equivalence check here is what keeps every gate's Vec-shaped assert
    /// meaningful. Also pins the sink contract: one call per shown picture,
    /// decode indices non-decreasing, and an `Err` from the sink aborting.
    #[test]
    fn streaming_decode_matches_the_collecting_one() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let pictures: Vec<_> = (0..4).map(|i| panned_test_card(128, 64, i * 3)).collect();
        let stream = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
            .unwrap()
            .stream;
        let collected = decode_stream(&stream).unwrap();

        let mut shown = Vec::new();
        let mut idxs = Vec::new();
        let mut live = 0usize;
        decode_stream_with(&stream, |p, idx, is_shown| {
            live += 1;
            if is_shown {
                idxs.push(idx);
                shown.push(p.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            shown.len(),
            collected.len(),
            "callback count vs shown frames"
        );
        assert!(
            live >= shown.len(),
            "every shown picture is also a callback"
        );
        assert!(
            idxs.windows(2).all(|w| w[0] <= w[1]),
            "decode indices out of order: {idxs:?}"
        );
        for (i, (got, want)) in shown.iter().zip(&collected).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma");
            assert_eq!(got.u, want.u, "frame {i} U");
            assert_eq!(got.v, want.v, "frame {i} V");
        }

        // A sink error stops the decode and comes back unchanged.
        let mut seen = 0usize;
        let err = decode_stream_with(&stream, |_, _, _| {
            seen += 1;
            Err(Error::corrupt("sink says stop"))
        })
        .unwrap_err();
        assert_eq!(seen, 1, "decode continued past a failing sink");
        assert!(format!("{err}").contains("sink says stop"), "{err}");
    }

    /// lane-av1oracleguard, direction 1 of 2: the 4:2:0 helper's COMPARATOR
    /// still bites on a real 4:2:0 pin, so the refusal added above did not
    /// buy its non-vacuity by making the helper stop comparing.
    ///
    /// `420_mixll_256x128_6f.obu` is 8-bit 4:2:0 at 256x128. Ours and ffmpeg
    /// agree on every sample of every output frame (bad == 0), then ONE U
    /// sample of the oracle is flipped and the same count must move to 1. A
    /// count that stays 0 on a flipped oracle is a comparator that compares
    /// nothing, and the refusal would then be the only thing this gate could
    /// prove.
    #[test]
    fn a_420_oracle_comparator_counts_a_flipped_oracle_sample() {
        const NAME: &str = "a_420_oracle_comparator_counts_a_flipped_oracle_sample";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = crate_pin("420_mixll_256x128_6f.obu");
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin is missing ({e})"));
        assert_eq!(data.len(), 22336, "{NAME}: pin size drifted");
        assert_eq!(
            fnv1a64(&data),
            5694016005779287373,
            "{NAME}: pin bytes drifted"
        );
        assert_eq!(
            stream_subsampling(&data, NAME),
            (1, 1),
            "{NAME}: this cell must be a 4:2:0 pin -- the refusal arm uses other pins"
        );

        let decoded = decode_stream(&data)
            .unwrap_or_else(|e| panic!("{NAME}: the committed 4:2:0 pin did not decode: {e}"));
        // The pin name is the spec: `420_mixll_256x128_6f` is 6 shown frames.
        // `decoded.len()` is the value under test, so it must not be the count
        // handed to ffmpeg (count-vacuity ceiling is 0).
        const SHOWN: usize = 6;
        assert_eq!(decoded.len(), SHOWN, "{NAME}: the pin is 6 shown frames");
        let want = ffmpeg_decode_sequence(&data, 256, 128, SHOWN);
        assert_eq!(decoded.len(), want.len(), "{NAME}: decoded/frame count");

        let plane_mismatches = |ours: &[Pic], theirs: &[Pic], plane: u8| -> usize {
            ours.iter()
                .zip(theirs)
                .map(|(a, b)| {
                    let (a, b) = match plane {
                        0 => (&a.y, &b.y),
                        1 => (&a.u, &b.u),
                        _ => (&a.v, &b.v),
                    };
                    a.iter().zip(b).filter(|&(x, y)| x != y).count()
                })
                .sum()
        };
        let clean: Vec<usize> = (0..3)
            .map(|p| plane_mismatches(&decoded, &want, p))
            .collect();
        assert_eq!(
            clean,
            vec![0, 0, 0],
            "{NAME}: the 4:2:0 oracle arm is not byte-exact to start with, so a flipped sample \
             would prove nothing"
        );

        // Flip exactly one oracle sample and the count MUST move.
        let mut flipped = want.clone();
        flipped[0].u[0] ^= 1;
        let moved: Vec<usize> = (0..3)
            .map(|p| plane_mismatches(&decoded, &flipped, p))
            .collect();
        assert_eq!(
            moved,
            vec![0, 1, 0],
            "{NAME}: one flipped oracle U sample must move the U comparison count 0 -> 1 and \
             leave Y and V at 0; got {moved:?} -- the comparator does not bite, so this gate \
             cannot tell a working oracle arm from a vacuous one"
        );
        eprintln!(
            "{NAME}: clean per-plane counts {clean:?}, after one flipped oracle U sample {moved:?}"
        );
    }

    /// lane-av1oracleguard, direction 2 of 2 (8-bit): the 4:2:0 helper must
    /// REFUSE a real 4:2:2 pin instead of letting ffmpeg convert it. The
    /// message must name the REAL subsampling AND the sibling to use -- a
    /// bare "wrong shape" would leave the caller to rediscover the fix.
    ///
    /// Before this lane, `ffmpeg_decode_sequence(&422_key_64x64, 64, 64, 1)`
    /// RETURNED a 4:2:0-converted picture without complaint, which is the
    /// silent-green the refusal exists to stop. This gate needs no ffmpeg at
    /// all: the guard runs before the spawn, so a host without ffmpeg still
    /// proves the refusal.
    #[test]
    fn a_420_oracle_helper_refuses_a_422_pin() {
        const NAME: &str = "a_420_oracle_helper_refuses_a_422_pin";
        let _gate_lock = lock_gate_counters();
        let path = crate_pin("422_key_64x64.obu");
        let stream = read_pin(&path, 755, 0x87fc569cf53bcbc6, NAME);
        assert_eq!(
            stream_subsampling(&stream, NAME),
            (1, 0),
            "{NAME}: the pin must really be 4:2:2 (1,0), or this proves nothing"
        );
        let msg = panic_message(|| {
            ffmpeg_decode_sequence(&stream, 64, 64, 1);
        })
        .unwrap_or_else(|| {
            panic!(
                "{NAME}: ffmpeg_decode_sequence ACCEPTED a (1,0) stream. ffmpeg converted it to \
                 4:2:0 and the helper returned a green-looking comparison"
            )
        });
        for needle in ["(1,0)", "4:2:2", "ffmpeg_decode_sequence_422"] {
            assert!(
                msg.contains(needle),
                "{NAME}: the refusal must name {needle:?}; it said: {msg}"
            );
        }
        eprintln!("{NAME}: refused -> {msg}");
    }

    /// lane-av1oracleguard, direction 2 of 2 (high depth): the same refusal on
    /// `ffmpeg_decode_sequence_10bit` and `_12bit`, which ask ffmpeg for
    /// `yuv420p10le`/`yuv420p12le` and slice chroma at `width * height / 4`
    /// exactly as the 8-bit helper does. Pinned 4:2:2 at both depths.
    #[test]
    fn a_420_high_bit_depth_oracle_helpers_refuse_a_422_pin() {
        const NAME: &str = "a_420_high_bit_depth_oracle_helpers_refuse_a_422_pin";
        let _gate_lock = lock_gate_counters();
        let cells: [(&str, usize, u64, fn(&[u8]) -> Vec<Pic>); 2] = [
            (
                "s422_352x242_10b.obu",
                20575,
                17425395593908182125,
                |s: &[u8]| ffmpeg_decode_sequence_10bit(s, 352, 242, 1),
            ),
            (
                "s422_12bit_160x128.obu",
                9214,
                0xa62b8e3fe6f60ae5,
                |s: &[u8]| ffmpeg_decode_sequence_12bit(s, 160, 128, 1),
            ),
        ];
        for (file, len, fnv, call) in cells {
            let path = crate_pin(file);
            let stream = read_pin(&path, len, fnv, NAME);
            assert_eq!(
                stream_subsampling(&stream, NAME),
                (1, 0),
                "{NAME}/{file}: the pin must really be 4:2:2 (1,0), or this proves nothing"
            );
            let msg = panic_message(|| {
                call(&stream);
            })
            .unwrap_or_else(|| {
                panic!(
                    "{NAME}/{file}: the 4:2:0 high-bit-depth oracle helper ACCEPTED a (1,0) \
                     stream -- ffmpeg converted it and the helper returned a comparison that \
                     could pass green"
                )
            });
            for needle in ["(1,0)", "4:2:2", "ffmpeg_decode_sequence_422_depth"] {
                assert!(
                    msg.contains(needle),
                    "{NAME}/{file}: the refusal must name {needle:?}; it said: {msg}"
                );
            }
            eprintln!("{NAME}/{file}: refused -> {msg}");
        }
    }

    /// lane-av1422mixll10: a skipped INTRABC 8x8 leaf inside a LOSSLESS
    /// segment of a 4:2:2 frame predicts its whole 4x8 chroma plane block --
    /// both TX_4X4 units, each windowed off the DV frame copy. The defect
    /// arm pushed only the first 4x4 (at `cpy`), leaving rows
    /// `cpy+4..cpy+8` unwritten: frame 0's U(140..143, 188..191) read the
    /// sentinel poison 0 against the oracle's 664 (16 samples, all planes
    /// otherwise exact). Pin `422_mixll_intrabc_lossless_320x240_5f.obu`
    /// (aomenc profile 1, 320x240 yuv422p10le, mixed-lossless AQ segment +
    /// intrabc screen content; 79333 bytes, sha256
    /// `9fa1f1d28d61e3096d70908728f9cdfa1a570b9769a3d31e8d385a61c12a2b25`).
    /// Gate: every frame of all three planes byte-exact against ffmpeg's
    /// `yuv422p10le`, plus the non-vacuity controls (the pin's chroma census
    /// must show coded units, and one flipped oracle sample must move the
    /// count).
    #[test]
    fn a_422_mixll_intrabc_lossless_leaf_predicts_whole_chroma_plane_block() {
        const NAME: &str = "a_422_mixll_intrabc_lossless_leaf_predicts_whole_chroma_plane_block";
        const FILE: &str = "422_mixll_intrabc_lossless_320x240_5f.obu";
        const LEN: usize = 79333;
        const FNV: u64 = 8342335520738450773;
        const W: usize = 320;
        const H: usize = 240;
        const SHOWN: usize = 5;
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = crate_pin(FILE);
        let stream = read_pin(&path, LEN, FNV, NAME);
        assert_eq!(
            stream_subsampling(&stream, NAME),
            (1, 0),
            "{NAME}: the pin must really be 4:2:2 (1,0)"
        );
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            10,
            "{NAME}: the pin is a 10-bit cell"
        );
        let _ = stream_bit_depth(&stream, NAME);

        let decoded = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the committed 4:2:2 pin did not decode: {e}"));
        assert_eq!(
            decoded.len(),
            SHOWN,
            "{NAME}: the pin is {SHOWN} shown frames"
        );
        let want = ffmpeg_decode_sequence_422_depth(&stream, W, H, SHOWN, 10, "yuv422p10le");
        assert_eq!(decoded.len(), want.len(), "{NAME}: decoded/frame count");

        let plane_mismatches = |ours: &[Pic], theirs: &[Pic], plane: u8| -> usize {
            ours.iter()
                .zip(theirs)
                .map(|(a, b)| {
                    let (a, b) = match plane {
                        0 => (&a.y, &b.y),
                        1 => (&a.u, &b.u),
                        _ => (&a.v, &b.v),
                    };
                    a.iter().zip(b).filter(|&(x, y)| x != y).count()
                })
                .sum()
        };
        let clean: Vec<usize> = (0..3)
            .map(|p| plane_mismatches(&decoded, &want, p))
            .collect();
        assert_eq!(
            clean,
            vec![0, 0, 0],
            "{NAME}: the fix must be byte-exact against ffmpeg on every plane of every frame; \
             got {clean:?} (16 differs on frame 0 U would be the unwritten 4x4 again)"
        );

        // Non-vacuity: one flipped oracle U sample must move the U count.
        let mut flipped = want.clone();
        flipped[0].u[188 * (W / 2) + 140] ^= 1;
        let moved: Vec<usize> = (0..3)
            .map(|p| plane_mismatches(&decoded, &flipped, p))
            .collect();
        assert_eq!(
            moved,
            vec![0, 1, 0],
            "{NAME}: one flipped oracle U sample at (140,188) must move the U count 0 -> 1; \
             got {moved:?} -- the comparator does not bite"
        );
        eprintln!(
            "{NAME}: byte-exact vs ffmpeg on all {SHOWN} frames; flip control moved {moved:?}"
        );
    }

    /// lane-av1oracledepth: the 8-bit `ffmpeg_decode_sequence_444` must REFUSE
    /// a real 10-bit 4:4:4 pin instead of letting ffmpeg convert it down to
    /// `yuv444p`. ffmpeg does not fail on the depth mismatch -- it re-scales
    /// -- so a gate built on the 8-bit helper would compare ffmpeg's rescaled
    /// planes against our 10-bit decode and could read exact. The message
    /// must name the real depth AND the depth-generic sibling
    /// `ffmpeg_decode_sequence_444_depth`, so the caller has the fix in front
    /// of it.
    ///
    /// Needs no ffmpeg at all: the guard runs before the spawn, so a host
    /// without ffmpeg still proves the refusal.
    #[test]
    fn the_8bit_444_oracle_helper_refuses_a_10bit_444_pin() {
        const NAME: &str = "the_8bit_444_oracle_helper_refuses_a_10bit_444_pin";
        let _gate_lock = lock_gate_counters();
        let path = crate_pin("444_lossy_palette_chroma_352x242_10b.obu");
        let stream = read_pin(&path, 24875, 0x7fda_dbb4_3bbf_9af3, NAME);
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            10,
            "{NAME}: the pin must really be 10-bit, or this proves nothing"
        );
        assert_eq!(
            stream_subsampling(&stream, NAME),
            (0, 0),
            "{NAME}: the pin must really be 4:4:4 (0,0), or this proves nothing"
        );
        let msg = panic_message(|| {
            ffmpeg_decode_sequence_444(&stream, 352, 242, 1);
        })
        .unwrap_or_else(|| {
            panic!(
                "{NAME}: ffmpeg_decode_sequence_444 ACCEPTED a 10-bit 4:4:4 stream. ffmpeg \
                 converted it to yuv444p and the helper returned a green-looking comparison"
            )
        });
        for needle in ["not 8-bit", "ffmpeg_decode_sequence_444_depth"] {
            assert!(
                msg.contains(needle),
                "{NAME}: the refusal must name {needle:?}; it said: {msg}"
            );
        }
        eprintln!("{NAME}: refused -> {msg}");
    }
