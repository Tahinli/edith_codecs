use super::*;
use super::support::*;
use super::gates_a::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// As [`a_real_libaom_gradients_stream_decodes_pixel_exact`], but leaves
    /// CDEF *on* (no `-enable-cdef 0`) -- the whole point of `lane-av1cdef`:
    /// proves `apply_cdef` itself, not just that this decoder still works
    /// when the filter never fires. Gradient content at a low crf reliably
    /// gives libaom something to dering.
    ///
    /// lane-av1skipfix: the attempt-loop shape. A single-SB 64x64 frame can
    /// never make libaom write `cdef_bits > 0` (nothing to differentiate per
    /// superblock), so this recipe is MULTI-SB and MULTI-FRAME, and it sweeps
    /// (size, cpu-used, crf) until one attempt reads a real `cdef_idx` literal
    /// -- MEASURED 2026-09-29 on `a21f3680`: 64x64 at every crf/cpu reads
    /// ZERO `cdef_idx` (single superblock, `bits == 0`, though `apply_cdef`
    /// does filter with a nonzero strength pair), while 128x64/128x128 at
    /// 24 frames read 2-8. Every attempt that decodes is ALWAYS pixel-
    /// compared against ffmpeg's own decode of the identical bytes; a decode
    /// error is a named refusal recorded per attempt, never a SKIP, and
    /// exhausting every attempt -- or decoding without ever reading a
    /// `cdef_idx` -- is a hard FAILURE (an unfired gate is a vacuous gate,
    /// class `gate-skips-on-its-own-failure`).
    #[test]
    fn a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact() {
        const NAME: &str = "a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // (width, height, frames, cpu-used, crf). The frame count is what
        // gives libaom's cdef search more than one superblock to price.
        const RECIPES: [(usize, usize, usize, &str, u32); 8] = [
            (128, 64, 24, "0", 20),
            (128, 64, 24, "2", 20),
            (128, 64, 24, "0", 16),
            (128, 128, 24, "2", 20),
            (128, 128, 24, "0", 16),
            (128, 128, 24, "0", 24),
            (128, 64, 24, "2", 24),
            (128, 128, 24, "2", 24),
        ];
        let mut refusals = Vec::new();
        let mut counted_exact = 0u32;
        let mut uncounted_exact = 0u32;
        for (w, h, frames, cpu, crf) in RECIPES {
            let Some(stream) = libaom_encode_with(
                &gradients_source(42, w, h, "rate=25"),
                w,
                h,
                crf,
                &[
                    "-cpu-used",
                    cpu,
                    "-frames:v",
                    &frames.to_string(),
                    "-threads",
                    "1",
                    "-row-mt",
                    "0",
                    "-tile-columns",
                    "0",
                    "-tile-rows",
                    "0",
                ],
            ) else {
                refusals.push(format!(
                    "{w}x{h} cpu-used {cpu} crf {crf}: ffmpeg itself refused"
                ));
                continue;
            };
            let before = decode::cdef_idx_hits();
            let decoded = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("{w}x{h} cpu-used {cpu} crf {crf}: {e}"));
                    continue;
                }
            };
            // A decode that succeeded is ALWAYS pixel-compared; the hit
            // counter only decides whether the attempt counts as firing
            // (class `counter-from-refused-stream`).
            let counted = decode::cdef_idx_hits() != before;
            if counted {
                counted_exact += 1;
            } else {
                uncounted_exact += 1;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, w, h, frames);
            assert_eq!(decoded.len(), frames);
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg ({w}x{h} cpu-used {cpu} crf {crf}, \
                     cdef_idx fired: {counted})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U vs ffmpeg ({w}x{h} cpu-used {cpu} crf {crf}, \
                     cdef_idx fired: {counted})"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V vs ffmpeg ({w}x{h} cpu-used {cpu} crf {crf}, \
                     cdef_idx fired: {counted})"
                );
            }
            if counted_exact >= 2 {
                break;
            }
        }
        gate_buckets(NAME, counted_exact, uncounted_exact, refusals.len());
        assert!(
            counted_exact > 0,
            "{NAME}: no attempt of {} read a cdef_idx literal -- CDEF is not exercised \
             (class gate-blind-to-feature):\n{}",
            RECIPES.len(),
            refusals.join("\n")
        );
    }

    /// lane-av1422h232: the non-vacuity test for [`count_rawvideo_diffs`].
    ///
    /// A cell-classifying counter that cannot report a difference is worse
    /// than no counter, because it reports "exact" for everything -- that is
    /// exactly how lane-av1422remeasure's "18/18 byte-exact" table was
    /// produced (it compared our own packed samples against the same
    /// samples, never reading `aomdec`'s output).
    ///
    /// It used to lean on the pinned witness DIFFERING to prove the counting
    /// path fires. That witness is byte-exact now (lane-av1422hrtx fixed the
    /// 32-point inter chroma `tx_type`), so "it differs" is no longer
    /// available and a witness-based version of this test would be vacuous by
    /// construction. Instead this proves the same property directly and
    /// unconditionally: flipping ONE bit of the ORACLE's own bytes must move
    /// the count by exactly one, in the plane that bit is in, and knock exactly
    /// one frame out of `frames_exact`. A comparator that had stopped reading
    /// the oracle reports the baseline numbers again and fails by name.
    #[test]
    fn the_counting_oracle_diff_detects_one_flipped_oracle_byte() {
        const NAME: &str = "the_counting_oracle_diff_detects_one_flipped_oracle_byte";
        // lane-av1cmpaudit2: same mutex as every other oracle gate -- this
        // one reads `aomdec_path()`, so it must not overlap a sibling's
        // tampered-oracle run.
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("420_oddheight_320x232.obu");
        let data = std::fs::read(&path).expect("the pinned witness is missing");
        let (wy, wu, wv, frames, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv, exact),
            (0, 0, 0, frames),
            "{NAME}: the pinned witness must decode byte-exactly against `aomdec --rawvideo`"
        );
        // Frame 0 byte 0 is luma; byte 90_000 is inside frame 0's chroma.
        for (idx, plane) in [(0usize, 0usize), (90_000, 1)] {
            let (wy, wu, wv, _, exact) = count_rawvideo_diffs(&data, NAME, Some(idx))
                .expect("the two sides must be byte-comparable");
            let got = [wy, wu, wv];
            assert_eq!(
                got.iter().sum::<usize>(),
                1,
                "{NAME}: flipping oracle byte {idx} must report exactly ONE wrong sample \
                 across all planes -- got {got:?}. Reporting the unflipped numbers means \
                 the comparison stopped reading the oracle's bytes"
            );
            assert_eq!(got[plane], 1, "{NAME}: byte {idx} is in plane {plane}");
            assert_eq!(
                exact,
                frames - 1,
                "{NAME}: one flipped byte spoils one frame"
            );
        }
        eprintln!("{NAME}: baseline exact, each flipped oracle byte reported as exactly 1");
    }

    /// lane-av1cmpframe: the control for the class
    /// `comparator-that-attributes-the-wrong-plane`.
    ///
    /// [`count_rawvideo_diffs`] used to derive its plane split from the FIRST
    /// decoded picture and compare every byte index against those frame-0
    /// thresholds. On a multi-frame stream that attributes every sample after
    /// frame 0's luma to the LAST plane: on the pinned 320x236 stream below
    /// (16 frames of 320x236 4:2:0, 113 280 bytes per frame, so plane V holds
    /// 302 080 bytes in total) it reported `wrong_v = 330 673` -- more wrong V
    /// bytes than exist. The per-frame totals and the frame-exact count were
    /// sound, so the defect was invisible to every gate that only asserted
    /// zeros; it corrupted exactly the numbers a per-plane report exists to
    /// give.
    ///
    /// Three arms, each of which the old attribution FAILS:
    ///
    /// 1. **Arithmetic.** Each plane's count must be at most that plane's own
    ///    byte capacity summed over frames, and the three must sum to the
    ///    independently computed total. The old code reported 330 673 wrong V
    ///    bytes in a 302 080-byte plane.
    /// 2. **A later frame's U.** Flipping one byte inside frame 5's U plane
    ///    must move U by exactly one and leave Y and V at their baseline.
    ///    The old code has no frame-5 U at all -- byte 600 000 is past every
    ///    frame-0 threshold, so it landed in V.
    /// 3. **Frame attribution.** `frames_exact` must equal the number of
    ///    frames this test counts itself, per frame, from the decoded
    ///    pictures' own plane lengths.
    ///
    /// The pin is asserted only for IDENTITY (bytes + fnv1a64) and for
    /// comparability. Whether the stream currently diverges is deliberately
    /// NOT asserted: this is a control for the INSTRUMENT, and it must keep
    /// biting after the odd-height entropy fork is fixed.
    #[test]
    fn the_counting_oracle_diff_attributes_planes_per_frame() {
        const NAME: &str = "the_counting_oracle_diff_attributes_planes_per_frame";
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_oddheight_320x236.obu";
        const BYTES: usize = 16562;
        // sha256 84e4d1ab56620af1c5b78e1aa3d2d67496a492f2e127198c82dd1d68b01c6200.
        // The tree pins fixture identity by fnv1a64; a `const SHA256`
        // compared against itself would be the tautology this lane exists to
        // kill, so it is not written.
        const FNV1A64: u64 = 521889434652397870;
        let data = crate_pin("420_oddheight_320x236.obu");
        let data = std::fs::read(&data)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        let bit_depth = stream_bit_depth(&data, NAME);
        let bpp = if bit_depth == 8 { 1 } else { 2 };

        // ---- this test's own reading of the oracle, from scratch ---------
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &data).expect("writing the stream");
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
        let want = std::fs::read(&raw).expect("aomdec rawvideo output");
        let _ = std::fs::remove_dir_all(&dir);
        let decoded = decode_stream(&data).expect("the pinned stream decodes");
        let ours = pack_rawvideo(&decoded, bit_depth, NAME);
        assert_eq!(
            ours.len(),
            want.len(),
            "{NAME}: the two sides differ in size"
        );

        // The per-frame plane table, built from each decoded picture's OWN
        // plane lengths -- the same source `pack_rawvideo` lays the bytes out
        // from, and never a formula.
        let mut spans: Vec<[usize; 4]> = Vec::with_capacity(decoded.len());
        let mut capacity = [0usize; 3];
        let mut off = 0usize;
        for f in &decoded {
            let (y, u, v) = (f.y.len() * bpp, f.u.len() * bpp, f.v.len() * bpp);
            spans.push([off, off + y, off + y + u, off + y + u + v]);
            capacity[0] += y;
            capacity[1] += u;
            capacity[2] += v;
            off += y + u + v;
        }
        assert_eq!(off, ours.len(), "{NAME}: the plane table covers the buffer");

        // The truth, counted here with no help from the helper under test.
        let mut truth = [0usize; 3];
        let mut truth_exact = 0usize;
        for s in &spans {
            let mut frame_exact = true;
            for (p, a, b) in [(0usize, s[0], s[1]), (1, s[1], s[2]), (2, s[2], s[3])] {
                let n = ours[a..b]
                    .iter()
                    .zip(&want[a..b])
                    .filter(|(x, y)| x != y)
                    .count();
                truth[p] += n;
                frame_exact &= n == 0;
            }
            truth_exact += usize::from(frame_exact);
        }

        let (wy, wu, wv, frames, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        eprintln!(
            "{NAME}: pinned {FILE} as wrongY={wy} wrongU={wu} wrongV={wv} over {frames} frames, \
             {exact} exact; this test's own count {truth:?} with {truth_exact} exact frames; \
             plane capacities {capacity:?}"
        );
        assert_eq!(frames, decoded.len(), "{NAME}: frame count");
        assert_eq!(
            [wy, wu, wv],
            truth,
            "{NAME}: the helper's per-plane split must equal an independent per-frame count"
        );
        assert_eq!(exact, truth_exact, "{NAME}: frames_exact");
        for p in 0..3 {
            assert!(
                truth[p] <= capacity[p],
                "{NAME}: plane {p} counts {} wrong bytes but holds only {}",
                truth[p],
                capacity[p]
            );
        }

        // ---- arm 2: a byte inside a LATER frame's U plane. --------------
        // Frame 5's U plane's first byte. The old first-picture attribution
        // has no frame-5 U at all: that byte is past every frame-0 threshold,
        // so it landed in V.
        let u5 = spans[5][1];
        let (fy, fu, fv, fframes, fexact) = count_rawvideo_diffs(&data, NAME, Some(u5))
            .expect("the two sides must be byte-comparable");
        let flipped = [fy, fu, fv];
        assert_eq!(fframes, frames, "{NAME}: frame count under the flip");
        assert_eq!(
            flipped.iter().sum::<usize>(),
            truth.iter().sum::<usize>() + 1,
            "{NAME}: flipping one byte inside frame 5's U plane must add exactly one wrong \
             byte -- got {flipped:?} against this test's own count {truth:?}"
        );
        assert_eq!(
            flipped[1],
            truth[1] + 1,
            "{NAME}: byte {u5} is inside frame 5's U plane"
        );
        assert_eq!(flipped[0], truth[0], "{NAME}: byte {u5} is not in luma");
        assert_eq!(
            flipped[2], truth[2],
            "{NAME}: byte {u5} must NOT be attributed to V -- the first-picture attribution \
             puts every byte past frame 0's luma in V"
        );
        let want_exact = if truth_exact < frames {
            truth_exact
        } else {
            frames - 1
        };
        assert_eq!(
            fexact, want_exact,
            "{NAME}: one flipped byte inside frame 5 leaves frames_exact at {want_exact}"
        );
    }

    /// lane-av1cmpaudit2: the permanent CONTROL for the class
    /// `comparator-that-never-reads-the-oracle`.
    ///
    /// The retracted predecessor lane's per-cell counter reported "0 wrong"
    /// on 18/18 cells -- including cells that differ from `aomdec
    /// --rawvideo` in 17658 samples -- because it packed OUR decoded samples
    /// into a buffer and compared that buffer against the same samples.
    /// `aomdec`'s output was written, length-checked, and never read.
    /// Nothing about that mistake is visible from the outside: the helper's
    /// signature, its panic text and its return type are all exactly what a
    /// working comparator has, and no amount of reading the source of a
    /// healthy-looking helper proves the next one is healthy.
    ///
    /// So the class is pinned here from the other direction.
    /// `EC_AV1_AOMDEC` is already the crate's oracle override, so this test
    /// points every oracle helper at a shim that runs the REAL `aomdec` and
    /// then corrupts exactly ONE byte of whatever it wrote -- the
    /// `--rawvideo` output and the `EC_AV1_FINAL_DUMP` frame 0 -- leaving
    /// every other sample a genuine decode. On a pin that IS byte-exact
    /// against the real oracle:
    ///
    /// 1. **arm 1, real oracle: every helper is GREEN.** Without this arm,
    ///    arm 2's red could just be a stream that was never exact.
    /// 2. **arm 2, one-byte-wrong oracle: every helper is RED.** A helper
    ///    that stays green here is the retracted defect itself: it cannot
    ///    fail, so its green is worth nothing.
    ///
    /// One test, five helpers, and it goes red for the exact mistake that
    /// was retracted. Measured over this file: the other 62 tests that
    /// reach an oracle helper all take `lock_gate_counters()` before their
    /// first oracle call, which is what makes the process-global
    /// `EC_AV1_AOMDEC` safe to flip here.
    #[test]
    fn every_oracle_comparator_reds_on_a_one_byte_wrong_oracle() {
        const NAME: &str = "every_oracle_comparator_reds_on_a_one_byte_wrong_oracle";
        let _gate_lock = lock_gate_counters();
        if !aomdec_available(NAME) {
            return;
        }
        let pin = crate_pin("420_odd65x65_key.obu");
        let stream = std::fs::read(&pin).unwrap_or_else(|e| {
            panic!(
                "{NAME}: the control's pin {} is missing ({e}) -- restore the committed copy",
                pin.display()
            )
        });
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            8,
            "{NAME}: the control's pin must be the 8-bit cell the shim corrupts"
        );
        let plane_samples = {
            let frames = decode_stream(&stream).expect("the control's pin must decode");
            let f = &frames[0];
            [f.y.len(), f.u.len(), f.v.len()]
        };

        // ---- arm 1: the real oracle, every comparator green ------------
        {
            let obu = std::env::temp_dir().join(format!("{NAME}-arm1.obu"));
            std::fs::write(&obu, &stream).expect("writing the pin for arm 1");
            assert_rawvideo_matches(&obu, &stream, "oracle-control-arm1-rawvideo", 1, 8);
            let _ = std::fs::remove_file(&obu);
            let (frames, hidden) =
                decode_all_frames_vs_oracle(&stream, "oracle-control-arm1-alldumps");
            assert_eq!(
                (frames, hidden),
                (1, 0),
                "{NAME}: arm 1 expects one decode-order frame and no hidden frames"
            );
            let (wy, wu, wv, n, exact) =
                count_rawvideo_diffs(&stream, "oracle-control-arm1-count", None)
                    .expect("arm 1: the two sides must be byte-comparable");
            assert_eq!(
                (wy, wu, wv),
                (0, 0, 0),
                "{NAME}: arm 1 runs against the REAL oracle, so every plane must be zero"
            );
            assert_eq!(
                (n, exact),
                (1, 1),
                "{NAME}: arm 1 expects one shown frame that is byte-identical"
            );
            let (differing, first) = key_frame_diffs(&stream, "oracle-control-arm1-kf");
            assert_eq!(
                (differing, first),
                (0, None),
                "{NAME}: arm 1's key frame must be byte-identical to the oracle's"
            );
            let [y, u, v] =
                oracle_plane_diffs(&stream, "oracle-control-arm1-planes", plane_samples);
            assert_eq!(
                [y, u, v],
                [0, 0, 0],
                "{NAME}: arm 1 must be zero on every plane against the real oracle"
            );
        }

        // ---- the one-byte-wrong oracle ---------------------------------
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let shim = dir.join("one-byte-wrong-aomdec.sh");
        // The real oracle's path rides in a sibling file rather than inside
        // the script, so a path with a quote or a space cannot mangle it and
        // the script body needs no `format!` escaping at all.
        std::fs::write(
            dir.join("real-aomdec"),
            aomdec_path().as_os_str().as_encoded_bytes(),
        )
        .expect("recording the real oracle's path for the shim");
        // Byte 7 of both the `--rawvideo` output and the `EC_AV1_FINAL_DUMP`
        // frame 0 is inside the Y plane, and its value is rotated by 128 so
        // the write always changes it. Nothing else is touched: the stream,
        // the geometry, the frame count and every other sample are a genuine
        // `aomdec` decode.
        let script = r#"#!/bin/sh
REAL=$(cat "$(dirname "$0")/real-aomdec")
rawvideo=0
out=""
prev=""
for a in "$@"; do
  if [ "$a" = "--rawvideo" ]; then rawvideo=1; fi
  if [ "$prev" = "-o" ]; then out="$a"; fi
  prev="$a"
