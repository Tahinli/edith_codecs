use super::*;
use super::support::*;
use super::gates_a::*;
use crate::encode::{Picture as Pic, encode_key_frame_with_ctx, encode_sequence_with_ctx};
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-av1oracledepth: the depth-generic 4:4:4 oracle comparator, armed
    /// on the same pinned 10-bit 4:4:4 cell
    /// `a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit`
    /// gates (fixture sha256
    /// `84df2f37cc7982f9a572443d85dfcc13bf366a64f3fb567391dcd5acd58a0ad2`):
    /// every plane of every SHOWN frame must come back byte-exact against
    /// ffmpeg's `yuv444p10le` rawvideo of the same bytes, with the depth read
    /// from the stream's own sequence header, never assumed.
    ///
    /// The flip control is the crate's own comparator-liveness shape (the
    /// same one
    /// [`every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg`] runs for
    /// the other helpers): with a thread-local ffmpeg shim that rotates ONE
    /// byte of the oracle's own rawvideo stdout, the compare must move from
    /// 0 to EXACTLY 1 differing sample in the tampered plane and stay at 0 in
    /// the other two -- a comparator that cannot see a one-sample difference
    /// would make every "byte-exact" claim above vacuous.
    #[test]
    fn the_444_depth_helper_is_byte_exact_on_a_pinned_10bit_444_cell() {
        const NAME: &str = "the_444_depth_helper_is_byte_exact_on_a_pinned_10bit_444_cell";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let _gate_lock = lock_gate_counters();
        let path = crate_pin("444_lossy_palette_chroma_352x242_10b.obu");
        let stream = read_pin(&path, 24875, 0x7fda_dbb4_3bbf_9af3, NAME);
        // The fixture is 16 SHOWN frames; ffmpeg's rawvideo muxer emits shown
        // frames only, and so does decode_stream's return.
        const W: usize = 352;
        const H: usize = 242;
        const FRAMES: usize = 16;
        assert_eq!(stream_bit_depth(&stream, NAME), 10, "{NAME}: pin depth");
        let ours = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned 10-bit 4:4:4 cell was refused: {e}"));
        assert_eq!(ours.len(), FRAMES, "{NAME}: decoded frame count");
        let theirs = ffmpeg_decode_sequence_444_depth(&stream, W, H, FRAMES, 10, "yuv444p10le");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            for (plane, x, y) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
                if let Some(at) = x.iter().zip(y).position(|(p, q)| p != q) {
                    panic!(
                        "{NAME}: {plane} differs from ffmpeg's yuv444p10le at frame {i} sample \
                         {at} (ours {} vs {}); {n} of {m} samples differ",
                        x[at],
                        y[at],
                        n = x.iter().zip(y).filter(|(p, q)| p != q).count(),
                        m = x.len()
                    );
                }
            }
        }
        eprintln!(
            "{NAME}: {FRAMES} shown frames byte-exact vs ffmpeg yuv444p10le on the pinned \
             10-bit 4:4:4 cell"
        );

        // ---- the flip control: one wrong oracle sample must move the count
        //      0 -> 1 in exactly the tampered plane -------------------------
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let real = if let Some(p) = std::env::var_os("EC_AV1_FFMPEG") {
            std::path::PathBuf::from(p)
        } else {
            let found = Command::new("ffmpeg")
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            assert!(found, "{NAME}: no working ffmpeg on PATH to wrap");
            let path_env = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path_env)
                .map(|d| d.join("ffmpeg"))
                .find(|p| p.is_file())
                .expect("{NAME}: ffmpeg is on PATH but not resolvable to a file")
        };
        std::fs::write(dir.join("real-ffmpeg"), real.as_os_str().as_encoded_bytes())
            .expect("recording the real ffmpeg's path for the shim");
        let script = r#"#!/bin/sh
REAL=$(cat "$(dirname "$0")/real-ffmpeg")
OUT=$(mktemp)
"$REAL" "$@" > "$OUT" || exit $?
while read -r o; do
  [ -n "$o" ] || continue
  b=$(od -An -tu1 -j "$o" -N 1 "$OUT" 2>/dev/null | tr -d ' \n')
  [ -n "$b" ] || continue
  esc=$(printf '\\%03o' $(( (b + 128) % 256 )))
  printf "$esc" | dd of="$OUT" bs=1 seek="$o" count=1 conv=notrunc 2>/dev/null
