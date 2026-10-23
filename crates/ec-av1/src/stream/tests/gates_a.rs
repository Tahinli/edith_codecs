use super::*;
use super::support::*;

    /// Three-bucket gate summary (class [[gate-skips-on-its-own-failure]]):
    /// every attempt lands in exactly one bucket -- decoded pixel-exact WITH
    /// the feature counter moving, decoded pixel-exact WITHOUT it, or a named
    /// refusal -- and the three sum to the attempts made.
    pub(super) fn gate_buckets(name: &str, counted_exact: u32, uncounted_exact: u32, named_refusals: usize) {
        eprintln!(
            "{name}: buckets counted-exact={counted_exact} uncounted-exact={uncounted_exact} \
             named-refusals={named_refusals} (attempts {})",
            counted_exact as usize + uncounted_exact as usize + named_refusals
        );
    }

    pub(crate) fn lock_gate_counters() -> std::sync::MutexGuard<'static, ()> {
        GATE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The body every 4:4:4 lossy live gate runs: assert the oracle compare
    /// CANNOT be skipped, assert the decode really is the cell it claims
    /// (frame count, geometry, full-resolution chroma), assert the 4:4:4-only
    /// own-extent route really ran, then compare every frame in decode order
    /// against the oracle aomdec with the length asserted first.
    ///
    /// The aomdec arm is a HARD failure, not a skip: these three gates encode
    /// LIVE, so reaching this point means the oracle aomenc resolved, and
    /// aomenc and aomdec ship in the same build directory -- an aomdec that is
    /// missing beside a present aomenc is a broken oracle, not a checkout
    /// without one. (The older shape, `if aomdec_path().is_file()`, let the
    /// compare vanish with no env escape at all; that is what
    /// [`aomdec_available`] replaced for the pinned gates, and it cannot be
    /// used here because these gates have no stream to compare without one.)
    pub(super) fn gate_444_lossy_live_exact(name: &str, w: usize, h: usize) {
        // The ONLY way out without measuring, and it has already printed a
        // SKIP naming the env var that would turn it into a failure.
        let Some(stream) = encode_444_lossy_live(name, w, h) else {
            return;
        };
        assert!(
            aomdec_path().is_file(),
            "{name}: the oracle aomenc resolved to {} but no aomdec sits beside it at {} -- \
             the pixel compare would silently skip, which this gate treats as a failure",
            aomenc_path().display(),
            aomdec_path().display()
        );

        let _guard = lock_gate_counters();
        crate::decode::reset_rect4_inter_own_chroma444_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{name}: the live {w}x{h} 4:4:4 lossy stream was refused: {e}")
        });
        let own = crate::decode::rect4_inter_own_chroma444_hits();
        assert!(
            own > 0,
            "{name}: no 4:4:4 1:4 inter strip read its chroma context over its own extent -- \
             the route this cell is a witness for never ran (measured: 13 at 66x66, 12 at \
             130x122, 24 at 256x128)"
        );
        assert_eq!(
            frames.len(),
            4,
            "{name}: decoded {} frame(s), the recipe encodes 4 -- a short decode would make the \
             oracle compare cover a prefix of the cell",
            frames.len()
        );
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                (f.width, f.height),
                (w, h),
                "{name}: frame {i} is {}x{}, the cell is {w}x{h}",
                f.width,
                f.height
            );
            // At 4:4:4 both chroma planes are full resolution. A decoder that
            // kept a subsampled extent would hand quarter-size planes to the
            // byte compare; say so here instead of letting the compare
            // stand in for the shape claim.
            assert_eq!(
                f.u.len(),
                w * h,
                "{name}: frame {i} U plane is {} samples; at 4:4:4 it must be full resolution \
                 ({w}x{h})",
                f.u.len()
            );
            assert_eq!(
                f.v.len(),
                w * h,
                "{name}: frame {i} V plane is {} samples; at 4:4:4 it must be full resolution \
                 ({w}x{h})",
                f.v.len()
            );
            assert_eq!(
                f.y.len(),
                w * h,
                "{name}: frame {i} luma plane is {} samples, expected {w}x{h}",
                f.y.len()
            );
        }
        let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, name);
        assert_eq!(
            decoded, 4,
            "{name}: the oracle decoded {decoded} frame(s) in decode order, the recipe carries 4 \
             (so far {hidden} hidden)"
        );
        eprintln!(
            "{name}: {w}x{h} 4:4:4 lossy, {decoded} decode-order frame(s) byte-exact vs aomdec \
             ({hidden} hidden), {own} own-extent 1:4 chroma gather(s)"
        );
    }

    /// lane-av1toolgates: the shared body of the two `enable-rect-tx`
    /// coverage gates (8-bit and 10-bit).
    ///
    /// The aomenc ARGUMENT VECTOR is built by each gate and passed in, never
    /// assembled here. That is not tidiness: `gate_coverage`'s `gate_bodies()`
    /// splits `stream.rs` on `\n    #[` and keeps a segment only if that
    /// segment itself contains `--passes=1`, so a flag living in a helper is
    /// invisible to the coverage derivation. A helper that owns the recipe
    /// would leave this gate counted at whatever depth the helper's own text
    /// implies -- which is the blind spot that kept `enable-rect-tx` on the
    /// `NEVER_EXERCISED` lists after real streams had exercised it.
    ///
    /// WHAT THIS GATE IS AND IS NOT. `enable_rect_tx` has no sequence-header
    /// bit in AV1 -- aom reads it only in `tx_search.c` (`search_tx_size`'s
    /// `!enable_rect_tx && tx_size_wide != tx_size_high` skip and the
    /// `tx_size_max_32_square` remap) -- so the flag never reaches a decoder.
    /// Spelling `--enable-rect-tx=1` proves the ENCODER was ALLOWED the tool
    /// and nothing more. It is CONTEXT here, not the witness: a gate that
    /// stopped at the flag would be the `tool-disabled-in-every-gate` defect
    /// `NEVER_EXERCISED` exists to prevent, in a new costume.
    ///
    /// The witness is the PARSED PARTITION SHAPE, the only thing in the
    /// bitstream that can carry a rect transform:
    /// [`decode::rect_partition_hits`] counts `PARTITION_HORZ`/`PARTITION_VERT`
    /// symbols actually read, i.e. a rect partition in the tree this decoder
    /// walked, and [`decode::rect_coeff_tu_hits`] counts the rectangular
    /// transform units that then coded coefficients through `read_coeffs_rect`
    /// (`all_zero == 0`) -- the rect coefficient TABLES being read, not a
    /// header bit. Both are asserted above zero over the arms that decoded AND
    /// compared, and every frame is pixel-compared against ffmpeg's own decode
    /// of the identical bytes.
    ///
    /// LIMIT, stated because it bounds what this gate can claim: this
    /// repository has no INDEPENDENT partition parser to list leaves from --
    /// `ec-av1-syntax` parses OBU headers and the sequence header only -- and
    /// `ec_av1::census`'s per-frame `blocks`/`tx` shape maps, the one true
    /// leaf list, are armed by a process-global `OnceLock` on
    /// `EC_AV1_BITCENSUS`, so arming them inside a lib test would switch the
    /// accountant on for every other gate in the process. The partition walk is
    /// therefore observed through the decoder's own counters, which is weaker
    /// than a second opinion but is still a shape fact about the stream rather
    /// than a fact about the flag.
    ///
    /// RED-BEFORE: the COUNTERS, one per assert, both measured.
    /// `--enable-rect-tx=0` is deliberately NOT claimed as the mutation: it
    /// does NOT zero these counters (39 rect coefficient TUs against 210 on
    /// the cq20 arm, because `--enable-rect-partitions=1` already forces
    /// non-square transform sizes regardless), and with every partition shape
    /// off the flag yields no rect transform at either setting. `=0` does turn
    /// the gate red, but through the ORACLE -- its `--enable-tx-size-search=0`
    /// arm is a stream ffmpeg cannot decode past frame 6 of 15 -- so the
    /// frame-count assert fires first and proves nothing about the tool.
    pub(super) fn rect_tx_tool_gate(name: &str, ten_bit: bool, args: &[String]) -> (usize, usize) {
        // A missing tool SKIPs, so the totals the caller sums stay 0 and the
        // gate's own witness assert fails loudly instead of passing vacuously
        // on a run that decoded nothing.
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return (0, 0);
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return (0, 0);
        }
        let (width, height) = (64usize, 64usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "mandelbrot=size=64x64:rate=25",
                "-pix_fmt",
                if ten_bit { "yuv420p10le" } else { "yuv420p" },
                // y4m calls 10-bit an unofficial pixel format (no-op at 8).
                "-strict",
                "-1",
                "-t",
                "0.60",
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
        let out = run_with_stdin(Command::new(aomenc_path()).args(args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{name}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        assert!(!stream.is_empty(), "{name}: aomenc wrote an empty stream");
        if ten_bit {
            assert_eq!(
                parsed_bit_depth(&stream),
                10,
                "{name}: aomenc did not write a 10-bit sequence header"
            );
        }
        let parts_before = decode::rect_partition_hits();
        let tus_before = decode::rect_coeff_tu_hits();
        let decoded = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{name}: decode_stream refused: {e}"),
        };
        // Sampled after the decode, before the pixel compare, so a mismatch
        // report can still say whether the arm fired.
        let parts = decode::rect_partition_hits() - parts_before;
        let tus = decode::rect_coeff_tu_hits() - tus_before;
        // The ENCODE's frame count: the fixture renders `mandelbrot=size=64x64:rate=25`
        // with `-t "0.60"` (10677-10691) -- 25 x 0.60 = 15 frames -- and the
        // caller's `args` carry no `--limit` (10823-10853), so every arm's stream
        // codes all 15. Asserted ABOVE the oracle call, which is the fix for the
        // misattribution this gate's own doc comment records: the
        // `--enable-tx-size-search=0` arm is a stream ffmpeg cannot decode past
        // frame 6 of 15, and with the count taken from our decode the helper's
        // byte-length assert fires FIRST and reads as an ffmpeg fault. Naming the
        // 15 here makes the shortfall ours to explain.
        const ENCODED_FRAMES: usize = 15;
        assert_eq!(
            decoded.len(),
            ENCODED_FRAMES,
            "{name}: the encode codes {ENCODED_FRAMES} frames, the decode showed {} (rect \
             partitions {parts}, rect coefficient TUs {tus})",
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
            "{name}: shown-frame count vs ffmpeg (rect partitions {parts}, rect coefficient \
             TUs {tus})"
        );
        for (i, (ours, theirs)) in decoded.iter().zip(&reference).enumerate() {
            for (plane, (a, b), w) in [
                ("Y", (&ours.y, &theirs.y), width),
                ("U", (&ours.u, &theirs.u), width / 2),
                ("V", (&ours.v, &theirs.v), width / 2),
            ] {
                assert_eq!(a.len(), b.len(), "{name}: frame {i} {plane} plane length");
                if a != b {
                    let pin = std::env::temp_dir().join(format!("{name}-mismatch.obu"));
                    crate::dumpio::pin("TEST_PIN", &pin, &stream);
                    let at = a
                        .iter()
                        .zip(b.iter())
                        .position(|(x, y)| x != y)
                        .unwrap_or(0);
                    panic!(
                        "{name}: frame {i} {plane} mismatch vs ffmpeg at row {}, col {}: {} vs \
                         {} (rect partitions {parts}, rect coefficient TUs {tus}; stream pinned \
                         at {})",
                        at / w,
                        at % w,
                        a[at],
                        b[at],
                        pin.display()
                    );
                }
            }
        }
        eprintln!(
            "{name}: {} bit, pixel-exact vs ffmpeg; parsed rect partitions {parts}, rect \
             coefficient TUs {tus}",
            if ten_bit { "10" } else { "8" }
        );
        (parts, tus)
    }

    /// lane-hbdgates r1: drive one 10-bit aomenc recipe over `seeds`, decode
    /// every attempt and pixel-compare every SHOWN frame against ffmpeg's own
    /// 10-bit decode of the identical bytes. Per-tool firing counters are
    /// sampled PER ATTEMPT and summed only over attempts that decoded *and*
    /// compared -- a stream that hit a named refusal contributes nothing
    /// (class `counter-from-refused-stream`). Named refusals are tolerated
    /// and reported; a pixel mismatch is a hard failure, never a SKIP.
    pub(super) fn ten_bit_tool_gate(
        name: &str,
        width: usize,
        height: usize,
        frames: usize,
        extra: &[&str],
        seeds: &[u32],
        counters: &[(&str, fn() -> usize)],
    ) {
        fn first_diff(ours: &[u16], theirs: &[u16], width: usize) -> Option<String> {
            ours.iter()
                .zip(theirs.iter())
                .position(|(a, b)| a != b)
                .map(|i| {
                    format!(
                        "first diff at index {i} (row {}, col {}): ours {} vs ffmpeg {}",
                        i / width,
                        i % width,
                        ours[i],
                        theirs[i]
                    )
                })
        }
        let mut totals = vec![0usize; counters.len()];
        let mut refusals: Vec<String> = Vec::new();
        let mut compared = 0usize;
        for &seed in seeds {
            let stream = encode_10bit_gradients_seed(name, seed, width, height, frames, extra);
            assert_eq!(
                parsed_bit_depth(&stream),
                10,
                "{name}: aomenc did not write a 10-bit sequence header (seed {seed})"
            );
            let before: Vec<usize> = counters.iter().map(|(_, f)| f()).collect();
            let decoded = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            // Deltas are read BEFORE the pixel compare so a mismatch report
            // says whether the tool under test actually fired on the failing
            // stream (a mismatch with a zero delta is some other defect).
            let deltas: Vec<usize> = counters
                .iter()
                .zip(before.iter())
                .map(|((_, f), b)| f() - b)
                .collect();
            let fired: Vec<String> = counters
                .iter()
                .zip(deltas.iter())
                .map(|((label, _), d)| format!("{label}={d}"))
                .collect();
            let fired = fired.join(" ");
            // The ENCODE's frame count, and it is this helper's `frames` PARAMETER
            // (15446) -- not our decode's length. It is the same value handed to
            // `encode_10bit_gradients_seed` (15469), which renders
            // `frames / 25.0` seconds at `rate=25` (15928, 15940) and runs
            // aomenc with no `--limit` of its own, so the stream codes exactly
            // `frames` frames: 1 for the single-key-frame callers, 24 for the
            // `--lag-in-frames=16` inter callers. Asserted ABOVE the oracle call,
            // so a decode that showed fewer reds naming the encode rather than
            // handing ffmpeg the same short count.
            assert_eq!(
                decoded.len(),
                frames,
                "{name}: seed {seed}: the encode codes {frames} frame(s), the decode showed {}",
                decoded.len()
            );
            let reference = ffmpeg_decode_sequence_10bit(&stream, width, height, decoded.len());
            assert_eq!(
                decoded.len(),
                reference.len(),
                "{name}: seed {seed}: shown-frame count vs ffmpeg"
            );
            for (i, (ours, theirs)) in decoded.iter().zip(reference.iter()).enumerate() {
                for (plane, (a, b), w) in [
                    ("Y", (&ours.y, &theirs.y), width),
                    ("U", (&ours.u, &theirs.u), width / 2),
                    ("V", (&ours.v, &theirs.v), width / 2),
                ] {
                    if let Some(d) = first_diff(a, b, w) {
                        panic!("{name}: seed {seed} frame {i} plane {plane}: {d} [{fired}]");
                    }
                    assert_eq!(
                        a.len(),
                        b.len(),
                        "{name}: seed {seed} frame {i} plane {plane}: ours has {} samples, \
                         ffmpeg's has {} -- a short plane would zip-truncate the walk above \
                         and hide a chroma-span defect",
                        a.len(),
                        b.len()
                    );
                }
            }
            compared += 1;
            for (i, d) in deltas.iter().enumerate() {
                totals[i] += d;
            }
        }
        assert!(
            compared > 0,
            "{name}: every attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
        for (i, (label, _)) in counters.iter().enumerate() {
            assert!(
                totals[i] > 0,
                "{name}: {compared} attempt(s) decoded pixel-exact but {label} never fired -- \
                 the gate would prove nothing about that tool. refusals:\n{}",
                refusals.join("\n")
            );
        }
        let summary: Vec<String> = counters
            .iter()
            .zip(totals.iter())
            .map(|((label, _), t)| format!("{label}={t}"))
            .collect();
        eprintln!(
            "{name}: {compared}/{} attempts pixel-exact, {}",
            seeds.len(),
            summary.join(" ")
        );
    }

    pub(super) fn intra_rect_in_inter_gate(bit_depth: u32) {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_an_intra_rect_strip_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (96usize, 96usize, 8usize);
        let mut refusals: Vec<String> = Vec::new();
        let mut uncounted_exact = 0u32;
        let mut compared = 0u32;
        let mut shapes = [0usize; 3];
        for attempt in 0..45u32 {
            let seed = 42 + attempt;
            // mandelbrot's fractal detail plus a HARD cut at frame 4: half
            // the picture becomes a flat white box, which no motion vector
            // can predict, so aomenc's RD codes those blocks INTRA inside an
            // inter frame. The box's vertical edge at x=48 is what makes the
            // partition search reach for HORZ/VERT strips there.
            // A FAST ZOOM: `end_scale`/`end_pts` make mandelbrot rush
            // through its own zoom, so consecutive frames share almost no
            // content and aomenc's RD codes blocks INTRA inside INTER frames
            // (`--kf-min-dist=1000` forbids a new key frame). That is the
            // only way a real encoder reaches this decoder's intra-in-inter
            // arm; a slowly-evolving source codes every rect strip inter.
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
            // MEASURED (r1): the speed preset gates the feature (class
            // [[gate-preset-gates-the-feature]]) -- at `--cpu-used=4` this
            // content produces no rect partition at all, at `--cpu-used=0`
            // aomenc emits AB partitions the tile path refuses (it ignores
            // `--enable-ab-partitions=0`, ledger). So the attempt sweep walks
            // the preset as well as the seed.
            let cpu = format!("--cpu-used={}", attempt % 5);
            // The quantiser drives how finely the RD partitions: the sweep
            // walks it too, so "no rect strip fired" cannot be an artefact of
            // one cq.
            let cq = format!("--cq-level={}", [30u32, 20, 12][(attempt / 5) as usize % 3]);
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args(depth_args)
                    .arg(&cpu)
                    .arg(&cq)
                    .args([
                        // class [[aomenc-first-flag-wins]]: every flag is spelled
                        // exactly ONCE in this recipe, so there is no override to
                        // order. These four are what this gate is about.
                        "--enable-rect-partitions=1",
                        // MEASURED (r1): the charter's `--min-partition-size=8`
                        // makes 24/24 attempts stop at a sub-16 AB-partition
                        // refusal ("a HORZ_A/HORZ_B/VERT_A partition below 16x16",
                        // "a coded (non-skip) HORZ_B/VERT_B rect strip below
                        // 16x16") before any intra rect strip is reached -- those
                        // are other lanes' surfaces. 16 keeps the 16x8/8x16 shape
                        // reachable (a 16x16 block's own HORZ/VERT children).
                        "--min-partition-size=16",
                        "--max-partition-size=64",
                        // MEASURED (r1): with tx-size search ON, 24/24 attempts stop at
                        // the SQUARE intra-in-inter arm's own refusal ("an intra
                        // block in an inter frame whose tx_depth splits its luma
                        // transform (round 1)", read_block_tx_size) long before a
                        // rect strip is reached -- that arm is another lane's
                        // surface. OFF, no tx-depth symbol is coded at all and
                        // every rect strip here is the unsplit single-transform
                        // case. The SPLIT rect arm (`depth_to_tx_wh` +
                        // decode_rect_split's per-unit walk) is wired but stays
                        // UNGATED on this path until that square refusal lifts.
                        "--enable-tx-size-search=0",
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--lag-in-frames=0",
                        "--auto-alt-ref=0",
                        // One key frame at the head: the frames after the cut must
                        // stay INTER frames carrying intra blocks, not become a
                        // new key frame.
                        "--kf-min-dist=1000",
                        "--kf-max-dist=1000",
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
                        // The intra-in-inter arm reads no `use_filter_intra`
                        // symbol (square OR rect) -- lane-fiinter owns that read.
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
            let before = [
                decode::intra_rect_strip_in_inter_hits(0),
                decode::intra_rect_strip_in_inter_hits(1),
                decode::intra_rect_strip_in_inter_hits(2),
            ];
            let kf_rect_before = decode::rect_partition_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "intra-rect-in-inter decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed} {cpu} {cq}: {msg}"));
                    continue;
                }
            };
            let delta = [
                decode::intra_rect_strip_in_inter_hits(0) - before[0],
                decode::intra_rect_strip_in_inter_hits(1) - before[1],
                decode::intra_rect_strip_in_inter_hits(2) - before[2],
            ];
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "intrarect gate {bit_depth}-bit seed={seed} {cpu} {cq}: intra rect strips in inter frames \
                 64-level={} 32-level={} 16-level={}, key-frame rect strips this stream={}",
                delta[0],
                delta[1],
                delta[2],
                decode::rect_partition_hits() - kf_rect_before
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
                            "intrarect gate {bit_depth}-bit seed={seed}: frame {i} {plane} first \
                             diff at ({}, {}) got {} want {} ({} samples differ)",
                            at % stride,
                            at / stride,
                            g[at],
                            w[at],
                            g.iter().zip(w.iter()).filter(|(a, b)| a != b).count(),
                        );
                        // Magnitude separates a deblock/prediction difference
                        // (all |d| == 1) from an entropy desync (large,
                        // arbitrary): the first thing r2 needs to know.
                        let maxd = g
                            .iter()
                            .zip(w.iter())
                            .map(|(a, b)| i32::from(*a).abs_diff(i32::from(*b)))
                            .max()
                            .unwrap_or(0);
                        let rows: std::collections::BTreeSet<usize> = g
                            .iter()
                            .zip(w.iter())
                            .enumerate()
                            .filter(|(_, (a, b))| a != b)
                            .map(|(k, _)| k / stride)
                            .collect();
                        eprintln!(
                            "intrarect gate {bit_depth}-bit seed={seed}: frame {i} {plane} max \
                             |diff|={maxd}, rows {:?}..{:?}",
                            rows.iter().next(),
                            rows.iter().next_back()
                        );
                    }
                    assert_eq!(g, w, "{plane} vs ffmpeg (seed {seed}, frame {i})");
                }
            }
            compared += 1;
            if delta.iter().all(|d| *d == 0) {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed} {cpu} {cq}: decoded and pixel-compared, but no intra rect strip on the \
                     inter path"
                ));
            }
            for k in 0..3 {
                shapes[k] += delta[k];
            }
            if shapes.iter().filter(|n| **n > 0).count() >= 2 {
                break;
            }
        }
        gate_buckets(
            NAME,
            compared - uncounted_exact,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        eprintln!(
            "{NAME} ({bit_depth}-bit): compared {compared} streams, intra rect strips in inter \
             frames per shape: 64x32/32x64={} 32x16/16x32={} 16x8/8x16={}; refusals:\n{}",
            shapes[0],
            shapes[1],
            shapes[2],
            refusals.join("\n")
        );
        assert!(
            compared > 0,
            "{NAME} ({bit_depth}-bit): every attempt hit a named refusal -- nothing was \
             pixel-compared:\n{}",
            refusals.join("\n")
        );
        // MEASURED (r1, 90 attempts; r2 re-ran the 45-attempt sweep at both
        // depths): only the 32x16/16x32 class is REACHABLE on this branch --
        // the inter tile path refuses any SB-level partition other than
        // NONE/SPLIT, so a 64x32/32x64 intra strip cannot be produced at all,
        // and every 16x8/8x16 candidate stops at a sub-16 refusal owned by
        // another lane. So the bar is that class, hard, not "two of three":
        // a green run still cannot come from a stream that fired nothing.
        assert!(
            shapes[1] > 0,
            "{NAME} ({bit_depth}-bit): no 32x16/16x32 intra rect strip fired over \
             {compared} compared streams (64-level={} 32-level={} 16-level={})",
            shapes[0],
            shapes[1],
            shapes[2]
        );
    }

    pub(super) fn tx_select_inter_gate(bit_depth: u32) {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // Detail is what makes aomenc's RD prefer a split transform; a
            // flat gradient resolves every `txfm_split` to 0. `mandelbrot`'s
            // fractal boundary is the same source the sibling intra tx-select
            // gate uses, at a different `start_scale` per attempt.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
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
                    // Desaturated chroma: `uv_mode` stays DC_PRED, so the
                    // still-refused directional-chroma gap never fires and
                    // luma detail alone drives the transform split.
                    "-vf",
                    "hue=s=0",
                    "-pix_fmt",
                    if bit_depth == 10 {
                        "yuv420p10le"
                    } else {
                        "yuv420p"
                    },
                    // y4m carries no official 10-bit tag; ffmpeg writes one
                    // only under `-strict -1` (a no-op at 8 bit).
                    "-strict",
                    "-1",
                    "-t",
                    "0.16",
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
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(depth_args).args([
                    // lane-defon r1: explicit on-value (aomenc keeps the FIRST
                    // occurrence, so overrides go before the base list).
                    "--enable-tx-size-search=1",
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
                    // DELIBERATELY ABSENT: --enable-tx-size-search=0.
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    // Sub-16 inter partitions are a separate named refusal
                    // (this decoder codes no inter block below 16x16 through
                    // the square path this gate drives), and detailed content
                    // makes aomenc's RD reach for them on nearly every seed.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
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
            let (reads_before, hits_before) =
                (decode::txfm_split_reads(), decode::txfm_split_hits());
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "tx-select inter decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let (reads, hits) = (
                decode::txfm_split_reads() - reads_before,
                decode::txfm_split_hits() - hits_before,
            );
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = hits != 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no block took a txfm_split \
                     ({reads} symbols read)"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "txselect gate {bit_depth}-bit seed={seed}: txfm_split reads={reads} splits taken={hits}"
            );
            // Name the first differing sample before the assert fires: a
            // 64x64 plane's `assert_eq!` output is 4096 numbers, useless for
            // locating a desync.
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                for (plane, (g, w)) in [
                    ("Y", (&got.y, &want.y)),
                    ("U", (&got.u, &want.u)),
                    ("V", (&got.v, &want.v)),
                ] {
                    let stride = if plane == "Y" { width } else { width / 2 };
                    if let Some(at) = g.iter().zip(w.iter()).position(|(a, b)| a != b) {
                        eprintln!(
                            "txselect gate seed={seed}: frame {i} {plane} first diff at \
                             ({}, {}) got {} want {} ({} samples differ)",
                            at % stride,
                            at / stride,
                            g[at],
                            w[at],
                            g.iter().zip(w.iter()).filter(|(a, b)| a != b).count(),
                        );
                        let coords: Vec<String> = g
                            .iter()
                            .zip(w.iter())
                            .enumerate()
                            .filter(|(_, (a, b))| a != b)
                            .take(12)
                            .map(|(k, (a, b))| format!("({},{})={a}/{b}", k % stride, k / stride))
                            .collect();
                        eprintln!("txselect gate seed={seed}: frame {i} {plane} diffs {coords:?}");
                    }
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                gate_buckets(
                    &format!("tx_select_inter_gate({bit_depth})"),
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            &format!("tx_select_inter_gate({bit_depth})"),
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "tx_select_inter_gate({bit_depth}) never observed a taken txfm_split:\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn intra_in_inter_split_tx_gate(bit_depth: u32) {
        let name = format!("intra_in_inter_split_tx_gate({bit_depth})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A zooming fractal: every frame moves (so the sequence is
            // genuinely inter-coded) and keeps sharp new detail entering at
            // the borders, which is what makes aomenc's RD choose an INTRA
            // block inside an inter frame -- and then split its transform.
            // A zooming fractal with a hard CUT halfway (`overlay` switching
            // a second, unrelated fractal on at frame 4): after the cut every
            // block is unpredictable from the reference, which is what makes
            // aomenc code INTRA blocks inside an inter frame -- and at this
            // cq the RD then splits their transforms.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25[a];\
                 mandelbrot=size=64x64:start_scale={}:rate=25[b];\
                 [a][b]overlay=enable='gte(n,4)'",
                5.0 - f64::from(attempt) * 0.06,
                0.9 + f64::from(attempt) * 0.11
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
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(depth_args).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=30",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    // One key frame only: every later frame is an inter frame,
                    // which is where this gate's intra blocks must sit. The
                    // MIN distance is what stops aomenc's scene-cut detector
                    // from inserting a second key frame at the cut this gate's
                    // source deliberately contains.
                    "--kf-max-dist=1000",
                    "--kf-min-dist=1000",
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
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag): the intra
                    // tx_depth symbol only exists under TX_MODE_SELECT, and
                    // rect partitions are a separate named refusal.
                    "--enable-tx-size-search=1",
                    "--enable-rect-partitions=0",
                    "--max-partition-size=32",
                    // Sub-16 inter partitions are a separate named refusal,
                    // as is the 8x8 intra leaf's own TX_4X4 split.
                    "--min-partition-size=16",
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
            let before = decode::intra_in_inter_split_tx_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let after = decode::intra_in_inter_split_tx_hits();
            let mut per_size = [0usize; 4];
            for i in 0..4 {
                per_size[i] = after[i] - before[i];
            }
            // class [[gate-skips-on-its-own-failure]]: a decode that
            // succeeded is ALWAYS pixel-compared below; the counter only
            // decides whether the attempt counts as firing.
            let counted = per_size.iter().sum::<usize>() != 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no intra block in an inter \
                     frame split its transform"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "{name} seed={seed}: split intra-in-inter blocks 8x8={} 16x16={} 32x32={} \
                 64x64={}",
                per_size[0], per_size[1], per_size[2], per_size[3]
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                gate_buckets(
                    &name,
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            &name,
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "{name} never observed a split-transform intra block in an inter frame:\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn filter_intra_in_inter_gate(bit_depth: u32) {
        let name = format!("filter_intra_in_inter_gate({bit_depth})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A zooming fractal: every frame moves (so the sequence is
            // genuinely inter-coded) and keeps sharp new detail entering at
            // the borders, which is what makes aomenc's RD choose an INTRA
            // block inside an inter frame -- and then split its transform.
            // A zooming fractal with a hard CUT halfway (`overlay` switching
            // a second, unrelated fractal on at frame 4): after the cut every
            // block is unpredictable from the reference, which is what makes
            // aomenc code INTRA blocks inside an inter frame -- and at this
            // cq the RD then splits their transforms.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25[a];\
                 mandelbrot=size=64x64:start_scale={}:rate=25[b];\
                 [a][b]overlay=enable='gte(n,4)'",
                5.0 - f64::from(attempt) * 0.06,
                0.9 + f64::from(attempt) * 0.11
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
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(depth_args).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=30",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    // One key frame only: every later frame is an inter frame,
                    // which is where this gate's intra blocks must sit. The
                    // MIN distance is what stops aomenc's scene-cut detector
                    // from inserting a second key frame at the cut this gate's
                    // source deliberately contains.
                    "--kf-max-dist=1000",
                    "--kf-min-dist=1000",
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
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag): the intra
                    // tx_depth symbol only exists under TX_MODE_SELECT, and
                    // rect partitions are a separate named refusal.
                    "--enable-filter-intra=1",
                    "--enable-tx-size-search=1",
                    "--enable-rect-partitions=0",
                    "--max-partition-size=32",
                    // Sub-16 inter partitions are a separate named refusal,
                    // as is the 8x8 intra leaf's own TX_4X4 split.
                    "--min-partition-size=16",
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
            let before = decode::intra_in_inter_filter_intra_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let fired = decode::intra_in_inter_filter_intra_hits() - before;
            // class [[gate-skips-on-its-own-failure]]: a decode that
            // succeeded is ALWAYS pixel-compared below; the counter only
            // decides whether the attempt counts as firing.
            let counted = fired != 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no intra block in an inter \
                     frame used filter intra"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!("{name} seed={seed}: filter-intra blocks inside inter frames = {fired}");
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                gate_buckets(
                    &name,
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            &name,
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "{name} never observed a filter-intra intra block in an inter frame:\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn angle_delta_in_inter_gate(bit_depth: u32, min_partition: &str) {
        let name = format!("angle_delta_in_inter_gate({bit_depth}, min={min_partition})");
        let leaf8 = min_partition.ends_with('8');
        assert!(
            !leaf8,
            "the 8x8-leaf arm keeps its refusal; see the module note"
        );
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A zooming fractal: every frame moves (so the sequence is
            // genuinely inter-coded) and keeps sharp new detail entering at
            // the borders, which is what makes aomenc's RD choose an INTRA
            // block inside an inter frame -- and then split its transform.
            // A zooming fractal with a hard CUT halfway (`overlay` switching
            // a second, unrelated fractal on at frame 4): after the cut every
            // block is unpredictable from the reference, which is what makes
            // aomenc code INTRA blocks inside an inter frame -- and at this
            // cq the RD then splits their transforms.
            let source = format!(
                "mandelbrot=size={width}x{height}:start_scale={}:rate=25[a];\
                 mandelbrot=size={width}x{height}:start_scale={}:rate=25[b];\
                 [a][b]overlay=enable='gte(n,4)'",
                5.0 - f64::from(attempt) * 0.06,
                0.9 + f64::from(attempt) * 0.11
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
            // The 8x8-leaf arm is fenced by three sibling refusals (non-DC
            // chroma on an 8x8 leaf, its TX_4X4 split, sub-16 rect inter
            // leaves), so it sweeps the quantiser as well as the source: a
            // coarser q is what pushes the RD to directional intra at 8x8.
            let cq = if leaf8 {
                format!("--cq-level={}", 20 + (attempt % 5) * 10)
            } else {
                "--cq-level=30".to_string()
            };
            let depth_args: &[&str] = if bit_depth == 10 {
                &["-b", "10", "--input-bit-depth=10"]
            } else {
                &[]
            };
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(depth_args).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq,
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    // One key frame only: every later frame is an inter frame,
                    // which is where this gate's intra blocks must sit. The
                    // MIN distance is what stops aomenc's scene-cut detector
                    // from inserting a second key frame at the cut this gate's
                    // source deliberately contains.
                    "--kf-max-dist=1000",
                    "--kf-min-dist=1000",
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
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=1",
                    "--enable-angle-delta=1",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag): the intra
                    // tx_depth symbol only exists under TX_MODE_SELECT, and
                    // rect partitions are a separate named refusal.
                    "--enable-filter-intra=1",
                    "--enable-tx-size-search=1",
                    "--enable-rect-partitions=0",
                    "--max-partition-size=32",
                    // Sub-16 inter partitions are a separate named refusal,
                    // as is the 8x8 intra leaf's own TX_4X4 split.
                    min_partition,
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
            let before = decode::intra_in_inter_angle_delta_y_hits();
            let before_uv = decode::intra_in_inter_angle_delta_uv_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let fired = decode::intra_in_inter_angle_delta_y_hits() - before;
            let fired_uv = decode::intra_in_inter_angle_delta_uv_hits() - before_uv;
            // class [[gate-skips-on-its-own-failure]]: a decode that
            // succeeded is ALWAYS pixel-compared below; the counter only
            // decides whether the attempt counts as firing.
            let counted = fired != 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no intra block in an inter \
                     frame carried a nonzero angle_delta_y"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "{name} seed={seed}: intra-in-inter blocks with a nonzero angle_delta_y = \
                 {fired}, angle_delta_uv = {fired_uv}"
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                gate_buckets(
                    &name,
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            &name,
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "{name} never observed a nonzero angle delta on an intra block in an inter \
             frame:\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn uv_mode_in_inter8_gate(bit_depth: u32, tile_columns: u32) {
        let name = format!("uv_mode_in_inter8_gate({bit_depth}, tiles={tile_columns})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 8usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        let mut totals = (0usize, 0usize, 0usize);
        for attempt in 0..30u32 {
            let seed = 42 + attempt;
            // A zooming fractal with a hard CUT at frame 4 (`overlay`
            // switching a second, unrelated fractal on): after the cut a block
            // is unpredictable from the reference, which is what makes aomenc
            // code INTRA blocks inside an inter frame. Chroma is NOT
            // desaturated here (every sibling inter gate passes `hue=s=0`) --
            // real chroma detail is exactly what pushes the chroma RD off
            // DC_PRED. `noise` carries its own explicit seed (lavfi
            // `gradients` ignores its seed; class [[seeded-fixture-not-reproducible]]).
            let source = format!(
                "mandelbrot=size={width}x{height}:start_scale={}:rate=25[a];\
                 mandelbrot=size={width}x{height}:start_scale={}:rate=25[b];\
                 [a][b]overlay=enable='gte(n,4)',noise=alls=12:allf=t+u:all_seed={seed}",
                5.0 - f64::from(attempt) * 0.06,
                0.9 + f64::from(attempt) * 0.11
            );
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
            // A coarser quantiser is what pushes the chroma RD to a cheap
            // non-DC mode on a small block; sweep 12..40 (the low end is what
            // lane-cdef's cq12 arm needs).
            let cq = format!("--cq-level={}", 12 + (attempt % 5) * 7);
            let depth_args: &[&str] = if bit_depth == 10 {
                &["-b", "10", "--input-bit-depth=10"]
            } else {
                &[]
            };
            let tiles = format!("--tile-columns={}", tile_columns - 1);
            // The y4m goes through a FILE, not aomenc's stdin: this gate's
            // stream is ~100 KB, bigger than a pipe buffer, so an aomenc that
            // is still being fed on stdin blocks writing its stdout and the
            // test blocks writing its stdin -- a hard deadlock (measured
            // here, 3 min into attempt 0, class [[gate-loader-slurps-whole-file]]).
            // The path is PID-keyed (class [[pid-keyed-temp-path]]).
            let y4m_path = std::env::temp_dir().join(format!(
                "ec-av1-uv8-{}-{bit_depth}-{tile_columns}-{seed}.y4m",
                std::process::id()
            ));
            std::fs::write(&y4m_path, &y4m.stdout).expect("writing the y4m fixture");
            let child = Command::new(aomenc_path())
                .args(depth_args)
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq,
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=9999",
                    "--kf-min-dist=9999",
                    "--threads=1",
                    "--row-mt=0",
                    &tiles,
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
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-ref-frame-mvs=0",
                    // Per-arm overrides go LAST (aomenc keeps the LAST
                    // occurrence of a repeated --enable-* flag). The three
                    // chroma intra tools this gate is about, plus the fences
                    // that keep the leaf's SIBLING refusals (its TX_4X4
                    // split, a nonzero angle delta, sub-16 rect inter leaves)
                    // out of the way.
                    "--enable-cfl-intra=1",
                    "--enable-smooth-intra=1",
                    "--enable-paeth-intra=1",
                    "--enable-directional-intra=1",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-rect-partitions=0",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--obu",
                    "-o",
                    "-",
                ])
                .arg(&y4m_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            let out = child.wait_with_output().expect("aomenc failed to run");
            let _ = std::fs::remove_file(&y4m_path);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::intra_in_inter8_uv_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let now = decode::intra_in_inter8_uv_hits();
            // class [[counter-from-refused-stream]]: per-attempt deltas, and
            // only for an attempt that decoded to the end and is pixel-
            // compared below.
            let fired = (now.0 - before.0, now.1 - before.1, now.2 - before.2);
            totals = (totals.0 + fired.0, totals.1 + fired.1, totals.2 + fired.2);
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared; the counter only decides whether the
            // attempt counts as firing.
            // lane-uv8 r3: every one of the three chroma classes this gate
            // exists for (directional, smooth/paeth, CfL) must fire on the
            // SAME pixel-compared attempt -- a sum-nonzero predicate would
            // pass on an attempt that only ever hit DC-adjacent CfL.
            let counted = fired.0 > 0 && fired.1 > 0 && fired.2 > 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no 8x8 intra leaf in an inter \
                     frame carried a non-DC uv_mode"
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "{name} seed={seed} {cq}: 8x8 intra-in-inter leaves with a non-DC uv_mode -- \
                 directional {}, smooth/paeth {}, CfL {}",
                fired.0, fired.1, fired.2
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                gate_buckets(
                    &name,
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            &name,
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "{name} never observed a non-DC uv_mode on an 8x8 intra leaf in an inter frame \
             (totals dir/smooth/cfl {totals:?}):\n{}",
            refusals.join("\n")
        );
    }

    pub(super) fn tx_split_angle_in_inter8_gate(bit_depth: u32) {
        let name = format!("tx_split_angle_in_inter8_gate({bit_depth})");
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 6usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        let mut counted_exact = 0u32;
        let mut totals = (0usize, 0usize);
        for attempt in 0..30u32 {
            let seed = 42 + attempt;
            // Same zoom + hard-cut fractal source the sibling uv_mode gate
            // uses (a cut is what makes aomenc code INTRA blocks inside an
            // inter frame); `noise` carries an explicit seed (class
            // [[seeded-fixture-not-reproducible]]).
            let source = format!(
                "mandelbrot=size={width}x{height}:start_scale={}:rate=25[a];\
                 mandelbrot=size={width}x{height}:start_scale={}:rate=25[b];\
                 [a][b]overlay=enable='gte(n,3)',noise=alls=12:allf=t+u:all_seed={seed}",
                5.0 - f64::from(attempt) * 0.06,
                0.9 + f64::from(attempt) * 0.11
            );
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
                    "0.24",
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
            let cq = format!("--cq-level={}", 12 + (attempt % 5) * 7);
            let depth_args: &[&str] = if bit_depth == 10 {
                &["-b", "10", "--input-bit-depth=10"]
            } else {
                &[]
            };
            // The y4m goes through a PID-keyed FILE, never aomenc's stdin
            // (pipe deadlock, class [[gate-loader-slurps-whole-file]]).
            let y4m_path = std::env::temp_dir().join(format!(
                "ec-av1-leaf8tx-{}-{bit_depth}-{seed}.y4m",
                std::process::id()
            ));
            std::fs::write(&y4m_path, &y4m.stdout).expect("writing the y4m fixture");
            let child = Command::new(aomenc_path())
                .args(depth_args)
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq,
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=9999",
                    "--kf-min-dist=9999",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=0",
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
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-ref-frame-mvs=0",
                    // Per-arm overrides LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag). Rect
                    // partitions stay off so the leaf's unrelated sub-16 rect
                    // inter refusal never fires.
                    "--enable-rect-partitions=0",
                    "--enable-tx-size-search=1",
                    "--enable-angle-delta=1",
                    "--enable-directional-intra=1",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--obu",
                    "-o",
                    "-",
                ])
                // r1 diagnostic: EC_LEAF8TX_CONTROL=1 turns THIS lane's two
                // tools back off (last occurrence of a repeated --enable-*
                // wins), so any mismatch that survives is not this lane's.
                .args(match std::env::var("EC_LEAF8TX_CONTROL").as_deref() {
                    Ok("angle") => vec!["--enable-tx-size-search=0", "--enable-angle-delta=1"],
                    Ok("tx") => vec!["--enable-tx-size-search=1", "--enable-angle-delta=0"],
                    Ok("tx8") => vec![
                        "--enable-tx-size-search=1",
                        "--enable-angle-delta=0",
                        "--max-partition-size=8",
                    ],
                    Ok(_) => vec!["--enable-tx-size-search=0", "--enable-angle-delta=0"],
                    // Shipped recipe (lane-leaf8tx r4): BOTH of this lane's
                    // tools on -- the tx_depth split at an 8x8 intra-in-inter
                    // leaf is what the lifted refusal covers, so
                    // `--enable-tx-size-search=1` must survive the per-arm
                    // override (aomenc keeps the LAST occurrence). Odd
                    // attempts also cap the partition at 8x8, the shape that
                    // produces the 2x2 TX_4X4 grid most often.
                    Err(_) => {
                        if attempt % 2 == 1 {
                            vec![
                                "--enable-tx-size-search=1",
                                "--enable-angle-delta=1",
                                "--max-partition-size=8",
                            ]
                        } else {
                            vec!["--enable-tx-size-search=1", "--enable-angle-delta=1"]
                        }
                    }
                })
                .arg(&y4m_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            let out = child.wait_with_output().expect("aomenc failed to run");
            let _ = std::fs::remove_file(&y4m_path);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = (
                decode::intra_in_inter_split_tx_hits()[0],
                decode::intra_in_inter8_angle_delta_y_hits(),
            );
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{name} decode failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
            };
            let now = (
                decode::intra_in_inter_split_tx_hits()[0],
                decode::intra_in_inter8_angle_delta_y_hits(),
            );
            let fired = (now.0 - before.0, now.1 - before.1);
            totals = (totals.0 + fired.0, totals.1 + fired.1);
            // Both lifted capabilities must fire on the SAME pixel-compared
            // attempt.
            let counted = fired.1 > 0;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but split-tx {} / angle-delta {} \
                     8x8 intra-in-inter leaves",
                    fired.0, fired.1
                ));
            }
            let ffmpeg_frames = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            eprintln!(
                "{name} seed={seed} {cq}: 8x8 intra-in-inter leaves with tx_depth=1 {}, with a \
                 nonzero angle_delta_y {} (split-tx per size 8/16/32/64 {:?})",
                fired.0,
                fired.1,
                decode::intra_in_inter_split_tx_hits()
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                counted_exact += 1;
            }
        }
        // lane-leaf8tx r4: continue-and-sweep -- every arm is encoded,
        // decoded and pixel-compared on every plane of every decode-order
        // frame above; the counters are asserted ONCE here. A decode error or
        // a mismatch is a failure, never a SKIP, and `uncounted_exact`
        // (decoded + compared but neither capability fired) is the
        // out-of-scope-mismatch bucket that the sweep tolerates only while at
        // least one attempt did fire.
        gate_buckets(
            &name,
            counted_exact,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        assert!(
            totals.0 > 0,
            "{name}: no decoded+compared attempt carried an 8x8 intra-in-inter leaf with \
             tx_depth split to 4x4 (split-tx per size {:?}, angle-delta {}):\n{}",
            decode::intra_in_inter_split_tx_hits(),
            totals.1,
            refusals.join("\n")
        );
        assert!(
            totals.1 > 0,
            "{name}: no decoded+compared attempt carried a nonzero angle_delta_y on an 8x8 \
             intra-in-inter leaf (split-tx {}):\n{}",
            totals.0,
            refusals.join("\n")
        );
    }

    /// As above, but a multi-frame inter sequence -- `--lag-in-frames=0
    /// --auto-alt-ref=0` keep every non-key frame a simple forward
    /// single-`LAST_FRAME` P frame (no alt-ref/backward reference this
    /// decoder's inter path does not model), `--kf-max-dist` past the clip's
    /// own length keeps frame 0 the only key frame.
    /// lane-inter8 r1: the inter tile path's SB-level `PARTITION_NONE` -- a
    /// whole 64x64 superblock coded as ONE inter block, which is what aomenc
    /// picks for static content and which this decoder refused by name
    /// ("an inter SB-level partition type other than SPLIT") until this
    /// round. HARD-asserts [`decode::inter_sb_none_hits`] moved (the arm
    /// really fired, class refusal-lifted-without-a-gate) and every frame is
    /// pixel-exact vs ffmpeg; a decode error is a FAILURE here, never a SKIP
    /// (class gate-skips-on-its-own-failure).
    /// lane-inter8 r1: generalised so the same recipe can target a deeper
    /// level of the inter partition tree -- `min_part` is aomenc's
    /// `--min-partition-size`, and `hits`/`what` name the counter that proves
    /// the arm under test really fired.
    #[track_caller]
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn inter_sb_none_gate(
        name: &str,
        ten_bit: bool,
        min_part: &str,
        max_part: &str,
        hits: fn() -> usize,
        what: &str,
        // lane-inter8 r3: extra aomenc flags + the frame width they need.
        // `--tile-columns=1` needs a frame at least two superblocks wide or
        // aomenc silently codes one tile column and the arm is vacuous.
        extra: &[&str],
        width: usize,
        // lane-interp3 r1: sequence length -- a compound block needs several
        // already-coded references, which a 4-frame sequence never has.
        frame_count: usize,
        // lane-av1dualfilter10: the encoded stream, so a caller can assert on
        // the PARSED sequence header (bit depth, `enable_dual_filter`) rather
        // than on the flag it asked for. Empty on the SKIP paths below.
    ) -> Vec<u8> {
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return Vec::new();
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return Vec::new();
        }
        let height = 64usize;
        let duration = format!("{:.2}", frame_count as f64 / 25.0);
        // A deterministic synthetic source with real motion (a static source
        // codes every inter frame as skip and never exercises the residual
        // path); bounded with `-t` (class gate-loader-slurps-whole-file).
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("mandelbrot=size={width}x{height}:rate=25"),
                "-t",
                &duration,
                "-pix_fmt",
                if ten_bit { "yuv420p10le" } else { "yuv420p" },
                // y4m calls 10-bit unofficial (same as the 10-bit key-frame
                // gate's own fixture).
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
        let mut args: Vec<String> = [
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=40",
            "--cpu-used=0",
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--kf-max-dist=1000",
            // No cross-frame CDF forwarding in this fixture (see the
            // deblocking gate's own note).
            "--error-resilient=1",
            // This gate targets the SB-level NONE arm only: rect/AB/1-to-4
            // partitions and sub-16x16 splits stay off so a failure here
            // cannot be another lane's missing arm.
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
            "--obu",
            "-o",
            "-",
            "-",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        for e in extra {
            args.insert(1, (*e).to_string());
        }
        args.insert(1, format!("--min-partition-size={min_part}"));
        args.insert(1, format!("--max-partition-size={max_part}"));
        if ten_bit {
            args.insert(1, "--input-bit-depth=10".to_string());
            args.insert(1, "--bit-depth=10".to_string());
        }
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{name}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                let pin = std::env::temp_dir().join(format!("{name}-refused.obu"));
                crate::dumpio::pin("TEST_PIN", &pin, &stream);
                panic!(
                    "{name}: decode_stream refused a real aomenc stream: {e} (pinned at {})",
                    pin.display()
                );
            }
        };
        assert!(
            hits() > before,
            "{name}: no {what} fired -- this gate proves nothing"
        );
        let ffmpeg_frames = if ten_bit {
            ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
        } else {
            ffmpeg_decode_sequence(&stream, width, height, frame_count)
        };
        assert_eq!(frames.len(), frame_count, "{name}: frame count");
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if got.y != want.y || got.u != want.u || got.v != want.v {
                let pin = std::env::temp_dir().join(format!("{name}-mismatch.obu"));
                crate::dumpio::pin("TEST_PIN", &pin, &stream);
                panic!(
                    "{name}: frame {i} mismatch vs ffmpeg -- stream pinned at {}",
                    pin.display()
                );
            }
        }
        stream
    }

    /// lane-eobc1 r1: an INTER sequence whose CHROMA coefficient blocks come
    /// off the class-1 (`TX_CLASS_HORIZ`/`VERT`) `eob_pt` row. Chroma codes
    /// no `tx_type` symbol of its own -- it inherits the colocated luma type
    /// (`av1_get_tx_type`, `blockd.h:1291`) -- so an 8x8 inter leaf whose
    /// luma picks `H_*`/`V_*` puts its 4x4 chroma on a CDF row this crate
    /// wired for only some sizes until this round (`eob_pt_class1: None` in
    /// 9 of 16 `TxbSet` arms, chroma 4x4/8x8 among them).
    ///
    /// The full tx set stays on (no `--reduced-tx-type-set=1`) and
    /// `--enable-flip-idtx=1` is asked for explicitly; the sinusoidal-stripe
    /// luma of the `tx_class1` key-frame gate is what actually resolves 1D
    /// types, and the temporally-moving chroma ramp keeps chroma out of
    /// `all_zero` so it has an `eob` to read at all. Attempts sweep cq and
    /// stripe period; a run counts only when
    /// [`decode::chroma_eob_class1_hits`] moved, and every shown frame is
    /// compared pixel-exact vs ffmpeg (a decode error is a FAILURE, never a
    /// SKIP -- class gate-skips-on-its-own-failure).
    pub(super) fn chroma_class1_eob_gate(name: &str, ten_bit: bool) {
        if !have_ffmpeg() {
            eprintln!("SKIP {name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        let mut refusals = Vec::new();
        for attempt in 0..12u32 {
            let cq = 28 + (attempt % 4) * 10;
            let period = [4u32, 6, 8][(attempt as usize / 4) % 3];
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size={width}x{height}:rate=25"),
                    "-vf",
                    &format!(
                        "geq=lum='128+80*sin(2*PI*Y/{period})':cb='128+60*sin(2*PI*(X+8*T)/{period})':cr='128+60*sin(2*PI*(Y+8*T)/{period})',noise=alls=6:allf=t:all_seed=7"
                    ),
                    "-t",
                    "0.16",
                    "-pix_fmt",
                    if ten_bit { "yuv420p10le" } else { "yuv420p" },
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
            let mut args: Vec<String> = [
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cpu-used=0",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                "--error-resilient=1",
                "--enable-flip-idtx=1",
                "--reduced-tx-type-set=0",
                "--min-partition-size=8",
                "--max-partition-size=16",
                // Motion tools this decoder refuses by name, kept off so a
                // refusal here can only be about the coefficient path.
                "--enable-global-motion=0",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
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
                "--loopfilter-control=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
            args.insert(1, format!("--cq-level={cq}"));
            if ten_bit {
                args.insert(1, "--input-bit-depth=10".to_string());
                args.insert(1, "--bit-depth=10".to_string());
            }
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{name}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::chroma_eob_class1_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                // A named refusal ("unsupported: ...") is another lane's arm
                // and moves this attempt on; anything else is a real decode
                // failure and fails the gate on the spot.
                Err(e) if e.to_string().contains("unsupported") => {
                    refusals.push(format!("cq={cq} period={period}: {e}"));
                    continue;
                }
                Err(e) => {
                    let pin = std::env::temp_dir().join(format!("{name}-refused.obu"));
                    crate::dumpio::pin("TEST_PIN", &pin, &stream);
                    panic!(
                        "{name}: decode_stream failed on a real aomenc stream (cq={cq} period={period}): {e} (pinned at {})",
                        pin.display()
                    );
                }
            };
            if decode::chroma_eob_class1_hits() == before {
                refusals.push(format!(
                    "cq={cq} period={period}: decoded, but no chroma block read a class-1 eob_pt row"
                ));
                continue;
            }
            let ffmpeg_frames = if ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count, "{name}: frame count");
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                if got.y != want.y || got.u != want.u || got.v != want.v {
                    let pin = std::env::temp_dir().join(format!("{name}-mismatch.obu"));
                    crate::dumpio::pin("TEST_PIN", &pin, &stream);
                    panic!(
                        "{name}: frame {i} mismatch vs ffmpeg (cq={cq} period={period}) -- stream pinned at {}",
                        pin.display()
                    );
                }
            }
            return;
        }
        panic!("{name}: no attempt fired a class-1 chroma eob: {refusals:?}");
    }

    /// lane-av1refs's decisive gate for the 5 references widened past
    /// `LAST_FRAME`/`GOLDEN_FRAME`: same envelope and pin-on-mismatch
    /// pattern as
    /// [`a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact`],
    /// generalised over `(target_ref, extra_args, frame_count)` so each of
    /// `LAST2`/`LAST3`/`BWDREF`/`ALTREF2`/`ALTREF` gets its own attempt pool
    /// and its own proof that `decode::ref_hits(target_ref)` -- not just
    /// `non_last_ref_hits`, which cannot distinguish one reference from
    /// another -- actually advanced. `--max-reference-frames=3
    /// --reduced-reference-set=1` (the golden gate's own narrowing) is
    /// DROPPED here: both suppress exactly the references this gate needs
    /// aomenc's RD to reach.
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn a_real_aomenc_single_ref_gate(
        gate_name: &str,
        target_ref: i8,
        target_ref_name: &str,
        extra_args: &[&str],
        width: usize,
        height: usize,
        frame_count: usize,
        attempts: u32,
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
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..attempts {
            let seed = 42 + attempt % 40;
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
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
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
                // use_ref_frame_mvs (temporal MV projection) is unimplemented in
                // mvstack; leaving it on silently desyncs symbols on inter frames.
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            // extra_args goes right before the trailing "-o", "-", "-" triple.
            let tail = args.split_off(args.len() - 3);
            args.extend_from_slice(extra_args);
            args.extend_from_slice(&tail);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::ref_hits(target_ref);
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
            let counted = decode::ref_hits(target_ref) != before;
            if !counted {
                never_fired += 1;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched
                && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, gate_name);
            eprintln!(
                "{gate_name} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(gate_name, &y4m.stdout, &args) {
                eprintln!(
                    "{gate_name} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            if counted {
                eprintln!(
                    "{gate_name} FIRING seed {seed}: {target_ref_name} hits advanced by {}",
                    decode::ref_hits(target_ref) - before
                );
                gate_buckets(gate_name, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(gate_name, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {gate_name}: {never_fired} attempts decoded but never fired {target_ref_name}, \
             and every other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-gmaffine r1: the two 8x8-LEAF motion gates. `decode_inter_block8`
    /// is a separate function from [`decode_inter_block`], and until this
    /// round it refused both `GLOBALMV` ("GLOBALMV (round 3)") and
    /// `WARPED_CAUSAL` at that size while the 16x16+ leaf had implemented
    /// both for rounds. The reason no earlier gate caught it: the existing
    /// 8x8 OBMC gate clamps `--max-partition-size=32` and aomenc's RD then
    /// never splits down to a clean 8x8 leaf (that gate has never fired in
    /// 80 attempts). `--min-partition-size=8 --max-partition-size=8` removes
    /// the choice entirely -- every inter leaf in the stream IS an 8x8 one,
    /// so the leaf path is exercised by construction rather than by luck.
    ///
    /// Content is the affine gate's rotating/zooming mandelbrot (global
    /// motion the encoder can actually model), and the hit counters
    /// (`globalmv_hits_8` / `warp_hits_8`) are 8x8-leaf-specific: a
    /// 16x16-leaf GLOBALMV or warp cannot satisfy them.
    pub(super) fn run_8x8_leaf_motion_gate(name: &str, global_motion: bool, counter: fn() -> usize) {
        // 72x64: `decode_inter_block8` is reached ONLY through the
        // true-edge-straddling 16x16 path (an interior 16x16
        // PARTITION_SPLIT is still refused by name), so the frame is 8
        // luma columns wider than its last whole superblock -- every 16x16
        // block in that right-hand strip is cut on ONE axis and decodes as
        // four 8x8 leaves by construction. `--min-partition-size=16` keeps
        // the interior out of the still-refused sub-16x16 case.
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let duration = frame_count as f64 / 25.0;
        // The encoder's own RD still chooses WHICH 8x8 leaf codes GLOBALMV /
        // WARPED_CAUSAL, and at some quantisers it picks a rect partition at
        // 16x16 (still refused) or a non-LAST reference (still refused at the
        // 8x8 leaf) before ever reaching one. So sweep a few quantisers and
        // require at least one attempt, at each depth, that BOTH decodes
        // pixel-exact AND fires the counter -- a refusing attempt is recorded,
        // never a pass (class `gate-skips-on-its-own-failure`).
        let mut fired_any = [false; 2];
        let mut notes: Vec<String> = Vec::new();
        for (di, depth) in [8u32, 10u32].into_iter().enumerate() {
            for cq in [32u32, 45, 55] {
                let cq_arg = format!("--cq-level={cq}");
                let max_part_arg = format!(
                    "--max-partition-size={}",
                    std::env::var("EC_8X8_MAXPART").unwrap_or_else(|_| "8".into())
                );
                let source = format!(
                    "mandelbrot=size=96x96:rate=25,rotate=a=0.12*t:c=black,\
                 scale=w='ceil(96*(1+0.03*n)/2)*2':h=96:eval=frame,crop={width}:{height}"
                );
                let pix_fmt = if depth == 10 {
                    "yuv420p10le"
                } else {
                    "yuv420p"
                };
                let mut ff_args: Vec<&str> = vec![
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", pix_fmt,
                ];
                if depth == 10 {
                    ff_args.extend(["-strict", "-1"]);
                }
                let duration_arg = duration.to_string();
                ff_args.extend(["-t", &duration_arg, "-f", "yuv4mpegpipe", "-"]);
                let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args(&ff_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                assert!(
                    y4m.status.success(),
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let bit_depth = format!("--bit-depth={depth}");
                let input_bit_depth = format!("--input-bit-depth={depth}");
                let gm_arg = format!("--enable-global-motion={}", u8::from(global_motion));
                let warp_arg = format!("--enable-warped-motion={}", u8::from(!global_motion));
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--auto-alt-ref=0",
                    "--lag-in-frames=0",
                    "--enable-fwd-kf=0",
                    "--enable-order-hint=1",
                    &gm_arg,
                    &warp_arg,
                    "--enable-obmc=0",
                    "--tune-content=default",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-onesided-comp=0",
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
                    &max_part_arg,
                    "--min-partition-size=8",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    &bit_depth,
                    &input_bit_depth,
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
                let before = counter();
                let frames = match decode_stream(&stream) {
                    Ok(f) => f,
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{name} ({depth}-bit, cq {cq}) failed outright: {msg}"
                        );
                        notes.push(format!("{depth}-bit cq {cq}: {msg}"));
                        continue;
                    }
                };
                let ffmpeg_frames = if depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                let hits = counter() - before;
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(got.y, want.y, "{name} {depth}-bit frame {i} luma vs ffmpeg");
                    assert_eq!(got.u, want.u, "{name} {depth}-bit frame {i} U vs ffmpeg");
                    assert_eq!(got.v, want.v, "{name} {depth}-bit frame {i} V vs ffmpeg");
                }
                if hits == 0 {
                    notes.push(format!(
                        "{depth}-bit cq {cq}: {frame_count} frames pixel-exact but 0 8x8-leaf hits"
                    ));
                    continue;
                }
                fired_any[di] = true;
                eprintln!(
                    "{name}: {depth}-bit cq {cq}, {frame_count} frames pixel-exact, 8x8 hits={hits}"
                );
            }
        }
        assert!(
            fired_any[0] && fired_any[1],
            "{name}: no attempt both decoded pixel-exact and fired the 8x8 leaf \
             (8-bit fired {}, 10-bit fired {}):\n{}",
            fired_any[0],
            fired_any[1],
            notes.join("\n")
        );
    }