done
"$REAL" "$@" || exit $?
flip() {
  [ -s "$1" ] || return 0
  b=$(od -An -tu1 -j 7 -N 1 "$1" 2>/dev/null | tr -d ' \n')
  [ -n "$b" ] || return 0
  esc=$(printf '\\%03o' $(( (b + 128) % 256 )))
  printf "$esc" | dd of="$1" bs=1 seek=7 count=1 conv=notrunc 2>/dev/null
  return 0
}
[ "$rawvideo" = 1 ] && flip "$out"
[ -n "$EC_AV1_FINAL_DUMP" ] && flip "$EC_AV1_FINAL_DUMP.f0"
exit 0
"#;
        std::fs::write(&shim, script).expect("writing the oracle shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("the oracle shim must be executable");
        }

        // ---- arm 2: same pin, oracle off by ONE byte -------------------
        // The override is THREAD-LOCAL, so the tampered oracle is confined
        // to this thread and no sibling test can see it; it is still cleared
        // before any result is asserted, so a red arm cannot leave this
        // test's own later work reading a wrong oracle.
        let reds: [bool; 5];
        {
            set_oracle_path(Some(shim.clone()));
            let panicked =
                |f: &dyn Fn()| std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err();
            let rawvideo_red = panicked(&|| {
                let obu = std::env::temp_dir().join(format!("{NAME}-arm2.obu"));
                std::fs::write(&obu, &stream).expect("writing the pin for arm 2");
                assert_rawvideo_matches(&obu, &stream, "oracle-control-arm2-rawvideo", 1, 8);
            });
            let _ = std::fs::remove_file(std::env::temp_dir().join(format!("{NAME}-arm2.obu")));
            let dumps_red = panicked(&|| {
                decode_all_frames_vs_oracle(&stream, "oracle-control-arm2-alldumps");
            });
            let count_red = {
                let counted = count_rawvideo_diffs(&stream, "oracle-control-arm2-count", None)
                    .unwrap_or_else(|| {
                        panic!(
                            "{NAME}: a single flipped oracle byte must NOT change the buffer \
                             LENGTH -- returning None means the shim corrupted more than the \
                             comparison"
                        )
                    });
                counted.0 + counted.1 + counted.2 > 0 && counted.4 < counted.3
            };
            let kf_red = {
                let (differing, first) = key_frame_diffs(&stream, "oracle-control-arm2-kf");
                differing > 0 && first.is_some()
            };
            let planes_red = {
                let [y, u, v] =
                    oracle_plane_diffs(&stream, "oracle-control-arm2-planes", plane_samples);
                y + u + v > 0
            };
            set_oracle_path(None);
            let _ = std::fs::remove_dir_all(&dir);
            reds = [rawvideo_red, dumps_red, count_red, kf_red, planes_red];
        }
        // One assert, naming EVERY comparator that stayed green: a partial
        // run would report the first and hide whether the other four are
        // covered.
        let blind: Vec<&str> = [
            ("assert_rawvideo_matches", reds[0]),
            ("decode_all_frames_vs_oracle", reds[1]),
            ("count_rawvideo_diffs", reds[2]),
            ("key_frame_diffs", reds[3]),
            ("oracle_plane_diffs", reds[4]),
        ]
        .into_iter()
        .filter(|(_, red)| !red)
        .map(|(what, _)| what)
        .collect();
        assert!(
            blind.is_empty(),
            "{NAME}: {} of 5 oracle comparators stayed GREEN against an oracle that is wrong \
             by exactly one byte -- {blind:?} -- they cannot fail, so their green proves \
             nothing (class `comparator-that-never-reads-the-oracle`)",
            blind.len()
        );
        eprintln!("{NAME}: all five oracle comparators red on a one-byte-wrong oracle");
    }

    /// lane-film10fix: the permanent CONTROL for the ffmpeg half of the class
    /// `comparator-that-never-reads-the-oracle`.
    ///
    /// Its sibling above covers the five `aomdec` comparators. The `ffmpeg`
    /// half had no such control, and it is the half the two 10-bit FILM gates
    /// rest on: `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_...`
    /// and `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_...`
    /// both declare "pixel-exact" purely as
    /// `assert_eq!(ours.<plane>, ffmpeg_decode_sequence_10bit(...).<plane>)`
    /// over 15 and 33 shown frames. If `ffmpeg_decode_sequence_10bit` ever
    /// stopped reading ffmpeg's stdout -- packed our own samples into the
    /// buffer it returns, say -- both gates would report exact on a decoder
    /// that could be arbitrarily wrong, and nothing in the crate would go
    /// red. Reading the helper's source proves it is live TODAY; only a
    /// deliberately wrong reference proves it can FAIL.
    ///
    /// So this is the same two-arm shape, pointed at ffmpeg. The shim runs the
    /// REAL ffmpeg and then rotates ONE byte of its rawvideo stdout per
    /// offset in a sibling file, leaving the stream, the geometry, the frame
    /// count and every other sample a genuine decode. On a pin that IS
    /// byte-exact against real ffmpeg:
    ///
    /// 1. **arm 1, real ffmpeg: every helper is GREEN.** Without this arm,
    ///    arm 2's red could just be a stream that was never exact.
    /// 2. **arm 2, one-sample-wrong ffmpeg, PER PLANE: every helper is RED
    ///    and the red names the plane that was tampered with.** Three arms,
    ///    one per plane, because a helper that only ever compared luma would
    ///    pass the Y arm and fail U and V -- and a 10-bit film gate that
    ///    reported "exact" while ignoring chroma entirely would be exactly
    ///    the class this controls.
    ///
    /// The three helpers are the whole reference surface in this module:
    /// `ffmpeg_decode_sequence` (4:2:0 8-bit), `ffmpeg_decode_gray_sequence`
    /// (monochrome, one plane) and `ffmpeg_decode_sequence_10bit` (the
    /// `yuv420p10le` family both film gates use). Each is driven on a pin it
    /// is known to be exact against: the 8-bit `420_odd65x65_key.obu` for the
    /// first, the same stream for the third's shape arithmetic, and the
    /// monochrome recipe the mono gate already builds for the second. The
    /// override is thread-local, so a red arm cannot leave a tampered ffmpeg
    /// behind for a sibling test on another thread.
    #[test]
    fn every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg() {
        const NAME: &str = "every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // Same mutex the oracle control takes: this test's shim is
        // thread-local, but the SCREAM it produces must not interleave with a
        // sibling's decode in the captured log.
        let _gate_lock = lock_gate_counters();

        // The 4:2:0 8-bit arm. It BUILDS its stream with the same
        // `libaom-av1` lavfi recipe the mono gate below uses rather than
        // pinning one: `ffmpeg_decode_sequence` derives its frame size as
        // `w*h + 2*(w*h/4)`, which is only right for EVEN dimensions -- on a
        // 65x65 stream 4:2:0 chroma is ceil(65/2)^2 = 1089, not 4225/4 =
        // 1056, and the helper's length assert fires (loudly, by design --
        // see `frame_count_diagnosis`). 320x240 keeps the offsets below
        // exact: luma 76800 samples, chroma 160x120 = 19200 each.
        let (w, h) = (320usize, 240usize);
        let (ys, cs) = (w * h, (w / 2) * (h / 2));
        assert_eq!(
            (ys, cs),
            (76800, 19200),
            "{NAME}: the byte offsets in this test are computed from this geometry"
        );
        let build_8bit = {
            let out = crate::tool_cache::cached_output_io(Command::new(ffmpeg_path())
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=30:duration=1",
                    "-pix_fmt",
                    "yuv420p",
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
                .expect("ffmpeg failed to build the 4:2:0 8-bit arm");
            assert!(
                out.status.success(),
                "{NAME}: ffmpeg could not build the 8-bit arm: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        assert_eq!(
            stream_bit_depth(&build_8bit, NAME),
            8,
            "{NAME}: this arm is the 8-bit cell the byte offsets below are computed for"
        );
        let our_8bit =
            decode_stream(&build_8bit).expect("the 8-bit arm must decode -- arm 1 needs it exact");
        assert!(
            !our_8bit.is_empty(),
            "{NAME}: the 8-bit arm decoded nothing"
        );
        let frames_8bit = our_8bit.len();

        // The monochrome recipe, byte-identical to the one
        // `a_real_libaom_monochrome_key_frame_decodes_pixel_exact` builds, so
        // the gray helper is driven on a stream that gate already proves
        // exact. A MONOCHROME stream is also this test's 4:2:0 4:2:0-free
        // arm: `ffmpeg_decode_gray_sequence` returns a flat luma buffer, so
        // the one flipped offset must be attributed to Y and to nothing else.
        let gray_stream = {
            let out = crate::tool_cache::cached_output_io(Command::new(ffmpeg_path())
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=30:duration=1",
                    "-pix_fmt",
                    "gray",
                    "-c:v",
                    "libaom-av1",
                    "-cpu-used",
                    "8",
                    "-b:v",
                    "100k",
                    "-f",
                    "obu",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to build the monochrome arm");
            assert!(
                out.status.success(),
                "{NAME}: ffmpeg could not build the monochrome arm: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let our_gray = decode_stream(&gray_stream).expect("the monochrome arm must decode");
        assert!(
            !our_gray.is_empty(),
            "{NAME}: the monochrome arm decoded nothing"
        );
        let frames_gray = our_gray.len();

        // ---- the one-sample-wrong ffmpeg shim ---------------------------
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // The real ffmpeg's path rides in a sibling file rather than inside
        // the script, so a path with a space or a quote cannot mangle it and
        // the script body needs no `format!` escaping.
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
            // Resolve the bare name through PATH so the shim does not depend
            // on inheriting whatever PATH the test runner had.
            let path = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path)
                .map(|d| d.join("ffmpeg"))
                .find(|p| p.is_file())
                .expect("{NAME}: ffmpeg is on PATH but not resolvable to a file")
        };
        std::fs::write(dir.join("real-ffmpeg"), real.as_os_str().as_encoded_bytes())
            .expect("recording the real ffmpeg's path for the shim");
        // Each line of `offsets` is one byte of the rawvideo stdout to rotate
        // by 128, so the write always changes it. Everything else -- the
        // stream, the geometry, the frame count, every other sample -- is a
        // genuine ffmpeg decode. stdin is inherited so the shim is a drop-in
        // for the helpers' `run_with_stdin` call.
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
        // A comparator that cannot report a difference is worse than none, so
        // each arm is scored as a COUNT of differing samples, not as a panic.
        let count8 = |theirs: &[Pic]| -> [usize; 3] {
            let mut wrong = [0usize; 3];
            for (plane, (x, y)) in [
                (0usize, (&our_8bit[0].y, &theirs[0].y)),
                (1, (&our_8bit[0].u, &theirs[0].u)),
                (2, (&our_8bit[0].v, &theirs[0].v)),
            ] {
                wrong[plane] = x.iter().zip(y).filter(|(a, b)| a != b).count();
            }
            wrong
        };

        // The 10-bit FILM pin, and the comparator both charter-named film
        // gates rest on. Driven on the real stream rather than a synthetic
        // one: `ffmpeg_decode_sequence_10bit` is the only reference helper
        // with a per-plane split, so it is the one that could silently lose a
        // plane. 1920x792 at 10 bits: luma 1920*792*2 bytes per frame, chroma
        // 960*396*2.
        let film = std::fs::read(crate_pin("reelc_sb128_inter_witness.obu")).unwrap_or_else(|e| {
            panic!(
                "{NAME}: the 10-bit film pin reelc_sb128_inter_witness.obu is missing ({e}) -- \
                 restore the committed copy"
            )
        });
        assert_eq!(
            stream_bit_depth(&film, NAME),
            10,
            "{NAME}: this arm is the 10-bit cell the byte offsets below are computed for"
        );
        let our_10bit =
            decode_stream(&film).expect("the 10-bit film pin must decode -- arm 1 needs it exact");
        let (fw, fh) = (1920usize, 792usize);
        let film_frames = our_10bit.len();
        let (fys, fcs) = (fw * fh * 2, (fw / 2) * (fh / 2) * 2);
        let count10 = |theirs: &[Pic]| -> [usize; 3] {
            let mut wrong = [0usize; 3];
            for (a, b) in our_10bit.iter().zip(theirs) {
                for (plane, (x, y)) in
                    [(0usize, (&a.y, &b.y)), (1, (&a.u, &b.u)), (2, (&a.v, &b.v))]
                {
                    wrong[plane] += x.iter().zip(y).filter(|(p, q)| p != q).count();
                }
            }
            wrong
        };
        let count_gray = |theirs: &[u8]| -> usize {
            our_gray[0]
                .y
                .iter()
                .zip(theirs)
                .filter(|(a, b)| **a != u16::from(**b))
                .count()
        };

        // ---- arm 1: the real ffmpeg, every comparator GREEN ------------
        {
            set_offsets(&[]);
            set_ffmpeg_path(None);
            let theirs = ffmpeg_decode_sequence(&build_8bit, w, h, frames_8bit);
            assert_eq!(
                count8(&theirs),
                [0, 0, 0],
                "{NAME}: arm 1 runs against the REAL ffmpeg, so every plane must be zero -- if \
                 this is red the stream is not the byte-exact pin this control needs"
            );
            let their_gray = ffmpeg_decode_gray_sequence(&gray_stream, 64, 64, frames_gray);
            assert_eq!(
                count_gray(&their_gray),
                0,
                "{NAME}: arm 1's monochrome plane must be zero against the real ffmpeg"
            );
            // arm 1's 10-bit half: the film pin is byte-exact against real
            // ffmpeg on every plane of every shown frame -- that is exactly
            // what `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_...`
            // asserts -- so this is the cell the two charter-named film gates
            // claim.
            let their_10bit = ffmpeg_decode_sequence_10bit(&film, fw, fh, film_frames);
            assert_eq!(
                count10(&their_10bit),
                [0, 0, 0],
                "{NAME}: arm 1's 10-bit film pin must be zero on every plane against the real \
                 ffmpeg -- if this is red the control's pin is not the byte-exact stream it needs"
            );
        }

        // ---- arm 2: one sample wrong, PER PLANE -------------------------
        // The shim is thread-local, so the tampered reference is confined to
        // this thread; it is cleared before anything is asserted, so a red arm
        // cannot leave this test's own later work reading a wrong ffmpeg.
        let mut blind: Vec<String> = Vec::new();
        {
            set_ffmpeg_path(Some(shim.clone()));
            for (plane, (_name, base)) in [("Y", 0usize), ("U", ys), ("V", ys + cs)]
                .into_iter()
                .enumerate()
            {
                // Byte 1000 of the named plane: inside the 4:2:0 8-bit pin's
                // luma (4225), inside its chroma (1024), or past both.
                set_offsets(&[base + 1000]);
                let theirs = ffmpeg_decode_sequence(&build_8bit, w, h, frames_8bit);
                let got = count8(&theirs);
                let want = [0usize, 0, 0];
                let mut expected = want;
                expected[plane] = 1;
                if got != expected {
                    blind.push(format!(
                        "ffmpeg_decode_sequence {plane} arm: counted {got:?}, expected \
                         {expected:?} (exactly the one tampered sample)"
                    ));
                }
            }
            // The 10-bit helper, per plane, on the 10-bit FILM pin. This is the
            // arm that matters for the charter: both
            // `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_...`
            // and
            // `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_...`
            // declare "pixel-exact on every plane" using nothing but this
            // helper's output. A helper that dropped U or V would leave both
            // gates green while chroma went unchecked for 15 and 33 frames.
            for (plane, (_name, base)) in [("Y", 0usize), ("U", fys), ("V", fys + fcs)]
                .into_iter()
                .enumerate()
            {
                // Sample 1000 of the named plane, two bytes per sample.
                set_offsets(&[base + 2000]);
                let theirs = ffmpeg_decode_sequence_10bit(&film, fw, fh, film_frames);
                let got = count10(&theirs);
                let mut expected = [0usize, 0, 0];
                expected[plane] = 1;
                if got != expected {
                    blind.push(format!(
                        "ffmpeg_decode_sequence_10bit {plane} arm: counted {got:?}, expected \
                         {expected:?} (exactly the one tampered sample)"
                    ));
                }
            }
            // The monochrome helper: one plane, so the single flipped offset
            // must show up as luma and nothing else can absorb it.
            set_offsets(&[1000]);
            let their_gray = ffmpeg_decode_gray_sequence(&gray_stream, 64, 64, frames_gray);
            if count_gray(&their_gray) != 1 {
                blind.push(format!(
                    "ffmpeg_decode_gray_sequence arm: counted {} differing luma samples, expected \
                     exactly 1",
                    count_gray(&their_gray)
                ));
            }
            set_ffmpeg_path(None);
        }
        let _ = std::fs::remove_dir_all(&dir);
        // One assert, naming EVERY helper that failed to notice: a partial run
        // would report the first and hide whether the others are covered.
        assert!(
            blind.is_empty(),
            "{NAME}: {} of 7 ffmpeg comparator arms stayed GREEN against an ffmpeg that is wrong \
             by exactly one sample -- {blind:#?} -- they cannot fail, so their green proves \
             nothing (class `comparator-that-never-reads-the-oracle`)",
            blind.len()
        );
        eprintln!(
            "{NAME}: ffmpeg_decode_sequence red on a one-sample-wrong Y, U and V separately; \
             ffmpeg_decode_gray_sequence red on one wrong luma sample; \
             ffmpeg_decode_sequence_10bit red on a one-sample-wrong Y, U and V of the 10-bit \
             film pin"
        );
    }

    /// lane-av1422chrtx: the odd-frame-HEIGHT 4:2:0 witness, pinned and now
    /// EXACT (lane-av1422anom had pinned it as a ratchet on its own defect).
    ///
    /// This is NOT a 4:2:2 cell and needs no bypass -- it is ordinary 4:2:0,
    /// reachable by any user. lane-av1422anom pinned it as a RATCHET asserting
    /// its own measured defect `(0, 200, 17658)`, and said whoever fixed it
    /// must flip that to exactness. That is done, and the flip is the
    /// deliverable:
    ///
    /// the defect was a chroma `tx_type`, not a geometry. libaom's
    /// `av1_get_ext_tx_set_type` (`blockd.h`) resolves a **32-point** chroma
    /// unit to `EXT_TX_SET_DCT_IDTX` when the block `is_inter` -- so an
    /// Intra-Block-Copy block whose luma coded `IDTX` runs the identity
    /// transform on its chroma. `read_plane`'s `side >= 32` clamp forced
    /// `DCT_DCT` there (its `DCT_DCT` cap is really libaom's
    /// `txsize_sqr_up_map[tx_size] > TX_32X32`, i.e. 64 and up), so the
    /// residual was a 32-point IDCT where libaom's was a plain copy of the
    /// dequantized coefficients `>>2`. The wrong samples sat in one 64x64
    /// IBC block's 32x32 chroma unit at chroma (128,96) on frame 0, and
    /// propagated from there through the inter frames.
    ///
    /// `the_counting_oracle_diff_detects_one_flipped_oracle_byte` is the
    /// non-vacuity control that keeps the `0` above worth anything.
    #[test]
    fn the_pinned_420_oddheight_witness_is_pinned_and_decodes_byte_exact() {
        const NAME: &str = "the_pinned_420_oddheight_witness_is_pinned_and_decodes_byte_exact";
        // lane-av1cmpaudit2: same mutex as every other oracle gate.
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_oddheight_320x232.obu";
        const BYTES: usize = 15773;
        // sha256 6832c3cd867f5fc491b45fd67f77c38453db0ca6d461e7d225e6276f4d0784c8,
        // recorded in `lanes/av1422anom.report.md`. The tree pins fixture
        // identity by fnv1a64, asserted below. A `const SHA256` compared
        // against itself would be a tautology -- the exact bug this lane
        // exists to kill -- so it is not written.
        const FNV1A64: u64 = 0x37877f15307c3b;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(FILE);
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: pinned witness {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        let (wy, wu, wv, frames, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv),
            (0, 0, 0),
            "{NAME}: the pinned 320x232 witness must decode byte-exactly against \
             `aomdec --rawvideo` on every plane. It was a RATCHET asserting its own \
             measured defect (0, 200, 17658); lane-av1422chrtx flipped it to exactness \
             when the 32-point INTER chroma `tx_type` (EXT_TX_SET_DCT_IDTX, not \
             EXT_TX_SET_DCTONLY) was fixed. Any non-zero value here is a regression -- \
             name it in the report."
        );
        assert_eq!(frames, 16, "{NAME}: shown frame count");
        assert_eq!(
            exact, frames,
            "{NAME}: every shown frame must be byte-exact"
        );
    }

    /// lane-av1oddheightfork3: the 320x236 cell this lane CLOSED, as a
    /// permanent BYTE-EXACTNESS gate against the live oracle -- not a route
    /// counter, not a source scan. It compares our decoded samples with
    /// `aomdec --rawvideo`'s own bytes plane by plane, and asserts every
    /// frame is byte-exact.
    ///
    /// **The defect it pins.** Before the fix this cell measured
    /// `Y 234349 / U 56957 / V 52981, 0 of 16 frames exact` -- reproduced on
    /// this lane's own tree by reverting the fix, matching
    /// `lanes/av1oddluma.report.md` and `lanes/av1cmpframe.report.md` digit
    /// for digit. The first divergent read is the `txfm_partition` symbol of
    /// the INTRABC 8x16 block at mi(44,60): our `TXFM_CONTEXT` grid held the
    /// BLOCK's own `16` in `left_txfm[44]` where libaom holds the split
    /// leaf's `8` (the operand pair is printed by the oracle rung
    /// `EC_VARTXCTX`, `scripts/instrument-aom-oracle.sh`), so `ctx` was 12
    /// against libaom's 13 and `is_split` 0 against 1.
    ///
    /// **Non-vacuity.** `the_counting_oracle_diff_detects_one_flipped_oracle_byte`
    /// is the comparator control and does not depend on any witness
    /// diverging; the mutation proof that this gate BITES the defect it
    /// names is in `lanes/av1oddheightfork3.report.md` (revert the
    /// `fill_lf_grid_rect_after_vartx` call sites -> this test reds BY NAME
    /// with the numbers above).
    #[test]
    fn the_pinned_420_oddheight_320x236_witness_decodes_byte_exact() {
        const NAME: &str = "the_pinned_420_oddheight_320x236_witness_decodes_byte_exact";
        let _gate_lock = lock_gate_counters();
        const FILE: &str = "420_oddheight_320x236.obu";
        const BYTES: usize = 16562;
        // sha256 84e4d1ab56620af1c5b78e1aa3d2d67496a492f2e127198c82dd1d68b01c6200.
        // Identity is pinned by fnv1a64 (the same value
        // `the_counting_oracle_diff_attributes_planes_per_frame` asserts) --
        // a `const SHA256` compared against itself would be the tautology
        // this lane exists to kill.
        const FNV1A64: u64 = 521889434652397870;
        let data = crate_pin("420_oddheight_320x236.obu");
        let data = std::fs::read(&data)
            .unwrap_or_else(|e| panic!("{NAME}: the committed pin {FILE} is missing ({e})"));
        assert_eq!(data.len(), BYTES, "{NAME}: {FILE} size drifted");
        assert_eq!(fnv1a64(&data), FNV1A64, "{NAME}: {FILE} bytes drifted");
        let (wy, wu, wv, frames, exact) =
            count_rawvideo_diffs(&data, NAME, None).expect("the two sides must be byte-comparable");
        assert_eq!(
            (wy, wu, wv),
            (0, 0, 0),
            "{NAME}: the pinned 320x236 cell must decode byte-exactly against \
             `aomdec --rawvideo` on every plane. It measured \
             (234349, 56957, 52981) with 0 of 16 frames exact until \
             lane-av1oddheightfork3 fixed the TXFM_CONTEXT whole-block \
             publish that overwrote a var-tx tree's per-leaf sizes. Any \
             non-zero value here is a regression -- name it in the report."
        );
        assert_eq!(frames, 16, "{NAME}: shown frame count");
        assert_eq!(
            exact, frames,
            "{NAME}: every shown frame must be byte-exact"
        );
    }

    /// The repo's first hidden-frame pixel gate. Every other aomenc gate
    /// here compares `decode_stream`'s output against `ffmpeg -f obu`, and
    /// both sides emit SHOWN frames only -- so an alt-ref frame that is
    /// decoded, stored into a reference slot and never displayed was, until
    /// this gate, never compared to anything (class
    /// gate-blind-to-hidden-frames). His films are aomenc-encoded with the
    /// default `--lag-in-frames`, so they are full of exactly these frames.
    ///
    /// `--auto-alt-ref=1 --lag-in-frames=25` on moving content makes aomenc
    /// build alt-refs; `--arnr-maxframes=0` is load-bearing -- with libaom's
    /// temporal filter on, this fixture's alt-ref candidates get filtered
    /// away and the encoder emits ZERO hidden frames (measured: 0 hidden
    /// with the filter on, 3 with it off), which would have made the gate
    /// vacuous. The `hidden >= 2` assertion is hard, on both bit depths, so
    /// a recipe that stops producing hidden frames fails rather than skips.
    #[test]
    fn a_real_aomenc_altref_sequence_hidden_frames_decode_pixel_exact() {
        const NAME: &str = "a_real_aomenc_altref_sequence_hidden_frames_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        for bit_depth in [8u32, 10u32] {
            let (width, height, frames) = (64usize, 64usize, 40usize);
            let duration = frames as f64 / 25.0;
            let source = format!("testsrc2=size={width}x{height}:duration={duration}:rate=25");
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
                "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let depth_arg = format!("--bit-depth={bit_depth}");
            let input_depth_arg = format!("--input-bit-depth={bit_depth}");
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    // lane-defon r1: the repo's only gate that decodes hidden
                    // frames is also the only place a FORWARD KEYFRAME can be
                    // proven -- a KEY frame with `show_frame == 0`, which is
                    // exactly the shape this gate already compares in decode
                    // order. On-values go before the base list (aomenc keeps
                    // the FIRST occurrence of a repeated flag).
                    "--enable-fwd-kf=1",
                    "--fwd-kf-dist=8",
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--kf-max-dist=1000",
                    "--enable-order-hint=1",
                    "--auto-alt-ref=1",
                    "--lag-in-frames=25",
                    "--arnr-maxframes=0",
                    &depth_arg,
                    &input_depth_arg,
                    // The same tool set every sibling inter gate pins -- the
                    // open capability gaps (rect/AB partitions, warped/OBMC,
                    // masked compound, palette, ...) are other lanes' work;
                    // this gate is about hidden frames, not about them.
                    "--enable-warped-motion=0",
                    // OBMC stays off HERE (this gate is about hidden frames);
                    // the tool's own coverage is the `obmc*` gates and, on a
                    // 16x8/8x16 leaf, the `--enable-obmc=1` arm of
                    // `..._with_a_16_level_rect_leaf_...` (lane-inter4 r4).
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
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
                "{NAME}: aomenc refused the {bit_depth}-bit fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let fwd_kf_before = crate::decode::fwd_kf_hits();
            let (total, hidden) =
                decode_all_frames_vs_oracle(&stream, &format!("altref-hidden-{bit_depth}bit"));
            let fwd_kf = crate::decode::fwd_kf_hits() - fwd_kf_before;
            assert!(
                hidden >= 2,
                "{NAME}: the {bit_depth}-bit recipe produced only {hidden} hidden frame(s) of \
                 {total} decoded -- the gate would be vacuous (class \
                 gate-blind-to-hidden-frames)"
            );
            // lane-defon r1: `--enable-fwd-kf=1` is spelled on this recipe, so
            // a KEY frame with `show_frame == 0` must actually have been
            // decoded -- otherwise the on-value proves nothing.
            assert!(
                fwd_kf > 0,
                "{NAME}: the {bit_depth}-bit recipe decoded {total} frames ({hidden} hidden) but \
                 zero forward keyframes -- --enable-fwd-kf=1 --fwd-kf-dist=8 never reached the \
                 bitstream"
            );
            eprintln!(
                "{NAME} {bit_depth}-bit: {total} frames decoded, {hidden} hidden, {fwd_kf} \
                 forward keyframes, all pixel-exact vs the oracle in decode order"
            );
        }
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
    /// seam) until the recipe without it round-tripped byte-exact.
    ///
    /// lane-av1skipfix: the attempt-loop shape this gate's single recipe
    /// lacked. It sweeps (seed, cq) and REQUIRES at least one attempt to both
    /// decode and read `use_filter_intra`: a decode error on EVERY attempt is
    /// a hard failure carrying the refusal texts, not a printed SKIP, and an
    /// attempt that decodes without the symbol firing is still pixel-compared
    /// (class `gate-skips-on-its-own-failure`). MEASURED 2026-09-29 on
    /// `a21f3680`: seed 42 cq 40 and seed 43 cq 40 read the symbol 3x each,
    /// pixel-exact, while seed 44 cq 45 and seed 45 cq 30 decode WITHOUT it --
    /// so the one fixed recipe really did carry the whole gate.
    ///
    /// The per-attempt decode error the old single recipe SKIPped on is
    /// `screen_content_tools_determination` (libaom `encoder_utils.c`): a real
    /// internal trial-encode/PSNR comparison, not a flag this CLI exposes a
    /// knob for -- on this flat gradient fixture it lands on
    /// `allow_screen_content_tools=0` most runs but occasionally trips over
    /// its 0.9 dB threshold the other way even with palette/intrabc both
    /// disabled. That is a genuine separate gap (`decode_stream` never reads
    /// `intrabc`/`palette_mode_info`), so it is a NAMED REFUSAL counted per
    /// attempt, never a silent skip and never a pass.
    #[test]
    fn a_real_aomenc_filter_intra_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_filter_intra_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        // Recipe-search hooks: with the encoder pinned to one thread the stream
        // is a deterministic function of (seed, cq), so a firing recipe can be
        // searched for without a rebuild. These pin attempt 0 for a bisect; the
        // sweep walks the same axes regardless.
        let fi_seed: u32 = std::env::var("EC_FI_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(42);
        let fi_cq: u32 = std::env::var("EC_FI_CQ")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        // Two seeds x three quantisers, so "every attempt refused" and "no
        // attempt ever read `use_filter_intra`" are distinguishable failures
        // rather than one unlucky recipe.
        const CQS: [u32; 3] = [40, 35, 30];
        let mut refusals = Vec::new();
        let mut counted_exact = 0u32;
        let mut uncounted_exact = 0u32;
        'attempts: for round in 0..2u32 {
            for (i, &walked) in CQS.iter().enumerate() {
                let seed = fi_seed + round;
                let cq = if round == 0 && i == 0 { fi_cq } else { walked };
                let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
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
                    "ffmpeg refused to generate the y4m fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        &cq_arg,
                        "--cpu-used=0",
                        // Every sibling gate pins these; this one did not, and a
                        // multi-threaded aomenc is not deterministic, so each run
                        // encoded a slightly different stream and the gate flaked
                        // against whichever features that run happened to pick.
                        // Pinning them makes seed 42 mean one stream (class
                        // `parallel-flake-is-attempt-selection`).
                        "--threads=1",
                        "--row-mt=0",
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
                    ]),
                    &y4m.stdout,
                );
                assert!(
                    out.status.success(),
                    "{NAME}: aomenc refused the seed {seed} cq {cq} fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let before = decode::filter_intra_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        refusals.push(format!("seed {seed} cq {cq}: {e}"));
                        continue;
                    }
                };
                // A decode that succeeded is ALWAYS pixel-compared; the hit
                // counter only decides whether the attempt counts as firing
                // (class `counter-from-refused-stream`).
                let counted = decode::filter_intra_hits() != before;
                if counted {
                    counted_exact += 1;
                } else {
                    uncounted_exact += 1;
                }
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                assert_eq!(
                    frames[0].y, ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed {seed} cq {cq}, use_filter_intra fired: {counted})"
                );
                assert_eq!(
                    frames[0].u, ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed {seed} cq {cq}, use_filter_intra fired: {counted})"
                );
                assert_eq!(
                    frames[0].v, ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed {seed} cq {cq}, use_filter_intra fired: {counted})"
                );
                if counted_exact >= 2 {
                    break 'attempts;
                }
            }
        }
        gate_buckets(NAME, counted_exact, uncounted_exact, refusals.len());
        assert!(
            counted_exact > 0,
            "{NAME}: none of {} attempts decoded a stream that read use_filter_intra -- the \
             gate would prove nothing (class gate-blind-to-feature):\n{}",
            CQS.len() * 2,
            refusals.join("\n")
        );
    }

    /// A real `aomenc` palette-Y fixture (lane-palette r3): `smptebars`
    /// (SMPTE colour bars -- a handful of large flat luma regions, exactly
    /// the "flat, few-colour, repetitive" content `av1_choose_palette_map`'s
    /// RD trial rewards) with `hue=s=0` to flatten chroma to one constant
    /// value, so `DC_PRED` already nails every chroma block with zero
    /// residual and the encoder's own RD never has a reason to spend bits on
    /// a *UV* palette too (a real symbol this decoder still refuses --
    /// lane-palette r3's own scope is Y only). `smptebars` has no seed to
    /// vary, so the fixture is deterministic by construction; hashed twice
    /// below to prove it rather than assert it. `--enable-rect-partitions=0`
    /// / `-ab-` / `-1to4-` keep every intra block square, sidestepping the
    /// rect-strip screen-content refusal at `read_intra_mode_rect`
    /// (decode.rs ~2226) -- that refusal is this lane's own next milestone,
    /// not this gate's. `--sb-size=64` and `--threads=1 --row-mt=0` are the
    /// sibling-lane facts this charter paid for: a single 64x64 frame is one
    /// superblock either way, but pinning them keeps the recipe identical to
    /// every other gate in this file. HARD-asserts
    /// [`decode::palette_hits`] moved -- a stream that never reads a real
    /// palette block would refuse (or not) by construction without proving
    /// this milestone at all.
    ///
    /// lane-palette r4/r6/r7: r3/r4 found that this fixture's reconstructed
    /// pixels did not match ffmpeg's decode of the same bytes even though
    /// every symbol read by `decode_color_index_map` checked line-for-line
    /// against the oracle and matched. r6's range trace (`compare-range-not-tell`)
    /// pinned the real cause: `decode_color_index_map` was called from the
    /// wrong syntax position -- inline right after the Y colours, instead of
    /// after the whole mode-info read (UV palette mode_info + `filter_intra`,
    /// `av1_visit_palette`'s real call order). r7 ported the UV palette
    /// mode_info read and moved the colour-index-map decode after
    /// `filter_intra`; this gate now asserts a real pixel match instead of
    /// the old named refusal.
    #[test]
    fn a_real_aomenc_stream_with_palette_y_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_palette_y_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let source = "smptebars=size=64x64:rate=25";
        fn render(source: &str) -> Vec<u8> {
            crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    source,
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
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run")
                .stdout
        }
        let y4m_a = render(source);
        let y4m_b = render(source);
        assert_eq!(
            y4m_a, y4m_b,
            "smptebars must render byte-identical across two runs"
        );
        let y4m = y4m_a;
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--tune-content=screen",
                "--enable-palette=1",
                "--enable-intrabc=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=32",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-cfl-intra=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--enable-tx-size-search=0",
                "--loopfilter-control=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m,
        );
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::palette_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            decode::palette_hits() > before,
            "palette_y_mode never fired decoding this stream -- gate is vacuous"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// lane-kf900 r3: the palette colour reader at BOTH bit depths.
    ///
    /// `read_palette_colors_y`/`_uv` (decode.rs, mirroring `decodemv.c:478`
    /// and `:509`) size every raw literal they read by the sequence's
    /// `bit_depth`: the first colour is `bit_depth` bits, `min_bits` is
    /// `bit_depth - 3` (Y/U) or `bit_depth - 4` (V), `range` starts at
    /// `1 << bit_depth`, and the clamp/wrap bound is `(1 << bit_depth) - 1`.
    /// Ours hardcoded 8, so every 10-bit palette block read TWO BITS TOO FEW
    /// per transmitted colour and silently decoded the rest of the tile from
    /// the wrong bit position.
    ///
    /// It was silent because raw literals are EQUIPROBABLE: at the ranges
    /// this content reaches an equiprobable read maps the msac range onto
    /// itself (62728 -> 31364 -> renormalised 62728), so a cross-decoder
    /// RANGE ladder stays bit-identical straight through the miscount and
    /// only the decoded VALUE moves. Pinned on a 256x192 10-bit
    /// `--cq-level=59` stream whose first block is a 32x16 palette: aomdec's
    /// `EC_PAL` ladder and ours agreed on every range and disagreed on the
    /// very first colour index (0 vs 1); after the fix all 1024 `EC_PAL`
    /// lines of that block match aomdec line for line.
    ///
    /// The recipe is the 8-bit `..._with_palette_y_...` gate's, with the
    /// depth as the only variable, so a failure here is the depth and
    /// nothing else. HARD-asserts [`decode::palette_hits`] moved on each arm
    /// (a stream with no real palette block would prove nothing) and
    /// compares all three planes against ffmpeg's decode of the same bytes.
    #[test]
    fn a_real_aomenc_palette_stream_decodes_pixel_exact_at_8_and_10_bit() {
        const NAME: &str = "a_real_aomenc_palette_stream_decodes_pixel_exact_at_8_and_10_bit";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        for depth in [8usize, 10] {
            let pix_fmt = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let render = || {
                crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        "smptebars=size=64x64:rate=25",
                        "-vf",
                        "hue=s=0",
                        "-pix_fmt",
                        pix_fmt,
                        "-strict",
                        "-1",
                        "-t",
                        "0.04",
                        "-f",
                        "yuv4mpegpipe",
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run")
                    .stdout
            };
            let y4m_a = render();
            let y4m_b = render();
            assert_eq!(
                y4m_a, y4m_b,
                "{NAME}: smptebars must render byte-identical across two runs at {depth}-bit"
            );
            let y4m = y4m_a;
            let depth_arg = format!("--bit-depth={depth}");
            let input_depth_arg = format!("--input-bit-depth={depth}");
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cq-level=30",
                        "--cpu-used=0",
                        "--threads=1",
                        "--row-mt=0",
                        "--sb-size=64",
                        "--tune-content=screen",
                        "--enable-palette=1",
                        "--enable-intrabc=0",
                        "--enable-rect-partitions=0",
                        "--enable-ab-partitions=0",
                        "--enable-1to4-partitions=0",
                        "--min-partition-size=32",
                        "--max-partition-size=32",
                        "--enable-filter-intra=0",
                        "--enable-smooth-intra=0",
                        "--enable-paeth-intra=0",
                        "--enable-directional-intra=0",
                        "--enable-angle-delta=0",
                        "--enable-cfl-intra=0",
                        "--enable-cdef=0",
                        "--enable-restoration=0",
                        "--enable-tx-size-search=0",
                        "--loopfilter-control=0",
                        "--limit=1",
                    ])
                    // Per-arm overrides go AFTER the base recipe: aomenc keeps the
                    // LAST occurrence of a repeated flag.
                    .args([&depth_arg, &input_depth_arg])
                    .args(["--obu", "-o", "-", "-"]),
                &y4m,
            );
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the {depth}-bit fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::palette_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => panic!("{NAME}: decode_stream refused the {depth}-bit stream: {e}"),
            };
            assert!(
                decode::palette_hits() > before,
                "{NAME}: no real palette block at {depth}-bit -- gate is vacuous"
            );
            let ffmpeg_frames = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, 1)
            };
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma at {depth}-bit"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U at {depth}-bit");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V at {depth}-bit");
        }
    }

    /// lane-pal8 r1, the gate for a palette on a SQUARE 8x8 LEAF.
    ///
    /// `read_palette_mode_info` (decodemv.c:567) reads `palette_y_mode` for
    /// every `DC_PRED` block whose `av1_get_palette_bsize_ctx`
    /// (`num_pels_log2 - num_pels_log2[BLOCK_8X8]`, 0 at 8x8) exists, and
    /// `av1_decode_palette_tokens` (decodeframe.c:1135, `av1_visit_palette`)
    /// reads the colour-index map after the WHOLE mode-info read. Ours passed
    /// `None`/`&[]` from [`decode::decode_leaf8`], reading the mode symbol off
    /// CDF row 0 and refusing "a block that actually uses a palette (Y) --
    /// reconstruction is out of scope" the moment it fired -- and at 8 bit
    /// aomenc palettes SQUARE blocks, so with `--min-partition-size=8` every
    /// 8-bit screen-content stream stopped there (lane-kf900 r4's measurement).
    ///
    /// Sweeps both bit depths x cq 30/40/55 x `--enable-tx-size-search` 0/1 x
    /// two lavfi sources (+ a `--tile-columns=1` arm, per the neighbour-map
    /// rule: the palette bands are per-mi and tile-guarded). Key frame only
    /// (`--limit=1`): the inter frames of these recipes stop on OTHER lanes'
    /// open refusals (8x8 inter leaf chroma mode, inter 1:4 partitions).
    /// HARD-asserts the 8x8-leaf palette counter moved on the compared arms
    /// and that no arm mismatched, in or out of scope.
    #[test]
    fn a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 128usize);
        let (mut leaf8_total, mut fired_arms, mut compared, mut frames_compared) =
            (0usize, 0u32, 0u32, 0usize);
        let (mut out_of_scope, mut out_of_scope_mismatch, mut refusals) = (0u32, 0u32, 0u32);
        let (mut sb_strip_total, mut sb_strip_arms) = (0usize, 0u32);
        // lane-t900 r34 LIFTED this shape; the counter stays so the gate
        // fails loudly if the refusal ever comes back.
        let mut sb_strip_refusals = 0u32;
        for src in ["testsrc2", "smptebars"] {
            for depth in [8usize, 10usize] {
                for cq in [30usize, 40, 55] {
                    for txs in [0usize, 1] {
                        for tiles in [0usize, 1] {
                            let arm = format!("{src}-{depth}bit-cq{cq}-txs{txs}-tc{tiles}");
                            let pix_fmt = if depth == 10 {
                                "yuv420p10le"
                            } else {
                                "yuv420p"
                            };
                            let size = format!("{src}=size={width}x{height}:rate=25");
                            let render = || {
                                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                                    .args([
                                        "-v",
                                        "error",
                                        "-f",
                                        "lavfi",
                                        "-i",
                                        &size,
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
                                assert!(out.status.success(), "{NAME}: ffmpeg failed for {arm}");
                                out.stdout
                            };
                            let y4m_a = render();
                            let y4m_b = render();
                            assert_eq!(
                                y4m_a, y4m_b,
                                "{NAME}: {arm} must render byte-identical across two runs"
                            );
                            let y4m = y4m_a;
                            let depth_arg = format!("--bit-depth={depth}");
                            let input_depth_arg = format!("--input-bit-depth={depth}");
                            let cq_arg = format!("--cq-level={cq}");
                            let txs_arg = format!("--enable-tx-size-search={txs}");
                            let tiles_arg = format!("--tile-columns={tiles}");
                            let out = run_with_stdin(
                                Command::new(aomenc_path())
                                    .args([
                                        "--codec=av1",
                                        "--passes=1",
                                        "--end-usage=q",
                                        "--cpu-used=0",
                                        "--threads=1",
                                        "--row-mt=0",
                                        "--lag-in-frames=0",
                                        "--limit=1",
                                        "--sb-size=64",
                                        "--tune-content=screen",
                                        "--enable-palette=1",
                                        "--enable-intrabc=0",
                                        "--min-partition-size=8",
                                        "--enable-cdef=0",
                                        "--enable-restoration=0",
                                        "--loopfilter-control=0",
                                    ])
                                    // aomenc keeps the LAST occurrence of a repeated flag,
                                    // so per-arm overrides go after the base recipe.
                                    .args([
                                        &depth_arg,
                                        &input_depth_arg,
                                        &cq_arg,
                                        &txs_arg,
                                        &tiles_arg,
                                    ])
                                    .args(["--obu", "-o", "-", "-"]),
                                &y4m,
                            );
                            assert!(
                                out.status.success(),
                                "{NAME}: aomenc refused {arm}: {}",
                                String::from_utf8_lossy(&out.stderr)
                            );
                            let stream = out.stdout;
                            decode::reset_palette_leaf8_hits();
                            decode::reset_palette_sb_strip_hits();
                            let decoded = decode_stream(&stream);
                            let leaf8 = decode::palette_leaf8_hits();
                            let sb_strip = decode::palette_sb_strip_hits();
                            leaf8_total += leaf8;
                            sb_strip_total += sb_strip;
                            match decoded {
                                Ok(frames) => {
                                    assert!(!frames.is_empty(), "{NAME}: {arm} decoded no frame");
                                    // The ENCODE's frame count: the per-arm
                                    // render (10417) builds
                                    // `{src}=size=128x128:rate=25` with `-t 0.04`
                                    // (10427) -- one frame -- and the aomenc args
                                    // carry `--limit=1` (10467), so every
                                    // (src, depth, cq, txs, tiles) arm codes
                                    // exactly 1 frame. Asserted ABOVE the oracle
                                    // call.
                                    assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
                                    let theirs = if depth == 10 {
                                        ffmpeg_decode_sequence_10bit(
                                            &stream,
                                            width,
                                            height,
                                            frames.len(),
                                        )
                                    } else {
                                        ffmpeg_decode_sequence(&stream, width, height, frames.len())
                                    };
                                    assert_eq!(
                                        frames.len(),
                                        theirs.len(),
                                        "{NAME}: {arm} frame count"
                                    );
                                    let mut same = true;
                                    for (i, (ours, ref_f)) in
                                        frames.iter().zip(theirs.iter()).enumerate()
                                    {
                                        let ok = ours.y == ref_f.y
                                            && ours.u == ref_f.u
                                            && ours.v == ref_f.v;
                                        if !ok {
                                            eprintln!("{NAME}: {arm} MISMATCH at frame {i}");
                                        }
                                        same &= ok;
                                        frames_compared += 1;
                                    }
                                    compared += 1;
                                    if sb_strip > 0 {
                                        sb_strip_arms += 1;
                                        assert!(
                                            same,
                                            "{NAME}: {arm} decoded {sb_strip} non-skip palette \
                                             block(s) on a superblock-level strip and did not \
                                             match ffmpeg"
                                        );
                                    }
                                    if leaf8 > 0 || sb_strip > 0 {
                                        fired_arms += 1;
                                        assert!(
                                            same,
                                            "{NAME}: {arm} decoded {leaf8} 8x8-leaf palette \
                                             block(s) and did not match ffmpeg"
                                        );
                                    } else {
                                        out_of_scope += 1;
                                        if !same {
                                            out_of_scope_mismatch += 1;
                                        }
                                    }
                                    eprintln!(
                                        "{NAME}: {arm} {} frame(s), {leaf8} 8x8-leaf palette \
                                         block(s), {sb_strip} superblock-strip palette \
                                         block(s), exact={same}",
                                        frames.len()
                                    );
                                }
                                Err(e) => {
                                    let msg = e.to_string();
                                    assert!(
                                        msg.contains("unsupported"),
                                        "{NAME}: {arm} decode failed: {e}"
                                    );
                                    // Named, counted, never turned into a skip.
                                    if msg.contains("superblock-level HORZ/VERT strip") {
                                        sb_strip_refusals += 1;
                                    }
                                    eprintln!("{NAME}: {arm} REFUSED: {e}");
                                    refusals += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "{NAME}: {leaf8_total} 8x8-leaf palette block(s) across {fired_arms} arm(s), \
             {compared} arms compared ({frames_compared} frames), {sb_strip_total} \
             superblock-strip palette block(s) across {sb_strip_arms} arm(s), \
             {out_of_scope} out of scope ({out_of_scope_mismatch} mismatched), \
             {refusals} refused ({sb_strip_refusals} superblock-strip palette)"
        );
        assert!(
            leaf8_total > 0 && fired_arms > 0,
            "{NAME}: gate is vacuous -- no compared arm decoded an 8x8-leaf palette block"
        );
        assert_eq!(
            sb_strip_refusals, 0,
            "{NAME}: the superblock-strip palette refusal is back (lane-t900 r34 lifted it)"
        );
        assert!(
            sb_strip_arms > 0 && sb_strip_total > 0,
            "{NAME}: no arm decoded a non-skip palette block on a superblock-level strip -- \
             the shape r34 lifted is no longer exercised"
        );
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: an out-of-scope arm mismatched ffmpeg -- a defect, never a skip"
        );
        assert!(
            frames_compared > 0,
            "{NAME}: no frame was compared pixel-exact"
        );
    }

    /// lane-av1toolgates: `enable-rect-tx` at 8 BITS. See
    /// [`rect_tx_tool_gate`] for what the flag does and does not prove, and
    /// why the recipe is spelled here rather than in the helper.
    ///
    /// The whole recipe is inline: `--passes=1` has to be findable in this
    /// segment for the coverage derivation to count the gate at all, and
    /// `--enable-rect-tx=1` has to be findable here for it to count the
    /// TOOL. The quantiser and the tx-size-search setting are swept as two
    /// arms so the witness cannot rest on one RD interaction.
    #[test]
    fn a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact() {
        let mut totals = (0usize, 0usize);
        for (cq, tx_search) in [("20", "1"), ("40", "0")] {
            let args: Vec<String> = [
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                "--error-resilient=1",
                "--sb-size=64",
                // The tool under test, spelled explicitly. CONTEXT, per
                // `rect_tx_tool_gate`: aomenc was ALLOWED rect tx sizes.
                "--enable-rect-tx=1",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--enable-cdef=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-order-hint=0",
                "--min-partition-size=8",
                "--max-partition-size=32",
            ]
            .map(String::from)
            .into_iter()
            .chain([
                format!("--cq-level={cq}"),
                format!("--enable-tx-size-search={tx_search}"),
            ])
            // The output tail comes LAST: aomenc stops reading flags at the
            // first bare argument, so a sweep flag appended after `-o - -` is
            // silently dropped and the arm encodes a different recipe than the
            // one this gate claims to have run.
            .chain(["--obu", "-o", "-", "-"].map(String::from))
            .collect();
            let name = format!(
                "a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact[cq{cq}/tx{tx_search}]"
            );
            let (p, t) = rect_tx_tool_gate(&name, false, &args);
            totals.0 += p;
            totals.1 += t;
        }
        assert_rect_tx_witness(
            "a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact",
            totals.0,
            totals.1,
        );
        eprintln!(
            "a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact: 2 arms \
             pixel-exact; parsed rect partitions {}, rect coefficient TUs {}",
            totals.0, totals.1
        );
    }

    /// lane-av1toolgates: `enable-rect-tx` at 10 BITS -- the same recipe, the
    /// same parsed-shape witness, at the depth the 8-bit arm says nothing
    /// about (the per-depth rule `NEVER_EXERCISED_10BIT` is built on:
    /// lane-hbdinter's two 10-bit-only defects survived because every 10-bit
    /// gate pinned the filters off). The depth is spelled in THIS body,
    /// which is what files the gate in the 10-bit bucket.
    #[test]
    fn a_real_aomenc_10bit_stream_with_a_rect_transform_decodes_pixel_exact() {
        let mut totals = (0usize, 0usize);
        for (cq, tx_search) in [("20", "1"), ("40", "0")] {
            let args: Vec<String> = [
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                "--error-resilient=1",
                "--sb-size=64",
                "--input-bit-depth=10",
                "--bit-depth=10",
                // The tool under test, spelled explicitly. CONTEXT, per
                // `rect_tx_tool_gate`: aomenc was ALLOWED rect tx sizes.
                "--enable-rect-tx=1",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--enable-cdef=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-order-hint=0",
                "--min-partition-size=8",
                "--max-partition-size=32",
            ]
            .map(String::from)
            .into_iter()
            .chain([
                format!("--cq-level={cq}"),
                format!("--enable-tx-size-search={tx_search}"),
            ])
            // The output tail comes LAST: aomenc stops reading flags at the
            // first bare argument, so a sweep flag appended after `-o - -` is
            // silently dropped and the arm encodes a different recipe than the
            // one this gate claims to have run.
            .chain(["--obu", "-o", "-", "-"].map(String::from))
            .collect();
            let name = format!(
                "a_real_aomenc_10bit_stream_with_a_rect_transform_decodes_pixel_exact[cq{cq}/tx{tx_search}]"
            );
            let (p, t) = rect_tx_tool_gate(&name, true, &args);
            totals.0 += p;
            totals.1 += t;
        }
        assert_rect_tx_witness(
            "a_real_aomenc_10bit_stream_with_a_rect_transform_decodes_pixel_exact",
            totals.0,
            totals.1,
        );
        eprintln!(
            "a_real_aomenc_10bit_stream_with_a_rect_transform_decodes_pixel_exact: 2 arms \
             pixel-exact; parsed rect partitions {}, rect coefficient TUs {}",
            totals.0, totals.1
        );
    }

    /// lane-kf900 r5, the gate for the rect-strip `use_intrabc` read.
    ///
    /// `av1_allow_intrabc` is a FRAME-level test (`decodemv.c:892`,
    /// `read_intra_frame_mode_info`): on an intra frame whose header set
    /// `allow_intrabc`, EVERY intra block reads a `use_intrabc` symbol right
    /// after `skip`/`cdef`/`delta_q` and before `mbmi->mode`. There is no
    /// block-shape term, so rect strips read it exactly like square leaves --
    /// `read_intra_mode_rect` never did, and the arithmetic decoder desynced
    /// at the FIRST symbol of the first rect block of every
    /// `--tune-content=screen` key frame (measured: `testsrc2` 256x192 cq59
    /// 8-bit decoded "OK" with all 49152 luma samples wrong).
    ///
    /// Arm 0 is that recipe at cq50 without palette: its rect strips really do
    /// use intrabc. lane-av1-rect14 reconstructs those strips (2:1 and the
    /// 16x4/4x16 pair), so the arm must now decode pixel-exact -- and it must
    /// still have read the symbol. The named refusal remains only for a
    /// 128-superblock strip (`decode_block_128rect`), which this sb64 recipe
    /// does not reach.
    /// Arm 1 is the same recipe without palette and without tx-size search --
    /// it decodes the whole frame, reading the symbol dozens of times.
    /// Arm 2 is a screen-tools key frame whose header does NOT set
    /// `allow_intrabc` (cq63): it stays pixel-exact, proving the new read is
    /// not taken when libaom does not write it.
    ///
    /// PIXEL RESIDUE (open, lane-kf900 r5): arm 1's frame is entropy-exact
    /// against the instrumented aomdec for 429 mode-ladder elements and then
    /// diverges inside the DV read of a SQUARE intrabc block at mi(32,44), so
    /// its pixels are not compared here. That is a separate defect in
    /// `read_intrabc_dv`, not in the read this gate covers.
    #[test]
    fn a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips() {
        const NAME: &str = "a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 192usize);
        // (arm, source, per-arm overrides AFTER the base recipe, expect_refusal,
        //  must_read_use_intrabc)
        const TS: &str = "testsrc2=s=256x192:r=25";
        const SB: &str = "smptebars=size=256x192:rate=25";
        // (arm, source, per-arm overrides AFTER the base recipe, expect_refusal,
        //  must_read_use_intrabc_on_a_rect_strip)
        let arms: [(&str, &str, &[&str], bool, bool); 8] = [
            // lane-av1-intrabc (2026-09-20): this arm used to REFUSE here -- the
            // 16x8 rect strip it decodes is the first block on main that reaches
            // `decode_intrabc_rect`, so the whole arm stopped at a named refusal.
            // Reconstruction is ported now, so the arm decodes pixel-exact and
            // the `rect_total` assert below keeps it from quietly falling back
            // to an ordinary-intra decode of the same block.
            (
                "testsrc2-cq50-txs0",
                TS,
                &[
                    "--cq-level=50",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
                false,
                true,
            ),
            (
                "smptebars-cq40-txs0",
                SB,
                &[
                    "--cq-level=40",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
                false,
                false,
            ),
            (
                "smptebars-cq45-txs0",
                SB,
                &[
                    "--cq-level=45",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
                false,
                true,
            ),
            (
                "smptebars-cq45-txs1",
                SB,
                &[
                    "--cq-level=45",
                    "--enable-palette=0",
                    "--enable-tx-size-search=1",
                ],
                false,
                false,
            ),
            (
                "smptebars-cq50-txs1",
                SB,
                &[
                    "--cq-level=50",
                    "--enable-palette=0",
                    "--enable-tx-size-search=1",
                ],
                false,
                false,
            ),
            (
                "smptebars-cq55-txs0",
                SB,
                &[
                    "--cq-level=55",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
                false,
                false,
            ),
            (
                "testsrc2-cq63-txs1",
                TS,
                &[
                    "--cq-level=63",
                    "--enable-palette=0",
                    "--enable-tx-size-search=1",
                ],
                false,
                false,
            ),
            // lane-kf900 r7: the 8x8-LEAF intrabc arm. Rect and 1:4
            // partitions off (so no rect strip refuses first), palette on,
            // `TxMode::Largest`: aomenc puts exactly one `use_intrabc` block
            // on a `BLOCK_8X8` leaf here, the shape `decode_leaf8` decoded as
            // ordinary intra before this round.
            (
                "testsrc2-cq55-leaf8",
                TS,
                &[
                    "--cq-level=55",
                    "--enable-palette=1",
                    "--enable-tx-size-search=0",
                    "--enable-rect-partitions=0",
                    "--enable-1to4-partitions=0",
                ],
                false,
                false,
            ),
        ];
        let (mut reads_total, mut refused, mut frames_compared) = (0usize, 0u32, 0usize);
        let (mut blocks_total, mut fired_arms, mut out_of_scope, mut out_of_scope_mismatch) =
            (0usize, 0u32, 0u32, 0u32);
        let mut leaf8_total = 0usize;
        let mut rect_total = 0usize;
        for (arm, src, extra, expect_refusal, must_read) in &arms {
            let render = || {
                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        *src,
                        "-t",
                        "0.2",
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
                assert!(out.status.success(), "{NAME}: ffmpeg failed for {arm}");
                out.stdout
            };
            let y4m_a = render();
            let y4m_b = render();
            assert_eq!(
                y4m_a, y4m_b,
                "{NAME}: {arm} must render byte-identical across two runs"
            );
            let y4m = y4m_a;
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args([
                        "--codec=av1",
                        "--bit-depth=8",
                        "--input-bit-depth=8",
                        "--passes=1",
                        "--end-usage=q",
                        "--cpu-used=0",
                        "--lag-in-frames=0",
                        "--kf-max-dist=1",
                        "--limit=1",
                        "--threads=1",
                        "--tile-columns=0",
                        "--enable-rect-partitions=1",
                        "--enable-1to4-partitions=1",
                        "--min-partition-size=8",
                        "--max-partition-size=32",
                        "--sb-size=64",
                        // `allow_screen_content_tools` (and with it the frame
                        // header's `allow_intrabc` bit) comes from here.
                        "--tune-content=screen",
                        // Spelled so `gate_coverage` counts the tool as exercised
                        // rather than defaulted (class `tool-disabled-in-every-gate`).
                        "--enable-intrabc=1",
                    ])
                    // aomenc keeps the LAST occurrence of a repeated flag.
                    .args(*extra)
                    .args(["--obu", "-o", "-", "-"]),
                &y4m,
            );
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused {arm}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            decode::reset_rect_intrabc_reads();
            crate::decode::reset_intrabc_hits();
            crate::decode::reset_leaf8_intrabc_hits();
            crate::decode::reset_intrabc_rect_hits();
            let decoded = decode_stream(&stream);
            let reads = decode::rect_intrabc_reads();
            // Blocks that decoded `use_intrabc == 1` (square path), i.e. that
            // read a DV and predicted from the current frame -- distinct from
            // `reads`, which counts rect strips that merely READ the flag.
            let blocks = crate::decode::intrabc_hits();
            reads_total += reads;
            blocks_total += blocks;
            // The 8x8-leaf shape has its own counter: without it this gate's
            // other arms (all rect/32x32 intrabc) would stay green while the
            // leaf path decoded an intrabc block as ordinary intra (class
            // `gate-blind-to-feature`).
            let leaf8 = crate::decode::leaf8_intrabc_hits();
            leaf8_total += leaf8;
            rect_total += crate::decode::intrabc_rect_hits();
            if *arm == "testsrc2-cq55-leaf8" {
                assert!(
                    leaf8 > 0,
                    "{NAME}: {arm} decoded no intrabc block at an 8x8 leaf -- the arm no                      longer exercises `decode_leaf8`'s intrabc path"
                );
            }
            if *must_read {
                assert!(
                    reads > 0,
                    "{NAME}: {arm} read no use_intrabc symbol on a rect strip -- the \
                     arm no longer exercises the fix (allow_intrabc not set?)"
                );
            }
            match decoded {
                Ok(frames) => {
                    assert!(
                        !*expect_refusal,
                        "{NAME}: {arm} decoded a frame whose rect strips use intrabc -- \
                         reconstruction is not ported, it must refuse by name"
                    );
                    assert!(!frames.is_empty(), "{NAME}: {arm} decoded no frame");
                    // The ENCODE's frame count, asserted ABOVE the oracle call:
                    // the per-arm render (11100) uses `-t 0.2` at TS/SB's
                    // `rate=25` (10987, 10988) -- 5 input frames -- and the aomenc
                    // args carry `--limit=1` (11145), so all eight arms code
                    // exactly 1 frame. The old comment below claimed a count
                    // assert here would be tautological; that is true of
                    // `ffmpeg_frames.len() == frames.len()` and false of this:
                    // `frames.len() == 1` is a fact about the ENCODE, checked
                    // before `frames.len()` is spent as ffmpeg's frame budget.
                    assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
                    let ffmpeg_frames =
                        ffmpeg_decode_sequence(&stream, width, height, frames.len());
                    // EVERY frame is compared; an arm whose decode never hit an
                    // intrabc block proves nothing about the DV/prediction path,
                    // so it is counted OUT OF SCOPE -- but a mismatch there is
                    // still a failure (`out_of_scope_mismatch == 0`), never a skip.
                    let mut same = true;
                    for (i, (ours, theirs)) in frames.iter().zip(ffmpeg_frames.iter()).enumerate() {
                        let ok = ours.y == theirs.y && ours.u == theirs.u && ours.v == theirs.v;
                        if !ok {
                            eprintln!("{NAME}: {arm} MISMATCH at frame {i}");
                        }
                        same &= ok;
                        frames_compared += 1;
                    }
                    if blocks > 0 {
                        fired_arms += 1;
                        assert!(
                            same,
                            "{NAME}: {arm} decoded {blocks} intrabc block(s) and did not match ffmpeg"
                        );
                    } else {
                        out_of_scope += 1;
                        if !same {
                            out_of_scope_mismatch += 1;
                        }
                    }
                    eprintln!(
                        "{NAME}: {arm} {} frame(s), {reads} rect use_intrabc reads, \
                         {blocks} intrabc block(s), exact={same}",
                        frames.len()
                    );
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: {arm} decode failed: {e}"
                    );
                    assert!(
                        *expect_refusal,
                        "{NAME}: {arm} was expected to decode, it refused: {e}"
                    );
                    assert!(
                        msg.contains("intra block copy") || msg.contains("intrabc"),
                        "{NAME}: {arm} refused for an unrelated reason: {e}"
                    );
                    eprintln!("{NAME}: {arm} REFUSED (expected): {e}");
                    refused += 1;
                }
            }
        }
        eprintln!(
            "{NAME}: {reads_total} rect-strip use_intrabc symbol(s) read, \
             {blocks_total} intrabc block(s) decoded across {fired_arms} arm(s), \
             {refused} refused by name, {frames_compared} frames compared, \
             {out_of_scope} out of scope ({out_of_scope_mismatch} mismatched)"
        );
        eprintln!(
            "{NAME}: {rect_total} intrabc block(s) on a HORZ/VERT rect strip              (decode_intrabc_rect)"
        );
        assert!(
            reads_total > 0,
            "{NAME}: gate is vacuous -- no arm read the symbol"
        );
        assert!(
            leaf8_total > 0,
            "{NAME}: no arm decoded an intrabc block at an 8x8 leaf -- lane-kf900 r7's fix              is untested"
        );
        assert!(
            blocks_total > 0 && fired_arms > 0,
            "{NAME}: gate is vacuous -- no compared arm actually decoded an intrabc block"
        );
        assert!(
            rect_total > 0,
            "{NAME}: no arm decoded an intrabc block on a HORZ/VERT rect strip -- \
             `decode_intrabc_rect` is untested"
        );
        // lane-av1-intrabc: this used to be exactly one -- the 2:1 rect strips
        // now reconstruct. The 1:4 pair strips (`decode_rect4_16_strip` et al.)
        // still refuse by name, but no arm HERE reaches that shape; it stays
        // pinned by `refusal_inventory`'s own gate.
        assert_eq!(refused, 0, "{NAME}: an arm still refuses by name");
        assert!(
            frames_compared > 0,
            "{NAME}: no frame was compared pixel-exact"
        );
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: an arm without an intrabc block still mismatched ffmpeg"
        );
    }

    /// lane-av1-rect14: the 16x4/4x16 intrabc pair. libaom codes the pair's
    /// chroma on the odd-mi strip only (`is_chroma_reference`,
    /// av1_common_int.h:1454). The witness is the report recipe: testsrc2
    /// 640x480 cq60 palette, txs off, which places a 16x4 intrabc strip at
    /// mi(76,108). Pre-fix this refused by name. VERT_4 was not produced by
    /// any fully-decoded recipe tried here (the allintra stream codes four
    /// `PARTITION_VERT_4` at bsize 6 and then stops on the unrelated var-tx
    /// mixed-leaf refusal); the VERT arm is the same body with the axes
    /// swapped.
    #[test]
    fn a_16x4_intrabc_pair_strip_decodes_pixel_exact() {
        const NAME: &str = "a_16x4_intrabc_pair_strip_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: ffmpeg/aomenc missing");
            return;
        }
        let (width, height) = (640usize, 480usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x480:r=25",
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-strict",
                "-1",
                "-f",
                "yuv4mpegpipe",
                "-",
            ]), None, |c| c.output())
            .expect("ffmpeg")
            .stdout;
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--bit-depth=8",
                "--input-bit-depth=8",
                "--passes=1",
                "--end-usage=q",
                "--cpu-used=0",
                "--lag-in-frames=0",
                "--kf-max-dist=1",
                "--limit=1",
                "--threads=1",
                "--tile-columns=0",
                "--enable-rect-partitions=1",
                "--enable-1to4-partitions=1",
                "--min-partition-size=4",
                "--max-partition-size=64",
                "--sb-size=64",
                "--tune-content=screen",
                "--enable-intrabc=1",
                "--cq-level=60",
                "--enable-palette=1",
                "--enable-tx-size-search=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m,
        );
        assert!(out.status.success(), "{NAME}: aomenc failed");
        crate::decode::reset_rect4_16_pair_hits();
        let frames = decode_stream(&out.stdout).unwrap_or_else(|e| panic!("{NAME}: {e}"));
        assert!(
            crate::decode::rect4_16_intrabc_hits() > 0,
            "{NAME}: no 16x4/4x16 intrabc strip -- the pair body was not reached"
        );
        // The ENCODE's frame count, asserted ABOVE the oracle call: the y4m is
        // rendered with `-frames:v 1` (11333) -- a single frame of
        // `testsrc2=s=640x480:r=25` -- and aomenc runs with
        // `--limit=1 --kf-max-dist=1` (11355, 11356), so the stream codes
        // exactly 1 key frame. Below the call this assert could not fire
        // first: `ffmpeg_decode_sequence` was already handed `frames.len()`.
        assert_eq!(frames.len(), 1, "{NAME}: expected one key frame");
        let refs = ffmpeg_decode_sequence(&out.stdout, width, height, frames.len());
        assert_eq!(frames[0].y, refs[0].y, "{NAME}: luma");
        assert_eq!(frames[0].u, refs[0].u, "{NAME}: u");
        assert_eq!(frames[0].v, refs[0].v, "{NAME}: v");
    }

    /// lane-av1-rect14: lossless 16x4/4x16 chroma is two TX_4X4s, not one
    /// TX_8X4. The measured site is luma col 108 of a 320x240 sb128 lossless
    /// key frame (lanes/av1lossless128.report.md §6). Rows 0-127 of that
    /// frame -- the first superblock row, which contains the site -- must
    /// match ffmpeg. A later mismatch is the pre-existing 64x64 square
    /// lossless residue (base fails at byte 108; this lane's first remaining
    /// mismatch is row 128). The gate fails if that residue moves back into
    /// the repaired region.
    #[test]
    fn a_lossless_16x4_chroma_pair_repairs_the_measured_site() {
        const NAME: &str = "a_lossless_16x4_chroma_pair_repairs_the_measured_site";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: ffmpeg/aomenc missing");
            return;
        }
        let (width, height) = (320usize, 240usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=320x240:r=25",
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-strict",
                "-1",
                "-f",
                "yuv4mpegpipe",
                "-",
            ]), None, |c| c.output())
            .expect("ffmpeg")
            .stdout;
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--cpu-used=0",
                "--lossless=1",
                "--sb-size=128",
                "--lag-in-frames=0",
                "--kf-max-dist=1",
                "--limit=1",
                "--threads=1",
                "--passes=1",
                "--enable-1to4-partitions=1",
                "--min-partition-size=4",
                "--enable-rect-partitions=1",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m,
        );
        assert!(out.status.success(), "{NAME}: aomenc failed");
        crate::decode::reset_rect4_16_pair_hits();
        let frames = decode_stream(&out.stdout).unwrap_or_else(|e| panic!("{NAME}: {e}"));
        assert!(
            crate::decode::rect4_16_lossless_chroma_hits() > 0,
            "{NAME}: no lossless 16x4/4x16 chroma pair -- the walk was not reached"
        );
        // The ENCODE's frame count: the y4m is rendered with `-frames:v 1`
        // (11414) -- a single frame of `testsrc2=s=320x240:r=25` -- and aomenc
        // runs with `--limit=1 --kf-max-dist=1` (11434, 11435), so the stream
        // codes exactly 1 lossless key frame. Asserted ABOVE the oracle call:
        // this gate then indexes `frames[0]`/`refs[0]`, which a zero-frame
        // decode would panic on rather than report.
        assert_eq!(frames.len(), 1, "{NAME}: expected one lossless key frame");
        let refs = ffmpeg_decode_sequence(&out.stdout, width, height, frames.len());
        let repaired = 128 * width;
        assert_eq!(
            &frames[0].y[..repaired],
            &refs[0].y[..repaired],
            "{NAME}: luma rows 0-127 (the measured col-108 site) diverged"
        );
        let chroma_rows = 64 * (width / 2);
        assert_eq!(
            &frames[0].u[..chroma_rows],
            &refs[0].u[..chroma_rows],
            "{NAME}: u"
        );
        assert_eq!(
            &frames[0].v[..chroma_rows],
            &refs[0].v[..chroma_rows],
            "{NAME}: v"
        );
        // The col-108 byte specifically, so a shift of the old site still fails
        // even if some other prefix sample happens to match.
        assert_eq!(
            frames[0].y[108], refs[0].y[108],
            "{NAME}: measured byte 108"
        );
    }

    /// lane-av1tilerows: the 4:4:4 LOSSLESS rect-pair chroma REACH fix --
    /// the defect lane `av1formatsweep` filed as **H4** ("4:4:4 lossless +
    /// `--tile-rows=1`, frame 3 onward the whole plane goes flat") and this
    /// lane's red-before actually measured.
    ///
    /// **The H4 label was wrong; the defect it points at is real.** Two
    /// corrections to the sweep's framing, both measured:
    ///
    /// 1. `--tile-rows=1` at 256x128 is a **no-op**. aomenc's default
    ///    superblock is 128px, so 256x128 is 2x1 superblocks -- there is no
    ///    second superblock ROW to split, and aomenc silently emits a
    ///    stream BYTE-IDENTICAL to `--tile-columns=0 --tile-rows=0`
    ///    (verified: both 64828 bytes, `cmp` equal). The sweep's "tiled"
    ///    fixture contained no tile rows at all. Tile rows at 4:4:4
    ///    lossless are exact here: measured pixel-exact at 1, 2 and 4 tile
    ///    rows, both axes (2x2), at 256x128 / 256x256 / 128x256 / 512x128,
    ///    on a recipe that is exact without tiles.
    /// 2. The "frames 0-2 byte-exact, frame 3 onward flat, Y(0,0) 128 vs
    ///    81" signature is a **harness frame-order mismatch, not a decode
    ///    defect**: with altref on, aomdec's `EC_AV1_FINAL_DUMP` writes 7
    ///    frames in DECODE order while `ffmpeg -f rawvideo` writes 6 in
    ///    DISPLAY order. Indexing one against the other makes frame 1 differ
    ///    by 29109 samples (30%) -- and aomdec-vs-ffmpeg ALONE reproduces
    ///    the identical 29109/24412/20046/19906 counts with no edith
    ///    decoder in the loop. Our decode-order output is byte-exact against
    ///    the oracle on all 7.
    ///
    /// The defect IS real, is in the class the sweep named (a hardcoded
    /// 4:2:0-shaped walk at 4:4:4), and is tile-INDEPENDENT: it reproduces
    /// on the zero-tile stream. `decode.rs`'s lossless 16x4/4x16 chroma-pair
    /// walk passed `tu_reach` a hardcoded 4:2:0 luma footprint
    /// (`ox * 2, oy * 2, 8`) for every chroma transform unit. At 4:4:4 a
    /// 4x4 chroma unit is 4x4 LUMA, so the footprint is doubled:
    /// `Reach::of_tu`'s `col_off + tx_w < bw` test then answers against a
    /// block twice its real width and every unit past the midpoint wrongly
    /// reports `above_right = false`, so a directional chroma strip predicts
    /// from a neighbour libaom never reads.
    ///
    /// **Measured site (red-before, `git stash` of the fix, same tree):**
    /// `testsrc2 256x128 yuv444p`, aomenc `--profile=1 --lossless=1
    /// --passes=1 --end-usage=q --threads=1 --row-mt=0 --limit=6
    /// --cpu-used=2 --lag-in-frames=0 --kf-max-dist=100` (pinned below).
    /// Frame 0 (the KEY frame) and all five inter frames: **63 samples wrong
    /// per frame, U 29 + V 34, first at U(110,26) / V(108,26), deltas -10..+13**,
    /// region x108..125 y26..29 -- a 16x4 chroma strip at 4:4:4, whose rows
    /// are NOT block-aligned (y26), which is the signature of a REACH
    /// answer and not of a coefficient or transform error.
    ///
    /// **Entropy is bit-identical**: `EC_SYMR` on both sides, 351109 reads
    /// compared on `(value, range, symbol, post_rng)` with the CDF-row
    /// convention reconciled (`32768 - ours_cdf0 == oracle_cdf0`): **zero
    /// divergence**. So this is reconstruction arithmetic, not an entropy
    /// fork -- which is what separates it from the H1 rect-partition fork
    /// (that one forks `EC_SYMR` at `decodetxb.c:158` `txb_skip`).
    ///
    /// A second, far worse instance of the SAME defect is the 512x128
    /// `--cpu-used=2` stream (18779 wrong in the key frame, growing to
    /// 130708 by frame 5) -- but that stream ALSO forks entropy
    /// (459866 oracle reads vs 369136 ours), so it is not claimed here;
    /// it belongs to the H1 rect-partition lane. The pinned fixture below is
    /// the entropy-clean, fully-repaired instance.
    ///
    /// Non-vacuity: the fixture bytes are pinned (length + FNV-1a, like the
    /// other `ll444_*` pins), the header must really be `ss (0,0)` at 8-bit
    /// with `lossless == 1` (asserted from the parsed sequence header, not
    /// assumed), the tiled sweep gates' counter
    /// `decode::rect4_16_lossless_chroma_hits()` must FIRE (measured 80
    /// units on this fixture -- a 4:2:0 stream of the same content leaves it
    /// at 0, because a 4:2:0 lossless 16x4 pair is an 8x4 chroma block and
    /// takes the pair-merge, not this walk), and every frame must be
    /// byte-exact through BOTH the oracle `aomdec --rawvideo` (decode order,
    /// the 7-frame compare that the hidden-frame gates use) and ffmpeg
    /// (display order, 4:4:4 full-resolution chroma).
    #[test]
    fn a_lossless_444_rect16x4_chroma_reach_is_ss_aware() {
        const NAME: &str = "a_lossless_444_rect16x4_chroma_reach_is_ss_aware";
        const FIXTURE_LEN: usize = 77918;
        const FIXTURE_FNV: u64 = 0xe2a2_90ac_f249_4bf7;
        const W: usize = 256;
        const H: usize = 128;
        const FRAMES: usize = 6;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ll444_rect16x4_chroma_reach.obu");
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        // The header must really be the shape the fix is about: 4:4:4
        // (ss 0/0) at 8-bit. A fixture that silently became 4:2:0 would
        // take the pair-merge and make every arm below vacuous.
        {
            let mut p = Av1Parser::new();
            let mut pos = 0usize;
            let mut seq = None;
            while pos < stream.len() {
                let Ok(obu) = p.parse_obu(&stream[pos..]) else {
                    break;
                };
                pos += obu.total_size;
                if matches!(obu.kind, ObuKind::SequenceHeader(_)) {
                    seq = p.sequence_header();
                    break;
                }
            }
            let seq = seq.unwrap_or_else(|| panic!("{NAME}: no sequence header parsed"));
            assert_eq!(
                (
                    seq.color_config.subsampling_x,
                    seq.color_config.subsampling_y
                ),
                (0, 0),
                "{NAME}: fixture is not 4:4:4 (ss {}x{})",
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            );
            assert_eq!(
                seq.color_config.bit_depth, 8,
                "{NAME}: fixture bit depth moved"
            );
            assert_eq!(seq.max_frame_width, W as u32, "{NAME}: fixture width moved");
            assert_eq!(
                seq.max_frame_height, H as u32,
                "{NAME}: fixture height moved"
            );
        }

        let _guard = lock_gate_counters();
        crate::decode::reset_rect4_16_pair_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        let hits = crate::decode::rect4_16_lossless_chroma_hits();
        assert!(
            hits > 0,
            "{NAME}: gate is vacuous -- no lossless 16x4/4x16 chroma-pair unit was \
             walked (rect4_16_lossless_chroma_hits = {hits}), so the repaired \
             `tu_reach` call site never ran"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: shown frame count");
        for f in &frames {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }

        // The oracle, DECODE order, every frame including hidden alt-ref
        // frames. This is the arm that proves the repair against aomdec's
        // own reconstruction, not just ffmpeg's.
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                decoded >= FRAMES,
                "{NAME}: oracle decoded {decoded} frames, expected at least {FRAMES}"
            );
            let _ = hidden;
        }

        // ffmpeg's decoder: DISPLAY order, full-resolution chroma.
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
                }
            }
        } else {
            eprintln!("SKIP {NAME} ffmpeg arm: no ffmpeg");
        }
    }

    /// lane-av1-intrabc r4: the reviewer's unblock witness for the rect-strip
    /// reconstruction -- a CODED `use_intrabc` block (so its residual reader
    /// runs, not just the prediction copy) on a 2:1 strip in BOTH
    /// orientations, plus the `TX_MODE_SELECT` (var-tx) shape, every arm
    /// byte-exact against ffmpeg.
    ///
    /// The orientation counters (`intrabc_rect_coded_hits`) are the
    /// non-vacuity instrument: [`intrabc_rect_hits`] counts neither the skip
    /// flag nor the orientation, so an arm that silently became an all-skip
    /// stream (or a stream with only one orientation) would keep the old gate
    /// green while the coded residual path went untested
    /// (class `gate-blind-to-feature`).
    ///
    /// # The VERT arm is also the DV-PREDICTOR regression witness
    ///
    /// The 512x384 arm is the stream that exposed r4's defect: 528 of 294912
    /// luma samples were wrong, all inside one `skip == 1` intrabc copy at
    /// `mi(84,104)`. Its coded diff was identical to aomdec's and the post-read
    /// range matched exactly (`rng=33207` both), so nothing was misread -- the
    /// DV PREDICTOR differed. libaom picks `nearestmv` (`-520`) from the
    /// `INTRA_FRAME` MV stack; this decoder picked `-512`, one stack entry
    /// lower, because the scan that builds that stack walked further than
    /// libaom's. The scans step by the candidate BLOCK's own width
    /// (`mi_size_wide[bsize]`) over `xd->mi[]`, which holds an entry for every
    /// coded block; this decoder's MV grid held only the intrabc blocks, so a
    /// non-intrabc neighbour read as uncovered and the scan advanced that cell
    /// by one instead of by its real width. See `MiGrid::cover` for the
    /// measured walk. A stream that decodes the DV predictor from the wrong
    /// stack entry cannot pass this arm's pixel comparison.
    #[test]
    fn a_coded_rect_intrabc_block_reconstructs_in_both_orientations() {
        const NAME: &str = "a_coded_rect_intrabc_block_reconstructs_in_both_orientations";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const TS256: &str = "testsrc2=s=256x192:r=25";
        const TS512: &str = "testsrc2=s=512x384:r=25";
        // (arm, source, width, height, per-arm overrides, the coded orientation
        //  this arm must exercise, and whether the frame must code
        //  `TX_MODE_SELECT` with `allow_intrabc` still set)
        let arms: [(&str, &str, usize, usize, &[&str], u8, bool); 3] = [
            // HORZ (16x8) coded strip -- the shape the in-gate witness above
            // already reconstructs, pinned here with its own orientation counter.
            (
                "horz-coded-256-cq45-pal0-txs0",
                TS256,
                256,
                192,
                &[
                    "--cq-level=45",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
                0,
                false,
            ),
            // VERT (8x16) coded strip AND the DV-predictor regression stream.
            (
                "vert-coded-512-cq45-pal1-txs0",
                TS512,
                512,
                384,
                &[
                    "--cq-level=45",
                    "--enable-palette=1",
                    "--enable-tx-size-search=0",
                ],
                1,
                false,
            ),
            // The `TX_MODE_SELECT` (stream A) shape: `--enable-tx-size-search=1`
            // makes the frame code `tx_mode == Select`, so the intrabc block's
            // transform size comes off the INTER var-tx tree.
            (
                "txmode-select-256-cq45-pal0-txs1",
                TS256,
                256,
                192,
                &[
                    "--cq-level=45",
                    "--enable-palette=0",
                    "--enable-tx-size-search=1",
                ],
                0,
                true,
            ),
        ];
        let mut coded_total = [0usize; 2];
        let mut frames_compared = 0usize;
        for (arm, src, width, height, extra, want_orient, want_select) in &arms {
            let render = || {
                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        *src,
                        "-t",
                        "0.2",
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
                assert!(out.status.success(), "{NAME}: ffmpeg failed for {arm}");
                out.stdout
            };
            let y4m = render();
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args([
                        "--codec=av1",
                        "--bit-depth=8",
                        "--input-bit-depth=8",
                        "--passes=1",
                        "--end-usage=q",
                        "--cpu-used=0",
                        "--lag-in-frames=0",
                        "--kf-max-dist=1",
                        "--limit=1",
                        "--threads=1",
                        "--tile-columns=0",
                        "--enable-rect-partitions=1",
                        "--enable-1to4-partitions=1",
                        "--min-partition-size=8",
                        "--max-partition-size=32",
                        "--sb-size=64",
                        "--tune-content=screen",
                        "--enable-intrabc=1",
                    ])
                    // aomenc keeps the LAST occurrence of a repeated flag.
                    .args(*extra)
                    .args(["--obu", "-o", "-", "-"]),
                &y4m,
            );
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused {arm}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            if *want_select {
                let (select, premise) = tx_select_and_intrabc_frames(&stream);
                assert!(
                    select > 0 && premise > 0,
                    "{NAME}: {arm} no longer codes a frame with both tx_mode == Select and \
                     allow_intrabc (select={select} premise={premise}), so the TX_MODE_SELECT \
                     shape is untested here"
                );
            }
            crate::decode::reset_intrabc_rect_hits();
            let frames =
                decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: {arm} refused: {e}"));
            assert!(!frames.is_empty(), "{NAME}: {arm} decoded no frame");
            let coded = crate::decode::intrabc_rect_coded_hits();
            coded_total[0] += coded[0];
            coded_total[1] += coded[1];
            assert!(
                coded[*want_orient as usize] > 0,
                "{NAME}: {arm} reconstructed no CODED ({}) rect intrabc block -- the residual \
                 reader for this orientation is untested (coded horz/vert = {coded:?})",
                if *want_orient == 0 { "HORZ" } else { "VERT" }
            );
            // The ENCODE's frame count: the per-arm render (11756) uses `-t 0.2`
            // at TS256/TS512's `rate=25` (11700, 11701) -- 5 input frames -- and
            // the aomenc args carry `--limit=1` (11797), so all three arms code
            // exactly 1 frame. Asserted ABOVE the oracle call.
            assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
            let theirs = ffmpeg_decode_sequence(&stream, *width, *height, frames.len());
            assert_eq!(frames.len(), theirs.len(), "{NAME}: {arm} frame count");
            for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                assert!(
                    ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                    "{NAME}: {arm} frame {i} does not match ffmpeg"
                );
                frames_compared += 1;
            }
            eprintln!(
                "{NAME}: {arm} {} frame(s) exact, coded rect intrabc horz={} vert={}",
                frames.len(),
                coded[0],
                coded[1]
            );
        }
        assert!(
            coded_total[0] > 0 && coded_total[1] > 0,
            "{NAME}: gate is vacuous -- coded rect intrabc blocks seen: horz={} vert={}",
            coded_total[0],
            coded_total[1]
        );
        assert!(
            frames_compared > 0,
            "{NAME}: no frame was compared pixel-exact"
        );

        // lane-av1-intrabc r5: the SKIPPED VERT 2:1 strip -- the arm the
        // reviewer's sweep panicked on (a skipped 8x32 strip passed a
        // `bw * bh`-sized ZERO_RESIDUAL slice at stride `side`, and
        // `reconstruct_mc_rect` indexed `row * side + w` past its end).
        // Skip-heavy recipes (low cq, palette off AND on), each decoded
        // byte-exact vs ffmpeg on the inline reconstruction path (the arm
        // that panicked pre-fix). The skipped-orientation
        // counter is asserted per arm (`gate-blind-to-feature`): a recipe
        // that stops producing a skipped VERT strip fails instead of passing
        // vacuously.
        let _par_lock = lock_gate_counters();
        let skip_arms: [(&str, &str, usize, usize, &[&str]); 2] = [
            (
                "vert-skip-512-cq30-pal0-txs0",
                TS512,
                512,
                384,
                &[
                    "--cq-level=30",
                    "--enable-palette=0",
                    "--enable-tx-size-search=0",
                ],
            ),
            (
                "vert-skip-512-cq30-pal1-txs0",
                TS512,
                512,
                384,
                &[
                    "--cq-level=30",
                    "--enable-palette=1",
                    "--enable-tx-size-search=0",
                ],
            ),
        ];
        for (arm, src, width, height, extra) in &skip_arms {
            let y4m = {
                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        *src,
                        "-t",
                        "0.2",
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
                assert!(out.status.success(), "{NAME}: ffmpeg failed for {arm}");
                out.stdout
            };
            let out = run_with_stdin(
                Command::new(aomenc_path())
                    .args([
                        "--codec=av1",
                        "--bit-depth=8",
                        "--input-bit-depth=8",
                        "--passes=1",
                        "--end-usage=q",
                        "--cpu-used=0",
                        "--lag-in-frames=0",
                        "--kf-max-dist=1",
                        "--limit=1",
                        "--threads=1",
                        "--tile-columns=0",
                        "--enable-rect-partitions=1",
                        "--enable-1to4-partitions=1",
                        "--min-partition-size=8",
                        "--max-partition-size=32",
                        "--sb-size=64",
                        "--tune-content=screen",
                        "--enable-intrabc=1",
                    ])
                    // aomenc keeps the LAST occurrence of a repeated flag.
                    .args(*extra)
                    .args(["--obu", "-o", "-", "-"]),
                &y4m,
            );
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused {arm}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            // The inline reconstruction path (the arm that panicked
            // pre-fix) must match ffmpeg.
            let mut skipped_vert = 0usize;
            for threads in [1usize] {
                crate::decode::reset_intrabc_rect_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        panic!("{NAME}: {arm} refused at {threads} recon thread(s): {e}")
                    }
                };
                let skipped = crate::decode::intrabc_rect_skipped_hits();
                assert!(
                    !frames.is_empty(),
                    "{NAME}: {arm} decoded no frame at {threads} recon thread(s)"
                );
                // The ENCODE's frame count: the skip_arms encode (11904) renders
                // TS512 with `-t 0.2` at `rate=25` (11907) and its aomenc runs
                // with `--limit=1` (11945), so the stream holds 1 coded frame --
                // and this loop re-decodes those SAME bytes at each `threads`
                // value, so the count is arm-invariant. Asserted ABOVE the call.
                assert_eq!(
                    frames.len(),
                    1,
                    "{NAME}: {arm} key frame at {threads} recon thread(s)"
                );
                let theirs = ffmpeg_decode_sequence(&stream, *width, *height, frames.len());
                assert_eq!(
                    frames.len(),
                    theirs.len(),
                    "{NAME}: {arm} frame count at {threads} recon thread(s)"
                );
                for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                    assert!(
                        ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                        "{NAME}: {arm} frame {i} does not match ffmpeg at {threads} recon thread(s)"
                    );
                }
                skipped_vert = skipped[1];
                eprintln!(
                    "{NAME}: {arm} {} frame(s) exact at {threads} recon thread(s), skipped rect intrabc horz={} vert={}",
                    frames.len(),
                    skipped[0],
                    skipped[1]
                );
            }
            assert!(
                skipped_vert > 0,
                "{NAME}: {arm} reconstructed no SKIPPED VERT rect intrabc strip -- the r5 stride arm is untested"
            );
        }
    }

    /// lane-t900 r34, the WITNESS (r33 pinned it ignored) that intrabc under
    /// a 128x128 superblock decodes pixel-exact.
    ///
    /// All four arms of r33's census -- `--sb-size=128 --tune-content=screen
    /// --enable-intrabc=1` over both `--enable-tx-size-search` settings and
    /// two quantisers, on a 2x2 tiling of a 128x96 bar pattern (256x192,
    /// 8-bit, `--cpu-used=0 --min-partition-size=8 --max-partition-size=32`,
    /// rect and 1:4 off) -- code `use_128x128_superblock && allow_intrabc`.
    ///
    /// The lift was one constant: `av1_find_ref_dv` (mvref_common.c) takes
    /// `mib_size`, the SEQUENCE's superblock size in mi units, so the
    /// fallback DV is one 128-px superblock up (or 128 + `INTRABC_DELAY_PIXELS`
    /// left on the tile's first superblock row) under a 128 superblock, where
    /// [`crate::decode::read_intrabc_dv`] hardcoded 64. Every arm is compared
    /// to ffmpeg and the decoded-intrabc-block counter is asserted non-zero,
    /// so a stream that stopped using intrabc could not make this vacuous.
    ///
    /// lane-av1-intrabc (2026-09-17): the four recipes now run at 10 bits as
    /// well, and both depths must SHOW the engagement -- `blocks_at_depth`
    /// below hard-asserts a decoded intrabc block per depth. The 10-bit
    /// coverage the guard records for `enable-intrabc` used to rest on a gate
    /// that only SPELLED `--enable-intrabc=1`: measured (same round), all five
    /// arms of `a_real_aomenc_rect_strip_palette_decodes_pixel_exact` decode
    /// ZERO intrabc blocks at BOTH depths, so the spelling proved nothing and
    /// the entry was one encoder-RD change away from being vacuous again
    /// (class `tool-disabled-in-every-gate`). An arm here that stops engaging
    /// intrabc now fails this gate instead of retiring the entry unproven.
    #[test]
    fn an_sb128_screen_stream_with_intrabc_decodes_pixel_exact() {
        const NAME: &str = "an_sb128_screen_stream_with_intrabc_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (256usize, 192usize);
        let _guard = lock_gate_counters();
        let (mut blocks_total, mut frames_compared) = (0usize, 0u32);
        let mut blocks_at_depth = [0usize; 2];
        for depth in [8usize, 10] {
            for txs in ["0", "1"] {
                for cq in ["30", "45"] {
                    let arm = format!("sb128 {depth}-bit txs={txs} cq={cq}");
                    let stream = screen_intrabc_stream_at_depth(
                        "smptebars=size=128x96:rate=25",
                        cq,
                        txs,
                        true,
                        true,
                        depth,
                        &["--sb-size=128"],
                    );
                    let (sb128, intrabc_frames) = sb128_and_intrabc_frames(&stream);
                    assert!(
                        sb128 && intrabc_frames > 0,
                        "{NAME}: {arm} no longer codes sb128 + allow_intrabc"
                    );
                    crate::decode::reset_intrabc_hits();
                    let frames =
                        decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: {arm}: {e}"));
                    let blocks = crate::decode::intrabc_hits();
                    blocks_total += blocks;
                    blocks_at_depth[usize::from(depth == 10)] += blocks;
                    // The ENCODE's frame count:
                    // `screen_intrabc_stream_at_depth` (12203) renders
                    // `smptebars=size=128x96:rate=25` with `-t 0.2` and
                    // `-vf tile=2x2` (12066, 12069) -- 5 frames in, 1 tiled frame
                    // out at 256x192 -- and its aomenc runs with `--limit=1`
                    // (12101), so all eight (depth, txs, cq) arms code exactly
                    // 1 frame. Asserted ABOVE the oracle call.
                    assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
                    let theirs = if depth == 10 {
                        ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
                    } else {
                        ffmpeg_decode_sequence(&stream, width, height, frames.len())
                    };
                    assert_eq!(frames.len(), theirs.len(), "{NAME}: {arm} frame count");
                    for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                        assert!(
                            ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                            "{NAME}: {arm} frame {i} does not match ffmpeg"
                        );
                        frames_compared += 1;
                    }
                    eprintln!(
                        "{NAME}: {arm} intrabc_frames={intrabc_frames} intrabc_blocks={blocks} \
                         DECODED {} frame(s) exact",
                        frames.len()
                    );
                }
            }
        }
        assert!(frames_compared > 0, "{NAME}: no frame was compared");
        assert!(
            blocks_total > 0,
            "{NAME}: gate is vacuous -- no arm decoded a single intrabc block"
        );
        assert!(
            blocks_at_depth[0] > 0 && blocks_at_depth[1] > 0,
            "{NAME}: 8-bit={} 10-bit={} -- an arm at that depth no longer decodes an intrabc \
             block, so `enable-intrabc` coverage there would rest on the flag spelling alone",
            blocks_at_depth[0],
            blocks_at_depth[1]
        );
    }

    /// lane-av1-ibcrect: `use_intrabc` on a 128-root HORZ/VERT strip
    /// (BLOCK_128X64 / BLOCK_64X128) decodes PIXEL-EXACT against the
    /// instrumented oracle -- the witness the refusal inventory names after
    /// `decode_block_128rect`'s rect-strip intrabc refusal was lifted
    /// (`decode_intrabc_128rect`). The square-block twin is
    /// [`an_sb128_screen_stream_with_intrabc_decodes_pixel_exact`] above.
    ///
    /// The recipes are the hunt's proven arms (lanes/ibcrect r1/r2, the same
    /// `--sb-size=128 --min-partition-size=64 --enable-rect-partitions=1
    /// --enable-ab-partitions=0 --enable-1to4-partitions=0` partition recipe
    /// [`a_real_aomenc_sb128_intra_rect_block_decodes_pixel_exact`] uses, plus
    /// `--tune-content=screen --enable-intrabc=1` over the DETERMINISTIC geq
    /// sinusoid sources -- lavfi `gradients` ignores its seed and re-rolls
    /// every run, so a "flake" there was really this):
    ///
    /// * HORZ arm: 384x320 cq32, 5 all-key frames -- 14 unskipped 128x64
    ///   intrabc blocks (`skip=false`, real residual reads);
    /// * VERT arm: 512x512 cq45, 5 all-key frames -- an unskipped 64x128
    ///   intrabc block at mi(64,96).
    ///
    /// Both arms HARD-assert `intrabc_128rect_hits() > 0` and decode-order
    /// pixel-exactness of EVERY frame (hidden alt-refs included) against the
    /// oracle's own dump, so a stream that stopped engaging the shape fails
    /// the gate instead of passing vacuously (class `gate-blind-to-feature`).
    ///
    /// The 10-bit arm engages the shape. Hunt r2 and the witness re-run both
    /// measured `intrabc_128rect_hits` +15 on arm 3, so aomenc does take a
    /// 128-root HORZ/VERT strip at 10 bit under this recipe. There is no
    /// per-depth coverage gap to print. The gate's hard asserts already
    /// cover that arm.
    ///
    /// The chroma context this shape reads was the r2 defect: per-unit chroma
    /// entropy states were stamped inside the mu-chunk loop, then the
    /// end-of-block `record_split_luma_rect_mi` re-stamped BOTH planes over
    /// the whole 128x64 span with ONE composed state -- level = the SUM of
    /// both 64x64 chunks' units, dc = chunk-0's top-left (class
    /// `override-slot-on-one-arm`; the inter-128 path fixed the same class
    /// with its `mu_chroma_units` replay, decode.rs). The next block then
    /// read `txb_skip` off the smeared state.
    #[test]
    fn an_sb128_rect_strip_with_intrabc_decodes_pixel_exact() {
        const NAME: &str = "an_sb128_rect_strip_with_intrabc_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // (tag, width, height, cq, geq luma, 10-bit)
        // The geq sources are the hunt's GEO_SMOOTH / GEO_SMOOTH_512,
        // byte-identical across runs (see the sb128 intra-rect gate's note).
        let arms: [(u32, usize, usize, &str, &str, bool); 3] = [
            (
                1,
                384,
                320,
                "32",
                "128+90*sin((X+2*N)/37)+30*sin(Y/29)",
                false,
            ),
            (
                2,
                512,
                512,
                "45",
                "128+80*sin((X+3*N)/17)+50*sin(Y/29)+30*sin((X+Y)/13)",
                false,
            ),
            (
                3,
                384,
                320,
                "32",
                "128+90*sin((X+2*N)/37)+30*sin(Y/29)",
                true,
            ),
        ];
        let mut engaged_arms = 0u32;
        let mut vert_arms = 0u32;
        for (tag, width, height, cq, lum, ten_bit) in arms {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!(
                        "nullsrc=size={width}x{height}:rate=25,format=yuv420p,\
                         geq=lum='{lum}':\
                         cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'"
                    ),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.2",
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
            let cq_arg = format!("--cq-level={cq}");
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq_arg,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--kf-max-dist=1",
                "--limit=5",
                "--tile-columns=0",
                "--sb-size=128",
                "--min-partition-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--tune-content=screen",
                "--enable-intrabc=1",
                "--deltaq-mode=0",
                "--enable-tx-size-search=0",
            ];
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the arm {tag} fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = crate::decode::intrabc_128rect_hits();
            let (frames, hidden) = decode_all_frames_vs_oracle(
                &stream,
                &format!("ibcrect-strip-{tag}-{width}x{height}"),
            );
            let hits = crate::decode::intrabc_128rect_hits() - before;
            eprintln!(
                "{NAME}: arm {tag} {width}x{height} cq={cq} {}bit: {frames} decode-order frames \
                 pixel-exact ({hidden} hidden), intrabc_128rect_hits=+{hits}",
                if ten_bit { 10 } else { 8 },
            );
            if hits > 0 {
                engaged_arms += 1;
                // The VERT arm is tag 2; its engagement is what proves the
                // 64x128 orientation, not just 128x64.
                if tag == 2 {
                    vert_arms += 1;
                }
            }
        }
        assert!(
            engaged_arms > 0,
            "{NAME}: {engaged_arms} arm(s) decoded pixel-exact but NOT ONE decoded a 128-root \
             rect-strip intrabc block -- the shape this gate exists for was never engaged"
        );
        assert!(
            vert_arms > 0,
            "{NAME}: the VERT (64x128) orientation was never engaged -- only 128x64 HORZ strips \
             proved the lift"
        );
    }

    /// lane-av1intrapred r2, REGRESSION GATE for the two intrabc TX4-split
    /// chroma defects this lane fixed.
    ///
    /// An intrabc 8x8 leaf whose var-tx tree split to TX_4X4 must (a) arm
    /// `INTRABC_CHROMA_TX` from the co-located luma TU's coded type -- the
    /// chroma units then take that type's class-1 eob row, HORIZ scan and nz
    /// contexts (libaom `av1_get_tx_type`'s is_inter branch; intrabc is
    /// inter-classified) -- and (b) predict the chroma units from the FRAME
    /// COPY at the DV, not intra-from-UV-mode (intrabc's UV mode is DC, so a
    /// leaf that loses the override predicts flat DC where libaom copies a
    /// busy region).
    ///
    /// Pre-r2, decode_leaf8's TX4 arm did neither: R5's chroma U txb at
    /// mi (36,66) decoded `eob=11` off the 2D row where aomdec decoded
    /// `eob=13` off the class-1 row, and the first surviving pixel fork was
    /// U(184,4) of mi (2,92) (class `intrabc-chroma-tx-not-armed`, then
    /// `intrabc-chroma-predicts-intra`).
    ///
    /// The fixture is the report's R5 stream pinned into `fixtures/`
    /// (aomenc oracle, testsrc2 320x180 tiled 2x2 -> 640x360, sb64 maxp64
    /// square-only screen intrabc, cq20, 1 frame; 7173 bytes,
    /// sha256 bc699af4a6e7c1c5657f041e8cbdf2d3d899ddb80ec2a549a950b7cebb5b41bd).
    /// The gate needs no oracle at run time and cannot be skipped: a missing
    /// fixture, a decode error, a zero counter, a moved pixel patch or a
    /// changed full-frame fingerprint each fail by name. The fingerprint is
    /// FNV-1a (dependency-free, same rationale as the cdf-forwarding gate's).
    #[test]
    fn an_intrabc_tx4_leaf_chroma_inherits_the_luma_type_and_predicts_from_the_frame_copy() {
        const NAME: &str =
            "an_intrabc_tx4_leaf_chroma_inherits_the_luma_type_and_predicts_from_the_frame_copy";
        const FIXTURE_LEN: usize = 7173;
        const FIXTURE_FNV: u64 = 0x3a57_d075_5e4c_fe5b;
        const RAW_LEN: usize = 640 * 360 * 3 / 2;
        const RAW_FNV: u64 = 0xf271_df27_ce4d_ba66;
        // The 4x4 chroma patches at (184,4) -- the first surviving pixel fork
        // -- as the oracles (aomdec and ffmpeg agree) reconstruct them.
        const U_PATCH: [u16; 16] = [
            86, 116, 55, 110, 114, 114, 51, 83, 99, 121, 48, 114, 103, 117, 102, 67,
        ];
        const V_PATCH: [u16; 16] = [
            179, 158, 199, 105, 155, 158, 195, 147, 163, 153, 194, 108, 170, 148, 168, 172,
        ];

        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/intrabc_tx4_chroma_kf.obu");
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
        crate::decode::reset_intrabc_tx4_chroma_copy_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream no longer decodes cleanly: {e}"));
        assert_eq!(frames.len(), 1, "{NAME}: frame count");
        let frame = &frames[0];
        assert_eq!(
            (frame.width, frame.height),
            (640, 360),
            "{NAME}: dimensions"
        );

        // Non-vacuity, both halves: the leaf shape must be reached at all,
        // and its chroma route must have consulted the ARMED slot (pre-r2
        // this counter is 0 -- the arm never armed the slot).
        assert!(
            crate::decode::leaf8_intrabc_hits() > 0,
            "{NAME}: gate is vacuous -- no intrabc 8x8 leaf was decoded"
        );
        assert!(
            crate::decode::intrabc_tx4_chroma_copy_hits() > 0,
            "{NAME}: gate is vacuous -- no intrabc TX4-split leaf's chroma read consulted \
             the armed slot (class intrabc-chroma-tx-not-armed)"
        );

        // The first surviving pixel fork: chroma 4x4 at (184,4). The wrong
        // wirings put flat intra-DC predictions or the wrong eob class here.
        let cw = frame.width / 2;
        let u_patch: Vec<u16> = (0..4)
            .flat_map(|dy| (0..4).map(move |dx| frame.u[(4 + dy) * cw + 184 + dx]))
            .collect();
        let v_patch: Vec<u16> = (0..4)
            .flat_map(|dy| (0..4).map(move |dx| frame.v[(4 + dy) * cw + 184 + dx]))
            .collect();
        assert_eq!(u_patch, U_PATCH, "{NAME}: U(184,4) patch moved");
        assert_eq!(v_patch, V_PATCH, "{NAME}: V(184,4) patch moved");
        // The unit the FRAME-COPY override specifically pins: chroma 4x4 at
        // (168,0) is an intrabc TX4-split leaf's chroma whose copy source is
        // a busy ramp region; without the `palette_uv_bufs` intrabc wrap the
        // read predicts flat intra-DC here (78k chroma bytes drift from this
        // unit alone), with it the copy reproduces the oracle exactly.
        const U_COPY_PATCH: [u16; 16] = [
            91, 91, 91, 91, 120, 120, 120, 120, 120, 120, 120, 120, 73, 102, 116, 116,
        ];
        const V_COPY_PATCH: [u16; 16] = [
            241, 241, 241, 241, 156, 156, 156, 156, 158, 158, 158, 158, 181, 163, 154, 154,
        ];
        let u_copy: Vec<u16> = (0..4)
            .flat_map(|dy| (0..4).map(move |dx| frame.u[dy * cw + 168 + dx]))
            .collect();
        let v_copy: Vec<u16> = (0..4)
            .flat_map(|dy| (0..4).map(move |dx| frame.v[dy * cw + 168 + dx]))
            .collect();
        assert_eq!(
            u_copy, U_COPY_PATCH,
            "{NAME}: U(168,0) patch moved -- the intrabc TX4 chroma read no longer \
             predicts from the frame copy (class intrabc-chroma-predicts-intra)"
        );
        assert_eq!(v_copy, V_COPY_PATCH, "{NAME}: V(168,0) patch moved");

        // The whole frame, packed as the rawvideo byte stream the oracles
        // produce: any remaining byte anywhere in Y/U/V fails the gate.
        let mut raw = Vec::with_capacity(RAW_LEN);
        for plane in [&frame.y, &frame.u, &frame.v] {
            raw.extend(plane.iter().map(|&s| s as u8));
        }
        assert_eq!(raw.len(), RAW_LEN, "{NAME}: raw size");
        assert_eq!(
            fnv1a64(&raw),
            RAW_FNV,
            "{NAME}: full-frame fingerprint moved"
        );
    }

    /// lane-t900 r33, CENSUS for "a sub-8x8 leaf that uses intrabc".
    ///
    /// The `use_intrabc` symbol IS read at that reader for every sub-8x8 leaf
    /// of an `allow_intrabc` frame, so arrival is provable rather than
    /// assumed: the arms below force 4x4 leaves (`--min-partition-size=4`) on
    /// the exact-repetition screen content that keeps `allow_intrabc`, and the
    /// gate requires both counters -- `allow_intrabc` frames and decoded sub-8
    /// leaves -- to be non-zero before it reads anything into the refusal
    /// firing 0 times (class `gate-blind-to-feature`).
    #[test]
    fn a_sub8_leaf_census_over_intrabc_screen_streams_measures_the_sub8_refusal() {
        const NAME: &str =
            "a_sub8_leaf_census_over_intrabc_screen_streams_measures_the_sub8_refusal";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (256usize, 192usize);
        let _guard = lock_gate_counters();
        let (mut premise_arms, mut reached, mut sub8_total, mut frames_compared) =
            (0u32, 0u32, 0usize, 0u32);
        for cq in ["30", "45"] {
            let arm = format!("sub8 cq={cq}");
            let stream = screen_intrabc_stream_with(
                "smptebars=size=128x96:rate=25",
                cq,
                "0",
                true,
                true,
                &["--min-partition-size=4"],
            );
            let (_, intrabc_frames) = sb128_and_intrabc_frames(&stream);
            if intrabc_frames > 0 {
                premise_arms += 1;
            }
            // No reset for this counter, so measure the delta (the gate lock
            // above keeps another gate's decode out of it).
            let sub8_before = crate::decode::sub8_split_hits();
            match decode_stream(&stream) {
                Ok(frames) => {
                    let sub8 = crate::decode::sub8_split_hits() - sub8_before;
                    sub8_total += sub8;
                    // The ENCODE's frame count: `screen_intrabc_stream_with` (12593)
                    // renders `smptebars=size=128x96:rate=25` with `-t 0.2` and
                    // `-vf tile=2x2` (12066, 12069) -- 5 frames in, 1 tiled frame
                    // out -- and its aomenc runs with `--limit=1` (12101), so both
                    // cq arms code exactly 1 frame. The census is report-only about
                    // the COUNTS, but its Ok arm asserts pixel-exactness, so the
                    // budget handed to ffmpeg is pinned here rather than derived
                    // from the decode under test.
                    assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
                    let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                    assert_eq!(frames.len(), theirs.len(), "{NAME}: {arm} frame count");
                    for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                        assert!(
                            ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                            "{NAME}: {arm} frame {i} does not match ffmpeg"
                        );
                        frames_compared += 1;
                    }
                    eprintln!(
                        "{NAME}: {arm} intrabc_frames={intrabc_frames} sub8_leaves={sub8} \
                         DECODED {} frame(s) exact",
                        frames.len()
                    );
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: {arm} decode failed: {e}"
                    );
                    if msg.contains("a sub-8x8 leaf that uses intrabc") {
                        reached += 1;
                    }
                    sub8_total += crate::decode::sub8_split_hits() - sub8_before;
                    eprintln!("{NAME}: {arm} intrabc_frames={intrabc_frames} REFUSED: {e}");
                }
            }
        }
        eprintln!(
            "{NAME}: {premise_arms}/2 arm(s) keep allow_intrabc, {sub8_total} sub-8 leaf/leaves \
             decoded, {reached} reach the refusal, {frames_compared} frame(s) exact"
        );
        assert!(
            premise_arms > 0,
            "{NAME}: census is vacuous -- no arm kept allow_intrabc, so the refusal's own \
             premise was never within reach"
        );
        assert!(
            sub8_total > 0,
            "{NAME}: census is vacuous -- no sub-8 leaf was decoded at all, so the \
             `use_intrabc` symbol this refusal guards was never read"
        );
    }

    /// lane-t900 r33, CENSUS for "an intrabc block whose var-tx tree resolved
    /// to mixed leaf transform sizes" -- the residue lane-t900 r32 left when
    /// it lifted the intrabc `TX_MODE_SELECT` refusal.
    ///
    /// The tree is read in full; lane-av1-ibcvtx taught the luma reconstruct
    /// to walk the tree's own leaf list, so mixed trees now decode. This
    /// census sweeps the quantiser across the exact-repetition recipe that
    /// reaches the tree at all and requires the tree to actually be read
    /// (`intrabc_vartx_hits`); it stays as the premise witness for the lifted
    /// refusal (`reached` is 0 by construction now).
    #[test]
    fn an_intrabc_vartx_census_measures_the_mixed_leaf_refusal() {
        const NAME: &str = "an_intrabc_vartx_census_measures_the_mixed_leaf_refusal";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (256usize, 192usize);
        let _guard = lock_gate_counters();
        let (mut vartx_total, mut reached, mut frames_compared) = (0usize, 0u32, 0u32);
        for cq in ["20", "30", "45", "55"] {
            let arm = format!("vartx cq={cq}");
            let stream =
                screen_intrabc_stream("smptebars=size=128x96:rate=25", cq, "1", true, true);
            let (select, premise) = tx_select_and_intrabc_frames(&stream);
            crate::decode::reset_intrabc_vartx_hits();
            match decode_stream(&stream) {
                Ok(frames) => {
                    let vartx = crate::decode::intrabc_vartx_hits();
                    vartx_total += vartx;
                    // The ENCODE's frame count: `screen_intrabc_stream` (12679)
                    // renders `smptebars=size=128x96:rate=25` with `-t 0.2` and
                    // `-vf tile=2x2` (12066, 12069) -- 5 frames in, 1 tiled frame
                    // out -- and its aomenc runs with `--limit=1` (12101), so all
                    // four cq arms code exactly 1 frame. The census is
                    // report-only about the COUNTS, but its Ok arm does assert
                    // pixel-exactness, so the budget it hands ffmpeg is pinned
                    // here rather than derived from the decode under test.
                    assert_eq!(frames.len(), 1, "{NAME}: {arm} key frame");
                    let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                    assert_eq!(frames.len(), theirs.len(), "{NAME}: {arm} frame count");
                    for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                        assert!(
                            ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                            "{NAME}: {arm} frame {i} does not match ffmpeg"
                        );
                        frames_compared += 1;
                    }
                    eprintln!(
                        "{NAME}: {arm} select={select} premise={premise} vartx_blocks={vartx} \
                         DECODED {} frame(s) exact",
                        frames.len()
                    );
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: {arm} decode failed: {e}"
                    );
                    vartx_total += crate::decode::intrabc_vartx_hits();
                    if msg.contains("mixed leaf transform sizes") {
                        reached += 1;
                    }
                    eprintln!("{NAME}: {arm} select={select} premise={premise} REFUSED: {e}");
                }
            }
        }
        eprintln!(
            "{NAME}: {vartx_total} intrabc block(s) read the var-tx tree, {reached} resolved to \
             mixed leaves, {frames_compared} frame(s) exact"
        );
        assert!(
            vartx_total > 0,
            "{NAME}: census is vacuous -- no intrabc block read the var-tx tree at all"
        );
        assert_eq!(
            reached, 0,
            "{NAME}: an intrabc var-tx tree was REFUSED -- the mixed-leaf refusal \
             is lifted (lane-av1-ibcvtx) and must stay gone"
        );
    }

    /// lane-av1-ibcvtx, capability gate for the LIFTED refusal "an intrabc
    /// block whose var-tx tree resolved to mixed leaf transform sizes": the
    /// t900 r33 census recipe class (screen content, square partitions,
    /// tx-size search) reaches mixed trees the moment the canvas carries
    /// testsrc2's texture and blocks grow past 32 -- on the PRE-fix tree this
    /// exact stream refused by name on its first mixed tree (5 of the 12
    /// hunt recipes hit it, `lanes/av1ibcvtx.report.md`).
    ///
    /// The luma reconstruct walks the tree's own leaf list per leaf (libaom
    /// `decode_token_recon_block`), and the block's chroma inherits the
    /// co-located luma unit's coded tx type (`av1_get_tx_type`,
    /// `blockd.h:1278`) -- the inheritance the uniform multi-TU intrabc path
    /// was missing as well: symbol-aligned against an instrumented aomdec,
    /// the 640x360 cq20 witness desynced inside the FIRST chroma unit of its
    /// first mixed tree until the arm landed, then matched every subsequent
    /// read.
    ///
    /// NOT yet asserted: full pixel-exactness against ffmpeg. A second,
    /// PRE-EXISTING defect that is not this refusal -- a NON-intrabc intra
    /// block (R5 witness: mi (32,32), 64x64, uniform 4x TX_32X32 tree, every
    /// coefficient all_zero, reads symbol-identical to aomdec) mispredicts
    /// isolated samples on testsrc2 content (first divergence px (128,296)
    /// of the 640x360 cq20 recipe) and cascades over roughly a third of the
    /// frame. It is pinned in `lanes/av1ibcvtx.report.md`; when it lands,
    /// tighten this gate to a full pixel-exact assert.
    #[test]
    fn a_real_aomenc_intrabc_mixed_vartx_tree_decodes_without_the_mixed_leaf_refusal() {
        const NAME: &str =
            "a_real_aomenc_intrabc_mixed_vartx_tree_decodes_without_the_mixed_leaf_refusal";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (640usize, 360usize);
        let _guard = lock_gate_counters();
        let stream = screen_intrabc_stream_with(
            "testsrc2=size=320x180:rate=25",
            "30",
            "1",
            true,
            true,
            &["--max-partition-size=64"],
        );
        let (select, premise) = tx_select_and_intrabc_frames(&stream);
        assert!(
            premise > 0,
            "{NAME}: vacuous -- no frame keeps allow_intrabc under TxMode::Select, \
             so the refusal's premise was never within reach (select={select})"
        );
        crate::decode::reset_intrabc_vartx_hits();
        crate::decode::reset_intrabc_vartx_mixed_leaves_hits();
        match decode_stream(&stream) {
            Ok(frames) => {
                let trees = crate::decode::intrabc_vartx_hits();
                let mixed = crate::decode::intrabc_vartx_mixed_leaves_hits();
                assert!(
                    trees > 0 && mixed > 0,
                    "{NAME}: vacuous -- {trees} var-tx tree(s), {mixed} mixed: the \
                     capability this gate pins was never exercised"
                );
                // The ENCODE's frame count: `screen_intrabc_stream_with` (12766)
                // renders `testsrc2=size=320x180:rate=25` with `-t 0.2` and
                // `-vf tile=2x2` (12066, 12069) -- 5 frames in, 1 tiled frame out
                // at 640x360 -- and its aomenc runs with `--limit=1` (12101), so
                // the stream codes exactly 1 frame. Asserted ABOVE the call.
                assert_eq!(frames.len(), 1, "{NAME}: key frame");
                let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                assert_eq!(frames.len(), theirs.len(), "{NAME}: frame count");
                let mut mismatched = 0usize;
                for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                    // A size mismatch makes every number this ladder prints
                    // meaningless -- the `zip` walks below would truncate to the
                    // shorter plane and report a clean ladder over what is
                    // really a plane-shape defect -- so it is LOUD. A PIXEL
                    // mismatch stays report-only: this gate deliberately
                    // tolerates the pinned second defect (see the doc comment).
                    for (plane, a, b) in [
                        ("Y", &ours.y, &ref_frame.y),
                        ("U", &ours.u, &ref_frame.u),
                        ("V", &ours.v, &ref_frame.v),
                    ] {
                        assert_eq!(
                            a.len(),
                            b.len(),
                            "{NAME}: frame {i} plane {plane}: ours has {} samples, ffmpeg's has \
                             {} -- a size mismatch makes this whole diagnostic ladder \
                             meaningless, so it is loud while a pixel mismatch stays \
                             report-only",
                            a.len(),
                            b.len()
                        );
                    }
                    mismatched += ours
                        .y
                        .iter()
                        .zip(ref_frame.y.iter())
                        .filter(|(x, y)| x != y)
                        .count();
                    mismatched += ours
                        .u
                        .iter()
                        .zip(ref_frame.u.iter())
                        .filter(|(x, y)| x != y)
                        .count();
                    mismatched += ours
                        .v
                        .iter()
                        .zip(ref_frame.v.iter())
                        .filter(|(x, y)| x != y)
                        .count();
                }
                eprintln!(
                    "{NAME}: select={select} premise={premise} vartx_blocks={trees} \
                     mixed_trees={mixed}, {mismatched} sample(s) differ from ffmpeg \
                     (the pinned non-intrabc second defect, see the doc comment)"
                );
            }
            Err(e) => panic!(
                "{NAME}: the mixed-leaf refusal is lifted (lane-av1-ibcvtx); the \
                 witness stream must decode, not stop: {e}"
            ),
        }
    }

    /// lane-t900 r31, CENSUS for the refusal "an intrabc block under
    /// TxMode::Select" -- which this gate DISPROVES as an "no encoder writes
    /// one" claim and pins as a real capability gap instead.
    ///
    /// The premise is reachable by the SPEC: `tx_mode` (5.9.21) is forced to
    /// `ONLY_4X4` only under `CodedLossless`, while `allow_intrabc` (5.9.2)
    /// needs `allow_screen_content_tools` at unscaled resolution -- two
    /// independent conditions. libaom leaves it open too: `select_tx_mode`
    /// (encodeframe.c:2355) never consults `allow_intrabc`, and the only
    /// downgrade to `TX_MODE_LARGEST` is `txb_split_count == 0`
    /// (encodeframe.c:2690).
    ///
    /// The measurement over 14 real `--tune-content=screen --enable-intrabc=1`
    /// recipes:
    ///
    /// * the 10 non-repeating arms never present the premise at all -- with
    ///   `--enable-tx-size-search=1` the frame codes `TX_MODE_SELECT` but RD
    ///   never picks intrabc, so libaom clears `allow_intrabc` again; with
    ///   `=0` intrabc IS used and the frame is `TX_MODE_LARGEST`. That
    ///   correlation, not any rule, is why earlier rounds never saw this;
    /// * the four EXACT-REPETITION arms (a 2x2 tiling of one 128x96 bar
    ///   pattern) all code `allow_intrabc && tx_mode == Select`. Two of them
    ///   stop on the older rect-strip intrabc refusal (the shape that fires
    ///   first, class `refusal-hides-a-defect`), and the two with rect/1:4
    ///   partitions off put intrabc on a SQUARE block: `cq=30` reaches THIS
    ///   refusal by name, `cq=45`'s intrabc blocks are all `skip` and decode
    ///   pixel-exact against ffmpeg.
    ///
    /// So the refusal is correct and load-bearing, exactly one arm reaches it,
    /// and the lift it names has a waiting witness:
    /// [`an_intrabc_block_under_tx_mode_select_decodes_pixel_exact`] (ignored
    /// until the intra path reads the inter var-tx tree for an intrabc block).
    #[test]
    fn an_intrabc_census_over_screen_key_frames_measures_the_tx_select_refusal() {
        const NAME: &str =
            "an_intrabc_census_over_screen_key_frames_measures_the_tx_select_refusal";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const TS: &str = "testsrc2=s=256x192:r=25";
        const SB: &str = "smptebars=size=256x192:rate=25";
        const PB: &str = "pal75bars=size=256x192:rate=25";
        const TILE: &str = "smptebars=size=128x96:rate=25";
        let (width, height) = (256usize, 192usize);
        // (arm, source, cq, tx-size-search, tiled, square_only)
        let arms: [(&str, &str, &str, &str, bool, bool); 14] = [
            ("smptebars-cq40-txs1", SB, "40", "1", false, false),
            ("smptebars-cq45-txs1", SB, "45", "1", false, false),
            ("smptebars-cq55-txs1", SB, "55", "1", false, false),
            ("smptebars-cq63-txs1", SB, "63", "1", false, false),
            ("pal75bars-cq40-txs1", PB, "40", "1", false, false),
            ("pal75bars-cq55-txs1", PB, "55", "1", false, false),
            ("testsrc2-cq50-txs1", TS, "50", "1", false, false),
            ("testsrc2-cq63-txs1", TS, "63", "1", false, false),
            ("smptebars-cq45-txs0", SB, "45", "0", false, false),
            ("pal75bars-cq45-txs0", PB, "45", "0", false, false),
            ("tiled-cq30-txs1", TILE, "30", "1", true, false),
            ("tiled-cq45-txs1", TILE, "45", "1", true, false),
            ("tiled-square-cq30-txs1", TILE, "30", "1", true, true),
            // lane-t900 r32: both square arms are now ORDINARY census members
            // -- cq30 (unskipped intrabc, the inter var-tx tree) and cq45
            // (all-skip intrabc, `maxRectTxSize` with no symbol at all) decode
            // pixel-exact, so nothing here reaches the lifted refusal.
            ("tiled-square-cq45-txs1", TILE, "45", "1", true, true),
        ];
        let (mut select_arms, mut premise_arms, mut intrabc_arms, mut blocks_total) =
            (0u32, 0u32, 0u32, 0usize);
        let (mut refused_tx_select, mut frames_compared) = (0u32, 0usize);
        for (arm, src, cq, txs, tiled, square_only) in &arms {
            let stream = screen_intrabc_stream(src, cq, txs, *tiled, *square_only);
            let (select, premise) = tx_select_and_intrabc_frames(&stream);
            if select > 0 {
                select_arms += 1;
            }
            if premise > 0 {
                premise_arms += 1;
            }
            crate::decode::reset_intrabc_hits();
            let decoded = decode_stream(&stream);
            let blocks = crate::decode::intrabc_hits();
            blocks_total += blocks;
            if blocks > 0 {
                intrabc_arms += 1;
            }
            match decoded {
                Ok(frames) => {
                    assert!(!frames.is_empty(), "{NAME}: {arm} decoded no frame");
                    let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                    assert_eq!(frames.len(), theirs.len(), "{NAME}: {arm} frame count");
                    for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                        assert!(
                            ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                            "{NAME}: {arm} frame {i} does not match ffmpeg"
                        );
                        frames_compared += 1;
                    }
                    eprintln!(
                        "{NAME}: {arm} select={select} premise={premise} {blocks} intrabc \
                         block(s), {} frame(s) exact",
                        frames.len()
                    );
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: {arm} decode failed: {e}"
                    );
                    if msg.contains("an intrabc block under TxMode::Select") {
                        refused_tx_select += 1;
                        assert!(
                            premise > 0,
                            "{NAME}: {arm} reached the TxMode::Select intrabc refusal in a \
                             frame that codes no such header"
                        );
                    } else {
                        assert!(
                            msg.contains("intra block copy"),
                            "{NAME}: {arm} refused for an unrelated reason: {e}"
                        );
                    }
                    eprintln!("{NAME}: {arm} select={select} premise={premise} REFUSED: {e}");
                }
            }
        }
        eprintln!(
            "{NAME}: {select_arms}/{n} arm(s) code a TX_MODE_SELECT frame, {premise_arms} of \
             those keep allow_intrabc, {intrabc_arms} arm(s) decode an intrabc block \
             ({blocks_total} total), {refused_tx_select} reach the refusal, \
             {frames_compared} frame(s) compared exact",
            n = arms.len()
        );
        assert!(
            intrabc_arms > 0 && blocks_total > 0,
            "{NAME}: census is vacuous -- no arm decoded an intrabc block at all"
        );
        assert!(
            premise_arms > 0,
            "{NAME}: census is vacuous -- no arm coded a frame with both allow_intrabc and \
             tx_mode == Select, so the refusal's own premise was never within reach"
        );
        assert_eq!(
            refused_tx_select, 0,
            "{NAME}: lane-t900 r32 lifted this refusal -- an intrabc block now reads the \
             INTER tx-size syntax, so no arm may reach it again"
        );
        assert!(
            frames_compared > 0,
            "{NAME}: no frame was compared pixel-exact"
        );
    }

    /// lane-t900 r31, the WAITING witness for the capability
    /// [`an_intrabc_census_over_screen_key_frames_measures_the_tx_select_refusal`]
    /// pins: a real aomenc screen key frame whose intrabc block sits on a
    /// square block of a `TX_MODE_SELECT` frame, unskipped. libaom's
    /// `parse_decode_block` treats an intrabc block as `inter_block_tx`, so
    /// its transform size comes off the inter var-tx partition tree
    /// (`read_var_tx_size`), which this decoder reads nowhere on the intra
    /// path -- reading the intra `tx_depth` symbol instead desyncs.
    ///
    /// lane-t900 r32 closed both arms. A SKIPPED intrabc block reads no
    /// tx-size symbol at all (`allowSelect = !skip || !is_inter`, false for an
    /// intrabc block since libaom's `is_inter_block` counts one); reading the
    /// intra `tx_depth` for it consumed a symbol libaom never wrote and
    /// desynced the tile a whole block later, at the superblock corner
    /// (192,64) (class `equal-range-means-unread`). An UNSKIPPED one reads the
    /// inter var-tx tree (`read_var_tx_size`). r31's note that cq=45's blocks
    /// are all `skip` was read off that desynced decode: with the tx-size read
    /// corrected, cq=45 carries 3 unskipped intrabc blocks of 10 and cq=30
    /// carries 1 of 16, so both arms exercise both sides of the split.
    #[test]
    fn an_intrabc_block_under_tx_mode_select_decodes_pixel_exact() {
        const NAME: &str = "an_intrabc_block_under_tx_mode_select_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (256usize, 192usize);
        for cq in ["30", "45"] {
            let stream =
                screen_intrabc_stream("smptebars=size=128x96:rate=25", cq, "1", true, true);
            let (select, premise) = tx_select_and_intrabc_frames(&stream);
            assert!(
                select > 0 && premise > 0,
                "{NAME}: cq={cq} no longer codes allow_intrabc under TX_MODE_SELECT \
                 (select={select} premise={premise})"
            );
            crate::decode::reset_intrabc_hits();
            crate::decode::reset_intrabc_vartx_hits();
            let frames = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: cq={cq}: {e}"));
            let (blocks, vartx) = (
                crate::decode::intrabc_hits(),
                crate::decode::intrabc_vartx_hits(),
            );
            assert!(
                blocks > 0,
                "{NAME}: cq={cq} no longer decodes an intrabc block"
            );
            // r31 recorded cq=45's intrabc blocks as ALL skip; that was read
            // off the desynced decode. With the tx-size read corrected both
            // arms carry unskipped ones (cq30 1 of 16, cq45 3 of 10), so both
            // must reach the var-tx tree or the gate is blind to the lift
            // (class `gate-blind-to-feature`).
            assert!(
                vartx > 0,
                "{NAME}: cq={cq} no longer reads the inter var-tx tree for an intrabc block"
            );
            eprintln!("{NAME}: cq={cq} {blocks} intrabc block(s), {vartx} through var-tx");
            // The ENCODE's frame count: `screen_intrabc_stream` (12022) renders
            // `smptebars=size=128x96:rate=25` with `-t 0.2` and `-vf tile=2x2`
            // (12066, 12069) -- 5 frames in, 1 tiled frame out -- and its aomenc
            // runs with `--limit=1` (12101), so both cq arms code exactly 1
            // frame. Asserted ABOVE the oracle call.
            assert_eq!(frames.len(), 1, "{NAME}: cq={cq} key frame");
            let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
            assert_eq!(frames.len(), theirs.len(), "{NAME}: cq={cq} frame count");
            for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
                assert!(
                    ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                    "{NAME}: cq={cq} frame {i} does not match ffmpeg"
                );
            }
        }
    }

    /// lane-av1txr: a real `aomenc --cdf-update-mode=0` stream (spec 5.9.2
    /// `disable_cdf_update`) decodes SAMPLE-EXACT. The read that was missing:
    /// `stream.rs` never threaded the header bit, so the tile readers adapted
    /// every symbol of a frame the encoder coded against fixed tables -- the
    /// stream desynced at its first multi-symbol block and the pixels differed
    /// from byte 6 (`EC_PROBE_OUT` vs `ffmpeg -pix_fmt yuv420p`, measured
    /// 2026-09-20). The msac unit gate
    /// (`a_reader_told_not_to_adapt_matches_a_non_adapting_writer`) pins the
    /// mechanism; this one pins the wiring end to end.
    #[test]
    fn a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (256usize, 192usize);
        let stream = screen_intrabc_stream_with(
            "testsrc2=size=256x192:rate=25",
            "45",
            "1",
            false,
            true,
            &["--cdf-update-mode=0", "--limit=3"],
        );
        // Non-vacuity: the fixture must really code the bit on every frame.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut headers = 0usize;
        while pos < stream.len() {
            let Ok(obu) = parser.parse_obu(&stream[pos..]) else {
                break;
            };
            pos += obu.total_size.max(1);
            let header = match &obu.kind {
                ObuKind::Frame(h, _) | ObuKind::FrameHeader(h) => h,
                _ => continue,
            };
            assert!(
                header.disable_cdf_update,
                "{NAME}: frame {headers} does not carry disable_cdf_update -- aomenc ignored \
                 --cdf-update-mode=0"
            );
            headers += 1;
        }
        assert!(
            headers >= 3,
            "{NAME}: only {headers} frame headers in the fixture"
        );
        let frames = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: {e}"));
        // The frame count, and it comes from the WIRE, not from our decode: the
        // loop above parsed this stream's own OBU frame headers with
        // `Av1Parser` and counted `headers` of them (>= 3, from the arm's
        // `--limit=3` at 13127, which overrides the helper's `--limit=1` because
        // aomenc keeps the last occurrence of a repeated flag). Asserted ABOVE
        // the oracle call, so a decode that showed fewer frames than the bitstream
        // codes reds naming the wire instead of asking ffmpeg for the same short
        // count. Deriving it from `headers` rather than a literal keeps the pin
        // honest if aomenc's frame count ever drifts.
        assert_eq!(
            frames.len(),
            headers,
            "{NAME}: the stream codes {headers} frame(s), the decode showed {}",
            frames.len()
        );
        let theirs = ffmpeg_decode_sequence(&stream, width, height, frames.len());
        assert_eq!(frames.len(), theirs.len(), "{NAME}: frame count");
        for (i, (ours, ref_frame)) in frames.iter().zip(theirs.iter()).enumerate() {
            assert!(
                ours.y == ref_frame.y && ours.u == ref_frame.u && ours.v == ref_frame.v,
                "{NAME}: frame {i} does not match ffmpeg"
            );
        }
        eprintln!("{NAME}: {headers} disable_cdf_update frame headers, all frames sample-exact");
    }

    /// lane-kf900 r4, the gate for the two lifted palette refusals ("a 1:4
    /// rect strip that actually uses a palette (reconstruction is not ported
    /// at this shape)" and "a palette block on a HORZ/VERT intra strip below
    /// 16x16 (reconstruction not ported)"): real `aomenc` streams that put a
    /// palette on a RECT strip, decoded whole-frame pixel-exact against
    /// ffmpeg's own decode of the identical bytes, at 8 AND 10 bit.
    ///
    /// Arm 0 is the stream lane-kf900 r3 pinned and stopped on -- `testsrc2`
    /// 256x192, `--cq-level=59 --cpu-used=0 --min-partition-size=8
    /// --enable-rect-partitions=1 --enable-tx-size-search=1`, md5
    /// `e5f033d8fe34e4418d8cca3ac9d2b595` at 10 bit. Arms 1-2 force the shape
    /// on screen-like content with `--tune-content=screen --enable-palette=1
    /// --enable-1to4-partitions=1`.
    ///
    /// An attempt whose decode never hit a rect-strip palette block proves
    /// nothing about the lift, so it is counted OUT OF SCOPE rather than
    /// silently passing -- but a mismatch on one is still a failure
    /// (`out_of_scope_mismatch == 0`), and at least one attempt must fire the
    /// counter or the gate is vacuous.
    #[test]
    fn a_real_aomenc_rect_strip_palette_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_rect_strip_palette_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 192usize);
        let screen: &[&str] = &[
            "--tune-content=screen",
            "--enable-palette=1",
            "--enable-1to4-partitions=1",
            "--min-partition-size=8",
        ];
        // lane-kf900 r6: arms 3-4 are the palette + `--enable-tx-size-search=1`
        // shape whose neighbour palette lookup was SUB-granular -- a 16x8 strip
        // pair shares one 16px cell, so strip 1 read strip 0's palette size and
        // the colour cache came out short. Both were whole-frame wrong before
        // the mi-granular fix (`$HOME/.cache/kf900-r6/sweep.log`: smptebars
        // cq40/cq55 txs=1 pal=1 = DIFF).
        let screen_txs: &[&str] = &[
            "--tune-content=screen",
            "--enable-palette=1",
            "--enable-1to4-partitions=1",
            "--min-partition-size=8",
            "--max-partition-size=32",
            "--sb-size=64",
            "--enable-intrabc=1",
            "--enable-tx-size-search=1",
        ];
        let cq40: Vec<&str> = screen_txs
            .iter()
            .copied()
            .chain(["--cq-level=40"])
            .collect();
        let cq55: Vec<&str> = screen_txs
            .iter()
            .copied()
            .chain(["--cq-level=55"])
            .collect();
        let arms: [(&str, String, &[&str]); 5] = [
            (
                "testsrc2-pinned",
                "testsrc2=s=256x192:r=25".to_string(),
                &[],
            ),
            (
                "testsrc2-screen",
                "testsrc2=s=256x192:r=25".to_string(),
                screen,
            ),
            (
                "smptebars-screen",
                "smptebars=size=256x192:rate=25".to_string(),
                screen,
            ),
            (
                "smptebars-screen-txs-cq40",
                "smptebars=size=256x192:rate=25".to_string(),
                &cq40,
            ),
            (
                "smptebars-screen-txs-cq55",
                "smptebars=size=256x192:rate=25".to_string(),
                &cq55,
            ),
        ];
        let (mut fired_arms, mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32, 0u32);
        let mut refused = 0u32;
        let mut fired_at_depth = [0u32; 2];
        let mut frames_compared = 0usize;
        for (arm, src, extra) in &arms {
            for depth in [8usize, 10] {
                let pix_fmt = if depth == 10 {
                    "yuv420p10le"
                } else {
                    "yuv420p"
                };
                let render = || {
                    let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                        .args([
                            "-v",
                            "error",
                            "-f",
                            "lavfi",
                            "-i",
                            src.as_str(),
                            "-t",
                            "0.2",
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
                    assert!(out.status.success(), "{NAME}: ffmpeg failed for {arm}");
                    out.stdout
                };
                let y4m_a = render();
                let y4m_b = render();
                assert_eq!(
                    y4m_a, y4m_b,
                    "{NAME}: {arm} must render byte-identical across two runs at {depth}-bit"
                );
                let y4m = y4m_a;
                let depth_arg = format!("--bit-depth={depth}");
                let in_depth_arg = format!("--input-bit-depth={depth}");
                let out = run_with_stdin(
                    Command::new(aomenc_path())
                        .args(["--codec=av1"])
                        .args([&depth_arg, &in_depth_arg])
                        .args([
                            "--passes=1",
                            "--end-usage=q",
                            "--cq-level=59",
                            "--cpu-used=0",
                            "--lag-in-frames=0",
                            "--kf-max-dist=1",
                            "--limit=1",
                            "--threads=1",
                            "--tile-columns=0",
                            "--enable-tx-size-search=1",
                            "--min-partition-size=8",
                            "--enable-rect-partitions=1",
                        ])
                        // Per-arm overrides go AFTER the base recipe: aomenc keeps
                        // the LAST occurrence of a repeated flag.
                        .args(*extra)
                        // The 8-bit twin of the pinned recipe picks a 128-root
                        // HORZ partition, which this decoder still refuses by name
                        // ("a 128x128 superblock HORZ/VERT or AB partition") --
                        // an open refusal of another lane, unrelated to palette.
                        // Pinning the superblock to 64 keeps the 8-bit arm on the
                        // shape this gate is about; the 10-bit arms are the
                        // untouched pinned recipe.
                        .args(if depth == 8 {
                            &["--sb-size=64"][..]
                        } else {
                            &[][..]
                        })
                        .args(["--obu", "-o", "-", "-"]),
                    &y4m,
                );
                assert!(
                    out.status.success(),
                    "{NAME}: aomenc refused {arm} at {depth}-bit: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let before = decode::rect4_palette_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    // A NAMED refusal is another lane's open shape (here: a
                    // palette on an 8x8 SQUARE leaf, still out of scope), never
                    // a pass -- the attempt is counted out of scope and the
                    // per-depth firing assert below keeps the gate honest.
                    Err(e) if e.to_string().contains("unsupported") => {
                        eprintln!("{NAME}: {arm} {depth}-bit REFUSED: {e}");
                        refused += 1;
                        continue;
                    }
                    Err(e) => panic!("{NAME}: decode failed on {arm} at {depth}-bit: {e}"),
                };
                let fired = decode::rect4_palette_hits() - before;
                // The ENCODE's frame count, asserted ABOVE the oracle call: the
                // y4m renders 5 frames (`-t 0.2` at every arm's `rate=25`,
                // 13275) but aomenc is given `--limit=1` (13313), so the stream
                // codes exactly 1 frame at both depths on every arm. Asserted
                // here it is NOT the tautology the old comment below claimed:
                // `frames.len() == 1` is a fact about the encode, checked before
                // `frames.len()` is spent as ffmpeg's frame budget.
                assert_eq!(frames.len(), 1, "{NAME}: {arm} at {depth}-bit key frame");
                let ffmpeg_frames = if depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frames.len())
                };
                assert!(
                    !frames.is_empty(),
                    "{NAME}: {arm} at {depth}-bit decoded no frame"
                );
                // EVERY decode-order frame, all three planes.
                let mut exact = true;
                for (i, (ours, theirs)) in frames.iter().zip(ffmpeg_frames.iter()).enumerate() {
                    if ours.y != theirs.y || ours.u != theirs.u || ours.v != theirs.v {
                        exact = false;
                        eprintln!("{NAME}: {arm} {depth}-bit frame {i} differs");
                    }
                    frames_compared += 1;
                }
                if fired > 0 {
                    fired_arms += 1;
                    fired_at_depth[usize::from(depth == 10)] += 1;
                    assert!(
                        exact,
                        "{NAME}: {arm} at {depth}-bit has {fired} rect-strip palette block(s) \
                         and does NOT decode pixel-exact"
                    );
                } else {
                    out_of_scope += 1;
                    if !exact {
                        out_of_scope_mismatch += 1;
                    }
                }
            }
        }
        eprintln!(
            "{NAME}: {fired_arms} attempt(s) with a rect-strip palette block, \
             {out_of_scope} out of scope ({out_of_scope_mismatch} mismatched), \
             {refused} refused, {frames_compared} frames compared, \
             fired 8-bit={} 10-bit={}",
            fired_at_depth[0], fired_at_depth[1]
        );
        // MEASURED 2026-09-02, every 8-bit attempt of these recipes: aomenc
        // puts its palettes on SQUARE blocks at 8 bit and every 8-bit stream
        // that reaches a rect strip with one lands on a palette shape still
        // out of scope (8x8 square leaf, or a 64x32/32x64 superblock strip
        // with a real transform). So the 8-bit arms here prove the DECODE
        // path, not the lift -- `fired_at_depth[0] == 0` is a known, reported
        // coverage gap and not a silent pass: the counter is printed.
        assert!(
            fired_at_depth[1] > 0,
            "{NAME}: no attempt put a palette on a rect strip \
             (8-bit={}, 10-bit={}) -- gate is vacuous",
            fired_at_depth[0],
            fired_at_depth[1]
        );
        assert!(fired_arms > 0, "{NAME}: gate is vacuous");
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: {out_of_scope_mismatch} attempt(s) without a rect-strip palette block \
             still mismatched -- an unrelated defect, not a SKIP"
        );
    }

    /// The 12-bit KEY witness: CDEF and loop restoration ON, grain off.
    /// Byte-exact against ffmpeg, and the 12-bit-sensitive pixel paths hard-
    /// assert fired: a CDEF index literal read (bits > 0) and at least one
    /// Wiener or SGR restoration unit. Gauntlet for the parameterised
    /// rounding sites: MC/Wiener round pairs, `coeff_shift = bd - 8` CDEF,
    /// the 12-bit dequant tables, bd-scaled SGR box sums.
    #[test]
    fn a_real_aomenc_12bit_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let y4m = y4m_12bit_subpel_source(1);
        let stream = encode_12bit(
            &y4m,
            1,
            &["--enable-warped-motion=0", "--enable-global-motion=0"],
        );
        assert_12bit_sequence_header(&stream, NAME);
        let before_cdef = decode::cdef_idx_hits();
        let before_lr = crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 12-bit key frame: {e}")
        });
        assert_eq!(frames.len(), 1);
        assert!(
            decode::cdef_idx_hits() > before_cdef,
            "{NAME}: no CDEF index literal was read -- CDEF is not exercised"
        );
        assert!(
            crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits() > before_lr,
            "{NAME}: no loop restoration unit was decoded -- LR is not exercised"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, 160, 128, 1);
        assert_eq!(
            frames[0].y, ffmpeg_frames[0].y,
            "luma vs ffmpeg (12-bit key)"
        );
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (12-bit key)");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (12-bit key)");
    }

    /// The 12-bit INTER witness: a key frame plus one P frame over sub-pixel
    /// true motion. Byte-exact against ffmpeg; hard-asserts the two-pass
    /// SUBPEL prediction path ran (`MC_SUBPEL_HITS`), so the frame's own
    /// `round_0`/`round_1` -- 5/9 at 12 bits, the whole point of the mc.rs
    /// parameterisation -- really executed, and asserts
    /// `compound_mode_hits` UNCHANGED (this recipe offers no divergent
    /// second reference, so no compound mode symbol is read -- the lane's
    /// compound witness is the six-frame gate below).
    #[test]
    fn a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let y4m = y4m_12bit_subpel_source(2);
        let stream = encode_12bit(
            &y4m,
            2,
            &["--enable-warped-motion=0", "--enable-global-motion=0"],
        );
        assert_12bit_sequence_header(&stream, NAME);
        let before_subpel = crate::mc::mc_subpel_hits();
        let before_compound = decode::compound_mode_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 12-bit inter stream: {e}")
        });
        assert_eq!(frames.len(), 2);
        assert!(
            crate::mc::mc_subpel_hits() > before_subpel,
            "{NAME}: no subpel inter prediction decoded -- the 12-bit round pair is unexercised"
        );
        assert_eq!(
            decode::compound_mode_hits(),
            before_compound,
            "{NAME}: a compound mode symbol was read -- the single-reference premise is gone"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, 160, 128, 2);
        for (i, (ours, reference)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(
                ours.y, reference.y,
                "frame {i} luma vs ffmpeg (12-bit inter)"
            );
            assert_eq!(ours.u, reference.u, "frame {i} U vs ffmpeg (12-bit inter)");
            assert_eq!(ours.v, reference.v, "frame {i} V vs ffmpeg (12-bit inter)");
        }
    }
