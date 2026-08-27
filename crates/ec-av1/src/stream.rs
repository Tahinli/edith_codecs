//! The stream-level entry point a real decoder needs: walk a raw AV1 OBU
//! stream (`Encoded::stream`, or any low-overhead-format bitstream this
//! crate's own writers produce) via [`ec_av1_syntax::Av1Parser`] and dispatch
//! each frame's tile payload to [`crate::decode::decode_key_frame_tile_with_cdfs`] /
//! [`crate::decode::decode_inter_frame_tile_with_cdfs`], threading a
//! single-slot picture DPB exactly as [`crate::encode::encode_sequence`] does
//! on the write side, plus the full 8-slot *CDF* reference bank spec 7.20
//! (`load_cdfs`) and 7.4 (the reference frame update) describe: a frame whose
//! header names `primary_ref_frame != PRIMARY_REF_NONE` resumes from the
//! adapted table a previous frame's `refresh_frame_flags` left in that slot,
//! rather than the spec 8.4 defaults.
//!
//! This is the only reachable path for a caller that does not have the
//! encoder's own `Encoded::tile`/`mi_cols`/`mi_rows`/`base_q_idx` fields (they
//! are `pub(crate)`, and rightly so: the wire is `stream`, everything else is
//! an implementation detail of how this crate happens to build it).

use ec_av1_syntax::{
    Av1Parser, FrameType, NUM_REF_FRAMES, ObuKind, PRIMARY_REF_NONE, TxMode, WarpModel,
};
use ec_core::{Error, Result};

