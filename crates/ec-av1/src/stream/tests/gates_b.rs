use super::*;
use super::support::*;
use super::gates_a::*;

    /// lane-cwarp r1: a real `--enable-global-motion=1 --enable-dist-wtd-comp=1`
    /// stream over a rotating/zooming source, so a `GLOBAL_GLOBALMV` compound
    /// block lands under a ROTZOOM/AFFINE model and each of its two references
    /// is predicted through its OWN global warp
    /// (`crate::warp::warp_affine_compound`, libaom `av1_warp_plane` with
    /// `conv_params->is_compound`) rather than translationally. Before r1 that
    /// path was SILENTLY WRONG -- no refusal named it, both taps went through
    /// `mc::predict_compound_intermediate`. `compound_warp_hits() > 0` is a
    /// hard assert: a run where the encoder never emitted one proves nothing.
    /// A decode error or a pixel mismatch is a FAILURE, never a skip.
    pub(super) fn run_compound_global_warp_gate(name: &str, bd: u32, min_part: u32, leaf8_assert: bool) {
        let _gate_lock = lock_gate_counters();
        assert!(have_ffmpeg(), "{name}: ffmpeg required");
        assert!(
            have_aomenc(),
            "{name}: no aomenc at {}",
            aomenc_path().display()
        );
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let before = crate::decode::compound_warp_hits();
        let before8 = crate::decode::compound_warp_hits_8();
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut attempts = 0u32;
        for cq in [32u32, 45, 55] {
            for (i, rate) in [0.010f64, 0.022].iter().enumerate() {
                attempts += 1;
                let duration = frame_count as f64 / 25.0;
                // A rotation about the frame centre plus a slow zoom is what
                // aomenc's global-motion search fits a ROTZOOM/AFFINE model to;
                // mandelbrot gives it real, non-flat texture to track.
                let source = format!(
                    "mandelbrot=size={w}x{h}:rate=25:start_x={sx}:start_y=-0.4,\
                     rotate=a={rate}*n:ow={w}:oh={h}:c=black,scale={width}:{height}",
                    w = width * 2,
                    h = height * 2,
                    sx = -0.6 + 0.01 * (i as f64),
                );
                let pix_fmt = match bd {
                    8 => "yuv420p",
                    10 => "yuv420p10le",
                    _ => "yuv420p12le",
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
                        &duration.to_string(),
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
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let (in_depth_arg, depth_arg) = (
                    format!("--input-bit-depth={bd}"),
                    format!("--bit-depth={bd}"),
                );
                let mut args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--auto-alt-ref=1",
                    "--lag-in-frames=25",
                    "--enable-fwd-kf=0",
                    "--enable-order-hint=1",
                    // the two tools this gate exists for
                    "--enable-global-motion=1",
                    "--enable-dist-wtd-comp=1",
                    "--enable-warped-motion=1",
                    "--enable-obmc=1",
                    "--enable-masked-comp=1",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-comp=1",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=1",
                    "--tune-content=default",
                    // partition/intra tools this decoder does not code yet --
                    // same exclusion list every compound gate carries.
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
                    "--enable-ref-frame-mvs=0",
                ];
                let min_part_arg = format!("--min-partition-size={min_part}");
                args.push(min_part_arg.as_str());
                if bd != 8 {
                    args.extend_from_slice(&[in_depth_arg.as_str(), depth_arg.as_str()]);
                }
                args.extend_from_slice(&["--obu", "-o", "-", "-"]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{name} failed outright, not a named refusal (cq {cq}): {msg}"
                        );
                        assert!(
                            !msg.contains("warp"),
                            "{name} refused on warp (cq {cq}) -- warp decode is ported: {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{name} cq {cq} rate {rate} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let want = match bd {
                    8 => ffmpeg_decode_sequence(&stream, width, height, frame_count),
                    10 => ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count),
                    _ => ffmpeg_decode_sequence_12bit(&stream, width, height, frame_count),
                };
                assert_eq!(frames.len(), frame_count);
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP")
                    && frames
                        .iter()
                        .zip(&want)
                        .any(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v)
                {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (cq {cq}) to {path}");
                }
                for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
                    assert_eq!(got.y, want.y, "{name} frame {i} luma vs ffmpeg (cq {cq})");
                    assert_eq!(got.u, want.u, "{name} frame {i} U vs ffmpeg (cq {cq})");
                    assert_eq!(got.v, want.v, "{name} frame {i} V vs ffmpeg (cq {cq})");
                }
                matched += 1;
            }
        }
        let hits = crate::decode::compound_warp_hits() - before;
        let hits8 = crate::decode::compound_warp_hits_8() - before8;
        eprintln!(
            "{name}: {matched}/{attempts} pixel-exact, {named_refusals} named refusals, \
             compound_warp_hits={hits} compound_warp_hits_8={hits8}"
        );
        if leaf8_assert {
            assert!(
                hits8 > 0,
                "{name}: no 8x8 COMPOUND leaf was predicted through a per-ref global \
                 warp ({hits} bigger compound warp blocks, {matched} matches, \
                 {named_refusals} refusals of {attempts}) -- gate vacuous"
            );
        } else {
            assert!(
                hits > 0,
                "{name}: no compound block was predicted through a per-ref global warp \
                 ({matched} matches, {named_refusals} refusals of {attempts}) -- gate vacuous"
            );
        }
        assert!(
            matched > 0,
            "{name}: every attempt refused -- no pixel comparison ran"
        );
    }

    /// lane-fimv r1 (class `wrong-alphabet-same-value`): libaom
    /// `motion_mode_allowed` (blockd.h ~1484) only returns `WARPED_CAUSAL`
    /// when `!xd->cur_frame_force_integer_mv`, so on a frame whose header set
    /// `force_integer_mv` an otherwise warp-eligible block reads the 2-symbol
    /// `obmc_cdf`, not the 3-symbol `motion_mode_cdf`. This decoder read the
    /// 3-symbol alphabet there (silent tile desync, no refusal named it).
    ///
    /// The fixture is screen content that is mostly static with only
    /// whole-pixel motion, which is exactly what aomenc's own
    /// `av1_is_integer_mv` (`encoder_utils.c:1357`, >=80% of blocks matching
    /// their collocated block) needs to set `cur_frame_force_integer_mv`
    /// under `--tune-content=screen` (seq `force_integer_mv == 2`, adaptive:
    /// `encoder.c:4171`). Two hard asserts, both class
    /// `gate-blind-to-feature`: at least one decoded frame header carried
    /// `force_integer_mv = 1`, and at least one block took the 2-symbol
    /// alphabet where `allow_warped_motion = 1` and `num_proj_ref >= 1` would
    /// have picked the 3-symbol one. A decode error or a pixel mismatch is a
    /// FAILURE, never a skip.
    #[track_caller]
    pub(super) fn run_fimv_motion_mode_gate(name: &str, ten_bit: bool) {
        let _gate_lock = lock_gate_counters();
        assert!(have_ffmpeg(), "{name}: ffmpeg required");
        assert!(
            have_aomenc(),
            "{name}: no aomenc at {}",
            aomenc_path().display()
        );
        let (width, height, frame_count) = (64usize, 64usize, 20usize);
        let fimv_frames_before = crate::decode::fimv_frame_hits();
        let alphabet_before = crate::decode::fimv_alphabet_hits();
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut attempts = 0u32;
        let mut refusals = Vec::new();
        for cq in [32u32, 45, 55] {
            for step in [4u32, 8] {
                attempts += 1;
                // lane-fimv r1: per-attempt snapshots. The aggregate
                // assertions below are satisfied by a single lucky cq/step
                // cell; these prove EVERY attempt that decoded and
                // pixel-compared really carried the header state and the
                // forced alphabet under test (class gate-blind-to-feature).
                let attempt_fimv_before = crate::decode::fimv_frame_hits();
                let attempt_alphabet_before = crate::decode::fimv_alphabet_hits();
                let duration = frame_count as f64 / 25.0;
                // Flat screen-content tiles with hard edges, translating by a
                // whole number of pixels per frame: static enough for
                // `av1_is_integer_mv`, moving enough for a real inter block
                // with overlappable neighbours.
                let source = format!(
                    "color=c=black:s={width}x{height}:r=25,\
                     drawbox=x='8+{step}*mod(trunc(t*25)\\,3)':y=8:w=20:h=20:color=white:t=fill,\
                     drawbox=x=36:y='6+{step}*mod(trunc(t*25)+1\\,3)':w=16:h=16:color=0x808080:t=fill,\
                     drawbox=x=4:y=44:w=56:h=12:color=0xC0C0C0:t=fill"
                );
                let pix_fmt = if ten_bit { "yuv420p10le" } else { "yuv420p" };
                let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        &source,
                        "-t",
                        &duration.to_string(),
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
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let mut args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    // single reference only: the 10-bit compound MC defect
                    // (see `a_real_compound_global_warp_10bit_stream_...`)
                    // is unrelated to this lane and would mask it.
                    "--auto-alt-ref=0",
                    "--lag-in-frames=0",
                    "--enable-fwd-kf=0",
                    "--enable-order-hint=1",
                    // the flag combination this gate exists for: warp allowed
                    // by the header, OBMC available, global motion off (so the
                    // GLOBALMV suppression is not what picks the alphabet).
                    "--tune-content=screen",
                    "--enable-warped-motion=1",
                    "--enable-obmc=1",
                    "--enable-global-motion=0",
                    "--enable-masked-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-dist-wtd-comp=0",
                    // partition/intra tools this decoder does not code yet --
                    // the same exclusion list every inter gate here carries.
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
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                ];
                if ten_bit {
                    args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
                }
                args.extend_from_slice(&["--obu", "-o", "-", "-"]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{name} failed outright (cq {cq} step {step}): {msg}"
                        );
                        named_refusals += 1;
                        refusals.push(format!("cq {cq} step {step}: {msg}"));
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let want = if ten_bit {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP")
                    && frames
                        .iter()
                        .zip(&want)
                        .any(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v)
                {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (cq {cq}) to {path}");
                }
                for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{name} frame {i} luma vs ffmpeg (cq {cq} step {step})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{name} frame {i} U vs ffmpeg (cq {cq} step {step})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{name} frame {i} V vs ffmpeg (cq {cq} step {step})"
                    );
                }
                assert!(
                    crate::decode::fimv_frame_hits() > attempt_fimv_before,
                    "{name} (cq {cq} step {step}): decoded and compared all \
                     frames but no frame header had force_integer_mv=1 -- \
                     this attempt is vacuous"
                );
                assert!(
                    crate::decode::fimv_alphabet_hits() > attempt_alphabet_before,
                    "{name} (cq {cq} step {step}): no block was forced onto the \
                     2-symbol obmc alphabet by force_integer_mv while \
                     allow_warped_motion=1 and num_proj_ref>=1 -- this attempt \
                     is vacuous"
                );
                matched += 1;
            }
        }
        let fimv_frames = crate::decode::fimv_frame_hits() - fimv_frames_before;
        let alphabet = crate::decode::fimv_alphabet_hits() - alphabet_before;
        eprintln!(
            "{name}: {matched}/{attempts} pixel-exact, {named_refusals} named refusals, \
             force_integer_mv frames={fimv_frames}, forced-2-symbol blocks={alphabet}\n{}",
            refusals.join("\n")
        );
        assert!(
            matched > 0,
            "{name}: every attempt refused -- no pixel comparison ran"
        );
        assert!(
            fimv_frames > 0,
            "{name}: no decoded frame header had force_integer_mv=1 \
             ({matched} matches of {attempts}) -- gate vacuous"
        );
        assert!(
            alphabet > 0,
            "{name}: no block read the 2-symbol obmc alphabet where \
             allow_warped_motion=1 and num_proj_ref>=1 would have picked the \
             3-symbol motion_mode alphabet -- gate vacuous"
        );
    }

    /// lane-mcomp64 r1: at 64x64 `av1_is_wedge_used` is false
    /// (`av1_wedge_params_lookup[BLOCK_64X64].wedge_types == 0`), so libaom's
    /// `read_compound_type` codes NO `compound_type` symbol and INFERS
    /// `COMPOUND_DIFFWTD`; only the 1-bit `mask_type` literal follows. This
    /// decoder used to refuse such a block by name ("a masked compound 64x64
    /// inter block..."), because reading the symbol would have desynced.
    ///
    /// The recipe pins every block to 64x64 (`--sb-size=64
    /// --max-partition-size=64 --min-partition-size=64`) with masked compound
    /// on and distance-weighted compound off, so the only way a
    /// `comp_group_idx == 1` block can be coded is through the inferred arm.
    /// `diffwtd_inferred_hits` is snapshotted PER ATTEMPT and only accumulated
    /// for attempts that decoded AND pixel-compared (class
    /// `counter-from-refused-stream`), and is hard-asserted at the end. A
    /// decode error or a pixel mismatch is a FAILURE, never a skip.
    #[track_caller]
    pub(super) fn run_diffwtd_inferred_64x64_gate(name: &str, ten_bit: bool) {
        let _gate_lock = lock_gate_counters();
        assert!(have_ffmpeg(), "{name}: ffmpeg required");
        assert!(
            have_aomenc(),
            "{name}: no aomenc at {}",
            aomenc_path().display()
        );
        let (width, height, frame_count) = (128usize, 128usize, 16usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut attempts = 0u32;
        let mut fired = 0usize;
        let mut refusals: Vec<String> = Vec::new();
        for cq in [32u32, 45, 55] {
            for (i, zoom) in [0.004f64, 0.010].iter().enumerate() {
                attempts += 1;
                let attempt_before = crate::decode::diffwtd_inferred_hits();
                let duration = frame_count as f64 / 25.0;
                // Hard-edged, moving content: a difference-weighted mask only
                // wins where the two references genuinely disagree over part
                // of a 64x64 block (a smooth `gradients` source measured
                // diffwtd_hits == 0, memory `gate-blind-to-feature`).
                let source = format!(
                    "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y=-0.4:\
                     end_scale=0.3:maxiter=200,\
                     noise=alls=6:allf=t+u:all_seed={seed}",
                    sx = -0.6 + zoom * (i as f64 + 1.0),
                    seed = 1000 + cq,
                );
                let pix_fmt = if ten_bit { "yuv420p10le" } else { "yuv420p" };
                let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        &source,
                        "-t",
                        &duration.to_string(),
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
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let mut args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--auto-alt-ref=1",
                    "--lag-in-frames=16",
                    "--enable-fwd-kf=0",
                    "--enable-order-hint=1",
                    // the tools this gate exists for: masked compound on, the
                    // difference-weighted arm on, the distance-weighted
                    // competitor off so comp_group_idx==1 is the encoder's
                    // only masked choice at a size where wedge does not exist.
                    "--enable-masked-comp=1",
                    "--enable-diff-wtd-comp=1",
                    "--enable-dist-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-global-motion=0",
                    "--enable-warped-motion=1",
                    "--enable-obmc=1",
                    "--tune-content=default",
                    // partition/intra tools this decoder does not code yet --
                    // same exclusion list every compound gate carries.
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
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    // every block is a whole 64x64 superblock: the ONLY size
                    // where compound_type is inferred rather than coded.
                    "--sb-size=64",
                    "--max-partition-size=64",
                    "--min-partition-size=64",
                ];
                if ten_bit {
                    args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
                }
                args.extend_from_slice(&["--obu", "-o", "-", "-"]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{name} failed outright, not a named refusal (cq {cq}): {msg}"
                        );
                        assert!(
                            !msg.contains("masked compound 64x64"),
                            "{name} refused the inferred-DIFFWTD 64x64 block (cq {cq}) -- \
                             that refusal is lifted, this is forbidden: {msg}"
                        );
                        named_refusals += 1;
                        refusals.push(msg.clone());
                        eprintln!("{name} cq {cq} variant {i} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let want = if ten_bit {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP")
                    && frames
                        .iter()
                        .zip(&want)
                        .any(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v)
                {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (cq {cq}) to {path}");
                }
                for (f, (got, want)) in frames.iter().zip(&want).enumerate() {
                    assert_eq!(got.y, want.y, "{name} frame {f} luma vs ffmpeg (cq {cq})");
                    assert_eq!(got.u, want.u, "{name} frame {f} U vs ffmpeg (cq {cq})");
                    assert_eq!(got.v, want.v, "{name} frame {f} V vs ffmpeg (cq {cq})");
                }
                // decoded AND compared: only now does this attempt's own
                // inferred-DIFFWTD count mean anything.
                fired += crate::decode::diffwtd_inferred_hits() - attempt_before;
                matched += 1;
            }
        }
        eprintln!(
            "{name}: {matched}/{attempts} pixel-exact, {named_refusals} named refusals, \
             diffwtd_inferred_hits (decoded+compared attempts only)={fired}"
        );
        assert!(
            matched > 0,
            "{name}: every attempt refused ({refusals:?}) -- no pixel compare ran"
        );
        assert!(
            fired > 0,
            "{name}: no inferred-DIFFWTD 64x64 block fired on a compared attempt \
             ({matched} matches, {named_refusals} refusals of {attempts}) -- gate vacuous"
        );
    }

    pub(super) fn both_axes_cut_sweep(
        name: &'static str,
        (arm, frames, kf_arg, limit_arg): (&str, usize, &str, &str),
    ) {
        // Kept upper-case so every message string below reads the same as in
        // the sibling gates.
        #[allow(non_snake_case)]
        let NAME = name;
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let sizes: &[(usize, usize)] = &[(200, 72), (136, 104), (264, 136), (72, 200), (104, 136)];
        let mut compared = 0u32;
        let mut refusals: Vec<String> = Vec::new();
        let mut totals = [0usize; 4];
        for &(width, height) in sizes {
            let mut size_compared = 0u32;
            {
                for bit_depth in [8u32, 10u32] {
                    // cq 58 is left out on purpose: measured r1, it is where
                    // libaom starts answering the frame-edge gathered symbol
                    // with a rect strip and other lanes' refusals fire first
                    // ("a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs
                    // rectangular residual coding").
                    for cq in [32u32, 45] {
                        let source = format!("mandelbrot=size={width}x{height}:rate=25");
                        // Feed exactly `frames` frames: --limit=1 makes
                        // aomenc stop reading after the first one and the
                        // y4m write then dies on a broken pipe.
                        let dur = format!("{}", frames as f64 / 25.0);
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
                                &dur,
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
                            "{NAME}: ffmpeg refused the {width}x{height} {bit_depth}-bit \
                             fixture: {}",
                            String::from_utf8_lossy(&y4m.stderr)
                        );
                        let depth_arg = format!("--bit-depth={bit_depth}");
                        let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                        let cq_arg = format!("--cq-level={cq}");
                        let out = run_with_stdin(
                            Command::new(aomenc_path()).args([
                                "--codec=av1",
                                "--passes=1",
                                "--end-usage=q",
                                &cq_arg,
                                // cpu-used >= 1 keeps libaom on 64x64
                                // superblocks at these sizes
                                // (`av1_select_sb_size`).
                                "--cpu-used=4",
                                "--threads=1",
                                "--row-mt=0",
                                // --lag-in-frames=0: no hidden alt-ref, so
                                // decode order == display order and the
                                // compare below covers every frame the stream
                                // carries (class gate-blind-to-hidden-frames).
                                kf_arg,
                                limit_arg,
                                "--lag-in-frames=0",
                                &depth_arg,
                                &input_depth_arg,
                                // Other lanes' named refusals this recipe must
                                // not trip. The both-cut corner is FORCED down
                                // to 8x8 leaves, which is exactly where the
                                // 8x8-intra-leaf-in-an-inter-frame gaps live,
                                // so the intra tool set and tx-size search are
                                // pinned off: without them 30/30 inter
                                // attempts refused (measured r1) on "a nonzero
                                // angle delta on an 8x8 intra leaf in an inter
                                // frame", "a non-DC chroma mode on an 8x8
                                // inter-frame leaf" and "an 8x8 intra leaf in
                                // an inter frame whose tx_depth splits it into
                                // 4x4 transform units".
                                "--enable-rect-partitions=0",
                                "--enable-ab-partitions=0",
                                "--enable-1to4-partitions=0",
                                "--min-partition-size=8",
                                "--max-partition-size=64",
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

                        // Reset, never a delta across attempts: a refused
                        // attempt's hits must not count (class
                        // counter-from-refused-stream).
                        crate::decode::reset_edge_both_cut_hits();
                        let decoded = match decode_stream(&stream) {
                            Ok(f) => f,
                            Err(e) => {
                                let msg = e.to_string();
                                assert!(
                                    msg.contains("unsupported"),
                                    "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} \
                                     failed outright, not a named refusal: {msg}"
                                );
                                eprintln!(
                                    "  {NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} \
                                     REFUSED {msg}"
                                );
                                refusals.push(msg);
                                continue;
                            }
                        };
                        let hits = crate::decode::edge_both_cut_hits();
                        let reference = if bit_depth == 10 {
                            ffmpeg_decode_sequence_10bit(&stream, width, height, frames)
                        } else {
                            ffmpeg_decode_sequence(&stream, width, height, frames)
                        };
                        assert_eq!(
                            decoded.len(),
                            frames,
                            "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} decoded {} \
                             frames, expected {frames}",
                            decoded.len()
                        );
                        assert_eq!(
                            reference.len(),
                            frames,
                            "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} reference \
                             frame count"
                        );
                        for (i, (ours, theirs)) in decoded.iter().zip(&reference).enumerate() {
                            let nd = |a: &[u16], b: &[u16]| {
                                a.iter().zip(b).filter(|(x, y)| x != y).count()
                            };
                            // Where a mismatch lands is the whole diagnosis
                            // here (corner block vs elsewhere), so print the
                            // luma bounding box before the assert kills the run.
                            if nd(&ours.y, &theirs.y) > 0 {
                                let (mut r0, mut c0, mut r1, mut c1) =
                                    (usize::MAX, usize::MAX, 0usize, 0usize);
                                for row in 0..height {
                                    for col in 0..width {
                                        if ours.y[row * width + col] != theirs.y[row * width + col]
                                        {
                                            r0 = r0.min(row);
                                            c0 = c0.min(col);
                                            r1 = r1.max(row);
                                            c1 = c1.max(col);
                                        }
                                    }
                                }
                                eprintln!(
                                    "  {NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} \
                                     frame {i} luma diff bbox rows {r0}..={r1} cols {c0}..={c1}"
                                );
                            }
                            assert_eq!(
                                (
                                    nd(&ours.y, &theirs.y),
                                    nd(&ours.u, &theirs.u),
                                    nd(&ours.v, &theirs.v)
                                ),
                                (0, 0, 0),
                                "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} frame \
                                 {i} differs from ffmpeg"
                            );
                            for (plane, a, b) in [
                                ("Y", &ours.y, &theirs.y),
                                ("U", &ours.u, &theirs.u),
                                ("V", &ours.v, &theirs.v),
                            ] {
                                assert_eq!(
                                    a.len(),
                                    b.len(),
                                    "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} \
                                     frame {i} plane {plane}: ours has {} samples, ffmpeg's has \
                                     {} -- a short plane would zip-truncate the diff count \
                                     above and hide a chroma-span defect",
                                    a.len(),
                                    b.len()
                                );
                            }
                        }
                        // Every compared attempt must carry the case, so no
                        // compared attempt is out of scope and
                        // out_of_scope_mismatch is 0 by construction.
                        assert!(
                            hits[0] > 0,
                            "{NAME}: {width}x{height} {arm} {bit_depth}-bit cq {cq} decoded \
                             pixel-exact but no 16-level both-axes-cut block fired \
                             (hits={hits:?}) -- the recipe no longer reaches the lifted arm"
                        );
                        for l in 0..4 {
                            totals[l] += hits[l];
                        }
                        compared += 1;
                        size_compared += 1;
                    }
                }
            }
            assert!(
                size_compared > 0,
                "{NAME}: every {width}x{height} attempt refused -- that size proved nothing"
            );
        }
        assert!(
            totals[1] > 0 && totals[2] > 0,
            "{NAME}: no 32-level ({}) or 64-level ({}) both-axes-cut node fired -- 200x72 puts \
             both in that state, so the sweep stopped reaching them",
            totals[1],
            totals[2]
        );
        eprintln!(
            "{NAME}: {compared} attempts pixel-exact over {} sizes ({arm} arm), \
             out_of_scope_mismatch=0, both_cut hits [16,32,64,128]={totals:?}, {} named refusals",
            sizes.len(),
            refusals.len()
        );
    }

    /// lane-av1tmvp: the temporal-MV gate -- unlike every `_single_ref_gate`
    /// attempt above, this one leaves `--enable-ref-frame-mvs` at aomenc's
    /// own default (on) instead of pinning it off, so `use_ref_frame_mvs`
    /// actually fires and `decode::tmv_hits()` (this gate's own capability
    /// proof, not just `non_last_ref_hits`) has to advance for the run to
    /// count as a real pass rather than a `never_fired` skip.
    pub(super) fn a_real_aomenc_temporal_mv_gate(seed_base: u32, attempts: u32) {
        let gate_name = "a_real_aomenc_temporal_mv_gate";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {gate_name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {gate_name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 16usize);
        let mut never_fired = 0u32;
        let mut refusals = Vec::new();
        for attempt in 0..attempts {
            let seed = seed_base + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
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
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // Same envelope as `a_real_aomenc_single_ref_gate`, minus its
            // `--enable-ref-frame-mvs=0` pin -- everything else this decoder
            // still cannot code stays refused the same way.
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
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
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::tmv_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{gate_name} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::tmv_hits() != before;
            if !counted {
                never_fired += 1;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                if mismatched {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{gate_name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{gate_name} frame {i} U vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.v, want.v,
                    "{gate_name} frame {i} V vs ffmpeg (seed {seed})"
                );
            }
            if counted {
                eprintln!(
                    "{gate_name} FIRING seed {seed}: tmv_hits advanced by {}",
                    decode::tmv_hits() - before
                );
                gate_buckets(gate_name, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(gate_name, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {gate_name}: {never_fired} attempts decoded but never fired a temporal MV \
             candidate, and every other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-mvtwin r1: the COMPOUND MV-stack gate. Every other compound gate
    /// here pins `--enable-ref-frame-mvs=0`, so `find_mv_stack_compound`'s
    /// temporal half (the three `tpl_sample_pos` extension samples lane-sb128
    /// r3 found missing) and its extension pass have never been exercised by
    /// a real encoder. This arm turns ref-frame-mvs ON, gives the encoder a
    /// 25-frame lookahead plus alt-refs so `reference_select` really drives
    /// blocks to COMPOUND_REFERENCE, and sweeps global motion off/on so the
    /// combine step's `gm_mv_candidates` fill (r1 defect 1) is reachable.
    ///
    /// Firing proof, hard-asserted: `decode::comp_mode_hits` (a real
    /// compound block) AND `mvstack::tmv_hits` (a temporal candidate landed
    /// in some stack) must BOTH have advanced on an attempt that also
    /// decoded pixel-exact -- class [[gate-skips-on-its-own-failure]]: a
    /// decode that succeeds is ALWAYS compared, on every decode-order frame,
    /// and only the counters decide whether the attempt counts.
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn a_real_aomenc_compound_mv_stack_gate(
        gate_name: &str,
        seed_base: u32,
        attempts: u32,
        bit_depth: u32,
        sb_size: u32,
        global_motion: u32,
        width: usize,
        tile_columns: u32,
    ) {
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {gate_name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {gate_name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (height, frame_count) = (64usize, 16usize);
        let mut never_fired = 0u32;
        let mut refusals = Vec::new();
        for attempt in 0..attempts {
            let seed = seed_base + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    if bit_depth == 10 {
                        "yuv420p10le"
                    } else {
                        "yuv420p"
                    },
                    "-strict",
                    "-1",
                    "-t",
                    "0.64",
                    "-f",
                    "yuv4mpegpipe",
                    "-strict",
                    "-1",
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
            let depth_args: &[&str] = if bit_depth == 10 {
                &["-b", "10", "--input-bit-depth=10"]
            } else {
                &[]
            };
            // The cq sweep: how finely RD partitions decides how often a
            // block's stack comes up short enough to reach the extension
            // pass at all.
            let cq = format!("--cq-level={}", [45u32, 55, 35][(attempt % 3) as usize]);
            let sb = format!("--sb-size={sb_size}");
            let gm = format!("--enable-global-motion={global_motion}");
            // `--tile-columns` is log2: 1 means two tile columns, which is
            // what makes every block in the right-hand tile carry a nonzero
            // `tile_col0` and so exercise r1 defect 3's reach clamp.
            let tiles = format!("--tile-columns={tile_columns}");
            // class [[aomenc-last-flag-wins]]: every flag appears exactly
            // once below, so there is no override to order.
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args(depth_args)
                    .arg(&cq)
                    .arg(&sb)
                    .arg(&gm)
                    .arg(&tiles)
                    .args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cpu-used=0",
                        "--kf-max-dist=1000",
                        "--threads=1",
                        "--row-mt=0",
                        // What this gate is about: real compound references
                        // with a real temporal MV field behind them.
                        "--enable-ref-frame-mvs=1",
                        "--enable-order-hint=1",
                        "--auto-alt-ref=1",
                        "--lag-in-frames=25",
                        "--enable-fwd-kf=0",
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
                        "--min-partition-size=32",
                        "--enable-palette=0",
                        "--enable-intrabc=0",
                        "--enable-cfl-intra=0",
                        "--obu",
                        "-o",
                        "-",
                        "-",
                    ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let (before_comp, before_tmv) = (decode::comp_mode_hits(), decode::tmv_hits());
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{gate_name} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let counted =
                decode::comp_mode_hits() != before_comp && decode::tmv_hits() != before_tmv;
            if !counted {
                never_fired += 1;
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            // Every SHOWN/display frame `decode_stream` returned is compared --
            // that is decode_stream's whole output, and nothing else is.
            // HIDDEN frames are NOT covered here: they are never emitted as
            // pictures, so no ffmpeg picture corresponds to them (class
            // [[gate-blind-to-hidden-frames]]).
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{gate_name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{gate_name} frame {i} U vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.v, want.v,
                    "{gate_name} frame {i} V vs ffmpeg (seed {seed})"
                );
            }
            if counted {
                eprintln!(
                    "{gate_name} FIRING seed {seed}: comp_mode_hits +{}, tmv_hits +{}, \
                     comp_gm_fill_hits={}, stack_full_drops={}, tile_reach_clips={}",
                    decode::comp_mode_hits() - before_comp,
                    decode::tmv_hits() - before_tmv,
                    crate::mvstack::comp_gm_fill_hits(),
                    crate::mvstack::stack_full_drops(),
                    crate::mvstack::tile_reach_clips(),
                );
                gate_buckets(gate_name, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(gate_name, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {gate_name}: {never_fired} attempts decoded but never drove a compound block \
             with a temporal MV candidate, and every other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-seg: a real `aomenc` stream with SEGMENTATION genuinely active.
    /// `--aq-mode=1` (variance AQ) / `--aq-mode=2` (complexity AQ) are the two
    /// paths libaom uses to switch `segmentation_enabled` on by itself
    /// (`av1_vaq_frame_setup`/`av1_setup_in_frame_q_adj`, both of which only
    /// ever set `SEG_LVL_ALT_Q`); `--deltaq-mode=0` keeps the per-superblock
    /// delta-q reader out of the picture so a mismatch points at the segment
    /// map, not at `CurrentQIndex`. Mandelbrot's varying local contrast is
    /// what gives variance/complexity AQ different segments to assign --
    /// a flat gradient collapses every block onto one segment, which the
    /// `segment_ids_seen() >= 2` assert below would catch as a vacuous run.
    /// The rest of the recipe is the delta-q gate's own feature-disable
    /// envelope, so this gate isolates the segmentation reader.
    pub(super) fn run_segmentation_gate(
        name: &str,
        aq_mode: &str,
        bit_depth: usize,
        frame_count: usize,
        rate_args: &[&str],
    ) {
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        crate::decode::reset_segment_hits();
        let (width, height) = (128usize, 64usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SEG_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source = format!(
                "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                sx = -0.6 + 0.005 * (attempt as f64),
                sy = -0.4 + 0.005 * (attempt as f64)
            );
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
                    &duration.to_string(),
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
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let depth_args: Vec<String> = if bit_depth == 10 {
                vec![
                    "--input-bit-depth=10".to_owned(),
                    "--bit-depth=10".to_owned(),
                ]
            } else {
                Vec::new()
            };
            let aq = format!("--aq-mode={aq_mode}");
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--cpu-used=4",
                &aq,
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
                "--enable-dist-wtd-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=1",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
            ];
            args.extend(rate_args.iter().copied());
            args.extend(depth_args.iter().map(String::as_str));
            args.extend(["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            // The stream must really carry `segmentation_enabled`, or the
            // gate proves nothing about the reader.
            let mut seg_frames = 0u32;
            {
                let mut probe = Av1Parser::new();
                let mut pos = 0usize;
                while pos < stream.len() {
                    let obu = probe
                        .parse_obu(&stream[pos..])
                        .expect("parsing the gate stream");
                    pos += obu.total_size;
                    match &obu.kind {
                        ObuKind::Frame(h, _) => {
                            if h.segmentation.enabled {
                                seg_frames += 1;
                            }
                        }
                        ObuKind::FrameHeader(h)
                            if h.segmentation.enabled => {
                                seg_frames += 1;
                            }
                        _ => {}
                    }
                }
            }
            if seg_frames == 0 {
                eprintln!(
                    "seed {seed}: aomenc wrote no segmentation-enabled frame, skipping attempt"
                );
                continue;
            }
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("segment_id symbol"),
                        "{name} refused on the segmentation reader itself (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{name} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{name} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{name}: every attempt refused on other capabilities ({named_refusals} refusals); \
             the gate never exercised segmentation"
        );
        assert!(
            crate::decode::segment_id_hits() > 0,
            "{name}: {matched} pixel-exact matches but zero segment_id symbols fired -- \
             the reader is unexercised"
        );
        assert!(
            crate::decode::segment_ids_seen() >= 2,
            "{name}: {matched} pixel-exact matches but only {} distinct segment id(s) -- \
             every block landed in one segment, so nothing was actually segmented",
            crate::decode::segment_ids_seen()
        );
        // An inter-frame run must also have exercised the TEMPORAL path
        // (`seg_id_predicted` against the primary reference's saved map);
        // the key-frame-only run has no previous map by construction.
        if frame_count > 1 {
            assert!(
                crate::decode::segment_pred_hits() > 0,
                "{name}: {matched} pixel-exact matches over {frame_count} frames but zero \
                 seg_id_predicted symbols -- the temporal_update path is unexercised"
            );
        }
        eprintln!(
            "{name}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, segment_id_hits={}, distinct segment ids={}, \
             seg_id_predicted symbols={}",
            crate::decode::segment_id_hits(),
            crate::decode::segment_ids_seen(),
            crate::decode::segment_pred_hits()
        );
    }

    pub(super) fn edge32_gate(name: &str, flat_band: bool) {
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // (width, height, frames, ten_bit, tile_columns_log2)
        let mut arms: Vec<(usize, usize, usize, bool, u32)> = Vec::new();
        for &(w, h) in &[(192usize, 80usize), (80usize, 192usize)] {
            arms.push((w, h, 1, false, 0));
            arms.push((w, h, 1, true, 0));
        }
        // The inter arm: >= 4 frames so the 32-level edge path in the INTER
        // tile walk is exercised too.
        arms.push((192, 80, 5, false, 0));
        // lane-golomb r9: the straddling-band arms, only under the ignored
        // `a_frame_edge_straddling_band_decodes_pixel_exact` name (they are
        // RED -- see that test's own comment for the stage bisection). Naming
        // them here rather than in a second copy of this gate keeps the
        // recipe, the counters and the refusal handling identical.
        if name == "a_frame_edge_straddling_band_decodes_pixel_exact" {
            arms.clear();
            for &tile_cols in &[0u32, 1] {
                arms.push((192, 68, 5, false, tile_cols));
                arms.push((192, 68, 5, true, tile_cols));
                arms.push((68, 192, 5, false, tile_cols));
                arms.push((68, 192, 5, true, tile_cols));
            }
        }
        // lane-golomb r8 MEASURED, arm STILL NOT added (every cq level is
        // vacuous, panicking or red -- each measured this round with
        // `arms.push((192, 68, 5, false))`, log
        // `$HOME/.cache/golomb-gate-r8{b,e,f,g}.log`):
        //   * the r7 blocker IS fixed -- no attempt refuses "a reference
        //     picture whose height does not match this frame's own true
        //     size" any more (`decode.rs`'s comparison now uses the header's
        //     frame height, per libaom's crop-dimension reference test).
        //   * cq35 now refuses LATER, by another lane's name: "an inter
        //     SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B)" -- so the
        //     arm compares nothing (class `counter-from-refused-stream`).
        //   * cq40 PANICS in `decode_inter_block8`
        //     (`from_switchable_symbol` handed a 4th symbol, `mc.rs:203`) =
        //     lane-sub8's own open below-8x8 desync; a panic cannot be
        //     tolerated as a named refusal.
        //   * cq59 8-bit is RED: `frame 1 plane Y: 217 pixels differ, first
        //     at row 60 col 170 (ours 172 vs ffmpeg 173)`, and the cq35
        //     10-bit twin is RED at `row 56 col 48 (ours 530 vs ffmpeg
        //     531)`. Both are +-1 bands in rows 56..63, i.e. the deblock/CDEF
        //     bleed ABOVE the 4-px straddling row band (64..67) -- the same
        //     shape as the `(68, 192, 5, false)` column defect below, so the
        //     two are very likely ONE straddling-band reconstruction defect.
        // lane-golomb r7 MEASURED, arm NOT added (it would be vacuous or red):
        // a 68-px axis leaves the last superblock straddling by 4 px, the
        // shape `read_var_tx_size`'s `blk_row >= max_h_mi || blk_col >=
        // max_w_mi` early return exists for. With
        // `arms.push((192, 68, 5, false))` all EIGHT cq attempts refuse by
        // name -- "a reference picture whose height does not match this
        // frame's own true size" -- so the arm compares nothing (class
        // `counter-from-refused-stream`). With
        // `arms.push((68, 192, 5, false))` the gate goes RED on a real,
        // separate defect: `68x192 cq35 frames=5 10bit=false frame 1 plane Y:
        // 141 pixels differ, first at row 0 col 64 (ours 167 vs ffmpeg 166)
        // [edge32=[0,34,0,0,1,17,0,1]]` -- LUMA, inside the 4-px straddling
        // COLUMN. Both are unrelated to this round's chroma tx_type fix.
        let cqs: [u32; 8] = [35, 40, 45, 50, 55, 57, 59, 61];
        let mut totals = [0usize; 8];
        let mut compared = 0usize;
        // lane-t900 r15 (class `counter-from-refused-stream`): the aggregate
        // `compared` counter is carried by the single-tile arms alone, so the
        // `--tile-columns=1` twins were free to refuse every attempt and keep
        // the gate green. Counted separately and asserted below.
        let mut compared_multi_tile = 0usize;
        let mut refusals: Vec<String> = Vec::new();
        let straddle = name == "a_frame_edge_straddling_band_decodes_pixel_exact";
        let mut straddle_units = 0usize;
        for &(width, height, frame_count, ten_bit, tile_cols) in &arms {
            for &cq in &cqs {
                let duration = frame_count as f64 / 25.0;
                // `noise` (explicit seed) both defeats aomenc's
                // screen-content detection -- which otherwise refuses every
                // attempt at "a HORZ/VERT intra strip in a screen-content
                // frame" -- and keeps the RD search off flat NONE blocks.
                // Natural content over the whole superblock-aligned area,
                // with the 16-px straddling band left FLAT (`pad`): with
                // detail in that band aomenc always answers the edge bit with
                // SPLIT (measured: 178 edge bits, 178 of them SPLIT, over a
                // cq 35..61 sweep), so the HORZ/VERT arm this round guards
                // never fired. `noise` on top keeps screen-content detection
                // (and its palette-strip refusal) off.
                let (cw, ch) = (
                    if flat_band && width % 64 != 0 {
                        width - 16
                    } else {
                        width
                    },
                    if flat_band && height % 64 != 0 {
                        height - 16
                    } else {
                        height
                    },
                );
                let pad = if flat_band {
                    format!("pad={width}:{height}:0:0:gray,")
                } else {
                    String::new()
                };
                let source = format!(
                    "testsrc2=size={cw}x{ch}:duration={duration}:rate=25,\
                     {pad}noise=alls=6:allf=t+u:all_seed={cq}"
                );
                let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
                let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        &source,
                        "-pix_fmt",
                        pix,
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
                    "{name}: ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let mut args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-intrabc=0",
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag).
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    // 32, not the charter's 8: at 8 the sub-16 AB and
                    // split-transform strip arms (still refused by name here)
                    // fire before the 32-level edge is reached and EVERY
                    // attempt refuses (measured). At 32 the edge block cannot
                    // split, so aomenc is forced onto the gathered edge bit's
                    // HORZ/VERT -- exactly the arm this round guards.
                    "--min-partition-size=32",
                    "--tune-content=film",
                    "--max-partition-size=64",
                ];
                if ten_bit {
                    args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
                }
                let tile_arg = format!("--tile-columns={tile_cols}");
                if tile_cols > 0 {
                    args.push(&tile_arg);
                }
                args.extend_from_slice(&["--obu", "-o", "-", "-"]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "{name}: aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                if ten_bit {
                    assert_eq!(
                        parsed_bit_depth(&stream),
                        10,
                        "{name}: aomenc did not write a 10-bit sequence header"
                    );
                }
                let tag = format!(
                    "{width}x{height} cq{cq} frames={frame_count} 10bit={ten_bit} \
                     tile_cols={tile_cols}"
                );
                let before = crate::decode::edge32_hits();
                let straddle_before = crate::decode::cdef_straddle_units();
                // lane-golomb r8: name the attempt BEFORE decoding it -- a
                // desync surfaces as a panic deep in mc.rs with no arm in the
                // message, and r8 spent a run bisecting arms by hand.
                if std::env::var_os("EC_GATE_VERBOSE").is_some() {
                    eprintln!("{name}: attempting {tag}");
                }
                let decoded = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{name} failed outright, not a named refusal ({tag}): {msg}"
                        );
                        refusals.push(format!("{tag}: {msg}"));
                        continue;
                    }
                };
                let after = crate::decode::edge32_hits();
                let delta: Vec<usize> = (0..8).map(|i| after[i] - before[i]).collect();
                // The ENCODE's frame count, and it is PER ARM: the arm tuple's
                // `frame_count` field is what the y4m is built from
                // (`duration = frame_count / 25.0`, 38085, rendered with
                // `-t {duration}` at 38131) and this gate's aomenc carries no
                // `--limit` and no `--lag-in-frames`, so the stream codes exactly
                // `frame_count` frames (1 for the intra arms, 5 for the
                // inter/straddling ones). Asserted ABOVE the oracle call, so a
                // decode that dropped one reds naming the encode.
                assert_eq!(
                    decoded.len(),
                    frame_count,
                    "{name}: {tag}: the encode codes {frame_count} frame(s), the decode showed {}",
                    decoded.len()
                );
                let reference = if ten_bit {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, decoded.len())
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, decoded.len())
                };
                assert_eq!(
                    decoded.len(),
                    reference.len(),
                    "{name}: {tag}: shown-frame count vs ffmpeg"
                );
                for (i, (ours, theirs)) in decoded.iter().zip(reference.iter()).enumerate() {
                    for (plane, a, b, w) in [
                        ("Y", &ours.y, &theirs.y, width),
                        ("U", &ours.u, &theirs.u, width / 2),
                        ("V", &ours.v, &theirs.v, width / 2),
                    ] {
                        if let Some(d) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
                            let n = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
                            if std::env::var_os("EC_EDGE32_ROWS").is_some() {
                                let mut per_row: std::collections::BTreeMap<usize, usize> =
                                    Default::default();
                                for (k, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                                    if x != y {
                                        *per_row.entry(k / w).or_default() += 1;
                                    }
                                }
                                eprintln!("{name}: {tag} plane {plane} diffs per row: {per_row:?}");
                            }
                            panic!(
                                "{name}: {tag} frame {i} plane {plane}: {n} pixels differ, first \
                                 at row {} col {} (ours {} vs ffmpeg {}) [edge32={delta:?}]",
                                d / w,
                                d % w,
                                a[d],
                                b[d]
                            );
                        }
                        assert_eq!(
                            a.len(),
                            b.len(),
                            "{name}: {tag} frame {i} plane {plane}: ours has {} samples, \
                             ffmpeg's has {} -- a short plane would zip-truncate the walk above \
                             and hide a chroma-span defect",
                            a.len(),
                            b.len()
                        );
                    }
                }
                compared += 1;
                if tile_cols > 0 {
                    compared_multi_tile += 1;
                }
                straddle_units += crate::decode::cdef_straddle_units() - straddle_before;
                for i in 0..8 {
                    totals[i] += delta[i];
                }
            }
        }
        assert!(
            compared > 0,
            "{name}: every attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
        assert!(
            !straddle || compared_multi_tile > 0,
            "{name}: every `--tile-columns=1` attempt hit a named refusal -- the \
             multi-tile twins compared nothing:\n{}",
            refusals.join("\n")
        );
        assert!(
            !straddle || straddle_units > 0,
            "{name}: no FILTERED 8x8 CDEF unit crossed the cropped frame edge over \
             {compared} compared attempts -- the straddling arms proved nothing"
        );
        assert!(
            totals[0] + totals[1] + totals[4] + totals[5] > 0,
            "{name}: no frame-edge partition bit was read at all over \
             {compared} compared attempts -- the gate proved nothing"
        );
        // lane-golomb r3: the flat-band arm's ASSERT moved one level up. Until
        // r3 the last superblock row was reached through a 32-level edge bit
        // only because our own out-of-frame transform units had desynced the
        // stream; with that fixed, real aomenc answers a bottom-edge
        // superblock with the 64-level `PARTITION_HORZ` (and a right-edge one
        // with `PARTITION_VERT`) and the 32-level slots stay 0 on this
        // content. Slots 6/7 are the strips actually decoded at the edge.
        assert!(
            !flat_band || totals[6] > 0,
            "{name}: no 64x32 bottom-edge HORZ strip was decoded \
             (edge32={totals:?}, {compared} compared attempts)"
        );
        assert!(
            !flat_band || totals[7] > 0,
            "{name}: no 32x64 right-edge VERT strip was decoded \
             (edge32={totals:?}, {compared} compared attempts)"
        );
        eprintln!(
            "{name}: {compared} pixel-exact attempts, 32-level edge bits \
             [horz_or_vert={} split={}] bottom-HORZ={} right-VERT={}, 64-level edge bits \
             [horz_or_vert={} split={}] bottom-HORZ={} right-VERT={}, \
             {straddle_units} straddling CDEF units, {} named refusals",
            totals[0],
            totals[1],
            totals[2],
            totals[3],
            totals[4],
            totals[5],
            totals[6],
            totals[7],
            refusals.len()
        );
        for r in &refusals {
            eprintln!("{name} refusal: {r}");
        }
    }

    /// lane-tiles r11: every tile gate before this one turns the coding tools
    /// OFF (rect/ab partitions, tx-size search, cdef, restoration all `=0`)
    /// to isolate the per-tile loop, which is exactly the
    /// [[tool-disabled-in-every-gate]] shape -- a tile-edge defect in any of
    /// those tools is invisible to all of them. This one runs the BROAD tool
    /// set (rect + ab partitions, default tx search, cdef and loop
    /// restoration both on, deblocking left at its default so it filters
    /// across tile boundaries as the spec requires) over a real 2D tile grid,
    /// through `decode_stream` rather than the tile entry point, so it is the
    /// same path a caller decoding a file takes.
    ///
    /// 256x128 is four 64x64 superblocks wide and two tall, so
    /// `--tile-columns=1 --tile-rows=1` really yields a 2x2 grid and
    /// `--tile-columns=2` really yields 4 columns; both are asserted from the
    /// parsed `tile_info` rather than assumed, and the decoded-tile counter
    /// (`tile_hits`) is asserted at >= 4 per decoded frame so a stream whose
    /// header claims 4 tiles but whose decode only walked one cannot pass.
    // `#[track_caller]` so a failing assert names the calling arm's line, and
    // so `gate_coverage`'s body split (on `\n    #[`) treats this helper's
    // recipe as its own gate rather than gluing it onto the preceding one.
    #[track_caller]
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_multi_tile_gate(
        name: &str,
        tile_cols_log2: u32,
        tile_rows_log2: u32,
        bit_depth: usize,
        frame_count: usize,
        extra: &[&str],
        // lane-tiles r12: the palette arm's own override proof. `true` asserts,
        // per attempt, that the arm's `--tune-content=screen --enable-palette=1`
        // really reached the encoder (frame header `allow_screen_content_tools`)
        // AND that a palette block fired at a TILE's left column -- the exact
        // block whose colour cache must not see the previous tile's colours.
        require_tile_left_palette: bool,
        // `None` = the shared `gradients_source`. The palette arm overrides it
        // with `smptebars`, the flat few-colour source the existing palette
        // gates use -- gradients never make aomenc pick a palette block.
        source_override: Option<&str>,
    ) {
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 128usize);
        let expected_tiles = (1usize << tile_cols_log2) * (1usize << tile_rows_log2);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut uv_edge_suppressed = 0usize;
        let n_attempts: u32 = std::env::var("EC_TILES_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source = match source_override {
                Some(src) => format!("{src}=size={width}x{height}:duration={duration}:rate=25"),
                None => {
                    gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
                }
            };
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
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let cols = format!("--tile-columns={tile_cols_log2}");
            let rows = format!("--tile-rows={tile_rows_log2}");
            let limit = format!("--limit={frame_count}");
            let depth_args: Vec<String> = if bit_depth == 10 {
                vec![
                    "--input-bit-depth=10".to_owned(),
                    "--bit-depth=10".to_owned(),
                ]
            } else {
                Vec::new()
            };
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=40",
                "--cpu-used=4",
                &limit,
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                // This decoder is 64x64-superblock only, and `--tile-columns`
                // splits on superblock boundaries, so 128x128 superblocks
                // would leave 256px only two tiles' worth of columns.
                "--sb-size=64",
                &cols,
                &rows,
                // The point of this gate: tools ON across tile edges.
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--enable-cdef=1",
                "--enable-restoration=1",
                // Still off: capabilities this decoder refuses outright, which
                // would turn every attempt into a refusal and make the gate
                // vacuous rather than proving anything about tiles.
                "--enable-1to4-partitions=0",
                "--min-partition-size=16",
                // Overridable by `extra` (aomenc takes the last occurrence of
                // a repeated flag), which the palette arm below relies on.
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-ref-frame-mvs=0",
                "--enable-global-motion=0",
            ];
            args.extend(extra.iter().copied());
            args.extend(depth_args.iter().map(String::as_str));
            args.extend(["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            // The stream must really carry the tile grid asked for, or the
            // gate proves nothing (aomenc silently clamps tile counts that do
            // not fit the frame's superblock grid).
            {
                let mut probe = Av1Parser::new();
                let mut pos = 0usize;
                let mut checked = false;
                while pos < stream.len() {
                    let obu = probe
                        .parse_obu(&stream[pos..])
                        .expect("parsing the gate stream");
                    pos += obu.total_size;
                    let header = match &obu.kind {
                        ObuKind::Frame(h, _) => h,
                        ObuKind::FrameHeader(h) => h,
                        // lane-tiles r12: prove the base recipe's tools-on
                        // flags arrived (a repeated aomenc `--enable-*` keeps
                        // the LAST occurrence -- measured, see the report --
                        // so a per-arm override must not silently vanish).
                        ObuKind::SequenceHeader(seq) => {
                            assert!(
                                seq.enable_cdef && seq.enable_restoration,
                                "{name}: --enable-cdef=1 --enable-restoration=1 did not reach \
                                 aomenc (seq enable_cdef={} enable_restoration={}, seed {seed})",
                                seq.enable_cdef,
                                seq.enable_restoration
                            );
                            continue;
                        }
                        _ => continue,
                    };
                    // A `show_existing_frame` header codes no tile info at all
                    // (spec 5.9.2 returns before `tile_info()`), so its
                    // `TileInfo` is the parser's 1x1 default -- checking it
                    // here would fail every inter stream for a header that
                    // carries no tiling.
                    if header.show_existing_frame {
                        continue;
                    }
                    if require_tile_left_palette {
                        assert!(
                            header.allow_screen_content_tools,
                            "{name}: --tune-content=screen/--enable-palette=1 did not reach \
                             aomenc -- allow_screen_content_tools is 0 (seed {seed})"
                        );
                    }
                    assert_eq!(
                        (header.tile_info.cols * header.tile_info.rows) as usize,
                        expected_tiles,
                        "{name}: aomenc wrote {}x{} tiles, not the {expected_tiles} asked for \
                         (seed {seed})",
                        header.tile_info.cols,
                        header.tile_info.rows
                    );
                    checked = true;
                }
                assert!(
                    checked,
                    "{name}: no frame header in the stream (seed {seed})"
                );
            }

            let before = crate::decode::tile_hits();
            let pal_before = crate::decode::palette_tile_left_hits();
            let uv_edge_before = crate::decode::uv_tile_edge_suppressed_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits >= expected_tiles * frames.len(),
                "{name}: tile_hits delta was {hits}, expected >= {} -- every tile of every \
                 frame must actually decode (seed {seed})",
                expected_tiles * frames.len()
            );
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{name} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{name} frame {i} V vs ffmpeg (seed {seed})");
            }
            if require_tile_left_palette {
                let pal = crate::decode::palette_tile_left_hits() - pal_before;
                assert!(
                    pal >= 1,
                    "{name}: this attempt decoded and matched but fired {pal} palette blocks at \
                     a tile's left column -- the tile-relative palette cache guard is untested \
                     (seed {seed})"
                );
            }
            // lane-mtfix r1: the chroma edge-filter neighbour of a block on a
            // tile's first mi row/column lives in the tile above/left, where
            // libaom has no neighbour at all. This counts the reads suppressed
            // there whose frame-wide coarse slot held a non-DC mode -- the
            // reads that would otherwise pick the wrong chroma edge filter
            // strength (they did, before this fix: seed 46 of this gate).
            uv_edge_suppressed += crate::decode::uv_tile_edge_suppressed_hits() - uv_edge_before;
            matched += 1;
        }
        assert!(
            matched > 0,
            "{name}: every attempt refused ({named_refusals}); the gate never decoded a \
             multi-tile stream with the coding tools on"
        );
        eprintln!(
            "{name}: {matched} pixel-exact matches, {named_refusals} named refusals, \
             {uv_edge_suppressed} chroma edge reads suppressed at a tile edge"
        );
    }

    /// The one recipe both 1:4-in-inter arms run, rewritten by
    /// lane-av1recipehunt. What changed, and why the old sweep could not fire:
    ///
    /// * `--min-partition-size=32` -> **`8`**. This is the flag that was
    ///   holding the shape off the menu entirely. MEASURED over 96 encodes
    ///   (10-bit, 192x128 and 256x192, `--cpu-used` 0..5, cq 63/55, this
    ///   crate's full intra mode set): at `mps=16` the 32-level 1:4 counter is
    ///   **0 in all 48 attempts**, at `mps=8` it fires in 40 of 48. The old
    ///   comment's "min size 16 so the 16x16-level 1:4 is out of reach" was
    ///   protecting against a refusal that no longer fires -- `mps=8` reaches
    ///   the 8- and 16-level strips too, and on this content the RD takes
    ///   neither (the `inter16_1to4` and `rect4_32` inter counters confirm the
    ///   only 1:4 partitions in the stream are the 32-level ones this gate
    ///   asserts).
    /// * the intra mode set: `--enable-smooth-intra/paeth-intra/
    ///   directional-intra/angle-delta` `0` -> **`1`**. A 8x32 or 32x8 strip
    ///   has four usable intra modes with these on; with them off the only
    ///   candidate is DC, and DC never wins a 1:4 strip against inter.
    /// * the 40-attempt content/preset/cq seed sweep -> one pinned cell at
    ///   256x192, 6 frames, `--cpu-used=2 --cq-level=63`. A sweep hides a
    ///   recipe that works; a pin makes it a precondition.
    ///
    /// The 8-bit arm's `hue=s=0` and `-t 0.32` are dropped: they only fixed
    /// ffmpeg's own frame count across the sweep, and the frame count is now
    /// pinned by `-frames:v 6` (6 decode-order frames, `kf-min-dist=1000`, so
    /// frame 0 is the only key frame and every counted strip is in an INTER
    /// frame).
    ///
    /// **The recipe**, exactly (the recorded command lines):
    /// ```text
    /// ffmpeg -v error -y -f lavfi \
    ///   -i mandelbrot=size=256x192:start_scale=5.0:end_scale=0.004:end_pts=8:rate=25 \
    ///   -pix_fmt yuv420p -frames:v 6 -f yuv4mpegpipe -        # 10-bit: yuv420p10le + -strict -1
    /// aomenc [--cpu-used=2 --input-bit-depth=10 -b 10] --codec=av1 --passes=1 --end-usage=q \
    ///   --cpu-used=2 --cq-level=63 --threads=1 --row-mt=0 \
    ///   --enable-rect-partitions=1 --enable-1to4-partitions=1 \
    ///   --min-partition-size=8 --max-partition-size=64 --enable-tx-size-search=0 \
    ///   --enable-filter-intra=1 --enable-intra-edge-filter=1 --enable-smooth-intra=1 \
    ///   --enable-paeth-intra=1 --enable-directional-intra=1 --enable-angle-delta=1 \
    ///   --lag-in-frames=0 --auto-alt-ref=0 --kf-min-dist=1000 --kf-max-dist=1000 \
    ///   --enable-order-hint=0 --enable-warped-motion=0 --enable-obmc=0 \
    ///   --enable-masked-comp=0 --enable-interintra-comp=0 --enable-dist-wtd-comp=0 \
    ///   --enable-diff-wtd-comp=0 --enable-onesided-comp=0 --enable-interintra-wedge=0 \
    ///   --enable-smooth-interintra=0 --enable-ab-partitions=0 --enable-cdef=0 \
    ///   --enable-restoration=0 --enable-palette=0 --enable-intrabc=0 \
    ///   --enable-cfl-intra=0 --enable-ref-frame-mvs=0 --obu -o - -
    /// ```
    /// Sources: 8-bit 442462 bytes, sha256
    /// `e4b46799b180a0d3e138caa18b2738c035e349219490313db664814a3eae9ec2`;
    /// 10-bit 884828 bytes, sha256
    /// `b7e0602e0bfc7a7f6813f40bb3989ec7574e5fbbfbf190298b9ab5b625156030`.
    /// Outputs, committed with `git add -f` past the `fixtures` gitignore:
    /// * `fixtures/intra14_256x192_8bit.obu`, 35179 bytes, sha256
    ///   `5e50eb6d4492e806a2803988d92bd47f069ca54e02c06344d5500c6b72deb861`
    ///   (fnv1a64 `0x86f8d1e5a221162a`) -- MEASURED `32x8=8 8x32=11`,
    ///   `64x16=0 16x64=0`, all 6 frames pixel-exact vs ffmpeg.
    /// * `fixtures/intra14_256x192_10bit.obu`, 34727 bytes, sha256
    ///   `ab6fae0a256af6410e8200f10827a3e965dee7b2d7ab63710e94968bddab23a2`
    ///   (fnv1a64 `0xecf9455bc20d60f0`) -- MEASURED `32x8=12 8x32=7`,
    ///   all 6 frames pixel-exact vs ffmpeg.
    ///
    /// The gate re-encodes live and byte-compares against the pin, so the pin
    /// is the expected output and not a second decode path. The control arm
    /// re-runs the SAME command line with `--enable-1to4-partitions=0` and
    /// requires the counter to stay at 0 -- that single flag is what carries
    /// the coverage, and asserting it is also this gate's own red-proof.
    pub(super) fn intra_rect4_in_inter_gate(bit_depth: u32) {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (256usize, 192usize, 6usize);
        let (src_len, src_sha, pin_name, pin_len, pin_fnv) = if bit_depth == 10 {
            (
                884_828usize,
                "b7e0602e0bfc7a7f6813f40bb3989ec7574e5fbbfbf190298b9ab5b625156030",
                "fixtures/intra14_256x192_10bit.obu",
                34_727usize,
                0xecf9_455b_c20d_60f0u64,
            )
        } else {
            (
                442_462usize,
                "e4b46799b180a0d3e138caa18b2738c035e349219490313db664814a3eae9ec2",
                "fixtures/intra14_256x192_8bit.obu",
                35_179usize,
                0x86f8_d1e5_a221_162au64,
            )
        };
        let mut source_args: Vec<&str> = vec![
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=size=256x192:start_scale=5.0:end_scale=0.004:end_pts=8:rate=25",
            "-pix_fmt",
        ];
        source_args.push(if bit_depth == 10 {
            "yuv420p10le"
        } else {
            "yuv420p"
        });
        source_args.extend(["-frames:v", "6"]);
        if bit_depth == 10 {
            // yuv420p10le is not an official yuv4mpegpipe format on this ffmpeg
            source_args.extend(["-strict", "-1"]);
        }
        source_args.extend(["-f", "yuv4mpegpipe", "-"]);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&source_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        assert_eq!(
            y4m.stdout.len(),
            src_len,
            "{NAME} ({bit_depth}-bit): the mandelbrot source is not the pinned 6-frame y4m -- \
             `rate=`, `end_pts` and `-frames:v` are all part of this recipe's identity"
        );
        eprintln!("{NAME} ({bit_depth}-bit): source y4m sha256 {src_sha}");
        let mut args: Vec<&str> = Vec::new();
        if bit_depth == 10 {
            args.extend(["-b", "10", "--input-bit-depth=10"]);
        }
        // Every flag spelled exactly once (class [[aomenc-last-flag-wins]]).
        args.extend([
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cpu-used=2",
            "--cq-level=63",
            "--threads=1",
            "--row-mt=0",
            "--enable-rect-partitions=1",
            // The feature this gate is about, and the flag the control arm
            // below reverts.
            "--enable-1to4-partitions=1",
            // 8, not 16 or 32: MEASURED, the 32-level 1:4 counter is 0 in all
            // 48 attempts at 16 and fires in 40 of 48 at 8.
            "--min-partition-size=8",
            "--max-partition-size=64",
            "--enable-tx-size-search=0",
            // A 1:4 strip needs an intra mode worth coding: with these four
            // off the only candidate is DC and it never wins.
            "--enable-filter-intra=1",
            "--enable-intra-edge-filter=1",
            "--enable-smooth-intra=1",
            "--enable-paeth-intra=1",
            "--enable-directional-intra=1",
            "--enable-angle-delta=1",
            // Single reference and one key frame, so every counted strip is
            // inside an INTER frame.
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--kf-min-dist=1000",
            "--kf-max-dist=1000",
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
            "--enable-ab-partitions=0",
            "--enable-cdef=0",
            "--enable-restoration=0",
            // Ledger dead-end (lane-r14): a NEW gate spelling
            // `--enable-cfl-intra=1` retires that tool from
            // `NEVER_EXERCISED_8BIT` on the flag alone while no CfL
            // hit counter exists to assert -- so it stays 0 here.
            // Palette stays 0 for the same class of reason: this arm
            // refuses a screen-content frame whole, so a palette flag
            // would buy no coverage.
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--enable-cfl-intra=0",
            "--enable-ref-frame-mvs=0",
            "--obu",
            "-o",
            "-",
            "-",
        ]);
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        assert_eq!(
            stream.len(),
            pin_len,
            "{NAME} ({bit_depth}-bit): encoded size moved"
        );
        assert_eq!(
            fnv1a64(&stream),
            pin_fnv,
            "{NAME} ({bit_depth}-bit): encoded bytes moved"
        );
        let pin = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(pin_name);
        assert_eq!(
            std::fs::read(&pin).unwrap_or_else(|e| panic!("{pin_name} must be committed: {e}")),
            stream,
            "{NAME} ({bit_depth}-bit): the live encode no longer matches the committed pin"
        );

        let before: Vec<usize> = (0..4)
            .map(decode::intra_rect4_strip_in_inter_hits)
            .collect();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME} ({bit_depth}-bit): the pin must decode: {e}"));
        let delta: Vec<usize> = (0..4)
            .map(|k| decode::intra_rect4_strip_in_inter_hits(k) - before[k])
            .collect();
        let ffmpeg_frames = if bit_depth == 10 {
            ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
        } else {
            ffmpeg_decode_sequence(&stream, width, height, frame_count)
        };
        assert_eq!(frames.len(), frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            for (plane, (g, w)) in [
                ("Y", (&got.y, &want.y)),
                ("U", (&got.u, &want.u)),
                ("V", (&got.v, &want.v)),
            ] {
                let stride = if plane == "Y" { width } else { width / 2 };
                if let Some(at) = g.iter().zip(w.iter()).position(|(a, b)| a != b) {
                    eprintln!(
                        "{NAME} ({bit_depth}-bit): frame {i} {plane} first diff at ({}, {})",
                        at % stride,
                        at / stride
                    );
                }
                assert_eq!(g, w, "{plane} vs ffmpeg (frame {i})");
            }
        }
        assert!(
            delta[2] + delta[3] > 0,
            "{NAME} ({bit_depth}-bit): {frame_count} frames pixel-exact but no 32-level intra 1:4 \
             strip (32x8/8x32) fired (64x16={} 16x64={} 32x8={} 8x32={}) -- the recipe stopped \
             producing the shape this gate names (class gate-blind-to-feature)",
            delta[0],
            delta[1],
            delta[2],
            delta[3]
        );

        // The control arm: the one flag that is this gate's feature, reverted.
        // If it ever fires too, 1:4 is no longer what puts these strips on the
        // RD menu and the feature arm above is not the witness it claims to be.
        // It is also this gate's own red-proof.
        let mut control = args.clone();
        let at = control
            .iter()
            .position(|a| *a == "--enable-1to4-partitions=1")
            .expect("the feature flag is in the recipe");
        control[at] = "--enable-1to4-partitions=0";
        let ctl = run_with_stdin(Command::new(aomenc_path()).args(&control), &y4m.stdout);
        assert!(
            ctl.status.success(),
            "aomenc refused the --enable-1to4-partitions=0 control: {}",
            String::from_utf8_lossy(&ctl.stderr)
        );
        let ctl_before: Vec<usize> = (0..4)
            .map(decode::intra_rect4_strip_in_inter_hits)
            .collect();
        decode_stream(&ctl.stdout)
            .unwrap_or_else(|e| panic!("{NAME}: the control stream must decode too: {e}"));
        let ctl_delta: Vec<usize> = (0..4)
            .map(|k| decode::intra_rect4_strip_in_inter_hits(k) - ctl_before[k])
            .collect();
        assert_eq!(
            ctl_delta, [0; 4],
            "{NAME} ({bit_depth}-bit): the --enable-1to4-partitions=0 control fired \
             64x16={} 16x64={} 32x8={} 8x32={} -- 1:4 partitions are no longer what produce these \
             strips, so the feature arm above is not witnessing what it names",
            ctl_delta[0], ctl_delta[1], ctl_delta[2], ctl_delta[3]
        );
        gate_buckets(NAME, 1, 0, 0);
        eprintln!(
            "{NAME} ({bit_depth}-bit): {frame_count} frames pixel-exact, intra 1:4 strips in inter \
             frames 64x16={} 16x64={} 32x8={} 8x32={} (--enable-1to4-partitions=0 control: \
             0/0/0/0), stream {pin_len} B fnv1a64={pin_fnv:016x}",
            delta[0], delta[1], delta[2], delta[3]
        );
    }

    pub(super) fn intra_rect_in_inter_split_tx_gate(bit_depth: u32) {
        let name = format!("intra_rect_in_inter_split_tx_gate({bit_depth})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // lane-intrasplit r3, MEASURED (probe3, 20 attempts x 2 depths): at
        // 192x128 with `--min/--max-partition-size=32` a correctly decoded
        // stream contains ZERO intra HORZ/VERT strips in an inter frame -- the
        // r1/r2 "firing" counters were read off our own desynced decode (class
        // counter-from-refused-stream). The merged unsplit-strip gate's own
        // geometry (96x96, min 16 / max 64) is the one that makes aomenc's RD
        // pick intra strips here, and at `--enable-tx-size-search=1` it splits
        // their transforms: 10-bit attempt 0 = 8 strips, 3 split; 8-bit
        // attempt 0 = 5 strips, 1 split.
        let (width, height, frame_count) = (96usize, 96usize, 8usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        let mut counted_exact = 0u32;
        let mut seen = [0usize; 3];
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // MEASURED (r1, 40 attempts): the same sweep the merged
            // `intra_rect_in_inter_gate` had to make -- at `--cpu-used=0`
            // aomenc emits AB partitions regardless of
            // `--enable-ab-partitions=0` (ledger), so the preset is swept
            // 1..4, and the quantiser with it (a hard-cut overlay source at
            // cpu-used=0 stopped 38/40 attempts on other lanes' refusals).
            let cpu = attempt % 5;
            let cq = [30u32, 20, 12, 4][((attempt / 5) % 4) as usize];
            // A FAST ZOOM: consecutive frames share almost no content, so
            // aomenc's RD codes blocks INTRA inside INTER frames
            // (`--kf-min-dist` forbids a new key frame) -- the only way a
            // real encoder reaches this decoder's intra-in-inter arm -- and
            // the fractal's fine detail is what then splits their transforms.
            let source = format!(
                "mandelbrot=size={width}x{height}:start_scale={}:end_scale=0.004:end_pts=8:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // Desaturated chroma keeps `uv_mode` on DC_PRED, so the
                    // still-refused directional-chroma gaps never fire and
                    // luma detail alone drives the transform split.
                    "-vf",
                    "hue=s=0",
                    "-pix_fmt",
                    if bit_depth == 10 {
                        "yuv420p10le"
                    } else {
                        "yuv420p"
                    },
                    "-strict",
                    "-1",
                    "-t",
                    "0.32",
                    "-f",
                    "yuv4mpegpipe",
                    "-strict",
                    "-1",
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
            let depth_args: &[&str] = if bit_depth == 10 {
                &["-b", "10", "--input-bit-depth=10"]
            } else {
                &[]
            };
            let cq_arg = format!("--cq-level={cq}");
            let cpu_arg = format!("--cpu-used={cpu}");
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(depth_args).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    &cpu_arg,
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=9999",
                    "--kf-min-dist=9999",
                    "--threads=1",
                    "--row-mt=0",
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
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    "--min-partition-size=16",
                    "--max-partition-size=64",
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag): this gate
                    // needs the rect partitions the strip lives in and the
                    // tx_depth symbol, which exists only under TX_MODE_SELECT.
                    "--enable-rect-partitions=1",
                    "--enable-tx-size-search=1",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::intra_rect_in_inter_split_tx_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed} cq {cq}): {msg}"
                    );
                    refusals.push(format!("seed {seed} cq {cq} cpu {cpu}: {msg}"));
                    continue;
                }
            };
            let after = decode::intra_rect_in_inter_split_tx_hits();
            let mut per_depth = [0usize; 3];
            for i in 0..3 {
                per_depth[i] = after[i] - before[i];
            }
            // class [[gate-skips-on-its-own-failure]]: a decode that
            // succeeded is ALWAYS pixel-compared below; the counter only
            // decides whether the attempt counts as firing.
            let counted = per_depth[1] + per_depth[2] != 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed} cq {cq} cpu {cpu}: decoded and pixel-compared, but no intra rect strip \
                     in an inter frame split its transform"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "{name} seed={seed} cq={cq} cpu={cpu}: split intra strips depth1={} depth2={}",
                per_depth[1], per_depth[2]
            );
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                for (plane, (g, w)) in [
                    ("Y", (&got.y, &want.y)),
                    ("U", (&got.u, &want.u)),
                    ("V", (&got.v, &want.v)),
                ] {
                    let stride = if plane == "Y" { width } else { width / 2 };
                    if let Some(at) = g.iter().zip(w.iter()).position(|(a, b)| a != b) {
                        eprintln!(
                            "{name} seed={seed}: frame {i} {plane} first diff at ({}, {}) got {} \
                             want {} ({} samples differ)",
                            at % stride,
                            at / stride,
                            g[at],
                            w[at],
                            g.iter().zip(w.iter()).filter(|(a, b)| a != b).count(),
                        );
                    }
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "frame {i} luma vs ffmpeg (seed {seed} cq {cq})"
                );
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed} cq {cq})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed} cq {cq})");
            }
            if counted {
                counted_exact += 1;
                seen[1] += per_depth[1];
                seen[2] += per_depth[2];
                // Keep sweeping until BOTH reachable depths have been seen in
                // a pixel-exact attempt; one firing attempt is the floor.
                if seen[1] != 0 && seen[2] != 0 {
                    break;
                }
            }
        }
        gate_buckets(
            &name,
            counted_exact,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        eprintln!(
            "{name}: split strips seen depth1={} depth2={}",
            seen[1], seen[2]
        );
        assert!(
            seen[1] + seen[2] != 0,
            "{name} never observed a split-transform intra strip in an inter frame:\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn screen_rect_intra_palette_syntax_gate(bit_depth: u32) {
        let name = format!("screen_rect_intra_palette_syntax_gate({bit_depth})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (256usize, 192usize, 10usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=256x192:r=25",
                "-t",
                "0.4",
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
            "{name}: ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        // The source stays 8-bit for both arms: `--bit-depth=10
        // --input-bit-depth=8` is the recipe that was measured to reach the
        // refusal, and the arm's point is the coded syntax, not the input.
        let depth_arg = format!("--bit-depth={bit_depth}");
        let cq_arg = format!("--cq-level={}", if bit_depth == 10 { 45 } else { 40 });
        let out = run_with_stdin(
            Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--kf-max-dist=30",
                    "--limit=10",
                    "--threads=1",
                    "--tile-columns=0",
                    "--enable-rect-partitions=1",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=8",
                    "--sb-size=64",
                    "--input-bit-depth=8",
                    // `allow_screen_content_tools` in every frame header comes
                    // from here; the two searches below stay off so the KEY frame
                    // does not stop on a palette/intrabc RECONSTRUCTION refusal
                    // before an inter frame exists.
                    "--tune-content=screen",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                ])
                .args([&depth_arg, &cq_arg])
                .args(["--obu", "-o", "-", "-"]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "{name}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        decode::reset_intra_rect_in_inter_screen_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{name}: decode failed -- the case this gate exists for: {e}")
        });
        let strips = decode::intra_rect_in_inter_screen_hits();
        eprintln!(
            "{name}: {} frames, {strips} rect intra strip(s) read the palette syntax",
            frames.len()
        );
        assert_eq!(frames.len(), frame_count, "{name}: frame count");
        let ffmpeg_frames = if bit_depth == 10 {
            ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
        } else {
            ffmpeg_decode_sequence(&stream, width, height, frame_count)
        };
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{name}: frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{name}: frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{name}: frame {i} V vs ffmpeg");
        }
        assert!(
            strips > 0,
            "{name}: no rect intra strip of a screen-content frame read the palette syntax -- \
             the gate is vacuous, the recipe stopped producing the shape"
        );
    }
