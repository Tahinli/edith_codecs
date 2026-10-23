    use super::*;
    use std::process::{Command, Stdio};

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

    fn test_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x + shift) % width;
                picture.y[y * width + x] = ((sx * 3 + y * 5) % 256) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * (width / 2) + x;
                picture.u[i] = ((100 + shift * 4) % 256) as u16;
                picture.v[i] = ((200 + 256 - shift * 2 % 256) % 256) as u16;
            }
        }
        picture
    }

    /// Drives a whole stream through the facade the way a caller must since
    /// it holds one picture of lookahead: every picture through
    /// [`Av1Encoder::encode_frames`], then [`Av1Encoder::flush`].
    fn encode_all(enc: &mut Av1Encoder, pictures: &[Picture]) -> Vec<Packet> {
        let mut packets = Vec::with_capacity(pictures.len());
        for (i, picture) in pictures.iter().enumerate() {
            packets.extend(
                enc.encode_frames(picture)
                    .unwrap_or_else(|e| panic!("picture {i}: {e}")),
            );
        }
        packets.extend(enc.flush().unwrap_or_else(|e| panic!("flush: {e}")));
        packets
    }

    /// [`gop_shape`] prices the frames the pyramid actually CODES: its hidden
    /// counts and leaf count match the packets a real run emits, at three
    /// pyramid shapes, and the plan [`RateLoop::new`] builds from it spends
    /// the run's budget exactly (`gop` frame targets, no more and no less).
    ///
    /// Both halves matter: a shape that misses a level (the old model priced
    /// ONE hidden frame per group when the mid level had made it two) splits
    /// the budget by weights that do not add up to the run.
    #[test]
    fn the_rate_loop_prices_the_frames_the_pyramid_codes() {
        let _knobs = crate::speed::knob_read();
        let pictures: Vec<Picture> = (0..48).map(|i| test_card(64, 64, i)).collect();
        let quarters = Pyramid {
            quarter_q_offset: Some(-20),
            ..Pyramid::default()
        };
        let two_level = Pyramid {
            mid_q_offset: None,
            ..Pyramid::default()
        };
        for (gop, pyramid) in [
            (48, Pyramid::default()),
            (12, Pyramid::default()),
            (48, quarters),
            (48, two_level),
        ] {
            let config = EncoderConfig {
                width: 64,
                height: 64,
                base_q_idx: 100,
                gop,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let rate = RateTarget::BytesPerFrame(2_000);
            let mut enc = Av1Encoder::with_pyramid_and_rate_target(config, pyramid, rate).unwrap();
            let packets = encode_all(&mut enc, &pictures[..gop]);
            let coded = |level: Level| packets.iter().filter(|p| p.level == level).count();
            let (hidden, leaves) = gop_shape(pyramid, gop);
            let hidden_frames: usize = hidden.iter().map(|&(_, n)| n).sum();
            assert_eq!(
                (hidden_frames, leaves, 1),
                (coded(Level::Arf), coded(Level::Leaf), coded(Level::Key)),
                "gop {gop} {pyramid:?}: shape {hidden:?} + {leaves} leaves"
            );
            assert_eq!(
                hidden_frames + leaves + 1,
                gop,
                "gop {gop}: every picture is coded exactly once"
            );
            // The plan adds up: one leaf's target times the run's whole
            // weight is the run's whole budget.
            let target_bytes = 2_000.0;
            let loop_ = RateLoop::new(target_bytes, 100.0, Some(pyramid), gop);
            let planned = loop_.target[0]
                + loop_.target[2]
                    * (leaves as f64
                        + hidden
                            .iter()
                            .map(|&(o, n)| n as f64 * loop_.weight(o))
                            .sum::<f64>());
            assert!(
                (planned - target_bytes * gop as f64).abs() < 1.0,
                "gop {gop}: plan {planned:.0} bytes against a {:.0} byte run",
                target_bytes * gop as f64
            );
        }
    }

    /// A level at its quantizer BOUND hands its unspent bytes to the levels
    /// that can still move ([[vorbis-rate-loop-windup]]'s class in the
    /// small), and it does so AT ONCE, not once the bound level's frames are
    /// all coded: the loop is driven by a synthetic per-level model where the
    /// first hidden frame is coded at `raw_q` 0 (past the coder's own 1..=255
    /// clamp) and spends half its plan, and the leaves' target must jump to
    /// the run's remainder with EVERY remaining hidden frame priced at that
    /// measured rate. The old division -- whole remainder over whole
    /// remaining weight -- handed most of the shortfall straight back to the
    /// hidden slot (which carries most of the pyramid's weight and cannot
    /// spend it), so the leaves only saw the surplus at the end of the run,
    /// when there was nothing left to code with it.
    #[test]
    fn a_bound_level_hands_its_unspent_bytes_to_the_others() {
        let pyramid = Pyramid::default();
        let (gop, target_bytes) = (48usize, 2_000.0);
        let (hidden, leaves) = gop_shape(pyramid, gop);
        let run_bytes = target_bytes * gop as f64;
        let mut rl = RateLoop::new(target_bytes, 100.0, Some(pyramid), gop);
        let scale0 = rl.target[2];
        let hidden_weight: f64 = hidden.iter().map(|&(o, n)| n as f64 * rl.weight(o)).sum();
        // The key codes exactly its plan; the first hidden frame is at the
        // floor and spends half of its.
        let key_bytes = rl.target[0].round();
        rl.update(Level::Key, 0, 100, key_bytes as usize);
        let (offset, n0) = hidden[0];
        let arf_bytes = (0.5 * scale0 * rl.weight(offset)).round();
        rl.update(Level::Arf, offset, 0, arf_bytes as usize);
        // Every frame of THAT LEVEL still to come is priced at what the bound
        // one measured -- and only that level: its slot-mates (the mid ARF)
        // sit at their own offset and are still free to follow the plan.
        // Everything else shares the rest.
        let want = (run_bytes - key_bytes - arf_bytes * n0 as f64)
            / (leaves as f64 + hidden_weight - n0 as f64 * rl.weight(offset));
        assert!(
            want < scale0 * RateLoop::REPLAN_CLAMP,
            "this arm must not reach the windup bound: {want:.1} vs {:.1}",
            scale0 * RateLoop::REPLAN_CLAMP
        );
        assert!(
            (rl.target[2] - want).abs() < 1e-6,
            "leaf target {:.3} is not the exact remainder {want:.3} (naive division would \
             give {:.3})",
            rl.target[2],
            (run_bytes - key_bytes - arf_bytes)
                / (leaves as f64 + hidden_weight - rl.weight(offset))
        );
        // And the re-plan stays BOUNDED: hidden levels that spend nothing at
        // all cannot run the leaves' target away.
        let mut rl = RateLoop::new(target_bytes, 100.0, Some(pyramid), gop);
        rl.update(Level::Key, 0, 100, key_bytes as usize);
        for &(offset, n) in &hidden {
            for _ in 0..n {
                rl.update(Level::Arf, offset, 0, 1);
            }
        }
        assert!(
            rl.target[2] <= scale0 * RateLoop::REPLAN_CLAMP + 1e-9,
            "the re-plan is unbounded: leaf target {:.1} past {:.1}",
            rl.target[2],
            scale0 * RateLoop::REPLAN_CLAMP
        );
        // ... and BOTH ways: a re-plan that reached zero would stop the loop
        // dead (`update` returns on a non-positive target).
        let mut rl = RateLoop::new(target_bytes, 100.0, Some(pyramid), gop);
        rl.update(Level::Key, 0, 100, key_bytes as usize);
        for &(offset, n) in &hidden {
            for _ in 0..n {
                rl.update(Level::Arf, offset, 0, 40 * key_bytes as usize);
            }
        }
        assert!(
            rl.target[2] >= scale0 * RateLoop::REPLAN_FLOOR - 1e-9,
            "the re-plan froze the loop: leaf target {:.1} under {:.1}",
            rl.target[2],
            scale0 * RateLoop::REPLAN_FLOOR
        );
    }

    /// The mini-GOP layout leaves no short tail group, and leaves the long
    /// GOP the fixed rule already coded alone: 47 inter pictures are
    /// 8 x 5 + 7 under both rules (so a 48-picture GOP is byte-identical),
    /// while a 12-picture GOP stops being 8 + 3 (lane-gopad).
    #[test]
    fn the_mini_gop_layout_leaves_no_short_tail() {
        let groups = |remaining: usize, mini_gop: usize, absorb: bool| {
            let (mut left, mut out) = (remaining, Vec::new());
            while left > 0 {
                let g = group_target(left, mini_gop, absorb).min(left);
                assert!(g > 0);
                out.push(g);
                left -= g;
            }
            out
        };
        // gop 48: what mini_gop 8 already coded, under either rule.
        assert_eq!(groups(47, 8, true), vec![8, 8, 8, 8, 8, 7]);
        assert_eq!(groups(47, 8, false), vec![8, 8, 8, 8, 8, 7]);
        // gop 12: 8 + 3 becomes one group of 11.
        assert_eq!(groups(11, 8, false), vec![8, 3]);
        assert_eq!(groups(11, 8, true), vec![11]);
        // A run shorter than a group is one group, as before (the 4-picture
        // pin clip).
        assert_eq!(groups(3, 8, true), vec![3]);
        // No group is ever below half the shape, and none is over 1.5x it.
        for mini_gop in 2..=16 {
            for remaining in 1..=64 {
                for g in groups(remaining, mini_gop, true) {
                    assert!(g * 2 <= 3 * mini_gop, "{remaining}/{mini_gop}: long {g}");
                    assert!(
                        g * 2 >= mini_gop || remaining < mini_gop,
                        "{remaining}/{mini_gop}: short tail {g}"
                    );
                }
            }
        }
    }

    /// Every OBU stream this test writes to `ffmpeg`/`ffprobe`, concatenated
    /// in order.
    fn concat(packets: &[Packet]) -> Vec<u8> {
        packets.iter().flat_map(|p| p.data.clone()).collect()
    }

    /// The facade's latency contract (lane-av1facade, deepened on
    /// lane-av1tpl2): the first [`crate::encode::tpl_depth`] - 1 calls hold
    /// their picture back and return an empty packet, every later call
    /// returns the packet of the picture that many frames earlier, and
    /// [`Av1Encoder::flush`] yields the whole queue — so a caller still gets
    /// exactly one non-empty packet per picture, `depth - 1` calls later.
    #[test]
    fn one_in_one_out_delayed_by_the_lookahead_depth() {
        let _knobs = crate::speed::knob_read();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 2,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let delay = crate::encode::tpl_depth() as u64 - 1;
        let pictures = delay + 5;
        let mut enc = Av1Encoder::new(config).unwrap();
        for t in 0..pictures {
            let packet = enc.encode(&test_card(64, 64, t as usize)).unwrap();
            if t < delay {
                assert!(packet.data.is_empty(), "call {t} holds its picture back");
                continue;
            }
            assert!(!packet.data.is_empty(), "picture {t}: empty packet");
            assert_eq!(packet.order, t - delay, "picture {t}: order");
        }
        let tail = enc.flush().unwrap();
        assert_eq!(tail.len() as u64, delay, "flush yields the whole queue");
        for (i, p) in tail.iter().enumerate() {
            assert_eq!(
                p.order,
                pictures - delay + i as u64,
                "flushed packet {i}: order"
            );
            assert!(!p.data.is_empty(), "flushed packet {i}: empty");
        }
        assert!(enc.flush().unwrap().is_empty(), "nothing is held twice");
    }

    /// A screen-like card: hard-edged 4-pixel bands of two flat luma values
    /// with a moving block over the lower half — few colours per 16x16 block
    /// at high variance, which is exactly what
    /// [`crate::encode::picture_is_screen`] separates. Used unforced, so the
    /// gate below runs the real detector rather than an override.
    fn screen_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let band = if (x / 4).is_multiple_of(2) { 40 } else { 200 };
                let over = y >= height / 2 && (x + shift) % width < width / 3;
                picture.y[y * width + x] = if over { 128 } else { band };
            }
        }
        picture
    }

    /// THE ENTRY-SURFACE GATE (lane-av1facade, extended to the pyramid on
    /// lane-av1pyrdef): the streaming facade — what the editor's export
    /// drives — codes byte for byte what [`crate::encode::encode_sequence`]
    /// codes, which is the path the BD gate measures. Both now drive ONE
    /// mini-GOP driver ([`Av1Encoder::encode_sequence_pyramid`]), so the
    /// identity is by construction; this pins it anyway, over both content
    /// classes the content gate splits:
    ///
    ///   * film-like (the gate's own clip when the fixture is there, a
    ///     multi-tone moving card otherwise): the pyramid is EFFECTIVE, the
    ///     stream is reordered (a hidden `ALTREF` per mini-GOP), and the two
    ///     paths' whole streams are identical;
    ///   * screen-like: the content gate drops BOTH paths back to flat, one
    ///     packet per picture, identical frame by frame as well as whole.
    ///
    /// Never skipped, since a skipped identity gate is the defect it is meant
    /// to catch.
    #[test]
    fn the_facade_codes_the_same_bytes_as_encode_sequence() {
        let _knobs = crate::speed::knob_read();
        let frames = 12usize;
        let film = h264_clip_frames("the facade identity film arm", 640, 384, frames)
            .unwrap_or_else(|| {
                // The probe inside already printed the ONE line naming the
                // resolved path and the escape hatch, so this is a note about
                // what runs instead — not a second SKIP.
                eprintln!("  ...so the film arm runs on a synthetic card, not real film");
                (0..frames).map(|t| test_card(64, 64, t)).collect()
            });
        let screen: Vec<Picture> = (0..frames).map(|t| screen_card(64, 64, t)).collect();
        let pyramid = Pyramid::from_env().expect("the pyramid is this build's default");
        for (content, pictures, is_screen) in [("film", &film, false), ("screen", &screen, true)] {
            let (width, height) = (pictures[0].width, pictures[0].height);
            for q in [150u8, 60] {
                let config = EncoderConfig {
                    width,
                    height,
                    base_q_idx: q,
                    // One key frame then all inter, which is
                    // `encode_sequence`'s only shape, and the gate's recipe.
                    gop: frames,
                    // `encode_sequence` writes an unspecified colour config.
                    colour: Colour::Unspecified,
                    tile_cols_log2: 0,
                    tile_rows_log2: 0,
                };
                let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
                let mut packets = Vec::new();
                for picture in pictures {
                    packets.extend(enc.encode_frames(picture).unwrap());
                }
                packets.extend(enc.flush().unwrap());
                assert_eq!(
                    enc.pyramid().is_none(),
                    is_screen,
                    "{content} q={q}: the content gate read this clip the other way round"
                );
                let sequence = crate::encode::encode_sequence(pictures, q, DEADZONE).unwrap();
                assert_eq!(
                    (
                        packets.iter().map(|p| p.data.len()).sum::<usize>(),
                        concat(&packets)
                    ),
                    (sequence.stream.len(), sequence.stream.clone()),
                    "{content} q={q}: the facade's stream is not the sequence path's"
                );
                assert_eq!(
                    sequence.frames.len(),
                    frames,
                    "{content} q={q}: one entry per picture, in display order"
                );
                if is_screen {
                    assert_eq!(
                        packets.len(),
                        frames,
                        "{content} q={q}: one packet per picture"
                    );
                    for (i, (packet, coded)) in packets.iter().zip(&sequence.frames).enumerate() {
                        let cropped = crate::encode::crop_encoded(coded, width, height);
                        assert_eq!(
                            (packet.data.len(), packet.data.clone()),
                            (cropped.stream.len(), cropped.stream.clone()),
                            "{content} q={q} frame {i}: the facade's bytes are not the \
                             sequence path's"
                        );
                    }
                } else {
                    assert!(
                        packets.iter().any(|p| p.level == Level::Arf),
                        "{content} q={q}: a non-screen stream coded no hidden frame"
                    );
                }
            }
        }
    }

    /// THE CONTENT GATE (lane-av1pyrgate): the coding pyramid is asked for by
    /// the caller but applied only to camera material. Over screen content
    /// the encoder must drop back to the flat path and code byte for byte
    /// what a flat encoder codes — that is what keeps the BD gate's screen
    /// row identical to the flat baseline — and over non-screen content the
    /// same request must still reorder (class `gate-blind-to-feature`: a gate
    /// that only checked the screen half would pass with the pyramid deleted).
    #[test]
    fn the_content_gate_keeps_screen_streams_flat() {
        let _knobs = crate::speed::knob_read();
        let frames = 8usize;
        let pictures: Vec<Picture> = (0..frames).map(|t| test_card(64, 64, t)).collect();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 120,
            gop: frames,
            colour: Colour::Unspecified,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        crate::encode::force_screen(Some(true));
        let mut flat = Av1Encoder::new(config).unwrap();
        let flat_packets = encode_all(&mut flat, &pictures);
        let mut gated = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
        let gated_packets = encode_all(&mut gated, &pictures);
        assert_eq!(
            gated.pyramid(),
            None,
            "the gate left a screen stream on the pyramid"
        );
        assert_eq!(
            concat(&gated_packets),
            concat(&flat_packets),
            "a screen stream's bytes are not the flat path's"
        );
        crate::encode::force_screen(Some(false));
        let mut kept = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
        let kept_packets = encode_all(&mut kept, &pictures);
        assert_eq!(
            kept.pyramid(),
            Some(Pyramid::default()),
            "the gate ate a film stream"
        );
        assert!(
            kept_packets.iter().any(|p| p.level == Level::Arf),
            "a non-screen stream coded no hidden frame"
        );
        crate::encode::force_screen(None);
    }

    /// A key frame every `gop` pictures, inter otherwise — checked against
    /// what the facade itself reports, and (below) against what `ffprobe`
    /// reads back out of the coded bytes.
    #[test]
    fn gop_cadence_is_honored() {
        let _knobs = crate::speed::knob_read();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 3,
            colour: Colour::Unspecified,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let pictures: Vec<Picture> = (0..7).map(|t| test_card(64, 64, t)).collect();
        let keys: Vec<bool> = encode_all(&mut enc, &pictures)
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, vec![true, false, false, true, false, false, true]);
    }

    fn ffprobe_frames(stream: &[u8]) -> Vec<(bool, u32, u32)> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = crate::tool_cache::cached_output_io(Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "frame=key_frame,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path), None, |c| c.output())
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| {
                let mut f = line.trim().split(',');
                let key: u32 = f.next().unwrap().parse().unwrap();
                let width: u32 = f.next().unwrap().parse().unwrap();
                let height: u32 = f.next().unwrap().parse().unwrap();
                (key == 1, width, height)
            })
            .collect()
    }

    fn ffprobe_colour(stream: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-colour-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = crate::tool_cache::cached_output_io(Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_primaries,color_transfer,color_space,color_range",
                "-of",
                "csv=p=0",
            ])
            .arg(&path), None, |c| c.output())
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A 30-picture stream at gop 15 codes 2 key frames (pictures 0 and 15)
    /// and 28 inter ones, at an odd-ish (not superblock-multiple) size that
    /// exercises the pad/crop path -- `ffprobe`'s own `key_frame` flags and
    /// coded size confirm what the facade already reported.
    #[test]
    fn thirty_pictures_at_gop_fifteen_decode_to_two_key_frames() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP thirty_pictures_at_gop_fifteen_decode_to_two_key_frames: no ffmpeg");
            return;
        }
        let (width, height) = (96usize, 96usize);
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 15,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        assert_eq!(enc.display_size(), (width, height));
        let (coded_w, coded_h) = enc.coded_size();
        assert_eq!(
            (coded_w, coded_h),
            (
                width.next_multiple_of(SUPERBLOCK),
                height.next_multiple_of(SUPERBLOCK)
            )
        );

        let pictures: Vec<Picture> = (0..30).map(|t| test_card(width, height, t)).collect();
        let packets: Vec<Packet> = encode_all(&mut enc, &pictures);
        let expect_key: Vec<bool> = (0..30).map(|t| t % 15 == 0).collect();
        assert_eq!(
            packets.iter().map(|p| p.key).collect::<Vec<_>>(),
            expect_key,
            "facade's own key/inter flags"
        );

        let stream = concat(&packets);
        let frames = ffprobe_frames(&stream);
        assert_eq!(frames.len(), 30, "ffprobe frame count");
        for (t, (key, w, h)) in frames.iter().enumerate() {
            assert_eq!(*key, expect_key[t], "picture {t}: ffprobe key_frame flag");
            assert_eq!(
                (*w, *h),
                (width as u32, height as u32),
                "picture {t}: ffprobe reports the true (display) size, not the padded coded one"
            );
        }
    }

    /// The configured colour properties are what `ffprobe` reports back,
    /// spec 5.5.2's CICP triple plus range -- BT.709 limited here, since it
    /// is the default a player assumes and the one this test would silently
    /// pass without wiring for (`ffprobe` reports "unknown" for the
    /// hardcoded "unspecified" this crate wrote before the facade existed,
    /// which is a visibly different string).
    #[test]
    fn bt709_limited_colour_is_reported_by_ffprobe() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP bt709_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packets = encode_all(&mut enc, &[test_card(64, 64, 0)]);
        let colour = ffprobe_colour(&packets[0].data);
        assert_eq!(colour, "tv,bt709,bt709,bt709", "ffprobe colour fields");
    }

    /// The same, for BT.601: a different set of CICP integers must produce a
    /// different string, or the wiring could be a no-op that happens to read
    /// as BT.709 for every input.
    #[test]
    fn bt601_limited_colour_is_reported_by_ffprobe() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP bt601_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt601Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packets = encode_all(&mut enc, &[test_card(64, 64, 0)]);
        let colour = ffprobe_colour(&packets[0].data);
        assert_eq!(
            colour, "tv,smpte170m,smpte170m,smpte170m",
            "ffprobe colour fields"
        );
    }

    /// A frame whose size does not match the encoder's configured geometry
    /// is refused by name.
    #[test]
    fn geometry_mismatch_is_refused() {
        let _knobs = crate::speed::knob_read();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let err = enc.encode(&Picture::grey(32, 32)).unwrap_err();
        assert!(err.to_string().contains("64x64"), "{err}");
    }

    /// `gop == 0` is refused at construction, not the first `encode()` call.
    #[test]
    fn zero_gop_is_refused() {
        let _knobs = crate::speed::knob_read();
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 0,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let err = Av1Encoder::new(config).unwrap_err();
        assert!(err.to_string().contains("gop"), "{err}");
    }

    /// The 12-frame real clip `real_clip_encodes_within_its_quality_and_size_budget`
    /// already gates on at `q=100` (`crates/ec-av1/src/encode.rs`), decoded down
    /// to this facade's own input size, and re-coded through the facade so the
    /// rate-target surface is exercised at the entry point a caller actually
    /// drives.
    ///
    /// lane-av1clipprobe: the ABSENCE of this clip used to be a bare
    /// `clip.exists()` -> `None`, and every caller turned that into a `SKIP`
    /// line and a return -- so a tree with no root `fixtures/` printed
    /// `SKIP the 1080p tile round trip: no fixture` and libtest reported `ok`:
    /// green with nothing asserted, and no env that made it RED. The presence
    /// check now lives in the crate's one library-fixture probe
    /// (`library_fixture::require_at`), which fails under `EC_REQUIRE_FIXTURES` /
    /// `EC_AV1_REQUIRE_FFMPEG` / `EC_AV1_REQUIRE_AOMENC` and otherwise prints
    /// that ONE line.
    fn h264_clip_frames(
        what: &str,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Option<Vec<Picture>> {
        if !have_ffmpeg() {
            eprintln!("SKIP {what}: no ffmpeg");
            return None;
        }
        // lane-av1speed3: the wall tables below are read on REAL film as
        // well as on the colour-bar fixture (`bars` vs `film` in the BD
        // gate's own vocabulary), and the film crop lives outside the repo --
        // `EC_AV1_WALL_CLIP` names it. Unset everywhere else, so every other
        // caller still gets the fixture.
        //
        // lane-av1clipprobe: the default path resolves through `EC_FIXTURES`
        // first, which is the root `scripts/verify-fixture-library.sh`
        // validates -- so the preflight's verdict and these bytes are the same
        // tree, and GREEN-with-an-absent-root is no longer reachable by
        // pointing EC_FIXTURES somewhere the gates do not read.
        let clip = match std::env::var("EC_AV1_WALL_CLIP") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => crate::library_fixture::path("video/h264-1080p-23.976-8bit.mp4"),
        };
        // `what` is the CALLING GATE's name, carried in so that both exits from
        // this function print exactly one line naming it: the ffmpeg line
        // above, and the probe's line naming the resolved path. Callers
        // therefore `return` on `None` WITHOUT printing a second one -- the
        // second "no fixture" line per gate was the noise that hid this defect.
        let clip = crate::library_fixture::require_at(clip, what)?;
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-i", clip.to_str().unwrap()])
            .args(["-frames:v", &frames.to_string()])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_len * frames,
            "expected {frames} 4:2:0 frames"
        );
        Some(
            (0..frames)
                .map(|i| {
                    let bytes = &out.stdout[i * frame_len..][..frame_len];
                    Picture {
                        width,
                        height,
                        y: bytes[..luma].iter().map(|&v| u16::from(v)).collect(),
                        u: bytes[luma..luma + chroma]
                            .iter()
                            .map(|&v| u16::from(v))
                            .collect(),
                        v: bytes[luma + chroma..]
                            .iter()
                            .map(|&v| u16::from(v))
                            .collect(),
                    }
                })
                .collect(),
        )
    }

    /// [`RateTarget::BytesPerFrame`] on a real, 24-frame clip lands its total
    /// coded size within ±20% of `frames * target` once the first few
    /// frames' settling has been discarded (the key frame's own reference
    /// state and the controller's first couple of steps).
    #[test]
    fn bytes_per_frame_target_settles_within_20_percent() {
        let _knobs = crate::speed::knob_read();
        let Some(pictures) = h264_clip_frames(
            "bytes_per_frame_target_settles_within_20_percent",
            640,
            384,
            24,
        ) else {
            return;
        };
        let target_bytes = 4_000u32;
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(target_bytes)).unwrap();
        let sizes: Vec<usize> = encode_all(&mut enc, &pictures)
            .iter()
            .map(|p| p.data.len())
            .collect();
        // Discard the key frame (always far larger than an inter target) and
        // the next 4 inter frames the loop needs to step toward it.
        let settled = &sizes[5..];
        let mean = settled.iter().sum::<usize>() as f64 / settled.len() as f64;
        let low = f64::from(target_bytes) * 0.8;
        let high = f64::from(target_bytes) * 1.2;
        assert!(
            mean >= low && mean <= high,
            "settled mean {mean:.0} bytes/frame outside ±20% of {target_bytes} ({low:.0}..{high:.0}); sizes={sizes:?}"
        );
    }

    /// One (bitrate, pyramid) arm's tally, built on the worker thread and
    /// handed back through the progress channel
    /// (`bitrate_target_lands_within_5_percent_over_48_frames`). Per LEVEL,
    /// so a miss reads as WHICH level's model is off rather than as one
    /// aggregate percentage.
    struct ArmResult {
        /// [`RateLoop::target`] as the arm's own encoder computed it.
        targets: [f64; 3],
        coded: usize,
        bytes: [usize; 4],
        count: [usize; 4],
    }

    impl ArmResult {
        fn take(&mut self, packets: Vec<Packet>) {
            for packet in packets {
                self.coded += packet.data.len();
                let i = match packet.level {
                    Level::Key => 0,
                    Level::Arf => 1,
                    Level::Leaf => 2,
                    Level::ShowExisting => 3,
                };
                self.bytes[i] += packet.data.len();
                self.count[i] += 1;
            }
        }
    }

    /// [`RateTarget::Bitrate`] lands the achieved bitrate within ±5% of the
    /// target over 48 frames of a real clip, flat and under a pyramid alike,
    /// across a 5x span of bitrates. This is the accuracy claim of the
    /// target-bitrate mode: no two-pass, no lookahead, just each frame's size
    /// predicted from the previous frame of its own level, one clamped
    /// proportional step, and the level model
    /// ([`WEIGHT_LEVEL_GAIN`]) that says how the run's budget splits.
    ///
    /// The tolerance was ±10% and every arm was inside ±3.0% when it was cut
    /// to ±5%; the worst arm is the 2 Mbit/s pyramid at -4.4%, which is the
    /// ENCODER'S OWN CEILING showing through, not the loop's aim: at that
    /// rate the hidden slot already codes at `q_idx` 1 (its 22863 bytes a
    /// frame are the same at 1.5 and 2 Mbit/s) and the leaves sit at 1 + 12
    /// `leaf_q_offset`, so this clip cannot be made to spend much past
    /// 2.4 Mbit/s under the default pyramid however the budget is split.
    ///
    /// # Bounded
    ///
    /// Each arm's encode runs on a worker thread under a per-CALL
    /// wall-clock ceiling, so this gate FAILS rather than hanging when an
    /// encode stops terminating (lane-av1enchang: a 3h20m wedge on one host
    /// and 30min on another, both here, both with the test thread spinning
    /// and no child process for the crate's child timeout to reap). The cost
    /// is real and is why the budget is not tight: the two high bitrate arms
    /// walk `q_idx` from the seed 100 down into the 20s to spend an
    /// 8000-byte budget on a 640x384 clip, and a frame at `q_idx` 25 costs
    /// ~15s in a debug build (~1s optimized) against ~0.5s at `q_idx` 100.
    /// lane-av1enchang: the PYRAMID CONSTRUCTORS fit on a tight stack.
    ///
    /// `Av1Encoder` used to be 139960 bytes BY VALUE, because each of the
    /// eight `DpbSlot`s held a 15 KB `CdfSnapshot` inline. The pyramid
    /// constructors nest three deep -- `with_pyramid_and_rate_target` ->
    /// `with_pyramid` -> `new` -- and each level materialises the struct in
    /// an unoptimized build, so ~840 KB of stack was live at the innermost
    /// `Ok(Self { .. })`. On libtest's default 2 MiB test thread that aborts:
    /// `fatal runtime error: stack overflow, aborting`, reproduced on the
    /// pre-fix binary at `crates/ec-av1/src/encoder.rs:1519` via
    /// `coredumpctl debug`.
    ///
    /// Boxing the snapshot brought the struct to 18168 bytes. This gate pins
    /// that: it builds BOTH constructors on a thread with a deliberately
    /// tight stack, so the old layout ABORTS THE PROCESS and the new one
    /// returns. It runs in milliseconds, where proving the same thing through
    /// the 48-frame gate takes 6m35s.
    ///
    /// Both thresholds were swept, in an unoptimized build:
    ///
    /// | stack | pre-fix (`CdfSnapshot` by value) | post-fix (boxed) |
    /// |---|---|---|
    /// | 256 KiB | overflow | overflow |
    /// | 384 KiB | overflow | ok |
    /// | 512 KiB | overflow | ok |
    /// | 1 MiB   | **overflow** | **ok** |
    /// | 2 MiB   | **overflow** (libtest's default) | ok |
    /// | 3 MiB   | ok | ok |
    ///
    /// 1 MiB is the gate: 2.7x what the boxed layout needs and 2x below what
    /// the old one did, so it is nowhere near either threshold and cannot
    /// flake. The pre-fix row at 1 MiB is the red -- which is also why
    /// `.cargo/config.toml` carries `RUST_MIN_STACK = 67108864`, a
    /// workspace-local band-aid this gate makes unnecessary for this struct.
    ///
    /// `EC_AV1_TIGHT_STACK_BYTES` overrides the size, which is how the sweep
    /// above is reproduced.
    #[test]
    fn the_pyramid_constructors_fit_on_a_tight_stack() {
        // 1 MiB: see the threshold table on this test. Red before the Box,
        // green after, with 2.7x margin either way.
        const STACK: usize = 1024 * 1024;
        let stack: usize = std::env::var("EC_AV1_TIGHT_STACK_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(STACK);
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 48,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let rate = RateTarget::Bitrate {
            bits_per_second: 1_536_000,
            frames_per_second: 24.0,
        };
        let handle = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                let flat = Av1Encoder::with_rate_target(config, rate)
                    .expect("with_rate_target")
                    .rate_loop
                    .is_some();
                let pyr =
                    Av1Encoder::with_pyramid_and_rate_target(config, Pyramid::default(), rate)
                        .expect("with_pyramid_and_rate_target")
                        .pyramid
                        .is_some();
                (flat, pyr)
            })
            .expect("spawn tight-stack thread");
        let (flat, pyr) = handle
            .join()
            .unwrap_or_else(|_| panic!("the encoder constructors overflowed a {stack}-byte stack"));
        assert!(flat, "with_rate_target lost its rate loop");
        assert!(pyr, "with_pyramid_and_rate_target lost its pyramid");
        eprintln!(
            "TIGHTSTACK {stack} bytes: Av1Encoder={} bytes, both constructors returned",
            std::mem::size_of::<Av1Encoder>()
        );
    }

    #[test]
    fn bitrate_target_lands_within_5_percent_over_48_frames() {
        let _knobs = crate::speed::knob_read();
        let (width, height, frames) = (640usize, 384usize, 48usize);
        let Some(pictures) = h264_clip_frames(
            "bitrate_target_lands_within_5_percent_over_48_frames",
            width,
            height,
            frames,
        ) else {
            return;
        };
        let fps = 24.0;
        let pictures = std::sync::Arc::new(pictures);
        for bits_per_second in [384_000u32, 768_000, 1_536_000, 2_000_000] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 100,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let rate = RateTarget::Bitrate {
                bits_per_second,
                frames_per_second: fps,
            };
            for pyramid in [None, Some(Pyramid::default())] {
                let arm = format!("{bits_per_second} bps pyramid {}", pyramid.is_some());
                // lane-av1enchang: a BACKSTOP, not a fix. This gate is NOT
                // hung and NOT regressed -- it is a ~7 hour test in an
                // unoptimized build (measured: the whole `ec-av1` lib suite
                // takes 24341s, and this test is the largest single item in
                // it), and it PASSES. What it lacked was any way to FAIL: a
                // future non-terminating encode would wedge the suite for
                // hours with no diagnostic, which is exactly how wave3j and
                // wave3h were lost. So the arm runs on a worker thread and
                // THIS thread watches a per-CALL deadline, purely so that
                // case reports itself.
                //
                // The budget is PER CALL, not per test, and it is valued from
                // a measured run so it cannot turn a correct-but-slow build
                // red. Measured on this gate's own workload (48 frames of
                // 640x384): one `encode_frames` call is 0.78s..1.04s
                // optimized and 8.6s..12.2s debug on the flat arms. A
                // PYRAMID call codes a whole mini-GOP at once, so its worst
                // single call is `Pyramid::default().mini_gop` frames
                // (~9), i.e. ~110s debug. 1800s is therefore ~16x that
                // worst honest call: no plausible host trips it, and a real
                // non-terminating encode is reported in 30 minutes instead
                // of never. `EC_AV1_ENCODE_CALL_BUDGET_SECS` overrides it,
                // which is how the backstop is proven to bite.
                let budget = std::time::Duration::from_secs(
                    std::env::var("EC_AV1_ENCODE_CALL_BUDGET_SECS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1800),
                );
                let total = pictures.len() + 1;
                // Shared, not cloned: the 48 decoded frames are ~35 MB of
                // `u16` luma+chroma, and all eight arms want the same ones.
                let pictures = std::sync::Arc::clone(&pictures);
                // `None` is a progress ticket: THIS call returned. `Some` is
                // the arm's result, sent after `flush` -- which is itself a
                // call that can spin, so it needs a ticket too, not a bare
                // `join` that would block forever.
                let (tx, rx) = std::sync::mpsc::channel::<Option<ArmResult>>();
                let worker = std::thread::spawn(move || {
                    let mut enc = match pyramid {
                        None => Av1Encoder::with_rate_target(config, rate).unwrap(),
                        Some(p) => {
                            Av1Encoder::with_pyramid_and_rate_target(config, p, rate).unwrap()
                        }
                    };
                    let targets = enc.rate_loop.as_ref().unwrap().target;
                    let mut out = ArmResult {
                        targets,
                        coded: 0,
                        bytes: [0; 4],
                        count: [0; 4],
                    };
                    for picture in pictures.iter() {
                        out.take(enc.encode_frames(picture).unwrap());
                        if tx.send(None).is_err() {
                            return;
                        }
                    }
                    out.take(enc.flush().unwrap());
                    let _ = tx.send(Some(out));
                });
                let started = std::time::Instant::now();
                let mut calls = 0usize;
                let out = loop {
                    match rx.recv_timeout(budget) {
                        // The result IS the last thing the worker sends, so
                        // receiving it means every call returned. A worker
                        // that panicked sends nothing and arrives as
                        // `Disconnected` below, which re-joins it and
                        // surfaces the real panic.
                        Ok(Some(out)) => break out,
                        Ok(None) => calls += 1,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                            "AV1 bitrate gate SLOW, NOT HUNG: the {arm} arm's encode did not \
                             return within {}s (arm elapsed {:.0}s, stuck on call {} of {total} \
                             -- the call that never came back). This gate is a ~7 hour test in \
                             an unoptimized build and it PASSES, so a red here is NOT a slow \
                             host: the budget is per CALL and the worst honest call measured \
                             is ~1s optimized / ~110s debug on a pyramid arm, so reaching this \
                             panic means an encode call that is NOT RETURNING. \
                             EC_AV1_ENCODE_CALL_BUDGET_SECS raises the budget.",
                            budget.as_secs(),
                            started.elapsed().as_secs_f64(),
                            calls + 1,
                        ),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            worker.join().expect("encode worker panicked");
                            panic!("AV1 encode worker for {arm} died without a result");
                        }
                    }
                };
                let ArmResult {
                    targets,
                    coded,
                    bytes,
                    count,
                } = out;
                let seconds = frames as f64 / fps;
                let achieved = coded as f64 * 8.0 / seconds;
                let error = achieved / f64::from(bits_per_second) - 1.0;
                let per = |i: usize| match count[i] {
                    0 => "-".to_string(),
                    n => format!("{n}x{}", bytes[i] / n),
                };
                eprintln!(
                    "bitrate {bits_per_second} bps, pyramid {}: {coded} bytes over {seconds:.2}s \
                     = {achieved:.0} bps ({:+.1}%) | targets {:.0}/{:.0}/{:.0} \
                     coded key {} arf {} leaf {}",
                    pyramid.is_some(),
                    100.0 * error,
                    targets[0],
                    targets[1],
                    targets[2],
                    per(0),
                    per(1),
                    per(2),
                );
                assert!(
                    error.abs() <= 0.05,
                    "achieved {achieved:.0} bps is {:+.1}% off the {bits_per_second} bps target \
                     (pyramid {})",
                    100.0 * error,
                    pyramid.is_some(),
                );
            }
        }
    }
    /// The `BytesPerFrame` controller's own windup bound: no frame's coded
    /// `base_q_idx` step exceeds [`RateLoop::STEP_CLAMP`], across a real
    /// clip's full range of content (so a scene cut can't be the one frame
    /// that breaks the bound).
    ///
    /// The SEEDING frame is the one exemption ([`RateLoop::update`]): a slot
    /// that has coded nothing yet is still sitting on the caller's
    /// `base_q_idx` guess, and its first step is the acquisition jump, not
    /// windup. Exactly one frame is exempt -- the loop below asserts the
    /// bound on every frame after it, which is where a scene cut would
    /// break it.
    #[test]
    fn bytes_per_frame_controller_never_oscillates_past_its_clamp() {
        let _knobs = crate::speed::knob_read();
        let Some(pictures) = h264_clip_frames(
            "bytes_per_frame_controller_never_oscillates_past_its_clamp",
            640,
            384,
            24,
        ) else {
            return;
        };
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(4_000)).unwrap();
        let mut prev_q = enc.rate_loop.as_ref().unwrap().q_idx(Level::Leaf);
        // The first frame this encoder EMITS is several `encode_frames` calls
        // in (the lookahead queue holds the earlier ones), so the seeding
        // step is not frame zero -- it is the first frame `q` moves at all.
        let mut seeded = false;
        let mut seed_frame = None;
        for (frame, picture) in pictures.iter().enumerate() {
            enc.encode_frames(picture).unwrap();
            let q = enc.rate_loop.as_ref().unwrap().q_idx(Level::Leaf);
            let step = (i32::from(q) - i32::from(prev_q)).abs();
            if step > 0 && !seeded {
                seeded = true;
                seed_frame = Some(frame);
            } else {
                assert!(
                    f64::from(step) <= RateLoop::STEP_CLAMP + 1.0, // +1 for u8 rounding
                    "frame {frame}: q stepped from {prev_q} to {q}, past the {} clamp",
                    RateLoop::STEP_CLAMP
                );
            }
            prev_q = q;
        }
        assert!(
            seed_frame.is_some(),
            "the controller never steered at all -- the bound below is vacuous"
        );
    }

    fn psnr(a: &[u16], b: &[u16]) -> f64 {
        let squared: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum();
        if squared == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0 * 255.0 * a.len() as f64 / squared).log10()
    }

    /// Decodes an OBU stream back to raw luma planes via `ffmpeg`/dav1d, one
    /// entry per coded frame, in order — this facade's own equivalent of
    /// `encode::tests::ffmpeg_decode_sequence`, needed here too since a
    /// PSNR check needs the decoded pixels, not just the coded byte count.
    fn ffmpeg_decode_planes(
        stream: &[u8],
        width: usize,
        height: usize,
    ) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-rate-decode-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the decode probe stream");
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-f", "obu", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]), None, |c| c.output())
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        out.stdout
            .chunks_exact(frame_len)
            .map(|f| {
                (
                    f[..luma].to_vec(),
                    f[luma..luma + chroma].to_vec(),
                    f[luma + chroma..].to_vec(),
                )
            })
            .collect()
    }

    /// [`ffmpeg_decode_planes`]' luma half, which is what most rows compare.
    fn ffmpeg_decode_luma(stream: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
        ffmpeg_decode_planes(stream, width, height)
            .into_iter()
            .map(|(y, _, _)| y)
            .collect()
    }

    /// lane-palw: OUR encoder's own witness for the writer half of the
    /// palette on-screen defect. `screen_card` at 384x152 is 38 mi rows tall,
    /// so the bottom superblock row's palette blocks hang 8 luma rows off the
    /// mi grid: spec 5.11.50 codes their colour-index map over the ON-SCREEN
    /// corner only and replicates it outward, and the writer used to code the
    /// full block grid -- symbols a conformant decoder never reads, so
    /// ffmpeg/dav1d desynced the rest of the tile while our own trial decode
    /// (unclamped on both sides) agreed with itself. Class
    /// `writer-phantom-units-past-edge`.
    ///
    /// Driven through the ENTRY SURFACE, `Av1Encoder` (what the editor's
    /// export calls), and compared on EVERY plane -- a chroma palette map is
    /// cut by the same edge. `palette_cut_maps_written` is asserted non-zero
    /// so the row cannot pass on a stream that cuts no palette map (class
    /// gate-blind-to-feature).
    #[test]
    fn a_palette_block_cut_by_the_frame_edge_decodes_exactly_through_ffmpeg() {
        const NAME: &str = "a_palette_block_cut_by_the_frame_edge_decodes_exactly_through_ffmpeg";
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (384usize, 152usize);
        // A 16-px two-tone checkerboard: every 16x16 and 32x32 block holds at
        // most two luma values, so the palette arm wins across the WHOLE
        // frame -- including the bottom superblock row, whose 16x16 blocks at
        // y=144 and 32x32 blocks at y=128 hang past the 152-row mi grid.
        // `screen_card`'s 4-px bands fire only 9 palettes here and none of
        // them at the cut edge (class gate-blind-to-feature).
        let card = |shift: usize| -> Picture {
            let mut picture = Picture::grey(width, height);
            for y in 0..height {
                for x in 0..width {
                    let cell = ((x + shift) / 8 + y / 8) % 2;
                    picture.y[y * width + x] = if cell == 0 { 40 } else { 200 };
                }
            }
            picture
        };
        let sources: Vec<Picture> = (0..2).map(|t| card(t * 16)).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 60,
            gop: 2,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        crate::tile::reset_palette_cut_maps_written();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        eprintln!(
            "{NAME}: {} cut colour-index maps written",
            crate::tile::palette_cut_maps_written()
        );
        assert!(
            crate::tile::palette_cut_maps_written() > 0,
            "{NAME}: no palette block is cut by the frame edge here -- the gate would pass blind"
        );
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "{NAME}: our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half of {NAME}: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_planes(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "{NAME}: ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            for (plane, got, want) in [("luma", &a.y, &b.0), ("U", &a.u, &b.1), ("V", &a.v, &b.2)] {
                let got: Vec<u8> = got.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(want).position(|(x, y)| x != y) {
                    let w = if plane == "luma" { width } else { width / 2 };
                    panic!(
                        "{NAME}: frame {i} {plane} differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % w,
                        at / w,
                        got[at],
                        want[at],
                    );
                }
                assert_eq!(
                    got.len(),
                    want.len(),
                    "{NAME}: frame {i} {plane} is {} samples, ffmpeg's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
    }

    /// A pyramid stream reorders pictures: the hidden `ALTREF` of each
    /// mini-GOP is coded before the leaves that precede it in display order,
    /// and a `show_existing_frame` header puts it back. This pins the shape
    /// (one packet per source picture plus one `show_existing_frame` per
    /// group, `dts` monotone, `order` NOT monotone) and then decodes the
    /// whole stream through OUR decoder and through ffmpeg/dav1d and requires
    /// the two display-order outputs to agree sample for sample — the
    /// [[gate-blind-to-hidden-frames]] check: a hidden frame that is never
    /// output, or output twice, or output in the wrong position, changes the
    /// display-order list and fails here.
    #[test]
    fn a_pyramid_stream_decodes_in_display_order_through_both_decoders() {
        // `take_ref_frame_hits` is a process-global counter: a reader-locked
        // sibling encoding at the same time zeroes it between our encode and
        // our read (seen once: LAST 0 GOLDEN 0 ALTREF 0), so this takes the
        // exclusive lock.
        let _knobs = crate::speed::knob_write();
        pyramid_round_trip(
            Pyramid {
                mini_gop: 4,
                mid_q_offset: None,
                ..Pyramid::default()
            },
            2,
        );
    }

    /// The same round trip over a THREE-level group (`mid_q_offset`): each
    /// mini-GOP now codes two hidden frames -- the top ARF at the group's
    /// last picture and the mid ARF at the middle of the leaf run -- and
    /// re-outputs both with `show_existing_frame`, so a mid frame emitted in
    /// the wrong display position, or twice, fails the display-order compare
    /// against ffmpeg exactly as a top ARF does.
    #[test]
    fn a_three_level_pyramid_stream_decodes_in_display_order() {
        let _knobs = crate::speed::knob_write();
        pyramid_round_trip(
            Pyramid {
                mini_gop: 8,
                mid_q_offset: Some(-8),
                ..Pyramid::default()
            },
            2,
        );
    }

    /// The same round trip over a FOUR-level group
    /// ([`Pyramid::quarter_q_offset`]): a mini-GOP of eight now codes four
    /// hidden frames -- the top ARF at picture 8, the mid at 4 and the two
    /// quarters at 2 and 6 -- each re-output by its own
    /// `show_existing_frame`. Every leaf reads a different pair of DPB slots
    /// than it did at three levels, so a slot map that lets one level
    /// overwrite a picture another is still reading shows up here as a
    /// display-order mismatch against ffmpeg.
    #[test]
    fn a_four_level_pyramid_stream_decodes_in_display_order() {
        let _knobs = crate::speed::knob_write();
        pyramid_round_trip(
            Pyramid {
                mini_gop: 8,
                mid_q_offset: Some(-8),
                quarter_q_offset: Some(4),
                ..Pyramid::default()
            },
            4,
        );
    }

    /// [`Pyramid::key_q_offset`] reaches the key frame: the whole run reads
    /// the key, so an offset that never arrived would be invisible in the BD
    /// table except as "no change" (class symbol-consumption-gap). A finer
    /// key must cost strictly more bytes than the same picture at base q.
    #[test]
    fn the_key_q_offset_reaches_the_key_frame() {
        let _knobs = crate::speed::knob_read();
        crate::encode::force_screen(Some(false));
        let key_bytes = |offset: i16| {
            let config = EncoderConfig {
                width: 128,
                height: 128,
                base_q_idx: 120,
                gop: 32,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let pyramid = Pyramid {
                key_q_offset: offset,
                ..Pyramid::default()
            };
            let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
            let packets = enc.encode_frames(&test_card(128, 128, 0)).unwrap();
            let key = packets
                .iter()
                .find(|p| p.level == Level::Key)
                .expect("key frame");
            key.data.len()
        };
        let (base, finer) = (key_bytes(0), key_bytes(-48));
        eprintln!("key at base q {base} bytes, at base q - 48 {finer} bytes");
        assert!(
            finer > base,
            "key_q_offset -48 did not reach the key frame ({finer} <= {base})"
        );
    }

    /// lane-last2's witness: a clip whose pictures ALTERNATE between two
    /// textures, so for every leaf the picture two back is an exact match and
    /// the one before it (`LAST_FRAME`) is not. With the lever on, the leaf
    /// chain alternates its refresh over [`LEAF_SLOTS`] and names the older
    /// slot `LAST2_FRAME`; blocks must actually be CODED off it
    /// ([[gate-blind-to-feature]]: a slot that is retained but never chosen
    /// proves only that the bookkeeping compiles), the stream must decode
    /// sample-exact through OUR decoder and through ffmpeg, and the hidden
    /// frames must still come out in display order. With the lever OFF the
    /// same clip codes ZERO `LAST2` blocks, which is what makes the counter a
    /// measurement of this lane and not of the reference set we already had.
    #[test]
    fn a_leaf_predicts_off_last2_when_the_picture_two_back_matches() {
        let _knobs = crate::speed::knob_write();
        crate::encode::force_screen(Some(false));
        let (width, height) = (128usize, 128usize);
        let sources: Vec<Picture> = (0..9)
            .map(|t| test_card(width, height, (t % 2) * 24))
            .collect();
        let run = |on: bool| -> ([usize; 8], Vec<u8>, Vec<Packet>) {
            set_last2(Some(on));
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 32,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let pyramid = Pyramid {
                mini_gop: 4,
                mid_q_offset: None,
                ..Pyramid::default()
            };
            let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
            let _ = crate::encode::take_ref_frame_hits();
            let mut packets = Vec::new();
            for picture in &sources {
                packets.extend(enc.encode_frames(picture).unwrap());
            }
            packets.extend(enc.flush().unwrap());
            let hits = crate::encode::take_ref_frame_hits();
            (hits, concat(&packets), packets)
        };
        let (off_hits, off_stream, _) = run(false);
        let (on_hits, stream, packets) = run(true);
        set_last2(None);
        eprintln!(
            "LAST2 off: LAST {} LAST2 {} GOLDEN {} ALTREF {} ({} B); \
             on: LAST {} LAST2 {} GOLDEN {} ALTREF {} ({} B)",
            off_hits[1],
            off_hits[2],
            off_hits[4],
            off_hits[7],
            off_stream.len(),
            on_hits[1],
            on_hits[2],
            on_hits[4],
            on_hits[7],
            stream.len(),
        );
        assert_eq!(off_hits[2], 0, "LAST2 was coded with the lever OFF");
        assert!(
            on_hits[2] >= 8,
            "the picture two back matches exactly and only {} blocks were coded off LAST2",
            on_hits[2],
        );

        // Display order and sample-exactness, both decoders (the same shape
        // `pyramid_round_trip` gates the hidden frames with).
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(
            ours.len(),
            sources.len(),
            "our decoder's display-order count"
        );
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half of the LAST2 witness: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's display-order count");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            assert_eq!(got.len(), b.len(), "frame {i}: luma size");
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "display frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
        }
        // The reordering itself is unchanged by the second leaf slot.
        let shown = packets
            .iter()
            .filter(|p| p.level == Level::ShowExisting)
            .count();
        let arfs = packets.iter().filter(|p| p.level == Level::Arf).count();
        assert_eq!(shown, arfs, "one show_existing_frame per hidden frame");
    }

    /// A picture whose content is a static gradient plus a per-picture
    /// pseudo-random +-2 of noise: motion search matches the neighbours
    /// almost exactly, so the ARF temporal filter's weights are near 1 and
    /// the filter actually changes the source (a moving test card gives every
    /// neighbour a large MSE, weight ~0, and the filter is a no-op that would
    /// make the future-window witness below pass blind).
    fn noisy_card(width: usize, height: usize, t: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        let mut seed = (t as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for y in 0..height {
            for x in 0..width {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let noise = ((seed >> 33) % 5) as i32 - 2;
                let base = 40 + (x + y) as i32 / 2;
                picture.y[y * width + x] = base.saturating_add(noise).clamp(0, 255) as u16;
            }
        }
        for i in 0..(width / 2) * (height / 2) {
            picture.u[i] = 110;
            picture.v[i] = 140;
        }
        picture
    }

    /// lane-lookahead's correctness witness. One group of lookahead
    /// (`Av1Encoder::drain_pending` hands the closed group to `ready` and
    /// codes the group before it) must not change WHAT is coded:
    ///
    /// * with the future half of the filter window empty AND the top ARF's
    ///   temporal lambda window pinned to the past (`TPL_FUT=0`) the stream
    ///   is byte for byte the one the encoder coded without a lookahead, at
    ///   every run length -- 1 picture (no hidden frame at all), 7 (a short
    ///   group), 8 (exactly `mini_gop`), 9 (a group plus a tail) and 17 --
    ///   and at a GOP short enough (8) that key frames drain the buffer
    ///   mid-run. lane-tplhalf's WIN=11 default also reads that buffer, so
    ///   the identity arm pins the lambda window off or this witness tests
    ///   a different lever;
    /// * every run decodes to exactly its picture count through OUR decoder
    ///   and through ffmpeg, sample-exact between the two, so the flush and
    ///   the `show_existing_frame` order survive the delay;
    /// * and with ONE future neighbour the top ARF's source filter actually
    ///   reads it -- the stream MOVES (class gate-blind-to-feature: a
    ///   buffered picture nothing consumes would pass every check above).
    #[test]
    fn the_lookahead_holds_one_group_and_still_codes_every_picture() {
        let _knobs = crate::speed::knob_write();
        // The preset is PROCESS-GLOBAL and another test may have left it
        // anywhere: at a high one every block of this fixture codes as skip
        // (17 pictures in 493 bytes) and the source filter cannot change a
        // byte, which failed the future-half assertion below in the suite and
        // passed it standalone (class process-global-knob-races-tests).
        let was = crate::speed::speed();
        crate::speed::set_speed(0);
        crate::encode::force_screen(Some(false));
        let (width, height) = (128usize, 128usize);
        let run = |n: usize, gop: usize, on: bool, fut: usize| -> (Vec<u8>, Vec<Packet>) {
            set_lookahead(Some(on));
            set_arf_tf_future(Some(fut));
            set_tpl_fut(Some(0));
            let config = EncoderConfig {
                // FINE (not the 120 the other facade tests use): at a coarse
                // quantizer this fixture's ARF codes almost pure skip (17
                // pictures in 493 bytes) and a filtered source quantizes back
                // to the same bytes, which made the future-half assertion
                // below depend on where the suite's other tests had left the
                // process-global preset. At q=60 the anchor carries real
                // coefficients and the filter must move them.
                base_q_idx: 60,
                width,
                height,
                gop,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
            let sources: Vec<Picture> = (0..n).map(|t| noisy_card(width, height, t)).collect();
            let packets = encode_all(&mut enc, &sources);
            // The content gate is process-global and so is the preset: if
            // either has been flipped under this test the pyramid is gone and
            // there is no ARF to filter, which is a different failure from
            // "the future neighbour changed nothing" (the suite reported the
            // second when it meant the first).
            assert!(
                enc.pyramid().is_some(),
                "{n} pictures at gop {gop}: the content gate dropped the pyramid,                  so no ARF was coded (speed {})",
                crate::speed::speed(),
            );
            (concat(&packets), packets)
        };
        for gop in [32usize, 8] {
            for n in [1usize, 7, 8, 9, 17] {
                let (flat, flat_packets) = run(n, gop, false, 0);
                let (held, packets) = run(n, gop, true, 0);
                assert_eq!(
                    flat.len(),
                    held.len(),
                    "{n} pictures at gop {gop}: the lookahead changed the stream LENGTH"
                );
                assert!(
                    flat == held,
                    "{n} pictures at gop {gop}: the stream is not byte-identical"
                );
                assert_eq!(
                    packets
                        .iter()
                        .map(|p| (p.order, p.key, p.level))
                        .collect::<Vec<_>>(),
                    flat_packets
                        .iter()
                        .map(|p| (p.order, p.key, p.level))
                        .collect::<Vec<_>>(),
                    "{n} pictures at gop {gop}: coding order/levels moved"
                );
                let ours = crate::stream::decode_stream(&held).expect("our decoder");
                assert_eq!(
                    ours.len(),
                    n,
                    "{n} pictures at gop {gop}: our decoder's count"
                );
                if !have_ffmpeg() {
                    continue;
                }
                let theirs = ffmpeg_decode_luma(&held, width, height);
                assert_eq!(theirs.len(), n, "{n} pictures at gop {gop}: ffmpeg's count");
                for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                    let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                    assert!(
                        got == *b,
                        "{n} pictures at gop {gop}, display frame {i}: ours differs from ffmpeg"
                    );
                }
            }
        }
        // The future half REACHES the filter, and what it produces still
        // decodes exactly.
        let (base, _) = run(17, 32, true, 0);
        let (symmetric, _) = run(17, 32, true, 1);
        set_lookahead(None);
        set_arf_tf_future(None);
        set_tpl_fut(None);
        assert!(
            base != symmetric,
            "one display-future neighbour left the top ARF's filter unchanged \
             ({} B vs {} B, preset {})",
            base.len(),
            symmetric.len(),
            crate::speed::speed()
        );
        let ours = crate::stream::decode_stream(&symmetric).expect("our decoder");
        assert_eq!(ours.len(), 17, "the symmetric arm's display-order count");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_luma(&symmetric, width, height);
            assert_eq!(theirs.len(), 17, "the symmetric arm's ffmpeg count");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                assert!(
                    got == *b,
                    "symmetric arm, display frame {i}: ours differs from ffmpeg"
                );
            }
        }
        crate::speed::set_speed(was);
    }

    /// lane-tplfut's correctness witness, the sibling of
    /// [`the_lookahead_holds_one_group_and_still_codes_every_picture`]: with
    /// the top ARF's TEMPORAL LAMBDA window taken out of the buffered next
    /// group (`speed::TPL_FUT`), every picture is still coded, the streams
    /// decode exactly through our decoder and ffmpeg, and the stream MOVES
    /// against the knob off wherever a next group exists (class
    /// gate-blind-to-feature). Coded at q=60, not 120: at a coarse
    /// quantizer this fixture's ARF is almost pure skip and a different
    /// lambda map re-quantises to identical bytes.
    #[test]
    fn the_future_tpl_window_still_codes_every_picture_and_moves_the_stream() {
        let _knobs = crate::speed::knob_write();
        let was = crate::speed::speed();
        crate::speed::set_speed(0);
        crate::encode::force_screen(Some(false));
        let (width, height) = (128usize, 128usize);
        let run = |n: usize, gop: usize, fut: usize| -> (Vec<u8>, Vec<Packet>) {
            set_lookahead(Some(true));
            set_arf_tf_future(Some(0));
            set_tpl_fut(Some(fut));
            let config = EncoderConfig {
                base_q_idx: 60,
                width,
                height,
                gop,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
            let sources: Vec<Picture> = (0..n).map(|t| noisy_card(width, height, t)).collect();
            let packets = encode_all(&mut enc, &sources);
            assert!(
                enc.pyramid().is_some(),
                "{n} pictures at gop {gop}: the content gate dropped the pyramid (speed {})",
                crate::speed::speed(),
            );
            (concat(&packets), packets)
        };
        for gop in [32usize, 8] {
            for n in [1usize, 7, 8, 9, 17] {
                let (off, off_packets) = run(n, gop, 0);
                for mode in [1usize, 2, 3] {
                    let (on, packets) = run(n, gop, mode);
                    assert_eq!(
                        packets
                            .iter()
                            .map(|p| (p.order, p.key, p.level))
                            .collect::<Vec<_>>(),
                        off_packets
                            .iter()
                            .map(|p| (p.order, p.key, p.level))
                            .collect::<Vec<_>>(),
                        "{n} pictures at gop {gop} mode {mode}: coding order/levels moved"
                    );
                    let ours = crate::stream::decode_stream(&on).expect("our decoder");
                    assert_eq!(
                        ours.len(),
                        n,
                        "{n} pictures at gop {gop} mode {mode}: our count"
                    );
                    if !have_ffmpeg() {
                        continue;
                    }
                    let theirs = ffmpeg_decode_luma(&on, width, height);
                    assert_eq!(
                        theirs.len(),
                        n,
                        "{n} pictures at gop {gop} mode {mode}: ffmpeg count"
                    );
                    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                        let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                        assert!(
                            got == *b,
                            "{n} pictures at gop {gop} mode {mode}, frame {i}: ours differs from ffmpeg"
                        );
                    }
                }
                let _ = &off;
            }
        }
        // The future window REACHES the lambda map: 17 pictures at gop 32 is
        // three groups, so the first two top ARFs each have a next group
        // buffered and their windows really are display-future.
        let (off, _) = run(17, 32, 0);
        // Mode 2 is NOT in this list: at its default split it keeps the three
        // NEAREST past neighbours, and those dominate the propagation -- on
        // this 128x128 fixture the map moves too little to flip an RD
        // decision and the stream comes back byte-identical to mode 0. Not a
        // dead knob: the same run with `EC_AV1_TPL_FUT_HALF=1` (one past
        // neighbour, six future) does move the stream, and the gate below
        // measures mode 2 on real content.
        for mode in [1usize, 3] {
            let (on, _) = run(17, 32, mode);
            assert!(
                off != on,
                "mode {mode} left the top ARF's temporal lambda map unchanged \
                 ({} B vs {} B, preset {})",
                off.len(),
                on.len(),
                crate::speed::speed()
            );
        }
        set_lookahead(None);
        set_arf_tf_future(None);
        set_tpl_fut(None);
        crate::speed::set_speed(was);
    }

    /// lane-arfmode: the top ARF's leaf offer set. Three guards in one
    /// witness, over 17 pictures at gop 32 (three groups, so several top
    /// ARFs) at q=60 -- the lookahead witness's lesson: at a fine q the
    /// anchor carries real coefficients, so a cut that moves only the
    /// hidden frame's internal decisions cannot hide behind "everything
    /// quantized to the same bytes":
    ///
    /// 1. OFF (default and explicit `Some(0)`) is byte-identical -- the
    ///    shipped table is 0 and the lever is bit-exact when off;
    /// 2. each armed mode (1 no intra, 2 no 8x8, 4 no compound) really
    ///    FIRES -- its own [`crate::encode::take_arfmode_hits`] counter
    ///    moves (class `gate-blind-to-feature`) -- and MOVES THE STREAM;
    /// 3. the coding order and levels never change, and every armed stream
    ///    decodes sample-exact through `decode_stream` AND ffmpeg.
    #[test]
    fn the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let was = crate::speed::speed();
        crate::speed::set_speed(0);
        crate::encode::force_screen(Some(false));
        let (width, height) = (128usize, 128usize);
        let run = |mode: Option<u8>| -> (Vec<u8>, Vec<Packet>) {
            crate::encode::set_arfmode(mode);
            let _ = crate::encode::take_arfmode_hits();
            let config = EncoderConfig {
                base_q_idx: 60,
                width,
                height,
                gop: 32,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::with_pyramid(config, Pyramid::default()).unwrap();
            // Groups at three luma LEVELS (the base ramp 40/110/180): the
            // middle group's top ARF sits halfway between the two dark/light
            // anchors its slots hold, which is exactly where neither
            // single-reference prediction nor intra wins by default -- the
            // AVERAGE of two references (compound) and the DC of the new
            // level (intra) do. Without this the synthetic cards' noise
            // quantizes to skip and no arm can move anything (measured:
            // 516 intra trials skipped, zero won, stream identical).
            let sources: Vec<Picture> = (0..17)
                .map(|t| {
                    let mut p = noisy_card(width, height, t);
                    let shift = 70i32 * (t / 8) as i32;
                    for v in p.y.iter_mut() {
                        *v = (i32::from(*v) + shift).clamp(0, 255) as u16;
                    }
                    p
                })
                .collect();
            let packets = encode_all(&mut enc, &sources);
            (concat(&packets), packets)
        };
        let (off, off_packets) = run(None);
        let (explicit_off, _) = run(Some(0));
        assert_eq!(
            off, explicit_off,
            "mode 0 is not bit-exact with the default"
        );
        for (mode, arm) in [(1u8, 0usize), (2, 1), (4, 2)] {
            let (on, packets) = run(Some(mode));
            let hits = crate::encode::take_arfmode_hits();
            assert_eq!(
                packets
                    .iter()
                    .map(|p| (p.order, p.key, p.level))
                    .collect::<Vec<_>>(),
                off_packets
                    .iter()
                    .map(|p| (p.order, p.key, p.level))
                    .collect::<Vec<_>>(),
                "mode {mode}: coding order/levels moved"
            );
            assert!(
                hits[arm] > 0,
                "mode {mode}: its counter never fired ({hits:?}) -- the lever is \
                 gate-blind on this fixture",
            );
            assert_ne!(
                off, on,
                "mode {mode} left the stream byte-identical despite {} firing hits",
                hits[arm],
            );
            eprintln!(
                "mode {mode}: {} -> {} B, hits {hits:?}",
                off.len(),
                on.len()
            );
            let ours = crate::stream::decode_stream(&on).expect("our decoder");
            assert_eq!(ours.len(), 17, "mode {mode}: our decoder's picture count");
            if !have_ffmpeg() {
                continue;
            }
            let theirs = ffmpeg_decode_luma(&on, width, height);
            assert_eq!(theirs.len(), 17, "mode {mode}: ffmpeg's picture count");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                assert!(
                    got == *b,
                    "mode {mode}, frame {i}: ours differs from ffmpeg"
                );
            }
        }
        crate::encode::set_arfmode(None);
        crate::speed::set_speed(was);
    }

    fn pyramid_round_trip(pyramid: Pyramid, hidden: usize) {
        let (width, height) = (128usize, 128usize);
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 32,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        // The content gate ([`Av1Encoder::encode_frames`]) would drop a
        // screen-content stream to the flat path, and a synthetic test card
        // is exactly the few-colour picture the detector fires on: this gate
        // is about the reordering, so it asks for camera content explicitly.
        crate::encode::force_screen(Some(false));
        let mut enc = Av1Encoder::with_pyramid(config, pyramid).unwrap();
        let sources: Vec<Picture> = (0..9).map(|t| test_card(width, height, t * 3)).collect();
        let _ = crate::encode::take_ref_frame_hits();
        let mut packets = Vec::new();
        for picture in &sources {
            packets.extend(enc.encode_frames(picture).unwrap());
        }
        packets.extend(enc.flush().unwrap());

        // Shape: every source picture is coded exactly once, and each closed
        // group adds one `show_existing_frame` packet.
        let coded: Vec<u64> = packets
            .iter()
            .filter(|p| p.level != Level::ShowExisting)
            .map(|p| p.order)
            .collect();
        let mut sorted = coded.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..9).collect::<Vec<u64>>(),
            "one packet per picture"
        );
        assert_ne!(coded, sorted, "the pyramid never reordered anything");
        let shown = packets
            .iter()
            .filter(|p| p.level == Level::ShowExisting)
            .count();
        assert_eq!(shown, hidden, "one show_existing_frame per hidden frame");
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(p.dts, i as u64, "packet {i}: dts is the coding position");
        }
        let hist = |l: Level| packets.iter().filter(|p| p.level == l).count();
        eprintln!(
            "pyramid levels: key {} arf {} leaf {} show_existing {}",
            hist(Level::Key),
            hist(Level::Arf),
            hist(Level::Leaf),
            hist(Level::ShowExisting),
        );
        assert_eq!(hist(Level::Arf), hidden, "hidden frames");
        let refs = crate::encode::take_ref_frame_hits();
        eprintln!(
            "blocks per reference: LAST {} GOLDEN {} ALTREF {}",
            refs[crate::mvstack::LAST_FRAME as usize],
            refs[crate::mvstack::GOLDEN_FRAME as usize],
            refs[crate::mvstack::ALTREF_FRAME as usize],
        );
        // [[gate-blind-to-feature]]: the whole point of the hidden frame is
        // that the leaves predict BACKWARD off it, which is also the only
        // thing that exercises `ref_frame_sign_bias` in the MV stack. A green
        // round trip in which `ALTREF_FRAME` never won a block would prove
        // only that a hidden frame can be written and re-output.
        assert!(
            refs[crate::mvstack::ALTREF_FRAME as usize] > 0,
            "no block predicted off the hidden ALTREF"
        );

        let stream = concat(&packets);
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(
            ours.len(),
            sources.len(),
            "our decoder's display-order count"
        );
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half of the pyramid round trip: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's display-order count");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            assert_eq!(got.len(), b.len(), "frame {i}: luma size");
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "display frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
        }
    }

    // Every tile layout this encoder can write decodes SAMPLE-EXACT
    // through both decoders -- ours and ffmpeg's -- and the frames really
    // carry the tiles the header claims (the OBU parser locates one payload
    // per tile, so a stream that quietly stayed single-tile fails here
    // rather than passing on a green round trip).
    //
    // The bytes are what the gate is on: an encoder that clipped its
    // neighbour availability wrong writes a stream whose two decoders agree
    // with each other and disagree with nothing -- so the ffmpeg half is
    // the one that catches a tile boundary the writer respected and the
    // spec does not, and the parser half catches the reverse.

    /// A STATIC clip: every inter frame predicts perfectly from the last, so
    /// the search takes the 64x64 root ([`crate::encode::B64_ROOT`]) instead
    /// of coding four 32x32 quadrants, and the stream both decoders
    /// reconstruct is still sample-exact against the encoder's own
    /// reconstruction. The counter is the point (class
    /// `gate-blind-to-feature`): without it a green three-way compare says
    /// nothing about whether a single 64x64 block was ever written.
    /// lane-sb128 step 1: the same static clip under a 128x128 SUPERBLOCK.
    /// Every partition decision below the root is the one the 64-superblock
    /// encoder makes -- the root itself is always `PARTITION_SPLIT` into the
    /// four 64x64 superblocks -- so the only thing this proves is the
    /// structural syntax: the root's own partition symbol, its loop
    /// restoration read once over the 128 span at the 128 unit size, and
    /// libaom's TL/TR/BL/BR visit order. 256x128 is 2x1 128x128 superblocks
    /// per frame over three frames, so the root fires 6 times.
    ///
    /// `#[ignore]`: [`crate::encode::force_sb128`] moves a process-global
    /// (the superblock size is a sequence field), so this gate runs alone:
    ///
    /// Runs only inside `superblock_size_global_suite`.
    pub(crate) fn a_static_clip_codes_128x128_superblock_roots_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        crate::encode::force_sb128(true);
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3).map(|_| still.clone()).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::tile::take_sb128_root_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let roots = crate::tile::take_sb128_root_hits();
        assert!(
            roots >= 4,
            "a 128x128-superblock clip coded {roots} superblock roots, wanted at least 4"
        );
        eprintln!("128x128 superblock roots: {roots}");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        assert!(
            crate::decode::part128_split_hits() >= 4,
            "our decoder read no 128x128 superblock root"
        );
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            assert_eq!(&got, b, "frame {i}: ffmpeg and our decoder disagree");
        }
    }

    #[test]
    fn a_static_clip_codes_64x64_roots_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3).map(|_| still.clone()).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_root_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let roots = crate::encode::take_b64_root_hits();
        // 8 superblocks per frame, two inter frames.
        assert!(
            roots > 0,
            "a static clip coded no 64x64 root at all (of 16 inter superblocks)"
        );
        eprintln!("64x64 roots on a static clip: {roots} of 16 inter superblocks");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
            assert_eq!(
                got.len(),
                b.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                b.len()
            );
        }
    }

    /// The same clip with a DC step between frames (lane-tx64): the 64x64
    /// root still predicts the whole superblock from the last frame, but the
    /// prediction is uniformly off, so the root's NON-skip arm -- one
    /// TX_64X64 luma transform and two TX_32X32 chroma ones -- prices below
    /// its skip arm and a 64x64 residual is really written. The counter is
    /// the gate (class `gate-blind-to-feature`; the static clip above cannot
    /// reach this arm at all because its prediction is exact), and both
    /// decoders have to reconstruct those coefficients sample-exact -- a
    /// writer that names the wrong coefficient set or the wrong `txfm_split`
    /// desyncs the whole tile here.
    #[test]
    fn a_dc_stepped_clip_codes_a_64x64_residual_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3)
            .map(|t| {
                let mut p = still.clone();
                let step = (t * 6) as u16;
                for v in &mut p.y {
                    *v = (*v + step).min(255);
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_residual_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let residuals = crate::encode::take_b64_residual_hits();
        assert!(
            residuals > 0,
            "a DC-stepped clip coded no 64x64 root with a residual at all"
        );
        eprintln!("64x64 roots with a residual: {residuals} of 16 inter superblocks");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    b[at],
                );
            }
            assert_eq!(
                got.len(),
                b.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                b.len()
            );
        }
    }

    /// A frame whose size is NOT a multiple of a superblock (lane-b64b):
    /// 232x168 leaves the last superblock column and row cut by the true
    /// frame edge, with more than half of each inside -- which is exactly
    /// what AV1's `has_cols`/`has_rows` allow to stay whole. Both gate films
    /// are superblock-aligned, so nothing but a synthetic size reaches this
    /// path at all (class `gate-blind-to-feature`), and the user's own
    /// exports are arbitrary sizes.
    #[test]
    fn a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (232usize, 168usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..3).map(|_| still.clone()).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_edge_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let edges = crate::encode::take_b64_edge_hits();
        assert!(
            edges > 0,
            "no 64x64 root was taken at a superblock the frame edge cuts"
        );
        eprintln!("edge 64x64 roots: {edges}");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (ours_i, theirs_i)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = ours_i.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(theirs_i).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    theirs_i[at],
                );
            }
            assert_eq!(
                got.len(),
                theirs_i.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                theirs_i.len()
            );
        }
    }

    /// The 1080p SHAPE at a fifth of its size (lane-sse): 384x216 is three
    /// whole superblock columns and 1.6875 superblock rows, so the bottom
    /// 88-pixel band straddles at EVERY level -- the 64x64 root lane-b64b
    /// already clipped, and the 32x32 (216 = 6*32 + 24) and 16x16 (13*16 + 8)
    /// leaves it did not. Every one of them is now scored over its inside
    /// part alone, and a distortion change that moved a partition or a
    /// transform decision the writer did not follow would desync the tile, so
    /// both decoders reconstruct it sample-exact.
    #[test]
    fn a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (384usize, 216usize);
        let sources: Vec<Picture> = (0..3).map(|i| test_card(width, height, i)).collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (ours_i, theirs_i)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = ours_i.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(theirs_i).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    theirs_i[at],
                );
            }
            assert_eq!(
                got.len(),
                theirs_i.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                theirs_i.len()
            );
        }
    }

    /// A clip whose inter residual is HIGH-FREQUENCY (lane-b64b): every
    /// frame adds a 4-pixel checkerboard, so the 64x64 root's prediction is
    /// off by exactly the detail a TX_64X64 throws away (it codes only the
    /// top-left 32x32 corner of its coefficient space) and the root's var-tx
    /// arm splits its luma into four TX_32X32 units instead. The counter is
    /// the gate (class `gate-blind-to-feature`), and a `txfm_split` the
    /// reader does not expect desyncs the whole tile, so both decoders have
    /// to reconstruct it sample-exact.
    #[test]
    fn a_high_frequency_clip_splits_the_64x64_roots_luma_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let still = test_card(width, height, 0);
        let sources: Vec<Picture> = (0..4)
            .map(|t| {
                let mut p = still.clone();
                for (i, v) in p.y.iter_mut().enumerate() {
                    let checker = ((i % width) / 4 + (i / width) / 4) % 2;
                    let bump = (t * 5 * (checker + 1)) as u16;
                    *v = (*v + bump).min(255);
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 90,
            gop: 8,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_var_tx_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let split = crate::encode::take_b64_var_tx_hits();
        assert!(split > 0, "no 64x64 root split its luma transform at all");
        eprintln!("64x64 roots with a SPLIT luma transform: {split}");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (ours_i, theirs_i)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = ours_i.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(theirs_i).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    theirs_i[at],
                );
            }
            assert_eq!(
                got.len(),
                theirs_i.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                theirs_i.len()
            );
        }
    }

    /// A CROSSFADE clip (lane-b64b): each picture is a linear blend of two
    /// stills, so on a leaf frame the average of its past (`LAST`) and future
    /// (`ALTREF`) references IS the source and neither reference alone is --
    /// exactly the content a COMPOUND 64x64 root pays on. The counter is the
    /// gate (class `gate-blind-to-feature`): without it a green three-way
    /// compare says nothing about whether one compound root was written, and
    /// the writer's compound arm (`write_compound_block` at `SB_MI` 16, no
    /// `motion_mode` symbol) desyncs the whole tile if it is wrong.
    #[test]
    fn a_crossfade_clip_codes_a_compound_64x64_root_and_decodes_sample_exact() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (256usize, 128usize);
        let (a, b) = (test_card(width, height, 0), test_card(width, height, 24));
        let sources: Vec<Picture> = (0..9)
            .map(|t| {
                let mut p = a.clone();
                let w = t as u32;
                for (v, &other) in p.y.iter_mut().zip(&b.y) {
                    *v = ((u32::from(*v) * (8 - w) + u32::from(other) * w) / 8) as u16;
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 9,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let mut stream = Vec::new();
        let _ = crate::encode::take_b64_compound_hits();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let compound = crate::encode::take_b64_compound_hits();
        assert!(
            compound > 0,
            "a crossfade clip coded no COMPOUND 64x64 root at all"
        );
        eprintln!("compound 64x64 roots on a crossfade clip: {compound}");

        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let theirs = ffmpeg_decode_luma(&stream, width, height);
        assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
        for (i, (ours_i, theirs_i)) in ours.iter().zip(&theirs).enumerate() {
            let got: Vec<u8> = ours_i.y.iter().map(|&v| v as u8).collect();
            if let Some(at) = got.iter().zip(theirs_i).position(|(x, y)| x != y) {
                panic!(
                    "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                    at % width,
                    at / width,
                    got[at],
                    theirs_i[at],
                );
            }
            assert_eq!(
                got.len(),
                theirs_i.len(),
                "frame {i}: luma is {} samples, ffmpeg's is {}",
                got.len(),
                theirs_i.len()
            );
        }
    }

    #[test]
    fn every_tile_layout_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_read();
        let (width, height) = (640usize, 384usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let mut sizes = Vec::new();
        for (cols_log2, rows_log2) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1), (2, 1)] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 4,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let layout = format!("{}x{} tiles", 1 << cols_log2, 1 << rows_log2);
            // lane-b128m: the SPEC's uniform spacing derives the tile count
            // from the superblock grid (`tileWidthSb = ceil(sbCols >> log2)`),
            // so a frame with fewer superblock columns than the requested
            // count codes fewer tiles -- which is what a 128-superblock
            // sequence does here. The grid the encoder itself computes is the
            // expectation; at 64 superblocks it is `1 << log2` exactly.
            let grid = crate::tile::TileLayout::new(
                (width as u32).div_ceil(4),
                (height as u32).div_ceil(4),
                cols_log2,
                rows_log2,
            );
            let (want_cols, want_rows) = (grid.cols(), grid.rows());
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            sizes.push((layout.clone(), stream.len()));

            // The frames carry the tiles their headers claim.
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut frames = 0usize;
            let mut offset = 0usize;
            while offset < stream.len() {
                let obus = parser.parse_temporal_unit(&stream[offset..]).unwrap();
                let unit: usize = obus.iter().map(|o| o.total_size).sum();
                for obu in &obus {
                    if let ec_av1_syntax::ObuKind::Frame(parsed, tiles) = &obu.kind {
                        assert_eq!(
                            (parsed.tile_info.cols, parsed.tile_info.rows),
                            (want_cols, want_rows),
                            "{layout}: the header's own tile grid"
                        );
                        assert_eq!(
                            tiles.len(),
                            (want_cols * want_rows) as usize,
                            "{layout}: tile payloads located in the tile group"
                        );
                        frames += 1;
                    }
                }
                offset += unit;
            }
            assert_eq!(frames, sources.len(), "{layout}: coded frames");

            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), sources.len(), "{layout}: our decoder's frames");
            if !have_ffmpeg() {
                eprintln!("SKIP the ffmpeg half of {layout}: no ffmpeg");
                continue;
            }
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "{layout}: ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                assert_eq!(got.len(), b.len(), "{layout} frame {i}: luma size");
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "{layout} frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
            }
        }
        let base = sizes[0].1 as f64;
        for (layout, bytes) in &sizes {
            eprintln!(
                "{layout}: {bytes} bytes ({:+.2}% vs one tile)",
                (*bytes as f64 / base - 1.0) * 100.0
            );
        }
    }

    /// lane-deltaq's three-way witness: a clip with a real gradient of tpl
    /// factors -- a moving detail region over a flat background, so the
    /// superblocks the NEXT picture predicts from are worth more than the
    /// ones it does not -- codes at least two distinct per-superblock
    /// quantizer levels (the fire count, class `gate-blind-to-feature`), and
    /// both decoders reconstruct every frame sample-exact from the
    /// `delta_qindex` syntax that carries them.
    #[test]
    fn a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly() {
        // `knob_write`, not `knob_read`: the override below is
        // process-global and every other test's bytes move under it.
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        crate::encode::set_deltaq_res(Some(2));
        let (width, height) = (640usize, 384usize);
        // The gate's own moving test card, with a 128x128 patch of fine
        // detail travelling right by 32 samples a frame laid over it: the
        // patch is what the tpl pass finds worth propagating and the card
        // around it is not, so the map -- and with it the quantizer grid --
        // is a gradient and not one flat number.
        let sources: Vec<Picture> = (0..4)
            .map(|t| {
                let mut p = test_card(width, height, t * 3);
                let x0 = 32 + t * 32;
                for y in 128..256 {
                    for x in x0..x0 + 128 {
                        p.y[y * width + x] = if (x / 2 + y / 3) % 2 == 0 { 40 } else { 210 };
                    }
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let _ = crate::encode::take_deltaq_levels();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let levels = crate::encode::take_deltaq_levels();
        // The quantizer grid rides the tpl map, and `speed::TPL_DEPTH` cuts
        // the lookahead window to one picture at the top presets, so there IS
        // no map -- and no delta_q syntax -- there. The exactness half below
        // still runs; only the fire count needs a window (lane-tplwin: the
        // predicate is the window, not preset 0, since presets 1..6 have one).
        if crate::speed::at(&crate::speed::TPL_DEPTH) > 1 {
            assert!(
                levels >= 2,
                "the quantizer grid was flat ({levels} level(s)): no delta_qindex was coded"
            );
        } else {
            eprintln!(
                "SKIP the fire count at preset {}: no tpl map",
                crate::speed::speed()
            );
        }
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
                assert_eq!(
                    got.len(),
                    b.len(),
                    "frame {i}: luma is {} samples, ffmpeg's is {}",
                    got.len(),
                    b.len()
                );
            }
        } else {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
        }
        crate::encode::set_deltaq_res(None);
        eprintln!(
            "delta_q witness: {levels} quantizer levels, {} bytes",
            stream.len()
        );
    }

    /// lane-txset's witness: a clip of hard directional edges makes the intra
    /// luma search take EVERY type of the five-type reduced set
    /// (`TX_SET_INTRA_2`: `DCT_DCT`, `ADST_ADST`, `ADST_DCT`, `DCT_ADST`,
    /// `IDTX`) at least once -- the fire count, class `gate-blind-to-feature`
    /// -- and both decoders reconstruct every frame sample-exact from the
    /// `tx_type` symbols that name them. Before the lane the writer coded
    /// `DCT_DCT` for every luma transform unit ever written, so this is also
    /// the statement that the other four are REACHABLE at all.
    #[test]
    fn an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly() {
        // `knob_write`: the screen override below is process-global, and the
        // type search is SCREEN-ONLY (`encode::tx_type_candidates`).
        let _knobs = crate::speed::knob_write();
        crate::encode::force_screen(Some(true));
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (320usize, 192usize);
        // Sharp horizontal and vertical steps: a vertical edge is what an
        // `ADST` column pass beats a `DCT` one on, a horizontal edge the
        // mirror, and the flat/ramp areas between them are where `IDTX` wins.
        let sources: Vec<Picture> = (0..2)
            .map(|t| {
                let mut p = test_card(width, height, t * 3);
                for y in 0..height {
                    for x in 0..width {
                        let v = match ((x + t * 4) / 16 % 2, y / 16 % 2) {
                            (0, 0) => 16u16,
                            (1, 0) => 235,
                            (0, 1) => (x % 64 * 3) as u16,
                            _ => (y % 64 * 3) as u16,
                        };
                        p.y[y * width + x] = v;
                    }
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 90,
            gop: 2,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let _ = crate::encode::take_tx_type_hits();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let hits = crate::encode::take_tx_type_hits();
        // The type search is a speed lever (`speed::TX_TYPE_SEARCH`); above
        // the presets that carry it only `DCT_DCT` is offered, and the
        // exactness half below is what still runs there. The FULL five-type
        // fire count is preset 0's to make: a faster preset prunes modes and
        // partitions before a type is ever priced on them, so `ADST_ADST` and
        // `IDTX` stop winning on this clip at preset 6 (measured: hits
        // `[0, 6192, 0, 79, 284, ..]`). Every preset that carries the lever
        // still has to reach SOME non-`DCT_DCT` type -- that is the
        // "reachable at all" statement, class `gate-blind-to-feature`.
        if crate::speed::at(&crate::speed::TX_TYPE_SEARCH) {
            let non_dct: usize =
                hits.iter().sum::<usize>() - hits[crate::transform::TxType::DctDct as usize];
            assert!(
                non_dct > 0,
                "every luma transform unit took DCT_DCT: {hits:?}"
            );
        }
        if crate::speed::at(&crate::speed::TX_TYPE_SEARCH) && crate::speed::speed() == 0 {
            for (name, ty) in [
                ("DCT_DCT", crate::transform::TxType::DctDct),
                ("ADST_ADST", crate::transform::TxType::AdstAdst),
                ("ADST_DCT", crate::transform::TxType::AdstDct),
                ("DCT_ADST", crate::transform::TxType::DctAdst),
                ("IDTX", crate::transform::TxType::Idtx),
            ] {
                assert!(
                    hits[ty as usize] > 0,
                    "no luma transform unit took {name}: {hits:?}"
                );
            }
        } else {
            eprintln!(
                "SKIP the five-type fire count at preset {}: {hits:?}",
                crate::speed::speed()
            );
        }
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
                assert_eq!(
                    got.len(),
                    b.len(),
                    "frame {i}: luma is {} samples, ffmpeg's is {}",
                    got.len(),
                    b.len()
                );
            }
        } else {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
        }
        crate::encode::force_screen(None);
        eprintln!("tx_type witness: hits {hits:?}, {} bytes", stream.len());
    }

    /// lane-txset2's witness: an INTER frame's luma transform units take BOTH
    /// types of the two-type inter set the writer already codes a symbol for
    /// (`TX_SET_INTER_3`: `IDTX` and `DCT_DCT`) at least once -- the fire
    /// count, class `gate-blind-to-feature` -- and both decoders reconstruct
    /// every frame sample-exact from the `tx_type` symbols that name them.
    /// Before this lane every inter luma unit was coded `DCT_DCT`.
    #[test]
    fn an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly() {
        // The lever is OFF at every preset (`speed::TX_TYPE_SEARCH_INTER`), so
        // this turns it on process-globally under the same exclusive lock the
        // presets take.
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        crate::encode::set_inter_tx_search(Some(true));
        let (width, height) = (320usize, 192usize);
        // Sharp steps that MOVE: the motion-compensated residual along a
        // moving edge is exactly where the identity transform beats a DCT,
        // and the flat plateaus between them predict to nothing at all.
        let sources: Vec<Picture> = (0..4)
            .map(|t| {
                let mut p = test_card(width, height, t * 3);
                for y in 0..height {
                    for x in 0..width {
                        let v = match ((x + t * 5) / 16 % 2, (y + t * 3) / 16 % 2) {
                            (0, 0) => 16u16,
                            (1, 0) => 235,
                            (0, 1) => ((x + t) % 64 * 3) as u16,
                            _ => ((y + t) % 64 * 3) as u16,
                        };
                        p.y[y * width + x] = v;
                    }
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 90,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let _ = crate::encode::take_inter_tx_type_hits();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let hits = crate::encode::take_inter_tx_type_hits();
        crate::encode::set_inter_tx_search(None);
        for (name, ty) in [
            ("DCT_DCT", crate::transform::TxType::DctDct),
            ("IDTX", crate::transform::TxType::Idtx),
        ] {
            assert!(
                hits[ty as usize] > 0,
                "no inter luma transform unit was CODED {name}: {hits:?}"
            );
        }
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
                assert_eq!(
                    got.len(),
                    b.len(),
                    "frame {i}: luma is {} samples, ffmpeg's is {}",
                    got.len(),
                    b.len()
                );
            }
        } else {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
        }
        eprintln!(
            "inter tx_type witness: hits {hits:?}, {} bytes",
            stream.len()
        );
    }

    /// lane-txi's witness: a `reduced_tx_set = 0` frame codes the WIDER
    /// `tx_type` alphabets -- the seven-type `TX_SET_INTRA_1` at intra
    /// 8x8/4x4 and the twelve/sixteen-type inter sets at 16x16 and below --
    /// and both decoders reconstruct every frame sample-exact from them. The
    /// statement is that the types only those alphabets can NAME (`V_DCT`,
    /// `H_DCT`, and any inter type outside the two-type `DCT_IDTX`) are
    /// reachable at all, and that the header bit, the writer's set map and
    /// the search's pricer agree on which alphabet a symbol was coded into
    /// (class `wrong-alphabet-same-value`: a disagreement here changes no
    /// symbol VALUE and desyncs the arithmetic coder anyway).
    #[test]
    #[ignore = "sets process-global search levers: run it alone"]
    fn a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly() {
        // Every lever below is process-global: the same exclusive lock the
        // presets take.
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        crate::encode::set_intra_tx_search(Some(true));
        crate::encode::set_inter_tx_search(Some(true));
        crate::encode::set_wide_tx_set(Some(true));
        let (width, height) = (320usize, 192usize);
        // The same moving hard-edge card the two narrow-set witnesses use:
        // vertical steps are what a 1-D `V_DCT`/`H_DCT` pass wins on, the
        // ramps are `ADST`'s and the moving plateaus `IDTX`'s.
        let sources: Vec<Picture> = (0..4)
            .map(|t| {
                let mut p = test_card(width, height, t * 3);
                for y in 0..height {
                    for x in 0..width {
                        let v = match ((x + t * 5) / 16 % 2, (y + t * 3) / 16 % 2) {
                            (0, 0) => 16u16,
                            (1, 0) => 235,
                            (0, 1) => ((x + t) % 64 * 3) as u16,
                            _ => ((y + t) % 64 * 3) as u16,
                        };
                        p.y[y * width + x] = v;
                    }
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 90,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let _ = crate::encode::take_tx_type_hits();
        let _ = crate::encode::take_inter_tx_type_hits();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let (intra, inter) = (
            crate::encode::take_tx_type_hits(),
            crate::encode::take_inter_tx_type_hits(),
        );
        crate::encode::set_wide_tx_set(None);
        crate::encode::set_inter_tx_search(None);
        crate::encode::set_intra_tx_search(None);
        eprintln!(
            "wide tx_type witness: intra {intra:?} inter {inter:?}, {} bytes",
            stream.len()
        );
        // The fire count is preset 0's, for the reason the five-type witness
        // spells out: a faster preset prunes the modes and partitions a type
        // wins on before it is ever priced (class `gate-blind-to-feature`).
        if crate::speed::at(&crate::speed::TX_TYPE_SEARCH) && crate::speed::speed() == 0 {
            use crate::transform::TxType;
            let wide_only = |hits: &[usize; 16]| -> usize {
                hits[TxType::VDct as usize] + hits[TxType::HDct as usize]
            };
            assert!(
                wide_only(&intra) > 0,
                "no intra luma unit took a 1-D DCT, the type only TX_SET_INTRA_1 names: {intra:?}"
            );
            // The INTER units of this same frame are coded into the
            // twelve/sixteen-symbol alphabets too (the header bit widens
            // every luma set at once), but their SEARCH still offers the
            // two-type list -- see `encode::inter_luma_set`'s note on the
            // reference-slot refusal the wider inter candidates trip. So the
            // statement here is that those units still decode sample-exact
            // out of the wider alphabet, which the two decoders below make.
            assert!(
                inter.iter().sum::<usize>() > 0,
                "the clip coded no inter luma transform unit at all: {inter:?}"
            );
        } else {
            eprintln!(
                "SKIP the wide-alphabet fire count at preset {}",
                crate::speed::speed()
            );
        }
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "frame {i}: luma differs first at ({}, {}): ours {} vs ffmpeg {}",
                        at % width,
                        at / width,
                        got[at],
                        b[at],
                    );
                }
                assert_eq!(
                    got.len(),
                    b.len(),
                    "frame {i}: luma is {} samples, ffmpeg's is {}",
                    got.len(),
                    b.len()
                );
            }
        } else {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
        }
    }
    /// lane-intertx's witness: on a clip the screen detector calls
    /// NON-screen, the inter luma search offers the wider
    /// `reduced_tx_set = 0` alphabets and CODES a 1-D `V_DCT`/`H_DCT` unit
    /// (a `coded > 0` guard on the committed-winners census, class
    /// `gate-blind-to-feature`), the block's chroma transforms inherit that
    /// type reduced to the chroma size's set against the frame's own honest
    /// header bit, and BOTH decoders reconstruct every frame sample-exact on
    /// ALL THREE planes -- chroma is the plane the inheritance desync
    /// corrupts, so luma-only comparisons cannot see it.
    #[test]
    #[ignore = "sets process-global search levers: run it alone"]
    fn a_non_screen_inter_clip_codes_a_one_d_type_both_decoders_read_every_plane_exactly() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        crate::encode::set_intra_tx_search(Some(true));
        crate::encode::set_inter_tx_search(Some(true));
        crate::encode::set_wide_tx_set(Some(true));
        crate::encode::force_screen(Some(false));
        let (width, height) = (320usize, 192usize);
        // The wide witness's moving card: vertical steps (where a 1-D
        // `V_DCT`/`H_DCT` beats the 2-D DCT on the motion-compensated
        // residual), ramps (`ADST`) and moving plateaus (`IDTX`), translated
        // a few samples per frame so the inter search sees real motion.
        let sources: Vec<Picture> = (0..4)
            .map(|t| {
                let mut p = test_card(width, height, t * 3);
                for y in 0..height {
                    for x in 0..width {
                        let v = match ((x + t * 5) / 16 % 2, (y + t * 3) / 16 % 2) {
                            (0, 0) => 16u16,
                            (1, 0) => 235,
                            (0, 1) => ((x + t) % 64 * 3) as u16,
                            _ => ((y + t) % 64 * 3) as u16,
                        };
                        p.y[y * width + x] = v;
                    }
                }
                p
            })
            .collect();
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 90,
            gop: 4,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let _ = crate::encode::take_inter_tx_type_hits();
        let mut stream = Vec::new();
        for packet in encode_all(&mut enc, &sources) {
            stream.extend_from_slice(&packet.data);
        }
        let hits = crate::encode::take_inter_tx_type_hits();
        crate::encode::force_screen(None);
        crate::encode::set_wide_tx_set(None);
        crate::encode::set_inter_tx_search(None);
        crate::encode::set_intra_tx_search(None);
        use crate::transform::TxType;
        eprintln!(
            "inter wide-set witness: inter hits {hits:?}, {} bytes",
            stream.len()
        );
        assert!(
            hits[TxType::Idtx as usize] > 0,
            "no inter luma unit was CODED IDTX: {hits:?}"
        );
        assert!(
            hits[TxType::VDct as usize] + hits[TxType::HDct as usize] > 0,
            "no inter luma unit was CODED a 1-D type -- the wider search \
             never fired on non-screen content (class gate-blind-to-feature): {hits:?}"
        );
        let ours = crate::stream::decode_stream(&stream).expect("our decoder");
        assert_eq!(ours.len(), sources.len(), "our decoder's frames");
        if have_ffmpeg() {
            let theirs = ffmpeg_decode_planes(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "ffmpeg's frames");
            let (cw, ch) = (width / 2, height / 2);
            for (i, (a, (y, u, v))) in ours.iter().zip(&theirs).enumerate() {
                for (name, got16, want, w) in [
                    ("y", &a.y, y, width),
                    ("u", &a.u, u, cw),
                    ("v", &a.v, v, ch),
                ] {
                    let got: Vec<u8> = got16.iter().map(|&s| s as u8).collect();
                    assert_eq!(got.len(), want.len(), "frame {i} {name} plane size");
                    if let Some(at) = got.iter().zip(want.iter()).position(|(x, y)| x != y) {
                        panic!(
                            "frame {i}: {name} differs first at ({}, {}): ours {} vs ffmpeg {}",
                            at % w,
                            at / w,
                            got[at],
                            want[at],
                        );
                    }
                }
            }
        } else {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
        }
    }

    /// Every SHIPPED SPEED PRESET codes a stream both decoders reconstruct
    /// sample-exact -- ours and ffmpeg's -- and codes a DIFFERENT stream from
    /// its neighbour (a preset that changes no byte is a preset that buys no
    /// wall: class `gate-blind-to-feature`). Preset 0's bytes are the
    /// full-search ones, which is the cheap in-suite statement of the byte
    /// pins' rule.
    ///
    /// Ignored by default because the preset is PROCESS-GLOBAL
    /// ([`crate::speed`]): flipping it while the suite's other tests encode
    /// in parallel would move their bytes under them. Run it on its own:
    /// `cargo test -p ec-av1 --release -- --ignored every_speed_preset`.
    #[test]
    #[ignore = "sets the process-global speed preset: run it alone"]
    fn every_speed_preset_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (640usize, 384usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let mut sizes: Vec<(u8, usize)> = Vec::new();
        for speed in [0u8, 3, 6, 10] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 4,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 1,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::with_speed(config, speed).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(
                ours.len(),
                sources.len(),
                "speed {speed}: our decoder's frames"
            );
            if have_ffmpeg() {
                let theirs = ffmpeg_decode_luma(&stream, width, height);
                assert_eq!(
                    theirs.len(),
                    sources.len(),
                    "speed {speed}: ffmpeg's frames"
                );
                for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                    let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                    if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                        panic!(
                            "speed {speed} frame {i}: luma differs first at ({}, {}): \
                             ours {} vs ffmpeg {}",
                            at % width,
                            at / width,
                            got[at],
                            b[at],
                        );
                    }
                    assert_eq!(
                        got.len(),
                        b.len(),
                        "speed {speed} frame {i}: luma is {} samples, ffmpeg's is {}",
                        got.len(),
                        b.len()
                    );
                }
            } else {
                eprintln!("SKIP the ffmpeg half of speed {speed}: no ffmpeg");
            }
            eprintln!(
                "speed {speed}: {} bytes [{}]",
                stream.len(),
                crate::speed::levers(speed).join(", ")
            );
            sizes.push((speed, stream.len()));
        }
        crate::speed::set_speed(0);
        for w in sizes.windows(2) {
            assert_ne!(
                w[0].1, w[1].1,
                "speed {} and {} coded the same bytes: the preset changed no decision",
                w[0].0, w[1].0
            );
        }
    }

    /// What tiles and tile threads are worth at the size the editor exports
    /// at: 1920x1080, eight real pictures, two passes per cell (ABAB), wall
    /// per cell. Ignored by default (minutes).
    #[test]
    #[ignore = "wall table, 16 cells x 2 passes at 1080p: 27 min measured; needs a host with >=8 cores"]
    fn tile_wall_table_at_1080p() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (1920usize, 1080usize);
        let Some(sources) = h264_clip_frames("tile_wall_table_at_1080p", width, height, 8) else {
            return;
        };
        let layouts = [(0u32, 0u32), (1, 0), (1, 1), (2, 1)];
        let threadings = [1usize, 2, 4, 8];
        let mut table: Vec<(String, usize, f64, usize)> = Vec::new();
        for pass in 0..2 {
            for &(cols_log2, rows_log2) in &layouts {
                for &threads in &threadings {
                    crate::par::set_tile_threads(threads);
                    let config = EncoderConfig {
                        width,
                        height,
                        base_q_idx: 120,
                        gop: 8,
                        colour: Colour::Bt709Limited,
                        tile_cols_log2: cols_log2,
                        tile_rows_log2: rows_log2,
                    };
                    let mut enc = Av1Encoder::new(config).unwrap();
                    let start = std::time::Instant::now();
                    let mut bytes = 0usize;
                    for packet in encode_all(&mut enc, &sources) {
                        bytes += packet.data.len();
                    }
                    let wall = start.elapsed().as_secs_f64();
                    let name = format!("{}x{}", 1 << cols_log2, 1 << rows_log2);
                    match table.iter_mut().find(|r| r.0 == name && r.1 == threads) {
                        Some(row) => row.2 = row.2.min(wall),
                        None => table.push((name, threads, wall, bytes)),
                    }
                    let _ = pass;
                }
            }
        }
        crate::par::set_tile_threads(1);
        eprintln!("| tiles | threads | wall (s, best of 2) | bytes |");
        for (name, threads, wall, bytes) in &table {
            eprintln!("| {name} | {threads} | {wall:.2} | {bytes} |");
        }
    }

    /// The tiles of a frame are entropy-independent, so the bytes must not
    /// depend on how many workers wrote them: the same stream at one tile
    /// thread and at four, per layout.
    #[test]
    fn tile_bytes_do_not_depend_on_the_thread_count() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (320usize, 192usize);
        let sources: Vec<Picture> = (0..3).map(|t| test_card(width, height, t * 3)).collect();
        let coded = |threads: usize, cols_log2: u32, rows_log2: u32| {
            crate::par::set_tile_threads(threads);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 3,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            stream
        };
        for (cols_log2, rows_log2) in [(1u32, 0u32), (0, 1), (1, 1), (2, 1)] {
            let one = coded(1, cols_log2, rows_log2);
            let four = coded(4, cols_log2, rows_log2);
            assert_eq!(
                one,
                four,
                "{}x{} tiles: {} bytes at one thread, {} at four",
                1 << cols_log2,
                1 << rows_log2,
                one.len(),
                four.len()
            );
        }
        crate::par::set_tile_threads(1);
    }

    /// What the per-tile search (lane-av1tsearch) is worth in wall and in
    /// frames per second at the editor's own export sizes: every tile layout
    /// crossed with every tile-thread count, plus librav1e at the matching
    /// `tile_cols`/`tile_rows`/`threads` as the reference point. Two passes
    /// over the whole table (A,B,A,B... rather than AA,BB) with the best of
    /// the two kept, so a thermal drift cannot order the rows.
    ///
    /// Measurement, not a pass/fail gate: it asserts only that every cell
    /// produced a stream.
    #[test]
    // MEASURED 1243 s in release on a 12-core workstation. A 4-core server
    // cannot finish it at all: the same run TIMED OUT at 2400 s there
    // (12 frames x 2 passes x 12 cells), so the 8- and 12-thread rows are
    // only meaningful on a host with the cores to run them. Do not schedule
    // it on a 4-core fleet box.
    #[ignore = "wall measurement: 20 min at 1080p and it TIMES OUT on a 4-core host -- run it on >=8 cores"]
    fn tile_search_wall_1080p() {
        let _knobs = crate::speed::knob_write();
        tile_search_wall(
            1920,
            1080,
            12,
            &[(0, 0), (1, 0), (1, 1), (2, 1)],
            &[1, 2, 4, 8],
        );
    }

    /// The same table at the 4K frame size the editor exports (3840x1608),
    /// at the two layouts with enough tiles to fill this box.
    #[test]
    #[ignore = "wall measurement: 12 min at 4K (732 s measured); run it with --ignored --nocapture"]
    fn tile_search_wall_4k() {
        let _knobs = crate::speed::knob_write();
        tile_search_wall(3840, 1608, 6, &[(2, 1), (3, 2)], &[1, 8, 12]);
    }

    /// lane-av1fpar: the frame-level FILTER stage is banded across the same
    /// workers the tile search uses (`par::override_filter_threads`), so the
    /// deblock/CDEF replays, the per-64 SSE and the plane copies all run in
    /// pieces. Every piece is a disjoint band of the same integers, so the
    /// stream must not move -- at a size that crops and is several superblock
    /// rows tall, which is what makes the bands non-trivial.
    #[test]
    fn filter_stage_bytes_do_not_depend_on_the_thread_count() {
        let _knobs = crate::speed::knob_write();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let (width, height) = (384usize, 288usize);
        let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
        let coded = |threads: usize| {
            crate::par::set_tile_threads(threads);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            stream
        };
        // One tile, so only the filter stage can differ between these.
        let one = coded(1);
        for threads in [2usize, 4, 8] {
            assert_eq!(
                one,
                coded(threads),
                "the filter stage moved at {threads} threads"
            );
        }
        crate::par::set_tile_threads(1);
    }

    /// lane-av1fpar: the filter search scores its ~30 candidates by replaying
    /// the loop filters on one captured reconstruction instead of decoding
    /// the coded tile again per candidate. The replay must be that decode,
    /// byte for byte -- including at a size that CROPS (1080 and 1608 both
    /// pad to a superblock row, i.e. every export size the editor uses),
    /// which is the arm this lane opened.
    #[test]
    fn filter_replay_codes_the_same_stream_as_a_full_decode() {
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let run = |width: usize, height: usize, off: bool| {
            let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
            crate::decode::set_filter_replay_disabled(off);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            crate::decode::set_filter_replay_disabled(false);
            stream
        };
        // 320x160 crops (the coding surface is 320x192, one superblock row
        // taller); 320x192 is a whole number of superblocks, the shape the
        // replay already covered.
        for (width, height) in [(320usize, 160usize), (320, 192)] {
            let replayed = run(width, height, false);
            let decoded = run(width, height, true);
            assert_eq!(
                replayed,
                decoded,
                "{width}x{height}: the filter replay is not the decode it stands in for \
                 ({} bytes replayed, {} decoded)",
                replayed.len(),
                decoded.len(),
            );
        }
    }

    /// lane-av1cap: the filter stage's FINAL replay -- the one that replaced
    /// the capture decode -- against that decode. `set_verify_final_replay`
    /// makes `encode::pick_and_apply_filters` run both on every frame and
    /// assert, per frame, that the spliced picture and the two filter stages
    /// the loop-restoration search reads are bit-identical.
    #[test]
    fn filter_replay_final_matches_the_capture_decode() {
        let _knobs = crate::speed::knob_read();
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let run = |sources: &[Picture], width: usize, height: usize, tiles: (u32, u32)| {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 2,
                colour: Colour::Bt709Limited,
                tile_cols_log2: tiles.0,
                tile_rows_log2: tiles.1,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            for packet in encode_all(&mut enc, sources) {
                assert!(!packet.data.is_empty());
            }
        };
        crate::decode::set_verify_final_replay(true);
        // 320x160 crops (its coding surface is one superblock row taller);
        // 320x192 is a whole number of superblocks.
        for (width, height) in [(320usize, 160usize), (320, 192)] {
            let sources: Vec<Picture> = (0..4).map(|t| test_card(width, height, t * 3)).collect();
            run(&sources, width, height, (0, 0));
        }
        // Real gate content, where the CDEF preset search actually chooses
        // `bits > 0` and the tile is re-coded, one tile and four.
        if let Some(sources) = h264_clip_frames("the gate-clip arm", 640, 384, 3) {
            run(&sources, 640, 384, (0, 0));
            run(&sources, 640, 384, (1, 1));
        }
        crate::decode::set_verify_final_replay(false);
    }

    /// lane-av1fpar: where a frame's wall actually goes, stage by stage --
    /// the per-tile search, the tile write, the frame-level filter search
    /// (with its deblock/CDEF replay and per-64 SSE inside it), the capture
    /// decode the loop-restoration search rides on, and that search itself.
    /// Measurement, not a pass/fail gate.
    #[test]
    #[ignore = "wall measurement: 36 s at 1080p; run it with --ignored --nocapture"]
    fn filter_stage_wall_1080p() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(1920, 1080, 8, (2, 1), &[8]);
    }

    #[test]
    #[ignore = "wall measurement: 62 s at 4K; run it with --ignored --nocapture"]
    fn filter_stage_wall_4k() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(3840, 1608, 4, (2, 1), &[8, 12]);
    }

    /// The same breakdown at the real films' own crop size. With
    /// `EC_AV1_WALL_CLIP` pointing at a film window this is the film row; with
    /// it unset it is the colour-bar fixture at the same size, which is what
    /// makes the pair a comparison rather than a number.
    #[test]
    // MEASURED 16 s WITHOUT `EC_AV1_WALL_CLIP`, and what it measures then is
    // the COLOUR-BAR fixture, not a film: `h264_clip_frames` falls back to
    // `fixtures/video/h264-1080p-23.976-8bit.mp4` when the var is unset. A
    // reader who takes the name for the content reads a bars number as a
    // film number, so the precondition belongs in the reason.
    #[ignore = "wall measurement: 16 s; MEASURES THE BARS FIXTURE unless EC_AV1_WALL_CLIP names a real film window"]
    fn filter_stage_wall_film() {
        let _knobs = crate::speed::knob_write();
        filter_stage_wall(1920, 768, 4, (2, 1), &[8]);
    }

    fn filter_stage_wall(
        width: usize,
        height: usize,
        frames: usize,
        (cols_log2, rows_log2): (u32, u32),
        threads: &[usize],
    ) {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let Some(sources) = h264_clip_frames("the filter-stage breakdown", width, height, frames)
        else {
            return;
        };
        crate::par::set_stage_times(true);
        for &t in threads {
            crate::par::set_tile_threads(t);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let _ = crate::par::take_stage_ns();
            let start = std::time::Instant::now();
            for packet in encode_all(&mut enc, &sources) {
                assert!(!packet.data.is_empty());
            }
            let wall = start.elapsed().as_secs_f64();
            let ns = crate::par::take_stage_ns();
            let ms = |i: usize| ns[i] as f64 / 1e6;
            eprintln!(
                "\n{width}x{height}, {frames} frames, {}x{} tiles, {t} tile threads: \
                 {wall:.2}s wall, {:.2} fps",
                1 << cols_log2,
                1 << rows_log2,
                frames as f64 / wall,
            );
            eprintln!("| stage | ms | ms/frame | % of wall |");
            for (i, name) in crate::par::STAGES.iter().enumerate() {
                eprintln!(
                    "| {name} | {:.0} | {:.1} | {:.1}% |",
                    ms(i),
                    ms(i) / frames as f64,
                    100.0 * ms(i) / (wall * 1e3),
                );
            }
            // Top-level stages only (the deblock/CDEF/SSE rows are inside
            // the filter-search total); the rest is per-frame setup, the
            // source conversion and the splice.
            let top: f64 = [
                crate::par::S_TILE_SEARCH,
                crate::par::S_TILE_WRITE,
                crate::par::S_FILTER,
                crate::par::S_CAPTURE,
                crate::par::S_LR,
            ]
            .iter()
            .map(|&i| ms(i))
            .sum();
            eprintln!(
                "accounted {:.1}% of wall; outside the tile search {:.1}%",
                100.0 * top / (wall * 1e3),
                100.0 * (1.0 - ms(crate::par::S_TILE_SEARCH) / (wall * 1e3)),
            );
        }
        crate::par::set_stage_times(false);
        crate::par::set_tile_threads(1);
    }

    fn tile_search_wall(
        width: usize,
        height: usize,
        frames: usize,
        layouts: &[(u32, u32)],
        threads: &[usize],
    ) {
        let _gate_lock = crate::stream::tests::lock_gate_counters();
        let Some(sources) = h264_clip_frames("the tile-search wall table", width, height, frames)
        else {
            return;
        };
        let run = |cols_log2: u32, rows_log2: u32, t: usize| -> (std::time::Duration, usize) {
            crate::par::set_tile_threads(t);
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: frames,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let mut enc = Av1Encoder::new(config).unwrap();
            let start = std::time::Instant::now();
            let mut bytes = 0usize;
            for packet in encode_all(&mut enc, &sources) {
                bytes += packet.data.len();
            }
            (start.elapsed(), bytes)
        };
        let cells: Vec<((u32, u32), usize)> = layouts
            .iter()
            .flat_map(|&l| threads.iter().map(move |&t| (l, t)))
            .collect();
        let mut best: Vec<Option<(std::time::Duration, usize)>> = vec![None; cells.len()];
        for _pass in 0..2 {
            for (i, &((c, r), t)) in cells.iter().enumerate() {
                let got = run(c, r, t);
                assert!(
                    got.1 > 0,
                    "{}x{} tiles at {t} threads: empty stream",
                    1 << c,
                    1 << r
                );
                if best[i].is_none_or(|(w, _)| got.0 < w) {
                    best[i] = Some(got);
                }
            }
        }
        crate::par::set_tile_threads(1);
        eprintln!("\n{width}x{height}, {frames} frames, base_q_idx 120 -- best of two passes");
        eprintln!("| layout | tiles | threads | wall | fps | speedup vs 1 tile 1 thread | bytes |");
        let base = best[0].expect("the first cell ran").0.as_secs_f64();
        for (i, &((c, r), t)) in cells.iter().enumerate() {
            let (wall, bytes) = best[i].expect("every cell ran");
            let secs = wall.as_secs_f64();
            eprintln!(
                "| {}x{} | {} | {t} | {secs:.2}s | {:.2} | {:.2}x | {bytes} |",
                1 << c,
                1 << r,
                1 << (c + r),
                frames as f64 / secs,
                base / secs,
            );
        }
        // Parallel efficiency per layout: what the extra threads actually
        // took off that layout's own single-threaded wall, and so what share
        // of the added cores sat idle.
        for &(c, r) in layouts {
            let one = cells
                .iter()
                .position(|&(l, t)| l == (c, r) && t == threads[0])
                .map(|i| best[i].expect("cell").0.as_secs_f64());
            let Some(one) = one else { continue };
            for &t in &threads[1..] {
                let i = cells
                    .iter()
                    .position(|&(l, tt)| l == (c, r) && tt == t)
                    .expect("cell");
                let secs = best[i].expect("cell").0.as_secs_f64();
                let workers = (t.min(1 << (c + r))) as f64 / threads[0] as f64;
                let speedup = one / secs;
                eprintln!(
                    "{}x{} tiles, {} -> {t} threads: {speedup:.2}x of a possible {workers:.2}x, \
                     idle share {:.0}%",
                    1 << c,
                    1 << r,
                    threads[0],
                    100.0 * (1.0 - speedup / workers).max(0.0),
                );
            }
        }
        rav1e_wall_reference(width, height, frames, layouts, threads);
    }

    /// librav1e at the same sizes, layouts and thread counts, through ffmpeg
    /// -- the reference the wall table is read against.
    fn rav1e_wall_reference(
        width: usize,
        height: usize,
        frames: usize,
        layouts: &[(u32, u32)],
        threads: &[usize],
    ) {
        let listed = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"]), None, |c| c.output());
        let has_rav1e = listed
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("librav1e"))
            .unwrap_or(false);
        // lane-av1clipprobe: this was `!has_rav1e || !clip.exists()` behind ONE
        // line, which is how the clip's absence hid inside a tool-absence
        // message and could not fail under any require env. The two causes are
        // now separate, and only the clip goes through the fixture probe, so
        // either way this helper prints exactly ONE line.
        if !has_rav1e {
            eprintln!("SKIP the rav1e reference: ffmpeg has no librav1e encoder");
            return;
        }
        let Some(clip) = crate::library_fixture::require(
            "video/h264-1080p-23.976-8bit.mp4",
            "the rav1e reference",
        ) else {
            return;
        };
        eprintln!("| rav1e layout | threads | wall | fps | bytes |");
        for &(c, r) in layouts {
            for &t in threads {
                let out =
                    std::env::temp_dir().join(format!("ec-av1-rav1e-{}.ivf", std::process::id()));
                let params = format!(
                    "speed=6:quantizer=100:tile_cols={}:tile_rows={}:threads={t}",
                    1 << c,
                    1 << r
                );
                let start = std::time::Instant::now();
                let status = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-i", clip.to_str().unwrap()])
                    .args(["-frames:v", &frames.to_string()])
                    .args(["-vf", &format!("scale={width}:{height}")])
                    .args(["-c:v", "librav1e", "-rav1e-params", &params])
                    .arg(out.to_str().unwrap()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                let secs = start.elapsed().as_secs_f64();
                if !status.status.success() {
                    eprintln!(
                        "| {}x{} | {t} | FAILED: {} |",
                        1 << c,
                        1 << r,
                        String::from_utf8_lossy(&status.stderr)
                            .lines()
                            .last()
                            .unwrap_or("")
                    );
                    continue;
                }
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(&out);
                eprintln!(
                    "| {}x{} | {t} | {secs:.2}s | {:.2} | {bytes} |",
                    1 << c,
                    1 << r,
                    frames as f64 / secs
                );
            }
        }
    }

    /// The same round trip at a real 1920x1080 crop of the gate's own clip
    /// -- the size the editor's export actually runs at, where a tile grid
    /// is worth having.
    ///
    /// UN-IGNORED by lane-av1probes: it was `#[ignore]`d only for its wall
    /// ("1080p encode: minutes"), and it is the only 1080p arm in the ignored
    /// set that actually ASSERTS. Measured 30.6 s in release on the
    /// workstation (12 cores) and 64 s on a 4-core server, so it is affordable
    /// in the suite. Evidence for the un-ignore: the three tile grids 2x1,
    /// 2x2 and 4x2 each decode sample-exact through BOTH our decoder and
    /// ffmpeg (84488 / 84208 / 84685 bytes), and the check is a per-frame
    /// first-differing-luma-sample comparison, not a length or non-empty
    /// smoke.
    ///
    /// The two skippable halves both route through the crate's EXISTING
    /// presence probes, not a new shape: `h264_clip_frames` (which itself asks
    /// `have_ffmpeg()` and `library_fixture::require_at`) and `have_ffmpeg()`
    /// for the ffmpeg arm. No bare presence check was added.
    ///
    /// lane-av1clipprobe: with the root `fixtures/` absent this gate printed
    /// `SKIP the 1080p tile round trip: no fixture` and libtest reported `ok`
    /// — a 1080p gate that asserts nothing on every linked worktree, and no
    /// env that made it RED. `library_fixture::require_at` now fails under
    /// `EC_REQUIRE_FIXTURES` / `EC_AV1_REQUIRE_FFMPEG` / `EC_AV1_REQUIRE_AOMENC`
    /// and otherwise prints exactly ONE line naming the resolved path, and the
    /// path it resolves honours `EC_FIXTURES` — the root
    /// `scripts/verify-fixture-library.sh` validates — so the preflight's
    /// verdict and these bytes cannot be different trees again.
    #[test]
    fn a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_read();
        let (width, height) = (1920usize, 1080usize);
        let Some(sources) = h264_clip_frames(
            "a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders",
            width,
            height,
            3,
        ) else {
            return;
        };
        for (cols_log2, rows_log2) in [(1u32, 0u32), (1, 1), (2, 1)] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 120,
                gop: 3,
                colour: Colour::Bt709Limited,
                tile_cols_log2: cols_log2,
                tile_rows_log2: rows_log2,
            };
            let layout = format!("{}x{} tiles", 1 << cols_log2, 1 << rows_log2);
            let mut enc = Av1Encoder::new(config).unwrap();
            let mut stream = Vec::new();
            for packet in encode_all(&mut enc, &sources) {
                stream.extend_from_slice(&packet.data);
            }
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(ours.len(), sources.len(), "{layout}: our decoder's frames");
            if !have_ffmpeg() {
                eprintln!("SKIP the ffmpeg half of 1080p {layout}: no ffmpeg");
                continue;
            }
            let theirs = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(theirs.len(), sources.len(), "{layout}: ffmpeg's frames");
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let got: Vec<u8> = a.y.iter().map(|&v| v as u8).collect();
                if let Some(at) = got.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "1080p {layout} frame {i}: luma differs first at ({}, {})",
                        at % width,
                        at / width
                    );
                }
                assert_eq!(
                    got.len(),
                    b.len(),
                    "1080p {layout} frame {i}: luma is {} samples, ffmpeg's is {}",
                    got.len(),
                    b.len()
                );
            }
            eprintln!("1080p {layout}: {} bytes, sample-exact", stream.len());
        }
    }

    /// [`RateTarget::Quality`] is monotone: a higher quality dial never
    /// produces a smaller stream or a worse mean PSNR than a lower one, on
    /// the same real clip.
    #[test]
    fn quality_target_is_monotone_in_bytes_and_psnr() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP quality_target_is_monotone_in_bytes_and_psnr: no ffmpeg");
            return;
        }
        let (width, height) = (640, 384);
        let Some(pictures) = h264_clip_frames(
            "quality_target_is_monotone_in_bytes_and_psnr",
            width,
            height,
            8,
        ) else {
            return;
        };
        let mut prev_bytes = 0usize;
        let mut prev_psnr = 0.0f64;
        for quality in [20u8, 50, 80] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 100,
                gop: 8,
                colour: Colour::Bt709Limited,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let mut enc =
                Av1Encoder::with_rate_target(config, RateTarget::Quality(quality)).unwrap();
            let mut stream = Vec::new();
            let mut total_bytes = 0usize;
            for packet in encode_all(&mut enc, &pictures) {
                total_bytes += packet.data.len();
                stream.extend_from_slice(&packet.data);
            }
            let decoded = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(
                decoded.len(),
                pictures.len(),
                "quality {quality}: dav1d frame count"
            );
            let mean_psnr: f64 = decoded
                .iter()
                .zip(&pictures)
                .map(|(d, p)| {
                    let d16: Vec<u16> = d.iter().map(|&v| u16::from(v)).collect();
                    psnr(&d16, &p.y)
                })
                .sum::<f64>()
                / decoded.len() as f64;
            assert!(
                total_bytes >= prev_bytes,
                "quality {quality}: {total_bytes} bytes not >= previous {prev_bytes}"
            );
            assert!(
                mean_psnr >= prev_psnr - 0.01, // rounding slack, same-ish q_idx neighbours
                "quality {quality}: {mean_psnr:.2} dB not >= previous {prev_psnr:.2} dB"
            );
            prev_bytes = total_bytes;
            prev_psnr = mean_psnr;
        }
    }