done < "$(dirname "$0")/offsets"
cat "$OUT"
rm -f "$OUT"
exit 0
"#;
        let shim = dir.join("one-sample-wrong-ffmpeg.sh");
        std::fs::write(&shim, script).expect("writing the ffmpeg shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("the ffmpeg shim must be executable");
        }
        let set_offsets = |bytes: &[usize]| {
            let body: String = bytes
                .iter()
                .map(|b| format!("{b}\n"))
                .collect::<Vec<_>>()
                .concat();
            std::fs::write(dir.join("offsets"), body).expect("writing the offsets file");
        };
        let count = |theirs_t: &[Pic]| -> [usize; 3] {
            let mut wrong = [0usize; 3];
            for (a, b) in ours.iter().zip(theirs_t) {
                for (plane, (x, y)) in
                    [(0usize, (&a.y, &b.y)), (1, (&a.u, &b.u)), (2, (&a.v, &b.v))]
                {
                    wrong[plane] += x.iter().zip(y).filter(|(p, q)| p != q).count();
                }
            }
            wrong
        };
        // Frame bytes are 3 planes x W*H samples x 2 bytes (10-bit container).
        let plane_bytes = W * H * 2;
        let mut blind: Vec<String> = Vec::new();
        set_ffmpeg_path(Some(shim.clone()));
        for (plane, base) in [("Y", 0usize), ("U", plane_bytes), ("V", 2 * plane_bytes)] {
            // Byte 2000 of the named plane: sample 1000, still frame 0 on
            // every plane at this geometry.
            set_offsets(&[base + 2000]);
            let theirs_t =
                ffmpeg_decode_sequence_444_depth(&stream, W, H, FRAMES, 10, "yuv444p10le");
            let got = count(&theirs_t);
            let mut expected = [0usize, 0, 0];
            expected[match plane {
                "Y" => 0,
                "U" => 1,
                _ => 2,
            }] = 1;
            if got != expected {
                blind.push(format!(
                    "{plane} arm: counted {got:?}, expected {expected:?} (exactly the one \
                     tampered sample)"
                ));
            }
        }
        set_ffmpeg_path(None);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            blind.is_empty(),
            "{NAME}: {} of 3 flip arms stayed GREEN against an ffmpeg that is wrong by exactly \
             one sample -- {blind:#?} -- the comparator cannot see a difference, so its \
             byte-exact verdict proves nothing (class comparator-that-never-reads-the-oracle)",
            blind.len()
        );
        eprintln!(
            "{NAME}: flip control bites on Y, U and V separately (0 -> 1 per tampered plane)"
        );
    }

    /// lane-av1oracledepth: `ffmpeg_decode_sequence_10bit` must REFUSE a
    /// wrong-depth stream by the stream's OWN sequence header, the way the
    /// 12-bit gates read `assert_12bit_sequence_header`. An 8-bit 4:2:0
    /// stream handed to the 10-bit helper is the wrong-depth shape: ffmpeg
    /// converts it to `yuv420p10le` and sizes no plane differently, so
    /// without the depth assert a gate could compare a converted 8-bit
    /// reference against our 10-bit decode and read exact.
    #[test]
    fn the_10bit_oracle_helper_refuses_a_wrong_depth_stream() {
        const NAME: &str = "the_10bit_oracle_helper_refuses_a_wrong_depth_stream";
        let _gate_lock = lock_gate_counters();
        let path = crate_pin("mix176_offtile_chroma_clip.obu");
        let stream = read_pin(&path, 4581, 0x5e31be5efc9c3c1d, NAME);
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            8,
            "{NAME}: the control pin must really be 8-bit, or this proves nothing"
        );
        assert_eq!(
            stream_subsampling(&stream, NAME),
            (1, 1),
            "{NAME}: the control pin must really be 4:2:0, or the refusal below could be the \
             shape guard instead of the depth guard"
        );
        let msg = panic_message(|| {
            ffmpeg_decode_sequence_10bit(&stream, 176, 144, 1);
        })
        .unwrap_or_else(|| {
            panic!(
                "{NAME}: ffmpeg_decode_sequence_10bit ACCEPTED an 8-bit stream -- ffmpeg \
                 converted it to yuv420p10le and the helper returned a comparison that could \
                 pass green"
            )
        });
        assert!(
            msg.contains("not 10-bit"),
            "{NAME}: the refusal must name the depth guard (\"not 10-bit\"); it said: {msg}"
        );
        eprintln!("{NAME}: refused -> {msg}");
    }

    /// lane-av1rawhelper: `assert_rawvideo_matches` used to pack our samples
    /// as `u16` for EVERY stream, while `aomdec --rawvideo` writes 8 bits
    /// per sample for an 8-bit stream. An 8-bit caller therefore died on a
    /// raw-SIZE assertion (2359296 vs 1179648) without comparing a single
    /// sample, and a high-bitdepth caller was the only one that could work.
    ///
    /// Two arms, both on a real `aomenc` 8-bit stream:
    ///
    /// 1. the correct label compares real samples -- the byte counts it
    ///    walks equal the oracle's, so every `u8` in the 8-bit pack is
    ///    zipped against a real oracle byte;
    /// 2. a WRONG label fails by name, quoting both depths and the fact that
    ///    the pack would compare nothing, rather than by size. Arm 2 is what
    ///    keeps arm 1 from being a green that a reverted check would also
    ///    give.
    #[test]
    fn the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth() {
        const NAME: &str = "the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth";
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() || !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no aomenc/aomdec oracle or no ffmpeg");
            return;
        }
        let (w, h) = (192usize, 128usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &gradients_source(7, w, h, "duration=0.24:rate=25"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "0.24",
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
            "{NAME}: ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=40",
            "--cpu-used=2",
            "--threads=1",
            "--row-mt=0",
            "--limit=6",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{NAME}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            8,
            "{NAME}: the fixture must really be 8-bit, or this gate measures nothing"
        );
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-raw8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &stream).expect("writing the stream");
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the 8-bit stream no longer decodes: {e}"))
            .len();
        assert!(frames > 0, "{NAME}: nothing decoded");

        // ARM 2 first: the wrong label must be refused BY NAME. A helper that
        // packed u16 unconditionally got this far and then failed on size.
        let wrong = std::panic::catch_unwind(|| {
            assert_rawvideo_matches(&obu, &stream, NAME, frames, 10);
        });
        let boom = wrong
            .err()
            .map(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
            .unwrap_or_default();
        assert!(
            boom.contains("sequence header says 8-bit") && boom.contains("compares nothing"),
            "{NAME}: a 10-bit label on an 8-bit stream must be refused by the depth check \
             naming both depths; the panic read: {boom}"
        );

        // ARM 1: the correct label compares real samples. The pre-fix helper
        // reached a size mismatch here (ours 2x the oracle); reaching the
        // sample compare is what this arm proves.
        assert_rawvideo_matches(&obu, &stream, NAME, frames, 8);
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("{NAME}: {frames} 8-bit frame(s) byte-identical to aomdec --rawvideo");
    }

    /// lane-svtinter: an SVT-AV1 screen stream whose frame height is not a
    /// multiple of the superblock size codes PALETTE blocks in the bottom
    /// (and right) superblock row that hang off the mi grid. Spec 5.11.50
    /// `palette_tokens` reads the colour-index map only over the block's
    /// `onscreenWidth x onscreenHeight` corner and replicates it outward;
    /// this decoder read the full block grid, so it consumed colour-index
    /// symbols libaom never wrote and desynced the rest of the tile -- either
    /// a wrong bottom superblock row (1599 luma samples in this stream's
    /// frame 0, ~80k per inter frame after) or, further along the same tile,
    /// a bogus "a Golomb tail longer than this decoder reads" refusal.
    ///
    /// Unlike its lane-svt1 sibling above this one BUILDS its stream: the
    /// recipe is synthetic (`smptebars,drawgrid` through `libsvtav1 -preset 8
    /// -svtav1-params screen-content-mode=1`), and 384x152 puts a 32x32
    /// palette block at mi row 32 of a 38-mi-tall frame, 24 of its 32 rows
    /// on screen. `palette_onscreen_cut_hits` is asserted non-zero so the row
    /// cannot pass on a stream that never cuts a palette map (class
    /// gate-blind-to-feature).
    #[test]
    fn an_svt_screen_palette_map_cut_by_the_frame_edge_decodes_exactly() {
        const NAME: &str = "an_svt_screen_palette_map_cut_by_the_frame_edge_decodes_exactly";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let (width, height) = (384usize, 152usize);
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "smptebars=s=384x152:r=12,drawgrid=w=8:h=8:t=1:c=black",
                "-frames:v",
                "4",
                "-c:v",
                "libsvtav1",
                "-preset",
                "8",
                "-crf",
                "30",
                "-svtav1-params",
                "lp=1:screen-content-mode=1",
                "-g",
                "12",
                "-f",
                "obu",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        if !out.status.success() || out.stdout.is_empty() {
            eprintln!(
                "SKIP {NAME}: no libsvtav1 in this ffmpeg: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let stream = out.stdout;
        crate::decode::reset_palette_onscreen_cut_hits();
        let decoded = decode_stream(&stream).expect("the SVT-AV1 screen stream decodes");
        assert_eq!(decoded.len(), 4, "{NAME}: frame count");
        assert!(
            crate::decode::palette_onscreen_cut_hits() > 0,
            "{NAME}: no palette map is cut by the frame edge here -- the gate would pass blind"
        );
        let want = ffmpeg_decode_sequence(&stream, width, height, decoded.len());
        assert_eq!(want.len(), decoded.len(), "{NAME}: ffmpeg frame count");
        for (i, (got, want)) in decoded.iter().zip(&want).enumerate() {
            assert_eq!(got.y, want.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME}: frame {i} V vs ffmpeg");
        }
    }

    /// lane-svt1: an SVT-AV1 (v3.1.2, `-preset 8`) screen key frame codes 16x16
    /// PALETTE blocks with `skip == 1` whose `tx_depth` split the luma
    /// transform. [`crate::decode::decode_block`] armed the palette prediction
    /// only when the transform covered the whole block, so that one block
    /// predicted DC off its own edges instead: the right symbols (the whole mode
    /// ladder diffs clean against an instrumented aomdec) and the wrong pixels
    /// -- 812 luma samples in frame 0, carried by the V_PRED blocks under it to
    /// 10122 samples over the 12-frame stream the defect was found on.
    ///
    /// lane-av1pinspec2: the gate used to RIDE an external crop named by
    /// `EC_AV1_SVT1_STREAM` and SKIP when that was unset, so it never ran on a
    /// stock box -- which is exactly why its oracle count could not be pinned
    /// (an assert on a gate that never executes is an unexercised assert wearing
    /// a pin's clothes). It now runs on the COMMITTED pin
    /// `crates/ec-av1/fixtures/svt1-split-tx-pin.obu`.
    ///
    /// The pin is a REPLAY, not a fresh capture, and the recipe is recorded
    /// because it reproduces the bytes exactly (two independent encodes on this
    /// box, SVT-AV1 v3.1.2, both sha256
    /// `c9d9c0ae934fba72fc9a9374bc36853f290796a8a4d6442140e239669e0b341f`,
    /// 1838 bytes):
    ///
    ///     ffmpeg -v error -f lavfi \
    ///       -i "smptehdbars=s=192x152:r=12,drawgrid=w=8:h=8:t=1:c=black" \
    ///       -frames:v 1 -c:v libsvtav1 -preset 8 -crf 50 \
    ///       -svtav1-params "lp=1:screen-content-mode=1" -g 12 -f obu out.obu
    ///
    /// `-frames:v 1` is the part lane-svt1's search lacked: its 6x5x6 sweep ran
    /// FOUR frames per stream, and the ~200k-sample misses that killed every
    /// candidate were on the INTER frames (`lanes/svt1.report.md`). One key
    /// frame has no inter frames, and this one reaches the fixed arm
    /// (`skip_split_tx_override_hits == 1`) and is pixel-exact.
    ///
    /// THE COUNT, and why it is derived from the wire rather than from a
    /// literal. The oracle is handed the number of frame OBUs this stream
    /// actually codes, counted with `Av1Parser` below -- never our decode's
    /// length, which is the thing under test. A header count would OVER-count
    /// a stream carrying `show_existing_frame` (spec 5.9.5: it codes no pixels,
    /// so the header count is larger than the shown-frame count), so that case
    /// is asserted impossible on this pin rather than assumed: the loop refuses
    /// any frame header with `show_existing_frame` set, which is what makes
    /// "one header == one shown frame" a checked fact here instead of an
    /// argument.
    ///
    /// `EC_AV1_SVT1_STREAM` still overrides the pin, so a re-capture is one env
    /// var away; `crate_pin` falls back to `pin_dir()` for the same reason.
    ///
    /// `skip_split_tx_override_hits` is asserted non-zero so the row cannot
    /// pass on a stream that never reaches the fixed arm (class
    /// gate-blind-to-feature).
    #[test]
    fn an_svt_screen_palette_block_with_a_split_transform_decodes_exactly() {
        const NAME: &str = "an_svt_screen_palette_block_with_a_split_transform_decodes_exactly";
        const PIN: &str = "svt1-split-tx-pin.obu";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = match std::env::var("EC_AV1_SVT1_STREAM") {
            Ok(p) => std::path::PathBuf::from(p),
            // The name is spelled as a LITERAL here even though `PIN` is right
            // above, on purpose: `gate_coverage::pin_inventory` matches a pin
            // read by the quote that follows the callee name, so a const
            // argument would leave this pin outside the "every pin a gate reads
            // is a committed crate-local copy" invariant. The dup is the
            // coverage -- and it is also why this comment must NOT quote that
            // spelling: the scanner reads prose too, and a quoted spelling here
            // parses as a pin called " and would not see a const argument, ...".
            Err(_) => crate_pin("svt1-split-tx-pin.obu"),
        };
        let data = require_pin(&path, PIN);
        // The ENCODE's frame count, read off the WIRE: parse this stream's own
        // OBUs and count the frame headers. `show_existing_frame` is refused
        // here on purpose -- such a header codes no pixels, so counting it
        // would make this count larger than the frames the oracle can decode.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut frame_headers = 0usize;
        while pos < data.len() {
            let Ok(obu) = parser.parse_obu(&data[pos..]) else {
                break;
            };
            pos += obu.total_size.max(1);
            let header = match &obu.kind {
                ObuKind::Frame(h, _) | ObuKind::FrameHeader(h) => h,
                _ => continue,
            };
            assert!(
                !header.show_existing_frame,
                "{NAME}: frame header {frame_headers} sets show_existing_frame, so the header \
                 count is not the shown-frame count and this gate's oracle budget would be wrong"
            );
            frame_headers += 1;
        }
        assert!(
            frame_headers > 0,
            "{NAME}: {} carries no frame header at all",
            path.display()
        );
        crate::decode::reset_skip_split_tx_override_hits();
        let decoded = decode_stream(&data).expect("the pinned SVT-AV1 screen stream decodes");
        assert!(
            crate::decode::skip_split_tx_override_hits() > 0,
            "{NAME}: no skipped split-transform palette block in {} -- the gate would pass blind",
            path.display()
        );
        assert_eq!(
            decoded.len(),
            frame_headers,
            "{NAME}: the stream codes {frame_headers} frame(s), the decode showed {}",
            decoded.len()
        );
        let (width, height) = (decoded[0].width, decoded[0].height);
        let want = ffmpeg_decode_sequence(&data, width, height, frame_headers);
        for (i, (got, want)) in decoded.iter().zip(&want).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg");
        }
    }

    /// `decode_stream` agrees with ffmpeg/dav1d on the same wire bytes -- an
    /// independent decoder, not just this crate checking its own tile path.
    #[test]
    fn decode_stream_agrees_with_ffmpeg_on_a_gop() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP decode_stream_agrees_with_ffmpeg_on_a_gop: no ffmpeg");
            return;
        }
        // lane-av1straddle: 216x96 joins 128x64 because only a size whose
        // 32x32 blocks straddle the right-hand frame edge can see the writer
        // coding a transform unit that starts outside the frame -- libdav1d
        // AND libaom both refused the whole key frame while it did, and this
        // crate's own (lenient) decoder only differed in the last block's V.
        for &(width, height) in &[(128usize, 64usize), (216usize, 96usize)] {
            let pictures: Vec<_> = (0..3)
                .map(|i| panned_test_card(width, height, i * 3))
                .collect();
            let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
            let ffmpeg_frames = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
            let decoded = decode_stream(&encoded.stream).unwrap();
            assert_eq!(decoded.len(), 3);
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{width}x{height} frame {i} luma vs ffmpeg");
                assert_eq!(got.u, want.u, "{width}x{height} frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "{width}x{height} frame {i} V vs ffmpeg");
            }
            // ... and the encoder's own reconstruction is what ffmpeg sees,
            // so the check fails on a writer-side desync too, not only on a
            // decoder-side one.
            for (i, (frame, want)) in encoded.frames.iter().zip(&ffmpeg_frames).enumerate() {
                let rec = &frame.reconstruction;
                assert_eq!(
                    rec.y, want.y,
                    "{width}x{height} frame {i} luma: reconstruction vs ffmpeg"
                );
                assert_eq!(
                    rec.u, want.u,
                    "{width}x{height} frame {i} U: reconstruction vs ffmpeg"
                );
                assert_eq!(
                    rec.v, want.v,
                    "{width}x{height} frame {i} V: reconstruction vs ffmpeg"
                );
            }
        }
    }

    /// lane-av1rect: the encoder used to REFUSE every size where a 32x32
    /// block straddles BOTH true frame edges at once ("straddles the true
    /// frame edge in a way that needs a rectangular (HORZ/VERT) transform
    /// this encoder does not code yet"). That refusal was a misreading of
    /// spec 5.11.4: a block whose `hasRows` and `hasCols` are BOTH false
    /// reads no partition symbol at all and is `PARTITION_SPLIT` by
    /// inference, down to the 8x8 leaves whose own mi origin is inside the
    /// frame -- exactly the leaves this writer already codes for the
    /// one-axis case, and no rectangular transform anywhere. The sweep walks
    /// the corners around the 32-multiples (each width has a 16x16 column
    /// whose right half is outside, each height a row whose bottom half is,
    /// so the bottom-right 16x16 of every size here is cut on both axes) and
    /// three-way compares: our decoder, the encoder's own reconstruction and
    /// ffmpeg, over a 4-frame GOP so the inter writer's copy of the same
    /// signaling is covered too. `edge_both_cut_hits` proves the decoder
    /// really took the inferred-split path rather than the gate passing on
    /// geometry that never fires it.
    #[test]
    fn a_sweep_of_doubly_straddling_sizes_round_trips_through_ffmpeg() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_sweep_of_doubly_straddling_sizes_round_trips_through_ffmpeg: no ffmpeg"
            );
            return;
        }
        const FRAMES: usize = 4;
        let mut both_cut = 0usize;
        for &width in &[200usize, 216, 232, 248] {
            for &height in &[104usize, 120, 136, 152] {
                let pictures: Vec<_> = (0..FRAMES as i64)
                    .map(|i| panned_test_card(width, height, i * 3))
                    .collect();
                let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
                    .unwrap_or_else(|e| panic!("{width}x{height}: encoder refused: {e}"));
                // Reset per attempt, never a delta across sizes (class
                // counter-from-refused-stream).
                crate::decode::reset_edge_both_cut_hits();
                let decoded = decode_stream(&encoded.stream)
                    .unwrap_or_else(|e| panic!("{width}x{height}: our decoder refused: {e}"));
                both_cut += crate::decode::edge_both_cut_hits().iter().sum::<usize>();
                let ffmpeg_frames = ffmpeg_decode_sequence(&encoded.stream, width, height, FRAMES);
                assert_eq!(
                    ffmpeg_frames.len(),
                    FRAMES,
                    "{width}x{height}: ffmpeg decoded {} of {FRAMES} frames",
                    ffmpeg_frames.len()
                );
                assert_eq!(decoded.len(), FRAMES, "{width}x{height}: frame count");
                for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(got.y, want.y, "{width}x{height} frame {i} luma vs ffmpeg");
                    assert_eq!(got.u, want.u, "{width}x{height} frame {i} U vs ffmpeg");
                    assert_eq!(got.v, want.v, "{width}x{height} frame {i} V vs ffmpeg");
                }
                for (i, (frame, want)) in encoded.frames.iter().zip(&ffmpeg_frames).enumerate() {
                    let rec = &frame.reconstruction;
                    assert_eq!(
                        rec.y, want.y,
                        "{width}x{height} frame {i} luma: recon vs ffmpeg"
                    );
                    assert_eq!(
                        rec.u, want.u,
                        "{width}x{height} frame {i} U: recon vs ffmpeg"
                    );
                    assert_eq!(
                        rec.v, want.v,
                        "{width}x{height} frame {i} V: recon vs ffmpeg"
                    );
                }
            }
        }
        assert!(
            both_cut > 0,
            "no partition node in the whole sweep was coded as the both-axes-cut \
             inferred split -- the gate is blind to the feature it pins"
        );
    }

    /// lane-corner: sizes whose LAST superblock is cut by the true frame
    /// edge on BOTH axes with more than half of each still inside -- so
    /// `has_cols` and `has_rows` are both true and the spec lets that
    /// superblock stay a whole 64x64 `PARTITION_NONE` block, which this
    /// encoder's roots take (`EC_AV1_B64EDGE` on the inter side, the key
    /// frame's own `Superblock::Whole`). The sibling luma-only test
    /// (`encoder.rs`'s `a_non_superblock_aligned_clip_...`) is blind to
    /// chroma (class `metric-blind-to-a-plane`), so this one compares all
    /// three planes three ways: our decoder, the encoder's own
    /// reconstruction, and ffmpeg.
    #[test]
    fn a_doubly_cut_superblock_root_round_trips_in_every_plane() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP a_doubly_cut_superblock_root_round_trips_in_every_plane: no ffmpeg");
            return;
        }
        const FRAMES: usize = 4;
        // Each size leaves MORE than half of its last superblock row (and,
        // but for 200x120's 8-px column, of its last column) inside, which is
        // what makes the root legal there at all; 248x168/232x168/248x184 are
        // cut on BOTH axes at once. 1920x1080 -- the user's own export shape
        // -- is the same case: 1080 = 16*64 + 56, a 56-px bottom straddle.
        for &(width, height) in &[(248usize, 168usize), (232, 168), (200, 120), (248, 184)] {
            let pictures: Vec<_> = (0..FRAMES as i64)
                .map(|i| panned_test_card(width, height, i * 3))
                .collect();
            let _ = crate::encode::take_b64_edge_hits();
            let _ = crate::encode::take_i64_root_hits();
            let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
                .unwrap_or_else(|e| panic!("{width}x{height}: encoder refused: {e}"));
            let edges = crate::encode::take_b64_edge_hits();
            let [i64_whole, i64_offered, i64_edge] = crate::encode::take_i64_root_hits();
            eprintln!(
                "{width}x{height}: edge inter 64 roots {edges}, key 64 roots {i64_whole} of \
                 {i64_offered} offered, {i64_edge} of them cut by the frame edge"
            );
            // gate-blind-to-feature: without this the three-way compare below
            // passes on a stream that never coded a whole root at the cut
            // superblock at all.
            assert!(
                i64_edge > 0,
                "{width}x{height}: no 64x64 root sat on a superblock the frame edge cuts"
            );
            let decoded = decode_stream(&encoded.stream)
                .unwrap_or_else(|e| panic!("{width}x{height}: our decoder refused: {e}"));
            let ffmpeg_frames = ffmpeg_decode_sequence(&encoded.stream, width, height, FRAMES);
            assert_eq!(
                ffmpeg_frames.len(),
                FRAMES,
                "{width}x{height}: ffmpeg frame count"
            );
            assert_eq!(decoded.len(), FRAMES, "{width}x{height}: frame count");
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{width}x{height} frame {i} luma: ours vs ffmpeg"
                );
                assert_eq!(
                    got.u, want.u,
                    "{width}x{height} frame {i} U: ours vs ffmpeg"
                );
                assert_eq!(
                    got.v, want.v,
                    "{width}x{height} frame {i} V: ours vs ffmpeg"
                );
            }
            for (i, (frame, want)) in encoded.frames.iter().zip(&ffmpeg_frames).enumerate() {
                let rec = &frame.reconstruction;
                assert_eq!(
                    rec.y, want.y,
                    "{width}x{height} frame {i} luma: recon vs ffmpeg"
                );
                assert_eq!(
                    rec.u, want.u,
                    "{width}x{height} frame {i} U: recon vs ffmpeg"
                );
                assert_eq!(
                    rec.v, want.v,
                    "{width}x{height} frame {i} V: recon vs ffmpeg"
                );
            }
        }
    }

    /// A hand-built 3-frame stream that actually fires `GOLDEN_FRAME`
    /// through [`decode_stream`], proving the round-4 flip (`decode.rs`'s
    /// `decode_inter_block` `GOLDEN_FRAME` arm) the way 120+ real aomenc
    /// attempts never could: frame 0 is a real key frame (this crate's own
    /// encoder, real texture), refreshing all 8 DPB slots as spec requires --
    /// slot 3 keeps that exact picture from here on. Frame 1 is a hand-coded
    /// inter frame whose lone 32x32 block is *intra* (`DC_PRED`, `skip`),
    /// refreshing only slot 0 with a flat 128 block -- distinguishable from
    /// frame 0's real texture, so slot 0 (`LAST_FRAME`) and slot 3
    /// (`GOLDEN_FRAME`) hold visibly different pictures once frame 2 runs.
    /// Frame 2 is a hand-coded inter frame naming `ref_frame_idx[3] = 3`
    /// (GOLDEN_FRAME's slot) and `ref_frame_idx[0] = 0` (LAST_FRAME's), whose
    /// lone block codes `single_ref` all the way out to `GOLDEN_FRAME`
    /// (`p1=0, p3=1, p5=1`) and a `skip` zero-MV `NEARESTMV`: a direct copy
    /// of GOLDEN's picture, so its output must equal frame 0's own
    /// reconstruction exactly, and must differ from frame 1's -- proof the
    /// decoder actually read the golden plane, not silently substituted
    /// `LAST_FRAME`'s. Rounds 4-9 masked and re-masked this arm chasing a
    /// deblock/MC residue that turned out to be film-grain synthesis
    /// (lane-av1golden7's `apply_grain` refusal, unrelated to GOLDEN);
    /// lane-av1golden8 fixed mvstack's single-ref extension pass, clearing
    /// the last real defect (`pinned_golden7` non_last_ref_hits 0->2), so
    /// this fixture flips back to its round-4 pixel-exact form. ffmpeg
    /// decodes the identical wire bytes as the foreign oracle
    /// (shared-oracle-blindness class): this crate's own decoder agreeing
    /// with itself would prove nothing.
    #[test]
    fn a_hand_built_golden_reference_decodes_pixel_exact_against_ffmpeg() {
        let fctx = &crate::decode::FrameCtx::new();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_hand_built_golden_reference_decodes_pixel_exact_against_ffmpeg: no ffmpeg"
            );
            return;
        }
        use crate::cdf_state::Cdfs;
        use crate::decode::{intra_inter_ctx, non_last_ref_hits, q_ctx_of};
        use crate::encode::{inter_frame_headers, key_frame_headers};
        use crate::frame::frame_obu;
        use crate::msac::SymbolEncoder;
        use crate::mvstack::{
            GOLDEN_FRAME, MiGrid, find_mv_stack, single_ref_p1_ctx, single_ref_p3_ctx,
            single_ref_p5_ctx,
        };
        use crate::obu::temporal_delimiter;

        let (width, height) = (32usize, 32usize);
        let base_q_idx = 100u8;
        // The whole picture is one 32x32 block: `partition_ctx`/`skip_ctx`/
        // `intra_inter_ctx`'s neighbour inputs are all "no neighbour yet" --
        // a fresh `Neighbours`' `above_side_mi`/`left_side_mi` start at `SB`
        // (64), so `partition_ctx_mi`'s `side_mi * 2 <= side` (32) is false
        // on both axes (ctx 0), and `above_skip`/`left_skip`/`above_inter`/
        // `left_inter`/`above_ref`/`left_ref` all start `false`/`-1`
        // (decode.rs `Neighbours::new`) -- computed here without
        // constructing the (module-private) `Neighbours` itself.
        const PARTITION_NONE: usize = 0;
        let ii_ctx = intra_inter_ctx(false, false, false, false);
        let sr_p1_ctx = single_ref_p1_ctx(None, None, None, None);
        let sr_p3_ctx = single_ref_p3_ctx(None, None, None, None);
        let sr_p5_ctx = single_ref_p5_ctx(None, None, None, None);

        for run in 0..4 {
            let picture = test_card(width, height);
            let key = encode_key_frame_with_ctx(&picture, base_q_idx, 0.5, fctx).unwrap();
            let (seq, _) = key_frame_headers(width, height, base_q_idx).unwrap();

            // Frame 1: hand-coded, intra-only, refreshes slot 0 with a flat
            // 128 block (no neighbours to predict `DC_PRED` from).
            let (_, mut header1) = inter_frame_headers(width, height, base_q_idx, 1, 0).unwrap();
            // These two tiles are hand-coded from a FRESH `Cdfs`, so the
            // frames must not carry CDF state the way the encoder's own inter
            // frames now do (spec 7.20): with the bit set, what each frame
            // stores is what it started from -- the defaults -- which is
            // exactly what the next hand-coded tile assumes.
            header1.disable_frame_end_update_cdf = true;
            let tile1 = {
                let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
                let mut enc = SymbolEncoder::new();
                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[0]);
                enc.symbol(1, &mut cdfs.skip[0]); // skip
                enc.symbol(0, &mut cdfs.intra_inter[ii_ctx]); // is_inter = false
                enc.symbol(0, &mut cdfs.y_mode[3]); // DC_PRED, size group 3 (32x32)
                enc.symbol(0, &mut cdfs.uv_mode_cfl[0]); // DC_PRED
                enc.finish()
            };
            let mut stream = key.stream.clone();
            stream.extend(temporal_delimiter());
            stream.extend(frame_obu(&seq, &header1, &tile1).unwrap());

            // Frame 2: hand-coded, single_ref all the way to GOLDEN_FRAME,
            // skip zero-MV NEARESTMV -- a direct copy of slot 3 (frame 0).
            let (_, mut header2) = inter_frame_headers(width, height, base_q_idx, 2, 0).unwrap();
            header2.disable_frame_end_update_cdf = true;
            header2.ref_frame_idx[3] = 3; // GOLDEN_FRAME's own slot: frame 0's, untouched by frame 1
            let tile2 = {
                let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
                let mut enc = SymbolEncoder::new();
                let grid = MiGrid::new(8, 8);
                let stack = find_mv_stack(&grid, 0, 0, 8, 8, GOLDEN_FRAME, 8, 8);
                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[0]);
                enc.symbol(1, &mut cdfs.skip[0]); // skip
                enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]); // is_inter = true
                enc.symbol(0, &mut cdfs.single_ref[sr_p1_ctx][0]); // p1: LAST/LAST2/LAST3/GOLDEN group
                enc.symbol(1, &mut cdfs.single_ref[sr_p3_ctx][2]); // p3: GOLDEN/LAST3 sub-group
                enc.symbol(1, &mut cdfs.single_ref[sr_p5_ctx][4]); // p5: GOLDEN_FRAME
                enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not_new -> not NEWMV
                enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not_zero -> not GLOBALMV
                enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // nearest -> NEARESTMV, (0, 0)
                enc.finish()
            };
            stream.extend(temporal_delimiter());
            stream.extend(frame_obu(&seq, &header2, &tile2).unwrap());

            let before = non_last_ref_hits();
            let decoded = decode_stream(&stream).unwrap();
            assert_eq!(decoded.len(), 3, "run {run}: expected 3 pictures");
            assert!(
                non_last_ref_hits() > before,
                "run {run}: GOLDEN_FRAME never fired"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 3);
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "run {run} frame {i}: luma vs ffmpeg");
                assert_eq!(got.u, want.u, "run {run} frame {i}: U vs ffmpeg");
                assert_eq!(got.v, want.v, "run {run} frame {i}: V vs ffmpeg");
            }
            assert_eq!(
                decoded[0].y, key.reconstruction.y,
                "run {run}: frame 0 vs the encoder's own reconstruction"
            );
            assert_eq!(
                decoded[2].y, key.reconstruction.y,
                "run {run}: GOLDEN_FRAME's block did not reproduce the keyframe it names"
            );
            assert_ne!(
                decoded[2].y, decoded[1].y,
                "run {run}: LAST_FRAME's and GOLDEN_FRAME's slots were not actually \
                 distinguishable -- the gate proves nothing"
            );
        }
    }

    /// The determinism guard: same seed twice must byte-match ffmpeg's own
    /// output; different seeds must not. Regresses this fix if it ever
    /// breaks.
    #[test]
    fn gradients_source_is_reproducible_per_seed() {
        if !have_ffmpeg() {
            eprintln!("SKIP gradients_source_is_reproducible_per_seed: no ffmpeg");
            return;
        }
        fn render_hash(lavfi: &str) -> Vec<u8> {
            let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    lavfi,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(out.status.success(), "ffmpeg refused {lavfi}");
            out.stdout
        }
        let a1 = render_hash(&gradients_source(42, 64, 64, "duration=0.04:rate=25"));
        let a2 = render_hash(&gradients_source(43, 64, 64, "duration=0.04:rate=25"));
        let a3 = render_hash(&gradients_source(42, 64, 64, "duration=0.04:rate=25"));
        assert_eq!(a1, a3, "same seed must reproduce byte-identical output");
        assert_ne!(a1, a2, "different seeds must not collide");
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
        let configs: [(String, usize, usize, u32); 2] = [
            ("testsrc2=size=64x64:rate=1".to_string(), 64, 64, 15),
            (gradients_source(42, 64, 64, "rate=1"), 64, 64, 15),
        ];
        let mut verdicts = Vec::new();
        for (lavfi, width, height, crf) in configs {
            let lavfi = lavfi.as_str();
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

    /// lane-dgolomb: the `force_integer_mv` gate. libaom turns screen-content
    /// tools on for DARK, SMOOTH content -- its `av1_set_screen_content_options`
    /// counts 16x16 blocks with 2..=4 distinct luma values -- and then sets
    /// `force_integer_mv` on any inter frame whose source is pixel-identical to
    /// the previously coded source (`av1_is_integer_mv`: `cs_rate >= 0.8` and
    /// `C == T`, every collocated 8x8 block matching, is the only path that
    /// returns 1). Both hold on his real AV1 films, and this decoder used to
    /// drop `lower_mv_precision`'s `is_integer` half as unreachable, so every
    /// temporal MV candidate on such a frame came out at eighth-pel: the frame
    /// decoded without any error into a whole-frame prediction mismatch, and
    /// the desync surfaced blocks later as "a Golomb tail longer than this
    /// decoder reads".
    ///
    /// The source is SYNTHETIC (no film sample lives in `fixtures`): the
    /// near-flat dark pattern libaom's own screen-content detector counts as
    /// screen content, at the film-B crop size 1920x1024, SCROLLING globally
    /// 5 px per frame and frozen on frame 8 -- an exact duplicate of frame 7
    /// (the `C == T` trigger) whose twin is not among its own references, so
    /// its blocks still code real motion under `force_integer_mv`.
    ///
    /// lane-dpm1 made this gate SENSITIVE: with the `force_integer_mv` branch
    /// of [`crate::motion_field::lower_mv_precision`] stubbed out (`if false
    /// &&`) the stream decodes WRONG -- frame 8, luma sample 25695, 20 against
    /// ffmpeg's 19 -- where the previous patch-in-a-static-background recipe
    /// (and two others tried) still decoded byte-exact: there the used mv-stack
    /// slots came from agreeing spatial neighbours and the rounded temporal
    /// entries never won one. The gate asserts both halves: at least one INTER
    /// frame really carries `force_integer_mv` (class `gate-blind-to-feature`),
    /// and every shown frame decodes sample-exact against ffmpeg's own decode.
    #[test]
    fn a_libaom_force_integer_mv_stream_decodes_exact() {
        const NAME: &str = "a_libaom_force_integer_mv_stream_decodes_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let (w, h, frames) = (1920usize, 1024usize, 16usize);
        let mut raw = Vec::with_capacity(frames * w * h * 3 / 2);
        let mut off = 0usize;
        for f in 0..frames {
            // lane-dpm1: the WHOLE picture scrolls 5 px per frame and freezes
            // on frame 8, so (a) every block of the frozen frame -- including
            // mi(0,0), which has no spatial neighbour at all -- still codes
            // real motion against the references that are not its twin, and
            // (b) 40 eighth-pels projected over the pyramid's distances lands
            // on non-multiples of 8 (40/2 = 20 -> 16). A patch moving inside a
            // static background was NOT sensitive: there the used stack slots
            // came from spatial neighbours that agreed, and the rounded
            // temporal entries never won one.
            if f > 0 && f != 8 {
                off = (off + 5) % h;
            }
            let mut y = vec![0u8; w * h];
            for row in 0..h {
                let src = (row + off) % h;
                for col in 0..w {
                    // 2..=4 distinct values per 16x16 block, all dark: the
                    // screen-content detector's own counting rule.
                    y[row * w + col] =
                        16 + ((col / 32 + src / 32) % 5) as u8 + ((col / 4 + src / 4) % 3) as u8;
                }
            }
            raw.extend_from_slice(&y);
            raw.extend(std::iter::repeat_n(128u8, w * h / 2));
        }
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let src = std::env::temp_dir().join(format!("ec-av1-intmv-{}.yuv", std::process::id()));
        std::fs::write(&src, &raw).expect("raw source");
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .args(["-s", &format!("{w}x{h}"), "-r", "24", "-i"])
            .arg(&src)
            .args(["-an", "-threads", "1", "-g", "16", "-c:v", "libaom-av1"])
            .args([
                "-cpu-used",
                "6",
                "-b:v",
                "0",
                "-crf",
                "35",
                "-f",
                "obu",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&src);
        if !out.status.success() {
            eprintln!(
                "SKIP {NAME}: ffmpeg has no libaom-av1 encoder ({})",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let stream = out.stdout;

        // Half of the gate: the stream must actually carry the feature.
        let mut parser = Av1Parser::new();
        let (mut pos, mut int_mv_inter) = (0usize, 0usize);
        while pos < stream.len() {
            let obu = parser.parse_obu(&stream[pos..]).expect("libaom OBU parses");
            pos += obu.total_size.max(1);
            let header = match &obu.kind {
                ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => h,
                _ => continue,
            };
            if header.force_integer_mv && !header.frame_is_intra && header.use_ref_frame_mvs {
                int_mv_inter += 1;
            }
        }
        assert!(
            int_mv_inter > 0,
            "{NAME} went blind: libaom coded no inter frame with force_integer_mv"
        );

        let ours = decode_stream(&stream).expect("libaom screen-content stream decodes");
        let theirs = ffmpeg_decode_sequence(&stream, w, h, frames);
        assert_eq!(ours.len(), frames, "shown frame count");
        for (i, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
            for (plane, l, r) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
                let first = l.iter().zip(r.iter()).position(|(x, y)| x != y);
                assert!(
                    first.is_none(),
                    "frame {i} plane {plane} differs from ffmpeg at sample {} \
                     ({int_mv_inter} force_integer_mv inter frames in this stream)",
                    first.unwrap()
                );
                assert_eq!(
                    l.len(),
                    r.len(),
                    "frame {i} plane {plane}: ours has {} samples, ffmpeg's has {} -- a short \
                     plane would zip-truncate the compare above and hide a chroma-span defect",
                    l.len(),
                    r.len()
                );
            }
        }
    }

    /// lane-dpm1: the 128x128-INTRA-inside-an-INTER-frame chroma context gate.
    /// A 128x128 block codes its chroma as four TX_32X32 units, one per 64x64
    /// mu chunk, and libaom stamps the coefficient context PER UNIT. The two
    /// INTER mu-chunk sites set `mu_chroma` so the block tail re-stamps the
    /// four units in mu order; the INTRA-in-inter site did not, so the
    /// whole-block record left every unit carrying the TOP-LEFT unit's level.
    /// The next block then read its `txb_skip` off ctx 11 where libaom reads
    /// ctx 10 (class `override-slot-on-one-arm`) -- one symbol on the wrong
    /// CDF row, tile desynced, and because the loop-restoration coefficients
    /// live in the same tile data the damage showed up as a +-1 luma sample at
    /// the TOP of the frame (measured: frame 1, luma sample 66, ours 146 /
    /// ffmpeg 145, with 1.7M samples wrong behind it).
    ///
    /// Source: the repository's own 2160p colour-bar fixture at the native
    /// gate's crop (1920x1024 out of the middle), 12 frames, libaom
    /// `-cpu-used 6 -crf 45 -g 12` -- the first reference point of
    /// `bd_rate_screen_native`'s "bars 2160p" row, which is where the assertion
    /// caught it. The gate asserts the tool fired (class
    /// `gate-blind-to-feature`) and that every shown frame decodes sample-exact
    /// against ffmpeg's own decode.
    #[test]
    fn a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact() {
        const NAME: &str = "a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the bare `clip.exists()` -> SKIP had no env
        // escape, so on a tree with no root `fixtures/` this 128-intra-block
        // gate reported green having decoded nothing.
        let Some(clip) = crate::library_fixture::require("video/h264-2160p-23.976-8bit.mp4", NAME)
        else {
            return;
        };
        let (w, h, frames) = (1920usize, 1024usize, 12usize);
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let src = std::env::temp_dir().join(format!("ec-av1-i128-{}.yuv", std::process::id()));
        let load = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&clip)
            .args([
                "-frames:v",
                &frames.to_string(),
                "-vf",
                "crop=1920:1024:960:568",
            ])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .arg(&src), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            load.status.success(),
            "{NAME}: {}",
            String::from_utf8_lossy(&load.stderr)
        );
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .args(["-s", &format!("{w}x{h}"), "-r", "24", "-i"])
            .arg(&src)
            .args(["-an", "-threads", "1", "-g", "12", "-c:v", "libaom-av1"])
            .args([
                "-cpu-used",
                "6",
                "-b:v",
                "0",
                "-crf",
                "45",
                "-f",
                "obu",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&src);
        if !out.status.success() {
            eprintln!(
                "SKIP {NAME}: ffmpeg has no libaom-av1 encoder ({})",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let stream = out.stdout;
        let before = crate::decode::intra_128_in_inter_hits();
        let ours = decode_stream(&stream).expect("libaom bars stream decodes");
        assert!(
            crate::decode::intra_128_in_inter_hits() > before,
            "{NAME} went blind: no 128-rooted intra block inside an inter frame"
        );
        let theirs = ffmpeg_decode_sequence(&stream, w, h, frames);
        assert_eq!(ours.len(), frames, "shown frame count");
        for (i, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
            for (plane, l, r) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
                let first = l.iter().zip(r.iter()).position(|(x, y)| x != y);
                assert!(
                    first.is_none(),
                    "frame {i} plane {plane} differs from ffmpeg at sample {}",
                    first.unwrap()
                );
                assert_eq!(
                    l.len(),
                    r.len(),
                    "frame {i} plane {plane}: ours has {} samples, ffmpeg's has {} -- a short \
                     plane would zip-truncate the compare above and hide a chroma-span defect",
                    l.len(),
                    r.len()
                );
            }
        }
    }

    /// lane-lossless: the refusal became an implementation, so its gate became
    /// a DECODE-EXACT one (class `refusal-lifted-without-a-gate`). libaom at
    /// `-crf 0` codes `base_q_idx == 0` -- the same lossless frame his
    /// screen-capture row gets at `-crf 5` -- where every plane is TX_4X4 with
    /// the Walsh-Hadamard transform, no `tx_type` symbol is coded
    /// (`read_tx_type`, `decodetxb.c`: "No need to read transform type for
    /// lossless mode(qindex==0)") and `is_cfl_allowed` (`blockd.h`) narrows to
    /// a BLOCK_4X4 chroma plane block. The gate asserts the stream really is
    /// lossless (class `gate-blind-to-feature`), that every KEY frame decodes
    /// sample-exact against ffmpeg, and that the still-unimplemented lossless
    /// INTER frame is refused BY NAME rather than decoded wrong.
    #[test]
    fn a_lossless_libaom_key_frame_decodes_sample_exact() {
        const NAME: &str = "a_lossless_libaom_key_frame_decodes_sample_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let (w, h, frames) = (256usize, 128usize, 2usize);
        let mut raw = Vec::with_capacity(frames * w * h * 3 / 2);
        for f in 0..frames {
            for row in 0..h {
                for col in 0..w {
                    raw.push(((col * 7 + row * 13 + f * 31) % 251) as u8);
                }
            }
            // lane-lossless2: TEXTURED chroma. A flat chroma plane codes one
            // `txb_skip` symbol per 4x4 unit with every context at 0, which
            // is blind to both the unit ORDER and the per-unit contexts
            // (class `gate-blind-to-feature`).
            for row in 0..h / 2 {
                for col in 0..w / 2 {
                    raw.push((((col + f) * 11 + row * 5) % 251) as u8);
                }
            }
            for row in 0..h / 2 {
                for col in 0..w / 2 {
                    raw.push(((col * 3 + (row + f) * 17) % 251) as u8);
                }
            }
        }
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let src = std::env::temp_dir().join(format!("ec-av1-lossless-{}.yuv", std::process::id()));
        std::fs::write(&src, &raw).expect("raw source");
        let encode = |gop: &str| {
            
            crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
                .args(["-s", &format!("{w}x{h}"), "-r", "24", "-i"])
                .arg(&src)
                .args(["-an", "-threads", "1", "-g", gop, "-c:v", "libaom-av1"])
                .args(["-cpu-used", "6", "-b:v", "0", "-crf", "0", "-f", "obu", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run")
        };
        let out = encode("1");
        let _ = std::fs::remove_file(&src);
        if !out.status.success() {
            eprintln!(
                "SKIP {NAME}: ffmpeg has no libaom-av1 encoder ({})",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let stream = out.stdout;
        // Half of the gate: the stream must really carry a lossless frame
        // (class `gate-blind-to-feature`).
        let mut parser = Av1Parser::new();
        let (mut pos, mut lossless) = (0usize, 0usize);
        while pos < stream.len() {
            let obu = parser.parse_obu(&stream[pos..]).expect("libaom OBU parses");
            pos += obu.total_size.max(1);
            let header = match &obu.kind {
                ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => h,
                _ => continue,
            };
            if header.lossless.iter().all(|&l| l) {
                lossless += 1;
            }
        }
        assert_eq!(
            lossless, frames,
            "{NAME} went blind: libaom coded no lossless key frame at -crf 0"
        );
        let ours = decode_stream(&stream).expect("a lossless key-frame stream decodes");
        let refs = ffmpeg_decode_sequence(&stream, w, h, frames);
        assert_eq!(ours.len(), frames, "{NAME}: frame count");
        for (i, (got, want)) in ours.iter().zip(refs.iter()).enumerate() {
            for (plane, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                .iter()
                .enumerate()
            {
                let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    bad, 0,
                    "{NAME}: frame {i} plane {plane}: {bad} samples differ"
                );
                assert_eq!(
                    g.len(),
                    r.len(),
                    "{NAME}: frame {i} plane {plane}: ours has {} samples, ffmpeg's has {} -- a \
                     short plane would zip-truncate the compare above and hide a chroma-span \
                     defect",
                    g.len(),
                    r.len()
                );
            }
        }
    }

    /// lane-lossless2: the refusal `a_lossless_inter_frame_is_refused_by_name`
    /// pinned became an implementation, so its gate became a DECODE-EXACT one
    /// (class `refusal-lifted-without-a-gate`). A lossless INTER frame codes
    /// its chroma plane as 4x4 transform units, plane-major inside each 64x64
    /// mu chunk (libaom `read_tx_size` returns TX_4X4 for every plane under
    /// `xd->lossless[..]`, and `decode_token_recon_block` walks
    /// `for (plane..) for (blk_row..) for (blk_col..)`), which is what
    /// `decode::read_inter_chroma_lossless` reads. The source carries TEXTURED
    /// CHROMA and real motion on purpose: a flat-chroma source codes one
    /// `txb_skip` symbol per unit and is blind to both the unit order and the
    /// per-unit contexts (class `gate-blind-to-feature`).
    #[test]
    fn a_lossless_libaom_inter_frame_decodes_sample_exact() {
        const NAME: &str = "a_lossless_libaom_inter_frame_decodes_sample_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // (w, h, frames, gop, bit depth, 128-superblock, screen content)
        // lane-lossless2: the SCREEN rows carry palette blocks, which is what
        // caught the whole-block palette prediction being handed to a 4x4
        // chroma unit.
        // lane-lossless3: the last row is a screen source with INTER frames --
        // the row that caught both of this lane's defects (a skip 8x8 leaf
        // restoring the previous block's luma level band over the zeros
        // `av1_reset_entropy_context` writes, and a rect sub-8x8 INTRA leaf
        // reading one TX_4X8 unit where a lossless frame codes two TX_4X4).
        let cases: [(usize, usize, usize, &str, u32, bool, bool); 7] = [
            (256, 128, 8, "4", 8, false, false),
            (192, 96, 6, "3", 10, false, false),
            (320, 192, 6, "3", 8, false, false),
            (256, 256, 4, "2", 8, true, false),
            (320, 192, 2, "1", 8, false, true),
            (640, 384, 2, "1", 8, false, true),
            (640, 384, 6, "3", 8, false, true),
        ];
        for &(w, h, frames, gop, depth, sb128, screen) in &cases {
            let tag = format!(
                "{w}x{h} {depth}-bit gop {gop}{}{}",
                if sb128 { " sb128" } else { "" },
                if screen { " screen" } else { "" }
            );
            let mut raw: Vec<u8> = Vec::new();
            let put = |v: u32, raw: &mut Vec<u8>| {
                if depth == 10 {
                    raw.extend_from_slice(&((v * 4) as u16).to_le_bytes());
                } else {
                    raw.push(v as u8);
                }
            };
            // A screen source is a few flat colours in 16x16 tiles plus
            // 1-px text-like rows -- what makes libaom code palette blocks.
            const PAL: [u32; 8] = [16, 60, 110, 145, 180, 210, 235, 128];
            for f in 0..frames {
                for row in 0..h {
                    for col in 0..w {
                        let v = if screen {
                            if row % 7 == 0 && col % 3 == 0 {
                                if (col / 3 + row + f) % 2 == 1 {
                                    16
                                } else {
                                    235
                                }
                            } else {
                                PAL[((col + 4 * f) / 16 + row / 16) % PAL.len()]
                            }
                        } else {
                            (((col + 2 * f) * 7 + row * 13) % 251) as u32
                        };
                        put(v, &mut raw);
                    }
                }
                for plane in 0..2usize {
                    for row in 0..h / 2 {
                        for col in 0..w / 2 {
                            let v = if screen {
                                PAL[((col + 2 * f) / 8 + row / 8 + plane) % PAL.len()]
                            } else if plane == 0 {
                                (((col + f) * 11 + row * 5) % 251) as u32
                            } else {
                                ((col * 3 + (row + f) * 17) % 251) as u32
                            };
                            put(v, &mut raw);
                        }
                    }
                }
            }
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
            let src = std::env::temp_dir().join(format!(
                "ec-av1-lossless2-{}-{w}x{h}-{depth}.yuv",
                std::process::id()
            ));
            std::fs::write(&src, &raw).expect("raw source");
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", pix])
                .args(["-s", &format!("{w}x{h}"), "-r", "24", "-i"])
                .arg(&src)
                .args(["-an", "-threads", "1", "-g", gop, "-c:v", "libaom-av1"])
                .args(["-pix_fmt", pix, "-cpu-used", "6", "-b:v", "0", "-crf", "0"]);
            if sb128 {
                cmd.args(["-aom-params", "sb-size=128"]);
            }
            if screen {
                cmd.args(["-aom-params", "tune-content=screen"]);
            }
            let out = crate::tool_cache::cached_output_io(cmd
                .args(["-f", "obu", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            let _ = std::fs::remove_file(&src);
            if !out.status.success() {
                eprintln!(
                    "SKIP {NAME}: ffmpeg has no libaom-av1 encoder ({})",
                    String::from_utf8_lossy(&out.stderr)
                );
                return;
            }
            let stream = out.stdout;
            // The stream must really be lossless AND really carry inter frames
            // (class `gate-blind-to-feature`).
            let mut parser = Av1Parser::new();
            let (mut pos, mut lossless, mut inters) = (0usize, 0usize, 0usize);
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).expect("libaom OBU parses");
                pos += obu.total_size.max(1);
                let header = match &obu.kind {
                    ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => h,
                    _ => continue,
                };
                if header.lossless.iter().all(|&l| l) {
                    lossless += 1;
                }
                if header.frame_type == FrameType::Inter {
                    inters += 1;
                }
            }
            assert!(
                lossless >= frames,
                "{NAME} [{tag}] went blind: {lossless} lossless frames"
            );
            assert!(
                screen || inters > 0,
                "{NAME} [{tag}] went blind: no inter frame at gop {gop}"
            );
            let ours = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} [{tag}]: a lossless inter stream decodes: {e}"));
            let refs = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, w, h, frames)
            } else {
                ffmpeg_decode_sequence(&stream, w, h, frames)
            };
            assert_eq!(ours.len(), frames, "{NAME} [{tag}]: frame count");
            for (i, (got, want)) in ours.iter().zip(refs.iter()).enumerate() {
                for (plane, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME} [{tag}]: frame {i} plane {plane}: {bad} samples differ"
                    );
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME} [{tag}]: frame {i} plane {plane}: ours has {} samples, ffmpeg's has \
                         {} -- a short plane would zip-truncate the compare above and hide a \
                         chroma-span defect",
                        g.len(),
                        r.len()
                    );
                }
            }
        }
    }

    /// lane-av1-llband: the pinned 4:4:4 LOSSLESS key frame
    /// (`fixtures/ll444-lossless-key.obu`, the `ll444` recipe's frame 0 --
    /// `testsrc2 128x96` yuv444p cut to its key frame) decodes sample-exact,
    /// and its HORZ/VERT strips really walked their chroma as 4x4 units at
    /// subsampling 0/0 (one unit per mi cell). That unit walk used the 4:2:0
    /// span (8 luma px per 4x4 chroma unit) unconditionally, so every unit
    /// past the first column/row read its `txb_skip` context two mi cells
    /// over and stamped a 2-cell smear -- the 178-byte key-frame miss, first
    /// divergence at the 16x8 leaf mi (16,24)'s U bc=2 unit (oracle
    /// `txb_skip` base 0 off `above[26]`, ours base 1 off the polluted
    /// `above[29]`). The counter assertion is the non-vacuity half (class
    /// `gate-blind-to-feature`): a stream that never reaches the 4:4:4 unit
    /// walk must not pass.
    #[test]
    fn a_real_aomenc_lossless_444_key_frame_decodes_sample_exact() {
        const NAME: &str = "a_real_aomenc_lossless_444_key_frame_decodes_sample_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // lane-av1pins: committed at crates/ec-av1/fixtures/ll444-lossless-key.obu
        // (7845 bytes; sha256
        // f496ef0a1171c2bc66d411cc60db05db547dcdb49cd834d4e0543de9db441374),
        // re-encoded 2026-09-29 from the recipe this very SKIP line records --
        // `testsrc2 128x96` yuv444p 6 frames, aomenc --profile=1 --lossless=1
        // --enable-palette=0 --enable-intrabc=0 --bit-depth=8 --cpu-used=0
        // --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100, --limit=1
        // so the key frame's OBUs are the whole stream (no OBU surgery needed).
        //
        // lane-av1skipfix: reading the pin from the GITIGNORED root
        // `fixtures/` (`pin_dir()`) is what made this gate report green having
        // decoded nothing -- a fresh clone, and every linked worktree, can
        // never have that directory. The pin is now COMMITTED under the crate
        // (the home the other pins use) and read through `CARGO_MANIFEST_DIR`,
        // so a missing file is a REPO DEFECT and fails unconditionally: there
        // is no environment left to escape, which is strictly stronger than
        // the `EC_REQUIRE_FIXTURES` / `EC_AV1_REQUIRE_AOMENC` opt-in hard
        // fail this arm used to carry.
        let pin = crate_pin("ll444-lossless-key.obu");
        let stream = std::fs::read(&pin).unwrap_or_else(|e| {
            panic!(
                "{NAME}: the pinned 4:4:4 lossless key frame is missing at {} ({e}). A \
                 COMMITTED pin that is absent is a repo defect and this gate would prove \
                 nothing. No script generates it -- regenerate it with `testsrc2 128x96` \
                 yuv444p 6 frames, aomenc --profile=1 --lossless=1 --enable-palette=0 \
                 --enable-intrabc=0 --bit-depth=8 --cpu-used=0 --threads=1 --row-mt=0 \
                 --lag-in-frames=0 --kf-max-dist=100 --limit=1, and commit the result.",
                pin.display()
            )
        });
        // Blindness guard: the pinned fixture must really be a lossless frame.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut lossless = false;
        while pos < stream.len() {
            let obu = parser.parse_obu(&stream[pos..]).expect("aomenc OBU parses");
            pos += obu.total_size.max(1);
            if let ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) = obu.kind {
                lossless = h.lossless.iter().all(|&l| l);
            }
        }
        assert!(
            lossless,
            "{NAME} went blind: the pinned frame is not lossless"
        );
        let before = crate::decode::rect_split_lossless_chroma444_hits();
        let ours = decode_stream(&stream).expect("a 444 lossless key-frame stream decodes");
        assert_eq!(ours.len(), 1, "{NAME}: frame count");
        assert!(
            crate::decode::rect_split_lossless_chroma444_hits() > before,
            "{NAME} went blind: no lossless HORZ/VERT strip walked its chroma \
             as 4x4 units at subsampling 0/0"
        );
        let refs = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv444p", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.as_mut().expect("stdin").write_all(&stream)?;
                child.wait_with_output()
            })
            .expect("ffmpeg 444 decode failed");
        assert!(
            refs.status.success(),
            "{}",
            String::from_utf8_lossy(&refs.stderr)
        );
        let frame = &ours[0];
        let planes = [
            (&frame.y, frame.width * frame.height),
            (&frame.u, frame.width * frame.height),
            (&frame.v, frame.width * frame.height),
        ];
        let mut off = 0usize;
        for (plane, (g, len)) in planes.iter().enumerate() {
            let raw = &refs.stdout[off..off + len];
            let bad = g
                .iter()
                .zip(raw.iter())
                .filter(|&(&a, &b)| a != u16::from(b))
                .count();
            assert_eq!(bad, 0, "{NAME}: plane {plane}: {bad} samples differ");
            off += len;
        }
    }

    /// lane-av1-llinter: the firing gate for the lossless 4:4:4 INTER chroma
    /// fix. The fixture is the report's dodge2 stream pinned into `fixtures/`:
    /// aomenc profile 1 over testsrc2 128x96 yuv444p, `--lossless=1`,
    /// `--sb-size=64 --min-partition-size=64 --max-partition-size=64`, 6
    /// frames; 37658 bytes, sha256
    /// `5a070fa351a3f2109de855234f2b3d0a872970f3e7f658db21f65c12eeb135b4`.
    /// The firing counter is the EXISTING `decode::chroma_split_tx_hits` (one
    /// bump per lossless chroma TX unit read): with min=max partition 64 the
    /// roots stay whole 64x64 inter blocks -- no 8x8 leaf exists in this
    /// stream -- so every chroma unit goes through the big-block ss-aware
    /// lossless route this lane fixed (fixes 2-5). A 4:2:0 lossless stream
    /// fires the same counter, but the fixture bytes are pinned, so the gate
    /// cannot run on anything else; the pre-fix 4:2:0-hardcoded math corrupts
    /// at 4:4:4, so the pixel-exact compares are what prove the ss-aware path
    /// (class `gate-blind-to-feature`). Every frame must come out pixel-exact
    /// through BOTH the oracle `aomdec --rawvideo` and ffmpeg. The
    /// default-partition ll444 stream is deliberately NOT claimed here: its
    /// key frame is still the intra lane's named sub8-chroma stop
    /// (lanes/av1llintra.report.md).
    #[test]
    fn a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact() {
        const NAME: &str = "a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact";
        const FIXTURE_LEN: usize = 37658;
        const FIXTURE_FNV: u64 = 0xf68d_c26f_a14d_283c;
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 6;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_minp64_inter.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        let before = crate::decode::chroma_split_tx_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let hits = crate::decode::chroma_split_tx_hits();
        assert!(
            hits > before,
            "{NAME}: gate is vacuous -- no lossless chroma TX unit was read \
             (delta {before} -> {hits})"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }

        // The oracle aomdec, rawvideo out: FRAMES concatenated yuv444p frames.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-ll444-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            for (i, f) in frames.iter().enumerate() {
                let base = i * W * H * 3;
                for ((plane, off), p) in [(&f.y, 0usize), (&f.u, W * H), (&f.v, 2 * W * H)]
                    .into_iter()
                    .zip(["y", "u", "v"])
                {
                    let want = &ref_raw[base + off..base + off + W * H];
                    let bad = plane
                        .iter()
                        .zip(want)
                        .filter(|&(&a, &b)| a as u8 != b)
                        .count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: aomdec frame {i} plane {p}: {bad} samples differ"
                    );
                    assert_eq!(
                        plane.len(),
                        want.len(),
                        "{NAME}: aomdec frame {i} plane {p}: ours has {} samples, the oracle's \
                         has {} -- a short plane would zip-truncate the compare above and hide \
                         a chroma-span defect",
                        plane.len(),
                        want.len()
                    );
                }
            }
        }

        // ffmpeg's decoder: same frames, full-resolution chroma.
        if have_ffmpeg() {
            let refs = ffmpeg_decode_sequence_444(&stream, W, H, FRAMES);
            for (i, (got, want)) in frames.iter().zip(refs.iter()).enumerate() {
                for (p, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: ffmpeg frame {i} plane {p}: {bad} samples differ"
                    );
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME}: ffmpeg frame {i} plane {p}: ours has {} samples, ffmpeg's has {} \
                         -- a short plane would zip-truncate the compare above and hide a \
                         chroma-span defect",
                        g.len(),
                        r.len()
                    );
                }
            }
        } else {
            eprintln!("SKIP {NAME} ffmpeg arm: no ffmpeg");
        }
    }

    /// lane-av1h5: the 128-ROOT lossless 4:4:4 pin, and the control arm
    /// that proves the 128 root is what it needs.
    ///
    /// Witness: testsrc2 128x96 yuv444p10le, aomenc `--profile=1 --lossless=1
    /// --enable-palette=0 --enable-intrabc=0 --cpu-used=0`, 6 frames; 63429
    /// bytes, sha256 `bbffb979d20d581deb271aac75b7ee779dde57c024524ed6b1389356f55ca0fa`.
    /// Control: the same recipe at `--max-partition-size=64` (61494 bytes,
    /// sha256 `4cadce41e1f8967567b32f5cf8abaa46ffdf4fd6d4afe784c8d99a7f6bf1ae4c`),
    /// which never reaches a 128 root.
    ///
    /// libaom's `decode_token_recon_block` (`av1/decoder/decodeframe.c:972`)
    /// walks 64x64 "mu" chunks with the PLANE loop inside the chunk loop, on a
    /// chunk grid of `mi_size_wide[BLOCK_64X64]` = 16 mi per axis. The var-tx
    /// leaf list is a BLOCK-wide raster (32 leaves across at 128), so a chunk's
    /// members are not contiguous: leaves 0..15 are chunk (0,0) row 0, but
    /// chunk (0,0) rows 1..15 are leaves 32..47, 64..79, ... A chunk test on
    /// the given order closed the chunk every SIXTEEN leaves and coded each
    /// chunk's chroma sixteen times.
    ///
    /// The three assertions below are all EXACT and all stay true when the
    /// stream becomes byte-exact. None of them is a floor on wrong output:
    /// a no-op [`crate::decode::mu_chunk_order`] permutation reads 0 on
    /// `mu_chunk_order_hits`, so the first cannot be satisfied by a gate that
    /// merely ran; the walk and unit totals are libaom's own per-chunk counts;
    /// the control arm is a straight exactness assertion.
    #[test]
    fn a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major() {
        const NAME: &str = "a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major";
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 6;
        /// 5 inter frames, one unsplit 128 root each; frame 0 splits.
        const ROOTS: usize = 5;
        /// libaom's chunk grid on a 128x128 root: 2x2 chunks per root.
        const CHUNKS_PER_ROOT: usize = 4;
        /// Chroma TX_4X4 units per inter frame: 768 per plane over 128x96.
        const UNITS_PER_FRAME: usize = 1536;

        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_128root_lossless.obu");
        let stream = read_pin(&obu, 63429, 0xf07d_47fc_fd51_2658, NAME);
        let _guard = lock_gate_counters();
        let (o0, w0, u0) = (
            crate::decode::mu_chunk_order_hits(),
            crate::decode::mu_chunk_walks(),
            crate::decode::mu_chunk_walk_units(),
        );
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let (o1, w1, u1) = (
            crate::decode::mu_chunk_order_hits(),
            crate::decode::mu_chunk_walks(),
            crate::decode::mu_chunk_walk_units(),
        );
        // (1) the reorder PERMUTED each root's walk -- a no-op reads 0.
        assert_eq!(
            o1 - o0,
            ROOTS,
            "{NAME}: the chunk-major sort permuted {}/{} of the 128 roots -- a no-op \
             ordering must read 0 here, so this is what makes the gate non-vacuous",
            o1 - o0,
            ROOTS
        );
        // (2) libaom's own per-chunk counts: 4 chroma walks per 128 root, and
        // the 1536 chroma units an inter frame codes at 128x96 4:4:4.
        assert_eq!(
            w1 - w0,
            ROOTS * CHUNKS_PER_ROOT,
            "{NAME}: {}/{} chunk-major chroma walks; the 16x redundancy (64 per \
             frame instead of 4) would read {}",
            w1 - w0,
            ROOTS * CHUNKS_PER_ROOT,
            ROOTS * CHUNKS_PER_ROOT * 16
        );
        assert_eq!(
            u1 - u0,
            ROOTS * UNITS_PER_FRAME,
            "{NAME}: {}/{} chroma units over the 128 roots; the oracle codes {} \
             per inter frame and our pre-fix walk read 16x that",
            u1 - u0,
            ROOTS * UNITS_PER_FRAME,
            UNITS_PER_FRAME
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }

        // (3) the control arm: no 128 root, and byte-exact through the oracle.
        let ctrl = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_minp64_128root_control.obu");
        let ctrl_stream = read_pin(
            &ctrl,
            61494,
            0xf6f5_a3cb_efc7_eb10,
            &format!("{NAME} control"),
        );
        let _ = decode_stream(&ctrl_stream)
            .unwrap_or_else(|e| panic!("{NAME}: the control arm no longer decodes cleanly: {e}"));
        if aomdec_available(NAME) {
            assert_rawvideo_matches(&ctrl, &ctrl_stream, NAME, FRAMES, 10);
        }
    }

    /// lane-av1lm444loss: a LOSSLESS block's transform-unit grid is clipped at
    /// the FRAME edge, exactly as libaom's `max_block_high`/`max_block_wide`
    /// clip it (`av1_common_int.h:1565`, `:1579` fold `mb_to_bottom_edge` /
    /// `mb_to_right_edge` into `block_size_high/wide` before `>>
    /// MI_SIZE_LOG2`), so a block hanging off the bottom/right codes only the
    /// units inside the frame. `decode_token_recon_block` then bounds every
    /// mu chunk with `unit_height = ROUND_POWER_OF_TWO(AOMMIN(mu_blocks_high
    /// + row, max_blocks_high), ss_y)`, so the overflow units are never read
    ///   there either.
    ///
    /// Fixture is the sibling [`a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major`]
    /// pin -- 128x96 yuv444p 4:4:4 lossless, 6 frames, 63429 bytes -- because
    /// 96 is NOT a multiple of the 128 superblock: its five inter frames each
    /// carry one unsplit 128 root that overhangs the bottom by 32 px. That is
    /// the whole reason this stream is the witness. The frame is 24 mi tall,
    /// so `max_blocks_high >> 2` is 24 transform rows where the block's own
    /// `side / MI` is 32.
    ///
    /// The three assertions are all non-vacuous and all EXACT:
    ///
    /// 1. `lossless_edge_clipped()` fired -- 5, one per inter frame's 128
    ///    root. It is 0 on any frame whose height and width are multiples of
    ///    the superblock, so a stream that merely decodes cannot satisfy it.
    /// 2. `lossless_edge_clip_units()` is 1280 = 5 x 256: the 8 transform rows
    ///    x 32 columns the unclipped walk read past the frame bottom, per
    ///    inter frame. The pre-fix walk read 2560 units per inter frame
    ///    against the oracle's 2304, and this is that 256.
    /// 3. every sample of every frame matches the oracle `aomdec
    ///    --rawvideo` in decode order -- all six frames, hidden alt-ref
    ///    included, which the pre-fix tree failed from frame 1 on
    ///    (5596 wrong samples there, growing to 33561 by frame 5).
    ///
    /// The control arm pins the other direction of the invariant: the
    /// `ll444_minp64_128root_control` stream has no overhanging root, so its
    /// clip counters must both read 0 -- the clip cannot fire on a block that
    /// fits, which is what keeps every other lossless pin byte-identical.
    #[test]
    fn a_lossless_block_clips_its_transform_grid_at_the_frame_edge() {
        const NAME: &str = "a_lossless_block_clips_its_transform_grid_at_the_frame_edge";
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 6;
        /// 5 inter frames, one unsplit 128 root each; frame 0 splits.
        const ROOTS: usize = 5;
        /// `max_blocks_high >> 2` = 24 rows (96 px / 4), against the block's
        /// own `side / MI` = 32 -- so 8 rows x 32 columns = 256 units per root
        /// were read past the frame bottom before the clip.
        const CLIP_UNITS_PER_ROOT: usize = 256;

        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_128root_lossless.obu");
        let stream = read_pin(&obu, 63429, 0xf07d_47fc_fd51_2658, NAME);
        let _guard = lock_gate_counters();
        let (c0, u0) = (
            crate::decode::lossless_edge_clipped(),
            crate::decode::lossless_edge_clip_units(),
        );
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let (c1, u1) = (
            crate::decode::lossless_edge_clipped(),
            crate::decode::lossless_edge_clip_units(),
        );
        assert_eq!(
            c1 - c0,
            ROOTS,
            "{NAME}: the frame-edge clip fired on {}/{} of the 128 roots -- it must be 0 \
             on a frame that fits, so this is what makes the gate non-vacuous",
            c1 - c0,
            ROOTS
        );
        assert_eq!(
            u1 - u0,
            ROOTS * CLIP_UNITS_PER_ROOT,
            "{NAME}: the clip removed {}/{} transform units; the unclipped walk read {} \
             per inter frame past the 96 px frame bottom (8 rows x 32 columns)",
            u1 - u0,
            ROOTS * CLIP_UNITS_PER_ROOT,
            CLIP_UNITS_PER_ROOT
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }
        if aomdec_available(NAME) {
            assert_rawvideo_matches(&obu, &stream, NAME, FRAMES, 10);
        }

        // Control: a stream whose roots all fit must not clip at all.
        let ctrl = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_minp64_128root_control.obu");
        let ctrl_stream = read_pin(
            &ctrl,
            61494,
            0xf6f5_a3cb_efc7_eb10,
            &format!("{NAME} control"),
        );
        let (c2, u2) = (
            crate::decode::lossless_edge_clipped(),
            crate::decode::lossless_edge_clip_units(),
        );
        let _ = decode_stream(&ctrl_stream)
            .unwrap_or_else(|e| panic!("{NAME}: the control arm no longer decodes cleanly: {e}"));
        assert_eq!(
            crate::decode::lossless_edge_clipped() - c2,
            0,
            "{NAME} control: the clip fired on a stream whose blocks all fit -- the clip \
             must be a no-op there, which is what keeps every other lossless pin \
             byte-identical"
        );
        assert_eq!(
            crate::decode::lossless_edge_clip_units() - u2,
            0,
            "{NAME} control: the clip removed units from a stream whose blocks all fit"
        );
    }

    /// lane-av1lm444loss-corr: the MI STEP at which `decode_intrabc_rect`'s
    /// lossless chroma replay walks the plane block.
    ///
    /// `lane-whtshape` (`f541d062`, merged `afa13bcf`) gave this route the
    /// per-TX_4X4 lossless chroma walk and the tail replay that re-stamps
    /// each unit's own coefficient context over the whole-block record. The
    /// replay stepped by `4 >> ss_y(fctx)` mi cells, which is 4:2:0-only
    /// arithmetic: at ss (1,1) it reads 2 and the ss-aware form
    /// `(4 << ss_y) / MI` also reads 2, so 4:2:0 agrees by construction; at
    /// ss (0,0) it reads 4 mi, i.e. 16 luma px per 4x4 chroma unit, where the
    /// unit covers `4 << 0` = 4 luma px = ONE mi. Every replayed unit was
    /// therefore stamped at 4x its own mi origin on a 4:4:4 lossless frame,
    /// writing over cells that belong to other blocks.
    ///
    /// This gate is the WIRNESS stream, not a vacuous one. The 4:2:0
    /// 4x4-identity is stated in the step's own comment and measured by the
    /// `4:2:0` arm of the lane's invariant battery, not asserted here --
    /// a 4:2:0 stream cannot fail this gate, by construction, and pretending
    /// otherwise would be a floor on wrong output.
    ///
    /// Fixture `ll444_intrabc_rect_l2.obu`, 121844 bytes, sha256
    /// `f0de080edce35d34cdd628b71f63dc452f946e8328352b69d3c1da090a54ec67`:
    /// `testsrc2 512x128 yuv444p`, aomenc `--lossless=1 --cpu-used=2
    /// --lag-in-frames=0 --kf-max-dist=100 --limit=6`, 6 frames. On main's
    /// step this stream is 10604 samples wrong in the KEY frame, first at
    /// byte 33216 (Y(64,448)) -- the corrupted chroma context desyncs the
    /// following luma, which is why the first divergence is luma at all.
    ///
    /// Assertions:
    ///
    /// 1. `(subsampling_x, subsampling_y, bit_depth) == (0, 0, 8)` from the
    ///    stream's OWN parsed sequence header. At (1,1) the corrected step
    ///    and the old one are the same number, so without this every
    ///    assertion below would pass on a stream that cannot fail.
    /// 2. `intrabc_rect_lossless_chroma4_hits()` (lane-whtshape's own
    ///    counter) is nonzero -- the replayed arm really ran, so the
    ///    corrected expression was evaluated.
    /// 3. the U and V plane extents are luma-sized, which is the geometry
    ///    the step indexes into.
    /// 4. `decode_all_frames_vs_oracle`: decode-order frame-count parity,
    ///    then byte-exactness on every frame.
    ///
    /// Control: the sibling 128x96 4:4:4 lossless pin, whose blocks are
    /// whole and square, must not take the lossless chroma walk at all --
    /// its chroma plane block IS one TX_4X4, so there is no step to replay.
    #[test]
    fn a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint() {
        const NAME: &str = "a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint";
        const W: usize = 512;
        const H: usize = 128;
        const FRAMES: usize = 6;

        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_intrabc_rect_l2.obu");
        let stream = read_pin(&obu, 121844, 0x38bf_968e_979c_7a0a, NAME);
        assert_eq!(
            subsampled_header_shape(&stream, NAME),
            (0, 0, 8),
            "{NAME}: the stream is not 8-bit 4:4:4 -- at ss (1,1) the corrected \
             step and the 4:2:0-only one are the SAME number (2 mi), so this gate \
             cannot fail on a 4:2:0 stream and would prove nothing there"
        );
        let _guard = lock_gate_counters();
        crate::decode::reset_intrabc_rect_lossless_chroma4_hits();
        let h0 = crate::decode::intrabc_rect_lossless_chroma4_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let walked = crate::decode::intrabc_rect_lossless_chroma4_hits() - h0;
        assert!(
            walked > 0,
            "{NAME}: lane-whtshape's lossless per-TX_4X4 chroma walk never ran, so the \
             replayed MI step -- the thing this gate exists for -- was never evaluated"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
            assert_eq!(
                f.u.len(),
                W * H,
                "{NAME}: U extent must be luma-sized at 4:4:4 -- the replay steps mi \
                 cells through this grid"
            );
            assert_eq!(
                f.v.len(),
                W * H,
                "{NAME}: V extent must be luma-sized at 4:4:4"
            );
        }
        // lane-3c-a: the oracle-presence scan (wave 3b, 7a865ee7) rejects a
        // bare `aomdec_path().is_file()` outside the probe that owns it -- this
        // gate would otherwise report green under EC_AV1_REQUIRE_AOMENC=1 with
        // the pixel compare never run. `aomdec_available` carries both the
        // env-escape assertion and the SKIP line, so the else-arm is dropped.
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(
                decoded, FRAMES,
                "{NAME}: decoded {decoded} frames in decode order, expected {FRAMES}"
            );
            eprintln!(
                "{NAME}: {decoded} frames byte-exact vs aomdec ({hidden} hidden), \
                 {walked} lossless chroma replays"
            );
        }

        // Control: whole square blocks have no multi-unit plane block to step.
        let ctrl = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_128root_lossless.obu");
        let ctrl_stream = read_pin(
            &ctrl,
            63429,
            0xf07d_47fc_fd51_2658,
            &format!("{NAME} control"),
        );
        crate::decode::reset_intrabc_rect_lossless_chroma4_hits();
        let _ = decode_stream(&ctrl_stream)
            .unwrap_or_else(|e| panic!("{NAME}: the control arm no longer decodes cleanly: {e}"));
        assert_eq!(
            crate::decode::intrabc_rect_lossless_chroma4_hits(),
            0,
            "{NAME} control: the lossless chroma walk ran on a stream of whole square \
             blocks -- there is no multi-unit chroma plane block to replay, so the \
             corrected step is not exercised there at all"
        );
    }

    /// lane-av1llpredgate2: the debug-exact pin for the ss-aware
    /// `obmc_skip_chroma_above` decision (lane-av1-llpred2). Fixture is the
    /// llpred/llpred2 reports' stream pinned into `fixtures/`: testsrc2
    /// 128x96 yuv444p, aomenc `--profile=1 --lossless=1 --enable-palette=0
    /// --enable-intrabc=0 --min-partition-size=8 --max-partition-size=64`,
    /// 6 frames; 34037 bytes, sha256
    /// `04fb6d38de5382e647bfb70b6a17802f1fccc5502e6698c0394167891f2b27dc`.
    ///
    /// This is a DEBUG-build path by construction: the old ratio-based ss
    /// derivation read `write_h >> trailing_zeros(0)` = `>> 64` on this
    /// stream's 4:4:4 OBMC rect strips, a panic ONLY under
    /// debug_assertions (release wraps and was already byte-exact), so
    /// `cargo test`'s default dev profile is the profile where the defect
    /// class reproduces and where the parent's byte-exact proof was
    /// measured.
    ///
    /// No skip-arm counter gate is possible on this stream, measured:
    /// OBMC runs 77 times with 20 rect leaves, but
    /// `obmc_skip_chroma_above` returns true 0 times -- at 4:4:4 the skip
    /// needs a 4x4/8x4/4x8 PLANE block and min-partition-8 never codes one.
    /// So per the lane ticket the proof splits in two: the decision TABLE
    /// (4:2:0 rect strips still skip, 4:4:4 shifts never leave `usize`)
    /// stays pinned by `decode::obmc_skip_chroma_tests::
    /// rect_strips_skip_above_chroma_at_420_and_shift_never_overflows`,
    /// and THIS stream gate pins the end-to-end behaviour -- every sample
    /// of every frame must match the oracle aomdec exactly, so a returning
    /// chroma diff (the lane-av1-llpred ss-blind re-blend class
    /// `obmc-ss-blind-chroma-above`) or the debug shift panic goes red
    /// here by name.
    #[test]
    fn a_lossless_444_min_partition8_inter_stream_decodes_sample_exact() {
        const NAME: &str = "a_lossless_444_min_partition8_inter_stream_decodes_sample_exact";
        const FIXTURE_LEN: usize = 34037;
        const FIXTURE_FNV: u64 = 0x2c8f_4a2c_c025_52da;
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 6;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_minp8_inter.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }

        // The oracle aomdec, rawvideo out: FRAMES concatenated yuv444p frames.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-llminp8-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            for (i, f) in frames.iter().enumerate() {
                let base = i * W * H * 3;
                for ((plane, off), p) in [(&f.y, 0usize), (&f.u, W * H), (&f.v, 2 * W * H)]
                    .into_iter()
                    .zip(["y", "u", "v"])
                {
                    let want = &ref_raw[base + off..base + off + W * H];
                    let bad = plane
                        .iter()
                        .zip(want)
                        .filter(|&(&a, &b)| a as u8 != b)
                        .count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: aomdec frame {i} plane {p}: {bad} samples differ \
                         (class obmc-ss-blind-chroma-above)"
                    );
                    assert_eq!(
                        plane.len(),
                        want.len(),
                        "{NAME}: aomdec frame {i} plane {p}: ours has {} samples, the oracle's \
                         has {} -- a short plane would zip-truncate the compare above and hide \
                         a chroma-span defect",
                        plane.len(),
                        want.len()
                    );
                }
            }
        }

        // ffmpeg's decoder: same frames, full-resolution chroma.
        if have_ffmpeg() {
            let refs = ffmpeg_decode_sequence_444(&stream, W, H, FRAMES);
            for (i, (got, want)) in frames.iter().zip(refs.iter()).enumerate() {
                for (p, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: ffmpeg frame {i} plane {p}: {bad} samples differ"
                    );
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME}: ffmpeg frame {i} plane {p}: ours has {} samples, ffmpeg's has {} \
                         -- a short plane would zip-truncate the compare above and hide a \
                         chroma-span defect",
                        g.len(),
                        r.len()
                    );
                }
            }
        } else {
            eprintln!("SKIP {NAME} ffmpeg arm: no ffmpeg");
        }
    }

    /// lane-av1444altref: the 4:4:4 LOSSLESS + altref chroma residual this
    /// lane closed -- 317 wrong bytes, Y exact, on
    /// `fixtures/ll444_c_altref_leaf8_warp.obu` (512220 bytes, sha256
    /// `999a41d88a092840456d5e9de83590ab819a8a14686a694889e63c2258ff2bb8`;
    /// the cell is committed here byte for byte, never re-encoded, and its
    /// `aomenc` recipe is still unrecorded -- see
    /// `lanes/av1444altref.report.md`).
    ///
    /// The defect: `decode_inter_block8`'s COMPOUND arm warped its LUMA
    /// intermediates (`warp_affine_compound`) and left BOTH CHROMA arms
    /// translational. libaom warps per PLANE
    /// (`build_inter_predictors_8x8_and_bigger`,
    /// reconinter_template.inc:230, runs `av1_init_warp_params` +
    /// `av1_warp_plane` inside the plane loop with that plane's
    /// subsampling and origin). A GLOBALMV (affine) 8x8 leaf's MV is the
    /// affine's local derivative, so a translation agrees with the warp at
    /// the block's top-left corner and drifts apart toward its bottom-right
    /// -- the measured signature, a bottom-right-biased +/-1 gradient in a
    /// few 8x8 chroma cells of hidden altref pictures while luma stayed exact.
    ///
    /// The assertion is BYTE-EXACTNESS on all three planes of all 16
    /// displayed frames against the oracle aomdec, never a route counter. The
    /// `flip` arm is the oracle-flip control: it rotates ONE byte of the
    /// oracle's own rawvideo output and requires the same comparator to
    /// report exactly that one sample, so the zeros above are a live
    /// comparison and not an empty or short zip (class
    /// `stale-output-faked-measurement`). `compound_warp_hits_8`'s delta is
    /// the supplementary reachability witness: it proves the 8x8 compound
    /// warp-leaf arm -- the body the fix lives in -- actually ran on this
    /// cell (class `gate-blind-to-feature`).
    #[test]
    fn a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact() {
        const NAME: &str = "a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact";
        const FIXTURE_LEN: usize = 512_220;
        const FIXTURE_FNV: u64 = 0xbe0a_569f_3d13_1026;
        const W: usize = 320;
        const H: usize = 240;
        const FRAMES: usize = 16;
        let stream =
            std::fs::read(crate_pin("ll444_c_altref_leaf8_warp.obu")).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned fixture ll444_c_altref_leaf8_warp.obu is missing ({e}) -- \
                     the gate cannot run, which is a failure, not a skip"
                )
            });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            8,
            "{NAME}: this cell is 8-bit"
        );

        let _guard = lock_gate_counters();
        // The pin is 18 decode-order pictures for 16 displayed frames (two
        // hidden altrefs); the geometry assert is on the DISPLAYED frames,
        // which is what aomdec's --rawvideo emits and what the comparator
        // packs.
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        assert_eq!(frames.len(), FRAMES, "{NAME}: displayed frame count");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: frame {i} dimensions");
        }

        if !aomdec_available(NAME) {
            return;
        }
        // ---- arm 1: the real oracle, every plane byte-identical ----------
        let (wy, wu, wv, n, exact) =
            count_rawvideo_diffs(&stream, NAME, None).unwrap_or_else(|| {
                panic!(
                    "{NAME}: the two sides are not the same size -- a zip-truncated compare \
                     would report zero and hide the whole chroma span"
                )
            });
        assert_eq!(
            (wy, wu, wv),
            (0, 0, 0),
            "{NAME}: {wy} Y / {wu} U / {wv} V samples differ from the oracle \
             (class altref-444-leaf8-chroma-unwarped)"
        );
        assert_eq!(
            (n, exact),
            (FRAMES, FRAMES),
            "{NAME}: every displayed frame must be byte-identical to the oracle's"
        );

        // ---- arm 2: the ORACLE-FLIP CONTROL -----------------------------
        // One byte of the oracle's own output (the first U sample of frame 0),
        // rotated in the comparator. The count MUST move, by exactly one, in
        // the U plane only: a comparator that ignored the reference, or zipped
        // a truncated buffer, would still report zero here.
        let (cy, cu, cv, cn, _) = count_rawvideo_diffs(&stream, NAME, Some(W * H))
            .unwrap_or_else(|| panic!("{NAME}: the control arm could not compare at all"));
        assert_eq!(
            (cy, cu, cv),
            (0, 1, 0),
            "{NAME}: the oracle-flip control did not move the count -- this gate's zero is \
             vacuous, not exact"
        );
        assert_eq!(
            cn, FRAMES,
            "{NAME}: the control arm must still see every frame"
        );

        // ---- supplementary reachability witness -------------------------
        let before8 = crate::decode::compound_warp_hits_8();
        let _ = decode_stream(&stream).expect("the reachability witness re-decodes the pin");
        assert!(
            crate::decode::compound_warp_hits_8() > before8,
            "{NAME}: no 8x8 compound warp leaf ran on this cell, so the fixed body was never \
             reached and the byte compare above proved nothing (class gate-blind-to-feature)"
        );
    }

    /// lane-av1-444sb: the byte-exact pin for the ss-aware 128-rect chroma
    /// unit walk (`decode_block_128rect`). The parent walk hardcoded the
    /// 4:2:0 mu-chunk span -- one 32x32 chroma unit per 64x64 chunk, chunk
    /// offsets `*32`, clip step `*8` -- so at 4:4:4 a 128-root HORZ/VERT
    /// intra block coded HALF its chroma units, left the remaining units'
    /// coefficients unread and desynced the tile from the first such block
    /// on; every superblock after SB(0,0) carried damage and the chroma
    /// first-diffs sat on the 64/128 grid.
    ///
    /// `crates/ec-av1/fixtures/444_sb128rect_lr_witness.obu`, 794 bytes,
    /// sha256
    /// `27825e14633a4b23e37d48a2acdc7f5d814ca4fadd0b8797d72a1ffb34061e80`:
    /// `ffmpeg -i bands444.y4m -frames:v 3 -vf scale=192:160 -pix_fmt
    /// yuv444p -f yuv4mpegpipe` piped to `aomenc --codec=av1 --profile=1
    /// --passes=1 --end-usage=q --cq-level=32 --cpu-used=0 --threads=1
    /// --row-mt=0 --sb-size=128 --lag-in-frames=0 --auto-alt-ref=0
    /// --kf-max-dist=1000 --i444 --enable-restoration=1 --obu --limit=3`.
    /// Real content is required: the synthetic lavfi sources this lane swept
    /// (testsrc2/testsrc/smptebars/mandelbrot/gradients) all decode frame 0
    /// exact because they never force a 128-root rect; bands does (kf
    /// PARTITION_VERT 64x128 at mi(0,32) plus an intra-in-inter 64x128).
    ///
    /// Non-vacuity via the `decode::intra_sb128_hits` delta: a stream that
    /// stopped coding 128-axis intra blocks reads 0 and the gate fails
    /// instead of passing over a decode that never reached the fixed body
    /// (class `gate-blind-to-feature`). The oracle arms pin every sample of
    /// every frame against both aomdec and ffmpeg, so a returning chroma
    /// span regression goes red here by name.
    #[test]
    fn a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact() {
        const NAME: &str = "a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact";
        const FIXTURE_LEN: usize = 794;
        const FIXTURE_FNV: u64 = 0xf5a7_27f4_3fa7_abc3;
        const W: usize = 192;
        const H: usize = 160;
        const FRAMES: usize = 3;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_sb128rect_lr_witness.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        let before = crate::decode::intra_sb128_hits();
        let before_replay = crate::decode::sb128rect_chroma_replay_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let (horz, vert, _) = crate::decode::intra_sb128_hits();
        assert!(
            horz - before.0 + (vert - before.1) > 0,
            "{NAME}: this stream coded NO 128-axis intra block -- the fixed body \
             never ran (class gate-blind-to-feature)"
        );
        // lane-av1-128rectspan: the block-level `record_split_luma_rect`
        // above clobbers the per-unit chroma contexts, so every coded unit
        // replays its own state after it -- the arm the span fix sits on.
        // The delta keeps THAT arm non-vacuous (a 128-root rect stream that
        // stopped replaying reads 0), and the mismatch half is the 4:2:2
        // signature: this stream is 4:4:4 (ss 0,0), where the unit's luma
        // width and height are equal, so a replay that stamped one axis with
        // decoded now, so this counter no longer "names the day 4:2:2 is
        // admitted" -- it fires there. **DO NOT copy this zero assert onto a
        // 4:2:2 stream**: measured on this branch's tip, the two
        // `422_intrabc_sb128_strip*` pins each read `mismatch = 9` while
        // being byte-exact against ffmpeg, because the mismatch half counts
        // the replay's own designed 4:2:2 case rather than a defect. What
        // stops the 8-row spill below the block from corrupting the next
        // block's `txb_skip` is the per-axis span itself, and the 4:2:2
        // exactness gates (`the_pinned_422_intrabc_sb128_strip_witnesses_
        // decode_pixel_exact`) are what hold it -- not this counter.
        let after_replay = crate::decode::sb128rect_chroma_replay_hits();
        assert!(
            after_replay.0 > before_replay.0,
            "{NAME}: no 128-root rect block replayed a chroma unit's own mi state -- the \
             replay arm never ran (class gate-blind-to-feature)"
        );
        assert_eq!(
            after_replay.1,
            before_replay.1,
            "{NAME}: the tail replay stamped {} unit(s) over a height that was not the \
             unit's own luma height (4:2:2 spans leaked into an admitted stream)",
            after_replay.1 - before_replay.1
        );
        eprintln!(
            "{NAME}: replayed {} chroma unit(s), {} span mismatch(es)",
            after_replay.0 - before_replay.0,
            after_replay.1 - before_replay.1
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }
        // All three frames are pinned byte-exact against both oracles (the
        // aomdec/ffmpeg arms below run over `.take(3)`): frame 0's keyframe
        // PARTITION_VERT 64x128, frame 1's inter pieces, and frame 2 -- whose
        // right superblock column (the 128-root VERT piece at the frame edge)
        // is three intra blocks (64x64, 64x32, 64x32) whose chroma units ride
        // the inter-frame intra tail. That tail read ONE whole-plane chroma
        // unit with the block-level context where 4:4:4 codes four
        // plane-major TX_32X32 units with per-unit contexts and the
        // offset-10 `get_txb_ctx` rows, which left three quarters of the
        // planes' residual unread and misaligned the symbol stream from the
        // first chroma unit on (f2 PREFILT Y 6952 / U 9568 / V 13496, first
        // diffs entering at column 128). Fixed on lane-av1-rect128recon;
        // the earlier "LR never restores RU row 0" attribution on this frame
        // was a stage-ladder mixup, refuted in lane-av1lrflush r3.

        // The oracle aomdec, rawvideo out: FRAMES concatenated yuv444p frames.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-444sb-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            for (i, f) in frames.iter().enumerate().take(3) {
                let base = i * W * H * 3;
                for ((plane, off), p) in [(&f.y, 0usize), (&f.u, W * H), (&f.v, 2 * W * H)]
                    .into_iter()
                    .zip(["y", "u", "v"])
                {
                    let want = &ref_raw[base + off..base + off + W * H];
                    let bad = plane
                        .iter()
                        .zip(want)
                        .filter(|&(&a, &b)| a as u8 != b)
                        .count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: aomdec frame {i} plane {p}: {bad} samples differ \
                         (class 444-128rect-chroma-span)"
                    );
                    assert_eq!(
                        plane.len(),
                        want.len(),
                        "{NAME}: aomdec frame {i} plane {p}: ours has {} samples, the oracle's \
                         has {} -- a short plane would zip-truncate the compare above and hide \
                         a chroma-span defect",
                        plane.len(),
                        want.len()
                    );
                }
            }
        }

        // ffmpeg's decoder: same frames, full-resolution chroma. Same
        // three-frame scope as the aomdec arm above.
        if have_ffmpeg() {
            let refs = ffmpeg_decode_sequence_444(&stream, W, H, FRAMES);
            for (i, (got, want)) in frames.iter().zip(refs.iter()).enumerate().take(3) {
                for (p, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: ffmpeg frame {i} plane {p}: {bad} samples differ"
                    );
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME}: ffmpeg frame {i} plane {p}: ours has {} samples, ffmpeg's has {} \
                         -- a short plane would zip-truncate the compare above and hide a \
                         chroma-span defect",
                        g.len(),
                        r.len()
                    );
                }
            }
        } else {
            eprintln!("SKIP {NAME} ffmpeg arm: no ffmpeg");
        }
    }

    /// lane-av1444rect (H1): the 4:4:4 LOSSY cell with a 1:4 (HORZ_4 /
    /// VERT_4) partition -- the largest correctness gap the chroma-format
    /// sweep left, and the first 4:4:4 lossy exactness gate in the crate.
    ///
    /// **The defect.** `decode_inter_block`'s `around_c` gathered a 1:4
    /// strip's chroma coefficient context at the 4:2:0 PAIR extent
    /// (`(16, 8)` HORZ / `(8, 16)` VERT) with no `ss` gate, so at ss (0,0)
    /// it read one extra luma mi row of LEFT chroma context. At 4:4:4
    /// `is_chroma_reference` (av1_common_int.h:1454) reduces both parity
    /// clauses to `!subsampling_y` / `!subsampling_x`, so every block is its
    /// own chroma reference and no pair exists: the strip codes its own
    /// 16x4 chroma over its own context. Our U unit's `get_txb_ctx` then saw
    /// a neighbour libaom does not have, and the arithmetic coder forked.
    ///
    /// **The recipe** (sha256
    /// `77f727e76d81fe48cd03709f799131677c04869b163e18d78e96bc742ec69153`,
    /// 6441 bytes, 2 frames, `testsrc2 128x96 rate=25` yuv444p):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=128x96:rate=25" -frames:v 2 \
    ///        -pix_fmt yuv444p -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=20 \
    ///        --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 \
    ///        --kf-max-dist=100 --limit=2 - out.obu
    /// ```
    /// (`rate=25` is load-bearing: `testsrc2`'s pattern is time-parameterised,
    /// so `rate=1` produces a different, already-exact stream.)
    ///
    /// Red before the fix: frame 1, 8884 of 36864 samples wrong, first at
    /// Y(91,32); the first wrong symbol was read 26263 of the frame, both
    /// sides at mi (8,16) -- ours a luma-shaped `txb_skip` row (our
    /// re-indexed chroma ctx 1), the oracle `decodetxb.c:158` on the U plane
    /// with `txb_skip_ctx = 7`.
    ///
    /// Non-vacuity, three ways: the pinned stream must really be ss (0,0)
    /// (asserted through the oracle aomdec's own sequence header, below);
    /// `decode::rect4_inter_own_chroma444_hits()` must be non-zero, so the
    /// 1:4-strip route the fix sits on really ran; and the SAME source and
    /// flags at 4:2:0 must leave that counter at zero, which is what makes
    /// it the 4:4:4-only route rather than a tally every strip bumps.
    #[test]
    fn a_444_lossy_rect4_inter_stream_decodes_pixel_exact() {
        const NAME: &str = "a_444_lossy_rect4_inter_stream_decodes_pixel_exact";
        const FIXTURE_LEN: usize = 6441;
        const FIXTURE_FNV: u64 = 0x0436_8b2d_2e91_72b0;
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 2;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_lossy_rect4_inter_witness.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        crate::decode::reset_rect4_inter_own_chroma444_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: the pinned 4:4:4 lossy stream no longer decodes cleanly: {e}")
        });
        let own_chroma = crate::decode::rect4_inter_own_chroma444_hits();
        assert!(
            own_chroma > 0,
            "{NAME}: no 1:4 inter strip read its chroma context over its own \
             extent -- the corrected route never ran (class gate-blind-to-feature)"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: frame {i} dimensions");
            // At 4:4:4 the chroma planes are full resolution. A decoder that
            // kept the 4:2:0 extent would hand quarter-size chroma planes to
            // the byte compare below, which cannot pass -- but say so here
            // rather than let the compare stand in for the shape claim.
            assert_eq!(
                f.u.len(),
                W * H,
                "{NAME}: frame {i} chroma plane is {} samples; at 4:4:4 it must be \
                 full resolution ({W}x{H})",
                f.u.len()
            );
        }
        if aomdec_available(NAME) {
            let (decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME}: {decoded} frame(s) byte-exact vs aomdec, {own_chroma} \
                 own-extent 1:4 chroma gather(s)"
            );
        }

        // The specificity arm: the SAME source, flags and 1:4 partitions at
        // 4:2:0 must not touch the 4:4:4-only route. If a later change made
        // the counter fire everywhere, this arm is what catches it.
        if !have_aomenc() {
            eprintln!("{NAME}: no aomenc, 4:2:0 specificity arm skipped");
            return;
        }
        let source = format!("testsrc2=size={W}x{H}:rate=25");
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-frames:v",
                "2",
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
        assert!(
            y4m.status.success(),
            "{NAME}: ffmpeg refused the 4:2:0 control source: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=20",
                "--cpu-used=2",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--kf-max-dist=100",
                "--limit=2",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "{NAME}: aomenc refused the 4:2:0 control recipe: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        crate::decode::reset_rect4_inter_own_chroma444_hits();
        decode_stream(&out.stdout)
            .unwrap_or_else(|e| panic!("{NAME}: the 4:2:0 control stream was refused: {e}"));
        let (h4, v4, _, _) = crate::decode::inter16_rect4_counters();
        // The control must actually code 1:4 inter strips, or it says nothing
        // about the arm the fix touches. Measured: 16 HORZ_4, 28 VERT_4.
        assert!(
            h4 + v4 > 0,
            "{NAME}: the 4:2:0 control coded NO 1:4 inter strip ({h4} HORZ_4, {v4} \
             VERT_4) -- the identity arm below would say nothing about 1:4 strips"
        );
        assert_eq!(
            crate::decode::rect4_inter_own_chroma444_hits(),
            0,
            "{NAME}: the 4:2:0 control fired the ss_x-0 own-extent route -- the route \
             is not specific to the 4:4:4 cell it was fixed for"
        );
        // The identity half: the fix adds an arm gated on `ss_x == 0`, so a
        // 4:2:0 stream must stay byte-exact through it. This is the arm that
        // would go red if the new arm were ever widened past ss_x 0.
        if aomdec_available(NAME) {
            decode_all_frames_vs_oracle(&out.stdout, &format!("{NAME}-420"));
        }
        eprintln!("{NAME}: 4:2:0 control {h4} HORZ_4 / {v4} VERT_4 strips, 0 own-extent gathers");
    }

    /// lane-av1gates444 cell 1: **odd coded dims, 66x66** -- the partial-frame
    /// walk in BOTH axes (66 is not a multiple of 8, so the frame is a
    /// partial superblock column AND a partial row). Measured EXACT 4/4 by
    /// the chroma-format sweep (§6.2, 5938 bytes) and unpinned: nothing in
    /// the crate would go red if the odd-dimension 4:4:4 walk regressed,
    /// and the only red-before evidence was a lane report nobody re-runs.
    #[test]
    fn a_444_lossy_odd_66x66_stream_decodes_pixel_exact() {
        gate_444_lossy_live_exact("a_444_lossy_odd_66x66_stream_decodes_pixel_exact", 66, 66);
    }

    /// lane-av1gates444 cell 2: **odd coded dims, 130x122** -- the
    /// partial-frame RIGHT edge that the sweep found DIVERGENT (first wrong
    /// sample Y(128,16), 11521 samples on frame 3) until lane-av1444rect's
    /// 4:4:4 own-chroma-extent fix landed. It measures EXACT 4/4 on this
    /// tree and is the discriminating geometry: 66x66 was exact at the same
    /// settings while this was not, so the pair is what keeps "the 4:4:4
    /// partial-frame walk is right" from being a one-geometry accident.
    #[test]
    fn a_444_lossy_odd_130x122_stream_decodes_pixel_exact() {
        gate_444_lossy_live_exact(
            "a_444_lossy_odd_130x122_stream_decodes_pixel_exact",
            130,
            122,
        );
    }

    /// lane-av1gates444 cell 3: **256x128**, the sweep's cell recorded as
    /// "4:4:4 superres 256x128 (`--superres-mode=1`)".
    ///
    /// IT IS NOT A SUPERRES CELL, and this gate is named for what it is
    /// instead. Measured here, live: encoding 256x128 with and without
    /// `--superres-mode=1` produces BYTE-IDENTICAL OBU (15239 bytes,
    /// sha256 06621606…, both), so the flag is a no-op on this aomenc build
    /// and the sweep's cell was a plain 4:4:4 lossy cq-20 stream that
    /// happened to carry the flag. The cell's actual value is the geometry
    /// plus the filter stages: 256x128 is two full 128 superblock columns,
    /// and the strip walk runs there with the post-deblock/CDEF/LR chain
    /// live on the later frames -- so this is the third witness for the
    /// 4:4:4 lossy strip class at a geometry the 128x96 rect4 gate does not
    /// cover.
    #[test]
    fn a_444_lossy_256x128_stream_decodes_pixel_exact() {
        gate_444_lossy_live_exact("a_444_lossy_256x128_stream_decodes_pixel_exact", 256, 128);
    }

    /// lane-av1txctxband — the 4:4:4 lossy `--sb-size=128` stream whose
    /// `EC_SYMR` ladder forked at read 32813, and the gate that makes the fix
    /// non-vacuous.
    ///
    /// **The defect.** `get_tx_size_context` (libaom
    /// `av1/common/pred_common.h:361-370`) gates its "an inter neighbour
    /// contributes its own BLOCK size" override on `xd->above_mbmi` /
    /// `xd->left_mbmi`, which `set_mi_offsets`
    /// (`av1_common_int.h:1381-1389`) takes as `xd->mi[-xd->mi_stride]` /
    /// `xd->mi[-1]`. `setup_mbs` (`decodeframe.c:355-361`) writes the covering
    /// block's own `MB_MODE_INFO` pointer into EVERY mi cell the block covers,
    /// so libaom's granularity is the covering block PER CELL. This decoder
    /// read the per-COLUMN `above_inter`/`left_inter` bands and the matching
    /// `above_side_mi`/`left_side_mi` bands instead — the LAST block written in
    /// that column, which names the abutting block only for the block directly
    /// below it. On this witness the 8x4 intra leaf at mi(57,52) (SB row 1,
    /// col 1) read a stale `above_inter=true` with `above_side=4`, took the
    /// override, got `above = (4 >= 8) = 0` where libaom keeps
    /// `above = (above_txfm_context[0] >= 8) = 1`, and read
    /// `tx_size_cat0[1]` where libaom reads `tx_size_cdf[...][2]`.
    ///
    /// **The fork signature that identifies it** (a CDF-row error, not a
    /// unit-enumeration one): the two sides held the IDENTICAL pre-state
    /// `pre=(7550, 45492)` and the IDENTICAL width/value `n=2 s=1`; only
    /// `cdf0` differed, ours 6424 (`32768-6424 = 26344`) vs the oracle's
    /// 19938, so the ICDF mirror convention does not reconcile it either.
    ///
    /// **Non-vacuity.** The pixel assert alone would be green on the old code
    /// for any stream where the bands happened to be right, so the gate also
    /// asserts `txctx_inter_cell_band_divergence_hits() > 0` — the number of
    /// `get_tx_size_context` reads where the per-CELL cover and the
    /// per-COLUMN band DISAGREE, i.e. exactly the reads the old code resolved
    /// off a different CDF row. Reverting either reader to the bands drives
    /// this to 0 and the gate red, with the oracle compare as the second,
    /// independent arm.
    ///
    /// **Fixture provenance** (`444_lossy_sb128_txctx_witness.obu`, 6186 B,
    /// sha256 `14acb74e825a2bb3ef195a52cc2abc67794d9c4d92080b66c739d318177fc7c6`):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x256:rate=25" -frames:v 2 \
    ///        -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe -y src.y4m
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=62 --cpu-used=2 \
    ///        --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    ///        --sb-size=128 --profile=1 --obu -o t256.obu src.y4m
    /// ```
    /// 256x256 is the smallest geometry that gives `--sb-size=128` a SECOND
    /// superblock row and column, which is where the stale band bites: a cell
    /// whose abutting block is not the last block written in its column. The
    /// cq level is load-bearing — this cell is entropy-exact at cq 20..58 and
    /// forks at 54/56/60/62 depending on the frame count (12 of the 99 cells
    /// swept in `lanes/av1txctxband.report.md` §4 forked, all of them at
    /// cq >= 54 on 256x256/320x240/256x384). `--sb-size=128` is load-bearing
    /// too: at the default 64 the two superblock columns are 4 mi wide, so a
    /// column band and a per-cell cover coincide for every 4-column run.
    #[test]
    fn a_444_lossy_sb128_txctx_reads_the_covering_block_per_cell() {
        const NAME: &str = "a_444_lossy_sb128_txctx_reads_the_covering_block_per_cell";
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_lossy_sb128_txctx_witness.obu");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- a failure, not a skip",
                path.display()
            )
        });
        assert_eq!(bytes.len(), 6186, "{NAME}: fixture length moved");
        assert_eq!(
            fnv1a64(&bytes),
            0x2ec1_bb1b_5196_c4b3,
            "{NAME}: fixture bytes moved"
        );

        let _guard = lock_gate_counters();
        crate::decode::reset_txctx_inter_cell_band_divergence_hits();
        let frames = decode_stream(&bytes)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned 4:4:4 stream was refused: {e}"));
        assert_eq!(frames.len(), 2, "{NAME}: frame count");
        let diverged = crate::decode::txctx_inter_cell_band_divergence_hits();
        assert!(
            diverged > 0,
            "{NAME}: no `get_tx_size_context` read on this stream had the \
             per-CELL cover and the per-COLUMN band disagree, so the witness \
             does not exercise the corrected read at all (class \
             gate-blind-to-feature) -- re-encode per the recipe above"
        );
        if aomdec_available(NAME) {
            decode_all_frames_vs_oracle(&bytes, NAME);
        }
        eprintln!(
            "{NAME}: {diverged} tx_depth read(s) where the per-cell cover and \
             the per-column band disagreed (the 8x4 intra leaf at mi(57,52) is \
             the one that forked the ladder at read 32813)"
        );
    }

    /// lane-arfchroma: 4:2:0 **lossless** with an alt-ref frame and 1:4 inter
    /// strips. Ordinary, user-reachable aomenc output, no bypass: before the
    /// fix this cell diverged in 15 of 16 shown frames (1 088 573 wrong
    /// samples, only frames 0-1 exact) or -- one knob away, e.g.
    /// `--sb-size=64`, `--enable-dual-filter=0`, `--max-reference-frames=3`,
    /// or the same stream cut to 5-12 frames -- REFUSED outright with
    /// `a Golomb tail longer than this decoder reads`. The refusal was this
    /// divergence arriving at a named guard, not an independent gap: the
    /// `16-frame` variant of this exact cell now decodes byte-exact.
    ///
    /// Mechanism (paired rungs, `EC_COEFF_STEP` / `EC_ECDUMP` against
    /// aomdec, 320x240, 5 frames): the first forked read is frame 2's 8x8
    /// inter block at mi (4,0) (ref0 = 7 = ALTREF, mv (36,0), NEWMV), its
    /// CHROMA U `txb_skip` read. All four LUMA units match read for read, and
    /// the chroma read's pre-state `(rng 35592)` is IDENTICAL on both sides --
    /// only the context differs: ours 2, libaom 8
    /// (`7 + get_entropy_context`, `txb_common.h:360`). `EC_ECDUMP` names the
    /// cause: libaom's chroma left neighbour is `[0]` and ours is coded. The
    /// publisher is the 16x4 (1:4) strip above it: `record_mi_chroma`
    /// published the unit at the BLOCK's mi origin, but a 4x4 chroma unit is
    /// `1 << ss` luma mi per axis, so a strip whose mi row is ODD at 4:2:0
    /// starts mid-cell and its state landed one mi row too low -- into the
    /// cell of the block that read it. libaom's per-plane
    /// `left/above_context_map` is indexed in the PLANE's own 4x4 units
    /// (`entropy.h:87`, `txb_common.h:429`), so the cell's origin is the
    /// step-aligned mi, which is what the fix aligns to.
    ///
    /// The gate asserts three things, in this order: the pin's identity, the
    /// mid-cell route counter (so it cannot pass on a stream whose chroma
    /// units all start on a cell boundary -- class
    /// gate-blind-to-feature), and then BOTH comparisons: decode order
    /// (which sees the hidden alt-ref frame no shown-frame gate can) and the
    /// per-plane shown-frame count.
    ///
    /// Recipe (both sides reproducible, 28394 bytes):
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25 -frames:v 5 \
    ///        -pix_fmt yuv420p y.yuv
    /// aomenc --codec=av1 --obu --lossless=1 -w 320 -h 240 \
    ///        --lag-in-frames=25 --auto-alt-ref=1 --limit=5 -o w.obu y.yuv
    /// ```
    /// sha256 c5507ed97f74ca495b71449fda24f5fe2fcfa34eed38988a69141d45b126627c.
    /// The tree pins fixture identity by fnv1a64; a `const SHA256` compared
    /// against itself would be the tautology lane-av1422anom exists to kill.
    #[test]
    fn a_420_lossless_alt_ref_1to4_strip_witness_is_byte_exact_in_decode_order() {
        const NAME: &str =
            "a_420_lossless_alt_ref_1to4_strip_witness_is_byte_exact_in_decode_order";
        // lane-av1cmpaudit2: same mutex as every other oracle gate.
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_lossless_arf_1to4_320x240_5f.obu";
        const BYTES: usize = 28394;
        const FNV1A64: u64 = 16971188390893372935;
        let path = crate_pin("420_lossless_arf_1to4_320x240_5f.obu");
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        crate::decode::reset_chroma_midcell_pub_hits();
        let (decoded, hidden) = decode_all_frames_vs_oracle(&data, NAME);
        assert_eq!(
            decoded, 6,
            "{NAME}: the oracle decoded {decoded} frames in DECODE order; the 5-frame recipe \
             carries 5 source frames, one of them a hidden alt-ref picture plus a \
             `show_existing_frame` header"
        );
        assert_eq!(
            hidden, 1,
            "{NAME}: this cell exists for the HIDDEN alt-ref frame -- {decoded} decode-order \
             dumps against {decoded} - {hidden} shown outputs means the gate saw the picture no \
             shown-frame compare can reach"
        );
        let mid = crate::decode::chroma_midcell_pub_hits();
        assert!(
            mid[2] > 0 || mid[0] + mid[1] > 0,
            "{NAME}: no chroma transform unit was published from a mid-cell origin \
             ({mid:?} = [row, col, both]), so this witness does not exercise the origin \
             alignment at all (class gate-blind-to-feature) -- re-encode per the recipe above"
        );
        let (wy, wu, wv, shown, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv, exact),
            (0, 0, 0, shown),
            "{NAME}: shown-frame per-plane diff vs `aomdec --rawvideo`"
        );
        eprintln!(
            "{NAME}: {decoded} decode-order frame(s) byte-exact ({hidden} hidden), \
             {shown} shown frame(s) exact per plane, mid-cell chroma publications {mid:?}"
        );
    }

    /// lane-av1mixllfork: per-segment MIXED LOSSLESS + an alt-ref structure.
    /// The first fork was entropy read **30889** of f1 (paired `EC_SYMR`:
    /// identical `pre[0..1]`, `n`, `s` on both sides, `post_rng` 42076 vs
    /// 54010), and the pre-fix `cdf0` at that read was 22401 vs the oracle's
    /// 15299 -- **not** the usual `32768 - x` ICDF mirror, i.e. two different
    /// reads, not one wrong CDF row.
    ///
    /// What each side read there: libaom `decodetxb.c:158`, the luma `txb_skip`
    /// of a TX_4X4 unit at `mi=(6,1)` `bc=0 br=0`; ours an untagged `n=2`
    /// symbol that a `set_symr_cdf` sweep over all six MV-stack copies
    /// (`ii16/newmv16/…/refmvB`, `inter8` phase inherited) ruled out as every
    /// inter mode-info read, leaving the one read that block makes *between*
    /// its `refmv` and its coefficients: the var-tx tree's `tx_size_cat1`.
    /// libaom's own reason it reads nothing: `parse_decode_block`
    /// (`decodeframe.c:1237`) carries `!xd->lossless[mbmi->segment_id]` as a
    /// **conjunct** of the tx-mode-select condition, and `read_tx_size`
    /// (`:1208`) answers TX_4X4 on the same flag. This tree tested
    /// `tx_select && !skip` and put `lossless(fctx) && !skip` in the `else if`
    /// arm instead, so on a lossless SEGMENT of a mixed-lossless frame it read
    /// a `tx_size_cat1` symbol libaom never reads, and the very next block's
    /// `txb_skip` forked. `SUB8_LOSSLESS_NO_VARTX` counts the leaves that
    /// newly take the lossless arm, so this gate cannot pass vacuously on a
    /// stream whose sub-8 leaves all sit in lossy segments. (Corrected
    /// 2026-09-30: the counter is now guarded on `tx_select_inter`, because the
    /// refutation pass measured 33 hits on a build with the conjunct REMOVED --
    /// the arm also serves leaves whose tx-select is off. The pin's own delta
    /// is 10, not the 40 the lane first reported; both numbers are measured.)
    ///
    /// The trigger really is the CONJUNCTION: `--cq-level=0` with AQ makes
    /// *some* segments lossless and others not (`--cq-level=20`, `--cq-level=32`,
    /// `--lossless=1` and `--aq-mode=0` are all byte-exact on this recipe), and
    /// `--auto-alt-ref=0` makes it exact at 5, 6, 8 and 12 frames. After the
    /// fix the whole-stream `EC_SYMR` ladder is bit-identical: **96997 reads,
    /// 96997 paired, zero divergence** -- not just up to the fork.
    ///
    /// Recipe (ffmpeg 8.1.3 `testsrc2`, shared oracle `aomenc`, 18525 bytes):
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=256x128:rate=25 -frames:v 5 \
    ///        -pix_fmt yuv420p src5b.y4m
    /// aomenc --codec=av1 --obu -o c_mix5.obu --passes=1 --cpu-used=4 \
    ///        --limit=5 --end-usage=q --cq-level=0 --aq-mode=1 src5b.y4m
    /// ```
    /// sha256 `3e06b5641e6a0f5a3f4ac8f114d4ee48c88638429e9eaafe6b4483e24f1dbc3a`.
    /// The tree pins fixture identity by fnv1a64; a `const SHA256` compared
    /// against itself would be the tautology lane-av1422anom exists to kill.
    #[test]
    fn a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order() {
        const NAME: &str =
            "a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order";
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_mixll_altref_256x128_5f.obu";
        const BYTES: usize = 18525;
        const FNV1A64: u64 = 16348502764482216576;
        let path = crate_pin(FILE);
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        let before = crate::decode::sub8_lossless_no_vartx();
        let (decoded, hidden) = decode_all_frames_vs_oracle(&data, NAME);
        let suppressed = crate::decode::sub8_lossless_no_vartx() - before;
        assert_eq!(
            decoded, 6,
            "{NAME}: the oracle decoded {decoded} frames in DECODE order; the 5-frame \
             altref recipe codes 5 source frames plus a hidden alt-ref picture"
        );
        assert_eq!(
            hidden, 1,
            "{NAME}: {decoded} decode-order dumps against {decoded} - {hidden} shown outputs \
             means the gate saw the hidden picture no shown-frame compare can reach"
        );
        assert!(
            suppressed > 0,
            "{NAME}: no sub-8 (4x8/8x4) inter leaf on a LOSSLESS segment read no var-tx \
             symbol ({suppressed} of them did), so this witness does not exercise the \
             ordering at all (class gate-blind-to-feature) -- re-encode per the recipe above"
        );
        let (wy, wu, wv, shown, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv, exact),
            (0, 0, 0, shown),
            "{NAME}: shown-frame per-plane diff vs `aomdec --rawvideo`"
        );
        eprintln!(
            "{NAME}: {decoded} decode-order frame(s) byte-exact ({hidden} hidden), \
             {shown} shown frame(s) exact per plane, {suppressed} sub-8 leaf/leaves on a \
             lossless segment read no var-tx symbol"
        );
    }

    /// lane-av1txctxband2: the 640x480 cell of the same mixed-lossless
    /// recipe — the THIRD defect `lanes/av1mixllconj.report.md` §4.3 /
    /// §11 handed over, class `context-band-not-published`.
    ///
    /// libaom's `parse_decode_block` (`decodeframe.c:1251-1261`) is an
    /// EITHER/OR: the var-tx branch's condition carries
    /// `!xd->lossless[mbmi->segment_id]` as a conjunct, and the `else` branch
    /// runs `read_tx_size` **and then `set_txfm_ctxs`**. `read_tx_size`'s
    /// FIRST line (`decodeframe.c:1214`) returns `TX_4X4` on a lossless
    /// segment — so a lossless block skips the symbol read, NOT the publish.
    /// This decoder's `read_block_tx_size` returned from its own lossless early
    /// return with nothing written, so on a MIXED frame every lossless square
    /// block left the `TXFM_CONTEXT` above band holding whatever an earlier,
    /// lossy block had put there. The RECT twin (`read_block_tx_size_rect`)
    /// has published on the same arm since lane-av1lm444loss; only the square
    /// arm had the hole.
    ///
    /// Measured on `420_mixll_altref_640x480_5f.obu`: the abutting 8x8 INTRA
    /// lossless block at mi(78,110) left `above_txfm[110]` at the 16 a skipped
    /// 16-px inter block at mi(36,108) had written 43 mi rows earlier, and the
    /// 8x4 intra leaf with `use_filter_intra` at mi(80,110) then read
    /// `tx_size_cat0[2]` where libaom reads row 1 — the oracle's own
    /// `EC_TXCTXB` prints `mi=80,110 abv=4 lft=8 above=0 left=1 ctx=1` against
    /// our `above_txfm=16 left_txfm=8 above=1 left=1 ctx=2`. The tile desynced
    /// there and the frame REFUSED (`a Golomb tail longer than this decoder
    /// reads`); at base the same cell decoded every frame but got 70–92 % of
    /// every inter frame wrong.
    ///
    /// `LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS` counts the lossless square inter
    /// blocks whose publish actually OVERWROTE a size an earlier block of the
    /// same frame had left in the column — so the gate cannot pass vacuously
    /// on a stream that merely decodes a lossless fixture. After the fix the
    /// whole-stream paired `EC_SYMR` ladder is bit-identical: **457 907 reads,
    /// 457 907 paired, zero divergence** — not just up to the fork.
    ///
    /// Recipe (ffmpeg 8.1.3 `testsrc2`, shared oracle `aomenc`, 87537 bytes):
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=640x480:rate=25 -frames:v 5 \
    ///        -pix_fmt yuv420p src640.y4m
    /// aomenc --codec=av1 --obu -o 420_mixll_altref_640x480_5f.obu --passes=1 \
    ///        --cpu-used=4 --limit=5 --end-usage=q --cq-level=0 --aq-mode=1 src640.y4m
    /// ```
    /// sha256 `3d505fd2e0bbb7ca3680a8e338d712a81defc947b955a4b7b82de6718301e73c`;
    /// the tree pins fixture identity by fnv1a64, not a `const SHA256` compared
    /// against itself.
    #[test]
    fn a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order() {
        const NAME: &str = "a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_\
                            in_decode_order";
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_mixll_altref_640x480_5f.obu";
        const BYTES: usize = 87537;
        const FNV1A64: u64 = 12707865649077681891;
        let path = crate_pin(FILE);
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        let before = crate::decode::lossless_sq_txfm_band_overwrite_hits();
        let (decoded, hidden) = decode_all_frames_vs_oracle(&data, NAME);
        let overwrote = crate::decode::lossless_sq_txfm_band_overwrite_hits() - before;
        assert_eq!(
            decoded, 6,
            "{NAME}: the oracle decoded {decoded} frames in DECODE order; the 5-frame \
             altref recipe codes 5 source frames plus a hidden alt-ref picture"
        );
        assert_eq!(
            hidden, 1,
            "{NAME}: {decoded} decode-order dumps against {decoded} - {hidden} shown outputs \
             means the gate saw the hidden picture no shown-frame compare can reach"
        );
        assert!(
            overwrote > 0,
            "{NAME}: no lossless square block on an inter frame published a txfm band that \
             OVERWROTE an earlier block's size ({overwrote} of them did), so this witness does \
             not exercise the missing publish at all (class gate-blind-to-feature) -- re-encode \
             per the recipe above"
        );
        let (wy, wu, wv, shown, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv, exact),
            (0, 0, 0, shown),
            "{NAME}: shown-frame per-plane diff vs `aomdec --rawvideo`"
        );
        eprintln!(
            "{NAME}: {decoded} decode-order frame(s) byte-exact ({hidden} hidden), \
             {shown} shown frame(s) exact per plane, {overwrote} lossless square inter \
             block(s) overwrote a stale above-band size"
        );
    }

    /// lane-av1txctxband2 r2: the SAME one-line publish, three MORE cells —
    /// the mixed-lossless RESIDUE `lanes/av1mixllconj.report.md` §7 left
    /// explicitly unnamed ("separate defects this change shrinks but does not
    /// remove"). **They are the same defect.** All three are this recipe's
    /// mixed-lossless+altref stream at 256x128 (6 frames) and 320x240 (5 and 6
    /// frames); each has a lossy block whose `get_tx_size_context` /
    /// `txfm_partition_context` read a band a lossless block above should have
    /// published into and did not.
    ///
    /// Measured on `f2dd27b0~1` (= `0bfee517`, the tip before this lane's
    /// publish) against `aomdec`'s `EC_AV1_FINAL_DUMP`, decode order, per
    /// differing frame:
    ///
    /// ```text
    /// 420_mixll_256x128_6f.obu  7 dumps  red f1..f6: 6051 / 4115 / 2962 / 26566 / 41876 / 14749
    /// 420_mixll_320x240_5f.obu  6 dumps  red f1..f5: 15031 / 16259 / 21932 / 65405 / 90077
    /// 420_mixll_320x240_6f.obu  7 dumps  red f6 only: 19398
    /// ```
    ///
    /// With the publish: **7/7, 6/6 and 7/7 byte-exact**, 0 differing bytes on
    /// every plane of every decode-order frame. `m5_320x240`'s f2/f3 here
    /// (16259 / 21932) differ from the 16006 / 21930 `lane-av1mixllconj` §7
    /// records, because that table was taken before `lane-av1offtile`'s
    /// chroma-plane clip merged; f1/f4/f5 and both other cells match it exactly.
    ///
    /// Recipe (ffmpeg 8.1.3 `testsrc2`, shared oracle `aomenc`; `--limit` is the
    /// only variable per cell):
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=<W>x<H>:rate=25 -frames:v <N> -pix_fmt yuv420p src.y4m
    /// aomenc --codec=av1 --obu -o <cell>.obu --passes=1 --cpu-used=4 --limit=<N> \
    ///        --end-usage=q --cq-level=0 --aq-mode=1 src.y4m
    /// ```
    /// sha256 `8a203e52…` (22336 B), `40d613fd…` (28716 B), `37fbe7cb…`
    /// (38686 B); pinned below by fnv1a64, not a `const SHA256` compared
    /// against itself.
    #[test]
    fn a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order() {
        const NAME: &str = "a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order";
        let _gate_lock = lock_gate_counters();
        // (file, bytes, fnv1a64, decode-order frames, hidden)
        const CELLS: [(&str, usize, u64, usize, usize); 3] = [
            ("420_mixll_256x128_6f.obu", 22336, 5694016005779287373, 7, 1),
            ("420_mixll_320x240_5f.obu", 28716, 5855109243102450179, 6, 1),
            ("420_mixll_320x240_6f.obu", 38686, 49021385186397678, 7, 1),
        ];
        for (file, bytes, fnv, frames, hidden) in CELLS {
            let path = crate_pin(file);
            let data = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: the committed pin {file} is missing ({e})"));
            assert_eq!(data.len(), bytes, "{NAME}: {file} size drifted");
            assert_eq!(fnv1a64(&data), fnv, "{NAME}: {file} bytes drifted");
            let before = crate::decode::lossless_sq_txfm_band_overwrite_hits();
            let (decoded, got_hidden) = decode_all_frames_vs_oracle(&data, file);
            let overwrote = crate::decode::lossless_sq_txfm_band_overwrite_hits() - before;
            assert_eq!(
                decoded, frames,
                "{NAME}/{file}: the oracle decoded {decoded} frames in DECODE order"
            );
            assert_eq!(
                got_hidden, hidden,
                "{NAME}/{file}: {decoded} decode-order dumps against {decoded} - {got_hidden} \
                 shown outputs means the gate saw {hidden} hidden alt-ref picture/ies"
            );
            assert!(
                overwrote > 0,
                "{NAME}/{file}: no lossless square block on an inter frame published a txfm \
                 band that OVERWROTE an earlier block's size ({overwrote} of them did), so this \
                 cell does not exercise the missing publish at all (class \
                 gate-blind-to-feature) -- re-encode per the recipe above"
            );
            eprintln!(
                "{NAME}/{file}: {decoded} decode-order frame(s) byte-exact ({got_hidden} hidden), \
                 {overwrote} lossless square inter block(s) overwrote a stale above-band size"
            );
        }
    }

    /// lane-av1444rect r2 — the SAME class as the gate above, second
    /// instance: `decode_rect4_16_intrabc` applied the 4:2:0 pair geometry at
    /// ss (0,0), where `is_chroma_reference` makes an intra-BC 1:4 strip its
    /// own chroma reference. It predicted and coded an 8x4 chroma block at
    /// `(lmi - 1) * MI / 2` where libaom codes 16x4 at the strip's origin.
    ///
    /// **This is a PROGRESS gate, not an exactness gate, and it says so.** The
    /// 4:4:4 intra-BC cell is still divergent: after the ss gate the LUMA
    /// plane is exact to sample 205264 (it was 164416) and the entropy fork
    /// moves from read 11750 to 12465, but both CHROMA planes still first
    /// diverge at index 20532 and 390380 of 921600 samples are wrong. So the
    /// assertion is a prefix floor on the plane the gate actually moved —
    /// which only ever grows, and which the next lane's fix cannot turn red —
    /// and the untouched chroma divergence is named, never asserted exact.
    ///
    /// Recipe (both fixtures, one frame, `testsrc2 640x480 r=25`):
    /// ```text
    /// aomenc --codec=av1 --bit-depth=8 --input-bit-depth=8 --passes=1
    ///        --end-usage=q --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1
    ///        --limit=1 --threads=1 --enable-rect-partitions=1
    ///        --enable-1to4-partitions=1 --min-partition-size=4
    ///        --max-partition-size=64 --sb-size=64 --tune-content=screen
    ///        --enable-intrabc=1 --enable-palette=1 --cq-level=60
    ///        --enable-tx-size-search=0 --obu [--profile=1] -o out.obu src.y4m
    /// ```
    /// 4:4:4 (1016 B, sha256 7fe888ed…) carries 17 intrabc 16x4 strips at
    /// ss (0,0); the 4:2:0 twin (890 B, sha256 d01352af…) is byte-exact and
    /// is the identity arm — the ss gate cannot reach it, and the whole frame
    /// is compared with the length asserted first.
    #[test]
    fn a_444_intrabc_rect4_reads_its_own_chroma_plane_block() {
        const NAME: &str = "a_444_intrabc_rect4_reads_its_own_chroma_plane_block";
        let read = |name: &str, file: &str, len: usize, fnv: u64| -> Vec<u8> {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("fixtures/{file}"));
            let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{name}: pinned fixture {} is missing ({e}) -- a failure, not a skip",
                    path.display()
                )
            });
            assert_eq!(bytes.len(), len, "{name}: {file} length moved");
            assert_eq!(fnv1a64(&bytes), fnv, "{name}: {file} bytes moved");
            bytes
        };
        let stream444 = read(
            NAME,
            "444_intrabc_rect4_witness.obu",
            1016,
            0x27c2_fad5_4047_2994,
        );
        let stream420 = read(
            NAME,
            "420_intrabc_rect4_witness.obu",
            890,
            0x57ec_3da2_a63b_403a,
        );

        let _guard = lock_gate_counters();
        crate::decode::reset_intrabc_rect4_own_chroma444_hits();
        let frames = decode_stream(&stream444)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned 4:4:4 stream was refused: {e}"));
        let own = crate::decode::intrabc_rect4_own_chroma444_hits();
        assert!(
            own > 0,
            "{NAME}: no intra-BC 1:4 strip coded its own chroma plane block -- the \
             corrected route never ran (class gate-blind-to-feature)"
        );
        assert_eq!(frames.len(), 1, "{NAME}: frame count");

        // The identity arm FIRST and in full: the 4:2:0 twin must stay
        // byte-exact. `decode_all_frames_vs_oracle` asserts the two frame
        // lengths match before it compares anything -- the check whose absence
        // turned an out-of-window divergence into a reported EXACT in this
        // lane's own round 1.
        if aomdec_available(NAME) {
            decode_all_frames_vs_oracle(&stream420, &format!("{NAME}-420"));
            crate::decode::reset_intrabc_rect4_own_chroma444_hits();
            decode_stream(&stream420)
                .unwrap_or_else(|e| panic!("{NAME}: the 4:2:0 twin was refused: {e}"));
            assert_eq!(
                crate::decode::intrabc_rect4_own_chroma444_hits(),
                0,
                "{NAME}: the 4:2:0 twin fired the ss (0,0)-only route"
            );

            // NO 4:4:4 pixel assert here, and deliberately so. The cell is
            // still divergent (390380 of 921600 samples; both chroma planes
            // first wrong at index 20532, x=52 y=32; entropy fork read 12465
            // at mi (90,108)) and an assertion of the form "our output is
            // wrong by exactly N" would encode the defect as expected
            // behaviour -- the shape this suite refuses everywhere else, and
            // one that outlives the memory of why. The true assertions this
            // gate can make today are the two above: the route is reached at
            // 4:4:4 and is not reached at 4:2:0, and the 4:2:0 twin is
            // byte-exact. The open chroma root and its numbers live in
            // lanes/av1444rect.report.md §6, not in an assertion.
            eprintln!(
                "{NAME}: {own} own-chroma intra-BC strip(s) at 4:4:4, 0 at 4:2:0; \
                 4:2:0 twin byte-exact; 4:4:4 intra-BC CHROMA still OPEN \
                 (U/V first wrong at index 20532, entropy fork read 12465 at \
                 mi (90,108)) -- see lanes/av1444rect.report.md \u{00a7}6"
            );
        }
    }

    /// lane-av1-444interband: the DEFAULT-partition lossless 4:4:4 pin for
    /// the inter 16x4/4x16 strip's chroma band stamping (the pair_chroma
    /// 4:2:0-only gate, commit d5325172). Fixture pinned into `fixtures/`:
    /// testsrc2 128x96 yuv444p, aomenc `--profile=1 --lossless=1
    /// --enable-palette=0 --enable-intrabc=0 --passes=1 --cpu-used=4
    /// --limit=6 --obu` (the av1llkf.report.md recipe), 6 frames; 32487
    /// bytes, sha256
    /// `1e6dd4e05038c8d51e2c8d29f8c337130c4fea0aed629415535e6e53af9a2c72`.
    ///
    /// This is the stream whose frame 1 desynced at the 16x4 inter strip
    /// mi(2,8) -- its U bc=0 `txb_skip` read base 0 where aom reads base 1,
    /// because the skipped strip (1,8)'s pair-form context stamp smeared
    /// zero over `left[2][1]`. Two pins, both measured at d5325172:
    /// end-to-end, every sample of every frame must match the oracle
    /// aomdec exactly (the pre-fix tree refuses mid-frame-1, so the gate
    /// is red by refusal, not by silent pass), and the 1:4 counter delta
    /// must show the strips decoded WITHOUT the pair rewrite firing
    /// (`horz4=20 vert4=28 chroma_pairs=0` -- chroma_pairs > 0 on this
    /// pairless 4:4:4 stream is the defect reappearing, the
    /// class `pair-rewrite-at-ss0`).
    #[test]
    fn a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact() {
        const NAME: &str = "a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact";
        const FIXTURE_LEN: usize = 32487;
        const FIXTURE_FNV: u64 = 0x5278_9f0f_ce2d_1e52;
        const W: usize = 128;
        const H: usize = 96;
        const FRAMES: usize = 6;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_defaultp_inter_strip.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        let strips_before = decode::inter16_rect4_counters();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let strips_after = decode::inter16_rect4_counters();
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }
        // The 1:4 strips must have fired, and the 4:2:0 pair rewrite must
        // NOT have (there are no chroma pairs at ss 0/0).
        let (horz4, vert4, chroma_pairs) = (
            strips_after.0 - strips_before.0,
            strips_after.1 - strips_before.1,
            strips_after.2 - strips_before.2,
        );
        assert_eq!(horz4, 20, "{NAME}: HORZ_4 strip count moved");
        assert_eq!(vert4, 28, "{NAME}: VERT_4 strip count moved");
        assert_eq!(
            chroma_pairs, 0,
            "{NAME}: chroma pairs closed on a 4:4:4 stream \
             (class pair-rewrite-at-ss0)"
        );

        // The oracle aomdec, rawvideo out: FRAMES concatenated yuv444p frames.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-ll444dp-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            for (i, f) in frames.iter().enumerate() {
                let base = i * W * H * 3;
                for ((plane, off), p) in [(&f.y, 0usize), (&f.u, W * H), (&f.v, 2 * W * H)]
                    .into_iter()
                    .zip(["y", "u", "v"])
                {
                    let want = &ref_raw[base + off..base + off + W * H];
                    let bad = plane
                        .iter()
                        .zip(want)
                        .filter(|&(&a, &b)| a as u8 != b)
                        .count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: aomdec frame {i} plane {p}: {bad} samples differ \
                         (class pair-rewrite-at-ss0)"
                    );
                }
            }
        }
    }

    /// lane-av1-leaf8oob: the OOB pin for [`decode_leaf8`]'s intrabc chroma
    /// frame-copy buffers. The closure hardcoding the leaf's chroma
    /// prediction at `4x4` (`vec![0u16; 4 * 4]`, `predict_with_filter` at
    /// `4, 4`) carried the 4:2:0 halving of an 8x8 luma leaf into every
    /// subsampling: at 4:4:4 the leaf's own chroma plane block is 8x8
    /// (`csz == 8`), so the 16-sample override reached
    /// [`PlaneBuf::reconstruct`] as an 8x8 prediction and indexed
    /// `prediction[16]` of a 16-sample buffer --
    /// `index out of bounds: the len is 16 but the index is 16` at the
    /// reconstruct call, on every conformant stream of this shape (a panic,
    /// never a refusal). Same signature class as lane-lossless128's
    /// 128-axis lossless extent (`len 16, index 16` there too).
    ///
    /// `crates/ec-av1/fixtures/444_leaf8_oob.obu`, 3577 bytes, sha256
    /// `72bed2791d1ceba8be7926a052c7838c5d96d4f9d56d28fb69789a753d3593af`
    /// (force-added past the `fixtures` gitignore like every sibling
    /// fixture): the lane ticket's witness-hunt reproducer (s1a/s2e) -- a
    /// 320x256, 3-frame, 8-bit 4:4:4 screen-content keyframe run whose
    /// census pins 4 intrabc 8x8 leaves (the oracle's own MB dump opens on
    /// unbroken inter-classified intra = intrabc blocks). The encoding
    /// recipe is not independently recoverable in this lane; the bytes are
    /// pinned by length + FNV-1a64 here and by the oracle raw fingerprint
    /// below.
    ///
    /// WHAT THIS GATE DOES NOT CLAIM: sample-exactness -- and it pins no
    /// pixel baseline. Measured against the oracle aomdec (`--rawvideo`;
    /// comparison method ground-truthed by the green
    /// `a_lossless_sb128_rect_intra_block_decodes_sample_exact` and
    /// `a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`
    /// witnesses on this same tree), this stream DIVERGES and its output is
    /// not even self-deterministic: frame 0's first differing LUMA sample
    /// is at (275,128), the (y, u, v) differing counts per frame are
    /// (20811, 35467, 32320)/(20308, 39177, 35994)/(20131, 39588, 36601) of
    /// 81920 samples per plane, and under `EC_AV1_PLANE_SENTINEL=1` the
    /// chroma counts move (u f0 35467 -> 35630) while luma stays put.
    /// Sentinel-vs-uninit output diffing localises that dependence to 458
    /// WHOLE 8x8 chroma blocks (frame 0: chroma x >= 96, y >= 64) reading
    /// samples this decode never wrote -- ambient memory in those slots,
    /// hence any pixel-exact assert here drifts run to run with the batch.
    /// Both facts belong to ONE pre-existing defect this lane must not
    /// chase (the ticket's fix touches only leaf8's chroma prediction
    /// buffers, which cannot feed a luma sample, and the pre-fix run never
    /// got past the panicked leaf): a decode desync on this stream around
    /// frame-0 SB (3..4, 2), after which samples diverge from the oracle
    /// and the misread layout leaves chroma blocks unreconstructed.
    /// Disposition: `deferred(unblock: divergence lane -- EC trace pairing
    /// against the oracle on this fixture)`. So this gate pins exactly what
    /// its ticket owns: fixture identity, a decode that RUNS TO COMPLETION
    /// where the old code died with `len 16, index 16`, and the arm counter.
    ///
    /// Assertions (all before/without any skip path):
    /// 1. fixture bytes: exact length + FNV-1a64;
    /// 2. `decode_stream` succeeds -- 3 frames, 320x256 (the pre-fix run
    ///    died here with the len-16/index-16 panic);
    /// 3. non-vacuity: `leaf8_intrabc_hits` moves (measured 0 -> 4), so the
    ///    arm owning the fixed buffers really fired;
    /// 4. the oracle aomdec still decodes the pinned bytes (raw size +
    ///    fingerprint), anchoring the deferred divergence comparison.
    #[test]
    fn an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob() {
        const NAME: &str = "an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob";
        const FIXTURE_LEN: usize = 3577;
        const FIXTURE_FNV: u64 = 0x86b9_3a51_fe16_353c;
        const W: usize = 320;
        const H: usize = 256;
        const FRAMES: usize = 3;
        let obu =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/444_leaf8_oob.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        crate::decode::reset_leaf8_intrabc_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        assert!(
            crate::decode::leaf8_intrabc_hits() > 0,
            "{NAME}: gate is vacuous -- no intrabc 8x8 leaf was decoded, so the arm \
             owning the fixed chroma buffers never fired"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }

        // The oracle aomdec, rawvideo out: FRAMES concatenated yuv444p frames.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-leaf8oob-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            assert_eq!(
                fnv1a64(&ref_raw),
                0x3821_0bcb_f572_973c,
                "{NAME}: aomdec raw fingerprint moved"
            );
        }
    }

    /// lane-av1p1pin: the permanent pin for the 4:4:4 superblock-128
    /// 128-root rect witness -- recipe P1 of `lanes/av1witness2recheck.report.md`.
    ///
    /// Provenance. Encoder: `~/.cache/aom-oracle/build/aomenc --codec=av1
    /// --obu -o s1d.obu --passes=1 --threads=1 bands444.y4m --limit=3
    /// --kf-max-dist=1 --cpu-used=2 --end-usage=q --cq-level=20 --sb-size=128
    /// --enable-rect-partitions=1 --enable-1to4-partitions=1
    /// --min-partition-size=4`, over the ffmpeg 8.1.2 lavfi source
    /// `nullsrc=s=320x256:rate=30:d=1,geq=lum='mod(floor((Y+T*160)/8)*67,255)':
    /// cb='128+20*sin(Y/17+T)':cr='128+20*sin(X/23+T*2)' -pix_fmt yuv444p`
    /// (`bands444.y4m`, sha256
    /// `fa6218399cce8dd9eaadb6e24642d31cb2d2bde505b39a8df237938492c642ee`).
    /// Pinned bytes: `crates/ec-av1/fixtures/444_sb128rect_chroma_tile_witness.obu`,
    /// 595 bytes, sha256
    /// `68773f2c6e01a2d211bf0213479accb4c2c360132c0e1bc3b326eab7861925b1`
    /// (FNV-1a `0xd4fa_ac7b_dccc_520b`). Rebuilt byte-for-byte from the recipe
    /// on `9a535ccb` -- the recipe did not drift. 3 frames, 320x256, yuv444p.
    /// This is NOT `fixtures/444_sb128rect_lr_witness.obu` (794 B, sha256
    /// `27825e14...`), the other in-tree 4:4:4 sb128 stream.
    ///
    /// What it guards. At the report's old base `fe3e8418` this stream
    /// panicked -- rc=101, `decode.rs:17631: assertion failed: lossless_frame
    /// || (chroma_w / chroma_tx, chroma_h / chroma_tx) == (nch_x, nch_y)` --
    /// because the 128-rect chroma unit walk was hardcoded to the 4:2:0 mu-chunk
    /// span, so a 128-root HORZ/VERT block at 4:4:4 coded half its chroma units.
    /// Closed by `349b3918` (the per-axis invariant now at `decode.rs:24515`);
    /// byte-exactness by `fa70a68c`.
    ///
    /// Two load-bearing arms, both proven to bite (lanes/av1p1pin.report.md):
    /// (1) the census deltas -- 6 gathered edge-VERT 128-roots, 6
    /// `PARTITION_VERT` 128 roots, 6 key-frame intra 128-axis blocks, 8 coded
    /// `32x32` VERT strips -- so the gate fails, rather than passes, if a
    /// future stream stops coding the shape (class `gate-blind-to-feature`);
    /// (2) every sample of every frame against ffmpeg `yuv444p` rawvideo, with
    /// the plane-length guard that stops a short plane from zip-truncating the
    /// compare. Oracle arm is aomdec `--rawvideo` as well; neither is a shim.
    #[test]
    fn a_444_sb128_rect_chroma_tile_witness_is_byte_exact() {
        const NAME: &str = "a_444_sb128_rect_chroma_tile_witness_is_byte_exact";
        const FIXTURE_LEN: usize = 595;
        const FIXTURE_FNV: u64 = 0xd4fa_ac7b_dccc_520b;
        const W: usize = 320;
        const H: usize = 256;
        const FRAMES: usize = 3;
        /// The guarded path: 128-root rect chroma tiling at 4:4:4.
        const EDGE_VERT: usize = 6;
        const PART128_VERT: usize = 6;
        const INTRA128_VERT: usize = 6;
        const RECT4_32_VERT: usize = 8;
        const RECT4_32_CODED: usize = 8;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_sb128rect_chroma_tile_witness.obu");
        let stream = read_pin(&obu, FIXTURE_LEN, FIXTURE_FNV, NAME);

        let _guard = lock_gate_counters();
        let rect_before = sb128_rect_counters();
        let part_before = part128_census();
        let r432_before = rect4_32_counters();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let rect_after = sb128_rect_counters();
        let part_after = part128_census();
        let r432_after = rect4_32_counters();

        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");
        for f in &frames {
            assert_eq!(
                (f.width, f.height),
                (W, H),
                "{NAME}: dimensions -- a resized frame would make the oracle \
                 slices below compare the wrong samples"
            );
        }

        // Non-vacuity: the guarded 128-root VERT path really fired.
        assert_eq!(
            rect_after.1 - rect_before.1,
            EDGE_VERT,
            "{NAME}: gathered edge-VERT 128 roots moved (expected {EDGE_VERT}) -- this \
             stream is the witness for the 128-rect 4:4:4 chroma tiling, so a \
             different count means the shape is gone or the counter moved (class \
             gate-blind-to-feature)"
        );
        assert_eq!(
            part_after.3 - part_before.3,
            PART128_VERT,
            "{NAME}: 128-root PARTITION_VERT blocks moved (expected {PART128_VERT})"
        );
        assert_eq!(
            part_after.6 - part_before.6,
            INTRA128_VERT,
            "{NAME}: key-frame intra 128-axis blocks moved (expected {INTRA128_VERT})"
        );
        assert_eq!(
            part_after.2 - part_before.2,
            0,
            "{NAME}: 128-root PARTITION_HORZ fired on this VERT witness"
        );
        assert_eq!(
            rect_after.0 - rect_before.0,
            0,
            "{NAME}: gathered edge-HORZ 128 roots fired on this VERT witness"
        );
        assert_eq!(
            (r432_after.1 - r432_before.1, r432_after.2 - r432_before.2),
            (RECT4_32_VERT, RECT4_32_CODED),
            "{NAME}: the 32x32 VERT strip counts moved -- the coded half of the \
             chroma tiling is what the old assert under-counted"
        );

        // Oracle 1: aomdec rawvideo.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-p1pin-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3 * FRAMES, "{NAME}: aomdec raw size");
            for (i, f) in frames.iter().enumerate() {
                let base = i * W * H * 3;
                for ((plane, off), p) in [(&f.y, 0usize), (&f.u, W * H), (&f.v, 2 * W * H)]
                    .into_iter()
                    .zip(["y", "u", "v"])
                {
                    let want = &ref_raw[base + off..base + off + W * H];
                    assert_eq!(
                        plane.len(),
                        want.len(),
                        "{NAME}: aomdec frame {i} plane {p}: ours has {} samples, the oracle's \
                         has {} -- a short plane would zip-truncate the compare below and \
                         hide a chroma-span defect",
                        plane.len(),
                        want.len()
                    );
                    let bad = plane
                        .iter()
                        .zip(want)
                        .filter(|&(&a, &b)| a as u8 != b)
                        .count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: aomdec frame {i} plane {p}: {bad} samples differ (class \
                         sb128-rect-chroma-tiling)"
                    );
                }
            }
        }

        // Oracle 2: ffmpeg yuv444p rawvideo. Hard-fails, never skips, when
        // EC_AV1_REQUIRE_FFMPEG=1 (see have_ffmpeg).
        if have_ffmpeg() {
            let refs = ffmpeg_decode_sequence_444(&stream, W, H, FRAMES);
            for (i, (got, want)) in frames.iter().zip(refs.iter()).enumerate() {
                for (p, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .iter()
                    .enumerate()
                {
                    assert_eq!(
                        g.len(),
                        r.len(),
                        "{NAME}: ffmpeg frame {i} plane {p}: ours has {} samples, ffmpeg's has \
                         {} -- a short plane would zip-truncate the compare below and hide a \
                         chroma-span defect",
                        g.len(),
                        r.len()
                    );
                    let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{NAME}: ffmpeg frame {i} plane {p}: {bad} samples differ (class \
                         sb128-rect-chroma-tiling)"
                    );
                }
            }
        } else {
            eprintln!("SKIP {NAME}: no ffmpeg -- the ffmpeg oracle arm only");
        }
    }

    /// lane-lossless128: a LOSSLESS key frame that codes a 128-axis
    /// (`BLOCK_64X128`) intra block -- the shape libaom's `read_tx_size` forces
    /// to `TX_4X4` on its very first line (`decodeframe.c:1183`), so every plane
    /// of the block is a 4x4 transform unit and NO `tx_size_cat3` symbol is
    /// coded.
    ///
    /// On the merged base `decode_block_128rect` had no lossless carve-out: it
    /// read `logical_tx = 64` off `depth == 0`, handed `PlaneBuf::reconstruct`
    /// a 64x64 geometry with the 16-sample 4x4 residual
    /// [`TxParams::run`](crate::decode::TxParams) produced -- `index out of
    /// bounds: the len is 16 but the index is 16` at the reconstruction call,
    /// on EVERY conformant stream of this shape (a panic, never a refusal).
    ///
    /// `crates/ec-av1/fixtures/lossless_sb128_rect_kf.obu`, 3471 bytes, sha256
    /// `013b07979c8be999c70d47f32768c2bf959be086a893f60dde1b0f5e35d4f475`:
    /// `aomenc --codec=av1 --passes=1 --threads=1 --obu --lossless=1
    /// --cpu-used=0` over a 320x256, 4-frame source that is flat grey except a
    /// textured 64x128 patch in the top-right corner -- which is what makes the
    /// 128 root take `PARTITION_VERT` and code the `BLOCK_64X128` at mi(0,64).
    ///
    /// The counter delta keeps the gate non-vacuous: a stream that stopped
    /// coding the shape reads 0 and the gate fails instead of passing on a
    /// decode that never reached the fixed body (class
    /// `gate-blind-to-feature`).
    #[test]
    fn a_lossless_sb128_rect_intra_block_decodes_sample_exact() {
        const NAME: &str = "a_lossless_sb128_rect_intra_block_decodes_sample_exact";
        // The pixel arm needs ffmpeg; the decode and its counter deltas do
        // not, so a box without ffmpeg still proves the shape was reached
        // (class `gate-skips-on-its-own-failure`) -- the skip is still named.
        let ffmpeg = have_ffmpeg();
        if !ffmpeg {
            eprintln!("SKIP {NAME}: no ffmpeg -- the ffmpeg pixel arm only");
        }
        let (w, h, frames) = (320usize, 256usize, 4usize);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/lossless_sb128_rect_kf.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let before = crate::decode::intra128_lossless_hits();
        let before_replay = crate::decode::sb128rect_chroma_replay_hits();
        let ours = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: a lossless 128-axis key frame decodes: {e}"));
        let hits = crate::decode::intra128_lossless_hits() - before;
        assert!(
            hits > 0,
            "{NAME}: this stream coded NO lossless 128-axis intra block -- the fixed body \
             was never reached (class gate-blind-to-feature)"
        );
        // lane-av1-128rectspan: the 4:2:0 twin of the 4:4:4 gate's replay
        // assertion. This stream codes the 64x128 block at mi(0,64) with
        // TX_4X4 chroma units, so the end-of-block replay re-stamps each of
        // them over its own luma-mi rows -- the arm the span fix sits on, on
        // the OTHER admitted ss (1,1). The mismatch half is the 4:2:2
        // signature; lane-av1422lift: 4:2:2 is decoded now, so this is no
        // longer "the tripwire for the day it is not". It is a zero this gate
        // asserts about the stream in front of it, and nothing else.
        // **DO NOT carry it to a 4:2:2 stream**: the two
        // `422_intrabc_sb128_strip*` pins read `mismatch = 9` each and are
        // byte-exact against ffmpeg, so a 4:2:2 zero assert would red on a
        // provably correct cell. The 4:2:2 rows of the corpus gate read
        // neither half of this counter.
        let after_replay = crate::decode::sb128rect_chroma_replay_hits();
        assert!(
            after_replay.0 > before_replay.0,
            "{NAME}: no 128-axis block replayed a chroma unit's own mi state -- the replay \
             arm never ran (class gate-blind-to-feature)"
        );
        assert_eq!(
            after_replay.1,
            before_replay.1,
            "{NAME}: the tail replay stamped {} unit(s) over a height that was not the \
             unit's own luma height (4:2:2 spans leaked into an admitted stream)",
            after_replay.1 - before_replay.1
        );
        eprintln!(
            "{NAME}: replayed {} chroma unit(s), {} span mismatch(es)",
            after_replay.0 - before_replay.0,
            after_replay.1 - before_replay.1
        );
        if !ffmpeg {
            return;
        }
        let refs = ffmpeg_decode_sequence(&stream, w, h, frames);
        assert_eq!(ours.len(), frames, "{NAME}: shown/display frame count");
        for (i, (got, want)) in ours.iter().zip(refs.iter()).enumerate() {
            for (plane, (g, r)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                .iter()
                .enumerate()
            {
                let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    bad, 0,
                    "{NAME}: frame {i} plane {plane}: {bad} samples differ from ffmpeg"
                );
                assert_eq!(
                    g.len(),
                    r.len(),
                    "{NAME}: frame {i} plane {plane}: ours has {} samples, ffmpeg's has {} -- a \
                     short plane would zip-truncate the compare above and hide a chroma-span \
                     defect",
                    g.len(),
                    r.len()
                );
            }
        }
        eprintln!("{NAME}: {frames} frames sample-exact, intra128_lossless hits {hits}");
    }

    /// lane-av1-loss64: a lossless superblock-128 frame whose intra blocks
    /// overhang the bottom frame edge codes NO coefficient symbols for the
    /// chroma transform units whose 4x4 origin sits outside the frame's mi
    /// grid -- libaom `decode_reconstruct_tx` returns before any read
    /// (`decodeframe.c:299`, `blk_row >= max_blocks_high || blk_col >=
    /// max_blocks_wide`, the limits from `max_block_wide/high`,
    /// av1_common_int.h:1565). The square multi-transform-unit chroma loop in
    /// [`crate::decode`] carried no such clip (the luma loop had one) and read
    /// `txb_skip`+coefficients for the phantom units: on the measured stream
    /// the first lossless 64x64 `NONE` block, at mi(48,0), overhangs the
    /// 240-px frame by 4 mi, so its 32x32 chroma codes SIX rows of 4x4 units,
    /// not eight -- 32 phantom unit reads per plane desynced the tile and
    /// every later symbol (including partition reads, which is what made
    /// aomdec's zero-`value=8/9` census look like a partition-reader defect
    /// in the rect14 review) decoded as garbage.
    ///
    /// Fail-pre-fix, measured on the ed12fe52 probe: both arms differ from
    /// ffmpeg at byte 41120 (luma row 128 col 160, the first block decoded
    /// after the phantom reads). The counter delta pins non-vacuity (class
    /// `gate-blind-to-feature`): 96 clipped units on each arm.
    #[test]
    fn a_lossless_sb128_square_frame_clips_overhanging_chroma_tus() {
        const NAME: &str = "a_lossless_sb128_square_frame_clips_overhanging_chroma_tus";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // Arm 0 is the rect14 report's `loss320` recipe verbatim; arm 1 is its
        // `lnone` companion (1:4 and rect partitions off) -- both coded the
        // overhanging 64x64 blocks and both desynced at byte 41120 pre-fix.
        let arms: [&[&str]; 2] = [
            &["--min-partition-size=4"],
            &[
                "--min-partition-size=4",
                "--enable-1to4-partitions=0",
                "--enable-rect-partitions=0",
            ],
        ];
        let mut clips_total = 0usize;
        for (arm, extra) in arms.iter().enumerate() {
            let stream = lossless_sb128_testsrc2(320, 240, extra);
            crate::decode::reset_loss64_hits();
            let ours =
                decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: arm {arm} decodes: {e}"));
            let clips = crate::decode::chroma_edge_tu_clip_hits();
            assert!(
                clips > 0,
                "{NAME}: arm {arm} clipped NO overhanging chroma transform unit -- \
                 the fixed body was never reached (class gate-blind-to-feature)"
            );
            clips_total += clips;
            let refs = ffmpeg_decode_sequence(&stream, 320, 240, 1);
            assert_eq!(ours.len(), 1, "{NAME}: arm {arm} frame count");
            for (plane, (g, r)) in [
                (&ours[0].y, &refs[0].y),
                (&ours[0].u, &refs[0].u),
                (&ours[0].v, &refs[0].v),
            ]
            .iter()
            .enumerate()
            {
                let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    bad, 0,
                    "{NAME}: arm {arm} plane {plane}: {bad} samples differ from ffmpeg"
                );
                assert_eq!(
                    g.len(),
                    r.len(),
                    "{NAME}: arm {arm} plane {plane}: ours has {} samples, ffmpeg's has {} -- a \
                     short plane would zip-truncate the compare above and hide a chroma-span \
                     defect",
                    g.len(),
                    r.len()
                );
            }
        }
        eprintln!("{NAME}: 2 arms full-frame exact, {clips_total} chroma units clipped");
    }

    /// lane-av1-loss64: a SKIPPED intrabc rect strip on a lossless frame
    /// zeroes its whole entropy-band footprint (libaom
    /// `av1_reset_entropy_context`, `if (mbmi->skip_txfm)` decodeframe.c:1262 ->
    /// blockd.c:58) instead of leaving the previous occupant's luma bands
    /// standing. The skip arm of `decode_intrabc_owned_rect` never wrote the
    /// luma bands: the lossless `read_block_tx_size_rect` resolves the 4x4
    /// leaf list before it looks at `skip`, so the tail took the
    /// per-transform-unit record path (chroma only) with no per-unit luma
    /// writes ever run. The next block then read a STALE band: on the 320x242
    /// `--min-partition-size=4` sibling of this stream the skipped 8x16
    /// intrabc strip at mi(48,66) left a 7 in left[48] and mi(48,68)'s first
    /// transform unit read `txb_skip_ctx` 3 where aomdec reads 1 (measured
    /// `EC_ECDUMP` against the instrumented aomdec) -- a coefficient-walk
    /// desync at byte 61712 with every mode symbol still matching.
    ///
    /// This gate uses the same `--min-partition-size=4` arm that reaches the
    /// sub-8x8 VERT -> 4x8 skipped-intrabc leaf. Post-fix it is full-frame
    /// exact: the skip path uses libaom's clamped 4x4 chroma intrabc
    /// prediction (`decodeframe.c:1011-1065`, `reconinter_template.inc:186-207`)
    /// instead of the ordinary skipped-intra DC predictor. The old route first
    /// differed at U(104,92) with the entropy ladder fully synced.
    ///
    /// Fail-pre-fix, measured on the ed12fe52 probe: differs at byte 41120
    /// (the chroma-clip class, fixed in the same lane). Single-mutation on
    /// the lane tree with ONLY this gate's walk reverted: differs at byte
    /// 61712, the band-leak site.
    #[test]
    fn a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands() {
        const NAME: &str = "a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let stream = lossless_sb128_testsrc2(320, 242, &["--min-partition-size=4"]);
        crate::decode::reset_loss64_hits();
        crate::decode::reset_skipped_intrabc_chroma_arm_hits();
        let ours = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: decodes: {e}"));
        let resets = crate::decode::skip_lossless_band_reset_hits();
        let intrabc_skips = crate::decode::skipped_intrabc_chroma_arm_hits();
        assert!(
            resets > 0,
            "{NAME}: this stream coded NO skipped lossless intrabc rect strip -- \
             the fixed body was never reached (class gate-blind-to-feature)"
        );
        assert!(
            intrabc_skips > 0,
            "{NAME}: this stream coded NO skipped sub-8x8 intrabc chroma leaf -- \
             the clamped chroma prediction fix was never reached (class gate-blind-to-feature)"
        );
        let refs = ffmpeg_decode_sequence(&stream, 320, 242, 1);
        assert_eq!(ours.len(), 1, "{NAME}: frame count");
        for (plane, (g, r)) in [
            (&ours[0].y, &refs[0].y),
            (&ours[0].u, &refs[0].u),
            (&ours[0].v, &refs[0].v),
        ]
        .iter()
        .enumerate()
        {
            let bad = g.iter().zip(r.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(
                bad, 0,
                "{NAME}: plane {plane}: {bad} samples differ from ffmpeg"
            );
            assert_eq!(
                g.len(),
                r.len(),
                "{NAME}: plane {plane}: ours has {} samples, ffmpeg's has {} -- a short plane \
                 would zip-truncate the compare above and hide a chroma-span defect",
                g.len(),
                r.len()
            );
        }
        eprintln!(
            "{NAME}: full-frame exact, {resets} band resets, \
             {intrabc_skips} skipped intrabc chroma predictions"
        );
    }

    /// lane-av1-ibc444rect: an unskipped intrabc `BLOCK_8X4`/`BLOCK_4X8` leaf
    /// on a lossless 4:4:4 frame walks its coefficients as per-4x4 inter
    /// `TX_4X4` units on every plane -- `read_tx_size`'s lossless first line
    /// (`decodeframe.c:1203`) forces TX_4X4 before any tree, the frame's
    /// `TxMode` is `ONLY_4X4` so no var-tx tree is read either, and
    /// `av1_read_tx_type`'s qindex==0 early return (`decodemv.c:681`) drops
    /// the tx_type symbol the lossy 4x4 inter sets carry -- NOT the single
    /// `LumaRect8x4Inter`/`ChromaRect8x4` rect unit the non-lossless arm
    /// reads, and not the lossy per-unit arm's tx_type read.
    ///
    /// The pinned fixture `ll444_ibc_rect.obu` (99614 bytes, provenance in
    /// `lanes/av1ibc444rect.report.md`) is a lossless `--profile=1 --i444
    /// --enable-intrabc=1` key frame. r1 pinned the (then-known) leaf8
    /// `palette_window` panic as its alignment witness; with that blocker
    /// fixed upstream (lane-av1leaf8oob) the frame decodes to completion and
    /// this gate is the FULL-FRAME exact `aomdec` comparison its r1 doc
    /// promised, flipped by lane-av1-ibcskip2 -- which also closed the
    /// SKIPPED intrabc sub8 chroma cells the r1 measurement called out: a
    /// skipped intrabc leaf's chroma is the bare frame copy at the DV
    /// (libaom's `av1_build_inter_predictors_sb` runs regardless of `skip`),
    /// never the intra-DC push that arm used to run. Fail-before, measured on
    /// this tree with the DV-copy arm cut back to the intra push: 2794 U +
    /// 2830 V samples differ across 180 distinct 4x4 cells, 175 of them
    /// exactly this frame's skipped intrabc leaves, luma clean; with the arm,
    /// 0 differing samples on every plane.
    #[test]
    fn a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units() {
        const NAME: &str = "a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units";
        const FIXTURE_LEN: usize = 99614;
        const FIXTURE_FNV: u64 = 0xa1b6_a84e_ac6a_656f;
        const W: usize = 320;
        const H: usize = 240;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_ibc_rect.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let _guard = lock_gate_counters();
        crate::decode::reset_ibc444rect_hits();
        crate::decode::reset_skipped_intrabc_dv_copy_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!(
                "{NAME}: the fixture must decode to completion (the leaf8 \
                    chroma buffers were fixed upstream): {e}"
            )
        });
        assert_eq!(frames.len(), 1, "{NAME}: shown frame count");
        let luma = crate::decode::intrabc_rect8_lossless_luma_hits();
        let chroma = crate::decode::intrabc_rect8_lossless_chroma_hits();
        assert_eq!(
            (luma, chroma),
            (339, 339),
            "{NAME}: the per-4x4 walk must fire on the fixture's full unskipped \
             intrabc rect census (got luma {luma}, chroma {chroma}) -- zero \
             means the fixture no longer reaches the fixed body (class \
             gate-blind-to-feature), more means the entropy desynced and \
             garbage blocks read as intrabc rect leaves"
        );
        let dv_skips = crate::decode::skipped_intrabc_dv_copy_hits();
        assert_eq!(
            dv_skips, 150,
            "{NAME}: the skipped-intrabc DV-copy chroma arm must fire on every \
             one of the fixture's 150 skipped intrabc leaves (got {dv_skips}) \
             -- zero means the arm is unreachable (class gate-blind-to-feature)"
        );

        // The oracle aomdec, rawvideo out: one concatenated yuv444p frame.
        if aomdec_available(NAME) {
            let dir = std::env::temp_dir().join(format!("ec-av1-ibcskip2-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let raw = dir.join("aomdec.raw");
            let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
                .args(["--codec=av1", "--rawvideo", "-o"])
                .arg(&raw)
                .arg(&obu)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("aomdec failed to run");
            assert!(
                out.status.success(),
                "{NAME}: the oracle aomdec refused the stream: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(ref_raw.len(), W * H * 3, "{NAME}: aomdec raw size");
            let f = &frames[0];
            for (plane, off, p) in [
                (&f.y, 0usize, "Y"),
                (&f.u, W * H, "U"),
                (&f.v, 2 * W * H, "V"),
            ] {
                let want = &ref_raw[off..off + W * H];
                let bad = plane
                    .iter()
                    .zip(want)
                    .filter(|&(&a, &b)| a as u8 != b)
                    .count();
                assert_eq!(
                    bad, 0,
                    "{NAME}: aomdec plane {p}: {bad} samples differ -- a \
                     regression here reopens the skipped-intrabc sub8 chroma \
                     predictor or the rect walk"
                );
            }
        }
        eprintln!(
            "{NAME}: frame sample-exact vs oracle aomdec; {luma}+{chroma} rect \
             walks, {dv_skips} skipped-intrabc DV copies"
        );
    }

    /// lane-dkey: the palette-neighbour-band gate. A palette-Y block's size and
    /// colours used to stay standing in the mi-granular above/left bands after a
    /// sub-8x8 group was decoded next to it -- the sub-8x8 readers published
    /// skip/lf/mode state but no palette state, while libaom's mi grid carries
    /// `palette_size == 0` in exactly those cells (`av1_allow_palette` needs
    /// BLOCK_8X8 and up). The next intra block then read `palette_y_mode` off
    /// CDF row 1 where libaom reads row 0 (class `cdf-row-held-constant`),
    /// decoded a palette libaom never coded, and skipped the `use_filter_intra`
    /// symbol a palette block has none of: one symbol short, tile desynced.
    ///
    /// Measured on the instrumented aomdec at KEY-frame block mi(76,88) of this
    /// very stream (`EC_PALSYN_AOM ... ctx=0 y=0` against our `mode_ctx=1`).
    /// Without the fix every one of the 12 frames differs from ffmpeg's decode
    /// from luma sample 491904 on.
    ///
    /// The source is SYNTHETIC (no film sample lives in `fixtures`): the dark,
    /// near-flat background libaom's own screen-content detector counts as
    /// screen content (2..=4 distinct luma values per 16x16 block), which is
    /// what turns palette on, plus one textured patch at a HALF-PEL position on
    /// half the frames so the encoder splits that region down to 4x4 groups
    /// beside its palette blocks.
    #[test]
    fn a_libaom_screen_stream_with_sub8_groups_beside_palette_blocks_decodes_exact() {
        const NAME: &str =
            "a_libaom_screen_stream_with_sub8_groups_beside_palette_blocks_decodes_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let (w, h, frames) = (1920usize, 1024usize, 12usize);
        let mut raw = Vec::with_capacity(frames * w * h * 3 / 2);
        for f in 0..frames {
            let mut y = vec![0u8; w * h];
            for row in 0..h {
                for col in 0..w {
                    y[row * w + col] =
                        16 + ((col / 32 + row / 32) % 5) as u8 + ((col / 4 + row / 4) % 3) as u8;
                }
            }
            // 2.5 px per pair of frames: integer on one pair, half a pixel off
            // on the next (linear interpolation), so the patch's edges land
            // mid-sample and the encoder splits there.
            let x0 = 200.0 + 2.5 * (f / 2) as f64;
            let (fx, ix) = (x0.fract(), x0.floor() as usize);
            for row in 300..428 {
                for col in 0..192 {
                    let tex = |u: f64| -> f64 {
                        let v = (row - 300) as f64;
                        30.0 + 100.0 * (0.5 + 0.5 * ((u * 0.35).sin() * (v * 0.21).cos()))
                    };
                    let s = tex(col as f64) * (1.0 - fx) + tex(col as f64 - 1.0) * fx;
                    y[row * w + ix + col] = s.round().clamp(0.0, 255.0) as u8;
                }
            }
            raw.extend_from_slice(&y);
            raw.extend(std::iter::repeat_n(128u8, w * h / 2));
        }
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let src = std::env::temp_dir().join(format!("ec-av1-palsub8-{}.yuv", std::process::id()));
        std::fs::write(&src, &raw).expect("raw source");
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .args(["-s", &format!("{w}x{h}"), "-r", "24", "-i"])
            .arg(&src)
            .args(["-an", "-threads", "1", "-g", "12", "-c:v", "libaom-av1"])
            .args([
                "-cpu-used",
                "6",
                "-b:v",
                "0",
                "-crf",
                "35",
                "-f",
                "obu",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&src);
        if !out.status.success() {
            eprintln!(
                "SKIP {NAME}: ffmpeg has no libaom-av1 encoder ({})",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let stream = out.stdout;
        let ours = decode_stream(&stream).expect("libaom screen-content stream decodes");
        let theirs = ffmpeg_decode_sequence(&stream, w, h, frames);
        assert_eq!(ours.len(), frames, "{NAME}: shown frame count");
        for (i, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
            for (plane, l, r) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
                let first = l.iter().zip(r.iter()).position(|(x, y)| x != y);
                assert!(
                    first.is_none(),
                    "{NAME}: frame {i} plane {plane} differs from ffmpeg at sample {}",
                    first.unwrap()
                );
                assert_eq!(
                    l.len(),
                    r.len(),
                    "{NAME}: frame {i} plane {plane}: ours has {} samples, ffmpeg's has {} -- a \
                     short plane would zip-truncate the compare above and hide a chroma-span \
                     defect",
                    l.len(),
                    r.len()
                );
            }
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
            &gradients_source(42, width, height, "rate=1"),
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
            &gradients_source(42, width, height, "rate=1"),
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