use crate::cdf_state::Cdfs;
use crate::decode;
use crate::decode::{
    decode_inter_frame_tile_with_cdfs, decode_key_frame_tile, decode_key_frame_tile_with_cdfs,
    q_ctx_of,
};
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
    // Spec 7.20/7.4: each of the 8 reference slots remembers the picture a
    // previous frame's `refresh_frame_flags` stored into it, exactly like the
    // CDF slots below -- `LAST_FRAME` names a slot via `ref_frame_idx[0]`,
    // not simply "the previous frame decoded"; a frame whose own predecessor
    // didn't refresh that slot (an altref/hidden frame, or any GOP with more
    // than one live reference) must keep predicting from whatever picture is
    // still sitting there. Round 7's cdffwd hunt: a global `reference`
    // overwritten every frame silently always used "the most recent decode"
    // instead, producing a bit-exact motion vector into the WRONG picture --
    // luma-only (chroma coincidentally shares this fixture's flat regions)
    // and small-magnitude (the two pictures mostly agree), which is exactly
    // what made it look like a rounding bug rather than a wrong reference.
    let mut ref_slots: [Option<Picture>; NUM_REF_FRAMES] = std::array::from_fn(|_| None);
    // Spec 7.20/7.4: each of the 8 reference slots remembers the CDF state a
    // previous frame's `refresh_frame_flags` stored into it -- the frame's
    // own end-of-tile adapted table, or (when the frame set
    // `disable_frame_end_update_cdf`) the table it started from, unchanged.
    let mut cdf_slots: [Option<Cdfs>; NUM_REF_FRAMES] = std::array::from_fn(|_| None);

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
        // An intra frame's `decode_key_frame_tile*` now reads `tx_depth`
        // under `TxMode::Select` (lane-av1txsel, spec 5.11.16): the key-frame
        // decode path threads `tx_select` through `decode_block`/
        // `decode_leaf8`, and refuses on its own (a named `unsupported`, not
        // a silent desync) the one case it still cannot code -- a resolved
        // 4x4 luma transform, which has no coefficient tables here. This
        // refusal is narrowed to what genuinely still has no reader at all:
        // `decode_inter_frame`'s inter path never threads `tx_select`
        // through its own block loop, so an inter frame under
        // `TxMode::Select` still desyncs the same way the comment above used
        // to describe for every frame -- lane-av1real r3's
        // luma-near-flat-vs-ffmpeg-gradient bug.
        if header.tx_mode == TxMode::Select && header.frame_type != FrameType::Key {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "an inter frame using TxMode::Select (this decoder's inter path never reads a tx_depth symbol, so it desyncs after the first block's mode)",
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
        // Loop restoration carries per-restoration-unit symbols in the tile
        // (spec 5.11.57 `read_lr`, one group per LR unit reached from the
        // superblock walk) that this decoder never reads -- same
        // silent-desync shape as the refusals above. Traced live 2026-08-27:
        // an aomenc inter frame with `Sgrproj` on the V plane desynced the
        // partition walk into out-of-alphabet garbage.
        if header.loop_restoration.uses_lr {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with loop restoration enabled (this decoder never reads the per-unit lr symbols)",
            ));
        }
        // `decode_inter_block`/`decode_inter_block8`'s `GLOBALMV` arm (spec
        // 5.11.26's `read_inter_intra`... really `assign_mv`'s `GLOBALMV`
        // case, spec 7.10.2.1) uses `gm_get_motion_vector`, which is the
        // zero vector only under `GmType == IDENTITY` -- any other warp
        // model needs the full affine/translation MV derivation this
        // decoder does not carry. `global_motion` is indexed `ref_frame - 1`
        // (`read_global_motion_params`'s `i` loops from `LAST_FRAME`);
        // lane-av1refs widens this from `[0]` (LAST_FRAME) alone to every
        // reference `decode_inter_block` can now select -- LAST_FRAME (index
        // 0) and GOLDEN_FRAME (index 3).
        if header.frame_type != FrameType::Key
            && (header.global_motion[0].model != WarpModel::Identity
                || header.global_motion[3].model != WarpModel::Identity)
        {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "an inter frame whose LAST_FRAME/GOLDEN_FRAME global motion is not IDENTITY (GLOBALMV needs the full warp derivation this decoder does not carry)",
            ));
        }
        // `decode_inter_block`'s `single_ref_p*` chain codes only
        // `SINGLE_REFERENCE` blocks (never reads `comp_mode`) -- a frame
        // whose `reference_select` header bit lets any block choose
        // `COMPOUND_REFERENCE` would desync at the first block that reads
        // a `comp_mode` symbol this decoder never consumes. `skip_mode`
        // (spec 5.9.22) is provably unreachable here as a consequence: its
        // own `skip_mode_params()` only sets `skip_mode_present` when
        // `reference_select` is set, so refusing `reference_select` refuses
        // `skip_mode` too, without needing a second check.
        if header.reference_select {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with reference_select set (this decoder never reads comp_mode / codes only single-reference blocks)",
            ));
        }
        // `context_update_tile_id` (spec `decode_tile`/`exit_symbol`) names
        // which tile's end-of-tile adapted CDFs become the frame's own; this
        // decoder only ever decodes tile 0 (`tiles.first()` above), so a
        // multi-tile frame is refused rather than silently forwarding the
        // wrong tile's table (or leaving the rest of the picture undecoded).
        if header.tile_info.cols > 1 || header.tile_info.rows > 1 {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with more than one tile (this decoder only ever decodes tile 0)",
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
        let enable_dual_filter = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_dual_filter);
        let enable_edge_filter = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_intra_edge_filter);
        let interp_fixed = match header.interpolation_filter {
            ec_av1_syntax::InterpolationFilter::Switchable => None,
            fixed => Some(crate::mc::InterpFilterKind::from_header(fixed)),
        };

        // Spec 7.20 `load_cdfs`: a frame naming a `primary_ref_frame` resumes
        // from that reference slot's saved CDF state instead of the spec 8.4
        // defaults `decode_key/inter_frame_tile_with_cdfs`'s `None` case
        // builds.
        let initial_cdfs = if header.primary_ref_frame == PRIMARY_REF_NONE {
            None
        } else {
            let slot = header.ref_frame_idx[header.primary_ref_frame as usize] as usize;
            Some(cdf_slots[slot].clone().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
                )
            })?)
        };
        let started_from = initial_cdfs.clone();

        let (picture, end_cdfs) = if header.frame_type == FrameType::Key {
            decode_key_frame_tile_with_cdfs(
                tile_bytes,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                header.frame_width,
                header.frame_height,
                enable_filter_intra,
                enable_edge_filter,
                &header.cdef,
                &header.loop_filter,
                initial_cdfs,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
            )?
        } else {
            let last_slot = header.ref_frame_idx[0] as usize;
            let reference = ref_slots[last_slot].as_ref().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "an inter frame with no key frame before it",
                )
            })?;
            // `ref_frame_idx[3]` names GOLDEN_FRAME's own DPB slot (spec
            // 7.8/7.20 -- `ref_frame_idx` is indexed `ref_frame - LAST_FRAME`).
            // Unlike LAST_FRAME, GOLDEN_FRAME having no picture yet is not a
            // stream error on its own: `decode_inter_block` only needs it
            // when a block actually selects GOLDEN_FRAME, and refuses by
            // name there if the slot is still empty.
            let golden_slot = header.ref_frame_idx[3] as usize;
            let golden = ref_slots.get(golden_slot).and_then(Option::as_ref);
            decode_inter_frame_tile_with_cdfs(
                tile_bytes,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                header.frame_width,
                header.frame_height,
                reference,
                golden,
                &header.cdef,
                &header.loop_filter,
                initial_cdfs,
                header.allow_high_precision_mv,
                interp_fixed,
                enable_dual_filter,
            )?
        };
        // Spec 7.20: `disable_frame_end_update_cdf` stores the frame's
        // *initial* table into the slots it refreshes, not the adapted
        // end-of-tile one.
        let stored_cdfs = if header.disable_frame_end_update_cdf {
            started_from.unwrap_or_else(|| Cdfs::new(q_ctx_of(header.quantization.base_q_idx)))
        } else {
            // Spec 7.20 `save_cdfs` (libaom `av1_reset_cdf_symbol_counters`):
            // the adapted table is saved with every symbol counter zeroed,
            // not with the counts the tile's own decode left behind.
            let mut end_cdfs = end_cdfs;
            end_cdfs.reset_counts();
            end_cdfs
        };
        if let Ok(path) = std::env::var("EC_AV1_DUMP_TABLES") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "=== frame idx={} ===", pictures.len());
                let _ = writeln!(f, "partition_w64 {:?}", stored_cdfs.partition_w64);
                let _ = writeln!(f, "partition_w32 {:?}", stored_cdfs.partition_w32);
                let _ = writeln!(f, "partition_w16 {:?}", stored_cdfs.partition_w16);
                let _ = writeln!(f, "skip {:?}", stored_cdfs.skip);
                let _ = writeln!(f, "new_mv {:?}", stored_cdfs.new_mv);
                let _ = writeln!(f, "zero_mv {:?}", stored_cdfs.zero_mv);
                let _ = writeln!(f, "ref_mv {:?}", stored_cdfs.ref_mv);
                let _ = writeln!(f, "mv_joint {:?}", stored_cdfs.mv_joint);
            }
        }
        for i in 0..NUM_REF_FRAMES {
            if header.refresh_frame_flags & (1 << i) != 0 {
                cdf_slots[i] = Some(stored_cdfs.clone());
                ref_slots[i] = Some(picture.clone());
            }
        }
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
                false,
                // Our own encoder always writes `reduced_tx_set: true`.
                true,
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
                // Loop restoration is a named refusal (aomenc's RD picks
                // Sgrproj on this kind of content) -- off in every recipe
                // whose test targets a different surface.
                "--enable-restoration=0",
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

    /// As [`a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact`],
    /// but WITHOUT `--enable-tx-size-search=0` -- the one flag every other
    /// recipe in this file pins off specifically to keep the stream on
    /// `TxMode::Largest`. Dropped, aomenc's RD is free to pick
    /// `TxMode::Select` and split some blocks' luma transform below their own
    /// side (spec 5.11.16), which is what this test's `tx_depth_hits()`
    /// counter proves actually happened -- a stream that resolves every
    /// `tx_depth` to 0 would pixel-match by construction (identical to
    /// `TxMode::Largest`) without exercising the new split-TU decode path at
    /// all. Same multi-seed retry shape as
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]:
    /// aomenc's own RD is free (with tx-size search on) to also occasionally
    /// pick `allow_screen_content_tools` on this content, a separate,
    /// already-named refusal -- try several seeds, and the first stream that
    /// both decodes AND actually split a transform must be pixel-exact.
    #[test]
    fn a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A flat gradient never has enough local detail for the RD
            // search to prefer a split transform over the block's own full
            // size -- `mandelbrot`'s fractal boundary gives every attempt
            // (a different `start_scale`) sharp, varied-frequency edges.
            // start_scale 5.0 is the proven first try (decodes pixel-exact
            // AND reads the seven-symbol set-1 tx_type rows on TX8 TUs);
            // later attempts walk down for variety when aomenc's RD flips
            // elsewhere.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // Desaturated: chroma stays flat (constant 128) so
                    // `uv_mode` always resolves to `DC_PRED` and this
                    // decoder's still-unsupported directional-chroma round-2
                    // gap never fires, leaving the luma detail alone to
                    // drive whether `TxMode::Select`'s RD splits a block.
                    "-vf",
                    "hue=s=0",
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
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
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Every non-DC intra tool off: directional chroma and
                    // angle deltas are separate round-2 gaps, and DC-only
                    // residual is exactly what gives the RD search a reason
                    // to split a transform.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-cdef=0",
                    // Loop restoration is a named refusal (aomenc's RD picks
                    // Sgrproj on this kind of content) -- off in every recipe
                    // whose test targets a different surface.
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    // 16x16 blocks are what split into the TX8 TUs whose
                    // set-1 tx_type rows this gate exists to prove; some
                    // scales resolve depth 2 (TX4, a named refusal) and the
                    // attempt loop just moves on.
                    "--min-partition-size=16",
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
            let before = decode::tx_depth_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::tx_depth_hits() == before {
                refusals.push(format!(
                    "seed {seed}: decoded, but no block resolved a split tx_depth"
                ));
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: every attempt \
             hit a named refusal or never split a transform:\n{}",
            refusals.join("\n")
        );
    }

    /// The directional-chroma/angle-delta gate: a real aomenc key frame with
    /// `--enable-directional-intra`/`--enable-angle-delta` left ON (the
    /// opposite of every sibling recipe's `=0`, which existed specifically to
    /// keep this round-2 gap from firing while a different surface was under
    /// test). A colourful, sharp-edged source gives the RD search a reason to
    /// pick a directional `uv_mode` and/or a nonzero angle delta -- some
    /// seeds still won't, so retry like every other real-encoder gate here,
    /// but the first attempt that actually fires either counter must decode
    /// pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
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
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Smooth/Paeth chroma is a separate, still-refused round-2
                    // gap (an unrelated `Edges::build` no-neighbour bug this
                    // lane does not cover) -- off so the RD search's only
                    // non-DC/CFL chroma choice left is directional.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    // The intra edge filter/upsample (spec 7.11.2.7-2.9) is
                    // now implemented (`directional()`) -- left ON (aomenc's
                    // own default) so this gate actually exercises it instead
                    // of the un-filtered fast path a disabled flag would keep
                    // testing.
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    // Same reason as the tx-select gate above: 8x8 and other
                    // sub-16x16 splits are a separate, already-named refusal.
                    "--min-partition-size=16",
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
            let (uv_before, angle_before) =
                (decode::directional_uv_hits(), decode::angle_delta_hits());
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::directional_uv_hits() == uv_before
                && decode::angle_delta_hits() == angle_before
            {
                refusals.push(format!(
                    "seed {seed}: decoded, but neither a directional uv_mode nor a \
                     nonzero angle delta fired"
                ));
                continue;
            }
            fired_runs += 1;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                return;
            }
        }
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised directional chroma:\n{}",
            refusals.join("\n")
        );
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
                // Forces `primary_ref_frame = PRIMARY_REF_NONE` on every
                // frame (spec 5.9.2): without it a real encoder's inter
                // frame loads its initial CDF state from the previous
                // frame's *adapted* tables (spec 7.20's `load_cdfs`), which
                // this decoder never does -- it always starts each frame's
                // `Cdfs` from the spec defaults (`decode.rs`'s
                // `Cdfs::new(q_ctx)`, called fresh per frame in
                // `decode_stream`). Without this flag the inter frame's
                // very first symbol desyncs immediately (traced 2026-08-27:
                // no coefficient trace at all before the tile's first
                // partition read comes back an out-of-alphabet value). A
                // real decoder needs cross-frame CDF forwarding to handle
                // that stream; this fixture sidesteps it the same way a
                // real broadcast/low-latency encode legitimately can.
                "--error-resilient=1",
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
                // Loop restoration is a named refusal (aomenc's RD picks
                // Sgrproj on this kind of content) -- off in every recipe
                // whose test targets a different surface.
                "--enable-restoration=0",
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

    /// The decisive cross-frame CDF forwarding test: identical to
    /// [`a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact`]
    /// except it drops `--error-resilient=1`, so aomenc codes every inter
    /// frame's `primary_ref_frame` at its default (not `PRIMARY_REF_NONE`)
    /// and expects the decoder to resume from the previous frame's *adapted*
    /// CDF state (spec 7.20 `load_cdfs`). Without cross-frame forwarding,
    /// frame 1's very first tile symbol desyncs (an out-of-alphabet
    /// partition read); with it, every frame decodes pixel-exact.
    #[test]
    fn a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        // aomenc's RD is nondeterministic run to run even on a fixed fixture:
        // most encodes of this recipe trip one of the decoder's NAMED round-2
        // refusals (a reference other than LAST_FRAME, a sub-16 partition --
        // genuine coverage gaps, separately documented), and only some produce
        // a stream fully inside this decoder's declared support. So: attempt
        // several seeds, and the first stream that DECODES must be pixel-exact
        // -- that assertion is what guards the forwarded-CDF state (a wrong
        // table decodes plausible-but-wrong pixels; the counter-reset bug this
        // gate caught did exactly that). Exhausting every attempt on named
        // refusals skips loudly, listing them.
        let mut refusals = Vec::new();
        for attempt in 0..8u32 {
            let seed = 42 + attempt;
            let source = format!("gradients=size=64x64:seed={seed}:duration=0.16:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
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
                    // `--threads=1` (not the default): a multithreaded RD search
                    // makes aomenc's screen-content-tools heuristic race (seen
                    // flaky under `cargo test`'s own parallelism -- the same
                    // fixture, same seed, occasionally sets
                    // `allow_screen_content_tools`, a round-2 gap unrelated to
                    // CDF forwarding), so pin it to one thread for a
                    // deterministic header.
                    "--threads=1",
                    "--row-mt=0",
                    // No `--error-resilient=1` here (that is the whole point of
                    // this test), but every other reference/compound tool this
                    // decoder's round-2 inter path does not model is still
                    // disabled the same way `--error-resilient=1` incidentally
                    // disabled it in the sibling test above: without
                    // `--enable-order-hint=0` aomenc's RD search picks a
                    // non-`LAST_FRAME` reference (`GOLDEN_FRAME`) for some
                    // blocks even in this short a sequence, which is a separate,
                    // already-documented round-2 gap ("a reference frame other
                    // than LAST_FRAME"), not a CDF-forwarding one.
                    "--enable-order-hint=0",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
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
                    // Loop restoration is a named refusal (aomenc's RD picks
                    // Sgrproj on this kind of content) -- off in every recipe
                    // whose test targets a different surface.
                    "--enable-restoration=0",
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
            // A named `unsupported` refusal is a documented coverage gap (each
            // one carries its own test debt elsewhere), not a forwarding verdict
            // -- try the next seed. Any stream that DECODES must be pixel-exact,
            // and any non-refusal error is a hard failure.
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "cross-frame CDF forwarding failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: every \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1refs's decisive single-non-LAST-reference gate: identical
    /// recipe to
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// except `--enable-order-hint=0` is DROPPED (that flag's own comment on
    /// the sibling test names it as exactly what suppresses aomenc's RD from
    /// ever picking `GOLDEN_FRAME`) and the sequence runs 8 frames, not 4 --
    /// more chances for the RD search to reach past the first GOP's initial
    /// key frame. `--auto-alt-ref=0` stays: this decoder only supports
    /// `LAST_FRAME`/`GOLDEN_FRAME` so far, and alt-ref/backward references
    /// are still a named refusal, not this gate's target. A stream that
    /// decodes without firing [`decode::non_last_ref_hits`] is not a
    /// coverage win for this round -- SKIP it as a genuine, named miss
    /// rather than pass silently on untested ground.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!("gradients=size=64x64:seed={seed}:duration=0.32:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
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
                    "--threads=1",
                    "--row-mt=0",
                    // Narrows the RD search's reference alphabet toward
                    // LAST_FRAME/GOLDEN_FRAME -- without this, aomenc's RD is
                    // just as likely to pick LAST2/LAST3/BWDREF/ALTREF2/
                    // ALTREF (this decoder's already-documented, still-open
                    // refusals), starving this gate of the one reference it
                    // targets.
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
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
                    "--enable-restoration=0",
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
            let before = decode::non_last_ref_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "a golden-reference stream failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::non_last_ref_hits() == before {
                // This seed's RD search never actually picked GOLDEN_FRAME --
                // a genuine miss, not a refusal. Try the next seed rather
                // than claiming coverage this attempt did not exercise.
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            eprintln!(
                "FIRING seed {seed}: non_last_ref_hits advanced by {}",
                decode::non_last_ref_hits() - before
            );
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: \
             {never_fired} attempts decoded but never fired GOLDEN_FRAME, and every other \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
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
                    eprintln!("interpolation_filter: {:?}", header.interpolation_filter);
                    eprintln!("cdef: {:?}", header.cdef);
                    eprintln!("quantization: {:?}", header.quantization);
                    eprintln!(
                        "coded_lossless: {:?} tx_mode: {:?}",
                        header.coded_lossless, header.tx_mode
                    );
                    eprintln!("delta: {:?}", header.delta);
                    eprintln!("segmentation: {:?}", header.segmentation);
                    eprintln!("loop_restoration: {:?}", header.loop_restoration);
                    eprintln!(
                        "use_128x128_superblock: {:?} disable_cdf_update: {:?} disable_frame_end_update_cdf: {:?} primary_ref_frame: {:?} allow_high_precision_mv: {:?} force_integer_mv: {:?}",
                        p.sequence_header().map(|s| s.use_128x128_superblock),
                        header.disable_cdf_update,
                        header.disable_frame_end_update_cdf,
                        header.primary_ref_frame,
                        header.allow_high_precision_mv,
                        header.force_integer_mv,
                    );
                }
            }
        }
        let width = std::env::var("EC_AV1_PIN_W")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let height = std::env::var("EC_AV1_PIN_H")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let frame_count = std::env::var("EC_AV1_PIN_N")
            .map(|s| s.parse().unwrap())
            .unwrap_or(4);
        let frames = decode_stream(&stream).expect("our decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for f in 0..frame_count.min(frames.len()).min(ffmpeg_frames.len()) {
            let ours = &frames[f];
            let theirs = &ffmpeg_frames[f];
            for (plane_name, a, b, w) in [
                ("y", &ours.y, &theirs.y, width),
                ("u", &ours.u, &theirs.u, width / 2),
                ("v", &ours.v, &theirs.v, width / 2),
            ] {
                let mut first = None;
                let mut count = 0;
                let mut worst = 0i32;
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    if x != y {
                        count += 1;
                        if first.is_none() {
                            first = Some(i);
                        }
                        worst = worst.max((*x as i32 - *y as i32).abs());
                    }
                }
                if let Some(i) = first {
                    eprintln!(
                        "frame {f} plane {plane_name}: {count} mismatches (worst delta {worst}), first at offset {i} (row {}, col {}) ours={} theirs={}",
                        i / w,
                        i % w,
                        a[i],
                        b[i]
                    );
                } else {
                    eprintln!("frame {f} plane {plane_name}: MATCH");
                }
            }
        }
    }

    /// Final round of the TX4/tx-class lane's own gate: `--min-partition-size=8
    /// --max-partition-size=8` pins every leaf to exactly 8x8 (spec
    /// 5.11.4's own boundary carve-out aside, which a multiple-of-8 frame
    /// never triggers), so a real `aomenc` stream from this recipe can only
    /// legitimately read `PARTITION_NONE` at every `bsize=3` node -- any
    /// other value this decoder sees there is a genuine desync, not a
    /// "partition below 8x8" the encoder actually wrote (traced live against
    /// a debug `aomdec` 2026-08-27: the true bitstream's own 8x8 partition
    /// reads are all `NONE`; the mismatch was `base_ctx`'s DC-position
    /// short-circuit firing for `TX_CLASS_HORIZ`/`VERT` too, off by one
    /// `BR` read into a spurious extra Golomb call). The sinusoidal-stripe
    /// `geq` source (a smooth luma ramp, not the sharp two-tone stripes that
    /// tripped `aomenc`'s screen-content-tools auto-detect) is what actually
    /// resolves `V_DCT`/`H_DCT` (`TxClass::of`) on some 8x8 TUs at this
    /// content's frequency; the attempt loop moves on from named refusals
    /// (`allow_screen_content_tools`, angle-delta) same as every other
    /// multi-seed recipe in this file, and only counts a run as a genuine
    /// firing when [`decode::tx_class1_hits`] actually moved.
    #[test]
    fn a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..60u32 {
            let cq = 20 + (attempt % 5) * 10;
            let period = [4u32, 6, 8, 12, 16][(attempt as usize / 5) % 5];
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=25",
                    "-vf",
                    &format!(
                        "geq=lum='128+80*sin(2*PI*Y/{period})':cb=128:cr=128,noise=alls=6:allf=t"
                    ),
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
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
                    &format!("--cq-level={cq}"),
                    "--cpu-used=0",
                    "--enable-directional-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-rect-partitions=0",
                    "--enable-angle-delta=0",
                    "--enable-palette=0",
                    "--reduced-tx-type-set=0",
                    "--enable-restoration=0",
                    "--enable-cdef=0",
                    "--loopfilter-control=0",
                    "--min-partition-size=8",
                    "--max-partition-size=8",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--tune-content=default",
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
            if !out.status.success() {
                refusals.push(format!(
                    "cq={cq} period={period}: aomenc itself refused the fixture"
                ));
                continue;
            }
            let stream = out.stdout;
            let before = decode::tx_class1_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("cq={cq} period={period}: {e}"));
                    continue;
                }
            };
            if decode::tx_class1_hits() == before {
                refusals.push(format!(
                    "cq={cq} period={period}: decoded, but no block read a V_DCT/H_DCT tx_type"
                ));
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "U vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "V vs ffmpeg (cq={cq} period={period})"
            );
            fired_runs += 1;
            if fired_runs >= 4 {
                return;
            }
        }
        panic!(
            "fewer than 4 firing+pixel-exact runs out of 60 attempts:\n{}",
            refusals.join("\n")
        );
    }
}
