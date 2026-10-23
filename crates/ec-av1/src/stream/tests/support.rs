use super::*;
use super::gates_a::*;

    pub(super) fn test_card(width: usize, height: usize) -> Pic {
        let mut picture = Pic::grey(width, height);
        for row in 0..height {
            for col in 0..width {
                picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u16;
            }
        }
        for row in 0..height / 2 {
            for col in 0..width / 2 {
                let i = row * width / 2 + col;
                picture.u[i] = (100 + (col * 60 / (width / 2).max(1))) as u16;
                picture.v[i] = (200 - (row * 80 / (height / 2).max(1))) as u16;
            }
        }
        picture
    }

    pub(super) fn panned_test_card(width: usize, height: usize, shift: i64) -> Pic {
        let mut picture = Pic::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
                let gradient = sx * 200.0 / width as f64;
                picture.y[y * width + x] = (20.0 + gradient).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
            }
        }
        picture
    }

    /// One census attempt: encode `frames` frames of `source` with real
    /// aomenc plus `extra` args, decode with THIS decoder, and return the
    /// stream's byte length with either the picture count or the refusal (any
    /// decode error) it produced. A census is about what the encoder emits,
    /// so a refusal is data here, not a failure.
    pub(super) fn census_attempt(
        source: &str,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
    ) -> (usize, std::result::Result<usize, String>) {
        let duration = format!("{}", frames as f64 / 25.0);
        let src = format!("{source}=size={width}x{height}:rate=25");
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &src,
                "-t",
                &duration,
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
            "ffmpeg refused the {width}x{height} fixture"
        );
        let limit = format!("--limit={frames}");
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
        args.push(limit);
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
        let stream = out.stdout;
        let result = match decode_stream(&stream) {
            Ok(pictures) => Ok(pictures.len()),
            Err(e) => Err(e.to_string()),
        };
        (stream.len(), result)
    }

    /// lane-av1seglvl: encode `frames` frames of `source` with real aomenc
    /// plus `extra` args and return the raw `.obu` bytes -- the census's
    /// half-sibling of [`census_attempt`] that keeps the STREAM instead of
    /// only its decode verdict, so the caller can parse the frame headers'
    /// segmentation tables themselves.
    pub(super) fn encode_aomenc_stream(
        source: &str,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
    ) -> Vec<u8> {
        let duration = format!("{}", frames as f64 / 25.0);
        let src = format!("{source}=size={width}x{height}:rate=25");
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &src,
                "-t",
                &duration,
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
            "ffmpeg refused the {width}x{height} fixture"
        );
        let limit = format!("--limit={frames}");
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
        args.push(limit);
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
    }

    /// lane-av1seglvl: encode `frames` frames with a real TWO-PASS aomenc
    /// (pass 1 writes the stats file into the session temp dir, pass 2
    /// consumes it) and return the `.obu` bytes. This is the only shape that
    /// reaches libaom's `is_stat_consumption_stage_twopass` segmentation
    /// setup (`configure_static_seg_features`), so the census is not proven
    /// over the encoder's whole reachable surface without it.
    pub(super) fn encode_aomenc_stream_twopass(
        source: &str,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
    ) -> Vec<u8> {
        let duration = format!("{}", frames as f64 / 25.0);
        let src = format!("{source}=size={width}x{height}:rate=25");
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &src,
                "-t",
                &duration,
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
            "ffmpeg refused the {width}x{height} fixture"
        );
        let limit = format!("--limit={frames}");
        let fpf =
            std::env::temp_dir().join(format!("ec-av1-segldapass-{}.fpf", std::process::id()));
        let mut base: Vec<String> = [
            "--codec=av1",
            "--passes=2",
            "--end-usage=q",
            "--threads=1",
            "--row-mt=0",
            "--kf-max-dist=9999",
            "--lag-in-frames=0",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        base.push(limit);
        base.extend(extra.iter().map(|s| (*s).to_owned()));
        let mut pass1 = base.clone();
        pass1.extend(["--pass=1".to_owned(), format!("--fpf={}", fpf.display())]);
        pass1.extend([
            "--obu".to_owned(),
            "-o".to_owned(),
            "/dev/null".to_owned(),
            "-".to_owned(),
        ]);
        let out1 = run_with_stdin(Command::new(aomenc_path()).args(&pass1), &y4m.stdout);
        assert!(
            out1.status.success(),
            "aomenc pass 1 refused {pass1:?}: {}",
            String::from_utf8_lossy(&out1.stderr)
        );
        let mut pass2 = base;
        pass2.extend(["--pass=2".to_owned(), format!("--fpf={}", fpf.display())]);
        pass2.extend([
            "--obu".to_owned(),
            "-o".to_owned(),
            "-".to_owned(),
            "-".to_owned(),
        ]);
        let out2 = run_with_stdin(Command::new(aomenc_path()).args(&pass2), &y4m.stdout);
        let _ = std::fs::remove_file(&fpf);
        assert!(
            out2.status.success(),
            "aomenc pass 2 refused {pass2:?}: {}",
            String::from_utf8_lossy(&out2.stderr)
        );
        out2.stdout
    }

    /// lane-av1seglvl: walk every frame OBU of `stream` and tally which
    /// `SEG_LVL` features the frame headers actually ENABLE, plus the shape
    /// counters a census premise is asserted against: frames with
    /// segmentation on, of which `update_map == 1` (the map is coded) and
    /// `update_map == 0` (every block's id is INHERITED), and frames whose
    /// table carries a real `SEG_LVL_ALT_Q` delta.
    /// The last vector counts per SEGMENT-FEATURE INSTANCE (a frame with
    /// ALT_Q on all 8 segments contributes 8), which is all the verdict
    /// needs: only index `SEG_LVL_ALT_Q` may be non-zero.
    /// Returns `(enabled_frames, update_map_frames, inherited_frames,
    /// alt_q_frames, feature_instances: [usize; SEG_LVL_MAX])`.
    #[allow(clippy::type_complexity)]
    pub(super) fn segmentation_feature_census(
        stream: &[u8],
    ) -> (usize, usize, usize, usize, [usize; SEG_LVL_MAX]) {
        let mut parser = Av1Parser::new();
        let obus = parser
            .parse_temporal_unit(stream)
            .unwrap_or_else(|e| panic!("the census stream does not even parse: {e}"));
        let mut enabled = 0usize;
        let mut update_map = 0usize;
        let mut inherited = 0usize;
        let mut alt_q = 0usize;
        let mut feature_frames = [0usize; SEG_LVL_MAX];
        for obu in &obus {
            if let ObuKind::Frame(header, _) = &obu.kind {
                let seg = &header.segmentation;
                if !seg.enabled {
                    continue;
                }
                enabled += 1;
                if seg.update_map {
                    update_map += 1;
                } else {
                    inherited += 1;
                }
                let mut any_q = false;
                for segment in 0..MAX_SEGMENTS {
                    // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
                    #[allow(clippy::needless_range_loop)]
                    for feature in 0..SEG_LVL_MAX {
                        if seg.feature_enabled[segment][feature] {
                            feature_frames[feature] += 1;
                            if feature == SEG_LVL_ALT_Q {
                                any_q = true;
                            }
                        }
                    }
                }
                if any_q {
                    alt_q += 1;
                }
            }
        }
        (enabled, update_map, inherited, alt_q, feature_frames)
    }

    /// lane-interedge r1: one attempt of the inter frame-edge half-strip gate --
    /// encode `width x height` with real aomenc (rect partitions ON, every other
    /// lane's open tool pinned off), decode it, and return
    /// `(inter_edge_strip hits, frames whose LUMA differs, frames whose CHROMA
    /// differs)` against ffmpeg. A refusal or a decode error is a hard failure:
    /// this recipe reached every size without one (measured r1).
    pub(super) fn inter_edge_strip_attempt(
        name: &str,
        width: usize,
        height: usize,
        bit_depth: u32,
        cq: u32,
        frames: usize,
    ) -> ([usize; 6], usize, usize) {
        let duration = format!("{}", frames as f64 / 25.0);
        let source = format!("mandelbrot=size={width}x{height}:rate=25");
        let pix_fmt = if bit_depth == 10 {
            "yuv420p10le"
        } else {
            "yuv420p"
        };
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-t",
                &duration,
                "-pix_fmt",
                pix_fmt,
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
            "{name}: ffmpeg refused the {width}x{height} {bit_depth}-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let depth_arg = format!("--bit-depth={bit_depth}");
        let input_depth_arg = format!("--input-bit-depth={bit_depth}");
        let cq_arg = format!("--cq-level={cq}");
        let limit_arg = format!("--limit={frames}");
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq_arg,
                // cpu-used >= 1 keeps libaom on 64x64 superblocks at these
                // sizes (`av1_select_sb_size`).
                "--cpu-used=4",
                "--threads=1",
                "--row-mt=0",
                "--kf-max-dist=9999",
                &limit_arg,
                // decode order == display order, so the compare below covers
                // every frame the stream carries (class gate-blind-to-hidden-frames).
                "--lag-in-frames=0",
                &depth_arg,
                &input_depth_arg,
                // Rect partitions stay ON: the frame-edge strip is a FORCED
                // (gathered) HORZ/VERT, but with rect coding off the encoder
                // answers that gathered symbol SPLIT every time and no edge
                // strip ever fires (lane-oddh r2's finding, re-measured r1).
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=8",
                "--max-partition-size=64",
                // Other lanes' open gaps this recipe must not trip.
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-ref-frame-mvs=0",
                "--enable-angle-delta=0",
                "--enable-cfl-intra=0",
                "--enable-directional-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-filter-intra=0",
                "--enable-tx-size-search=0",
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

        // Reset, never a delta: a refused attempt's hits must not leak into the
        // next one (class counter-from-refused-stream).
        crate::decode::reset_inter_edge_strip_hits();
        let decoded = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{name}: {width}x{height} {bit_depth}-bit cq {cq} did not decode: {e}")
        });
        let hits = crate::decode::inter_edge_strip_hits();
        let reference = if bit_depth == 10 {
            ffmpeg_decode_sequence_10bit(&stream, width, height, frames)
        } else {
            ffmpeg_decode_sequence(&stream, width, height, frames)
        };
        assert_eq!(
            decoded.len(),
            reference.len(),
            "{name}: {width}x{height} {bit_depth}-bit cq {cq} frame count"
        );
        assert!(
            decoded.len() >= frames,
            "{name}: {width}x{height} {bit_depth}-bit cq {cq} decoded only {} frames",
            decoded.len()
        );
        let (mut luma_bad, mut chroma_bad) = (0usize, 0usize);
        for (ours, theirs) in decoded.iter().zip(&reference) {
            if ours.y != theirs.y {
                luma_bad += 1;
            }
            if ours.u != theirs.u || ours.v != theirs.v {
                chroma_bad += 1;
            }
        }
        (hits, luma_bad, chroma_bad)
    }

    /// A GOP (key frame plus panned inter frames, so blocks actually take
    /// motion) decodes bit-exact through `decode_stream` alone, threading its
    /// own DPB rather than a test-supplied reference.
    pub(super) fn gop_round_trips(width: usize, height: usize, fctx: &crate::decode::FrameCtx) {
        let pictures: Vec<_> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
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

    /// Firing-detection gates (single-ref envelope, temporal-MV, `comp_mode`,
    /// `skip_mode`/compound) all read a process-global atomic counter delta
    /// (`decode::ref_hits`/`tmv_hits`/`comp_mode_hits`/`skip_mode_hits`) to
    /// prove a real block picked the feature under test, not just that a
    /// header bit was set. Under `cargo test`'s default parallel run, a
    /// SIBLING test's own decode bumps the same global counter mid-gate --
    /// a false "fired" reading pixel-compares an attempt the gate would
    /// otherwise `continue` past, on top of genuine defects that path may
    /// still have. Serialising every counter-delta gate behind this one
    /// mutex makes firing detection honest again (matches the existing
    /// cdf-forwarding-gate flake shape, ledger `omp-liveness-pgrep-self-match`).
    pub(super) static GATE_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// lane-cdfflake: a cheap, dependency-free (FNV-1a, not cryptographic --
    /// this is a load-bearing-identity fingerprint for flake triage, not a
    /// security digest, so no `sha2` dep is pulled in) fixture fingerprint
    /// for the cdf-forwarding gate's instrumentation below.
    /// Reads a pinned fixture, asserting its length and FNV first, so a
    /// swapped or truncated pin fails loudly instead of silently changing
    /// what the gate measures.
    pub(super) fn read_pin(path: &std::path::Path, len: usize, fnv: u64, name: &str) -> Vec<u8> {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            panic!(
                "{name}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                path.display()
            )
        });
        assert_eq!(bytes.len(), len, "{name}: fixture length moved");
        assert_eq!(fnv1a64(&bytes), fnv, "{name}: fixture bytes moved");
        bytes
    }

    /// The bit depth the stream's OWN sequence header declares, read from
    /// the bytes rather than taken from the gate's name or recipe (class
    /// `helper-packs-u16-for-an-8-bit-stream`: aomdec's `--rawvideo`
    /// writes 8 bits per sample for an 8-bit stream and 16 bits LE for a
    /// high-bitdepth one, so a u16 packer turns every 8-bit compare into a
    /// raw-SIZE failure that never looks at a sample).
    pub(super) fn stream_bit_depth(stream: &[u8], name: &str) -> u8 {
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: OBU at byte {pos} does not parse: {e:?}"));
            pos += obu.total_size;
        }
        probe
            .sequence_header()
            .unwrap_or_else(|| panic!("{name}: the stream carries no sequence header OBU"))
            .color_config
            .bit_depth
    }

    /// The stream's OWN `color_config.subsampling_x/y` (spec 5.5.2), read
    /// from its bytes the same way [`stream_bit_depth`] reads the depth.
    pub(super) fn stream_subsampling(stream: &[u8], name: &str) -> (u8, u8) {
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: OBU at byte {pos} does not parse: {e:?}"));
            pos += obu.total_size;
        }
        let cc = &probe
            .sequence_header()
            .unwrap_or_else(|| panic!("{name}: the stream carries no sequence header OBU"))
            .color_config;
        (cc.subsampling_x, cc.subsampling_y)
    }

    /// The chroma shape a `(subsampling_x, subsampling_y)` pair means.
    pub(super) fn chroma_shape(sx: u8, sy: u8) -> &'static str {
        match (sx, sy) {
            (1, 1) => "4:2:0",
            (1, 0) => "4:2:2",
            (0, 0) => "4:4:4",
            (0, 1) => "4:4:0",
            _ => "an unassigned subsampling",
        }
    }

    /// lane-av1oracleguard: HARD REFUSAL for the 4:2:0 ffmpeg oracle helpers.
    ///
    /// `ffmpeg_decode_sequence`, `ffmpeg_decode_sequence_10bit` and
    /// `ffmpeg_decode_sequence_12bit` ask ffmpeg for `-pix_fmt yuv420p*` and
    /// size chroma at `width * height / 4`. Those are the 4:2:0 numbers, so
    /// for a stream that is NOT (1,1) ffmpeg does not fail -- it CONVERTS,
    /// and the helper then slices a 4:2:0-shaped picture out of it. A 4:2:2
    /// gate built on one of them compares the wrong planes and passes green:
    /// the exact failure `lanes/av1subsizesweep.report.md` names. The correct
    /// siblings (`ffmpeg_decode_sequence_422` / `_422_depth` /
    /// `ffmpeg_decode_sequence_444`) exist and take the same bytes, so the
    /// refusal points at them by name rather than leaving the caller to
    /// rediscover the shape.
    ///
    /// Refused by SUBSAMPLING read from the stream's own sequence header, not
    /// from the caller's `width`/`height` (class
    /// `helper-assumes-the-shape-the-gate-name-implies`).
    pub(super) fn assert_420_oracle_stream(stream: &[u8], helper: &str, sibling: &str) {
        let (sx, sy) = stream_subsampling(stream, helper);
        assert!(
            (sx, sy) == (1, 1),
            "{helper} REFUSED a ({sx},{sy}) stream: this helper asks ffmpeg for a 4:2:0 \
             pix_fmt, which CONVERTS the stream's real {} chroma down to 4:2:0 instead of \
             failing, then sizes chroma at `width * height / 4` rather than the {} shape. A \
             gate built on it would compare converted planes against real ones and pass green. \
             Use `{sibling}` for a ({sx},{sy}) stream.",
            chroma_shape(sx, sy),
            chroma_shape(sx, sy),
        );
    }

    /// lane-av1oracleguard: run `f`, return the panic MESSAGE it raised (or
    /// `None` if it did not panic). The panic hook is silenced so a refusal
    /// that PASSES its gate prints nothing -- the assertion is the return.
    pub(super) fn panic_message<F: FnOnce()>(f: F) -> Option<String> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::panic::set_hook(hook);
        match caught {
            Ok(()) => None,
            Err(payload) => Some(
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "<non-string panic payload>".to_string()),
            ),
        }
    }

    /// Packs decoded planes the way `aomdec --rawvideo` writes them for
    /// `bit_depth`: 1 byte per sample at 8-bit, 2 bytes little-endian at
    /// 10/12-bit. Plane order is Y, U, V at the frame's own subsampled
    /// shape, which is exactly what rawvideo emits.
    pub(super) fn pack_rawvideo(decoded: &[Pic], bit_depth: u8, name: &str) -> Vec<u8> {
        let mut ours: Vec<u8> = Vec::new();
        for (fi, f) in decoded.iter().enumerate() {
            for p in [&f.y, &f.u, &f.v] {
                if bit_depth == 8 {
                    for &s in p {
                        assert!(
                            s <= 0xff,
                            "{name}: decode-order frame {fi} carries the sample {s}, which an \
                             8-bit stream cannot hold -- the stream is not the one this gate thinks \
                             it is"
                        );
                        ours.push(s as u8);
                    }
                } else {
                    for &s in p {
                        ours.extend_from_slice(&s.to_le_bytes());
                    }
                }
            }
        }
        ours
    }

    /// Decodes `stream` and the oracle's `aomdec --rawvideo` output and
    /// requires every sample of every frame to agree, in decode order.
    ///
    /// `bit_depth` is the depth the CALLER believes the stream has; it is
    /// asserted against the stream's own sequence header, so a gate that
    /// hands this an 8-bit stream under a 10-bit label fails here by name
    /// instead of dying on a raw-size mismatch that reads like a pixel bug.
    pub(super) fn assert_rawvideo_matches(
        obu: &std::path::Path,
        stream: &[u8],
        name: &str,
        frames: usize,
        bit_depth: u8,
    ) {
        let actual_depth = stream_bit_depth(stream, name);
        assert_eq!(
            actual_depth, bit_depth,
            "{name}: the stream's sequence header says {actual_depth}-bit, the gate passed \
             {bit_depth}-bit -- aomdec's --rawvideo writes {actual_depth} bits per sample for it, \
             so packing for {bit_depth} compares nothing"
        );
        let dir = std::env::temp_dir().join(format!("ec-av1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let raw = dir.join("aomdec.raw");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "--rawvideo", "-o"])
            .arg(&raw)
            .arg(obu)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{name}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ref_raw = std::fs::read(&raw).expect("aomdec rawvideo output");
        let _ = std::fs::remove_dir_all(&dir);
        let decoded = decode_stream(stream).expect("decode for comparison");
        let ours = pack_rawvideo(&decoded, bit_depth, name);
        assert_eq!(
            ours.len(),
            ref_raw.len(),
            "{name}: {bit_depth}-bit raw sizes differ -- ours packs {} bytes ({} samples at \
             {} bit/sample), aomdec wrote {}",
            ours.len(),
            decoded
                .iter()
                .map(|f| f.y.len() + f.u.len() + f.v.len())
                .sum::<usize>(),
            bit_depth,
            ref_raw.len()
        );
        assert_eq!(decoded.len(), frames, "{name}: frame count");
        let first = ours
            .iter()
            .zip(&ref_raw)
            .position(|(a, b)| a != b)
            .unwrap_or(ours.len());
        assert_eq!(
            first,
            ours.len(),
            "{name}: {} bytes of decoded output differ from aomdec (first at {})",
            ours.iter().zip(&ref_raw).filter(|(a, b)| a != b).count(),
            first
        );
    }

    pub(super) fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Whether ffmpeg is on PATH. Absence normally SKIPs, but
    /// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
    /// aomenc gate decodes its stream through ffmpeg and is meaningless
    /// without it -- turns it into a hard failure. Without this the require
    /// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
    /// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
    /// printed SKIP and reported green (class gate-skips-on-its-own-failure).
    pub(super) fn have_ffmpeg() -> bool {
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

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    /// The frame-count diagnostic shared by the three `ffmpeg_decode_sequence*`
    /// helpers (lane-av1pins5 r6).
    ///
    /// Before this, a caller that passed its OWN decode's length as the expected
    /// count got `expected N 4:2:0 frames, ffmpeg said: ` -- with an EMPTY tail,
    /// because ffmpeg ran at `-v error` and succeeded. The message named ffmpeg,
    /// said nothing about where `N` came from, and sent the next reader hunting
    /// ffmpeg for a count ffmpeg never chose.
    ///
    /// The class is MISATTRIBUTED RED, not silent pass: the helper's
    /// `out.stdout.len() == frame_bytes * frames` assert fires unconditionally, so
    /// a wrong count cannot pass -- it just fails while blaming the wrong
    /// component. (Measured: `ffmpeg_decode_sequence(&s, 192, 128, 0)` panics
    /// here, not silently.) Naming the real cause is the whole fix; no caller has
    /// to change to benefit.
    pub(super) fn frame_count_diagnosis(
        format: &str,
        frames: usize,
        width: usize,
        height: usize,
        frame_bytes: usize,
        got: usize,
        stderr: &[u8],
    ) -> String {
        let actual = got.checked_div(frame_bytes).unwrap_or(0);
        let leftover = if frame_bytes == 0 {
            got
        } else {
            got % frame_bytes
        };
        format!(
            "FRAME COUNT MISMATCH ({format} {width}x{height}): the caller passed \
             `frames={frames}`, ffmpeg produced {actual} frame(s) ({got} bytes, \
             {leftover} byte(s) not a whole frame, at {frame_bytes} B/frame).\n\
             `frames` is the EXPECTED count the CALLER asserted -- it is NOT measured \
             from ffmpeg, so it never had ffmpeg's agreement. If it came from our own \
             decode_stream(), this is OUR decoder disagreeing with ffmpeg about how many \
             frames the stream holds, and the fix is on OUR side, not ffmpeg's. ffmpeg \
             itself exited cleanly{}.",
            if stderr.is_empty() {
                " with no diagnostics (`-v error`)"
            } else {
                ""
            }
        )
    }

    pub(super) fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        // lane-av1oracleguard: this helper's numbers are 4:2:0's. A (1,0) or
        // (0,0) stream would be CONVERTED by ffmpeg rather than refused.
        assert_420_oracle_stream(
            stream,
            "ffmpeg_decode_sequence",
            "ffmpeg_decode_sequence_422 (4:2:2) or ffmpeg_decode_sequence_444 (4:4:4)",
        );
        let out = run_with_stdin(
            Command::new(ffmpeg_path()).args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ]),
            stream,
        );
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
            "{}",
            frame_count_diagnosis(
                "4:2:0",
                frames,
                width,
                height,
                frame_bytes,
                out.stdout.len(),
                &out.stderr
            )
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Pic {
                    width,
                    height,
                    y: out.stdout[base..base + luma]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                    u: out.stdout[base + luma..base + luma + chroma]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                }
            })
            .collect()
    }

    /// As [`ffmpeg_decode_sequence`], for a MONOCHROME stream: one 8-bit luma
    /// plane per frame, exactly what `ffmpeg -pix_fmt gray -f rawvideo` writes
    /// (lane-av1mono). Fed through [`run_with_stdin`] for the reason its doc
    /// records.
    pub(super) fn ffmpeg_decode_gray_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<u8> {
        let out = run_with_stdin(
            Command::new(ffmpeg_path()).args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "gray", "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the monochrome stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout.len(),
            width * height * frames,
            "expected {frames} monochrome frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// As [`ffmpeg_decode_sequence`], but for a `yuv420p10le` stream: samples
    /// are 2-byte little-endian, one full 16-bit range regardless of the
    /// stream's real 10-bit depth (ffmpeg's rawvideo muxer never packs to the
    /// bit depth). The 8-bit helper above stays untouched -- every existing
    /// gate depends on it.
    ///
    /// lane-av1oracledepth: the stream's OWN sequence header must parse
    /// 10-bit, the same way `assert_12bit_sequence_header` covers the 12-bit
    /// gates. A wrong-depth stream handed here would otherwise CONVERT
    /// silently -- ffmpeg re-scales 8-bit samples into the 16-bit container
    /// and sizes no plane differently -- so a gate could compare ffmpeg's
    /// rescaled 8-bit planes against our 10-bit decode and read exact (class
    /// `helper-assumes-the-shape-the-gate-name-implies`, the depth axis).
    pub(super) fn ffmpeg_decode_sequence_10bit(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        assert_420_oracle_stream(
            stream,
            "ffmpeg_decode_sequence_10bit",
            "ffmpeg_decode_sequence_422_depth(.., 10, \"yuv422p10le\") (4:2:2); \
             ffmpeg_decode_sequence_444_depth(.., 10, \"yuv444p10le\") (4:4:4)",
        );
        assert_eq!(
            stream_bit_depth(stream, "ffmpeg_decode_sequence_10bit"),
            10,
            "ffmpeg_decode_sequence_10bit REFUSED a stream whose own sequence header is not \
             10-bit -- ffmpeg converts it to yuv420p10le instead of failing, and the comparison \
             would read a converted reference"
        );
        let out = run_with_stdin(
            Command::new(ffmpeg_path()).args([
                "-v",
                "error",
                "-f",
                "obu",
                "-i",
                "-",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p10le",
                "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_samples = luma + 2 * chroma;
        let frame_bytes = frame_samples * 2;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "{}",
            frame_count_diagnosis(
                "4:2:0 10-bit",
                frames,
                width,
                height,
                frame_bytes,
                out.stdout.len(),
                &out.stderr
            )
        );
        fn le16(bytes: &[u8]) -> Vec<u16> {
            bytes
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        }
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Pic {
                    width,
                    height,
                    y: le16(&out.stdout[base..base + luma * 2]),
                    u: le16(&out.stdout[base + luma * 2..base + luma * 2 + chroma * 2]),
                    v: le16(&out.stdout[base + luma * 2 + chroma * 2..base + frame_bytes]),
                }
            })
            .collect()
    }

    /// Builds a `gradients` lavfi source string with colours derived from
    /// `seed`, not left to the filter's own default.
    ///
    /// Measured (lane-fixdet r1): ffmpeg's `gradients` source IGNORES its
    /// `seed=` parameter for colour selection -- five encodes of an
    /// identical command line (same `seed=`, same everything) produced five
    /// different fixtures, because the unset `c0..c9` stops are randomized
    /// from something other than that seed. Every one of this file's ~20
    /// gate fixtures built a `gradients` source this way, so none of them
    /// had a reproducible input: a pin captured from one run could not be
    /// regenerated, and "attempt N failed" was as likely to be a different
    /// fixture as a different decode outcome.
    ///
    /// Freezing the colours outright (as the earliest four gates below do,
    /// hand-picking `c0=red:c1=blue:c2=green`) fixes reproducibility but
    /// throws away the per-attempt content variety the sweep gates need to
    /// make features (OBMC, warp, wedge, ...) fire at all -- this repo's own
    /// `sampler-decorrelated-gate` class. So: derive `c0..c3` deterministically
    /// FROM the seed with a plain integer hash, and keep passing `seed=`
    /// itself too, so a future ffmpeg that actually honours it only adds
    /// more variety, never removes the determinism this buys.
    pub(super) fn gradients_source(seed: u32, width: usize, height: usize, tail: &str) -> String {
        fn hash_color(seed: u32, salt: u32) -> String {
            let h = seed
                .wrapping_mul(2_654_435_761)
                .wrapping_add(salt.wrapping_mul(0x9E37_79B9))
                ^ (seed.rotate_left(13));
            format!("0x{:06x}", h & 0x00ff_ffff)
        }
        let (c0, c1, c2, c3) = (
            hash_color(seed, 0),
            hash_color(seed, 1),
            hash_color(seed, 2),
            hash_color(seed, 3),
        );
        format!(
            "gradients=size={width}x{height}:c0={c0}:c1={c1}:c2={c2}:c3={c3}:seed={seed}:{tail}"
        )
    }

    /// Encodes a real libaom-av1 stream (via ffmpeg's own encoder, not this
    /// crate's) with the same reduced-partition flags lane-av1adst's r6
    /// probe found still select `ADST_ADST`/`ADST_DCT`/`DCT_ADST` on
    /// gradient content (TX_SET_INTRA_2), and decodes it with
    /// [`decode_stream`] -- lifting the round-2 refusal is only real if it
    /// gets past an encoder this crate never wrote a byte of.
    pub(super) fn libaom_encode(lavfi: &str, width: usize, height: usize, crf: u32) -> Option<Vec<u8>> {
        libaom_encode_with(lavfi, width, height, crf, &[])
    }

    /// As [`libaom_encode`], with extra encoder args appended -- used to pin
    /// a deterministic stream (`-threads 1` etc., since libaom's `cpu-used 8`
    /// RD search is not otherwise stable run-to-run on this box, and lavfi
    /// `gradients` needs its own `seed=` for the same reason) and to gate
    /// individual feature flags (`-enable-cdef 0`) a given fixture needs off.
    pub(super) fn libaom_encode_with(
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
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
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

    /// As [`ffmpeg_decode_sequence`], for a 4:4:4 stream: chroma planes are
    /// FULL resolution (`yuv444p` rawvideo), the shape lane-av1-llinter's
    /// lossless profile-1 fixture codes.
    pub(super) fn ffmpeg_decode_sequence_444(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        ffmpeg_decode_sequence_444_depth(stream, width, height, frames, 8, "yuv444p")
    }

    /// lane-av1oracledepth: the depth-generic 4:4:4 oracle comparator -- the
    /// [`ffmpeg_decode_sequence_422_depth`] twin for full-resolution chroma.
    /// `yuv444p`, `yuv444p10le` or `yuv444p12le`, with the container width
    /// taken from `depth` itself (8 -> one byte per sample; 10/12 -> a 16-bit
    /// LE container on BOTH sides, so ours and ffmpeg's are directly
    /// comparable sample for sample and nothing narrows). Until this helper
    /// existed a 10/12-bit 4:4:4 gate had to fall back to the 8-bit
    /// `ffmpeg_decode_sequence_444`, which ffmpeg converts at `yuv444p`
    /// instead of refusing, or to a raw hand-rolled spawn (class
    /// `helper-assumes-the-shape-the-gate-name-implies`, the depth axis).
    ///
    /// Refused by SHAPE read from the stream's own sequence header, like its
    /// 4:2:0 siblings: `(0,0)` only, so a 4:2:2 or 4:2:0 stream handed here
    /// cannot have ffmpeg silently convert it.
    pub(super) fn ffmpeg_decode_sequence_444_depth(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
        depth: u8,
        pix_fmt: &str,
    ) -> Vec<Pic> {
        assert!(
            matches!(depth, 8 | 10 | 12),
            "lane-av1oracledepth: ffmpeg_decode_sequence_444_depth takes 8, 10 or 12, got {depth}"
        );
        let (sx, sy) = stream_subsampling(stream, "ffmpeg_decode_sequence_444_depth");
        assert!(
            (sx, sy) == (0, 0),
            "ffmpeg_decode_sequence_444_depth REFUSED a ({sx},{sy}) stream: this helper asks \
             ffmpeg for a 4:4:4 pix_fmt, which CONVERTS the stream's real {} chroma instead of \
             failing, then slices full-resolution planes from converted bytes. \
             ffmpeg_decode_sequence_422_depth handles (1,0); ffmpeg_decode_sequence handles \
             (1,1).",
            chroma_shape(sx, sy)
        );
        assert_eq!(
            stream_bit_depth(stream, "ffmpeg_decode_sequence_444_depth"),
            depth,
            "ffmpeg_decode_sequence_444_depth REFUSED a stream whose own sequence header is \
             not {depth}-bit -- ffmpeg would convert it to {pix_fmt} instead of failing"
        );
        let out = run_with_stdin(
            Command::new(ffmpeg_path()).args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the 4:4:4 stream ({pix_fmt}): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let plane = width * height;
        let bps = if depth == 8 { 1 } else { 2 };
        let frame_bytes = 3 * plane * bps;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "{}",
            frame_count_diagnosis(
                if depth == 8 {
                    "4:4:4"
                } else {
                    "4:4:4 high depth"
                },
                frames,
                width,
                height,
                frame_bytes,
                out.stdout.len(),
                &out.stderr
            )
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                let samples = |off: usize| -> Vec<u16> {
                    let slice = &out.stdout[base + off..base + off + plane * bps];
                    if bps == 1 {
                        slice.iter().map(|&v| u16::from(v)).collect()
                    } else {
                        slice
                            .as_chunks::<2>().0.iter()
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect()
                    }
                };
                Pic {
                    width,
                    height,
                    y: samples(0),
                    u: samples(plane * bps),
                    v: samples(2 * plane * bps),
                }
            })
            .collect()
    }

    /// lane-av1422lift: as [`ffmpeg_decode_sequence`] (4:2:0 8-bit) and
    /// [`ffmpeg_decode_sequence_444`], for a 4:2:2 stream: `yuv422p`
    /// rawvideo, whose chroma planes are HALF WIDTH and FULL HEIGHT -- the
    /// `w/2 * h` shape the whole per-axis port exists to produce. A helper
    /// that computed the chroma span 4:2:0-style would make every 4:2:2
    /// gate compare half the oracle's bytes and pass, so the span is
    /// asserted against the byte count below, not assumed.
    pub(super) fn ffmpeg_decode_sequence_422(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        ffmpeg_decode_sequence_422_depth(stream, width, height, frames, 8, "yuv422p")
    }

    /// lane-av1422lift: the depth-generic 4:2:2 oracle comparator --
    /// `yuv422p`, `yuv422p10le` or `yuv422p12le`, with the container width
    /// taken from `depth` itself (8 -> one byte per sample; 10/12 -> a
    /// 16-bit LE container on BOTH sides, so ours and ffmpeg's are directly
    /// comparable sample for sample and nothing narrows).
    pub(super) fn ffmpeg_decode_sequence_422_depth(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
        depth: u8,
        pix_fmt: &str,
    ) -> Vec<Pic> {
        assert!(
            matches!(depth, 8 | 10 | 12),
            "lane-av1422lift: ffmpeg_decode_sequence_422_depth takes 8, 10 or 12, got {depth}"
        );
        let out = run_with_stdin(
            Command::new("ffmpeg").args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the 4:2:2 stream ({pix_fmt}): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width.div_ceil(2) * height);
        let bps = if depth == 8 { 1 } else { 2 };
        let frame_bytes = (luma + 2 * chroma) * bps;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "{}",
            frame_count_diagnosis(
                "4:2:2",
                frames,
                width,
                height,
                frame_bytes,
                out.stdout.len(),
                &out.stderr
            )
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                let samples = |off: usize, n: usize| -> Vec<u16> {
                    let slice = &out.stdout[base + off..base + off + n * bps];
                    if bps == 1 {
                        slice.iter().map(|&v| u16::from(v)).collect()
                    } else {
                        slice
                            .as_chunks::<2>().0.iter()
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect()
                    }
                };
                Pic {
                    width,
                    height,
                    y: samples(0, luma),
                    u: samples(luma * bps, chroma),
                    v: samples((luma + chroma) * bps, chroma),
                }
            })
            .collect()
    }

    /// lane-av1422lift: the shared body of every 4:2:2 exactness gate this
    /// lane adds, and the answer to "a byte-exact 4:2:2 gate can still be
    /// vacuous".
    ///
    /// Before lane-av1422census422 the per-plane coefficient-unit census was
    /// armed at `ss_x == 0 && ss_y == 0` only, so on a 4:2:2 stream
    /// `census_nonsub_units()` read `[0, 0, 0]` BY CONSTRUCTION -- a gate
    /// could have been green while the decode walked nothing at all. The
    /// census is now armed for `(ss_x == 0 || ss_y == 0)`, so the delta
    /// asserted here is a real measurement of the walk this stream caused:
    /// every plane must show a non-zero number of transform units AND of
    /// units that coded at least one coefficient.
    ///
    /// Four things are asserted per frame, in this order:
    ///
    /// 1. the decoded frame's dimensions, and that its chroma planes are
    ///    `round_ss(w, 1) * round_ss(h, 0)` -- half width, FULL height. A
    ///    decoder that kept 4:2:0 plane allocation while walking 4:2:2
    ///    entropy would pass a per-sample compare against a 4:2:0-sized
    ///    oracle; this is the shape assertion that closes that hole, and it
    ///    is asserted against the DECODED frame, never against the gate's
    ///    own name.
    /// 2. the depth, read out of the stream's own sequence header.
    /// 3. per-plane, per-frame byte-exactness against ffmpeg's rawvideo of
    ///    the same bytes at the depth-correct pixel format. Frame N of ours
    ///    against frame N of ffmpeg's: `decode_stream` returns DISPLAY order
    ///    and so does ffmpeg's rawvideo muxer, so a hidden alt-ref does not
    ///    shift the pairing (the decode-order-vs-display-order trap the
    ///    out-of-process corpus comparator has to solve separately).
    /// 4. the census delta, asserted once for the whole stream rather than
    ///    per frame, because the counters are cumulative per thread.
    ///
    ///    `require_coded` is false ONLY for a cell whose identity IS that it
    ///    codes nothing -- `422_allskip_2f.obu`, whose whole point is that
    ///    the inter frame reads no residual and the pre-port decoder read
    ///    PHANTOM units there. Demanding coded units on it would be
    ///    demanding the defect; its caller asserts the all-skip property
    ///    directly instead.
    pub(super) fn assert_422_stream_pixel_exact(
        name: &str,
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
        depth: u8,
        require_coded: bool,
    ) -> ([usize; 3], [usize; 3]) {
        let _gate_lock = lock_gate_counters();
        assert_eq!(
            stream_bit_depth(stream, name),
            depth,
            "{name}: the stream's OWN sequence header does not say {depth} bit"
        );
        let before = crate::decode::census_nonsub_units();
        let before_coded = crate::decode::census_nonsub_coded();
        let ours = decode_stream(stream)
            .unwrap_or_else(|e| panic!("{name}: decode_stream refused a real 4:2:2 stream: {e}"));
        assert_eq!(ours.len(), frames, "{name}: decoded frame count");
        for (i, f) in ours.iter().enumerate() {
            assert_eq!(
                (f.width, f.height),
                (width, height),
                "{name}: frame {i} dimensions"
            );
            assert_eq!(
                (f.u.len(), f.v.len()),
                (width.div_ceil(2) * height, width.div_ceil(2) * height),
                "{name}: frame {i} chroma planes are {}/{} samples -- 4:2:2 is HALF WIDTH and \
                 FULL HEIGHT ({width}/2 x {height}); a quarter-height plane means the decode kept \
                 a 4:2:0 allocation",
                f.u.len(),
                f.v.len()
            );
        }
        let pix_fmt = match depth {
            8 => "yuv422p",
            10 => "yuv422p10le",
            12 => "yuv422p12le",
            other => panic!("{name}: no ffmpeg pixel format mapped for depth {other}"),
        };
        let theirs =
            ffmpeg_decode_sequence_422_depth(stream, width, height, frames, depth, pix_fmt);
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            for (plane, ours_plane, theirs_plane) in
                [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)]
            {
                assert_eq!(
                    ours_plane.len(),
                    theirs_plane.len(),
                    "{name}: {plane} plane {i} is {} samples against ffmpeg's {} -- the 4:2:2 chroma \
                     span is wrong on one of the two sides",
                    ours_plane.len(),
                    theirs_plane.len()
                );
                if let Some(bad) = ours_plane
                    .iter()
                    .zip(theirs_plane)
                    .position(|(x, y)| x != y)
                {
                    panic!(
                        "{name}: {plane} differs from ffmpeg at frame {i} sample {bad} ({} vs {}; \
                         {} of {} samples differ)",
                        ours_plane[bad],
                        theirs_plane[bad],
                        ours_plane
                            .iter()
                            .zip(theirs_plane)
                            .filter(|(x, y)| x != y)
                            .count(),
                        ours_plane.len()
                    );
                }
            }
        }
        let units = crate::decode::census_nonsub_units();
        let coded = crate::decode::census_nonsub_coded();
        let delta: [usize; 3] = std::array::from_fn(|p| units[p] - before[p]);
        let coded_delta: [usize; 3] = std::array::from_fn(|p| coded[p] - before_coded[p]);
        for (p, plane) in ["luma", "U", "V"].into_iter().enumerate() {
            assert!(
                delta[p] > 0,
                "{name}: the {plane} plane read ZERO coefficient units across the whole stream -- \
                 this byte-exact compare is vacuous with respect to 4:2:2 (class \
                 gate-blind-to-feature)"
            );
            if require_coded {
                assert!(
                    coded_delta[p] > 0,
                    "{name}: the {plane} plane coded ZERO coefficient units across the whole \
                     stream -- every unit took the all-zero early return, so no coefficient path \
                     was exercised"
                );
            }
        }
        eprintln!(
            "{name}: byte-exact vs ffmpeg ({pix_fmt}, {frames} frames); census delta \
             units=[{}, {}, {}] coded=[{}, {}, {}]",
            delta[0], delta[1], delta[2], coded_delta[0], coded_delta[1], coded_delta[2]
        );
        (delta, coded_delta)
    }

    /// `(subsampling_x, subsampling_y, bit_depth)` out of the stream's OWN
    /// sequence header. A gate that claims to exercise a subsampling-specific
    /// path must assert this from the parsed header, not from the recipe that
    /// produced the fixture: the recipe can drift (or a wrong `--pix_fmt`
    /// argument can silently produce a different subsampling) and every other
    /// assertion would then pass vacuously.
    pub(super) fn subsampled_header_shape(stream: &[u8], name: &str) -> (u8, u8, usize) {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut shape = None;
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            if let ObuKind::SequenceHeader(seq) = obu.kind {
                shape = Some((
                    seq.color_config.subsampling_x,
                    seq.color_config.subsampling_y,
                    seq.color_config.bit_depth as usize,
                ));
            }
        }
        shape.unwrap_or_else(|| panic!("{name}: no sequence header parsed"))
    }

    /// lane-av1gates444: the live 4:4:4 LOSSY cell the chroma-format sweep
    /// measured EXACT 4/4 and left unpinned. The recipe is the sweep's own
    /// §6.2 encode, reproduced LIVE against the oracle `aomenc` rather than
    /// from a pin, because the claim these three gates carry is a property
    /// of the *decoder* at a geometry, not of a blob:
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=<W>x<H>:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=20 \
    ///        --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 \
    ///        --kf-max-dist=100 --limit=4 --obu -o - -
    /// ```
    /// `rate=25` is load-bearing (testsrc2's pattern is time-parameterised)
    /// and `--limit=4` is the sweep's frame count: at `--limit=6` the same
    /// recipe emits 7674 / 11465 / 19121 bytes instead of the sweep's
    /// 5938 / 8945 / 15239, i.e. a different stream than the one measured.
    /// `--profile=1` is what makes the source 4:4:4 at all.
    ///
    /// Every assertion here is a non-vacuity assertion first and a pixel
    /// compare second. The frame count, the per-frame geometry and the
    /// FULL-RESOLUTION chroma plane length are asserted because a truncated
    /// or 4:2:0-shaped decode can pass a lenient compare; the
    /// `rect4_inter_own_chroma444_hits()` reach assert is there because the
    /// class these cells belong to is the 4:4:4-only own-chroma-extent route
    /// in [`decode_inter_block`]'s `around_c` (a 4:2:0 stream of the same
    /// content leaves it at 0), and a counter at 0 would mean the gate
    /// measured a different route than it names.
    /// The guard is [`have_aomenc`], which probes first and asserts LAST with
    /// the env escape: with `EC_AV1_REQUIRE_AOMENC` set, a missing oracle is
    /// a hard failure naming the path it looked for, and a batch run cannot
    /// report green off three skipped gates. With it unset the `None` below is
    /// a developer-only convenience for a checkout without an oracle -- it is
    /// the ONLY way this gate can return without measuring, and it announces
    /// itself on stderr naming the env var that would make it a failure.
    /// (It used to return an empty `Vec` that the caller turned into a silent
    /// early `return`: the same green, reached through a second exit nobody
    /// was reading.)
    pub(super) fn encode_444_lossy_live(name: &str, w: usize, h: usize) -> Option<Vec<u8>> {
        if !have_aomenc() {
            eprintln!(
                "SKIP {name}: no aomenc oracle at {} -- set EC_AV1_REQUIRE_AOMENC=1 to make this \
                 a failure, or run scripts/build-aom-oracle.sh",
                aomenc_path().display()
            );
            return None;
        }
        let source = format!("testsrc2=size={w}x{h}:rate=25");
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-frames:v",
                "4",
                "-pix_fmt",
                "yuv444p",
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
            "{name}: ffmpeg refused the {w}x{h} 4:4:4 source: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--profile=1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=20",
                "--cpu-used=2",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--kf-max-dist=100",
                "--limit=4",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "{name}: aomenc refused the {w}x{h} 4:4:4 lossy recipe: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stdout.is_empty(),
            "{name}: aomenc produced no OBU for {w}x{h} -- a zero-length stream would \
             decode to nothing and the compare below would compare nothing"
        );
        Some(out.stdout)
    }

    /// lane-av1-loss64: the generator for the two gates below -- a deterministic
    /// 1-frame lossless superblock-128 aomenc stream of `testsrc2`. `extra`
    /// flags land last (aomenc keeps the LAST occurrence of a repeated flag).
    /// The recipe is the one lanes/av1rect14.report.md section 2 measured
    /// (`--lossless=1 --sb-size=128 --enable-1to4-partitions=1`); the
    /// `--min-partition-size` override is what steers the bottom superblock
    /// row's RD into overhanging 64x64 `NONE` blocks (the shape the first fix
    /// clips) or, at 8, keeps sub-8x8 partitions out of the second gate's
    /// stream so the whole frame is decodable.
    pub(super) fn lossless_sb128_testsrc2(width: usize, height: usize, extra: &[&str]) -> Vec<u8> {
        let mut ff = Command::new("ffmpeg");
        let out = crate::tool_cache::cached_output_io(ff
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s={width}x{height}:r=25"))
            .args([
                "-frames:v",
                "1",
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
            out.status.success(),
            "ffmpeg failed for testsrc2 {width}x{height}"
        );
        let y4m = out.stdout;
        let out = run_with_stdin(
            Command::new(aomenc_path())
                .args(["--codec=av1", "--bit-depth=8", "--input-bit-depth=8"])
                .args([
                    "--passes=1",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--kf-max-dist=1",
                    "--limit=1",
                    "--threads=1",
                    "--tile-columns=0",
                    "--lossless=1",
                    "--sb-size=128",
                    "--enable-1to4-partitions=1",
                ])
                .args(extra)
                .args(["--obu", "-o", "-", "-"]),
            &y4m,
        );
        assert!(
            out.status.success(),
            "aomenc failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
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
    pub(super) fn aomenc_path() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("EC_AV1_AOMENC") {
            return std::path::PathBuf::from(p);
        }
        // The oracle used to live only under `/tmp`, which this box's tmpfs
        // guard reclaims -- when it did, all 20 aomenc gates below went
        // silently vacuous (SKIP, suite still green). The durable location
        // is provisioned by `scripts/build-aom-oracle.sh`; the `/tmp` path
        // stays last as a legacy fallback.
        let mut candidates = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(std::path::PathBuf::from(&home).join(".cache/aom-oracle/build/aomenc"));
        }
        candidates.push("/tmp/libaom-src/build/encoder/aomenc".into());
        candidates
            .iter()
            .find(|p| p.is_file())
            .cloned()
            .unwrap_or_else(|| candidates.remove(0))
    }

    /// Whether the aomenc oracle is present. Absence normally SKIPs (a
    /// checkout without the oracle should still run the rest of the suite),
    /// but `EC_AV1_REQUIRE_AOMENC=1` turns it into a hard failure so a batch
    /// run cannot report green off 20 skipped gates.
    /// Where a pinned stream lives. `fixtures/` is a symlink to a durable
    /// directory outside the repo, so a pin captured in one session is still
    /// there in the next; a session scratchpad is not (tmpfs reaps it, and
    /// four pins were silently dead because of that).
    pub(super) fn pin_dir() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("EC_AV1_PIN_DIR") {
            return std::path::PathBuf::from(p);
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("fixtures"))
    }

    /// A COMMITTED pin: `crates/ec-av1/fixtures/<name>`, which travels in the
    /// tarball and is therefore present on every runner. `pin_dir()` resolves
    /// to the gitignored root `fixtures/`, so a pin written there is lost when
    /// the machine's scratchpad is reaped -- three gates sat at `rc=0` having
    /// tested nothing for exactly that reason (lane-av1pins). Falls back to
    /// `pin_dir()` so a locally re-captured stream still overrides.
    /// Read a pin that a gate cannot proceed without. A missing pin is a
    /// HARD failure naming the path and the generator, never a raw io panic
    /// and never a silent skip: `EC_AV1_GATE_DUMP_PIN` points the gate at a
    /// freshly captured stream, and a runner without the committed copy must
    /// say so instead of reporting green having tested nothing.
    pub(super) fn require_pin(path: &std::path::Path, name: &str) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|e| {
            panic!(
                "no pinned bytes at {} ({e}) -- set EC_AV1_GATE_DUMP_PIN to a freshly \
             captured stream, or restore the committed copy at \
             crates/ec-av1/fixtures/{name}",
                path.display()
            )
        })
    }

    pub(super) fn crate_pin(name: &str) -> std::path::PathBuf {
        let in_crate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        if in_crate.is_file() {
            in_crate
        } else {
            pin_dir().join(name)
        }
    }

    pub(super) fn have_aomenc() -> bool {
        let present = aomenc_path().is_file();
        assert!(
            present || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
            "EC_AV1_REQUIRE_AOMENC is set but no aomenc at {} -- run scripts/build-aom-oracle.sh",
            aomenc_path().display()
        );
        present
    }

    /// lane-av1oracleskip: the probe for the ORACLE COMPARER, in the same
    /// shape as [`have_ffmpeg`] -- probe first, then `assert!` LAST, with the
    /// env escape -- and it prints its own SKIP so a caller cannot skip the
    /// compare silently.
    ///
    /// Why this exists: thirteen gates wrapped their oracle arm in a bare
    /// `if aomdec_path().is_file()`, and that shape had NO env escape at all.
    /// An oracle build with `aomenc` but no `aomdec` beside it (or an
    /// `EC_AV1_AOMDEC` override pointing at a moved path) made the gate
    /// encode, decode, assert its geometry and its counters, and then quietly
    /// never compare -- green having measured nothing, with no line in the
    /// output to notice. Nine of the thirteen printed a SKIP in an `else` and
    /// four printed nothing at all; both are the same defect, and both are
    /// closed by one probe.
    ///
    /// With `EC_AV1_REQUIRE_AOMENC` (or the aomdec-specific
    /// `EC_AV1_REQUIRE_AOMDEC`) set, the absence is a HARD failure naming the
    /// path that was looked for -- that is the batch-run guarantee, and it is
    /// what the bare `is_file()` could not give. With it unset the SKIP below
    /// is printed and the gate still passes: that remains a developer-only
    /// convenience for a checkout without an oracle, never evidence.
    pub(super) fn aomdec_available(name: &str) -> bool {
        let present = aomdec_path().is_file();
        let require = std::env::var_os("EC_AV1_REQUIRE_AOMDEC").is_some()
            || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_some();
        assert!(
            present || !require,
            "{name}: no oracle aomdec at {} -- the pixel compare this gate exists for would be \
             skipped, and EC_AV1_REQUIRE_AOMDEC/EC_AV1_REQUIRE_AOMENC is set. Build the oracle \
             with scripts/build-aom-oracle.sh, or point EC_AV1_AOMDEC at a real aomdec.",
            aomdec_path().display()
        );
        if !present {
            eprintln!(
                "SKIP {name}: no oracle aomdec at {} -- the pixel compare is NOT running (set \
                 EC_AV1_REQUIRE_AOMDEC=1 to make this a failure)",
                aomdec_path().display()
            );
        }
        present
    }

    // lane-av1cmpaudit2: this thread's oracle override, set by
    // `set_oracle_path` and consulted by `aomdec_path` ahead of the env var.
    //
    // It is thread-local for the same reason `FINAL_DUMP_PREFIX` is: the
    // control test `every_oracle_comparator_reds_on_a_one_byte_wrong_oracle`
    // has to hand the helpers a DELIBERATELY WRONG oracle, and a
    // process-global `std::env::set_var` could not do that safely -- the
    // crate forbids `unsafe_code`, and even where it did not, one test's
    // tampered oracle would leak into every sibling test's comparison
    // running on another thread. A thread-local override confines the wrong
    // oracle to the one thread that asked for it.
    thread_local! {
        static ORACLE_PATH: std::cell::RefCell<Option<std::path::PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Points this thread's oracle helpers at `path`. `None` restores the
    /// real `aomdec`.
    pub(super) fn set_oracle_path(path: Option<std::path::PathBuf>) {
        ORACLE_PATH.with(|c| *c.borrow_mut() = path);
    }

    // lane-film10fix: this thread's ffmpeg override, set by
    // `set_ffmpeg_path` and consulted by `ffmpeg_path` ahead of the bare
    // name. Same shape and same reason as `ORACLE_PATH` above: the control
    // test `every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg` has
    // to hand the `ffmpeg_decode_sequence*` helpers a DELIBERATELY WRONG
    // reference, and a process-global PATH edit would leak that wrong
    // reference into every sibling test decoding on another thread. The
    // crate forbids `unsafe_code`, so `std::env::set_var` is not available
    // either; a thread-local confines the tamper to the one thread.
    thread_local! {
        static FFMPEG_PATH: std::cell::RefCell<Option<std::path::PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Points this thread's `ffmpeg_decode_sequence*` helpers at `path`.
    /// `None` restores the real `ffmpeg` on PATH.
    pub(super) fn set_ffmpeg_path(path: Option<std::path::PathBuf>) {
        FFMPEG_PATH.with(|c| *c.borrow_mut() = path);
    }

    /// The `ffmpeg` every reference helper in this module spawns.
    pub(super) fn ffmpeg_path() -> std::path::PathBuf {
        if let Some(p) = FFMPEG_PATH.with(|c| c.borrow().clone()) {
            return p;
        }
        std::path::PathBuf::from("ffmpeg")
    }

    /// The instrumented `aomdec` beside the `aomenc` oracle
    /// (`scripts/build-aom-oracle.sh` + `scripts/instrument-aom-oracle.sh`).
    pub(super) fn aomdec_path() -> std::path::PathBuf {
        if let Some(p) = ORACLE_PATH.with(|c| c.borrow().clone()) {
            return p;
        }
        if let Some(p) = std::env::var_os("EC_AV1_AOMDEC") {
            return std::path::PathBuf::from(p);
        }
        aomenc_path().with_file_name("aomdec")
    }

    /// Decodes `stream` with both this decoder and the instrumented oracle
    /// `aomdec`, and compares EVERY decoded frame in DECODE order --
    /// including the hidden (`show_frame == 0`) alt-ref frames no other gate
    /// in this repo can see, because `decode_stream` only pushes shown
    /// frames and `ffmpeg -f obu ... rawvideo` only emits shown frames
    /// (class gate-blind-to-hidden-frames: a hidden frame's defect surfaces
    /// only as drift on some later shown frame, if at all).
    ///
    /// Both sides write the final, post-deblock/CDEF/LR/superres frame as it
    /// is stored into the reference buffer (`EC_AV1_FINAL_DUMP`, ours in
    /// `decode_stream`, the oracle's in rung 12 of
    /// `scripts/instrument-aom-oracle.sh`), 8-bit as u8 / high bitdepth as
    /// u16 LE. Returns `(frames_decoded, hidden_frames)`.
    pub(super) fn decode_all_frames_vs_oracle(stream: &[u8], name: &str) -> (usize, usize) {
        let dir = std::env::temp_dir().join(format!("ec-av1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{name}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let count_dumps = |prefix: &std::path::Path| -> usize {
            let mut n = 0usize;
            while std::path::PathBuf::from(format!("{}.f{n}", prefix.display())).is_file() {
                n += 1;
            }
            n
        };
        let oracle_frames = count_dumps(&aom_prefix);
        assert!(
            oracle_frames > 0,
            "{name}: the oracle wrote no EC_AV1_FINAL_DUMP frames -- rung 12 of \
             scripts/instrument-aom-oracle.sh is missing from {}",
            aomdec_path().display()
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let decoded = decode_stream(stream);
        set_final_dump_prefix(None);
        let shown = decoded
            .unwrap_or_else(|e| panic!("{name}: this decoder refused the stream: {e}"))
            .len();
        let our_frames = count_dumps(&ours_prefix);
        assert_eq!(
            our_frames, oracle_frames,
            "{name}: decoded {our_frames} frames in decode order, the oracle decoded \
             {oracle_frames}"
        );
        for i in 0..oracle_frames {
            let want =
                std::fs::read(format!("{}.f{i}", aom_prefix.display())).expect("oracle dump");
            let got = std::fs::read(format!("{}.f{i}", ours_prefix.display())).expect("our dump");
            assert_eq!(
                got.len(),
                want.len(),
                "{name}: decode-order frame {i} is {} bytes, the oracle's is {}",
                got.len(),
                want.len()
            );
            if let Some(at) = got.iter().zip(&want).position(|(a, b)| a != b) {
                let differing = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                panic!(
                    "{name}: decode-order frame {i} of {oracle_frames} ({shown} shown, {} \
                     hidden) differs from the oracle at byte {at} (ours {} vs {}), {differing} \
                     bytes differ -- stream pinned at {}",
                    oracle_frames - shown,
                    got[at],
                    want[at],
                    obu.display()
                );
            }
        }
        let hidden = oracle_frames - shown;
        let _ = std::fs::remove_dir_all(&dir);
        (oracle_frames, hidden)
    }

    /// lane-av1422anom: a COUNTING oracle comparison.
    ///
    /// [`assert_rawvideo_matches`] is the right tool for "these must be equal"
    /// and the wrong tool for a lane that has to classify many cells: it
    /// panics on the first differing byte, so it answers one cell per process
    /// and cannot report a magnitude. This returns the count instead.
    ///
    /// It exists because the previous lane's per-cell counter was tautological:
    /// it packed our own decoded samples into `ours` and then compared `ours`
    /// against the same samples, never touching the oracle's bytes, so it
    /// reported "0 wrong" on every cell including one that differs from
    /// `aomdec` in 1612 samples. [`the_counting_oracle_diff_reports_a_real_difference`]
    /// below is the test that keeps that mistake from recurring.
    ///
    /// Returns `(wrong_y, wrong_u, wrong_v, frames_total, frames_exact)`, or
    /// `None` when the two sides are not byte-comparable at all (a size
    /// mismatch, which is a different failure and must not read as "exact").
    /// `flip` flips ONE bit of the ORACLE's own bytes before comparing, so
    /// [`the_counting_oracle_diff_detects_one_flipped_oracle_byte`] can prove
    /// the comparison still reads the oracle on a stream that is now exact.
    pub(super) fn count_rawvideo_diffs(
        stream: &[u8],
        name: &str,
        flip: Option<usize>,
    ) -> Option<(usize, usize, usize, usize, usize)> {
        let dir = std::env::temp_dir().join(format!("ec-av1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, stream).expect("writing the stream");
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
            "{name}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut want = std::fs::read(&raw).expect("aomdec rawvideo output");
        if let Some(i) = flip {
            assert!(
                i < want.len(),
                "{name}: flip index {i} is past the oracle's {} bytes",
                want.len()
            );
            want[i] ^= 0x01;
        }
        let _ = std::fs::remove_dir_all(&dir);
        let bit_depth = stream_bit_depth(stream, name);
        let decoded = decode_stream(stream).expect("decode for comparison");
        let ours = pack_rawvideo(&decoded, bit_depth, name);
        if ours.len() != want.len() {
            return None;
        }
        let bpp = if bit_depth == 8 { 1 } else { 2 };
        // lane-av1cmpframe: the plane split is PER FRAME and comes from each
        // decoded frame's OWN plane lengths -- never from a formula, and
        // never from the first picture.
        //
        // The first-picture form was wrong on every multi-frame stream: it
        // compared a byte index against frame 0's thresholds forever, so
        // every sample after frame 0's luma was attributed to the LAST
        // plane. On `420_oddheight_320x236` (16 frames of 320x236
        // 4:2:0, whose frame is 113 280 bytes and whose V plane therefore
        // holds 302 080 bytes in total) it reported `wrong_v = 330 673` --
        // more wrong V bytes than exist. The per-frame byte totals and the
        // frame-exact count were sound; only the per-plane attribution was
        // not, and a wrong attribution is the one thing a per-plane report
        // is read for.
        //
        // `pack_rawvideo` already lays the bytes out per frame from the same
        // plane lengths, so this table is exactly the layout it produced.
        let mut spans: Vec<[usize; 4]> = Vec::with_capacity(decoded.len());
        let mut off = 0usize;
        for f in &decoded {
            let (y, u, v) = (f.y.len() * bpp, f.u.len() * bpp, f.v.len() * bpp);
            spans.push([off, off + y, off + y + u, off + y + u + v]);
            off += y + u + v;
        }
        assert_eq!(
            off,
            ours.len(),
            "{name}: the per-frame plane table covers {off} bytes but the packed output is {}",
            ours.len()
        );
        let mut wrong = [0usize; 3];
        let mut frame = 0usize;
        for (i, (&a, &b)) in ours.iter().zip(&want).enumerate() {
            while frame + 1 < spans.len() && i >= spans[frame][3] {
                frame += 1;
            }
            if a == b {
                continue;
            }
            let s = spans[frame];
            let pi = if i < s[1] {
                0
            } else if i < s[2] {
                1
            } else {
                2
            };
            wrong[pi] += 1;
        }
        let frames = decoded.len();
        let exact = spans
            .iter()
            .filter(|s| ours[s[0]..s[3]] == want[s[0]..s[3]])
            .count();
        Some((wrong[0], wrong[1], wrong[2], frames, exact))
    }

    /// lane-hidden r2: the `--arnr-maxframes=0` SECOND ARM of an existing
    /// gate -- the gate's own recipe is never changed, this re-encodes the
    /// same fixture with libaom's temporal alt-ref filter switched off and
    /// runs [`decode_all_frames_vs_oracle`] on the result. Measured: with
    /// the filter at its default EVERY alt-ref recipe in this file emits
    /// ZERO hidden frames (the candidates are filtered away), so the
    /// hidden-frame compare on the gate's own stream is vacuous; this arm
    /// is what actually puts `show_frame == 0` frames on the wire.
    ///
    /// Runs at most once per gate name per process (the loop gates sweep
    /// dozens of attempts and each arm costs a full encode + two decodes).
    /// A stream this decoder refuses BY NAME skips the arm -- the arm is
    /// about hidden frames, not about that capability gap -- while a pixel
    /// mismatch is a hard failure raised inside the compare.
    pub(super) fn hidden_arnr_arm(name: &str, y4m: &[u8], args: &[&str]) -> Option<(usize, usize)> {
        static ARMED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        if !ARMED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(name.to_string())
        {
            return None;
        }
        let mut arm: Vec<&str> = vec!["--arnr-maxframes=0"];
        arm.extend_from_slice(args);
        let out = run_with_stdin(Command::new(aomenc_path()).args(&arm), y4m);
        assert!(
            out.status.success(),
            "{name} HIDDEN-ARM: aomenc refused its own recipe plus --arnr-maxframes=0: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        match decode_stream(&stream) {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("unsupported"),
                    "{name} HIDDEN-ARM failed outright: {msg}"
                );
                eprintln!("{name} HIDDEN-ARM: skipped on a named refusal: {msg}");
                None
            }
            Ok(_) => Some(decode_all_frames_vs_oracle(
                &stream,
                &format!("{name}-arnr0"),
            )),
        }
    }

    /// lane-av1toolgates: the gate-level WITNESS, summed over the arms that
    /// decoded AND compared. It cannot be asserted per arm: the cq20 /
    /// tx-size-search=1 arm reaches a rect transform through the var-tx
    /// search on a SQUARE block (a nonzero rect coefficient count with zero
    /// rect partitions read -- the purest form of what `--enable-rect-tx`
    /// buys), while the rect PARTITION only shows up on the
    /// tx-size-search=0 arm. Either fact alone would let a stream through on
    /// which the tool was allowed and unused, which is the defect this
    /// coverage entry was written for; together they are the parsed-shape
    /// evidence the entry asked for.
    #[track_caller]
    pub(super) fn assert_rect_tx_witness(name: &str, parts: usize, tus: usize) {
        assert!(
            parts > 0,
            "{name}: no PARTITION_HORZ/PARTITION_VERT symbol was read on any pixel-exact decode \
             -- the parsed partition tree never yielded a rect leaf, so `--enable-rect-tx=1` \
             was allowed and unused"
        );
        assert!(
            tus > 0,
            "{name}: no rectangular transform unit coded coefficients through read_coeffs_rect \
             ({parts} rect partitions read) -- the rect coefficient path was never taken"
        );
    }

    /// lane-t900 r31: builds one `--tune-content=screen --enable-intrabc=1`
    /// key frame, 256x192 8-bit, from a deterministic lavfi source. `tiled`
    /// renders a 2x2 tiling of a 128x96 pattern -- exact spatial repetition,
    /// the content intra block copy exists for; `square_only` turns the rect
    /// and 1:4 partitions off so an intrabc block lands on a SQUARE block
    /// instead of stopping on the rect-strip intrabc refusal first.
    pub(super) fn screen_intrabc_stream(
        src: &str,
        cq: &str,
        txs: &str,
        tiled: bool,
        square_only: bool,
    ) -> Vec<u8> {
        screen_intrabc_stream_with(src, cq, txs, tiled, square_only, &[])
    }

    /// [`screen_intrabc_stream`] with per-arm flags appended AFTER the base
    /// recipe -- aomenc keeps the LAST occurrence of a repeated flag, so an
    /// arm can override `--sb-size` / `--min-partition-size` here.
    pub(super) fn screen_intrabc_stream_with(
        src: &str,
        cq: &str,
        txs: &str,
        tiled: bool,
        square_only: bool,
        extra: &[&str],
    ) -> Vec<u8> {
        screen_intrabc_stream_at_depth(src, cq, txs, tiled, square_only, 8, extra)
    }

    /// [`screen_intrabc_stream_with`] at an explicit bit depth (8 or 10): the
    /// lavfi source is rendered straight to that depth's yuv4mpegpipe pixel
    /// format and aomenc is told to encode at the matching
    /// `--bit-depth`/`--input-bit-depth`, so a 10-bit arm produces a genuine
    /// high-bit-depth stream rather than an 8-bit one in disguise.
    pub(super) fn screen_intrabc_stream_at_depth(
        src: &str,
        cq: &str,
        txs: &str,
        tiled: bool,
        square_only: bool,
        depth: usize,
        extra: &[&str],
    ) -> Vec<u8> {
        // lane-av1screen12: depth 12 joins 8 and 10. The SAME recipe (`-vf
        // tile=2x2` over `smptebars=size=128x96:rate=25`,
        // `--tune-content=screen --enable-intrabc=1`, cq30,
        // `--enable-tx-size-search=0`) that keeps `allow_intrabc` at 8 and 10
        // bits keeps it at 12 bits too -- measured, and pinned by
        // `a_real_aomenc_12bit_intrabc_screen_stream_decodes_pixel_exact`.
        // That is why "neither palette nor intrabc has a 12-bit witness" was
        // false: the intrabc half WAS producible, `--tune-content=screen`
        // alone just is not what decides the frame-header bit (libaom's
        // content detector is).
        assert!(
            depth == 8 || depth == 10 || depth == 12,
            "unsupported bit depth {depth}"
        );
        // `if depth == 10` is SPELLED, not matched, because
        // `gate_coverage::covers_both_depths` recognises this two-depth
        // helper by that literal (the lane-defon/reelckf blind spot with
        // `depth` spelled instead of `bit_depth`).
        let pix_fmt = if depth == 10 {
            "yuv420p10le"
        } else if depth == 12 {
            "yuv420p12le"
        } else {
            "yuv420p"
        };
        let mut ff = Command::new("ffmpeg");
        ff.args(["-v", "error", "-f", "lavfi", "-i", src, "-t", "0.2"]);
        if tiled {
            ff.args(["-vf", "tile=2x2"]);
        }
        let out = crate::tool_cache::cached_output_io(ff
            .args([
                "-pix_fmt",
                pix_fmt,
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
        assert!(out.status.success(), "ffmpeg failed for {src}");
        let y4m = out.stdout;
        let enc = run_with_stdin(
            Command::new(aomenc_path())
                .args(["--codec=av1"])
                .args([
                    format!("--bit-depth={depth}"),
                    format!("--input-bit-depth={depth}"),
                ])
                .args([
                    "--passes=1",
                    "--end-usage=q",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--kf-max-dist=1",
                    "--limit=1",
                    "--threads=1",
                    "--tile-columns=0",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--sb-size=64",
                    "--tune-content=screen",
                    "--enable-intrabc=1",
                    "--enable-palette=0",
                ])
                .args([
                    format!("--cq-level={cq}"),
                    format!("--enable-tx-size-search={txs}"),
                ])
                // aomenc keeps the LAST occurrence of a repeated flag.
                .args(if square_only {
                    &["--enable-rect-partitions=0", "--enable-1to4-partitions=0"][..]
                } else {
                    &[][..]
                })
                .args(extra)
                .args(["--obu", "-o", "-", "-"]),
            &y4m,
        );
        assert!(
            enc.status.success(),
            "aomenc refused {src} cq={cq}: {}",
            String::from_utf8_lossy(&enc.stderr)
        );
        enc.stdout
    }

    /// The sequence header's `use_128x128_superblock`, and how many frames of
    /// `stream` keep `allow_intrabc` -- the two bits the refusal "intrabc
    /// under a 128x128 superblock" is the conjunction of, read out of the
    /// headers rather than assumed from the flags handed to the encoder.
    pub(super) fn sb128_and_intrabc_frames(stream: &[u8]) -> (bool, u32) {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let (mut sb128, mut intrabc) = (false, 0u32);
        while pos < stream.len() {
            let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
                break;
            };
            pos += obu.total_size.max(1);
            if let Some(seq) = parser.sequence_header() {
                sb128 = seq.use_128x128_superblock;
            }
            let header = match &obu.kind {
                ObuKind::Frame(h, _) => Some(&**h),
                ObuKind::FrameHeader(h) => Some(&**h),
                _ => None,
            };
            if header.is_some_and(|h| h.allow_intrabc) {
                intrabc += 1;
            }
        }
        (sb128, intrabc)
    }

    /// How many frames of `stream` carry `tx_mode == Select`, and how many of
    /// those also keep `allow_intrabc` -- the frame-level premise of the
    /// refusal "an intrabc block under TxMode::Select", read out of the
    /// headers rather than assumed from the flags handed to the encoder
    /// (libaom clears `allow_intrabc` again when no block picked it,
    /// encodeframe.c:2336).
    pub(super) fn tx_select_and_intrabc_frames(stream: &[u8]) -> (u32, u32) {
        let (mut select, mut premise) = (0u32, 0u32);
        let mut p = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() {
            let obu = p.parse_obu(&stream[pos..]).expect("parse");
            pos += obu.total_size;
            let header = match &obu.kind {
                ObuKind::Frame(h, _) => Some(&**h),
                ObuKind::FrameHeader(h) => Some(&**h),
                _ => None,
            };
            if let Some(h) = header
                && h.tx_mode == TxMode::Select {
                    select += 1;
                    if h.allow_intrabc {
                        premise += 1;
                    }
                }
        }
        (select, premise)
    }

    // /// lane-hbd r5: a real `aomenc --bit-depth=10` stream, decoded through
    // /// this crate's own 10-bit planes (`Picture` widened to `u16` in an
    // /// earlier round) and checked pixel-exact against ffmpeg's own decode of
    // /// the identical bytes via [`ffmpeg_decode_sequence_10bit`]. Confirms
    // /// (hard-asserts, not just trusts) the sequence header this stream
    // /// actually parses to says `bit_depth == 10` before trusting any pixel
    // /// match -- a stream that silently fell back to 8-bit would still
    // /// "match" trivially.
    // -------------------------------------------------------------------
    // lane-av112bit: the 12-bit gate battery. The blanket refusal is lifted
    // for what these gates witness byte-exact against ffmpeg on real
    // `aomenc --bit-depth=12 --profile=2` streams: the full intra stack,
    // dequant/transforms at the 12-bit dequant tables, deblock, CDEF, loop
    // restoration (Wiener AND SGR), and translational subpel motion
    // compensation through the bit-depth-parameterised `round_0`/`round_1`
    // (libaom `convolve.h:81..86`). Everything else stays refused by name:
    // warp and screen content tools (palette/intrabc) at the frame level,
    // each with its own real-stream refusal gate below.
    // lane-av112bitc: compound joined the witnessed set (refusal gate became
    // the six-frame compound witness).
    // lane-av112bitg: film grain joined the witnessed set (refusal gate became
    // `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact`).
    // -------------------------------------------------------------------
    /// The 12-bit witness source, byte-identical to the lane's committed
    /// fixture recipe: 160x128 C420p12, a gradient + fine-texture base with a
    /// 24x20 box that travels 1.25 px/frame horizontally and 0.75 px/frame
    /// vertically with linear edge coverage -- sub-pixel true motion, so an
    /// inter frame over it cannot be coded whole-pel-only.
    pub(super) fn y4m_12bit_subpel_source(frames: usize) -> Vec<u8> {
        const W: usize = 160;
        const H: usize = 128;
        let cov = |a: f64, b: f64, i: i64| (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
        let mut y4m = format!("YUV4MPEG2 W{W} H{H} F30:1 Ip A1:1 C420p12\n").into_bytes();
        for t in 0..frames {
            let (ax, ay) = (12.0 + 1.25 * t as f64, 10.0 + 0.75 * t as f64);
            let (bx, by) = (ax + 24.0, ay + 20.0);
            y4m.extend_from_slice(b"FRAME\n");
            for j in 0..H as i64 {
                for i in 0..W as i64 {
                    let g = (i * 190) / W as i64 + (j * 150) / H as i64;
                    let mut v = (g * 12
                        + if (i * 5 + j * 3 + t as i64) % 7 < 3 {
                            90
                        } else {
                            220
                        }
                        + if (60..110).contains(&i) && (70..110).contains(&j) {
                            640
                        } else {
                            0
                        }) as f64;
                    let c = cov(ax, bx, i) * cov(ay, by, j);
                    if c > 0.0 {
                        v = v * (1.0 - c) + 3500.0 * c;
                    }
                    y4m.extend_from_slice(&(v.min(4095.0) as u16).to_le_bytes());
                }
            }
            for (base, fx) in [(1900usize, 0usize), (2100, 1)] {
                for j in 0..H / 2 {
                    for i in 0..W / 2 {
                        let v = if fx == 0 {
                            base + (((i / 5 + t * 2) % 7) * 120 + (j * 60) / H)
                        } else {
                            base + (((j / 4 + t) % 5) * 140 + (i * 40) / W)
                        };
                        y4m.extend_from_slice(&(v.min(4095) as u16).to_le_bytes());
                    }
                }
            }
        }
        y4m
    }

    /// lane-av1gwarp12 fixture: rotating + uniformly zooming mandelbrot as a
    /// C420p12 y4m. Rotation composed with a UNIFORM scale is a similarity
    /// transform -- exactly the family a 4-parameter ROTZOOM global model
    /// expresses (the affine gate needs the anisotropic stretch to force
    /// 6 parameters; this lane needs the model vanilla aomenc actually
    /// searches, ROTZOOM).
    pub(super) fn y4m_12bit_rotzoom_gm_source(frames: usize) -> Vec<u8> {
        const W: usize = 128;
        const H: usize = 128;
        let source = format!(
            "mandelbrot=size=192x192:rate=25,rotate=a=0.12*t:c=black,\
             scale=w='ceil(192*(1+0.04*n)/2)*2':h='ceil(192*(1+0.04*n)/2)*2':eval=frame,\
             crop={W}:{H}"
        );
        let duration = (frames as f64 / 25.0).to_string();
        let ff_args: Vec<&str> = vec![
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &source,
            "-pix_fmt",
            "yuv420p12le",
            // y4m has no official 12-bit tag, same as the 10-bit gate.
            "-strict",
            "-1",
            "-t",
            &duration,
            "-f",
            "yuv4mpegpipe",
            "-",
        ];
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&ff_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// The 12-bit coding recipe shared by the witness gates (cpu-used=0 so
    /// every search runs; explicit CDEF/restoration ON). Warp/global-motion
    /// are deliberately NOT spelled here: this aomenc honours the LAST
    /// occurrence of these flags (measured), so a base pin plus an override
    /// would silently take the base value -- each gate spells what it needs
    /// exactly once.
    pub(super) fn encode_12bit(y4m: &[u8], frames: usize, extra: &[&str]) -> Vec<u8> {
        let limit = format!("--limit={frames}");
        let mut args: Vec<&str> = vec![
            "--codec=av1",
            "--profile=2",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=20",
            "--cpu-used=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=64",
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--kf-max-dist=1000",
            "--input-bit-depth=12",
            "--bit-depth=12",
            "--enable-cdef=1",
            "--enable-restoration=1",
            "--obu",
            "-o",
            "-",
            "-",
            &limit,
        ];
        // AOMENC FLAG PRECEDENCE (COMMON): aomenc keeps the FIRST occurrence
        // of a flag, so the per-arm override goes BEFORE the base list.
        args.splice(1..1, extra.iter().copied());
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), y4m);
        assert!(
            out.status.success(),
            "aomenc refused the 12-bit fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Asserts the stream's sequence header really says 12-bit before any
    /// pixel comparison is trusted.
    pub(super) fn assert_12bit_sequence_header(stream: &[u8], name: &str) {
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
        }
        let seq = probe
            .sequence_header()
            .expect("stream has a sequence header OBU");
        assert_eq!(
            seq.color_config.bit_depth, 12,
            "{name}: aomenc did not actually write a 12-bit sequence header"
        );
    }

    pub(super) fn ffmpeg_decode_sequence_12bit(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        assert_420_oracle_stream(
            stream,
            "ffmpeg_decode_sequence_12bit",
            "ffmpeg_decode_sequence_422_depth(.., 12, \"yuv422p12le\") (4:2:2); \
             ffmpeg_decode_sequence_444_depth(.., 12, \"yuv444p12le\") (4:4:4)",
        );
        let out = run_with_stdin(
            Command::new("ffmpeg").args([
                "-v",
                "error",
                "-f",
                "obu",
                "-i",
                "-",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p12le",
                "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the 12-bit stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_bytes = (luma + 2 * chroma) * 2;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 12-bit frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fn le16(bytes: &[u8]) -> Vec<u16> {
            bytes
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        }
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Pic {
                    width,
                    height,
                    y: le16(&out.stdout[base..base + luma * 2]),
                    u: le16(&out.stdout[base + luma * 2..base + luma * 2 + chroma * 2]),
                    v: le16(&out.stdout[base + luma * 2 + chroma * 2..base + frame_bytes]),
                }
            })
            .collect()
    }

    /// lane-av1screen12: how many frames of `stream` carry
    /// `allow_screen_content_tools`, and how many of THOSE carry
    /// `allow_intrabc`. Both are read out of the frame headers this crate
    /// parsed, never taken from the flags handed to the encoder (class
    /// `tool-disabled-in-every-gate`): on this oracle build
    /// `--tune-content=screen` does NOT by itself decide the frame-header
    /// `allow_intrabc` bit -- an untiled `smptebars=size=128x96` source reads
    /// `false` with and without the flag, while the 256x192 `tile=2x2` source
    /// the intrabc census uses reads `true` with and without it (measured;
    /// libaom's content detector, `encoder.c:2404`, is what decides it).
    pub(super) fn screen_and_intrabc_frame_counts(stream: &[u8]) -> (u32, u32) {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let (mut screen, mut intrabc) = (0u32, 0u32);
        while pos < stream.len() {
            let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
                break;
            };
            pos += obu.total_size.max(1);
            let header = match &obu.kind {
                ObuKind::Frame(h, _) => Some(&**h),
                ObuKind::FrameHeader(h) => Some(&**h),
                _ => None,
            };
            if let Some(h) = header
                && h.allow_screen_content_tools
            {
                screen += 1;
                if h.allow_intrabc {
                    intrabc += 1;
                }
            }
        }
        (screen, intrabc)
    }

    /// The three committed 12-bit screen-content witnesses, by literal name
    /// (a pin name built from a loop variable leaves the pin census
    /// unproven), with the length and `fnv1a64` each gate asserts. sha256,
    /// recorded in the gates' doc comments and in
    /// `lanes/av1screen12.report.md`:
    ///
    /// * `screen12_palette.obu` 3616 bytes,
    ///   `adfe26c67c08a7e986db2743b914ecc3e214c56dd2abbdfd17ebf99f06de64c8`
    /// * `screen12_intrabc.obu` 482 bytes,
    ///   `a3b6b94cabdd09a12057c4688990d55495826b87a14332c386d319629070382e`
    /// * `screen12_lossless_palette.obu` 11698 bytes,
    ///   `ec7942c96763593e6d8e4b3fb23c03930bba3bb743102d0cf552fe73ae11c203`
    pub(super) const PIN_LEN_PALETTE: usize = 3616;

    pub(super) const PIN_FNV_PALETTE: u64 = 0xf9bb_d408_61d6_f0da;

    pub(super) const PIN_LEN_INTRABC: usize = 482;

    pub(super) const PIN_FNV_INTRABC: u64 = 0xae83_3eb7_9380_263b;

    pub(super) const PIN_LEN_LOSSLESS: usize = 11698;

    pub(super) const PIN_FNV_LOSSLESS: u64 = 0x6e28_41c7_7b0a_f9db;

    /// lane-av1screen12: the shared body of the three 12-bit screen-content
    /// gates. Every assertion a "this arm really is 12-bit screen content,
    /// and this decoder reproduces it" claim needs, in one place:
    ///
    /// 1. the committed PIN is the witness -- its length and fnv1a64 are
    ///    pinned, and `require_pin` HARD-fails on a missing copy rather than
    ///    testing nothing;
    /// 2. the live recipe reproduces that pin byte for byte (aomenc present),
    ///    so the recipe in the doc comment is not decoration;
    /// 3. the stream's OWN sequence header says 12-bit and its frame headers
    ///    say `allow_screen_content_tools` (and `allow_intrabc` for the arm
    ///    that wants it);
    /// 4. the decode ENGAGED the feature -- `palette_hits()` /
    ///    `palette_uv_hits()` / `intrabc_hits()` deltas are hard-asserted,
    ///    because a stream that merely carries the header bit would leave the
    ///    arm it claims to witness untested (class `gate-blind-to-feature`);
    /// 5. every sample of every frame matches the instrumented `aomdec
    ///    --rawvideo` (`assert_rawvideo_matches`), and independently ffmpeg;
    /// 6. THE CONTROL: the same counting comparator, run twice -- clean it
    ///    must report zero wrong samples in all three planes, and with ONE
    ///    bit flipped in the ORACLE's own frame-0 luma high byte it must
    ///    report exactly one wrong luma byte and one fewer exact frame. That
    ///    is the proof the comparator READS the oracle side: a comparator
    ///    that compared our samples with themselves would report 0 both
    ///    times (class `oracle-diff-counter-tautology`).
    ///
    /// Flip index 1 is the high byte of the first luma sample of frame 0.
    /// A 12-bit sample's high byte holds bits 8..15 only, so flipping its low
    /// bit can never carry into another byte: the move is exactly +1 wrong
    /// byte in the Y plane, which is what makes "+1 in the right plane and
    /// frame" a real statement rather than a hope.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assert_12bit_screen_content_arm(
        name: &str,
        pin: &'static str,
        live: Vec<u8>,
        pin_len: usize,
        pin_fnv: u64,
        frames: usize,
        width: usize,
        height: usize,
        want_intrabc: bool,
    ) {
        let path = crate_pin(pin);
        let stream = require_pin(&path, pin);
        assert_eq!(
            stream.len(),
            pin_len,
            "{name}: the committed pin {pin} is {} bytes, this gate names {pin_len}",
            stream.len()
        );
        assert_eq!(
            fnv1a64(&stream),
            pin_fnv,
            "{name}: the committed pin {pin} moved"
        );
        if have_aomenc() {
            assert_eq!(
                live.len(),
                pin_len,
                "{name}: the live recipe produced {} bytes, the committed pin {pin} is \
                 {pin_len} -- aomenc changed, re-pin it",
                live.len()
            );
            assert_eq!(
                fnv1a64(&live),
                pin_fnv,
                "{name}: the live recipe no longer reproduces the committed pin {pin}"
            );
        }
        assert_12bit_sequence_header(&stream, name);
        let (screen_frames, intrabc_frames) = screen_and_intrabc_frame_counts(&stream);
        assert!(
            screen_frames > 0,
            "{name}: no frame carries allow_screen_content_tools -- this is not a \
             screen-content stream and the arm would prove nothing"
        );
        assert_eq!(
            intrabc_frames > 0,
            want_intrabc,
            "{name}: {intrabc_frames} frame(s) carry allow_intrabc, the gate wants {}",
            u32::from(want_intrabc)
        );

        let pal_before = crate::decode::palette_hits();
        let paluv_before = crate::decode::palette_uv_hits();
        let ibc_before = crate::decode::intrabc_hits();
        let decoded = decode_stream(&stream).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(decoded.len(), frames, "{name}: frame count");
        let palette = crate::decode::palette_hits() - pal_before;
        let palette_uv = crate::decode::palette_uv_hits() - paluv_before;
        let intrabc = crate::decode::intrabc_hits() - ibc_before;
        assert!(
            palette > 0,
            "{name}: the stream decoded no palette block -- the 12-bit palette syntax \
             would be untested (class gate-blind-to-feature)"
        );
        assert!(
            palette_uv > 0,
            "{name}: the stream decoded no chroma palette block -- the 12-bit palette-UV \
             syntax would be untested (class gate-blind-to-feature)"
        );
        if want_intrabc {
            assert!(
                intrabc > 0,
                "{name}: allow_intrabc is set but no block decoded use_intrabc=1 -- the \
                 12-bit intra-block-copy syntax would be untested (class \
                 gate-blind-to-feature)"
            );
        }

        if aomdec_available(name) {
            let dir = std::env::temp_dir().join(format!("ec-av1-{name}-arm"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let obu = dir.join("in.obu");
            std::fs::write(&obu, &stream).expect("writing the stream");
            assert_rawvideo_matches(&obu, &stream, name, frames, 12);
            let _ = std::fs::remove_dir_all(&dir);
            // The +1 control (part 6): clean is all-zero, one flipped oracle
            // byte is exactly +1 luma byte and one fewer exact frame.
            let clean = count_rawvideo_diffs(&stream, name, None).unwrap_or_else(|| {
                panic!("{name}: our packed output and the oracle's rawvideo differ in SIZE")
            });
            assert_eq!(
                clean,
                (0, 0, 0, frames, frames),
                "{name}: {frames} frame(s) must be byte-exact against the oracle before the \
                 control arm means anything"
            );
            let flipped = count_rawvideo_diffs(&stream, name, Some(1))
                .unwrap_or_else(|| panic!("{name}: the control arm lost the size check"));
            assert_eq!(
                flipped,
                (1, 0, 0, frames, frames - 1),
                "{name}: flipping bit 0 of the ORACLE's frame-0 luma high byte must move the \
                 count by exactly +1 luma byte and cost exactly one exact frame -- if this \
                 reports 0 the comparator never read the oracle (class \
                 oracle-diff-counter-tautology)"
            );
        }
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_sequence_12bit(&stream, width, height, frames);
            assert_eq!(theirs.len(), frames, "{name}: ffmpeg frame count");
            for (i, (ours, want)) in decoded.iter().zip(&theirs).enumerate() {
                assert_eq!(ours.y, want.y, "{name}: 12-bit luma vs ffmpeg (frame {i})");
                assert_eq!(ours.u, want.u, "{name}: 12-bit U vs ffmpeg (frame {i})");
                assert_eq!(ours.v, want.v, "{name}: 12-bit V vs ffmpeg (frame {i})");
            }
        }
        eprintln!(
            "{name}: {frames} 12-bit frame(s) byte-exact vs aomdec --rawvideo and ffmpeg, \
             {screen_frames} screen frame(s), {intrabc_frames} allow_intrabc, {palette} palette \
             + {palette_uv} palette-UV + {intrabc} intrabc block(s)"
        );
    }

    /// Renders a 12-bit y4m for the 12-bit screen-content gates. `tiled`
    /// applies `-vf tile=2x2`, which is what takes the source from 128x96 to
    /// the 256x192 shape libaom's content detector clears the (stricter)
    /// IntraBC threshold on.
    pub(super) fn y4m_12bit_screen_source(src: &str, tiled: bool) -> Vec<u8> {
        let mut ff = Command::new("ffmpeg");
        ff.args(["-v", "error", "-f", "lavfi", "-i", src, "-t", "0.2"]);
        if tiled {
            ff.args(["-vf", "tile=2x2"]);
        }
        let out = crate::tool_cache::cached_output_io(ff
            .args([
                "-pix_fmt",
                "yuv420p12le",
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
            out.status.success(),
            "ffmpeg failed for the 12-bit screen source {src}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Shared body for the lane-av1superhbd HIGH-BIT-DEPTH 4:4:4 superres
    /// arms: 10-bit and 12-bit, `--superres-mode=1` at a fixed denominator
    /// and `--superres-mode=2` (a per-frame denominator libaom picks itself).
    ///
    /// Everything the 8-bit 4:4:4 superres cell is non-vacuous by, repeated
    /// here, plus the one thing that only matters above 8 bits: the gate
    /// asserts the PARSED `bit_depth` equals the depth it names, because a
    /// 10-bit recipe whose flags were mistyped still decodes -- it just
    /// decodes a DIFFERENT cell than the gate claims, which is the same
    /// "phantom cell" trap lane-av1444edge r3 recorded for
    /// `use_superres=false`.
    ///
    /// The per-frame table is `(denominator, coded_width)` in DECODE order.
    /// The coded width is re-derived as `(UpscaledWidth*8 + denom/2)/denom`
    /// from that frame's OWN parsed denominator rather than only compared to
    /// a literal, so a re-pinned fixture cannot satisfy the literals while
    /// the scaled-size arithmetic moves under them. `superres_hits` and
    /// `predict_scaled_hits` are asserted as DELTAS (both are process-wide
    /// counters with no reset).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn a_444_hbd_superres_arm(
        name: &str,
        fixture: &str,
        fixture_len: usize,
        fixture_fnv: u64,
        fixture_sha: &str,
        bit_depth: u8,
        upscaled: (usize, usize),
        per_frame: &[(u8, usize)],
    ) -> Vec<FrameHeader> {
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{name}: pinned fixture {fixture} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip"
            )
        });
        assert_eq!(stream.len(), fixture_len, "{name}: fixture length moved");
        assert_eq!(fnv1a64(&stream), fixture_fnv, "{name}: fixture bytes moved");

        // --- the header facts the CELL depends on, parsed not assumed ------
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut seq = None;
        let mut heads: Vec<FrameHeader> = Vec::new();
        while pos < stream.len() {
            let parsed = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: aomenc OBU does not parse: {e:?}"));
            if parsed.total_size == 0 {
                break;
            }
            pos += parsed.total_size;
            match parsed.kind {
                ObuKind::SequenceHeader(s) => seq = Some(s),
                ObuKind::FrameHeader(h) => heads.push(*h),
                ObuKind::Frame(h, _) => heads.push(*h),
                _ => {}
            }
        }
        let seq = seq.unwrap_or_else(|| panic!("{name}: the stream carries no sequence header"));
        assert_eq!(
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            ),
            (0, 0),
            "{name}: the stream is not 4:4:4 (ss = ({}, {}))",
            seq.color_config.subsampling_x,
            seq.color_config.subsampling_y
        );
        // The high-bit-depth claim itself. A `--bit-depth` flag that silently
        // did not take effect still produces a decodable 4:4:4 superres
        // stream -- at 8 bits -- and every other assertion below would pass.
        assert_eq!(
            seq.color_config.bit_depth, bit_depth,
            "{name}: the sequence header parses bit_depth {}, not the {bit_depth} this gate \
             names -- the encoder flags did not take effect and this is a different cell",
            seq.color_config.bit_depth
        );
        assert!(
            seq.enable_superres,
            "{name}: the sequence header does not enable superres at all"
        );
        assert_eq!(
            heads.len(),
            per_frame.len(),
            "{name}: {} frame headers, expected {}",
            heads.len(),
            per_frame.len()
        );
        for (i, (h, (denom, coded))) in heads.iter().zip(per_frame).enumerate() {
            assert!(
                h.use_superres,
                "{name}: frame {i} has use_superres = false -- this is an UNSCALED stream and \
                 the gate would measure something other than the cell it names (class \
                 gate-blind-to-feature)"
            );
            assert_eq!(
                h.superres_denom, *denom,
                "{name}: frame {i} PARSED denominator moved from {denom} -- read the header, \
                 never the encoder flag (mode 2 chooses its own)"
            );
            assert_eq!(
                (h.upscaled_width as usize, h.frame_height as usize),
                upscaled,
                "{name}: frame {i} upscaled size moved"
            );
            assert_eq!(
                (h.frame_width as usize, h.frame_height as usize),
                (*coded, upscaled.1),
                "{name}: frame {i} CODED size moved -- this is the scaled half of the cell and \
                 the part an unsized stream gets wrong"
            );
            let d = u32::from(*denom);
            assert_eq!(
                (h.upscaled_width * 8 + d / 2) / d,
                h.frame_width,
                "{name}: frame {i}: FrameWidth is not (UpscaledWidth*8 + denom/2)/denom -- the \
                 scaled size the cell rests on is not the spec's"
            );
        }

        // --- the pixels -----------------------------------------------------
        let _guard = lock_gate_counters();
        let before_sr = crate::superres::superres_hits();
        let before_scaled = crate::mc::predict_scaled_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{name}: the pinned {bit_depth}-bit 4:4:4 superres stream was refused: {e}")
        });
        let scaled_up = crate::superres::superres_hits() - before_sr;
        let scaled_mc = crate::mc::predict_scaled_hits() - before_scaled;
        assert!(
            scaled_up > 0,
            "{name}: zero superres_hits -- the upscaler never ran (class gate-blind-to-feature)"
        );
        assert!(
            scaled_mc > 0,
            "{name}: zero predict_scaled_hits -- no inter block read a scaled reference, so the \
             scaled-PREDICTION half of the cell is unexercised (class gate-blind-to-feature)"
        );
        assert_eq!(frames.len(), per_frame.len(), "{name}: decoded frame count");
        let max = (1u32 << bit_depth) - 1;
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                (f.width, f.height),
                upscaled,
                "{name}: frame {i} came back {}x{}, not the UPSCALED {}x{}",
                f.width,
                f.height,
                upscaled.0,
                upscaled.1
            );
            for (plane, samples) in [("Y", &f.y), ("U", &f.u), ("V", &f.v)] {
                assert_eq!(
                    samples.len(),
                    upscaled.0 * upscaled.1,
                    "{name}: frame {i} {plane} plane is {} samples; at 4:4:4 upscaled it must be \
                     full resolution ({}x{})",
                    samples.len(),
                    upscaled.0,
                    upscaled.1
                );
                // The depth claim, on the pixels: a decoder that lost the
                // high bits (or produced 8-bit values padded into a u16
                // plane) would satisfy every length and geometry assert above
                // and only be caught here.
                assert!(
                    samples.iter().all(|&s| u32::from(s) <= max),
                    "{name}: frame {i} {plane} carries a sample above the {bit_depth}-bit \
                     maximum {max} -- the bit depth of the OUTPUT does not match the header"
                );
            }
        }
        if aomdec_available(name) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, name);
            eprintln!(
                "{name}: {decoded} decode-order frame(s) ({hidden} hidden) byte-exact vs aomdec \
                 at {bit_depth}-bit {upscaled:?} upscaled from {per_frame:?} (denominator, coded \
                 width) per frame, {scaled_up} upscale(s), {scaled_mc} scaled MC block(s); \
                 fixture sha256 {fixture_sha}"
            );
        }
        heads
    }

    /// Seeds swept by the 10-bit LR gate (the 8-bit gate needs ~40 attempts
    /// to land a non-RESTORE_NONE frame; every attempt here still has to
    /// decode pixel-exact).
    /// lane-obmcrec r1: the `LR_SEEDS` members whose 1:4-partition stream both
    /// fires `obmc_pair_filter_hits` and decodes every frame pixel-exact.
    /// Measured over seeds 42..=73 (32 encodes): 43/46/47/65/66 fire and are
    /// exact; 70 and 73 fire and mismatch, but mismatch IDENTICALLY with
    /// `--enable-obmc=0` (seed 73 frame 1 luma, seed 70 frame 16), so their
    /// defect is in the `--min-partition-size=4` recipe, not in OBMC -- kept
    /// out of this gate rather than papered over, and reported as open.
    pub(super) const OBMC_PAIR_SEEDS: [u32; 5] = [43, 46, 47, 65, 66];

    pub(super) const LR_SEEDS: [u32; 16] = [
        42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57,
    ];

    /// Wiener + SGR + switchable loop-restoration firings, summed.
    pub(super) fn ten_bit_lr_hits() -> usize {
        crate::restoration::wiener_hits()
            + crate::restoration::sgrproj_hits()
            + crate::restoration::switchable_hits()
    }

    /// `bit_depth` of the first sequence header in `stream`.
    pub(super) fn parsed_bit_depth(stream: &[u8]) -> u8 {
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
        }
        probe
            .sequence_header()
            .expect("stream has a sequence header OBU")
            .color_config
            .bit_depth
    }

    /// Render `frames` of the shared `gradients_source` as `yuv420p10le` and
    /// encode it with a real 10-bit `aomenc`, returning the OBU stream.
    /// `extra` carries the tool flags under test.
    pub(super) fn encode_10bit_gradients(
        name: &str,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
    ) -> Vec<u8> {
        encode_10bit_gradients_seed(name, 42, width, height, frames, extra)
    }

    /// As [`encode_10bit_gradients`], but with the fixture seed exposed --
    /// lane-hbdgates r1 sweeps seeds so a tool that aomenc's RD declines on
    /// one fixture still gets a firing stream.
    pub(super) fn encode_10bit_gradients_seed(
        name: &str,
        seed: u32,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
    ) -> Vec<u8> {
        let duration = frames as f64 / 25.0;
        // lane-ab32 r1: `--ec-fixture-noise` is not an aomenc flag -- it is a
        // fixture request, consumed (and stripped) here: the smooth gradients
        // source codes as PARTITION_NONE everywhere, so a partition gate needs
        // seeded noise on top of it to give the RD a reason to split.
        let noisy = extra.contains(&"--ec-fixture-noise");
        let stripped: Vec<&str> = extra
            .iter()
            .copied()
            .filter(|a| *a != "--ec-fixture-noise")
            .collect();
        let extra: &[&str] = &stripped;
        let base = gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
        let source = if noisy {
            format!("{base},noise=alls=40:all_seed={seed}")
        } else {
            base
        };
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-pix_fmt",
                "yuv420p10le",
                "-strict",
                "-1",
                "-t",
                &format!("{duration}"),
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
            "{name}: ffmpeg failed to render the 10-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut args: Vec<&str> = vec!["--codec=av1", "--passes=1", "--end-usage=q"];
        // lane-hbdgates r1: a caller-supplied `--cq-level` REPLACES the
        // default rather than being appended after it -- two `--cq-level`
        // flags on one aomenc line is a coin flip on which one lands.
        if !extra.iter().any(|a| a.starts_with("--cq-level=")) {
            args.push("--cq-level=30");
        }
        args.extend_from_slice(&[
            "--cpu-used=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=64",
            "--input-bit-depth=10",
            "--bit-depth=10",
        ]);
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--obu", "-o", "-", "-"]);
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{name}: aomenc refused the 10-bit fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// The affine-capable encoder (`scripts/build-aom-affine-oracle.sh`),
    /// separate from the shared oracle because stock libaom's global-motion
    /// model search is pinned to ROTZOOM and can never emit AFFINE.
    pub(super) fn affine_aomenc_path() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("EC_AV1_AFFINE_AOMENC") {
            return std::path::PathBuf::from(p);
        }
        let home = std::env::var_os("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join(".cache/aom-affine/build/aomenc")
    }

    pub(super) fn have_affine_aomenc() -> bool {
        let present = affine_aomenc_path().is_file();
        assert!(
            present || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
            "EC_AV1_REQUIRE_AOMENC is set but no affine-capable aomenc at {} -- run \
             scripts/build-aom-affine-oracle.sh",
            affine_aomenc_path().display()
        );
        present
    }

    pub(super) fn check_pinned_warp_stream(path: &std::path::Path) {
        use crate::decode::warp_selected_hits;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stream = require_pin(path, &name);
        let before = warp_selected_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "warp_selected_hits before={before} after={}",
            warp_selected_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 24);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if got.y != want.y {
                for row in 0..64 {
                    let mut line = String::new();
                    for col in 0..64 {
                        let d = got.y[row * 64 + col] as i32 - want.y[row * 64 + col] as i32;
                        line.push(if d == 0 {
                            '.'
                        } else if d.abs() < 4 {
                            'o'
                        } else {
                            'X'
                        });
                    }
                    eprintln!("{line}");
                }
            }
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// Which of the four 128-root AB shapes the gate above MEASURED aomenc
    /// picking (lane-sb128c r9). An arm that is `false` is not reachable from
    /// this sweep's recipes and stays unasserted rather than weakening the
    /// asserts of the ones that are.
    pub(super) const AB_ARMS_REACHED: [bool; 4] = [true, true, true, true];

    /// lane-av1muchunk422b: read a pinned 4:2:2 128-root mu-chunk fixture,
    /// asserting its byte length and FNV-1a64 -- a missing fixture is a
    /// failure, not a skip.
    pub(super) fn read_pinned_muchunk_fixture(name: &str, len: usize, fnv: u64, gate: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{gate}: pinned fixture {} is missing ({e}) -- the gate cannot run, which is a \
                 failure, not a skip",
                path.display()
            )
        });
        assert_eq!(bytes.len(), len, "{gate}: {name} length moved");
        assert_eq!(fnv1a64(&bytes), fnv, "{gate}: {name} bytes moved");
        bytes
    }

    /// lane-av1muchunk422b: `(use_128x128_superblock, ss_x, ss_y, mono_chrome)`
    /// of an OBU stream's sequence header. Shape first: a stream that came back
    /// 4:2:0 would leave the `side > 64` mu-chunk walk unexercised while every
    /// counter assert below still passed.
    pub(super) fn obu_stream_shape(stream: &[u8]) -> Option<(bool, u8, u8, bool)> {
        let mut parser = ec_av1_syntax::Av1Parser::new();
        let mut pos = 0usize;
        while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
            pos += obu.total_size;
            if pos >= stream.len() {
                break;
            }
        }
        parser.sequence_header().map(|q| {
            (
                q.use_128x128_superblock,
                q.color_config.subsampling_x,
                q.color_config.subsampling_y,
                q.color_config.mono_chrome,
            )
        })
    }

    /// The chroma-rect gate's fixture + aomenc recipe for one attempt, shared
    /// with the seed-46 pin test below so the pin cannot drift from the gate.
    pub(super) fn rectchroma_stream(
        bit_depth: u32,
        cq: u32,
        attempt: u32,
        width: usize,
        height: usize,
    ) -> Vec<u8> {
        const NAME: &str = "rectchroma_stream";
        let seed = 42 + attempt;
        let pix_fmt = if bit_depth == 10 {
            "yuv420p10le"
        } else {
            "yuv420p"
        };
        // `gradients_source`, the content the sibling split-transform
        // superblock gate uses. A band/noise `geq` fixture was tried
        // first (r1) and gave 12/12 pixel-exact streams carrying ZERO
        // 1:4 strips -- vacuous. Odd attempts render it transposed so
        // HORZ_4 and VERT_4 both appear (class scan-weights-cross-axis).
        let bands = format!(
            "geq=lum='mod(floor(Y/16),2)*160+48':cb=128:cr=128,noise=alls=10:all_seed={seed}"
        );
        let source = if attempt.is_multiple_of(2) {
            format!("color=c=black:size={width}x{height}:duration=0.04:rate=25,{bands}")
        } else {
            format!("color=c=black:size={height}x{width}:duration=0.04:rate=25,{bands},transpose=1")
        };
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-t",
                "0.04",
                "-pix_fmt",
                pix_fmt,
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
            "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let cq_arg = format!("--cq-level={cq}");
        let depth_arg = format!("--bit-depth={bit_depth}");
        let input_depth_arg = format!("--input-bit-depth={bit_depth}");
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq_arg,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                &depth_arg,
                &input_depth_arg,
                "--enable-rect-partitions=1",
                // AB off: it competes with 1:4 in the RD search (with
                // AB on, 0 superblock 1:4 strips appeared in 16
                // compared streams), and AB below 16x16 is another
                // lane's refusal.
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=1",
                // 16, not 8: with 8 every seed of this recipe stops
                // at a 32x32-level PARTITION_HORZ_4/VERT_4 (partition
                // values 8/9, 32x8/8x32 luma strips), a shape this
                // decoder does not code at all yet -- 12/12 attempts
                // refused before a pixel was compared (measured r1).
                // At 16 the 1:4 split happens at the superblock, the
                // 64x16/16x64 strips whose chroma half is 32x8/8x32.
                "--min-partition-size=16",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "{NAME}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Runs the census child on `fixture` and returns the `GRAINCENSUS` line's
    /// `(unwritten, scanned)` -- the POST-grain pair, over the picture
    /// `apply_grain` returns.
    pub(super) fn grain_census(fixture: &str) -> (usize, usize) {
        const NAME: &str =
            "the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample";
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "stream::tests::t09::uwdep_child_unwritten_census",
                "--nocapture",
            ])
            .env("UWDEP_CENSUS_FIXTURE", fixture)
            .env("EC_AV1_PLANE_SENTINEL", "1")
            .output()
            .expect("running the census child");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        text.lines()
            .find_map(|l| l.strip_prefix("GRAINCENSUS "))
            .and_then(parse_census_line)
            .unwrap_or_else(|| {
                panic!(
                    "{NAME}: the census child reported no grained count for {fixture} \
                     (child said: {:?})",
                    text.trim()
                )
            })
    }

    /// The synthetic non-vacuity control for the grained census: the child with
    /// `UWDEP_CENSUS_FIXTURE` UNSET hands [`census_unwritten_grained`] planes
    /// carrying 5 sentinel samples and reports what it counted. Function-level,
    /// so no fixture's decode behaviour can change it.
    pub(super) fn grain_census_synthetic() -> (usize, usize) {
        const NAME: &str =
            "the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample";
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "stream::tests::t09::uwdep_child_unwritten_census",
                "--nocapture",
            ])
            .env("EC_AV1_PLANE_SENTINEL", "1")
            .env("UWDEP_CENSUS_SYNTHETIC", "1")
            .env_remove("UWDEP_CENSUS_FIXTURE")
            .output()
            .expect("running the census child");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        text.lines()
            .find_map(|l| l.strip_prefix("GRAINCENSUS "))
            .and_then(parse_census_line)
            .unwrap_or_else(|| {
                panic!(
                    "{NAME}: the census child reported no synthetic grained count (child said: \
                     {:?})",
                    text.trim()
                )
            })
    }

    /// Parses the child's `UWDEP_CENSUS <unwritten> frames=<n> scanned=<n>` line
    /// into `(unwritten, scanned)`. The scanned count is what makes the fixture
    /// arms meaningful: `unwritten == 0` is also what a build with the census call
    /// deleted reports, and only a non-zero scanned count distinguishes "scanned
    /// and found nothing" from "never scanned".
    pub(super) fn parse_census_line(v: &str) -> Option<(usize, usize)> {
        let mut parts = v.split_whitespace();
        let unwritten = parts.next()?.parse().ok()?;
        let scanned = parts
            .find_map(|p| p.strip_prefix("scanned="))?
            .parse()
            .ok()?;
        Some((unwritten, scanned))
    }

    /// Runs the census child with `UWDEP_CENSUS_FIXTURE` UNSET, which makes it
    /// take the synthetic-positive path: hand [`census_unwritten_final`] planes
    /// with a known number of `PLANE_SENTINEL` values and report what it counted.
    /// This is the non-vacuity control, and it is a direct function-level check
    /// rather than a decode, so no fixture's decode behaviour can change it.
    pub(super) fn census_synthetic() -> (usize, usize) {
        const NAME: &str = "the_frame_the_caller_receives_carries_no_unwritten_plane_sample";
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "stream::tests::t09::uwdep_child_unwritten_census",
                "--nocapture",
            ])
            .env("EC_AV1_PLANE_SENTINEL", "1")
            .env("UWDEP_CENSUS_SYNTHETIC", "1")
            .env_remove("UWDEP_CENSUS_FIXTURE")
            .output()
            .expect("running the census child");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        text.lines()
            .find_map(|l| l.strip_prefix("UWDEP_CENSUS "))
            .and_then(parse_census_line)
            .unwrap_or_else(|| {
                panic!(
                    "{NAME}: the census child reported no synthetic count (child said: {:?})",
                    text.trim()
                )
            })
    }

    /// Runs [`allocdet_child_decode`] in a fresh process and parses its
    /// `ALLOCDET <fixture> frames=<n> hash=<hex>` lines into
    /// `(fixture, frames, hash)` in FIXTURES order.
    pub(super) fn allocdet_child(
        fixtures: &[&str],
        poison: Option<&str>,
        alloc: &str,
    ) -> Vec<(String, usize, u64)> {
        const NAME: &str = "the_frame_does_not_depend_on_the_plane_buffers_initial_content";
        let exe = std::env::current_exe().expect("test binary path");
        let mut cmd = std::process::Command::new(exe);
        cmd.args([
            "--exact",
            "stream::tests::t09::allocdet_child_decode",
            "--nocapture",
        ])
        .env("ALLOCDET_FIXTURES", fixtures.join(","))
        .env("EC_AV1_PLANE_ALLOC", alloc)
        .env_remove("ALLOCDET_SYNTHETIC");
        match poison {
            Some(p) => cmd.env("EC_AV1_PLANE_SENTINEL", p),
            None => cmd.env_remove("EC_AV1_PLANE_SENTINEL"),
        };
        let out = cmd.output().expect("running the allocdet child");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let mut rows = Vec::new();
        for f in fixtures {
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!("ALLOCDET {f} ")))
                .unwrap_or_else(|| {
                    panic!(
                        "{NAME}: the child reported nothing for {f} (child said: {:?})",
                        text.trim()
                    )
                });
            let mut parts = line.split_whitespace();
            assert_eq!(parts.next(), Some("ALLOCDET"));
            assert_eq!(parts.next(), Some(*f));
            let frames = parts
                .find_map(|p| p.strip_prefix("frames="))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{NAME}: no frame count in {line:?}"));
            let hash = parts
                .find_map(|p| p.strip_prefix("hash="))
                .and_then(|v| u64::from_str_radix(v, 16).ok())
                .unwrap_or_else(|| panic!("{NAME}: no hash in {line:?}"));
            rows.push(((*f).to_string(), frames, hash));
        }
        rows
    }

    /// Two pictures identical except for ONE sample of ONE plane, hashed by the
    /// same FNV-1a the child uses. Equal values here would make the gate's
    /// equalities vacuous, so it is asserted to differ.
    pub(super) fn allocdet_synthetic() -> (u64, u64) {
        const P: u64 = 0x0000_0100_0000_01b3;
        let hash = |mut h: u64, samples: &[u16]| {
            for &s in samples {
                for b in s.to_le_bytes() {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(P);
                }
            }
            h
        };
        let a = vec![128u16; 4096];
        let mut b = a.clone();
        b[2048] = 129;
        (
            hash(0xcbf2_9ce4_8422_2325, &a),
            hash(0xcbf2_9ce4_8422_2325, &b),
        )
    }

    /// lane-av1formatsweep: encodes the odd-dimension arm's fixture --
    /// `testsrc2` (or the smoothed `mandelbrot` source at 12 bits, which
    /// keeps `allow_screen_content_tools` off) as `y4m`, then hands the pipe
    /// to `aomenc`. The child deadline comes from `run_with_stdin`
    /// (`EC_AV1_CHILD_TIMEOUT_SECS`), so a wedged encoder fails loudly.
    pub(super) fn odd_dim_stream(
        width: usize,
        height: usize,
        pix_fmt: &str,
        bit_depth: usize,
        smooth: bool,
    ) -> Vec<u8> {
        let source = if smooth {
            format!("mandelbrot=size={width}x{height}:rate=25")
        } else {
            format!("testsrc2=size={width}x{height}:rate=25")
        };
        let mut ffmpeg_args: Vec<String> = vec![
            "-v".into(),
            "error".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            source,
        ];
        if smooth {
            ffmpeg_args.extend(["-vf".into(), "gblur=sigma=6".into()]);
        }
        ffmpeg_args.extend([
            "-frames:v".into(),
            "4".into(),
            "-pix_fmt".into(),
            pix_fmt.into(),
            "-strict".into(),
            "-1".into(),
            "-f".into(),
            "yuv4mpegpipe".into(),
            "-".into(),
        ]);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg refused the {width}x{height} {pix_fmt} source: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut args: Vec<String> = [
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=20",
            "--cpu-used=2",
            "--threads=1",
            "--row-mt=0",
            "--lag-in-frames=0",
            "--kf-max-dist=100",
            "--limit=4",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        if bit_depth > 8 {
            args.push(format!("--input-bit-depth={bit_depth}"));
            args.push(format!("--bit-depth={bit_depth}"));
        }
        args.extend(["--obu".into(), "-o".into(), "-".into(), "-".into()]);
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "aomenc refused {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Reads `(mi_cols, mi_rows)` out of a stream's first frame header, and
    /// asserts the sequence header says the bit depth the caller asked for.
    pub(super) fn odd_dim_header_mi_grid(stream: &[u8], name: &str, bit_depth: usize) -> (usize, usize) {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut seen_depth = None;
        let mut grid = None;
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: aomenc OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            match obu.kind {
                ObuKind::SequenceHeader(seq) => {
                    assert_eq!(
                        seq.color_config.bit_depth as usize, bit_depth,
                        "{name}: the stream is not {bit_depth}-bit"
                    );
                    assert_eq!(
                        (
                            seq.color_config.subsampling_x,
                            seq.color_config.subsampling_y
                        ),
                        (1, 1),
                        "{name}: not a 4:2:0 stream"
                    );
                    seen_depth = Some(seq.color_config.bit_depth);
                }
                ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) => {
                    grid = Some((
                        (h.frame_width as usize).div_ceil(8),
                        (h.frame_height as usize).div_ceil(8),
                    ));
                }
                _ => {}
            }
        }
        assert!(seen_depth.is_some(), "{name}: no sequence header parsed");
        grid.expect("the stream carries no frame header")
    }

    /// lane-av1formatsweep: a `testsrc2`/`mandelbrot` source rendered to
    /// `pix_fmt` as y4m and piped to `aomenc` at `bit_depth`, with `extra`
    /// aomenc flags. `smooth` selects the gblur'd `mandelbrot` source (1) or
    /// `testsrc2` (0). The child deadline comes from `run_with_stdin`
    /// (`EC_AV1_CHILD_TIMEOUT_SECS`), so a wedged encoder fails loudly.
    pub(super) fn chroma_format_stream(
        width: usize,
        height: usize,
        pix_fmt: &str,
        bit_depth: usize,
        frames: usize,
        extra: &[&str],
        smooth: u8,
    ) -> Vec<u8> {
        let source = if smooth == 1 {
            format!("mandelbrot=size={width}x{height}:rate=25")
        } else {
            format!("testsrc2=size={width}x{height}:rate=25")
        };
        let mut ffmpeg_args: Vec<String> = vec![
            "-v".into(),
            "error".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            source,
        ];
        if smooth == 1 {
            ffmpeg_args.extend(["-vf".into(), "gblur=sigma=6".into()]);
        }
        ffmpeg_args.extend([
            "-frames:v".into(),
            frames.to_string(),
            "-pix_fmt".into(),
            pix_fmt.into(),
            "-strict".into(),
            "-1".into(),
            "-f".into(),
            "yuv4mpegpipe".into(),
            "-".into(),
        ]);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg refused the {width}x{height} {pix_fmt} source: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        // AOMENC FLAG PRECEDENCE (COMMON): aomenc keeps the FIRST occurrence
        // of a flag, so the per-arm override goes BEFORE the base list.
        let limit = format!("--limit={frames}");
        let mut args: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
        args.extend(
            [
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--kf-max-dist=100",
                limit.as_str(),
            ]
            .iter()
            .map(|s| (*s).to_owned()),
        );
        if bit_depth > 8 {
            args.push(format!("--input-bit-depth={bit_depth}"));
            args.push(format!("--bit-depth={bit_depth}"));
        }
        args.extend(["--obu".into(), "-o".into(), "-".into(), "-".into()]);
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "aomenc refused {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Asserts the stream's sequence header really is 4:4:4 (`ss 0,0`) at
    /// `bit_depth` before any pixel comparison is trusted (class
    /// `arrival-not-intent`: a recipe that silently produced 4:2:0 would
    /// make the whole gate a no-op for its own subject).
    pub(super) fn assert_444_header(stream: &[u8], name: &str, bit_depth: usize) {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut seen = false;
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: aomenc OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            if let ObuKind::SequenceHeader(seq) = obu.kind {
                let cc = &seq.color_config;
                assert_eq!(
                    (cc.subsampling_x, cc.subsampling_y),
                    (0, 0),
                    "{name}: the stream is not 4:4:4 (ss {}x{})",
                    cc.subsampling_x,
                    cc.subsampling_y
                );
                assert_eq!(
                    cc.bit_depth as usize, bit_depth,
                    "{name}: the stream is {}-bit, not {bit_depth}-bit",
                    cc.bit_depth
                );
                seen = true;
            }
        }
        assert!(seen, "{name}: no sequence header parsed");
    }

    /// lane-av1tilemeasure: the per-frame facts a tiling / superres / odd-size
    /// cell's OWN claim needs, read off the parsed stream.
    ///
    /// Every gate below asserts its cell through this, never through the
    /// aomenc flag that was supposed to produce it. `lanes/av1formatsweep.
    /// report.md` trap 2 is exactly that failure: `--tile-rows=1` below 256px
    /// high at `--sb-size=128` leaves the stream byte-identical, so a gate
    /// that trusted the flag would report an untiled decode as a tile-row
    /// decode (and the sweep's own 10/12-bit "tile rows" numbers were that).
    #[derive(Debug, Clone, Copy)]
    pub(super) struct FrameFacts {
        pub(super) tile_cols: u32,
        pub(super) tile_rows: u32,
        pub(super) use_superres: bool,
        pub(super) coded_w: u32,
        pub(super) coded_h: u32,
        pub(super) upscaled_w: u32,
        pub(super) mi_cols: u32,
        pub(super) mi_rows: u32,
    }

    pub(super) fn frame_facts(stream: &[u8], name: &str) -> Vec<FrameFacts> {
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut out: Vec<FrameFacts> = Vec::new();
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: aomenc OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            if let ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) = obu.kind {
                out.push(FrameFacts {
                    tile_cols: h.tile_info.cols,
                    tile_rows: h.tile_info.rows,
                    use_superres: h.use_superres,
                    coded_w: h.frame_width,
                    coded_h: h.frame_height,
                    upscaled_w: h.upscaled_width,
                    mi_cols: h.mi_cols,
                    mi_rows: h.mi_rows,
                });
            }
        }
        assert!(
            !out.is_empty(),
            "{name}: the stream carries no frame header"
        );
        out
    }

    /// Decode-order frame 0 only, against the oracle's own
    /// `EC_AV1_FINAL_DUMP` for that frame: how many SAMPLES differ, and the
    /// first one as `(plane, x, y, ours, oracle)`.
    ///
    /// lane-av1loss444kf: the 4:4:4 lossless 256x256 cell's inter frames carry a
    /// SEPARATE, already-diagnosed walk defect (the per-mu-chunk lossless
    /// chroma span, `lanes/av1loss444mm.report.md` r1), so a whole-stream
    /// byte-exactness assert cannot name this one. `aomdec --limit=1` stops
    /// after the key frame, which is exactly the frame the reach fix moves.
    #[allow(clippy::type_complexity)]
    pub(super) fn key_frame_diffs(
        stream: &[u8],
        name: &str,
    ) -> (usize, Option<(usize, usize, usize, u8, u8)>) {
        let dir = std::env::temp_dir().join(format!("ec-av1-{name}-kf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, stream).expect("writing the stream");
        let aom_prefix = dir.join("aomkf");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "--limit=1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{name}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ourskf");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let decoded = decode_stream(stream);
        set_final_dump_prefix(None);
        decoded.unwrap_or_else(|e| panic!("{name}: this decoder refused the stream: {e}"));
        let want = std::fs::read(format!("{}.f0", aom_prefix.display())).expect(
            "the oracle wrote no decode-order frame 0 -- rung 12 of \
                     scripts/instrument-aom-oracle.sh is missing",
        );
        let got = std::fs::read(format!("{}.f0", ours_prefix.display()))
            .expect("this decoder wrote no decode-order frame 0");
        assert_eq!(
            got.len(),
            want.len(),
            "{name}: our key frame is {} bytes, the oracle's is {}",
            got.len(),
            want.len()
        );
        let side = (want.len() / 3).isqrt();
        let differing = got.iter().zip(&want).filter(|(a, b)| a != b).count();
        let first = got.iter().zip(&want).position(|(a, b)| a != b).map(|at| {
            // the dump is plane after plane (Y, U, V), so the in-plane
            // offset is what carries the x/y split
            let plane = at / (side * side);
            let in_plane = at % (side * side);
            (plane, in_plane % side, in_plane / side, got[at], want[at])
        });
        let _ = std::fs::remove_dir_all(&dir);
        (differing, first)
    }

    /// lane-av1cfl: per-plane sample-mismatch counts between this decoder's
    /// final stored frames and the instrumented oracle's, summed over every
    /// decode-order frame. Returns `[Y, U, V]`.
    ///
    /// [`decode_all_frames_vs_oracle`] panics on the first differing byte,
    /// which names an offset but not WHICH PLANE the offset falls in -- and
    /// for a chroma-scoped defect the plane split is the whole diagnosis
    /// (luma byte-exact while U and V both diverge is the chroma-sizing
    /// signature; all three diverging is not).
    ///
    /// `plane_samples` is each plane's own sample count in DECODE order
    /// (`[Y, U, V]`) and is read off this decoder's own frame rather than
    /// derived from the dimensions: at 4:2:0 the chroma planes are half-size
    /// and at 4:4:4 full-size, and a helper that assumed one layout would
    /// report every chroma byte as a luma byte. 8-bit only: the dump is one
    /// byte per sample at 8-bit and two at 10/12.
    pub(super) fn oracle_plane_diffs(stream: &[u8], name: &str, plane_samples: [usize; 3]) -> [usize; 3] {
        let dir = std::env::temp_dir().join(format!("ec-av1-{name}-planes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{name}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut oracle_frames = 0usize;
        while std::path::PathBuf::from(format!("{}.f{oracle_frames}", aom_prefix.display()))
            .is_file()
        {
            oracle_frames += 1;
        }
        assert!(
            oracle_frames > 0,
            "{name}: the oracle wrote no EC_AV1_FINAL_DUMP frames -- rung 12 of \
             scripts/instrument-aom-oracle.sh is missing from {}",
            aomdec_path().display()
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let decoded = decode_stream(stream);
        set_final_dump_prefix(None);
        let _ = decoded.expect("the caller already decoded this stream cleanly");
        let total: usize = plane_samples.iter().sum();
        // Plane boundaries in the concatenated dump, in DECODE order.
        let mut starts = [0usize; 3];
        for p in 1..3 {
            starts[p] = starts[p - 1] + plane_samples[p - 1];
        }
        let mut per_plane = [0usize; 3];
        for i in 0..oracle_frames {
            let want =
                std::fs::read(format!("{}.f{i}", aom_prefix.display())).expect("oracle dump");
            let got = std::fs::read(format!("{}.f{i}", ours_prefix.display())).expect("our dump");
            assert_eq!(
                got.len(),
                total,
                "{name}: decode-order frame {i} is {} bytes, expected {plane_samples:?} at 8-bit",
                got.len()
            );
            for p in 0..3 {
                let (a, b) = (starts[p], starts[p] + plane_samples[p]);
                per_plane[p] += got[a..b]
                    .iter()
                    .zip(&want[a..b])
                    .filter(|(x, y)| x != y)
                    .count();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        per_plane
    }

    /// Shared body for the lane-av1superpin 4:4:4 superres arms. It is the
    /// body of [`a_444_lossy_superres_stream_decodes_pixel_exact`] with the
    /// single `(denominator, coded_width)` constant generalised to a PER-FRAME
    /// table, which is what the mode-2 cell needs: libaom picks a different
    /// denominator for each frame there, so a single constant would either
    /// describe frame 0 only or be a literal the arithmetic below could not
    /// hold.
    ///
    /// Everything that makes the den=12 cell non-vacuous is here, unchanged:
    /// the pinned bytes (length + FNV-1a), 4:4:4 / 8-bit / superres-enabled
    /// from the sequence header, `use_superres` on every parsed frame header,
    /// the per-frame parsed denominator and coded size, and the coded width
    /// RE-DERIVED as `(UpscaledWidth*8 + denom/2)/denom` from that frame's OWN
    /// parsed denominator rather than compared to a literal alone -- so a
    /// re-pinned fixture cannot satisfy the literals while the scaled-size
    /// arithmetic moves under them. Then `superres_hits` and
    /// `predict_scaled_hits` as DELTAS (both counters are process-wide with no
    /// reset, so absolute values would pass on residue from another stream),
    /// the returned frames at the UPSCALED width with full-resolution 4:4:4
    /// chroma planes, and every decode-order frame byte-exact against the
    /// instrumented aomdec.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn a_444_superres_arm(
        name: &str,
        fixture: &str,
        fixture_len: usize,
        fixture_fnv: u64,
        fixture_sha: &str,
        upscaled: (usize, usize),
        // `(denominator, coded_width)` per DECODE-ORDER frame.
        per_frame: &[(u8, usize)],
    ) -> Vec<ec_av1_syntax::FrameHeader> {
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{name}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), fixture_len, "{name}: fixture length moved");
        assert_eq!(fnv1a64(&stream), fixture_fnv, "{name}: fixture bytes moved");

        // --- the header facts the CELL depends on, parsed not assumed ------
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut seq = None;
        let mut heads: Vec<FrameHeader> = Vec::new();
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{name}: aomenc OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            match obu.kind {
                ObuKind::SequenceHeader(s) => seq = Some(s),
                ObuKind::FrameHeader(h) => heads.push(*h),
                ObuKind::Frame(h, _) => heads.push(*h),
                _ => {}
            }
        }
        let seq = seq.unwrap_or_else(|| panic!("{name}: the stream carries no sequence header"));
        assert_eq!(
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            ),
            (0, 0),
            "{name}: the stream is not 4:4:4 (ss = ({}, {}))",
            seq.color_config.subsampling_x,
            seq.color_config.subsampling_y
        );
        assert_eq!(
            seq.color_config.bit_depth, 8,
            "{name}: not the 8-bit cell this gate names"
        );
        assert!(
            seq.enable_superres,
            "{name}: the sequence header does not enable superres at all"
        );
        assert_eq!(
            heads.len(),
            per_frame.len(),
            "{name}: {} frame headers, expected {}",
            heads.len(),
            per_frame.len()
        );
        for (i, (h, (denom, coded))) in heads.iter().zip(per_frame).enumerate() {
            // The assertion the phantom cell failed. Without a denominator
            // flag aomenc picks 8 and this is false while the stream still
            // decodes, which is how a whole sweep cell ends up mislabelled.
            assert!(
                h.use_superres,
                "{name}: frame {i} has use_superres = false -- this is an UNSCALED \
                 stream and the gate would be measuring something other than the \
                 cell it names (class gate-blind-to-feature)"
            );
            assert_eq!(
                h.superres_denom, *denom,
                "{name}: frame {i} PARSED denominator moved from {denom} -- read the \
                 header, never the encoder flag (mode 2 chooses its own)"
            );
            assert_eq!(
                (h.upscaled_width as usize, h.frame_height as usize),
                upscaled,
                "{name}: frame {i} upscaled size moved"
            );
            assert_eq!(
                (h.frame_width as usize, h.frame_height as usize),
                (*coded, upscaled.1),
                "{name}: frame {i} CODED size moved -- this is the scaled half of \
                 the cell and the part an unsized stream gets wrong"
            );
            // Re-derive rather than only compare literals, so a re-pinned
            // fixture cannot satisfy the numbers above while the scaled-size
            // arithmetic has moved under them. The denominator here is the
            // one PARSED out of this frame, so the check cannot be satisfied by
            // a literal that merely happens to agree.
            let d = u32::from(*denom);
            assert_eq!(
                (h.upscaled_width * 8 + d / 2) / d,
                h.frame_width,
                "{name}: frame {i}: FrameWidth is not (UpscaledWidth*8 + denom/2)/denom -- \
                 the scaled size the cell rests on is not the spec's"
            );
        }

        // --- the pixels -----------------------------------------------------
        let _guard = lock_gate_counters();
        let before_sr = crate::superres::superres_hits();
        let before_scaled = crate::mc::predict_scaled_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{name}: the pinned 4:4:4 superres stream was refused: {e}")
        });
        let scaled_up = crate::superres::superres_hits() - before_sr;
        let scaled_mc = crate::mc::predict_scaled_hits() - before_scaled;
        assert!(
            scaled_up > 0,
            "{name}: zero superres_hits -- the upscaler never ran (class \
             gate-blind-to-feature)"
        );
        assert!(
            scaled_mc > 0,
            "{name}: zero predict_scaled_hits -- no inter block read a scaled reference, \
             so the scaled-PREDICTION half of the cell is unexercised (class \
             gate-blind-to-feature)"
        );
        assert_eq!(frames.len(), per_frame.len(), "{name}: decoded frame count");
        for (i, f) in frames.iter().enumerate() {
            // The upscale must have REPLACED the frame: a decoder that returned
            // the coded-width picture fails here, and without this check the
            // chroma-shape assertion below could be satisfied by coded-size
            // planes.
            assert_eq!(
                (f.width, f.height),
                upscaled,
                "{name}: frame {i} came back {}x{}, not the UPSCALED {}x{}",
                f.width,
                f.height,
                upscaled.0,
                upscaled.1
            );
            for (plane, len) in [("U", f.u.len()), ("V", f.v.len())] {
                assert_eq!(
                    len,
                    upscaled.0 * upscaled.1,
                    "{name}: frame {i} {plane} plane is {len} samples; at 4:4:4 upscaled it \
                     must be full resolution ({}x{})",
                    upscaled.0,
                    upscaled.1
                );
            }
        }
        if aomdec_available(name) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, name);
            eprintln!(
                "{name}: {decoded} decode-order frame(s) ({hidden} hidden) byte-exact vs \
                 aomdec at {upscaled:?} upscaled from {per_frame:?} (denominator, coded \
                 width) per frame, {scaled_up} upscale(s), {scaled_mc} scaled MC block(s); \
                 fixture sha256 {fixture_sha}"
            );
        }
        heads
    }
