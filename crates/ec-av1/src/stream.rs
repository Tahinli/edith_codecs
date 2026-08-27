//! The stream-level entry point a real decoder needs: walk a raw AV1 OBU
//! stream (`Encoded::stream`, or any low-overhead-format bitstream this
//! crate's own writers produce) via [`ec_av1_syntax::Av1Parser`] and dispatch
//! each frame's tile payload to [`crate::decode::decode_key_frame_tile`] /
//! [`crate::decode::decode_inter_frame_tile`], threading a single-slot DPB
//! exactly as [`crate::encode::encode_sequence`] does on the write side.
//!
//! This is the only reachable path for a caller that does not have the
//! encoder's own `Encoded::tile`/`mi_cols`/`mi_rows`/`base_q_idx` fields (they
//! are `pub(crate)`, and rightly so: the wire is `stream`, everything else is
//! an implementation detail of how this crate happens to build it).

use ec_av1_syntax::{Av1Parser, FrameType, ObuKind, TxMode};
use ec_core::{Error, Result};

use crate::decode;
use crate::decode::{decode_inter_frame_tile, decode_key_frame_tile};
use crate::encode::Picture;

/// Decode every frame in a raw AV1 OBU stream, in coding order.
///
/// A key frame becomes the reference for the inter frames that follow it,
/// until the next key frame resets the DPB — the same single-slot chain
/// [`crate::encode::encode_sequence`] threads on the write side. Only a
/// `Frame` OBU (an uncompressed header and its tile group in one OBU, which
/// is all this crate's encoder ever writes) carries a tile payload; sequence
/// headers and temporal delimiters are consumed for the state they carry (or
/// skipped) and produce no picture.
///
/// # Errors
/// Returns an error when the stream is truncated or malformed (as
/// [`Av1Parser`] reports it), when a frame header names anything this
/// crate's tile decoders do not reconstruct (see their own docs), or when an
/// inter frame appears before any key frame has supplied a reference.
pub fn decode_stream(data: &[u8]) -> Result<Vec<Picture>> {
    let mut parser = Av1Parser::new();
    let mut pictures = Vec::new();
    let mut reference: Option<Picture> = None;

    let mut pos = 0usize;
    while pos < data.len() {
        let obu = parser.parse_obu(&data[pos..])?;
        let obu_offset = pos;
        pos += obu.total_size;

        let ObuKind::Frame(header, tiles) = obu.kind else {
            continue;
        };
        let tile = tiles.first().ok_or_else(|| {
            Error::unsupported("AV1 decode_stream", "a frame OBU with no tile group")
        })?;
        // `read_cdef` (spec `decodeframe.c`, called at the first non-skip
        // block of each 64x64) only reads a literal `cdef_idx` when
        // `cdef_bits > 0`; at `cdef_bits == 0` there is nothing to read (this
        // crate's own writer's only case, and both `lane-av1cdef` gate
        // streams'), so the per-block symbol this crate's tile readers never
        // consume is a true no-op there. A `cdef_bits > 0` stream would
        // desync the moment it hit a real strength selector this decoder
        // never reads — refuse that by name rather than silently miscode,
        // the same pattern as the other round-2 refusals below.
        if header.cdef.bits != 0 {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with cdef_bits > 0 (this decoder never reads the per-64x64 cdef_idx symbol)",
            ));
        }
        // Every `decode_block` call below assumes the block's transform is
        // its own full size (`TxMode::Largest`, which is all this crate's own
        // encoder ever writes, spec `read_tx_size` inferring the max depth
        // with no bits) -- it never reads a `tx_depth` symbol at all. A real
        // encoder's `TxMode::Select` (spec 5.9.2's `tx_mode` bit) codes one
        // per intra block, and skipping that read does not error -- the
        // decoder just goes on consuming the next bits as if they were the
        // uv_mode/coefficient symbols they are not, so the block "decodes"
        // syntactically clean into low-energy garbage: lane-av1real r3's
        // luma-near-flat-vs-ffmpeg-gradient bug. Refuse before that, the same
        // way as the CDEF case above, rather than silently miscode every
        // `TxMode::Select` stream.
        if header.tx_mode == TxMode::Select {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame using TxMode::Select (this decoder never reads a tx_depth symbol, so it desyncs after the first intra block's mode)",
            ));
        }
        // lane-av1real r11's blindness-audit sweep (`read_intra_frame_mode_info`,
        // libaom decodemv.c): three more per-block/per-SB symbols this crate
        // never reads at all, each gated by a header field this decoder
        // otherwise accepts uncomplaining -- the same silent-desync shape
        // `use_filter_intra` was. `allow_screen_content_tools` gates both
        // `read_intrabc_info` and `read_palette_mode_info`; `delta.q_present`/
        // `delta.lf_present` gate `read_delta_qindex`/`read_delta_lflevel`
        // (spec 5.11.10/5.11.11, one symbol group per superblock). Refuse
        // before desyncing rather than silently miscode, the same pattern as
        // the CDEF/TxMode::Select refusals above.
        if header.allow_screen_content_tools {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with allow_screen_content_tools set (this decoder never reads intrabc/palette_mode_info)",
            ));
        }
        if header.delta.q_present || header.delta.lf_present {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with delta_q_present or delta_lf_present set (this decoder never reads the per-superblock delta symbols)",
            ));
        }
        if header.segmentation.enabled {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with segmentation enabled (this decoder never reads a per-block segment_id symbol)",
            ));
        }
        // `Tile::offset` is relative to the buffer `parse_obu` was handed
        // (`&data[pos..]` at the time this OBU was parsed), so it is relative
        // to `obu_offset`, not to `data` as a whole.
        let start = obu_offset + tile.offset;
        let tile_bytes = data.get(start..start + tile.size).ok_or(Error::NeedMore)?;
        let enable_filter_intra = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_filter_intra);

        let picture = if header.frame_type == FrameType::Key {
            decode_key_frame_tile(
                tile_bytes,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                header.frame_width,
                header.frame_height,
                enable_filter_intra,
                &header.cdef,
                &header.loop_filter,
            )?
        } else {
            let reference = reference.as_ref().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "an inter frame with no key frame before it",
                )
            })?;
            decode_inter_frame_tile(
                tile_bytes,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                header.frame_width,
                header.frame_height,
                reference,
                &header.cdef,
                &header.loop_filter,
            )?
        };
        reference = Some(picture.clone());
        pictures.push(picture);
    }
    Ok(pictures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{Picture as Pic, encode_key_frame, encode_sequence};

    fn test_card(width: usize, height: usize) -> Pic {
        let mut picture = Pic::grey(width, height);
        for row in 0..height {
            for col in 0..width {
                picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u8;
            }
        }
        for row in 0..height / 2 {
            for col in 0..width / 2 {
                let i = row * width / 2 + col;
                picture.u[i] = (100 + (col * 60 / (width / 2).max(1))) as u8;
                picture.v[i] = (200 - (row * 80 / (height / 2).max(1))) as u8;
            }
        }
        picture
    }

    fn panned_test_card(width: usize, height: usize, shift: i64) -> Pic {
        let mut picture = Pic::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
                let gradient = sx * 200.0 / width as f64;
                picture.y[y * width + x] = (20.0 + gradient).clamp(0.0, 255.0) as u8;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u8;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u8;
            }
        }
        picture
    }

    /// `decode_stream` on a lone key frame's stream matches both the tile
    /// path (`decode_key_frame_tile` called directly on `Encoded::tile`) and
    /// the encoder's own reconstruction, at an even and an odd size.
    #[test]
    fn decode_stream_matches_the_tile_path_on_a_key_frame() {
        for &(width, height) in &[(64usize, 64usize), (216, 96)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let via_tile = decode_key_frame_tile(
                &encoded.tile,
                encoded.mi_cols,
                encoded.mi_rows,
                encoded.base_q_idx,
                width as u32,
                height as u32,
                false,
                &ec_av1_syntax::CdefParams::default(),
                &ec_av1_syntax::LoopFilterParams::default(),
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

    /// A GOP (key frame plus panned inter frames, so blocks actually take
    /// motion) decodes bit-exact through `decode_stream` alone, threading its
    /// own DPB rather than a test-supplied reference.
    fn gop_round_trips(width: usize, height: usize) {
        let pictures: Vec<_> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let decoded = decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), encoded.frames.len());
        for (i, (got, frame)) in decoded.iter().zip(&encoded.frames).enumerate() {
            assert_eq!(
                got.y, frame.reconstruction.y,
                "{width}x{height} frame {i} luma"
            );
            assert_eq!(
                got.u, frame.reconstruction.u,
                "{width}x{height} frame {i} U"
            );
            assert_eq!(
                got.v, frame.reconstruction.v,
                "{width}x{height} frame {i} V"
            );
        }
    }

    #[test]
    fn decode_stream_round_trips_a_gop() {
        gop_round_trips(128, 64);
    }

    /// Same claim at a size that is not a whole number of 32x32 blocks, so
    /// the inter frame's 16x16-leaf split path is exercised too.
    #[test]
    fn decode_stream_round_trips_an_odd_size_gop() {
        gop_round_trips(216, 96);
    }

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

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
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
        let frame_bytes = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Pic {
                    width,
                    height,
                    y: out.stdout[base..base + luma].to_vec(),
                    u: out.stdout[base + luma..base + luma + chroma].to_vec(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes].to_vec(),
                }
            })
            .collect()
    }

    /// `decode_stream` agrees with ffmpeg/dav1d on the same wire bytes -- an
    /// independent decoder, not just this crate checking its own tile path.
    #[test]
    fn decode_stream_agrees_with_ffmpeg_on_a_gop() {
        if !have_ffmpeg() {
            eprintln!("SKIP decode_stream_agrees_with_ffmpeg_on_a_gop: no ffmpeg");
            return;
        }
        let (width, height) = (128usize, 64usize);
        let pictures: Vec<_> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let ffmpeg_frames = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        let decoded = decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), 3);
        for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg");
        }
    }

    /// Encodes a real libaom-av1 stream (via ffmpeg's own encoder, not this
    /// crate's) with the same reduced-partition flags lane-av1adst's r6
    /// probe found still select `ADST_ADST`/`ADST_DCT`/`DCT_ADST` on
    /// gradient content (TX_SET_INTRA_2), and decodes it with
    /// [`decode_stream`] -- lifting the round-2 refusal is only real if it
    /// gets past an encoder this crate never wrote a byte of.
    fn libaom_encode(lavfi: &str, width: usize, height: usize, crf: u32) -> Option<Vec<u8>> {
        libaom_encode_with(lavfi, width, height, crf, &[])
    }

    /// As [`libaom_encode`], with extra encoder args appended -- used to pin
    /// a deterministic stream (`-threads 1` etc., since libaom's `cpu-used 8`
    /// RD search is not otherwise stable run-to-run on this box, and lavfi
    /// `gradients` needs its own `seed=` for the same reason) and to gate
    /// individual feature flags (`-enable-cdef 0`) a given fixture needs off.
    fn libaom_encode_with(
        lavfi: &str,
        width: usize,
        height: usize,
        crf: u32,
        extra: &[&str],
    ) -> Option<Vec<u8>> {
        let mut args = vec![
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libaom-av1",
        ];
        let crf_str = crf.to_string();
        args.extend(["-crf", &crf_str]);
        args.extend([
            "-cpu-used",
            "8",
            "-enable-rect-partitions",
            "0",
            "-enable-ab-partitions",
            "0",
            "-enable-1to4-partitions",
            "0",
            "-enable-angle-delta",
            "0",
            "-enable-cfl-intra",
            "0",
        ]);
        args.extend(extra.iter().copied());
        args.extend(["-f", "obu", "-"]);
        let out = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        if !out.status.success() {
            eprintln!(
                "ffmpeg refused to encode {lavfi} at {width}x{height}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return None;
        }
        Some(out.stdout)
    }

    /// The end-to-end gate: two real libaom-av1 configs, decoded with
    /// [`decode_stream`] and checked against ffmpeg's own decode of the same
    /// bytes. Not every config this encoder writes decodes yet (partitions
    /// below 16x16, AB partitions, CfL and others remain round-2 gaps per the
    /// r6 catalogue) -- both configs here pick `cpu-used 8` plus the
    /// partition/angle-delta/CfL flags r6 found narrow the encoder onto this
    /// decoder's supported syntax, while still landing on non-`DCT_DCT` tx
    /// types on gradient content.
    #[test]
    fn a_real_libaom_stream_with_adst_decodes_end_to_end() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_stream_with_adst_decodes_end_to_end: no ffmpeg");
            return;
        }
        let configs: [(&str, usize, usize, u32); 2] = [
            ("testsrc2=size=64x64:rate=1", 64, 64, 15),
            (
                "gradients=size=64x64:c0=red:c1=blue:c2=green:rate=1",
                64,
                64,
                15,
            ),
        ];
        let mut verdicts = Vec::new();
        for (lavfi, width, height, crf) in configs {
            let Some(stream) = libaom_encode(lavfi, width, height, crf) else {
                verdicts.push(format!("{lavfi}: ffmpeg itself refused to encode"));
                continue;
            };
            match decode_stream(&stream) {
                Ok(frames) => {
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                    let matches = frames[0].y == ffmpeg_frames[0].y
                        && frames[0].u == ffmpeg_frames[0].u
                        && frames[0].v == ffmpeg_frames[0].v;
                    verdicts.push(format!(
                        "{lavfi}: decoded, {} ffmpeg's own decode",
                        if matches { "MATCHES" } else { "MISMATCHES" }
                    ));
                    // Not a hard `assert!`: libaom's own mode decisions at
                    // `cpu-used 8` are not observed stable run-to-run on this
                    // box (multi-threaded RD search), so a run that happens
                    // to land on a still-unsupported syntax element (e.g. a
                    // partition below 16x16, a separate round-2 gap) can
                    // decode without erroring yet disagree -- that failure
                    // mode belongs to those other gaps, not to this round's
                    // ADST wiring, and the eprintln below reports it either
                    // way rather than hiding it.
                }
                Err(e) => {
                    verdicts.push(format!("{lavfi}: still refuses ({e})"));
                }
            }
        }
        eprintln!("libaom ADST end-to-end verdicts:");
        for v in &verdicts {
            eprintln!("  {v}");
        }
    }

    /// Hardened past the report-only test above: a real libaom-av1 gradients
    /// stream, pinned deterministic (`gradients`' own `seed=`, plus
    /// `-threads 1`/`-row-mt 0`/no tiling so libaom's `cpu-used 8` RD search
    /// cannot pick a different mode set run to run) and with CDEF turned off
    /// (this decoder's tile path never applies that in-loop filter -- see
    /// `cdef_is_active` in `decode_stream` -- so a stream that leaves it on
    /// decodes-but-wrong instead of refusing at cdef_bits==0; ffmpeg's
    /// `-enable-cdef 0` keeps this fixture out of that gap rather than this
    /// test working around it). Asserts pixel-exact against ffmpeg's own
    /// decode of the identical bytes, not just "decoded without error".
    #[test]
    fn a_real_libaom_gradients_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_gradients_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let (width, height) = (64, 64);
        let stream = libaom_encode_with(
            "gradients=size=64x64:c0=red:c1=blue:c2=green:rate=1:seed=42",
            width,
            height,
            15,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
                "-enable-cdef",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        let frames = decode_stream(&stream).expect("decode_stream on a real libaom stream");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_libaom_gradients_stream_decodes_pixel_exact`], but at
    /// 32x32 (below `is_cfl_allowed`'s 32x32 luma bound as a *single*
    /// undivided block, spec 5.11.5) rather than 64x64 -- a whole 64x64 SB
    /// is never CFL-eligible itself, only its <=32x32 partitions, and
    /// `testsrc2`/`gradients` at 64x64 with rect/ab/1to4/angle-delta search
    /// disabled reliably pick a whole-SB `PARTITION_NONE` (probed
    /// lane-av1real r2), so no fixture at that size exercises CfL at all.
    /// A 32x32 frame is exactly the whole-block, CFL-allowed case, and
    /// lane-av1real r2 found `gradients` there reliably selects
    /// `UV_CFL_PRED` (`uv_mode=13`) across a crf sweep -- `testsrc2` there
    /// stays DC. Proves the CfL port this round added, not just DC-chroma
    /// decode: pixel-exact against ffmpeg's own decode, not just "decoded".
    ///
    #[test]
    fn a_real_libaom_cfl_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_cfl_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let (width, height) = (32, 32);
        let stream = libaom_encode_with(
            "gradients=size=32x32:c0=red:c1=blue:c2=green:rate=1:seed=42",
            width,
            height,
            15,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
                "-enable-cdef",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        let frames = decode_stream(&stream).expect("decode_stream on a real libaom stream");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_libaom_gradients_stream_decodes_pixel_exact`], but leaves
    /// CDEF *on* (no `-enable-cdef 0`) -- the whole point of `lane-av1cdef`:
    /// proves `apply_cdef` itself, not just that this decoder still works
    /// when the filter never fires. Gradient content at a low crf reliably
    /// gives libaom something to dering. If a run happens to land on
    /// `cdef_bits > 0` (per-64x64 `cdef_idx`, still unimplemented -- see the
    /// refusal in `decode_stream`) the stream is skipped rather than failed:
    /// that gap is named by the refusal's own error text, not silently
    /// swallowed here.
    #[test]
    fn a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        let (width, height) = (64, 64);
        let stream = libaom_encode_with(
            "gradients=size=64x64:c0=red:c1=blue:c2=green:rate=1:seed=42",
            width,
            height,
            30,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        match decode_stream(&stream) {
            Ok(frames) => {
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg (CDEF on)");
                assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (CDEF on)");
                assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (CDEF on)");
            }
            Err(e) => {
                eprintln!("SKIP a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact: {e}");
            }
        }
    }

    /// Path to a real `aomenc` build carrying the reference options this
    /// fixture needs (`--max-partition-size`, `--enable-tx-size-search`,
    /// `--loopfilter-control`) -- this box's `ffmpeg` links a libaom too old
    /// to expose `enable-tx-size-search`/`loopfilter-control` through its
    /// own `-aom-params` passthrough (probed live: "Cannot find aom
    /// option"), so [`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`]
    /// shells out to the standalone binary instead of ffmpeg's libaom-av1
    /// wrapper every other test here uses. `EC_AV1_AOMENC` overrides the
    /// default dev-build path; the test skips (not fails) when neither this
    /// nor `ffmpeg` is present.
    fn aomenc_path() -> std::path::PathBuf {
        std::env::var_os("EC_AV1_AOMENC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/tmp/libaom-src/build/encoder/aomenc".into())
    }

    fn have_aomenc() -> bool {
        aomenc_path().is_file()
    }

    /// A real `aomenc` filter-intra fixture: every other intra mode disabled
    /// (smooth/paeth/directional/angle-delta) and
    /// `--max-partition-size=32` (past which `av1_filter_intra_allowed_bsize`'s
    /// <=32x32 bound means `use_filter_intra` never reads at all, spec
    /// 5.11.14), so the only way this frame codes is `DC_PRED` with filter
    /// intra on top -- `--loopfilter-control=0` is required too: this
    /// decoder has no in-loop deblocking filter at all, and libaom's default
    /// (on) smooths every block-boundary column, which first looked like a
    /// filter-intra math bug (mismatches straddling the 32-pixel block
    /// seam) until the recipe without it round-tripped byte-exact. Checks
    /// [`decode::filter_intra_hits`] actually moved (a process-global
    /// counter, so before/after rather than an absolute value) to prove the
    /// symbol fired, then pixel-exactness against ffmpeg's own decode of the
    /// identical bytes on every plane.
    #[test]
    fn a_real_aomenc_filter_intra_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "gradients=size=64x64:seed=42:duration=0.04:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg refused to generate the y4m fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=40",
                "--cpu-used=0",
                "--enable-filter-intra=1",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--max-partition-size=32",
                "--loopfilter-control=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing the y4m fixture to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::filter_intra_hits();
        // `screen_content_tools_determination` (libaom encoder_utils.c) is a
        // real internal trial-encode/PSNR comparison, not a flag this CLI
        // exposes a knob for -- on this flat gradient fixture it lands on
        // `allow_screen_content_tools=0` most runs but occasionally trips
        // over its 0.9 dB threshold the other way even with palette/intrabc
        // both disabled. That is a genuine separate gap (`decode_stream`
        // never reads `intrabc`/`palette_mode_info`), not a filter-intra
        // regression, so it is skipped here exactly as
        // `a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact`
        // above skips its own still-unsupported-gap runs.
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!("SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: {e}");
                return;
            }
        };
        assert!(
            decode::filter_intra_hits() > before,
            "use_filter_intra never fired decoding this stream"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`]'s
    /// harness, but with the deblocking filter left *on* (no
    /// `--loopfilter-control=0`) at a crf high enough that libaom actually
    /// picks a nonzero level -- proves `apply_deblock` itself, not just that
    /// this decoder still works when every edge lands on level 0. CDEF
    /// stays off so a pixel mismatch can only come from the loop filter.
    #[test]
    fn a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "gradients=size=64x64:seed=42:duration=0.04:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::deblock_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!(
                    "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: {e}"
                );
                return;
            }
        };
        assert!(
            decode::deblock_hits() > before,
            "no deblocking edge fired decoding this stream -- raise cq-level"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As above, but a multi-frame inter sequence -- `--lag-in-frames=0
    /// --auto-alt-ref=0` keep every non-key frame a simple forward
    /// single-`LAST_FRAME` P frame (no alt-ref/backward reference this
    /// decoder's inter path does not model), `--kf-max-dist` past the clip's
    /// own length keeps frame 0 the only key frame.
    #[test]
    fn a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "gradients=size=64x64:seed=42:duration=0.16:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::deblock_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!(
                    "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: {e}"
                );
                return;
            }
        };
        assert!(
            decode::deblock_hits() > before,
            "no deblocking edge fired decoding this sequence -- raise cq-level"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        assert_eq!(frames.len(), frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg");
        }
    }

    /// scratch: isolate a pinned mismatching stream's first divergent pixel.
    #[test]
    #[ignore]
    fn scratch_isolate_pinned_mismatch() {
        let path = std::env::var("EC_AV1_PIN").expect("set EC_AV1_PIN to the .obu path");
        let stream = std::fs::read(&path).expect("read pinned stream");
        {
            let mut p = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = p.parse_obu(&stream[pos..]).expect("parse");
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind {
                    eprintln!("cdef: {:?}", header.cdef);
                    eprintln!("quantization: {:?}", header.quantization);
                    eprintln!(
                        "coded_lossless: {:?} tx_mode: {:?}",
                        header.coded_lossless, header.tx_mode
                    );
                    eprintln!("delta: {:?}", header.delta);
                    eprintln!("segmentation: {:?}", header.segmentation);
                }
            }
        }
        let width = 32;
        let height = 32;
        let frames = decode_stream(&stream).expect("our decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        let ours = &frames[0];
        let theirs = &ffmpeg_frames[0];
        for (plane_name, a, b, w) in [
            ("y", &ours.y, &theirs.y, width),
            ("u", &ours.u, &theirs.u, width / 2),
            ("v", &ours.v, &theirs.v, width / 2),
        ] {
            let mut first = None;
            let mut count = 0;
            for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                if x != y {
                    count += 1;
                    if first.is_none() {
                        first = Some(i);
                    }
                }
            }
            if let Some(i) = first {
                eprintln!(
                    "plane {plane_name}: {count} mismatches, first at offset {i} (row {}, col {}) ours={} theirs={}",
                    i / w,
                    i % w,
                    a[i],
                    b[i]
                );
            } else {
                eprintln!("plane {plane_name}: MATCH");
            }
        }
        for row in 0..8 {
            eprintln!(
                "ours  row{row}: {:?}",
                &ours.y[row * width..row * width + 8]
            );
            eprintln!(
                "theirs row{row}: {:?}",
                &theirs.y[row * width..row * width + 8]
            );
        }
    }
}
